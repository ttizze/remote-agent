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
    Agent, AgentChange, AgentEvent, AnswerWrite, Identity, SessionPage, SessionSummary,
    SubmissionState, emit, session_pages,
};
use agent_transport::peer::PeerEvent;
use futures_util::{FutureExt, TryStreamExt};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::broadcast;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
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

/// Build only the contiguous turns present in this page, preserving occurrences.
fn timeline_turns(entries: Vec<TimelineEntry>, exhausted: bool) -> Result<Vec<Arc<Turn>>, String> {
    let mut positions = std::collections::BTreeMap::new();
    for entry in entries {
        if positions
            .insert(entry.position, Value::Object(entry.event))
            .is_some()
        {
            return Err("timeline position repeated".into());
        }
    }
    let mut turns: Vec<Turn> = Vec::new();
    for mut event in positions.into_values() {
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        if kind == "realtime" {
            continue;
        }
        let id = event["turnId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("timeline item identity is missing")?
            .to_owned();
        if kind == "turnStarted" || turns.last().is_none_or(|turn| turn.id.as_str() != id) {
            turns.push(Turn {
                id: id.into(),
                items: Some(Vec::new()),
                items_has_more: Some(!exhausted),
                ..Default::default()
            });
        }
        let turn = turns.last_mut().unwrap();
        match kind.as_str() {
            "item" => {
                let item = super::native::codex_item(event["item"].take())
                    .map_err(|error| error.to_string())?;
                if item.id.is_empty() {
                    return Err("timeline item identity is missing".into());
                }
                turn.items.as_mut().unwrap().push(Arc::new(item));
            }
            "turnStarted" => {
                turn.items_has_more = Some(false);
                turn.started_at =
                    serde_json::from_value(event["startedAt"].take()).map_err(|e| e.to_string())?;
            }
            "turnCompleted" => {
                event["id"] = event["turnId"].take();
                let mut completed = super::native::codex_turn(event).map_err(|e| e.to_string())?;
                completed.items = turn.items.take();
                completed.items_has_more = turn.items_has_more;
                *turn = completed;
            }
            _ => return Err("unknown timeline event".into()),
        }
    }
    Ok(turns.into_iter().map(Arc::new).collect())
}

fn unmaterialized_history(code: Option<&str>, message: &str, thread_id: &str) -> bool {
    code == Some("-32600")
        && message
            == format!(
                "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
            )
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

    async fn timeline_page(
        &self,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        // Native Codex caps a timeline page at 100 events. One request owns one
        // page; older reads continue from its cursor instead of reloading it.
        let page: Page<TimelineEntry> = self
            .request(
                "thread/timeline/list",
                &serde_json::json!({"threadId":id,"cursor":cursor,"limit":100}),
            )
            .await?;
        if page.data.len() > 100 {
            return Err(Failure::new(
                "invalid_thread_history",
                "timeline page exceeds requested size",
            ));
        }
        let next_cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
        if next_cursor
            .as_deref()
            .is_some_and(|next| Some(next) == cursor)
        {
            return Err(Failure::new(
                "invalid_thread_history",
                "timeline cursor repeated",
            ));
        }
        Ok(agent_protocol::session::HistoryPage {
            turns: timeline_turns(page.data, next_cursor.is_none())
                .map_err(|error| Failure::new("invalid_thread_history", error))?,
            next_cursor,
        })
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

struct RequestSource {
    process: Arc<CodexAppServer>,
    stopped: tokio_util::sync::CancellationToken,
    answers: super::requests::NativeAnswers,
}
pub(crate) fn request_origin(
    instance: uuid::Uuid,
    native_id: Value,
    stopped: tokio_util::sync::CancellationToken,
    process: Arc<CodexAppServer>,
    answers: super::requests::NativeAnswers,
) -> super::requests::RequestOrigin {
    super::requests::RequestOrigin {
        instance,
        native_id,
        provider: ProviderKind::Codex,
        source: Arc::new(RequestSource {
            process,
            stopped,
            answers,
        }),
    }
}
#[async_trait::async_trait]
impl super::requests::AnswerSource for RequestSource {
    fn is_alive(&self) -> bool {
        !self.stopped.is_cancelled()
    }
    async fn prepare(
        &self,
        native_id: &Value,
        body: &agent_protocol::requests::RequestBody,
        answer: &agent_protocol::requests::Answer,
    ) -> Result<AnswerWrite, Failure> {
        let process = self.process.clone();
        if self.stopped.is_cancelled() {
            return Err(Failure::new("answer_not_sent", "request source has ended"));
        }
        let result = self
            .answers
            .translate(body, answer)
            .map_err(|e| Failure::new("invalid_answer", e))?;
        let line = serde_json::json!({"id":native_id,"result":result}).to_string();
        Ok(async move {
            process
                .send_raw(&line)
                .await
                .map_err(|e| Failure::unknown("answer_delivery_unknown", e))
        }
        .boxed())
    }
}

fn request_change(
    instance: uuid::Uuid,
    stopped: tokio_util::sync::CancellationToken,
    native: NativeRequest,
    process: Arc<CodexAppServer>,
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
        origin: request_origin(instance, native.id, stopped, process, adapted.answers),
        request: adapted.request,
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
    async fn account(
        &self,
        command: super::agent::AccountCommand,
    ) -> Result<super::agent::AccountReply, Failure> {
        let mut accounts = self.accounts.lock().await;
        accounts
            .as_mut()
            .ok_or_else(|| Failure::new("account_unavailable", "アカウント管理が利用できません。"))?
            .request(self.server()?, command)
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
    fn running_input(&self) -> super::submission::RunningInput {
        super::submission::RunningInput::SteerOrQueue
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
                .map(|value| {
                    let branch = value["gitInfo"]["branch"].as_str().map(str::to_owned);
                    super::native::codex_thread(value)
                        .map(|thread| SessionSummary { thread, branch })
                })
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
        if paginated {
            let page = match self.timeline_page(id, None).await {
                Ok(page) => page,
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
                    agent_protocol::session::HistoryPage {
                        turns: Vec::new(),
                        next_cursor: None,
                    }
                }
                Err(error) => return Err(error),
            };
            response.thread.turns = Some(page.turns);
            response.thread.history_has_more = Some(page.next_cursor.is_some());
            response.thread.history_cursor = page.next_cursor;
        } else {
            response = self
                .thread_response(
                    "thread/read",
                    &serde_json::json!({"threadId":id,"includeTurns":true}),
                )
                .await?;
            if response
                .thread
                .id
                .as_ref()
                .map(|session| session.id.as_str())
                != Some(id)
            {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "native session identity changed",
                ));
            }
            if let Some(turns) = &mut response.thread.turns
                && turns.len() > limit
            {
                turns.drain(..turns.len() - limit);
                response.thread.history_has_more = Some(true);
            }
        }
        if response.thread.status == agent_protocol::models::SessionStatus::Running
            && let Some(last) = response
                .thread
                .turns
                .as_mut()
                .and_then(|turns| turns.last_mut())
            && last.status == agent_protocol::execution::TurnStatus::Unknown
        {
            Arc::make_mut(last).status = agent_protocol::execution::TurnStatus::Running;
        }
        Ok(response)
    }

    async fn read_history(
        &self,
        id: &str,
        cursor: &str,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        if cursor.is_empty() {
            return Err(Failure::new("invalid_cursor", "history cursor is empty"));
        }
        self.timeline_page(id, Some(cursor)).await
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
            let query = serde_json::json!({
                "threadId": params.thread_id.id, "turnId": params.turn_id,
                "cursor": cursor, "limit": 100, "sortDirection": "asc",
            });
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
            for summary in page {
                let thread = summary.thread;
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
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        self.thread_response(
            "thread/fork",
            &with_browser_config(
                serde_json::json!({"threadId":id,"lastTurnId":last_turn_id,"excludeTurns":false}),
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
            let (cause, reason) = loop {
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
                                    request_change(
                                        instance,
                                        stopped.clone(),
                                        request,
                                        process.as_ref().map_err(|e| e.clone())?.clone(),
                                    )
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
                                break (
                                    "event_processing_failed",
                                    format!(
                                        "sequence={sequence} method={} error={error}",
                                        request.method().unwrap_or_default(),
                                    ),
                                );
                            }
                        }
                    }
                    Ok(PeerEvent::Response { sequence, .. }) => {
                        processed.send_replace(sequence);
                    }
                    Ok(PeerEvent::Closed(reason)) => break ("peer_closed", reason),
                    Err(broadcast::error::RecvError::Closed) => {
                        break ("event_stream_closed", "event sender dropped".into());
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        break ("event_stream_lagged", format!("missed_events={missed}"));
                    }
                }
            };
            // Record the cause before cancellation or process cleanup can hide
            // whether this was a requested shutdown or an unexpected stop.
            let shutdown_requested = stopped.is_cancelled();
            let message = format!(
                "cause={cause} instance={instance} last_processed_sequence={} shutdown_requested={shutdown_requested} reason={reason}",
                *processed.borrow(),
            );
            if shutdown_requested {
                tracing::info!(target: "bex", operation = "host.codex.event_stream_stopped", message = %message);
            } else {
                tracing::error!(target: "bex", operation = "host.codex.event_stream_stopped", message = %message);
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
            let turns = timeline_turns(serde_json::from_value(entries.clone()).unwrap(), exhausted)
                .unwrap();
            assert_eq!(
                turns.len(),
                2,
                "metadata-only turns must not create empty history rows"
            );
            assert_eq!(
                turns[1].status,
                agent_protocol::execution::TurnStatus::Failed
            );
            assert_eq!(
                (turns[1].started_at, turns[1].duration_ms),
                (Some(1.5), Some(2000))
            );
            assert_eq!(turns[1].error.as_ref().unwrap().message, "saved failure");
            assert_eq!(turns[1].items_has_more, Some(false));
            assert_eq!(
                turns[1]
                    .items
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                ["earlier", "later"]
            );
            let partial = turns[0].items.as_ref().unwrap();
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
            assert_eq!(turns[0].items_has_more, Some(!exhausted));
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
        let response =
            agent_protocol::protocol::Response::from_result::<(), _>(Err(response)).into_value();
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
