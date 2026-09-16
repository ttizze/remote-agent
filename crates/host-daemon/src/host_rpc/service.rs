use std::sync::{Arc, OnceLock};

use agent_core::peer::RpcMessageKind;
use agent_core::{
    models::{ListQuery, Thread, ThreadResponse},
    peer::{PeerEvent, RpcMessage, RpcMessageError, RpcResponse},
    state::operations as op,
};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::broadcast;

use super::codex::ThreadListParams;

/// Creation parameters; history reads use the typed adapter directly.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ThreadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

use super::routing::{HostSession, SessionId, SessionRouter};
use crate::{DesktopProjectStore, HOST_THREAD_LIST_METHOD, HOST_THREAD_START_METHOD};

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(untagged)]
pub(super) enum Failure {
    #[error("{message}")]
    Host { code: &'static str, message: String },
    #[error("{0}")]
    Upstream(Box<RawValue>),
}
impl From<RpcMessageError> for Failure {
    fn from(error: RpcMessageError) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<AppServerError> for Failure {
    fn from(error: AppServerError) -> Self {
        Self::new("codex_unavailable", error)
    }
}
impl From<crate::DesktopProjectError> for Failure {
    fn from(error: crate::DesktopProjectError) -> Self {
        Self::new("desktop_project_state_unavailable", error)
    }
}

impl Failure {
    pub(super) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Host {
            code,
            message: error.to_string(),
        }
    }
}
async fn run_handler<P, R, E: std::fmt::Display, F: Future<Output = Result<R, String>>>(
    params: Result<P, E>,
    code: &'static str,
    run: impl FnOnce(P) -> F,
) -> Result<R, Failure> {
    let params = params.map_err(|error| Failure::new(code, error))?;
    run(params).await.map_err(|error| Failure::new(code, error))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalParams<'a> {
    process_handle: &'a str,
    cwd: &'a str,
    size: agent_core::client::TerminalSize,
    command: &'a [&'a str],
    env: TerminalEnvironment,
    tty: bool,
    stream_stdin: bool,
    stream_stdout_stderr: bool,
    timeout_ms: Option<u64>,
    output_bytes_cap: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "UPPERCASE")]
struct TerminalEnvironment {
    term: &'static str,
    colorterm: &'static str,
}

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    claude: OnceLock<crate::claude::Claude>,
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    codex: super::codex::Codex,
    desktop_projects: DesktopProjectStore,
    router: SessionRouter,
    event_pump_started: OnceLock<()>,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    process_directories: std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
}

impl HostRpcService {
    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        desktop_projects: DesktopProjectStore,
    ) -> Self {
        let files = crate::workspace_files::WorkspaceFiles::new(
            desktop_projects.path().with_file_name("bex-attachments"),
        );
        Self {
            inner: Arc::new(ServiceInner {
                claude: OnceLock::new(),
                accounts: tokio::sync::Mutex::new(None),
                restoration_error: tokio::sync::watch::channel(None).0,
                codex: super::codex::Codex::new(codex),
                worktrees: crate::worktrees::Worktrees::new(desktop_projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                process_directories: std::sync::Mutex::new(Default::default()),
                desktop_projects,
                router: SessionRouter::new(),
                event_pump_started: OnceLock::new(),
                files,
            }),
        }
    }

    pub async fn enable_accounts(
        &self,
        directory: std::path::PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        let accounts = crate::codex_accounts::Accounts::load(
            directory,
            config,
            self.inner
                .codex
                .server()
                .map_err(|error| error.to_string())?,
            self.inner.restoration_error.clone(),
        )
        .await?;
        *self.inner.accounts.lock().await = Some(accounts);
        Ok(())
    }

    pub async fn enable_claude(
        &self,
        program: std::path::PathBuf,
        directory: std::path::PathBuf,
        native_home: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        let claude =
            crate::claude::Claude::load(program, directory, native_home, self.inner.router.clone())
                .await?;
        self.inner
            .claude
            .set(claude)
            .map_err(|_| "Claude Code is already configured".into())
    }

    pub(crate) async fn shutdown_claude(&self) {
        if let Some(claude) = self.inner.claude.get() {
            claude.shutdown().await;
        }
    }

    pub fn open_session(&self, capacity: usize) -> HostSession {
        self.start_codex_event_pump();
        self.inner.router.open_session(capacity)
    }

    pub fn close_session(&self, session: SessionId) {
        self.inner.files.clear_session(session);
        self.inner.router.close_session(session);
    }

    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        match self.inner.codex.server() {
            Ok(_) => serde_json::json!({}),
            Err(error) => serde_json::json!({"codex":error}),
        }
    }

    pub fn start(&self) {
        self.start_codex_event_pump();
    }

    /// Dispatch a classified message from an authenticated session.
    pub async fn dispatch(
        &self,
        session: SessionId,
        message: &RpcMessage<'_>,
    ) -> Result<(), String> {
        self.inner.router.ensure_session(session)?;
        let line = match message.kind() {
            RpcMessageKind::Request => {
                if message.method() == Some("host/session/open") {
                    return self.session_open(session, message).await;
                }
                let input = if matches!(
                    message.method(),
                    Some("turn/start" | "turn/steer" | "thread/queue/add")
                ) {
                    let params: serde_json::Value = message.params().map_err(invalid_message)?;
                    match self.inner.router.begin_submission(&params) {
                        Ok(target) => Some((
                            target,
                            params["clientUserMessageId"].as_str().unwrap().to_owned(),
                        )),
                        Err(error) => {
                            return self.inner.router.send_line(
                                session,
                                message
                                    .error("submission_outcome_unknown", &error)
                                    .map_err(invalid_message)?,
                            );
                        }
                    }
                } else {
                    None
                };
                let response = self
                    .request(session, message)
                    .await
                    .map_err(invalid_message)?;
                if let Some((target, id)) = input {
                    let envelope: serde_json::Value =
                        serde_json::from_str(&response).map_err(invalid_message)?;
                    if !envelope["error"].is_null()
                        && envelope["error"]["code"] != "submission_outcome_unknown"
                    {
                        self.inner.router.reject_submission(&target, &id);
                    }
                }
                return self.inner.router.send_line(session, response);
            }
            RpcMessageKind::Notification => {
                if matches!(message.method(), Some("initialize" | "initialized")) {
                    return Err(format!(
                        "method {} is owned by the Host daemon",
                        message.method().unwrap_or_default()
                    ));
                }
                message.line()
            }
            RpcMessageKind::Response => {
                return Err("provider responses must use host/session/answer".into());
            }
        };
        let sent = match self.inner.codex.server() {
            Ok(codex) => codex.send_raw(line).await.map_err(Failure::from),
            Err(error) => Err(error),
        };
        if let Err(error) = sent {
            agent_core::diagnostics::error("host.codex.delivery", &error.to_string());
            if let Some(id) = message.raw_id() {
                self.inner.router.response_unknown(id);
            }
        }
        Ok(())
    }

    async fn answer_request(
        &self,
        session: SessionId,
        id: serde_json::Value,
        result: serde_json::Value,
    ) -> Result<agent_core::models::Empty, Failure> {
        let id = id.to_string();
        let (provider, native) = self
            .inner
            .router
            .claim_response(session, &id, &result)
            .map_err(|error| Failure::new("invalid_answer", error))?;
        let line = serde_json::json!({"id":native,"result":result}).to_string();
        let sent = if provider == agent_core::session::ProviderKind::Claude {
            match self.inner.claude.get() {
                Some(claude) => claude
                    .respond(&RpcMessage::parse(&line).map_err(Failure::from)?)
                    .await
                    .map(|_| ()),
                None => Err("Claude is unavailable".into()),
            }
        } else {
            match self.inner.codex.server() {
                Ok(codex) => codex.send_raw(&line).await.map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            }
        };
        if let Err(error) = sent {
            self.inner.router.response_unknown(&id);
            return Err(Failure::new("answer_delivery_unknown", error));
        }
        Ok(agent_core::models::Empty {})
    }

    async fn session_open(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<(), String> {
        let result = async {
            let params: agent_core::session::OpenSession =
                request.params().map_err(invalid_message)?;
            let target = params.session.clone();
            if target.provider == agent_core::session::ProviderKind::Codex {
                self.inner
                    .codex
                    .server()
                    .map_err(|error| error.to_string())?;
            }
            let limit = params.limit;
            let read = self.inner.router.begin_session_read(params)?;
            let id = target.thread_id();
            let mut response = match target.provider {
                agent_core::session::ProviderKind::Claude => {
                    self.inner
                        .claude
                        .get()
                        .ok_or("Claude is unavailable")?
                        .read(&id, limit)
                        .await?
                }
                agent_core::session::ProviderKind::Codex => self
                    .inner
                    .codex
                    .read(&id, limit)
                    .await
                    .map_err(|error| error.to_string())?,
            };
            self.inner
                .desktop_projects
                .enrich_threads(std::slice::from_mut(&mut response.thread))
                .await
                .map_err(|error| error.to_string())?;
            response.thread.extra.insert(
                "capabilities".into(),
                serde_json::to_value(target.capabilities()).map_err(|error| error.to_string())?,
            );
            let more = response.thread.extra.get("historyHasMore")
                == Some(&serde_json::Value::Bool(true))
                || response
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.items_has_more == Some(true));
            response
                .thread
                .extra
                .insert("historyHasMore".into(), more.into());
            response
                .thread
                .extra
                .insert("historyLimit".into(), limit.into());
            response
                .thread
                .extra
                .entry("historyReadState")
                .or_insert_with(
                    || serde_json::json!({"type": if more {"partial"} else {"complete"}}),
                );
            self.inner
                .router
                .finish_session_read(read, session, request, response)
        }
        .await;
        if let Err(error) = result {
            self.inner.router.send_line(
                session,
                request
                    .error("session_open_failed", &error)
                    .map_err(invalid_message)?,
            )?;
        }
        Ok(())
    }

    async fn read_item(
        &self,
        session: SessionId,
        params: op::ReadItem,
    ) -> Result<agent_core::client::ItemResponse, Failure> {
        let live = self
            .inner
            .router
            .current_turn(&params.thread_id, &params.turn_id)
            .and_then(|turn| {
                turn.items?
                    .into_iter()
                    .find(|item| item.id == params.item_id)
            });
        let mut response = if let Some(item) = live {
            agent_core::client::ItemResponse {
                item: Arc::unwrap_or_clone(item),
                transfer: None,
                extra: Default::default(),
            }
        } else if params.thread_id.starts_with("claude:") {
            let claude = self
                .inner
                .claude
                .get()
                .ok_or_else(|| Failure::new("claude_unavailable", "Claude is unavailable"))?;
            let value = claude
                .request(
                    "host/thread/item/read",
                    serde_json::to_value(&params).expect("item request serializes"),
                )
                .await
                .map_err(|error| Failure::new("item_read_failed", error))?;
            serde_json::from_value(value)
                .map_err(|error| Failure::new("item_read_failed", error))?
        } else {
            self.inner.codex.item_read(params).await?
        };
        let bytes = serde_json::to_vec(&response.item).expect("item serializes");
        if bytes.len() > agent_core::models::MAX_INLINE_ITEM_BYTES {
            response.transfer = Some(
                self.inner
                    .files
                    .download_bytes(session, bytes)
                    .await
                    .map_err(|error| Failure::new("item_transfer_failed", error))?,
            );
            response.item.retain_header();
        }
        Ok(response)
    }

    async fn request(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<String, RpcMessageError> {
        let line = request.line();
        let method = request.method().expect("classified request has a method");
        if method == "host/thread/item/read" {
            return request.response(self.read_item(session, request.params()?).await);
        }
        if matches!(
            method,
            "host/thread/read"
                | "host/thread/watch"
                | "host/thread/unwatch"
                | "host/thread/turns/list"
                | "host/thread/items/list"
                | "thread/read"
                | "thread/turns/list"
                | "thread/items/list"
        ) {
            return request.error(
                "retired_session_rpc",
                &"use host/session/open or host/thread/item/read",
            );
        }
        let _workspace_read = if matches!(
            method,
            HOST_THREAD_START_METHOD
                | "thread/start"
                | "host/thread/resume"
                | "thread/resume"
                | "turn/start"
                | "turn/steer"
                | "host/terminal/start"
                | "process/spawn"
                | "process/exec"
                | "host/file/write"
                | "host/blob/upload"
        ) {
            Some(self.inner.worktree_access.read().await)
        } else {
            None
        };
        let registered_process = if matches!(method, "host/terminal/start" | "process/spawn") {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ProcessDirectory {
                process_handle: String,
                cwd: String,
            }
            if let Ok(params) = request.params::<ProcessDirectory>() {
                let cwd = tokio::fs::canonicalize(&params.cwd)
                    .await
                    .unwrap_or_else(|_| std::path::PathBuf::from(params.cwd));
                let mut processes = self.inner.process_directories.lock().unwrap();
                if processes.contains_key(&params.process_handle) {
                    None
                } else {
                    processes.insert(params.process_handle.clone(), cwd);
                    Some(params.process_handle)
                }
            } else {
                None
            }
        } else {
            None
        };
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Target<'a> {
            thread_id: Option<&'a str>,
            model: Option<&'a str>,
        }
        let target = request.params::<Target>().unwrap_or_default();
        let claude_thread = target.thread_id.is_some_and(|id| id.starts_with("claude:"));
        if !claude_thread
            && matches!(method, "turn/start" | "turn/steer" | "thread/queue/add")
            && let Err(error) = self.inner.codex.server()
        {
            return request.response::<(), _>(Err(error));
        }
        if method == "turn/start"
            && !claude_thread
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return request.error("account_unavailable", &error);
        }
        let response = async {
            if claude_thread {
                let result = match self.inner.claude.get() {
                    Some(claude) => claude
                        .request(method, request.params()?)
                        .await
                        .map_err(|error| {
                            let code = if error.starts_with("submission outcome unknown:") {
                                "submission_outcome_unknown"
                            } else {
                                "claude_failed"
                            };
                            Failure::new(code, error)
                        }),
                    None => Err(Failure::new(
                        "claude_unavailable",
                        "このHostではClaude Codeが有効になっていません。",
                    )),
                };
                let result = match result {
                    Ok(mut value) if value.get("thread").is_some() => {
                        let mut thread: Thread = serde_json::from_value(value["thread"].take())?;
                        self.inner
                            .desktop_projects
                            .enrich_threads(std::slice::from_mut(&mut thread))
                            .await
                            .map_err(Failure::from)
                            .map(|()| {
                                value["thread"] =
                                    serde_json::to_value(thread).expect("Thread serializes");
                                value
                            })
                    }
                    result => result,
                };
                return request.response(result);
            }
            if method == "turn/start"
                && target
                    .model
                    .is_some_and(|model| model.starts_with(crate::claude::MODEL_PREFIX))
            {
                return request.error(
                    "provider_mismatch",
                    &"Claudeへ切り替える場合は新しい会話を作成してください。",
                );
            }
            let response = match method {
                "initialize" | "initialized" => request.error(
                    "daemon_owned_method",
                    &format_args!("{method} is managed by the Host daemon"),
                )?,
                "host/account/list"
                | "host/account/select"
                | "host/account/logout"
                | "host/account/login/start"
                | "host/account/login/status"
                | "host/account/login/cancel" => {
                    if let Err(error) = self.inner.codex.server() {
                        return request.response::<(), _>(Err(error));
                    }
                    let mut accounts = self.inner.accounts.lock().await;
                    let result = match accounts.as_mut() {
                        Some(accounts) => {
                            run_handler(
                                serde_json::from_str(line),
                                "account_operation_failed",
                                |params| async move {
                                    accounts
                                        .request(
                                            self.inner.codex.server().map_err(|error| error.to_string())?,
                                            params,
                                        )
                                        .await
                                },
                            )
                            .await
                        }
                        None => Err(Failure::new(
                            "account_operation_failed",
                            "このHostはアカウント切り替えに対応していません。",
                        )),
                    };
                    request.response(result)?
                }

                "model/list" if self.inner.claude.get().is_some() => {
                    let line = match self.inner.codex.request(line).await {
                        Ok(line) => line,
                        Err(error) => {
                            request.response::<agent_core::client::ModelPage, _>(Err(error))?
                        }
                    };
                    let mut response = RpcResponse::<agent_core::client::ModelPage>::parse(&line)?;
                    if let Err(error) = &response.outcome {
                        let mut extra = serde_json::Map::new();
                        extra.insert("providerErrors".into(), serde_json::json!({"codex":error}));
                        response.outcome = Ok(agent_core::client::ModelPage {
                            data: Vec::new(),
                            next_cursor: None,
                            extra,
                        });
                    }
                    let first_page = request.params::<serde_json::Value>()?["cursor"].is_null();
                    let page = response.outcome.as_mut().expect("model page initialized");
                    if first_page {
                        match self.inner.claude.get().unwrap().models().await {
                            Ok(models) => page.data.extend_from_slice(models),
                            Err(error) => {
                                let errors = page
                                    .extra
                                    .entry("providerErrors")
                                    .or_insert_with(|| serde_json::json!({}));
                                let Some(errors) = errors.as_object_mut() else {
                                    return request.error(
                                        "invalid_model_catalog",
                                        &"providerErrors must be an object",
                                    );
                                };
                                errors
                                    .insert("claude".into(), serde_json::json!({"message":error}));
                            }
                        }
                    }
                    if first_page
                        && page.data.is_empty()
                        && page.extra.contains_key("providerErrors")
                    {
                        request.error("models_unavailable", &page.extra["providerErrors"])?
                    } else {
                        serde_json::to_string(&response)?
                    }
                }

                "host/session/request" => {
                    let params: agent_core::state::operations::OpenRequest = request.params()?;
                    request.response(self.inner.router.request_session(&params.request_id.to_string()).ok_or_else(|| Failure::new("request_unavailable", "request is no longer pending")))?
                }
                "host/session/scope" => {
                    let codex = self.inner.desktop_projects.path().parent().unwrap_or_else(|| std::path::Path::new("."));
                    let path = canonical_storage_path;
                    let areas = serde_json::json!({"codex":path(codex),"claude":self.inner.claude.get().map(|claude| path(claude.storage_directory()))});
                    let digest = ring::digest::digest(&ring::digest::SHA256, areas.to_string().as_bytes());
                    let scope: String = digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
                    request.response::<_, ()>(Ok(scope))?
                }
                "host/session/answer" => {
                    let params: serde_json::Value = request.params()?;
                    request.response(
                        self.answer_request(
                            session,
                            params["requestId"].clone(),
                            params["result"].clone(),
                        )
                        .await,
                    )?
                }
                "host/session/close" => {
                    let params: agent_core::session::CloseSession = request.params()?;
                    self.inner
                        .router
                        .close_subscription(session, params.subscription_id);
                    request.response::<_, ()>(Ok(agent_core::models::Empty {}))?
                }
                HOST_THREAD_LIST_METHOD => {
                    let params: op::ListThreads = request.params()?;
                    request.response(self.host_title_list(params.query).await)?
                }
                "host/worktree/settings/read" | "host/worktree/settings/update" => {
                    let update = if method.ends_with("/update") {
                        request.params().map(Some)
                    } else {
                        Ok(None)
                    };
                    request.response(
                        run_handler(update, "worktree_settings_failed", |update| async move {
                            self.inner.worktrees.settings(update).await
                        })
                        .await,
                    )?
                }
                "host/worktree/list" => request.response(self.worktree_list().await)?,
                "host/worktree/remove" => {
                    let _exclusive = self.inner.worktree_access.write().await;
                    request.response(self.remove_worktree(request.params()?).await)?
                }

                HOST_THREAD_START_METHOD | "thread/start" => request.response(self.start_thread(request).await)?,
                "host/terminal/start" => match request.params::<op::StartTerminal>() {
                    Ok(params) => {
                        let params = TerminalParams {
                            process_handle: &params.handle,
                            cwd: &params.cwd,
                            size: params.size,
                            command: crate::platform::terminal_command(),
                            env: TerminalEnvironment {
                                term: "xterm-256color",
                                colorterm: "truecolor",
                            },
                            tty: true,
                            stream_stdin: true,
                            stream_stdout_stderr: true,
                            timeout_ms: None,
                            output_bytes_cap: None,
                        };
                        match self.inner.codex.request(&request.request("process/spawn", &params)?)
                            .await
                        {
                            Ok(response) => response,
                            Err(error) => request.error("terminal_start_failed", &error)?,
                        }
                    }
                    Err(error) => request.error("invalid_terminal_params", &error)?,
                },
                "host/dictation/transcribe" => request.response(
                    run_handler(
                        request.params().map_err(|_| "録音データがありません。"),
                        "dictation_failed",
                        |params| async move {
                            crate::dictation::transcribe(
                                self.inner.codex.server().map_err(|error| error.to_string())?,
                                &params,
                            )
                            .await
                        },
                    )
                    .await,
                )?,
                "host/workspace/review" => request.response(
                    run_handler(
                        request
                            .params::<op::ReviewWorkspace>()
                            .map_err(|_| "working directory is required"),
                        "workspace_review_failed",
                        |params| async move { crate::inspect_workspace(params.cwd).await },
                    )
                    .await,
                )?,
                "host/file/list"
                | "host/file/read"
                | "host/file/write"
                | "host/blob/upload"
                | "host/blob/download"
                | "host/visualize/read" => request.response(
                    run_handler(
                        serde_json::from_str(line).map_err(|_| "invalid file parameters"),
                        "file_operation_failed",
                        |params| async move { self.inner.files.request(session, params).await },
                    )
                    .await,
                )?,
                _ => match self.inner.codex.request(line).await {
                    Ok(response) => response,
                    Err(error) => request.error(if matches!(method, "turn/start" | "turn/steer" | "thread/queue/add") { "submission_outcome_unknown" } else { "codex_unavailable" }, &error)?,
                },
            };
            Ok::<_, RpcMessageError>(response)
        }
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => request.error("invalid_params", &invalid_message(error))?,
        };
        if let Ok(response) = RpcMessage::parse(&response)
            && let Some(error) = response.raw_error()
        {
            if let Some(handle) = registered_process {
                self.inner
                    .process_directories
                    .lock()
                    .unwrap()
                    .remove(&handle);
            }
            agent_core::diagnostics::rpc_error(
                method,
                request.raw_id().and_then(|id| id.parse().ok()),
                error,
            );
        }
        Ok(response)
    }

    pub(crate) fn files(&self) -> &crate::workspace_files::WorkspaceFiles {
        &self.inner.files
    }

    async fn worktree_list(&self) -> Result<Vec<agent_core::models::Worktree>, Failure> {
        let mut worktrees = self
            .inner
            .worktrees
            .list()
            .await
            .map_err(|error| Failure::new("worktree_list_failed", error))?;
        if worktrees.is_empty() {
            return Ok(worktrees);
        }
        let mut params = ThreadListParams {
            limit: 100,
            sort_key: "updated_at",
            sort_direction: "desc",
            use_state_db_only: true,
            search_term: None,
            cursor: None,
        };
        let mut cursors = std::collections::HashSet::new();
        let mut threads = match self.inner.claude.get() {
            Some(claude) => claude
                .list("")
                .await
                .map_err(|error| Failure::new("claude_history_unavailable", error))?,
            None => Vec::new(),
        };
        loop {
            let page = match self.inner.codex.thread_page(&params).await {
                Ok(page) => page,
                Err(error) => {
                    for worktree in &mut worktrees {
                        worktree.blocked_reason = Some(format!(
                            "Codexの稼働状況を確認できないため削除できません: {error}"
                        ));
                    }
                    break;
                }
            };
            threads.extend(page.data);
            params.cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            let Some(cursor) = &params.cursor else {
                break;
            };
            if !cursors.insert(cursor.clone()) {
                return Err(Failure::new(
                    "invalid_thread_list",
                    "thread list cursor repeated",
                ));
            }
        }
        for thread in threads {
            let Some(cwd) = thread.cwd.as_deref() else {
                continue;
            };
            let cwd = tokio::fs::canonicalize(cwd)
                .await
                .unwrap_or_else(|_| std::path::PathBuf::from(cwd));
            for worktree in &mut worktrees {
                if !cwd.starts_with(&worktree.path) {
                    continue;
                }
                let active = thread
                    .status
                    .as_ref()
                    .is_some_and(|status| status.kind == "active");
                if active {
                    worktree.blocked_reason = Some("このワークツリーで作業を実行中です。完了または停止してから削除してください。".into());
                }
                if let Some(id) = &thread.id {
                    worktree.threads.push(agent_core::models::WorktreeThread {
                        id: id.clone(),
                        name: thread
                            .name
                            .clone()
                            .filter(|name| !name.is_empty())
                            .or_else(|| {
                                thread
                                    .preview
                                    .as_deref()
                                    .map(|preview| preview.chars().take(120).collect())
                            })
                            .unwrap_or_else(|| "新しいチャット".into()),
                        active,
                    });
                }
            }
        }
        let processes = self.inner.process_directories.lock().unwrap();
        for worktree in &mut worktrees {
            if processes
                .values()
                .any(|cwd| cwd.starts_with(&worktree.path))
            {
                worktree.blocked_reason =
                    Some("このワークツリーのターミナルを閉じてから削除してください。".into());
            }
        }
        Ok(worktrees)
    }

    async fn remove_worktree(&self, params: op::RemoveWorktree) -> Result<(), Failure> {
        let entries = self.worktree_list().await?;
        let entry = entries
            .iter()
            .find(|entry| entry.path == params.path)
            .ok_or_else(|| {
                Failure::new(
                    "worktree_remove_failed",
                    "Bexが作成したワークツリーではありません。",
                )
            })?;
        if let Some(reason) = &entry.blocked_reason {
            return Err(Failure::new("worktree_remove_failed", reason));
        }
        self.inner
            .worktrees
            .remove(params.path)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }

    async fn host_title_list(
        &self,
        query: ListQuery,
    ) -> Result<agent_core::models::ThreadList, Failure> {
        let snapshot = self
            .inner
            .desktop_projects
            .load()
            .await
            .map_err(Failure::from)?;
        let mut titles =
            crate::desktop_projects::titles::TitleList::new(&snapshot.projects, &query);
        let mut params = ThreadListParams {
            limit: 100,
            sort_key: "updated_at",
            sort_direction: "desc",
            use_state_db_only: true,
            search_term: (!query.search_term.trim().is_empty())
                .then_some(query.search_term.as_str()),
            cursor: None,
        };
        let claude_listing = async {
            match self.inner.claude.get() {
                Some(claude) => claude.list(&query.search_term).await,
                None => Ok(Vec::new()),
            }
        };
        let (claude_result, codex_result) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(5), claude_listing),
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.inner.codex.thread_page(&params)
            ),
        );
        let mut provider_errors = serde_json::Map::new();
        let mut claude_threads = match claude_result
            .unwrap_or_else(|_| Err("Claude listing timed out; results are partial".into()))
        {
            Ok(threads) => threads,
            Err(error) => {
                provider_errors.insert("claude".into(), serde_json::json!({"message":error}));
                Vec::new()
            }
        }
        .into_iter()
        .peekable();
        let mut first_page = Some(codex_result.unwrap_or_else(|_| {
            Err(Failure::new(
                "provider_timeout",
                "Codex listing timed out; results are partial",
            ))
        }));
        let mut cursors = std::collections::HashSet::new();
        loop {
            // DB metadata avoids scanning or repairing the rollout. Every page
            // uses one Desktop snapshot and the same membership decisions.
            let result = match first_page.take() {
                Some(result) => result,
                None => self.inner.codex.thread_page(&params).await,
            };
            let page = match result {
                Ok(page) => page,
                Err(error) if self.inner.claude.get().is_some() => {
                    provider_errors.insert(
                        "codex".into(),
                        serde_json::to_value(error).expect("Failure serializes"),
                    );
                    break;
                }
                Err(error) => return Err(error),
            };
            for mut thread in page.data {
                while claude_threads.peek().is_some_and(|claude| {
                    crate::claude::updated_at(claude) >= crate::claude::updated_at(&thread)
                }) {
                    let mut claude = claude_threads.next().unwrap();
                    snapshot.enrich_thread(&mut claude);
                    titles.push(claude);
                }
                snapshot.enrich_thread(&mut thread);
                titles.push(thread);
            }
            params.cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            if titles.complete() || params.cursor.is_none() {
                break;
            }
            if !cursors.insert(params.cursor.as_ref().unwrap().clone()) {
                return Err(Failure::new(
                    "invalid_thread_list",
                    "thread list cursor repeated",
                ));
            }
        }
        for mut thread in claude_threads {
            snapshot.enrich_thread(&mut thread);
            titles.push(thread);
        }
        let mut page = titles.finish();
        let merged = crate::worktrees::merged_directories(
            page.data
                .iter()
                .filter_map(|thread| thread.cwd.clone())
                .collect(),
        )
        .await
        .map_err(|error| Failure::new("worktree_status_failed", error))?;
        for thread in &mut page.data {
            thread.worktree_merged =
                Some(thread.cwd.as_ref().is_some_and(|cwd| merged.contains(cwd)));
        }
        if !provider_errors.is_empty() {
            page.extra
                .insert("providerErrors".into(), provider_errors.into());
        }
        Ok(page)
    }

    async fn start_thread(&self, request: &RpcMessage<'_>) -> Result<ThreadResponse, Failure> {
        let mut params: ThreadParams = request.params()?;
        if !params
            .extra
            .get("model")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|model| model.starts_with(crate::claude::MODEL_PREFIX))
        {
            self.inner.codex.server()?;
        }
        // A missing selection must not inherit the App Server's checkout.
        // Keep the real cwd on the thread; project enrichment identifies
        // this persisted location as a chat even after a Host restart.
        if params
            .cwd
            .as_deref()
            .is_none_or(|cwd| cwd.trim().is_empty())
        {
            let directory = self.inner.desktop_projects.chat_directory();
            tokio::fs::create_dir_all(&directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            let directory = tokio::fs::canonicalize(directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            params.cwd = Some(directory.into_os_string().into_string().map_err(|_| {
                Failure::new("chat_directory_unavailable", "chat path is not UTF-8")
            })?);
        } else {
            match self.inner.worktrees.prepare(params.cwd.as_deref()).await {
                Ok(Some(cwd)) => {
                    params.cwd = Some(cwd.into_os_string().into_string().map_err(|_| {
                        Failure::new("worktree_creation_failed", "worktree path is not UTF-8")
                    })?)
                }
                Ok(None) => {}
                Err(error) => return Err(Failure::new("worktree_creation_failed", error)),
            }
        }
        let mut response = if let Some(model) = params
            .extra
            .get("model")
            .and_then(serde_json::Value::as_str)
            && model.starts_with(crate::claude::MODEL_PREFIX)
        {
            let claude = self.inner.claude.get().ok_or_else(|| {
                Failure::new(
                    "claude_unavailable",
                    "このHostではClaude Codeが有効になっていません。",
                )
            })?;
            claude
                .create(params.cwd.as_deref().unwrap_or_default(), model)
                .await
                .map_err(|error| Failure::new("claude_unavailable", error))?
        } else {
            let line = self
                .inner
                .codex
                .request(&request.request("thread/start", &params)?)
                .await?;
            RpcResponse::<ThreadResponse>::parse(&line)?
                .outcome
                .map_err(Failure::Upstream)?
        };
        self.inner
            .desktop_projects
            .enrich_threads(std::slice::from_mut(&mut response.thread))
            .await?;
        Ok(response)
    }

    fn start_codex_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Subscribe before spawning the pump. Otherwise Codex can emit a
        // server request in the scheduling gap and it would be lost before
        // there is a receiver to retain it for the next phone.
        let Ok(codex) = &self.inner.codex.process else {
            return;
        };
        let mut events = codex.subscribe();
        let router = self.inner.router.clone();
        let stopped = self.inner.codex.stopped.clone();
        let processed = self.inner.codex.processed.clone();
        let inner = Arc::downgrade(&self.inner);
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
                        if request.method() == Some("process/exited")
                            && let Some(inner) = inner.upgrade()
                        {
                            #[derive(Deserialize)]
                            #[serde(rename_all = "camelCase")]
                            struct Exited {
                                process_handle: String,
                            }
                            if let Ok(params) = request.params::<Exited>() {
                                inner
                                    .process_directories
                                    .lock()
                                    .unwrap()
                                    .remove(&params.process_handle);
                            }
                        }
                        if request.kind() == RpcMessageKind::Request
                            && request.method() == Some("account/chatgptAuthTokens/refresh")
                        {
                            let Some(inner) = inner.upgrade() else {
                                return;
                            };
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
                                let mut accounts = inner.accounts.lock().await;
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
                                if let Ok(codex) = &inner.codex.process {
                                    let _ = codex.send_raw(&line).await;
                                }
                            });
                        } else if request.kind() == RpcMessageKind::Request {
                            let admission = serde_json::from_str(&line)
                                .map_err(|error| error.to_string())
                                .and_then(|request| {
                                    router
                                        .request(agent_core::session::ProviderKind::Codex, request)
                                });
                            if let Err(error) = admission
                                && let Some(inner) = inner.upgrade()
                                && let Ok(codex) = &inner.codex.process
                                && let Ok(response) = request.error(-32000, &error)
                            {
                                let _ = codex.send_raw(&response).await;
                            }
                        } else if let Err(error) = super::codex::event(&router, &request) {
                            agent_core::diagnostics::error("host.codex.event", &error.to_string());
                            break;
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
            if let Some(inner) = inner.upgrade() {
                if let Ok(codex) = &inner.codex.process
                    && let Err(error) = codex.shutdown().await
                {
                    agent_core::diagnostics::error("host.codex.shutdown", &error.to_string());
                }
                let processes = std::mem::take(&mut *inner.process_directories.lock().unwrap());
                for handle in processes.into_keys() {
                    let line = serde_json::json!({"method":"host/terminal/failed","params":{
                                    "processHandle":handle,"message":"Codexとの接続が終了しました。Hostを再起動してからターミナルを開き直してください。"
                                }}).to_string();
                    router.broadcast(&line);
                }
            }
            router.fail_provider(agent_core::session::ProviderKind::Codex,
                            "Codexとの接続が終了したため、この実行は継続できません。Hostを再起動してから再送信してください。");
        });
    }
}

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid raw JSONL message: {error}")
}

// Resolve existing ancestors too: a newly created native directory must not
// change a scope merely because /var is a symlink to /private/var on macOS.
fn canonical_storage_path(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            canonical_storage_path(parent).join(name)
        }
        _ => std::env::current_dir().unwrap_or_default().join(path),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn unavailable_provider_does_not_retain_a_submission_as_in_flight() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            DesktopProjectStore::new(root.path().join("projects.json")),
        );
        let mut session = service.open_session(16);
        let line = serde_json::json!({"id":1,"method":"turn/start","params":{"threadId":"native","clientUserMessageId":"input","input":[]}}).to_string();
        for _ in 0..2 {
            service
                .dispatch(session.id(), &RpcMessage::parse(&line).unwrap())
                .await
                .unwrap();
            let response: serde_json::Value =
                serde_json::from_str(&session.recv().await.unwrap()).unwrap();
            assert_eq!(response["error"]["code"], "codex_unavailable");
        }
    }

    #[tokio::test]
    async fn opening_claude_history_reads_native_files_without_starting_a_cli() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native");
        let project = native.join("projects/example");
        std::fs::create_dir_all(&project).unwrap();
        let id = "12345678-1234-4234-8234-123456789abc";
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            include_str!("../../tests/fixtures/claude-2.1.266.jsonl"),
        )
        .unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            DesktopProjectStore::new(root.path().join("projects.json")),
        );
        service
            .enable_claude(
                root.path().join("does-not-exist"),
                root.path().join("state"),
                Some(native),
            )
            .await
            .unwrap();
        let mut session = service.open_session(16);
        let line = serde_json::json!({"id":1,"method":"host/session/open","params":{"session":{"provider":"claude","id":id},"limit":5}}).to_string();
        service
            .dispatch(session.id(), &RpcMessage::parse(&line).unwrap())
            .await
            .unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&session.recv().await.unwrap()).unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
        let thread = &reply["result"]["response"]["thread"];
        assert!(!thread["turns"].as_array().unwrap().is_empty());
        assert_ne!(thread["historyReadState"]["type"], "unavailable");
        let transcript = project.join(format!("{id}.jsonl"));
        let changed = std::fs::read_to_string(&transcript)
            .unwrap()
            .replace("Fixture user input", "Changed outside Bex");
        std::fs::write(transcript, changed).unwrap();
        // Even with the original subscription open, every open reads native data.
        service
            .dispatch(session.id(), &RpcMessage::parse(&line).unwrap())
            .await
            .unwrap();
        let reply = session.recv().await.unwrap();
        assert!(reply.contains("Changed outside Bex"), "{reply}");
        assert!(!reply.contains("Fixture user input"));
    }

    #[test]
    fn creating_native_storage_does_not_change_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native").join("projects");
        let before = super::canonical_storage_path(&path);
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(before, super::canonical_storage_path(&path));
    }
}
