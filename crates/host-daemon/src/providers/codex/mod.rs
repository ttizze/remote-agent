//! Codex native protocol boundary: execution operations, one shared process,
//! ordered request completion, native cursors and deferred item reads.
pub(crate) mod accounts;
mod background;
mod cli;
mod composer;
mod fork;
pub(crate) mod native;
mod permissions;
mod retention;
mod usage;

use crate::providers::Failure;
use agent_protocol::{
    models::{Item, ThreadResponse},
    operations as op,
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use agent_transport::peer::{RpcMessage, RpcMessageKind};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Every turn asks for reasoning summaries, resumed threads included: model
/// catalogues can default them to "none", and clients show them as Thinking.
const REASONING_SUMMARY: &str = "detailed";

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
    pub source_kinds: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestor_thread_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

use crate::providers::{
    Agent, AgentChange, AgentEvent, AnswerWrite, Identity, SessionListScope, SessionPage,
    SessionSummary, SubmissionState, emit,
};
use agent_transport::peer::PeerEvent;
use futures_util::FutureExt;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::broadcast;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryItem {
    #[serde(deserialize_with = "native::deserialize_item")]
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

pub struct Codex {
    cli: cli::Updater,
    accounts: Arc<tokio::sync::Mutex<Option<crate::providers::codex::accounts::Accounts>>>,
    list_access: tokio::sync::Semaphore,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    directory: PathBuf,
    instance: uuid::Uuid,
    process: Result<Arc<CodexAppServer>, String>,
    stopped: tokio_util::sync::CancellationToken,
    processed: tokio::sync::watch::Sender<u64>,
    subscriptions: Arc<retention::Subscriptions>,
    rosters: Arc<background::Rosters>,
    scopes: Arc<super::scopes::Scopes>,
}
pub use codex_app_server::AppServerConfig as Config;

impl From<Codex> for super::Backend {
    fn from(agent: Codex) -> Self {
        let dictation =
            crate::dictation::Dictation::new(agent.process.clone(), agent.stopped.clone());
        Self {
            provider: ProviderKind::Codex,
            agent: Ok(Arc::new(agent)),
            dictation: Some(dictation),
        }
    }
}

impl Codex {
    pub async fn spawn(config: Config, account_directory: PathBuf) -> Self {
        let process = CodexAppServer::spawn(config.clone())
            .await
            .map(Arc::new)
            .map_err(|error| error.to_string());
        if let Err(error) = &process {
            tracing::error!(target: "agent", operation = "host.codex", message = %error);
        }
        let available = process.is_ok();
        let mut adapter = Self::new(process);
        adapter.cli = cli::Updater::new(config.clone());
        if available && let Err(error) = adapter.enable_accounts(account_directory, config).await {
            tracing::error!(target: "agent", operation = "host.codex.accounts", message = %error);
        }
        adapter
    }

    pub fn new(process: Result<Arc<CodexAppServer>, String>) -> Self {
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
        let scopes = Arc::new(super::scopes::Scopes::default());
        let (processed, progress) = tokio::sync::watch::channel(0);
        let stopped = tokio_util::sync::CancellationToken::new();
        Self {
            cli: cli::Updater::new(Config::default()),
            accounts: Arc::default(),
            list_access: tokio::sync::Semaphore::new(4),
            restoration_error: tokio::sync::watch::channel(None).0,
            directory,
            instance: uuid::Uuid::new_v4(),
            process,
            stopped: stopped.clone(),
            processed,
            subscriptions: Arc::new(retention::Subscriptions::new(
                scopes.clone(),
                progress,
                stopped,
            )),
            rosters: Arc::default(),
            scopes,
        }
    }
    pub async fn enable_accounts(
        &self,
        directory: PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        let accounts = crate::providers::codex::accounts::Accounts::load(
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
                "Codexが終了しました。ホストを再起動すると再接続できます。Claudeの会話は継続できます。",
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
        native::parse_thread_response(self.request(method, params).await?).map_err(Into::into)
    }

    fn subscribed(&self, response: &ThreadResponse) {
        if let Some(thread) = &response.thread.id {
            self.subscriptions.touch(&thread.id);
        }
    }

    async fn resume(
        &self,
        id: &str,
        cwd: Option<&str>,
        browser: Option<Value>,
        reload: bool,
    ) -> Result<(), Failure> {
        let params = thread_config(serde_json::json!({"threadId":id,"cwd":cwd}), browser);
        let resume = async {
            self.scopes.suspend(id);
            self.thread_response("thread/resume", &params)
                .await
                .map(|response| self.scopes.history(&response))
                .map_err(Failure::before_submission)
        };
        self.subscriptions.start(id, reload, resume).await
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
                "sortDirection":"desc","itemsView":"full"}),
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
        if page.data.iter().any(|turn| turn["itemsView"] != "full") {
            return Err(Failure::new(
                "invalid_thread_history",
                "turn summary is missing",
            ));
        }
        let turns = page
            .data
            .into_iter()
            .map(native::parse_turn)
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
        let turns = turns
            .into_iter()
            .rev()
            .map(|mut turn| {
                if !include_activity {
                    turn.items = turn.items.map(native::summary_items);
                }
                Arc::new(turn)
            })
            .collect();
        Ok(agent_protocol::session::HistoryPage {
            turns,
            next_cursor,
            history_read_state: None,
        })
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
    let mut execution = native::execution_error(&value, false);
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
    // A different app-server owns this rollout. Never steal its writer or
    // silently fork the user's conversation; the client can explicitly fork.
    let writer_busy = execution.provider_code.as_deref() == Some("-32600")
        && execution.message.starts_with("thread ")
        && execution.message.ends_with(" already has an active writer");
    let mut failure = Failure::unknown(
        if writer_busy {
            "thread_in_use"
        } else {
            "provider_failed"
        },
        &execution.message,
    );
    if writer_busy {
        failure.message = agent_protocol::error::THREAD_IN_USE_MESSAGE.into();
    }
    failure.execution = Some(Box::new(execution));
    failure
}

/// Configuration of threads this Host subscribes to. The Host decides when a
/// thread is idle, so app-server unloads it soon after the subscription ends
/// (older app-servers otherwise keep it for 30 more minutes).
fn thread_config(mut params: Value, browser: Option<Value>) -> Value {
    params["config"] = serde_json::json!({"thread_unload_delay_secs": 60});
    if let Some(browser) = browser {
        params["config"]["mcp_servers.agent_browser"] = browser;
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
            status: native::session_status(&value["status"]),
        },
        "turn/started" | "turn/completed" => {
            let turn = native::parse_turn(value["turn"].clone())?;
            SessionChange::Turn {
                turn,
                completed: method == "turn/completed",
            }
        }
        "item/started" | "item/completed" => {
            let mut item = native::parse_item(value["item"].clone())?;
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
                    item: native::approval_review(
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
            error: native::execution_error(&value["error"], value["willRetry"] == true),
        },
        _ => return Ok(None),
    };
    Ok(Some((
        Deserialize::deserialize(&value["threadId"])?,
        change,
    )))
}

/// Provider-specific notifications end at this adapter boundary.
pub(crate) fn event_change(
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
    let change = if message.method() == Some("thread/started") {
        let thread = native::parse_thread(params["thread"].clone()).map_err(|e| e.to_string())?;
        let Some(parent) = thread.parent_id else {
            return Ok(None);
        };
        AgentChange::TaskParent {
            session: thread.id.expect("native thread has an ID"),
            parent,
        }
    } else if message.method() == Some("serverRequest/resolved") {
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
        AgentChange::Renamed {
            session: SessionRef::new(
                ProviderKind::Codex,
                params["threadId"]
                    .as_str()
                    .ok_or("renamed session ID is missing")?
                    .into(),
            )
            .map_err(str::to_owned)?,
            name: serde_json::from_value(params["threadName"].clone())
                .map_err(|e| e.to_string())?,
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
    answers: crate::providers::requests::NativeAnswers,
}
pub(crate) fn request_origin(
    instance: uuid::Uuid,
    native_id: Value,
    stopped: tokio_util::sync::CancellationToken,
    process: Arc<CodexAppServer>,
    answers: crate::providers::requests::NativeAnswers,
) -> crate::providers::RequestOrigin {
    crate::providers::RequestOrigin {
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
impl crate::providers::AnswerSource for RequestSource {
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
    let adapted = crate::providers::requests::codex(
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
        command: crate::providers::AccountCommand,
    ) -> Result<crate::providers::AccountReply, Failure> {
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
impl crate::providers::Configuration for Codex {
    async fn codex_cli(&self, update: bool) -> Result<op::CodexCliStatus, Failure> {
        Ok(self.cli.request(update).await)
    }
    fn data_recipient(&self) -> &'static str {
        "OpenAI"
    }
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        let config: Result<Value, _> = self
            .request("config/read", &serde_json::json!({"includeLayers":false}))
            .await;
        let configured = config
            .as_ref()
            .ok()
            .and_then(|value| value["config"]["model"].as_str());
        let page: Page<Value> = self.request("model/list", &serde_json::json!({"limit":params.limit,"cursor":params.cursor,"includeHidden":true})).await?;
        let mut data = page
            .data
            .into_iter()
            .filter(|value| value["hidden"] != true || value["model"].as_str() == configured)
            .map(native::parse_model)
            .collect::<Result<Vec<_>, _>>()?;
        if params.cursor.is_none()
            && let Ok(config) = config
        {
            let account: Result<Value, _> = self
                .request("account/read", &serde_json::json!({"refreshToken":false}))
                .await;
            let account_type = account
                .as_ref()
                .ok()
                .and_then(|value| value["account"]["type"].as_str());
            native::include_configured_model(&mut data, &config["config"], account_type);
        }
        Ok(op::ModelPage {
            data,
            next_cursor: page.next_cursor,
            provider_errors: None,
        })
    }

    async fn catalog(&self, cwd: &str) -> agent_protocol::composer::ComposerCatalog {
        self.composer_catalog(cwd).await
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
}

#[async_trait::async_trait]
impl crate::providers::Discovery for Codex {
    fn storage_directory(&self) -> &std::path::Path {
        &self.directory
    }
    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        scope: SessionListScope<'_>,
        limit: u32,
    ) -> Result<SessionPage, Failure> {
        // Share admission across title lists, agent fleets and loaded metadata.
        let _listing = self
            .list_access
            .acquire()
            .await
            .expect("list access is open");
        let (source_kinds, ancestor_thread_id): (&'static [&'static str], Option<&str>) =
            match scope {
                SessionListScope::Loaded => {
                    let page: Page<String> = self
                        .request(
                            "thread/loaded/list",
                            &serde_json::json!({"limit":limit,"cursor":cursor}),
                        )
                        .await?;
                    use futures_util::{StreamExt, TryStreamExt};
                    let data =
                        futures_util::stream::iter(page.data.into_iter().map(|id| async move {
                            let mut response = self
                                .thread_response(
                                    "thread/read",
                                    &serde_json::json!({"threadId":id,"includeTurns":false}),
                                )
                                .await?;
                            response.thread.turns = None;
                            Ok::<_, Failure>(SessionSummary {
                                thread: response.thread,
                                branch: None,
                            })
                        }))
                        .buffer_unordered(4)
                        .try_collect()
                        .await?;
                    return Ok(SessionPage {
                        data,
                        next_cursor: page.next_cursor,
                    });
                }
                SessionListScope::Roots(_) => (&["cli", "vscode", "appServer", "unknown"], None),
                SessionListScope::WithSubagents(_) => (
                    &[
                        "cli",
                        "vscode",
                        "appServer",
                        "unknown",
                        "subAgentThreadSpawn",
                    ],
                    None,
                ),
                SessionListScope::All => (
                    &[
                        "cli",
                        "vscode",
                        "exec",
                        "appServer",
                        "unknown",
                        "subAgentThreadSpawn",
                    ],
                    None,
                ),
                SessionListScope::Descendants(id) => (&["subAgentThreadSpawn"], Some(id)),
            };
        let page: Page<Value> = self
            .request(
                "thread/list",
                &ThreadListParams {
                    limit: limit as usize,
                    sort_key: "updated_at",
                    sort_direction: "desc",
                    use_state_db_only: true,
                    source_kinds,
                    ancestor_thread_id,
                    search_term: (!search.trim().is_empty()).then_some(search),
                    cursor,
                },
            )
            .await?;
        Ok(SessionPage {
            data: page
                .data
                .into_iter()
                .map(|value| {
                    let branch = value["gitInfo"]["branch"].as_str().map(str::to_owned);
                    native::parse_thread(value).map(|thread| SessionSummary { thread, branch })
                })
                .collect::<Result<_, _>>()?,
            next_cursor: page.next_cursor,
        })
    }
    async fn open(
        &self,
        id: &str,
        _limit: usize,
        include_activity: bool,
    ) -> Result<ThreadResponse, Failure> {
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
            let page = self
                .item_page(&params.thread_id.id, &params.turn_id, cursor.as_deref())
                .await?;
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

#[async_trait::async_trait]
impl Agent for Codex {
    fn capabilities(&self) -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            steer: true,
            fork: true,
            model_change: true,
            ..Default::default()
        }
    }
    fn availability(&self) -> Result<(), Failure> {
        self.server()?;
        if let Some(error) = self.restoration_error.borrow().as_ref() {
            return Err(Failure::new("account_unavailable", error));
        }
        Ok(())
    }
    fn requires_restart_snapshot(&self) -> bool {
        self.process.is_ok()
    }
    fn validate_create(&self) -> Result<(), Failure> {
        self.availability()
    }
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        let response = self
            .thread_response(
                "thread/start",
                &thread_config(serde_json::json!({"cwd":cwd,"model":model}), browser_config),
            )
            .await?;
        self.subscribed(&response);
        Ok(response)
    }
    async fn inject_history(
        &self,
        id: &str,
        context: &agent_protocol::handoff::Context,
    ) -> Result<bool, Failure> {
        let state = self.state(id).await?;
        self.resume(
            id,
            state.response.thread.cwd.as_deref(),
            None,
            state.needs_reload,
        )
        .await?;
        match self
            .request::<_, Value>(
                "thread/inject_items",
                &serde_json::json!({"threadId":id,"items":context.native_items()}),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .execution
                    .as_ref()
                    .and_then(|error| error.provider_code.as_deref())
                    == Some("-32601") =>
            {
                Ok(false)
            }
            Err(mut error) => {
                error.delivery = agent_protocol::error::Delivery::Unknown;
                Err(error)
            }
        }
    }
    async fn unload(&self, id: &str) -> Result<(), Failure> {
        self.subscriptions
            .release(self.server()?, id, false)
            .await
            .map_err(|error| Failure::unknown("provider_unload_failed", error))
    }
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure> {
        self.availability()?;
        let native: Value = self
            .request(
                "thread/read",
                &serde_json::json!({"threadId":id,"includeTurns":false}),
            )
            .await
            .map_err(Failure::before_submission)?;
        let needs_reload = native["thread"]["status"]["type"] == "notLoaded";
        let response = native::parse_thread_response(native)
            .map_err(|error| Failure::new("invalid_thread", error))?;
        Ok(SubmissionState {
            response,
            needs_reload,
        })
    }
    async fn submit(
        &self,
        input: &op::Submission,
        route: crate::providers::SubmissionTarget<'_>,
        reload: bool,
        browser: Option<Value>,
        execution: uuid::Uuid,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use crate::providers::SubmissionTarget;
        let mut params = serde_json::json!({
            "threadId": input.thread_id.id,
            "clientUserMessageId": input.client_user_message_id,
            "input": native::encode_input(&input.input),
        });
        let turn_id = match route {
            SubmissionTarget::Steer(turn) => {
                self.scopes.reserve(&input.thread_id.id, execution);
                self.scopes
                    .bind(&input.thread_id.id, turn.into(), execution);
                params["expectedTurnId"] = turn.into();
                let result = self
                    .request::<_, agent_protocol::models::Empty>("turn/steer", &params)
                    .await;
                if result
                    .as_ref()
                    .is_err_and(|error| error.delivery == agent_protocol::error::Delivery::NotSent)
                {
                    self.scopes.cancel(&input.thread_id.id, execution);
                }
                result?;
                Some(turn.into())
            }
            SubmissionTarget::Start { cwd } => {
                self.resume(&input.thread_id.id, Some(cwd), browser, reload)
                    .await?;
                self.scopes.reserve(&input.thread_id.id, execution);
                params["model"] = serde_json::json!(input.model.as_ref().map(|model| &model.id));
                params["effort"] = serde_json::json!(input.effort);
                params["serviceTierForTurn"] = serde_json::json!(input.service_tier);
                params["summary"] = REASONING_SUMMARY.into();
                let result = self.request::<_, Value>("turn/start", &params).await;
                if result
                    .as_ref()
                    .is_err_and(|error| error.delivery == agent_protocol::error::Delivery::NotSent)
                {
                    self.scopes.cancel(&input.thread_id.id, execution);
                }
                let reply = result?;
                Some(native_turn_id(reply["turn"]["id"].as_str())?)
            }
        };
        if let Some(turn) = &turn_id {
            self.scopes
                .bind(&input.thread_id.id, turn.clone(), execution);
        }
        Ok(op::SubmissionReceipt { turn_id })
    }
    async fn continue_turn(
        &self,
        id: &str,
        model: Option<&str>,
        effort: Option<&str>,
        service_tier: Option<&str>,
        execution: uuid::Uuid,
    ) -> Result<op::SubmissionReceipt, Failure> {
        self.scopes.reserve(id, execution);
        let result = self
            .request::<_,Value>("turn/start", &serde_json::json!({"threadId":id,"input":[],"model":model,"effort":effort,"serviceTierForTurn":service_tier,"summary":REASONING_SUMMARY}))
            .await;
        if result
            .as_ref()
            .is_err_and(|error| error.delivery == agent_protocol::error::Delivery::NotSent)
        {
            self.scopes.cancel(id, execution);
        }
        let reply = result?;
        let turn = native_turn_id(reply["turn"]["id"].as_str())?;
        self.scopes.bind(id, turn.clone(), execution);
        Ok(op::SubmissionReceipt {
            turn_id: Some(turn),
        })
    }
    async fn interrupt(
        &self,
        id: &str,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        // As T3 Code, Stop on a conversation that waits on its commands ends
        // their background terminals.
        let session = SessionRef::new(ProviderKind::Codex, id.into())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let terminals = self.rosters.stop(&session);
        if terminals.is_empty() {
            return self
                .request(
                    "turn/interrupt",
                    &serde_json::json!({"threadId":id,"turnId":turn_id}),
                )
                .await;
        }
        for process in terminals {
            let _: Value = self
                .request(
                    "thread/backgroundTerminals/terminate",
                    &serde_json::json!({"threadId":id,"processId":process}),
                )
                .await?;
        }
        Ok(agent_protocol::models::Empty {})
    }
    async fn discard_workspace_processes(&self, _dir: &std::path::Path) -> Result<(), Failure> {
        Ok(())
    }
    async fn fork(
        &self,
        id: &str,
        last_turn_id: &str,
        browser_config: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        let params = thread_config(
            serde_json::json!({"threadId":id,"lastTurnId":last_turn_id,"excludeTurns":false}),
            browser_config,
        );
        let value = fork::request(params, |method, params| async move {
            self.request(method, &params).await
        })
        .await?;
        let response = native::parse_thread_response(value)?;
        self.subscribed(&response);
        Ok(response)
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
        let process = codex.clone();
        let subscriptions = self.subscriptions.clone();
        let rosters = self.rosters.clone();
        let scopes = self.scopes.clone();
        subscriptions.clone().spawn_releaser(codex.clone());
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
                                refresh_auth_tokens(&accounts, &process, &line).await;
                            });
                        } else if request.kind() == RpcMessageKind::Request {
                            let admission = admit_request(
                                instance,
                                stopped.clone(),
                                &process,
                                &output,
                                &scopes,
                                &line,
                            )
                            .await;
                            if let Err(error) = &admission {
                                tracing::warn!(target: "agent", operation = "host.codex.request_rejected", message = %error);
                            }
                            if let Err(error) = admission
                                && let Ok(response) = request.error(-32000, &error)
                            {
                                let _ = process.send_raw(&response).await;
                            }
                        } else {
                            let result = match event_change(instance, &request) {
                                Ok(Some(change)) => {
                                    subscriptions.observe(&change);
                                    let background = rosters.observe(
                                        &change,
                                        &request.params::<Value>().unwrap_or_default(),
                                    );
                                    wake(&process, &change, background.continuations);
                                    let child =
                                        if let AgentChange::TaskParent { session, .. } = &change {
                                            Some(session.id.clone())
                                        } else {
                                            None
                                        };
                                    let execution = scopes.observe(&change);
                                    let result = emit_all(
                                        &output,
                                        [Some(change), background.roster],
                                        execution,
                                    )
                                    .await;
                                    if let (Ok(()), Some(child)) = (&result, child) {
                                        scopes.admit(&child);
                                    }
                                    result
                                }
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
                tracing::info!(target: "agent", operation = "host.codex.event_stream_stopped", message = %message);
            } else {
                tracing::error!(target: "agent", operation = "host.codex.event_stream_stopped", message = %message);
            }
            stopped.cancel();
            if let Err(error) = process.shutdown().await {
                tracing::error!(target: "agent", operation = "host.codex.shutdown", message = %error);
            }
            let _=emit(&output,AgentChange::Stopped {provider:ProviderKind::Codex,error:agent_protocol::execution::ExecutionError {message:"エージェントとの接続が終了しました。ホストを再起動してから再送信してください。".into(),category:agent_protocol::execution::ErrorCategory::Network,..Default::default()}}).await;
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

/// Starts the turn in which Codex reports on its ended background commands,
/// as T3 Code continues a thread with the command's detail.
fn wake(process: &Arc<CodexAppServer>, change: &AgentChange, prompts: Vec<String>) {
    let AgentChange::Session { session, .. } = change else {
        return;
    };
    for prompt in prompts {
        let process = process.clone();
        let params = serde_json::json!({
            "threadId": session.id,
            "input": native::encode_input(&[op::Input::Text { text: prompt }]),
        });
        tokio::spawn(async move {
            let started = process
                .request_sequenced::<_, Value>("turn/start", &params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|reply| reply.value.outcome.map_err(|error| format!("{error:?}")));
            if let Err(error) = started {
                tracing::warn!(target: "agent", operation = "host.codex.background_wake", message = %error);
            }
        });
    }
}

async fn emit_all(
    output: &tokio::sync::mpsc::Sender<AgentEvent>,
    changes: impl IntoIterator<Item = Option<AgentChange>>,
    execution: Option<uuid::Uuid>,
) -> Result<(), String> {
    for change in changes.into_iter().flatten() {
        crate::providers::emit_for(output, change, execution).await?;
    }
    Ok(())
}

async fn refresh_auth_tokens(
    accounts: &tokio::sync::Mutex<Option<accounts::Accounts>>,
    process: &Arc<CodexAppServer>,
    line: &str,
) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Refresh<'a> {
        previous_account_id: Option<&'a str>,
    }
    let Ok(request) = RpcMessage::parse(line) else {
        return;
    };
    let Ok(params) = request.params::<Refresh>() else {
        return;
    };
    let mut accounts = accounts.lock().await;
    let result = match accounts.as_mut() {
        Some(accounts) => accounts.refresh(params.previous_account_id).await,
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
    let _ = process.send_raw(&line).await;
}

async fn admit_request(
    instance: uuid::Uuid,
    stopped: tokio_util::sync::CancellationToken,
    process: &Arc<CodexAppServer>,
    output: &tokio::sync::mpsc::Sender<AgentEvent>,
    scopes: &super::scopes::Scopes,
    line: &str,
) -> Result<(), String> {
    let request = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let change = request_change(instance, stopped, request, process.clone())?;
    let execution = scopes.observe(&change);
    crate::providers::emit_for(output, change, execution).await
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn restart_snapshot_checks_loaded_codex_even_after_connection_closes() {
        use std::{os::unix::fs::PermissionsExt, sync::Arc};
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex-fixture");
        std::fs::write(
            &program,
            r##"#!/usr/bin/env node
const rl = require('node:readline').createInterface({ input: process.stdin });
rl.on('line', line => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  const result = message.method === 'initialize'
    ? { userAgent: 'restart-fixture', codexHome: process.env.CODEX_HOME }
    : { data: [], nextCursor: null };
  process.stdout.write(JSON.stringify({ id: message.id, result }) + '\n');
});
"##,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = Arc::new(
            super::CodexAppServer::spawn(super::Config {
                program,
                codex_home: Some(directory.path().join("codex")),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let agent = Arc::new(super::Codex::new(Ok(server.clone())));
        let _events = super::Agent::event_stream(agent.as_ref()).unwrap();
        let service = |name: &str| {
            let service = crate::HostRpcService::new(
                [crate::providers::Backend {
                    provider: super::ProviderKind::Codex,
                    agent: Ok(agent.clone()),
                    dictation: None,
                }],
                crate::ProjectStore::new(directory.path().join(name)),
            );
            service.set_history_scope(directory.path().to_owned(), true);
            service
        };
        service("healthy.json").prepare_update().await.unwrap();
        server.shutdown().await.unwrap();
        assert!(service("broken.json").prepare_update().await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_complete_listings_include_exec_runs() {
        use std::{os::unix::fs::PermissionsExt, sync::Arc};
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex-fixture");
        // Filters by source like Codex: Host tasks are `vscode`, agent runs are `exec`.
        std::fs::write(
            &program,
            r##"#!/usr/bin/env node
const rl = require('node:readline').createInterface({ input: process.stdin });
const threads = [
  { id: 'task', source: 'vscode' },
  { id: 'review', source: 'exec' },
  { id: 'helper', source: 'subAgentThreadSpawn' },
];
rl.on('line', line => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  const result = message.method === 'initialize'
    ? { userAgent: 'list-fixture', codexHome: process.env.CODEX_HOME }
    : { data: threads.filter(thread => message.params.sourceKinds.includes(thread.source))
          .map(({ id }) => ({ id, cwd: '/fixture', status: { type: 'idle' } })), nextCursor: null };
  process.stdout.write(JSON.stringify({ id: message.id, result }) + '\n');
});
"##,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = Arc::new(
            super::CodexAppServer::spawn(super::Config {
                program,
                codex_home: Some(directory.path().join("codex")),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let agent = super::Codex::new(Ok(server));
        let _events = super::Agent::event_stream(&agent).unwrap();
        for (scope, expected) in [
            (super::SessionListScope::Roots(&[]), vec!["task"]),
            (
                super::SessionListScope::WithSubagents(&[]),
                vec!["task", "helper"],
            ),
            (
                super::SessionListScope::All,
                vec!["task", "review", "helper"],
            ),
        ] {
            let page = crate::providers::Discovery::list(&agent, "", None, scope, 10)
                .await
                .unwrap();
            let ids: Vec<_> = page
                .data
                .iter()
                .map(|summary| summary.thread.id.as_ref().unwrap().id.as_str())
                .collect();
            assert_eq!(ids, expected);
        }
    }

    #[test]
    fn native_rename_notices_carry_the_name_including_a_cleared_name() {
        for name in [Some("New name"), None] {
            let line = serde_json::json!({"method":"thread/name/updated","params":{"threadId":"task","threadName":name}}).to_string();
            let change =
                super::event_change(uuid::Uuid::nil(), &super::RpcMessage::parse(&line).unwrap())
                    .unwrap()
                    .unwrap();
            let super::AgentChange::Renamed {
                session,
                name: received,
            } = change
            else {
                panic!("name was not published")
            };
            assert_eq!(session.id, "task");
            assert_eq!(received.as_deref(), name);
        }
    }

    #[test]
    fn started_subagents_report_parent_for_subscription_retention() {
        let line = serde_json::json!({"method":"thread/started","params":{"thread":{"id":"child","parentThreadId":"parent"}}}).to_string();
        let change =
            super::event_change(uuid::Uuid::nil(), &super::RpcMessage::parse(&line).unwrap())
                .unwrap()
                .unwrap();
        let super::AgentChange::TaskParent { session, parent } = change else {
            panic!("parent was not published")
        };
        assert_eq!(session.id, "child");
        assert_eq!(parent.id, "parent");
        let root = serde_json::json!({"method":"thread/started","params":{"thread":{"id":"root"}}})
            .to_string();
        assert!(
            super::event_change(uuid::Uuid::nil(), &super::RpcMessage::parse(&root).unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn writer_conflicts_have_a_distinct_recovery_code() {
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "code": -32600,
            "message": "thread example already has an active writer"
        }))
        .unwrap();
        let failure = super::native_failure(&raw).before_submission();
        assert_eq!(failure.code, "thread_in_use");
        assert_eq!(failure.delivery, agent_protocol::error::Delivery::NotSent);
        assert_eq!(
            failure.execution.unwrap().provider_code.as_deref(),
            Some("-32600")
        );
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
        for method in ["process/outputDelta", "process/exited"] {
            let line = serde_json::json!({"method":method,"params":{"processHandle":"owned","deltaBase64":"aW5qZWN0ZWQ=","exitCode":0}}).to_string();
            assert!(
                super::event_change(uuid::Uuid::nil(), &super::RpcMessage::parse(&line).unwrap())
                    .unwrap()
                    .is_none()
            );
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
