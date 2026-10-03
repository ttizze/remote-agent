//! Codex native protocol boundary: execution operations, one shared process,
//! ordered request completion, native cursors and deferred item reads.
use super::{routing::SessionRouter, service::Failure};
use agent_protocol::{
    models::{Item, Thread, ThreadResponse, Turn},
    operations as op,
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use agent_transport::peer::{RpcMessage, RpcMessageKind};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

impl From<AppServerError> for Failure {
    fn from(error: AppServerError) -> Self {
        Self::unknown("codex_unavailable", error)
    }
}

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
struct History {
    turns: Option<Vec<Arc<Turn>>>,
    read_state: Option<agent_protocol::session::HistoryReadState>,
    has_more: Option<bool>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryItem {
    #[serde(deserialize_with = "super::native::deserialize_item")]
    pub item: Arc<Item>,
    pub turn_id: Option<String>,
}

#[derive(Deserialize)]
struct TimelineEntry {
    position: u64,
    #[serde(flatten)]
    event: serde_json::Map<String, Value>,
}

/// Positions preserve canonical item order; start boundaries prove completeness.
fn timeline_turns(
    turns: Vec<Arc<Turn>>,
    entries: Vec<TimelineEntry>,
    exhausted: bool,
) -> Result<Vec<Arc<Turn>>, String> {
    let mut positions = std::collections::BTreeMap::new();
    for entry in entries {
        if positions
            .insert(entry.position, Value::Object(entry.event))
            .is_some()
        {
            return Err("timeline position repeated".into());
        }
    }
    let mut turns: Vec<_> = turns.into_iter().map(Arc::unwrap_or_clone).collect();
    let indices: std::collections::HashMap<_, _> = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.id.to_string(), index))
        .collect();
    for turn in &mut turns {
        turn.items = Some(Vec::new());
        turn.items_has_more = Some(!exhausted);
    }
    for mut event in positions.into_values() {
        let id = event["turnId"].as_str().unwrap_or_default();
        if id.is_empty() && event["type"] != "realtime" {
            return Err("timeline item identity is missing".into());
        }
        let index = indices.get(id).copied();
        match event["type"].as_str() {
            Some("item") => {
                let item = super::native::codex_item(event["item"].take())
                    .map_err(|error| error.to_string())?;
                if item.id.is_empty() {
                    return Err("timeline item identity is missing".into());
                }
                if let Some(index) = index {
                    turns[index].items.as_mut().unwrap().push(Arc::new(item));
                }
            }
            Some("turnStarted") => {
                if let Some(index) = index {
                    turns[index].items_has_more = Some(false);
                }
            }
            Some("turnCompleted") => {
                event["id"] = event["turnId"].take();
                let completed =
                    super::native::codex_turn(event).map_err(|error| error.to_string())?;
                if let Some(index) = index {
                    let turn = &mut turns[index];
                    turn.status = completed.status;
                    turn.started_at = completed.started_at;
                    turn.completed_at = completed.completed_at;
                    turn.duration_ms = completed.duration_ms;
                    turn.error = completed.error;
                }
            }
            Some("realtime") => {}
            _ => return Err("unknown timeline event".into()),
        }
    }
    Ok(turns
        .into_iter()
        .map(|mut turn| {
            turn.items_view = Some(
                if turn.items_has_more == Some(false) {
                    "full"
                } else if turn.items.as_ref().unwrap().is_empty() {
                    "notLoaded"
                } else {
                    "summary"
                }
                .into(),
            );
            Arc::new(turn)
        })
        .collect())
}

fn unmaterialized_history(code: Option<&str>, message: &str, thread_id: &str) -> bool {
    code == Some("-32600")
        && message
            == format!(
                "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
            )
}

impl Page<HistoryItem> {
    fn into_items(self, limit: usize) -> Result<Vec<Arc<Item>>, &'static str> {
        if self.data.len() > limit {
            return Err("item page exceeds requested size");
        }
        self.data
            .into_iter()
            .map(|entry| {
                if entry.item.id.is_empty() {
                    Err("history item ID is missing")
                } else {
                    Ok(entry.item)
                }
            })
            .collect()
    }
}

pub(super) struct Codex {
    pub(super) instance: uuid::Uuid,
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
            instance: uuid::Uuid::new_v4(),
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

    // Also used by the Codex catalog and permission adapter implementations.
    pub(super) async fn request<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<T, Failure> {
        let reply = self
            .server()?
            .request_sequenced::<_, T>(method, params)
            .await
            .map_err(Failure::from)?;
        self.wait_for_events(reply.sequence).await?;
        reply.value.outcome.map_err(|error| native_failure(&error))
    }

    async fn thread_response<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<ThreadResponse, Failure> {
        super::native::codex_thread_response(self.request(method, params).await?)
            .map_err(Into::into)
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

    /// Read current routing evidence without loading conversation history.
    pub(super) async fn submission_state(
        &self,
        id: &str,
    ) -> Result<(ThreadResponse, bool), Failure> {
        let native: Value = self
            .request(
                "thread/read",
                &serde_json::json!({"threadId":id,"includeTurns":false}),
            )
            .await
            .map_err(Failure::before_submission)?;
        let requires_resume = native["thread"]["status"]["type"] == "notLoaded";
        let response = super::native::codex_thread_response(native)
            .map_err(|error| Failure::new("invalid_thread", error))?;
        Ok((response, requires_resume))
    }

    pub(super) async fn steer(
        &self,
        id: &str,
        turn_id: &str,
        client_input_id: &str,
        input: &[op::Input],
    ) -> Result<(), Failure> {
        self.request::<_, agent_protocol::models::Empty>(
            "turn/steer",
            &serde_json::json!({
                "threadId":id,
                "expectedTurnId":turn_id,
                "clientUserMessageId":client_input_id,
                "input":super::native::codex_input(input),
            }),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn queue_input(
        &self,
        id: &str,
        client_input_id: &str,
        input: &[op::Input],
    ) -> Result<(), Failure> {
        let reply: Value = self
            .request(
                "thread/queue/add",
                &serde_json::json!({
                    "threadId":id,
                    "clientUserMessageId":client_input_id,
                    "input":super::native::codex_input(input),
                }),
            )
            .await?;
        native_turn_id(reply["queuedSubmission"]["id"].as_str())?;
        Ok(())
    }

    pub(super) async fn start_turn(
        &self,
        input: &op::Submission,
        cwd: &str,
        resume: bool,
        browser_config: Option<Value>,
    ) -> Result<agent_protocol::ids::TurnId, Failure> {
        if resume {
            self.thread_response(
                "thread/resume",
                &with_browser_config(
                    serde_json::json!({"threadId":input.thread_id.id,"cwd":cwd}),
                    browser_config,
                ),
            )
            .await
            .map_err(Failure::before_submission)?;
        }
        let reply: Value = self
            .request(
                "turn/start",
                &serde_json::json!({
                    "threadId":input.thread_id.id,
                    "clientUserMessageId":input.client_user_message_id,
                    "input":super::native::codex_input(&input.input),
                    "model":input.model.as_ref().map(|model| &model.id),
                    "effort":input.effort,
                    "serviceTierForTurn":input.service_tier,
                }),
            )
            .await?;
        native_turn_id(reply["turn"]["id"].as_str())
    }

    pub(super) async fn create(
        &self,
        cwd: Option<&str>,
        model: Option<&str>,
        project_id: Option<&str>,
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        self.thread_response(
            "thread/start",
            &with_browser_config(
                serde_json::json!({"cwd":cwd,"model":model,"projectId":project_id}),
                browser_config,
            ),
        )
        .await
    }

    pub(super) async fn fork(
        &self,
        id: &str,
        last_turn_id: &str,
        exclude_turns: bool,
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        self.thread_response(
            "thread/fork",
            &with_browser_config(
                serde_json::json!({"threadId":id,"lastTurnId":last_turn_id,"excludeTurns":exclude_turns}),
                browser_config,
            ),
        )
        .await
    }

    pub(super) async fn interrupt(
        &self,
        id: &str,
        turn_id: &str,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        self.request(
            "turn/interrupt",
            &serde_json::json!({"threadId":id,"turnId":turn_id}),
        )
        .await
    }

    pub(super) async fn rename(
        &self,
        id: &str,
        name: &str,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        self.request(
            "thread/name/set",
            &serde_json::json!({"threadId":id,"name":name}),
        )
        .await
    }

    pub(super) async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        let mut native: Value = self.request("model/list", params).await?;
        let data = native["data"]
            .as_array_mut()
            .ok_or_else(|| Failure::new("invalid_models", "native model catalog is missing"))?;
        for model in data {
            let id = model["model"].take();
            model["model"] = serde_json::json!({"provider":"codex","id":id});
        }
        serde_json::from_value(native).map_err(Into::into)
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
                .map_err(|error| native_failure(&error))?;
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
        })).await.map_err(Failure::from)?.outcome.map_err(|error| native_failure(&error))?;
        Ok(())
    }

    pub(super) async fn thread_page(
        &self,
        params: &ThreadListParams<'_>,
    ) -> Result<Page<Thread>, Failure> {
        let value: Value = self.request("thread/list", params).await?;
        let data = value["data"]
            .as_array()
            .ok_or_else(|| Failure::new("invalid_thread", "native session list is missing"))?
            .iter()
            .cloned()
            .map(super::native::codex_thread)
            .collect::<Result<_, _>>()?;
        Ok(Page {
            data,
            next_cursor: serde_json::from_value(value["nextCursor"].clone())?,
        })
    }

    pub(super) async fn read(&self, id: &str, limit: usize) -> Result<ThreadResponse, Failure> {
        let mut response: ThreadResponse = self
            .thread_response(
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
        response.thread.history_read_state = history.read_state;
        response.thread.history_has_more = history.has_more;
        Ok(response)
    }

    async fn history(&self, id: &str, paginated: bool, limit: usize) -> Result<History, Failure> {
        if !paginated {
            let mut thread = self
                .thread_response(
                    "thread/read",
                    &serde_json::json!({"threadId":id,"includeTurns":true}),
                )
                .await?
                .thread;
            if thread.id.as_ref().map(|session| session.id.as_str()) != Some(id) {
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
            return Ok(History {
                turns: thread.turns,
                read_state: thread.history_read_state,
                has_more: thread.history_has_more,
            });
        }
        let query = HistoryParams {
            thread_id: id,
            turn_id: None,
            cursor: None,
            limit,
            sort_direction: "desc",
            items_view: Some("notLoaded"),
        };
        let page = match self
            .request::<_, Page<super::native::NativeTurn>>("thread/turns/list", &query)
            .await
        {
            Ok(page) => Page {
                data: page.data.into_iter().map(|turn| turn.0).collect(),
                next_cursor: page.next_cursor,
            },
            Err(error)
                if unmaterialized_history(
                    error
                        .execution
                        .as_ref()
                        .and_then(|execution| execution.provider_code.as_deref()),
                    &error.to_string(),
                    id,
                ) =>
            {
                // Native Codex explicitly confirms there is no persisted first
                // message. The session router overlays any in-flight live turn.
                Page {
                    data: Vec::new(),
                    next_cursor: None,
                }
            }
            Err(error)
                if error
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.provider_code.as_deref())
                    == Some("-32601") =>
            {
                // Native servers advertise pagination before materializing a
                // first turn. Read that same native session without cursors.
                return match Box::pin(self.history(id, false, limit)).await {
                    Ok(history) => Ok(history),
                    Err(error) => Ok(History {
                        turns: None,
                        has_more: None,
                        read_state: Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Unavailable,
                            vec![error.to_string()],
                        )),
                    }),
                };
            }
            Err(error) => return Err(Failure::new("invalid_thread_history", error)),
        };
        let mut page = self
            .hydrate_turn_page(page, &query)
            .await
            .map_err(|error| Failure::new("invalid_thread_history", error))?;
        page.data.reverse();
        Ok(History {
            turns: Some(page.data),
            read_state: None,
            has_more: Some(page.next_cursor.is_some_and(|cursor| !cursor.is_empty())),
        })
    }
    // Read a bounded timeline window rather than paging each turn separately.
    // Expansion follows opaque timeline cursors in pages of at most 500 entries.
    async fn hydrate_turn_page(
        &self,
        mut page: Page<Arc<Turn>>,
        query: &HistoryParams<'_>,
    ) -> Result<Page<Arc<Turn>>, String> {
        if page.data.len() > query.limit {
            return Err("turn page exceeds requested size".into());
        }
        let thread_id = query.thread_id;
        let mut ids = std::collections::HashSet::new();
        let repeated = page.data.iter().any(|turn| !ids.insert(turn.id.as_str()));
        if repeated {
            // Timeline hydration groups items by turn ID. Read full turns to
            // keep each repeated historical occurrence's own items.
            let native_full = self
                .request::<_, Page<super::native::NativeTurn>>(
                    "thread/turns/list",
                    &HistoryParams {
                        items_view: Some("full"),
                        ..*query
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let full = Page {
                data: native_full
                    .data
                    .into_iter()
                    .map(|turn| turn.0)
                    .collect::<Vec<_>>(),
                next_cursor: native_full.next_cursor,
            };
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
            return Ok(full);
        }
        if page.data.is_empty() {
            return Ok(page);
        }
        let budget = query.limit.saturating_mul(100).min(100_000);
        let mut entries = Vec::new();
        let mut cursor = None;
        let mut cursors = std::collections::HashSet::new();
        // A provider may return a smaller page than requested. Do not fill
        // the initial window by waiting for more pages; expansion adds pages.
        for offset in (0..budget).step_by(500) {
            let limit = (budget - offset).min(500);
            let timeline: Page<TimelineEntry> = self
                .request(
                    "thread/timeline/list",
                    &serde_json::json!({"threadId":thread_id,"cursor":cursor,"limit":limit}),
                )
                .await
                .map_err(|error| error.to_string())?;
            if timeline.data.len() > limit {
                return Err("timeline page exceeds requested size".into());
            }
            entries.extend(timeline.data);
            cursor = timeline.next_cursor.filter(|cursor| !cursor.is_empty());
            if let Some(cursor) = &cursor
                && !cursors.insert(cursor.clone())
            {
                return Err("timeline cursor repeated".into());
            }
            if cursor.is_none() {
                break;
            }
        }
        page.data = timeline_turns(page.data, entries, cursor.is_none())?;
        page.next_cursor = page.next_cursor.or(cursor);
        // Only a truncated turn needs a separate read for its opening question.
        for turn in &mut page.data {
            let turn = Arc::make_mut(turn);
            let items = turn.items.as_deref().unwrap();
            if turn.items_has_more == Some(true)
                && !items.is_empty()
                && let Some(item) = self.opening_question(thread_id, &turn.id, items).await?
            {
                turn.opening_user_message = Some(item);
            }
        }
        Ok(page)
    }

    async fn opening_question(
        &self,
        thread_id: &str,
        turn_id: &str,
        items: &[Arc<Item>],
    ) -> Result<Option<Arc<Item>>, String> {
        let opening = self
            .request::<_, Page<HistoryItem>>(
                "thread/items/list",
                &HistoryParams {
                    thread_id,
                    turn_id: Some(turn_id),
                    cursor: None,
                    limit: 2,
                    sort_direction: "asc",
                    items_view: None,
                },
            )
            .await
            .map_err(|error| error.to_string())?
            .into_items(2)?;
        Ok(opening
            .into_iter()
            .find(|item| !matches!(item.body(), agent_protocol::items::ItemBody::Compaction {}))
            .filter(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                ) && !items.iter().any(|value| value.id == item.id)
            }))
    }

    pub(super) async fn item_read(
        &self,
        params: op::ReadItem,
    ) -> Result<agent_protocol::operations::ItemResponse, Failure> {
        if [
            params.thread_id.id.as_str(),
            params.turn_id.as_str(),
            params.item_id.as_str(),
        ]
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
                thread_id: &params.thread_id.id,
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
            let page = response.outcome.map_err(|error| native_failure(&error))?;
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

fn native_failure(native: &serde_json::value::RawValue) -> Failure {
    let value: Value = serde_json::from_str(native.get()).unwrap_or_default();
    let mut execution = super::native::codex_error(&value, false);
    execution.message = agent_transport::diagnostics::sanitize(&execution.message);
    if execution.message.trim().is_empty() {
        execution.message = "接続先で操作に失敗しました。もう一度お試しください。".into();
    }
    execution.details = execution
        .details
        .map(|text| agent_transport::diagnostics::sanitize(&text));
    if execution.provider_code.is_none() {
        execution.provider_code = value["code"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| value["code"].as_i64().map(|code| code.to_string()));
    }
    let mut failure = Failure::unknown("provider_failed", &execution.message);
    failure.execution = Some(Box::new(execution));
    failure
}

fn with_browser_config(mut params: Value, browser: Option<Value>) -> Value {
    if let Some(browser) = browser {
        params["config"] = serde_json::json!({"mcp_servers.bex_browser":browser});
    }
    params
}

fn native_turn_id(id: Option<&str>) -> Result<agent_protocol::ids::TurnId, Failure> {
    id.filter(|id| !id.trim().is_empty())
        .map(Into::into)
        .ok_or_else(|| {
            Failure::unknown(
                "invalid_submission_reply",
                "native submission ID is missing",
            )
        })
}

/// Decode only native conversation events. Every other message keeps its owner.
fn notification_change(
    method: &str,
    value: &Value,
) -> Result<Option<(String, SessionChange)>, serde_json::Error> {
    use serde::Deserialize;
    let turn_id: agent_protocol::ids::TurnId = value["turnId"].as_str().unwrap_or_default().into();
    let change = match method {
        "thread/status/changed" => SessionChange::Status {
            status: super::native::session_status(&value["status"]),
        },
        "turn/started" | "turn/completed" => {
            let mut turn = super::native::codex_turn(value["turn"].clone())?;
            turn.items_view.get_or_insert_with(|| "full".into());
            turn.items_has_more.get_or_insert(false);
            SessionChange::Turn {
                turn,
                completed: method == "turn/completed",
            }
        }
        "item/started" | "item/completed" => {
            let mut item = super::native::codex_item(value["item"].clone())?;
            if item.status == agent_protocol::execution::ItemStatus::Unknown {
                item.status = if method == "item/completed" {
                    agent_protocol::execution::ItemStatus::Completed
                } else {
                    agent_protocol::execution::ItemStatus::Running
                };
            }
            SessionChange::Item {
                turn_id,
                item: item.into(),
            }
        }
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
                    item: super::native::approval_review(
                        id,
                        &value["review"],
                        &value["action"],
                        Deserialize::deserialize(&value["targetItemId"])?,
                        Deserialize::deserialize(&value["startedAtMs"])?,
                        Deserialize::deserialize(&value["completedAtMs"])?,
                    )?
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
                "item/agentMessage/delta" => TextField::AssistantText,
                "item/commandExecution/outputDelta" => TextField::CommandOutput,
                "item/fileChange/outputDelta" => TextField::FileOutput,
                "item/reasoning/textDelta" => TextField::ReasoningContent {
                    index: Deserialize::deserialize(&value["contentIndex"])?,
                },
                _ => TextField::ReasoningSummary {
                    index: Deserialize::deserialize(&value["summaryIndex"])?,
                },
            },
        },
        "item/reasoning/summaryPartAdded" => SessionChange::ReasoningPart {
            turn_id,
            item_id: Deserialize::deserialize(&value["itemId"])?,
            field: agent_protocol::session::ReasoningField::Summary,
            index: Deserialize::deserialize(&value["summaryIndex"])?,
        },
        "error" => SessionChange::Error {
            turn_id,
            error: super::native::codex_error(&value["error"], value["willRetry"] == true),
        },
        _ => return Ok(None),
    };
    Ok(Some((
        Deserialize::deserialize(&value["threadId"])?,
        change,
    )))
}

/// Provider-specific notifications end at this adapter boundary.
pub(super) fn event(
    router: &SessionRouter,
    instance: uuid::Uuid,
    message: &RpcMessage<'_>,
) -> Result<(), String> {
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
        router.resolve_native_request(instance, &params["requestId"]);
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
    } else if message.method() == Some("thread/name/updated") {
        let id = params["threadId"]
            .as_str()
            .ok_or("renamed session ID is missing")?
            .to_owned();
        router.broadcast(agent_protocol::protocol::Notification::SessionRenamed {
            session: SessionRef::new(ProviderKind::Codex, id).map_err(str::to_owned)?,
        });
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub(super) struct NativeRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

pub(super) fn request(
    router: &SessionRouter,
    instance: uuid::Uuid,
    stopped: tokio_util::sync::CancellationToken,
    native: NativeRequest,
) -> Result<(), String> {
    let params = Value::Object(native.params);
    let id = params["threadId"]
        .as_str()
        .ok_or("request session ID is missing")?
        .to_owned();
    let target = SessionRef::new(ProviderKind::Codex, id).map_err(str::to_owned)?;
    let adapted = super::requests::codex(
        uuid::Uuid::new_v4().to_string().into(),
        &native.method,
        &params,
    )?;
    router.request(
        target,
        super::requests::RequestOrigin {
            instance,
            native_id: native.id,
            destination: super::requests::RequestDestination::Codex { stopped },
        },
        adapted,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeline_hydration_preserves_order_boundaries_and_terminal_metadata() {
        use super::*;
        use serde_json::json;
        let entries = json!([
            {"type":"item","position":9,"turnId":"finished","item":{"id":"later","type":"agentMessage","text":"later"}},
            {"type":"turnCompleted","position":10,"turnId":"finished","status":"failed","startedAt":1.5,"completedAt":3.5,"durationMs":2000,"error":{"message":"saved failure"}},
            {"type":"item","position":3,"turnId":"partial","item":{"id":"repeated","type":"agentMessage","text":"second occurrence"}},
            {"type":"realtime","position":5,"item":{}},
            {"type":"turnStarted","position":7,"turnId":"finished"},
            {"type":"item","position":8,"turnId":"finished","item":{"id":"earlier","type":"agentMessage","text":"earlier"}},
            {"type":"item","position":2,"turnId":"partial","item":{"id":"repeated","type":"agentMessage","text":"first occurrence"}}
        ]);
        for exhausted in [false, true] {
            let turns = ["finished", "partial", "unloaded"]
                .map(|id| {
                    Arc::new(Turn {
                        id: id.into(),
                        status: agent_protocol::execution::TurnStatus::Running,
                        ..Default::default()
                    })
                })
                .into();
            let turns = timeline_turns(
                turns,
                serde_json::from_value(entries.clone()).unwrap(),
                exhausted,
            )
            .unwrap();
            assert_eq!(
                turns[0].status,
                agent_protocol::execution::TurnStatus::Failed
            );
            assert_eq!(
                (
                    turns[0].started_at,
                    turns[0].completed_at,
                    turns[0].duration_ms
                ),
                (Some(1.5), Some(3.5), Some(2000))
            );
            assert_eq!(turns[0].error.as_ref().unwrap().message, "saved failure");
            assert_eq!(turns[0].items_view.as_deref(), Some("full"));
            assert_eq!(turns[0].items_has_more, Some(false));
            assert_eq!(
                turns[0]
                    .items
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                ["earlier", "later"]
            );
            let partial = turns[1].items.as_ref().unwrap();
            assert_eq!(
                partial.len(),
                2,
                "repeated item IDs must retain both occurrences"
            );
            assert!(
                matches!(partial[0].body(), agent_protocol::items::ItemBody::AssistantText {text, ..} if text == "first occurrence")
            );
            assert!(
                matches!(partial[1].body(), agent_protocol::items::ItemBody::AssistantText {text, ..} if text == "second occurrence")
            );
            assert_eq!(
                turns[1].items_view.as_deref(),
                Some(if exhausted { "full" } else { "summary" })
            );
            assert_eq!(
                turns[2].items_view.as_deref(),
                Some(if exhausted { "full" } else { "notLoaded" })
            );
            assert_eq!(turns[1].items_has_more, Some(!exhausted));
            assert_eq!(turns[2].items_has_more, Some(!exhausted));
        }
    }

    #[test]
    fn empty_provider_errors_keep_a_localized_recovery_message() {
        for message in ["", "  "] {
            let raw =
                serde_json::value::to_raw_value(&serde_json::json!({"message":message})).unwrap();
            let failure: agent_protocol::error::RpcFailure = super::native_failure(&raw).into();
            assert_eq!(
                failure.message,
                "接続先で操作に失敗しました。もう一度お試しください。"
            );
            assert_eq!(failure.execution.unwrap().message, failure.message);
        }
    }

    #[test]
    fn native_error_fields_never_prove_non_delivery() {
        let native = serde_json::json!({"code":123,"message":"not sent","delivery":"notSent","details":{"kept":true}});
        let response = super::native_failure(&serde_json::value::to_raw_value(&native).unwrap());
        let response = agent_protocol::protocol::Response::from_result::<(), _>(Err(response))
            .unwrap()
            .into_value();
        assert_eq!(response["error"]["delivery"], "unknown");
        assert!(response["error"].get("providerError").is_none());
        assert_eq!(response["error"]["execution"]["providerCode"], "123");
        assert_eq!(response["error"]["execution"]["category"], "other");
        assert_eq!(response["error"]["message"], "not sent");
    }

    #[test]
    fn malformed_provider_reply_does_not_prove_non_delivery() {
        let error: agent_protocol::error::RpcFailure =
            super::native_turn_id(None).unwrap_err().into();
        assert_eq!(error.delivery, agent_protocol::error::Delivery::Unknown);
    }

    #[tokio::test]
    async fn provider_process_events_cannot_mutate_host_owned_terminals() {
        use futures_util::FutureExt;
        let router = super::SessionRouter::new();
        let mut connection = router.open_session();
        for method in ["process/outputDelta", "process/exited"] {
            let line = serde_json::json!({"method":method,"params":{"processHandle":"owned","deltaBase64":"aW5qZWN0ZWQ=","exitCode":0}}).to_string();
            super::event(
                &router,
                uuid::Uuid::nil(),
                &super::RpcMessage::parse(&line).unwrap(),
            )
            .unwrap();
            assert!(connection.recv().now_or_never().is_none());
        }
    }
    #[test]
    fn empty_history_requires_the_native_unmaterialized_error_for_this_session() {
        let message = "thread new-thread is not materialized yet; thread/turns/list is unavailable before first user message";
        assert!(super::unmaterialized_history(
            Some("-32600"),
            message,
            "new-thread"
        ));
        for (code, message, id) in [
            (Some("-32601"), message, "new-thread"),
            (Some("-32600"), "invalid request", "new-thread"),
            (Some("-32603"), message, "new-thread"),
            (Some("-32600"), message, "another-thread"),
            (None, message, "new-thread"),
        ] {
            assert!(!super::unmaterialized_history(code, message, id));
        }
    }
}
