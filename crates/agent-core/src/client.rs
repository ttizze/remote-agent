//! Client workflows built on the shared transport and protocol.
use crate::{peer::PeerError, state::operations::ReadThread};
use agent_protocol::operations::*;
use agent_transport::client::{Client, Updates};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}

/// One resumable terminal per device and working directory. Host scopes this ID to the
/// authenticated device, so reopening a view or application can attach safely.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_handle(cwd: String) -> String {
    format!(
        "bex-terminal-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, cwd.as_bytes())
    )
}

#[allow(async_fn_in_trait)]
pub trait ClientExt {
    async fn open_subscription<O: RpcMethod>(
        &self,
        operation: &O,
    ) -> Result<Option<(O::Output, Updates, uuid::Uuid)>, PeerError>;
    async fn models(&self) -> Result<ModelPage, PeerError>;
    async fn respond(&self, request: &ServerRequest, answer: &Answer) -> Result<(), PeerError>;
    async fn session_images(
        &self,
        thread_id: &str,
        session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError>;
}
impl ClientExt for Client {
    async fn open_subscription<O: RpcMethod>(
        &self,
        operation: &O,
    ) -> Result<Option<(O::Output, Updates, uuid::Uuid)>, PeerError> {
        if operation.method() != "host/session/open" {
            return Ok(None);
        }
        let (mut output, updates) = self
            .request_stream::<O::Output>(&operation.request()?)
            .await?;
        validate_output(operation, &output)?;
        let id = uuid::Uuid::new_v4();
        O::subscription(&mut output, id);
        Ok(Some((output, updates, id)))
    }

    async fn models(&self) -> Result<ModelPage, PeerError> {
        let mut provider_errors = Map::<String, Value>::new();
        let mut models: Vec<crate::models::Model> = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .call(&ListModels {
                    limit: 100,
                    cursor: cursor.clone(),
                })
                .await?;
            if let Some(errors) = page.provider_errors {
                provider_errors.extend(errors);
            }
            for model in page.data {
                if let Some(index) = models.iter().position(|previous| previous.id == model.id) {
                    models[index] = model;
                } else {
                    models.push(model);
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                return Ok(ModelPage {
                    data: models,
                    next_cursor: None,
                    provider_errors: (!provider_errors.is_empty()).then_some(provider_errors),
                });
            };
            if !seen.insert(next.clone()) {
                return Err(PeerError::InvalidMessage(
                    "model cursor did not advance".into(),
                ));
            }
        }
    }

    async fn respond(&self, request: &ServerRequest, answer: &Answer) -> Result<(), PeerError> {
        let result = answer_result(request, answer)?;
        validate_answer(request, &result).map_err(PeerError::InvalidMessage)?;
        self.request::<crate::models::Empty>(&crate::protocol::Call::AnswerSession(SessionAnswer {
            request_id: request.id.clone(),
            result,
        }))
        .await
        .map(|_| ())
    }

    async fn session_images(
        &self,
        thread_id: &str,
        session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError> {
        // One bounded provider view; close this transient subscription before
        // returning images so it never replaces the Store's visible session.
        let read = ReadThread {
            limit: 1000,
            ..ReadThread::new(thread_id.to_owned())
        };
        let (opened, stream) = self
            .request_stream::<crate::session::OpenedSession>(&read.request()?)
            .await?;
        validate_output(&read, &opened)?;
        drop(stream);
        let thread = opened.response.thread;
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        let mut native_items = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for turn in thread.turns.as_deref().unwrap_or_default().iter().rev() {
            for item in turn.items.as_deref().unwrap_or_default().iter().rev() {
                if item.kind.as_deref() != Some("imageGeneration") || !native_items.insert(&item.id)
                {
                    continue;
                }
                let detail;
                let item = if turn
                    .deferred_item_ids
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&item.id))
                {
                    detail = crate::transfers::resolve_item(
                        self.call(&crate::state::operations::ReadItem {
                            thread_id: thread_id.into(),
                            turn_id: turn.id.clone(),
                            item_id: item.id.clone(),
                        })
                        .await?,
                        session,
                    )
                    .await?;
                    &detail.item
                } else {
                    item.as_ref()
                };
                let image = item
                    .saved_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .map(|path| (path, false))
                    .or_else(|| {
                        item.result
                            .as_ref()
                            .and_then(Value::as_str)
                            .filter(|data| !data.trim().is_empty())
                            .map(|data| (data, true))
                    });
                if let Some((source, encoded)) = image
                    && sources.insert(source.to_owned())
                {
                    bytes = bytes.saturating_add(source.len());
                    if bytes > 16 * 1024 * 1024 {
                        return Err(PeerError::InvalidMessage("画像一覧が16 MiBの上限に達しました。会話内の画像から個別に開いてください。".into()));
                    }
                    images.push(SessionImage {
                        source: source.into(),
                        encoded,
                    });
                }
            }
        }
        if thread.history_has_more == Some(true)
            || thread
                .history_read_state
                .as_ref()
                .is_some_and(|state| state.kind != crate::session::HistoryReadKind::Complete)
        {
            return Err(PeerError::InvalidMessage("履歴が部分取得のため、画像一覧を完全には取得できません。会話内の画像から個別に開いてください。".into()));
        }
        images.reverse();
        Ok(images)
    }
}
