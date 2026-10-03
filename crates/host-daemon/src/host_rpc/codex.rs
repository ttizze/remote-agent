//! Codex native protocol boundary: execution operations, one shared process,
//! ordered request completion, native cursors and deferred item reads.
use super::service::Failure;
use agent_protocol::{
    models::{Item, ThreadResponse, Turn},
    operations as op,
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use agent_transport::peer::{RpcMessage, RpcMessageKind};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

impl From<AppServerError> for Failure {
    fn from(error: AppServerError) -> Self {
        Self::unknown("provider_unavailable", error)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListParams<'a> {
    pub limit: usize,
    pub sort_key: &'a str,
    pub sort_direction: &'a str,
    pub use_state_db_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

use super::agent::{
    Agent, AgentChange, AgentEvent, AnswerWrite, Identity, SessionPage, SubmissionState, emit,
    session_pages,
};
use agent_protocol::protocol::Call;
use agent_transport::peer::PeerEvent;
use futures_util::{FutureExt, TryStreamExt};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::broadcast;

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

fn unmaterialized_history(code: Option<&str>, message: &str, thread_id: &str) -> bool {
    code == Some("-32600")
        && message
            == format!(
                "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
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
    accounts: Arc<tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    directory: PathBuf,
    instance: uuid::Uuid,
    process: Result<Arc<CodexAppServer>, String>,
    stopped: tokio_util::sync::CancellationToken,
    processed: tokio::sync::watch::Sender<u64>,
}
impl Codex {
    pub(super) fn new(process: Result<Arc<CodexAppServer>, String>) -> Self {
        let directory = process
            .as_ref()
            .ok()
            .map(|server| server.initialize_response().codex_home.clone())
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| {
                directories::BaseDirs::new()
                    .map(|dirs| dirs.home_dir().join(".codex"))
                    .unwrap_or_default()
            });
        Self {
            accounts: Arc::default(),
            restoration_error: tokio::sync::watch::channel(None).0,
            directory,
            instance: uuid::Uuid::new_v4(),
            process,
            stopped: Default::default(),
            processed: tokio::sync::watch::channel(0).0,
        }
    }
    pub(super) async fn enable_accounts(
        &self,
        directory: PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        let accounts = crate::codex_accounts::Accounts::load(
            directory,
            config,
            self.server().map_err(|error| error.to_string())?,
            self.restoration_error.clone(),
        )
        .await
        .inspect_err(|error| {
            self.restoration_error.send_replace(Some(error.clone()));
        })?;
        *self.accounts.lock().await = Some(accounts);
        Ok(())
    }
    pub(super) fn server(&self) -> Result<&CodexAppServer, Failure> {
        if self.stopped.is_cancelled() {
            return Err(Failure::new(
                "provider_unavailable",
                "Codexが終了しました。Hostを再起動すると再接続できます。Claudeの会話は継続できます。",
            ));
        }
        self.process
            .as_deref()
            .map_err(|error| Failure::new("provider_unavailable", error))
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
                result.map_err(|_| Failure::unknown("provider_unavailable", "Codex event pump stopped"))?;
            }
            _ = self.stopped.cancelled() => return Err(Failure::unknown("provider_unavailable", "Codex event stream is unavailable")),
        }
        Ok(())
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
        let mut page = match self
            .request::<_, Page<super::native::NativeTurn>>("thread/turns/list", &query)
            .await
        {
            Ok(page) => Page {
                data: page.data.into_iter().map(|turn| turn.turn).collect(),
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
        self.hydrate_turn_page(&mut page, &query)
            .await
            .map_err(|error| Failure::new("invalid_thread_history", error))?;
        page.data.reverse();
        Ok(History {
            turns: Some(page.data),
            read_state: None,
            has_more: Some(page.next_cursor.is_some_and(|cursor| !cursor.is_empty())),
        })
    }
    // Keep App Server cursors opaque. Both initial hydration and older pages
    // use the desktop five-turn / 500-item initial window, in pages of 100.
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
            let items_loaded = native_full.data.iter().all(|turn| turn.items_loaded);
            let full = Page {
                data: native_full
                    .data
                    .into_iter()
                    .map(|turn| turn.turn)
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
            if !items_loaded {
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
                    .request::<_, Page<HistoryItem>>(
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
                    .await
                    .map_err(|error| error.to_string())?;
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
            .request::<_, Page<HistoryItem>>(
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
            .await
            .map_err(|error| error.to_string())?;
        if let Some(item) = opening
            .into_items()?
            .0
            .into_iter()
            .find(|item| !matches!(item.body(), agent_protocol::items::ItemBody::Compaction {}))
            .filter(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                )
            })
            && !values.iter().any(|value| value.id == item.id)
        {
            turn.opening_user_message = Some(item);
        }
        Ok(())
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
pub(super) fn event_change(
    instance: uuid::Uuid,
    message: &RpcMessage<'_>,
) -> Result<Option<AgentChange>, String> {
    if matches!(
        message.method(),
        Some("process/outputDelta" | "process/exited")
    ) {
        return Ok(None);
    }
    if message.kind() != RpcMessageKind::Notification {
        return Err("expected native notification".into());
    }
    let params: Value = message.params().map_err(|e| e.to_string())?;
    let change = if message.method() == Some("serverRequest/resolved") {
        AgentChange::Resolved {
            instance,
            native_id: params["requestId"].clone(),
        }
    } else if let Some((id, change)) =
        notification_change(message.method().unwrap_or_default(), &params)
            .map_err(|e| e.to_string())?
    {
        AgentChange::Session {
            session: SessionRef::new(ProviderKind::Codex, id).map_err(str::to_owned)?,
            change,
        }
    } else if message.method() == Some("thread/name/updated") {
        AgentChange::Renamed(
            SessionRef::new(
                ProviderKind::Codex,
                params["threadId"]
                    .as_str()
                    .ok_or("renamed session ID is missing")?
                    .into(),
            )
            .map_err(str::to_owned)?,
        )
    } else {
        return Ok(None);
    };
    Ok(Some(change))
}

#[derive(serde::Deserialize)]
struct NativeRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

fn request_change(
    instance: uuid::Uuid,
    stopped: tokio_util::sync::CancellationToken,
    native: NativeRequest,
) -> Result<AgentChange, String> {
    let params = Value::Object(native.params);
    let id = params["threadId"]
        .as_str()
        .ok_or("request session ID is missing")?
        .to_owned();
    let session = SessionRef::new(ProviderKind::Codex, id).map_err(str::to_owned)?;
    let adapted = super::requests::codex(
        uuid::Uuid::new_v4().to_string().into(),
        &native.method,
        &params,
    )?;
    Ok(AgentChange::Request {
        session,
        origin: super::requests::RequestOrigin {
            instance,
            native_id: native.id,
            destination: super::requests::RequestDestination::Codex { stopped },
        },
        adapted,
    })
}

#[async_trait::async_trait]
impl Identity for Codex {
    async fn list(&self) -> Result<op::Accounts, Failure> {
        let mut accounts = self.accounts.lock().await;
        let accounts = accounts.as_mut().ok_or_else(|| {
            Failure::new("account_unavailable", "アカウント管理が利用できません。")
        })?;
        self.server()?;
        accounts
            .list()
            .await
            .map_err(|e| Failure::new("account_operation_failed", e))
    }
    async fn login(&self, request: Call) -> Result<agent_protocol::protocol::Body, Failure> {
        let mut accounts = self.accounts.lock().await;
        accounts
            .as_mut()
            .ok_or_else(|| Failure::new("account_unavailable", "アカウント管理が利用できません。"))?
            .request(self.server()?, request)
            .await
            .map_err(|e| Failure::new("account_operation_failed", e))
    }
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, Failure> {
        let fetch = self
            .accounts
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| Failure::new("account_unavailable", "アカウント管理が利用できません。"))?
            .usage_request(id)
            .map_err(|e| Failure::new("account_operation_failed", e))?;
        Ok(fetch.await)
    }
}

#[async_trait::async_trait]
impl Agent for Codex {
    fn session_state(
        &self,
        status: agent_protocol::models::SessionStatus,
        running_turn: Option<agent_protocol::ids::TurnId>,
    ) -> crate::host_rpc::submission::SessionState {
        let running = status == agent_protocol::models::SessionStatus::Running;
        crate::host_rpc::submission::SessionState {
            status,
            running_turn,
            accepts_steer: running,
            accepts_queue: running,
        }
    }
    fn capabilities(&self) -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            additional_input: true,
            fork: true,
            rename: true,
            model_change: true,
        }
    }
    fn availability(&self) -> Result<(), Failure> {
        self.server().map(|_| ())
    }
    fn validate_create(&self) -> Result<(), Failure> {
        self.availability()
    }
    fn storage_directory(&self) -> &std::path::Path {
        &self.directory
    }
    async fn list(&self, search: &str, cursor: Option<String>) -> Result<SessionPage, Failure> {
        let value: Value = self
            .request(
                "thread/list",
                &ThreadListParams {
                    limit: 100,
                    sort_key: "updated_at",
                    sort_direction: "desc",
                    use_state_db_only: true,
                    search_term: (!search.trim().is_empty()).then_some(search),
                    cursor,
                },
            )
            .await?;
        Ok(SessionPage {
            data: value["data"]
                .as_array()
                .ok_or_else(|| Failure::new("invalid_thread", "native session list is missing"))?
                .iter()
                .cloned()
                .map(super::native::codex_thread)
                .collect::<Result<_, _>>()?,
            next_cursor: serde_json::from_value(value["nextCursor"].clone())?,
        })
    }
    async fn open(&self, id: &str, limit: usize) -> Result<ThreadResponse, Failure> {
        let native: Value = self
            .request(
                "thread/read",
                &serde_json::json!({"threadId":id,"includeTurns":false}),
            )
            .await?;
        let paginated = native["thread"]["historyMode"] == "paginated";
        let mut response = super::native::codex_thread_response(native)?;
        let history = self.history(id, paginated, limit).await?;
        response.thread.turns = history.turns;
        response.thread.history_read_state = history.read_state;
        response.thread.history_has_more = history.has_more;
        Ok(response)
    }

    async fn read_item(
        &self,
        params: &op::ReadItem,
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
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        self.thread_response(
            "thread/start",
            &with_browser_config(serde_json::json!({"cwd":cwd,"model":model}), browser_config),
        )
        .await
    }
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure> {
        if let Some(error) = self.restoration_error.borrow().as_ref() {
            return Err(Failure::new("account_unavailable", error));
        }
        let native: Value = self
            .request(
                "thread/read",
                &serde_json::json!({"threadId":id,"includeTurns":false}),
            )
            .await
            .map_err(Failure::before_submission)?;
        let needs_reload = native["thread"]["status"]["type"] == "notLoaded";
        let response = super::native::codex_thread_response(native)
            .map_err(|error| Failure::new("invalid_thread", error))?;
        Ok(SubmissionState {
            response,
            needs_reload,
        })
    }
    async fn submit(
        &self,
        input: &op::Submission,
        route: super::submission::SubmissionTarget<'_>,
        reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use super::submission::SubmissionTarget;
        let mut params = serde_json::json!({
            "threadId": input.thread_id.id,
            "clientUserMessageId": input.client_user_message_id,
            "input": super::native::codex_input(&input.input),
        });
        let turn_id = match route {
            SubmissionTarget::Steer(turn) => {
                params["expectedTurnId"] = turn.into();
                self.request::<_, agent_protocol::models::Empty>("turn/steer", &params)
                    .await?;
                Some(turn.into())
            }
            SubmissionTarget::Queue => {
                let reply: Value = self.request("thread/queue/add", &params).await?;
                native_turn_id(reply["queuedSubmission"]["id"].as_str())?;
                None
            }
            SubmissionTarget::Start { cwd } => {
                if reload {
                    self.thread_response(
                        "thread/resume",
                        &with_browser_config(
                            serde_json::json!({"threadId":input.thread_id.id,"cwd":cwd}),
                            browser,
                        ),
                    )
                    .await
                    .map_err(Failure::before_submission)?;
                }
                params["model"] = serde_json::json!(input.model.as_ref().map(|model| &model.id));
                params["effort"] = serde_json::json!(input.effort);
                params["serviceTierForTurn"] = serde_json::json!(input.service_tier);
                let reply: Value = self.request("turn/start", &params).await?;
                Some(native_turn_id(reply["turn"]["id"].as_str())?)
            }
        };
        Ok(op::SubmissionReceipt { turn_id })
    }
    async fn interrupt(
        &self,
        id: &str,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        self.request(
            "turn/interrupt",
            &serde_json::json!({"threadId":id,"turnId":turn_id}),
        )
        .await
    }
    async fn answer(
        &self,
        origin: super::requests::RequestOrigin,
        result: Value,
    ) -> Result<AnswerWrite, Failure> {
        self.server().map_err(Failure::before_submission)?;
        if origin.instance != self.instance {
            return Err(Failure::new(
                "answer_not_sent",
                "request source has changed",
            ));
        }
        let process = self
            .process
            .as_ref()
            .map_err(|e| Failure::new("answer_not_sent", e))?
            .clone();
        Ok(async move {
            process
                .send_raw(&serde_json::json!({"id":origin.native_id,"result":result}).to_string())
                .await
                .map_err(|e| Failure::unknown("answer_delivery_unknown", e))
        }
        .boxed())
    }
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
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
    async fn catalog(&self, cwd: &str) -> agent_protocol::composer::ComposerCatalog {
        self.composer_catalog(cwd).await
    }
    async fn active_sessions_in(&self, dir: &std::path::Path) -> Result<Vec<SessionRef>, Failure> {
        let mut active = Vec::new();
        let pages = session_pages(self, "");
        futures_util::pin_mut!(pages);
        while let Some(page) = pages.try_next().await? {
            for thread in page {
                if thread.status == agent_protocol::models::SessionStatus::Running
                    && let Some(cwd) = &thread.cwd
                {
                    let cwd = tokio::fs::canonicalize(cwd)
                        .await
                        .unwrap_or_else(|_| cwd.into());
                    if dunce::simplified(&cwd).starts_with(dir)
                        && let Some(id) = thread.id
                    {
                        active.push(id);
                    }
                }
            }
        }
        Ok(active)
    }
    async fn discard_workspace_processes(&self, _dir: &std::path::Path) -> Result<(), Failure> {
        Ok(())
    }
    async fn read_permissions(
        &self,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        Codex::read_permissions(self).await
    }
    async fn update_permissions(
        &self,
        mode: agent_protocol::permissions::PermissionMode,
        version: &str,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        Codex::update_permissions(self, mode, version).await
    }
    async fn fork(
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
    async fn rename(&self, id: &str, name: &str) -> Result<agent_protocol::models::Empty, Failure> {
        self.request(
            "thread/name/set",
            &serde_json::json!({"threadId":id,"name":name}),
        )
        .await
    }
    fn event_stream(&self) -> Option<tokio::sync::mpsc::Receiver<AgentEvent>> {
        // Subscribe before spawning the pump. Otherwise Codex can emit a
        // server request in the scheduling gap and it would be lost before
        // there is a receiver to retain it for the next phone.
        let Ok(codex) = &self.process else {
            return None;
        };
        let mut events = codex.subscribe();
        let stopped = self.stopped.clone();
        let instance = self.instance;
        let processed = self.processed.clone();
        let accounts = self.accounts.clone();
        let process = self.process.clone();
        let (output, receiver) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(PeerEvent::Message(message)) => {
                        let sequence = message.sequence;
                        let _processed = scopeguard::guard(sequence, |sequence| {
                            processed.send_replace(sequence);
                        });
                        let line = message.value;
                        let Ok(request) = RpcMessage::parse(&line) else {
                            continue;
                        };
                        if request.kind() == RpcMessageKind::Request
                            && request.method() == Some("account/chatgptAuthTokens/refresh")
                        {
                            let accounts = accounts.clone();
                            let process = process.clone();
                            tokio::spawn(async move {
                                #[derive(Deserialize)]
                                #[serde(rename_all = "camelCase")]
                                struct Refresh<'a> {
                                    previous_account_id: Option<&'a str>,
                                }
                                let Ok(request) = RpcMessage::parse(&line) else {
                                    return;
                                };
                                let Ok(params) = request.params::<Refresh>() else {
                                    return;
                                };
                                let mut accounts = accounts.lock().await;
                                let result = match accounts.as_mut() {
                                    Some(accounts) => {
                                        accounts.refresh(params.previous_account_id).await
                                    }
                                    None => Err("アカウントを選択してください。".into()),
                                };
                                let response = match result {
                                    Ok(result) => request.response::<_, ()>(Ok(result)),
                                    Err(error) => request.error(-32000, &error),
                                };
                                let Ok(response) = response else {
                                    return;
                                };
                                let line = zeroize::Zeroizing::new(response);
                                if let Ok(codex) = &process {
                                    let _ = codex.send_raw(&line).await;
                                }
                            });
                        } else if request.kind() == RpcMessageKind::Request {
                            let admission = match serde_json::from_str(&line)
                                .map_err(|e| e.to_string())
                                .and_then(|request| {
                                    request_change(instance, stopped.clone(), request)
                                }) {
                                Ok(change) => emit(&output, change).await,
                                Err(error) => Err(error),
                            };
                            if let Err(error) = &admission {
                                tracing::warn!(target: "bex", operation = "host.codex.request_rejected", message = %error);
                            }
                            if let Err(error) = admission
                                && let Ok(codex) = &process
                                && let Ok(response) = request.error(-32000, &error)
                            {
                                let _ = codex.send_raw(&response).await;
                            }
                        } else {
                            let result = match event_change(instance, &request) {
                                Ok(Some(change)) => emit(&output, change).await,
                                Ok(None) => Ok(()),
                                Err(error) => Err(error),
                            };
                            if let Err(error) = result {
                                tracing::error!(target:"bex",operation="host.codex.event",message=%error);
                                break;
                            }
                        }
                    }
                    Ok(PeerEvent::Response { sequence, .. }) => {
                        processed.send_replace(sequence);
                    }
                    Ok(PeerEvent::Closed(_))
                    | Err(broadcast::error::RecvError::Closed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => break,
                }
            }
            stopped.cancel();
            if let Ok(codex) = &process
                && let Err(error) = codex.shutdown().await
            {
                tracing::error!(target: "bex", operation = "host.codex.shutdown", message = %error);
            }
            let _=emit(&output,AgentChange::Stopped {provider:ProviderKind::Codex,reason:"エージェントとの接続が終了しました。Hostを再起動してから再送信してください。".into()}).await;
        });
        Some(receiver)
    }
    async fn shutdown(&self) {
        self.stopped.cancel();
        if let Ok(process) = &self.process {
            let _ = process.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
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
        let router = super::super::routing::SessionRouter::new();
        let mut connection = router.open_session();
        for method in ["process/outputDelta", "process/exited"] {
            let line = serde_json::json!({"method":method,"params":{"processHandle":"owned","deltaBase64":"aW5qZWN0ZWQ=","exitCode":0}}).to_string();
            if let Some(change) =
                super::event_change(uuid::Uuid::nil(), &super::RpcMessage::parse(&line).unwrap())
                    .unwrap()
            {
                change.apply(&router).unwrap();
            }
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
