//! Codex native protocol boundary: execution operations, one shared process,
//! ordered request completion, native cursors and deferred item reads.
use super::service::Failure;
use agent_protocol::{
    models::{Item, ThreadResponse},
    operations as op,
    session::{ProviderRef, SessionChange, SessionRef, TextField},
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

fn unmaterialized_history(code: Option<&str>, message: &str, thread_id: &str) -> bool {
    code == Some("-32600")
        && message
            == format!(
                "thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message"
            )
}

pub(super) struct Codex {
    pub(super) reference: ProviderRef,
    accounts: Arc<tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    directory: PathBuf,
    instance: uuid::Uuid,
    process: Result<Arc<CodexAppServer>, String>,
    stopped: tokio_util::sync::CancellationToken,
    processed: tokio::sync::watch::Sender<u64>,
    history_catalog: tokio::sync::Mutex<Option<Arc<super::codex_history::Catalog>>>,
}
impl Codex {
    pub(super) fn new(
        reference: ProviderRef,
        process: Result<Arc<CodexAppServer>, String>,
        home: Option<PathBuf>,
    ) -> Self {
        let directory = home
            .or_else(|| {
                process
                    .as_ref()
                    .ok()
                    .map(|server| server.initialize_response().codex_home.clone())
            })
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| {
                directories::BaseDirs::new()
                    .map(|dirs| dirs.home_dir().join(".codex"))
                    .unwrap_or_default()
            });
        Self {
            reference,
            accounts: Arc::default(),
            restoration_error: tokio::sync::watch::channel(None).0,
            directory,
            instance: uuid::Uuid::new_v4(),
            process,
            stopped: Default::default(),
            processed: tokio::sync::watch::channel(0).0,
            history_catalog: Default::default(),
        }
    }
    pub(super) async fn enable_accounts(
        &self,
        directory: PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        let accounts = crate::codex_accounts::Accounts::load(
            self.reference.instance_id.clone(),
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

    async fn history_catalog(
        &self,
        refresh: bool,
    ) -> Result<Arc<super::codex_history::Catalog>, Failure> {
        let mut catalog = self.history_catalog.lock().await;
        if refresh || catalog.is_none() {
            let directory = self.directory.clone();
            let scanned = tokio::task::spawn_blocking(move || {
                super::codex_history::Catalog::scan(&directory)
            })
            .await
            .map_err(|_| Failure::new("history_import_failed", "Codex history reader stopped"))?
            .map_err(|error| Failure::new("history_import_failed", error))?;
            *catalog = Some(Arc::new(scanned));
        }
        Ok(catalog.as_ref().unwrap().clone())
    }

    async fn file_history(&self, id: &str) -> Result<ThreadResponse, Failure> {
        let catalog = self.history_catalog(false).await?;
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || catalog.read(&id))
            .await
            .map_err(|_| Failure::new("history_import_failed", "Codex history reader stopped"))?
            .map_err(|error| Failure::new("history_import_failed", error))
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
        super::native::codex_thread_response(
            self.request(method, params).await?,
            &self.reference.instance_id,
        )
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

    async fn history_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        include_activity: bool,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        let page: Page<Value> = match self
            .request(
                "thread/turns/list",
                &serde_json::json!({"threadId":id,"cursor":cursor,"limit":5,
                "sortDirection":"desc","itemsView":if include_activity { "full" } else { "summary" }}),
            )
            .await
        {
            Ok(page) => page,
            Err(error)
                if cursor.is_none()
                    && unmaterialized_history(
                        error
                            .execution
                            .as_ref()
                            .and_then(|error| error.provider_code.as_deref()),
                        &error.to_string(),
                        id,
                    ) =>
            {
                Page {
                    data: Vec::new(),
                    next_cursor: None,
                }
            }
            Err(error) => return Err(error),
        };
        if page.data.len() > 5 {
            return Err(Failure::new(
                "invalid_thread_history",
                "turn page exceeds requested size",
            ));
        }
        let next_cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
        if next_cursor
            .as_deref()
            .is_some_and(|next| Some(next) == cursor)
        {
            return Err(Failure::new(
                "invalid_thread_history",
                "history cursor repeated",
            ));
        }
        if page
            .data
            .iter()
            .any(|turn| turn["itemsView"] != if include_activity { "full" } else { "summary" })
        {
            return Err(Failure::new(
                "invalid_thread_history",
                "turn summary is missing",
            ));
        }
        let mut turns = page
            .data
            .into_iter()
            .map(super::native::codex_turn)
            .map(|turn| turn.map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        if turns.iter().any(|turn| {
            turn.id.is_empty()
                || turn.items.is_none()
                || turn.items.iter().flatten().any(|item| item.id.is_empty())
        }) {
            return Err(Failure::new(
                "invalid_thread_history",
                "turn summary is missing",
            ));
        }
        turns.reverse();
        Ok(agent_protocol::session::HistoryPage { turns, next_cursor })
    }

    async fn item_page(
        &self,
        id: &str,
        turn_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<HistoryItem>, Failure> {
        let page: Page<HistoryItem> = self.request("thread/items/list", &serde_json::json!({
            "threadId":id,"turnId":turn_id,"cursor":cursor,"limit":100,"sortDirection":"asc",
        })).await?;
        if page.data.len() > 100
            || page
                .data
                .iter()
                .any(|entry| entry.turn_id.as_deref() != Some(turn_id) || entry.item.id.is_empty())
        {
            return Err(Failure::new(
                "invalid_thread_history",
                "turn item page identity or size is invalid",
            ));
        }
        Ok(page)
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
            let turn = super::native::codex_turn(value["turn"].clone())?;
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
            session: SessionRef::new(id).map_err(str::to_owned)?,
            change,
        }
    } else if message.method() == Some("thread/name/updated") {
        AgentChange::Renamed {
            session: SessionRef::new(
                params["threadId"]
                    .as_str()
                    .ok_or("renamed session ID is missing")?
                    .into(),
            )
            .map_err(str::to_owned)?,
            name: params["threadName"]
                .as_str()
                .ok_or("renamed session title is missing")?
                .into(),
        }
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
    let session = SessionRef::new(id).map_err(str::to_owned)?;
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
    fn dictation_backend(&self) -> Option<Arc<CodexAppServer>> {
        self.server().ok()?;
        self.process.as_ref().ok().cloned()
    }
    fn reference(&self) -> &ProviderRef {
        &self.reference
    }
    fn capabilities(&self) -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            active_steering: true,
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
        if cursor
            .as_deref()
            .is_some_and(|cursor| cursor.starts_with("codex-file:"))
            || (cursor.is_none() && self.server().is_err())
        {
            return self
                .history_catalog(cursor.is_none())
                .await?
                .page(search, cursor.as_deref())
                .map_err(|error| Failure::new("history_import_failed", error));
        }
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
    async fn open(
        &self,
        id: &str,
        _limit: usize,
        include_activity: bool,
    ) -> Result<ThreadResponse, Failure> {
        if self.server().is_err() {
            return self.file_history(id).await;
        }
        let params = serde_json::json!({"threadId":id,"includeTurns":false});
        let (mut response, page) = tokio::try_join!(
            self.thread_response("thread/read", &params),
            self.history_page(id, None, include_activity),
        )?;
        response.thread.turns = Some(page.turns);
        response.thread.history_has_more = Some(page.next_cursor.is_some());
        response.thread.history_cursor = page.next_cursor;
        Ok(response)
    }

    async fn read_history(
        &self,
        id: &str,
        cursor: &str,
        include_activity: bool,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        if cursor.is_empty() {
            return Err(Failure::new("invalid_cursor", "history cursor is empty"));
        }
        self.history_page(id, Some(cursor), include_activity).await
    }

    async fn read_turn_items(
        &self,
        id: &str,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<Vec<Arc<Item>>, Failure> {
        if turn_id.is_empty() {
            return Err(Failure::new("invalid_params", "turn ID is required"));
        }
        let mut items = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self.item_page(id, turn_id, cursor.as_deref()).await?;
            items.extend(page.data.into_iter().map(|entry| entry.item));
            cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            let Some(next) = &cursor else {
                break;
            };
            if !seen.insert(next.clone()) {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "item cursor repeated",
                ));
            }
        }
        Ok(items)
    }

    async fn read_item(
        &self,
        native_id: &str,
        turn_id: &agent_protocol::ids::TurnId,
        item_id: &agent_protocol::ids::ItemId,
    ) -> Result<agent_protocol::operations::ItemResponse, Failure> {
        if [native_id, turn_id.as_str(), item_id.as_str()]
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
            let page = self
                .item_page(native_id, turn_id, cursor.as_deref())
                .await?;
            if let Some(entry) = page.data.into_iter().find(|entry| {
                entry.turn_id.as_deref() == Some(turn_id.as_str()) && &entry.item.id == item_id
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
        let response = super::native::codex_thread_response(native, &self.reference.instance_id)
            .map_err(|error| Failure::new("invalid_thread", error))?;
        Ok(SubmissionState {
            response,
            needs_reload,
        })
    }
    async fn submit(
        &self,
        input: &op::Submission,
        native_id: &str,
        route: super::submission::SubmissionTarget,
        reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use super::submission::SubmissionTarget;
        let mut params = serde_json::json!({
            "threadId": native_id,
            "clientUserMessageId": input.client_user_message_id,
            "input": super::native::codex_input(&input.input),
        });
        let turn_id = match route {
            SubmissionTarget::Steer(turn) => {
                params["expectedTurnId"] = turn.clone().into();
                self.request::<_, agent_protocol::models::Empty>("turn/steer", &params)
                    .await?;
                Some(turn.into())
            }
            SubmissionTarget::Queue => {
                return Err(Failure::new(
                    "invalid_execution_route",
                    "the Host owns queued input",
                ));
            }
            SubmissionTarget::Start { cwd } => {
                if reload {
                    self.thread_response(
                        "thread/resume",
                        &with_browser_config(
                            serde_json::json!({"threadId":native_id,"cwd":cwd}),
                            browser,
                        ),
                    )
                    .await
                    .map_err(Failure::before_submission)?;
                }
                params["model"] = serde_json::json!(input.model.as_ref().map(|model| &model.id));
                params["effort"] = serde_json::json!(agent_protocol::models::model_option_string(
                    &input.options,
                    "reasoningEffort"
                ));
                params["serviceTierForTurn"] = serde_json::json!(
                    agent_protocol::models::model_option_string(&input.options, "serviceTier")
                );
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
        let native = self.request("model/list", params).await?;
        super::model_catalog::codex_page(native, &self.reference.instance_id)
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
                        let line = message.value;
                        let Ok(request) = RpcMessage::parse(&line) else {
                            processed.send_replace(sequence);
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
                        // An event is processed only after the Host has committed
                        // it. A failed commit closes this stream before advancing
                        // the barrier awaited by native command responses.
                        processed.send_replace(sequence);
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
            let _=emit(&output,AgentChange::Stopped {reason:"エージェントとの接続が終了しました。Hostを再起動してから再送信してください。".into()}).await;
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
