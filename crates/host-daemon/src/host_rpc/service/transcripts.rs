//! Conversation reads from Host transcripts, and their one-time native import.
use super::*;
use crate::host_rpc::conversations::Conversation;
use agent_protocol::session::SessionRef;
use conversations::HistoryUse;
use futures_util::FutureExt;

#[cfg(test)]
mod tests;

/// Every transcript window, including older pages, holds at most this many turns.
pub(super) const WINDOW_TURNS: usize = 10;

/// Resolves once the conversation's transcript is stored or cannot be.
pub(super) type Importing =
    futures_util::future::Shared<futures_util::future::BoxFuture<'static, Result<(), Failure>>>;

/// Native reads still show these conversations, with what they can read.
pub(super) fn readable_without_import(error: &Failure) -> bool {
    matches!(error.code, "provider_unavailable" | "incomplete_history")
}

pub(super) enum Import {
    Running(Importing),
    Failed,
}

pub(super) fn storage_failure(error: String) -> Failure {
    Failure::new("transcript_storage_failed", error)
}

// Saved conversations already paged by numeric start positions. Other
// conversations mark positions so that native cursors never alias them.
fn cursor(row: bool, start: usize) -> String {
    if row {
        start.to_string()
    } else {
        format!("t{start}")
    }
}

/// The start position a transcript cursor names, or None for a native cursor.
pub(super) fn position(row: bool, cursor: &str) -> Option<usize> {
    if row {
        cursor.parse().ok()
    } else {
        cursor.strip_prefix('t')?.parse().ok()
    }
}

impl HostRpcService {
    /// A stored window with Host-owned metadata applied, or None before import.
    pub(super) fn stored_history(
        &self,
        target: &SessionRef,
        row: Option<&Conversation>,
        end: Option<usize>,
        limit: usize,
        include_activity: bool,
    ) -> Result<Option<ThreadResponse>, Failure> {
        if row.is_some_and(|row| row.startup.is_some()) {
            return Ok(None);
        }
        let Some(window) = self
            .inner
            .transcripts
            .window(target, end, limit)
            .map_err(storage_failure)?
        else {
            return Ok(None);
        };
        let mut response = window.response;
        let native = row.map_or(target, |row| &row.active);
        let saved = self
            .inner
            .model_selections
            .resolve(native, None, None, None)?;
        if let Some(row) = row {
            response = self.describe_conversation_history(row, response, saved)?;
        } else {
            response.thread.capabilities = Some(
                self.inner
                    .providers
                    .get(target.provider)
                    .map(|agent| agent.capabilities())
                    .unwrap_or_default(),
            );
            // Accepted turns update Host selections; the imported snapshot
            // describes the conversation before the Host ran it.
            response.model = saved.model.or(response.model);
            response.effort = saved.effort.or(response.effort);
            response.service_tier = saved.service_tier.or(response.service_tier);
        }
        if !include_activity {
            for turn in response.thread.turns.iter_mut().flatten() {
                let provider = turn.provider.unwrap_or(target.provider);
                let Ok(agent) = self.inner.providers.get(provider) else {
                    continue;
                };
                let items = turn.items.as_deref().unwrap_or_default();
                // Only a summary that hides items offers to load the turn's details.
                if let Some(summary) = agent.summary_items(items)
                    && summary.len() < items.len()
                {
                    let turn = Arc::make_mut(turn);
                    turn.items = Some(summary);
                    turn.items_summary = true;
                }
            }
        }
        let more = window.start > 0;
        response.thread.history_cursor = more.then(|| cursor(row.is_some(), window.start));
        response.thread.history_has_more = Some(more);
        response.thread.history_read_state = Some(agent_protocol::session::HistoryReadState::new(
            if more {
                agent_protocol::session::HistoryReadKind::Partial
            } else {
                agent_protocol::session::HistoryReadKind::Complete
            },
            Vec::new(),
        ));
        Ok(Some(response))
    }

    /// Copy native history once; concurrent callers share one import, which
    /// completes even if they stop waiting. Incomplete native history is not
    /// imported again until a re-import or a Host restart; other failures,
    /// such as a provider still starting, let the next open try again.
    pub(super) fn import_transcript(&self, target: SessionRef, retry: bool) -> Option<Importing> {
        let mut imports = self.inner.imports.lock().unwrap();
        match imports.get(&target) {
            Some(Import::Running(importing)) => return Some(importing.clone()),
            Some(Import::Failed) if !retry => return None,
            _ => {}
        }
        let service = self.clone();
        let key = target.clone();
        let importing = tokio::spawn(async move { service.import(key).await })
            .map(|finished| {
                finished.unwrap_or_else(|error| Err(Failure::new("import_failed", error)))
            })
            .boxed()
            .shared();
        imports.insert(target, Import::Running(importing.clone()));
        Some(importing)
    }

    async fn import(&self, target: SessionRef) -> Result<(), Failure> {
        let started = std::time::Instant::now();
        // Native reads can outlive a rewind or a new recorded turn. Admit
        // their result only while the captured transcript is still current.
        let result = async {
            let version = self
                .inner
                .transcripts
                .version(&target)
                .map_err(storage_failure)?;
            let response = self.import_history(&target).await?;
            let transcripts = self.inner.transcripts.clone();
            let key = target.clone();
            tokio::task::spawn_blocking(move || transcripts.import(&key, version, response))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
                .map_err(storage_failure)
        }
        .await;
        let mut imports = self.inner.imports.lock().unwrap();
        match &result {
            Ok(()) => {
                imports.remove(&target);
                tracing::info!(target: "agent", operation = "transcript.import",
                    message = %format_args!("provider={} total_ms={}", target.provider.key(), started.elapsed().as_millis()));
            }
            Err(error) => {
                if error.code == "incomplete_history" {
                    imports.insert(target, Import::Failed);
                } else {
                    imports.remove(&target);
                }
                tracing::warn!(target: "agent", operation = "transcript.import", message = %error.message);
            }
        }
        result
    }

    /// History in public identity, keeping the conversation's content.
    async fn import_history(&self, target: &SessionRef) -> Result<ThreadResponse, Failure> {
        let mut response = if let Some(row) = self.inner.conversations.get(target)? {
            if row.startup.is_some() {
                return Err(Failure::new(
                    "preparation_active",
                    "A conversation is imported after its preparation finishes.",
                ));
            }
            self.conversation_history(&row, HistoryUse::Import).await?
        } else {
            self.native_history(target, HistoryUse::Import).await?
        };
        // Past activity is not imported; turns the Host runs keep every item.
        for turn in response.thread.turns.iter_mut().flatten() {
            if let Some(items) = &mut Arc::make_mut(turn).items {
                items.retain(|item| content(item.body()));
            }
        }
        Ok(response)
    }
}

/// What an import copies from native history: messages and images.
pub(super) fn content(body: &agent_protocol::items::ItemBody) -> bool {
    matches!(
        body,
        agent_protocol::items::ItemBody::UserMessage { .. }
            | agent_protocol::items::ItemBody::AssistantText { .. }
            | agent_protocol::items::ItemBody::ImageGeneration { .. }
            | agent_protocol::items::ItemBody::ImageView { .. }
    )
}
