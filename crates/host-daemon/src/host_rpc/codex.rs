//! Codex native protocol boundary: one shared process, ordered request
//! completion, native cursors, deferred item hydration and detail reads.
use super::{routing::SessionRouter, service::Failure};
use agent_protocol::{
    models::{Item, Thread, ThreadResponse, Turn},
    operations as op,
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use agent_transport::peer::{RpcMessage, RpcMessageKind};
use codex_app_server::CodexAppServer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadListParams<'a> {
    pub limit: usize,
    pub sort_key: &'a str,
    pub sort_direction: &'a str,
    pub use_state_db_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

use std::sync::Arc;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
    pub limit: usize,
    pub sort_direction: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_view: Option<&'a str>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryItem {
    pub item: Arc<Item>,
    pub turn_id: Option<String>,
}

fn unmaterialized_history(error: &str, thread_id: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(error) else {
        return false;
    };
    value["code"] == -32600
            && value["message"].as_str()
                == Some(
                    format!(
                        "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
                    )
                    .as_str(),
                )
}

impl Page<HistoryItem> {
    fn into_items(self) -> Result<(Vec<Arc<Item>>, Option<String>), &'static str> {
        if self.data.len() > 100 {
            return Err("item page exceeds requested size");
        }
        let items = self
            .data
            .into_iter()
            .map(|entry| {
                if entry.item.id.is_empty() {
                    Err("history item ID is missing")
                } else {
                    Ok(entry.item)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok((items, self.next_cursor))
    }
}

pub(super) struct Codex {
    pub(super) process: Result<Arc<CodexAppServer>, String>,
    pub(super) stopped: tokio_util::sync::CancellationToken,
    pub(super) processed: tokio::sync::watch::Sender<u64>,
}
impl Codex {
    pub(super) fn capabilities() -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            additional_input: true,
            fork: true,
            rename: true,
            model_change: true,
        }
    }

    pub(super) fn new(process: Result<Arc<CodexAppServer>, String>) -> Self {
        Self {
            process,
            stopped: Default::default(),
            processed: tokio::sync::watch::channel(0).0,
        }
    }
    pub(super) fn server(&self) -> Result<&CodexAppServer, Failure> {
        if self.stopped.is_cancelled() {
            return Err(Failure::new(
                "codex_unavailable",
                "Codexが終了しました。Hostを再起動すると再接続できます。Claudeの会話は継続できます。",
            ));
        }
        self.process
            .as_deref()
            .map_err(|error| Failure::new("codex_unavailable", error))
    }

    pub(super) async fn request_raw(&self, line: &str) -> Result<String, Failure> {
        let reply = self
            .server()?
            .request_raw_sequenced(line)
            .await
            .map_err(Failure::from)?;
        self.wait_for_events(reply.sequence).await?;
        Ok(reply.value)
    }

    pub(super) async fn request<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<T, Failure> {
        let reply = self
            .server()?
            .request_sequenced(method, params)
            .await
            .map_err(Failure::from)?;
        self.wait_for_events(reply.sequence).await?;
        reply.value.outcome.map_err(Failure::upstream)
    }

    async fn wait_for_events(&self, sequence: u64) -> Result<(), Failure> {
        let mut processed = self.processed.subscribe();
        tokio::select! {
            result = processed.wait_for(|position| *position >= sequence) => {
                result.map_err(|_| Failure::unknown("codex_unavailable", "Codex event pump stopped"))?;
            }
            _ = self.stopped.cancelled() => return Err(Failure::unknown("codex_unavailable", "Codex event stream is unavailable")),
        }
        Ok(())
    }

    pub(super) async fn projects(&self) -> Result<Vec<agent_protocol::models::Project>, Failure> {
        let mut projects = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page: Page<agent_protocol::models::Project> = self
                .server()?
                .request(
                    "project/list",
                    &serde_json::json!({"limit":100,"cursor":cursor}),
                )
                .await
                .map_err(Failure::from)?
                .outcome
                .map_err(Failure::upstream)?;
            projects.extend(page.data);
            match page.next_cursor {
                None => return Ok(projects),
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                Some(_) => {
                    return Err(Failure::new(
                        "invalid_project_list",
                        "project cursor did not advance",
                    ));
                }
            }
        }
    }

    pub(super) async fn create_project(&self, root: &std::path::Path) -> Result<(), Failure> {
        self.server()?.request::<_, Value>("project/create", &serde_json::json!({
            "idempotencyKey":uuid::Uuid::new_v4().to_string(),
            "name":root.file_name().map(|name| name.to_string_lossy()).unwrap_or_else(|| root.to_string_lossy()),
            "roots":[{"path":root}],
        })).await.map_err(Failure::from)?.outcome.map_err(Failure::upstream)?;
        Ok(())
    }

    pub(super) async fn thread_page(
        &self,
        params: &ThreadListParams<'_>,
    ) -> Result<Page<Thread>, Failure> {
        self.server()?
            .request("thread/list", params)
            .await
            .map_err(Failure::from)?
            .outcome
            .map_err(Failure::upstream)
    }

    pub(super) async fn read(&self, id: &str, limit: usize) -> Result<ThreadResponse, Failure> {
        let mut response: ThreadResponse = self
            .request(
                "thread/read",
                &serde_json::json!({"threadId":id,"includeTurns":false}),
            )
            .await?;
        let history = self
            .history(
                id,
                response.thread.history_mode.as_deref() == Some("paginated"),
                limit,
            )
            .await?;
        response.thread.turns = history.turns;
        response.thread.history_read_state = history.history_read_state;
        response.thread.history_has_more = history.history_has_more;
        Ok(response)
    }

    async fn history(&self, id: &str, paginated: bool, limit: usize) -> Result<Thread, Failure> {
        if !paginated {
            let mut thread = self
                .request::<_, ThreadResponse>(
                    "thread/read",
                    &serde_json::json!({"threadId":id,"includeTurns":true}),
                )
                .await?
                .thread;
            if thread.id.as_deref() != Some(id) {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "native session identity changed",
                ));
            }
            if let Some(turns) = &mut thread.turns
                && turns.len() > limit
            {
                turns.drain(..turns.len() - limit);
                thread.history_has_more = Some(true);
            }
            return Ok(thread);
        }
        let query = HistoryParams {
            thread_id: id,
            turn_id: None,
            cursor: None,
            limit,
            sort_direction: "desc",
            items_view: Some("notLoaded"),
        };
        let mut page = match self
            .history_request::<Arc<Turn>>("thread/turns/list", &query)
            .await
        {
            Ok(page) => page,
            Err(error) if unmaterialized_history(&error, id) => {
                // Native Codex explicitly confirms there is no persisted first
                // message. The session router overlays any in-flight live turn.
                Page {
                    data: Vec::new(),
                    next_cursor: None,
                }
            }
            Err(error)
                if serde_json::from_str::<Value>(&error)
                    .ok()
                    .is_some_and(|value| value["code"] == -32601) =>
            {
                // Native servers advertise pagination before materializing a
                // first turn. Read that same native session without cursors.
                return match Box::pin(self.history(id, false, limit)).await {
                    Ok(history) => Ok(history),
                    Err(error) => Ok(Thread {
                        id: Some(id.into()),
                        history_read_state: Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Unavailable,
                            vec![error.to_string()],
                        )),
                        ..Default::default()
                    }),
                };
            }
            Err(error) => return Err(Failure::new("invalid_thread_history", error)),
        };
        self.hydrate_turn_page(&mut page, &query)
            .await
            .map_err(|error| Failure::new("invalid_thread_history", error))?;
        let mut thread = Thread {
            id: Some(id.into()),
            ..Default::default()
        };
        page.data.reverse();
        thread.turns = Some(page.data);
        thread.history_has_more = Some(page.next_cursor.is_some_and(|cursor| !cursor.is_empty()));
        Ok(thread)
    }
    // Keep App Server cursors opaque. Both initial hydration and older pages
    // use the desktop five-turn / 500-item initial window, in pages of 100.
    async fn history_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &HistoryParams<'_>,
    ) -> Result<Page<T>, String> {
        self.request(method, params)
            .await
            .map_err(|error| error.to_string())
    }

    async fn hydrate_turn_page(
        &self,
        page: &mut Page<Arc<Turn>>,
        query: &HistoryParams<'_>,
    ) -> Result<(), String> {
        if page.data.len() > query.limit {
            return Err("turn page exceeds requested size".into());
        }
        let thread_id = query.thread_id;
        let mut ids = std::collections::HashSet::new();
        let repeated = page.data.iter().any(|turn| !ids.insert(turn.id.as_str()));
        if repeated {
            // Item pagination is keyed only by turn ID, so it cannot preserve
            // boundaries between historical occurrences sharing that ID.
            let full = self
                .history_request::<Arc<Turn>>(
                    "thread/turns/list",
                    &HistoryParams {
                        items_view: Some("full"),
                        ..*query
                    },
                )
                .await?;
            if full.next_cursor != page.next_cursor
                || !full
                    .data
                    .iter()
                    .map(|turn| &turn.id)
                    .eq(page.data.iter().map(|turn| &turn.id))
            {
                return Err("turn history changed while loading repeated IDs".into());
            }
            if full
                .data
                .iter()
                .any(|turn| turn.items.is_none() || turn.items_view.as_deref() == Some("notLoaded"))
            {
                return Err("full turn history omitted repeated-turn items".into());
            }
            *page = full;
            return Ok(());
        }
        let mut budget = query.limit.saturating_mul(100).min(100_000);
        for turn in &mut page.data {
            let turn = Arc::make_mut(turn);
            let mut values = Vec::new();
            let mut cursor = None;
            let mut has_more = true;
            let mut cursors = std::collections::HashSet::new();
            let mut ids = std::collections::HashSet::new();
            while has_more && budget > 0 {
                let page = self
                    .history_request::<HistoryItem>(
                        "thread/items/list",
                        &HistoryParams {
                            thread_id,
                            turn_id: Some(&turn.id),
                            cursor: cursor.as_deref(),
                            limit: budget.min(100),
                            sort_direction: "desc",
                            items_view: None,
                        },
                    )
                    .await?;
                let (items, next_cursor) = page.into_items()?;
                cursor = next_cursor;
                for item in items {
                    if ids.insert(item.id.clone()) {
                        budget = budget
                            .checked_sub(1)
                            .ok_or("item page exceeds requested budget")?;
                        values.push(item);
                    }
                }
                has_more = cursor.is_some();
                if let Some(cursor) = &cursor
                    && !cursors.insert(cursor.clone())
                {
                    return Err("history cursor repeated".into());
                }
            }
            values.reverse();
            turn.items_view = Some(
                if !has_more {
                    "full"
                } else if values.is_empty() {
                    "notLoaded"
                } else {
                    "summary"
                }
                .into(),
            );
            turn.items_has_more = Some(has_more);
            turn.items = Some(values);
            self.preserve_opening_question(turn, thread_id).await?;
        }
        Ok(())
    }

    async fn preserve_opening_question(
        &self,
        turn: &mut Turn,
        thread_id: &str,
    ) -> Result<(), String> {
        let values = turn.items.as_deref().ok_or("turn items are missing")?;
        if turn.items_has_more != Some(true) || values.is_empty() {
            return Ok(());
        }
        let opening = self
            .history_request::<HistoryItem>(
                "thread/items/list",
                &HistoryParams {
                    thread_id,
                    turn_id: Some(&turn.id),
                    cursor: None,
                    limit: 2,
                    sort_direction: "asc",
                    items_view: None,
                },
            )
            .await?;
        if let Some(item) = opening
            .into_items()?
            .0
            .into_iter()
            .find(|item| item.kind.as_deref() != Some("contextCompaction"))
            .filter(|item| item.kind.as_deref() == Some("userMessage"))
            && !values.iter().any(|value| value.id == item.id)
        {
            turn.opening_user_message = Some(item);
        }
        Ok(())
    }

    pub(super) async fn item_read(
        &self,
        params: op::ReadItem,
    ) -> Result<agent_protocol::operations::ItemResponse, Failure> {
        if [&params.thread_id, &params.turn_id, &params.item_id]
            .iter()
            .any(|id| id.is_empty())
        {
            return Err(Failure::new(
                "invalid_params",
                "threadId, turnId and itemId are required",
            ));
        }
        let mut cursor = None;
        let mut cursors = std::collections::HashSet::new();
        loop {
            let query = HistoryParams {
                thread_id: &params.thread_id,
                turn_id: Some(&params.turn_id),
                limit: 100,
                sort_direction: "asc",
                cursor: cursor.as_deref(),
                items_view: None,
            };
            let response = self
                .server()?
                .request::<_, Page<HistoryItem>>("thread/items/list", &query)
                .await
                .map_err(Failure::from)?;
            let page = response.outcome.map_err(Failure::upstream)?;
            if let Some(entry) = page.data.into_iter().find(|entry| {
                entry.turn_id.as_deref() == Some(params.turn_id.as_str())
                    && entry.item.id == params.item_id
            }) {
                return Ok(agent_protocol::operations::ItemResponse {
                    item: Arc::unwrap_or_clone(entry.item),
                    transfer: None,
                });
            }
            cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            let Some(next) = &cursor else {
                return Err(Failure::new(
                    "item_not_found",
                    "The activity is no longer available. Refresh the task.",
                ));
            };
            if !cursors.insert(next.clone()) {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "item page cursor repeated",
                ));
            }
        }
    }
}

/// Decode only native conversation events. Every other message keeps its owner.
fn notification_change(
    method: &str,
    value: &Value,
) -> Result<Option<(String, SessionChange)>, serde_json::Error> {
    use serde::Deserialize;
    let turn_id = value["turnId"].as_str().unwrap_or_default().to_owned();
    let change = match method {
        "thread/status/changed" => SessionChange::Status {
            status: Deserialize::deserialize(&value["status"])?,
        },
        "turn/started" | "turn/completed" => {
            let mut turn: Turn = Deserialize::deserialize(&value["turn"])?;
            turn.items_view.get_or_insert_with(|| "full".into());
            turn.items_has_more.get_or_insert(false);
            SessionChange::Turn {
                turn,
                completed: method == "turn/completed",
            }
        }
        "item/started" | "item/completed" => SessionChange::Item {
            turn_id,
            item: Deserialize::deserialize(&value["item"])?,
        },
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
            let id = Deserialize::deserialize(&value["reviewId"])?;
            if value["review"]["status"] == "approved" {
                SessionChange::RemoveItem {
                    turn_id,
                    item_id: id,
                }
            } else {
                SessionChange::Item {
                    turn_id,
                    item: Item {
                        id,
                        kind: Some("automaticApprovalReview".into()),
                        review: value.get("review").cloned(),
                        ..Default::default()
                    }
                    .into(),
                }
            }
        }
        "item/agentMessage/delta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta" => SessionChange::Text {
            turn_id,
            item_id: Deserialize::deserialize(&value["itemId"])?,
            delta: Deserialize::deserialize(&value["delta"])?,
            field: match method {
                "item/agentMessage/delta" => TextField::Message,
                "item/commandExecution/outputDelta" => TextField::CommandOutput,
                "item/fileChange/outputDelta" => TextField::FileChange,
                _ => TextField::Reasoning,
            },
        },
        "error" => SessionChange::Error {
            turn_id,
            error: value["error"].clone(),
            will_retry: value["willRetry"] == true,
        },
        _ => return Ok(None),
    };
    Ok(Some((
        Deserialize::deserialize(&value["threadId"])?,
        change,
    )))
}

/// Provider-specific notifications end at this adapter boundary.
pub(super) fn event(router: &SessionRouter, message: &RpcMessage<'_>) -> Result<(), String> {
    // Only the Host terminal owner can publish events for client PTY handles.
    if matches!(
        message.method(),
        Some("process/outputDelta" | "process/exited")
    ) {
        return Ok(());
    }
    if message.kind() != RpcMessageKind::Notification {
        return Err("expected Codex notification".into());
    }
    let params: Value = message.params().map_err(|error| error.to_string())?;
    if message.method() == Some("serverRequest/resolved") {
        router.resolve_native_request(ProviderKind::Codex, &params["requestId"]);
    } else if let Some((id, change)) =
        notification_change(message.method().unwrap_or_default(), &params)
            .map_err(|error| error.to_string())?
    {
        router.session_change(
            &SessionRef {
                provider: ProviderKind::Codex,
                id,
            },
            change,
        );
    } else {
        router.broadcast(agent_protocol::protocol::Notification::Provider {
            method: message.method().unwrap_or_default().into(),
            params,
        });
    }
    Ok(())
}

pub(super) fn request(
    router: &SessionRouter,
    mut request: agent_protocol::operations::ServerRequest,
) -> Result<(), String> {
    let id = request
        .params
        .get("threadId")
        .and_then(Value::as_str)
        .ok_or("request session ID is missing")?
        .to_owned();
    let target = SessionRef {
        provider: ProviderKind::Codex,
        id,
    };
    request
        .params
        .insert("threadId".into(), target.thread_id().into());
    router.request(target, request)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn provider_process_events_cannot_mutate_host_owned_terminals() {
        use futures_util::FutureExt;
        let router = super::SessionRouter::new();
        let mut connection = router.open_session(8);
        for method in ["process/outputDelta", "process/exited"] {
            let line = serde_json::json!({"method":method,"params":{"processHandle":"owned","deltaBase64":"aW5qZWN0ZWQ=","exitCode":0}}).to_string();
            super::event(&router, &super::RpcMessage::parse(&line).unwrap()).unwrap();
            assert!(connection.recv().now_or_never().is_none());
        }
    }
    use super::unmaterialized_history;
    use serde_json::json;

    #[test]
    fn empty_history_requires_the_native_unmaterialized_error_for_this_thread() {
        let message = "thread new-thread is not materialized yet; thread/turns/list is unavailable before first user message";
        assert!(unmaterialized_history(
            &json!({"code":-32600,"message":message}).to_string(),
            "new-thread"
        ));
        for error in [
            json!({"code":-32601,"message":"list_turns is not supported yet"}).to_string(),
            json!({"code":-32600,"message":"invalid request"}).to_string(),
            json!({"code":-32603,"message":message}).to_string(),
            json!({"code":-32600,"message":message.replace("new-thread", "another-thread")})
                .to_string(),
            json!({"message":message}).to_string(),
            message.to_owned(),
        ] {
            assert!(!unmaterialized_history(&error, "new-thread"), "{error}");
        }
    }
}
