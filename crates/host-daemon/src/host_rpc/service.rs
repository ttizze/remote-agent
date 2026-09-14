use std::sync::{Arc, OnceLock};

use crate::desktop_projects::ThreadPage;
use agent_core::peer::RpcMessageKind;
use agent_core::{
    models::{
        HistoryItem, HistoryPage, HistoryParams, ListQuery, Thread, ThreadListParams, ThreadParams,
        ThreadResponse, Turn,
    },
    peer::{PeerEvent, RpcMessage, RpcMessageError, RpcResponse},
    state::operations as op,
};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::broadcast;

use super::routing::{HostSession, ResponseRoute, SessionId, SessionRouter};
use crate::{
    DesktopProjectStore, HOST_THREAD_LIST_METHOD, HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(untagged)]
enum Failure {
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
    fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
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

const MOBILE_THREAD_PAGE_SIZE: usize = 5;

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    claude: OnceLock<crate::claude::Claude>,
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    codex: Result<Arc<CodexAppServer>, String>,
    desktop_projects: DesktopProjectStore,
    router: SessionRouter,
    event_pump_started: OnceLock<()>,
    codex_stopped: tokio_util::sync::CancellationToken,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    process_directories: std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
    thread_watches: super::thread_watch::ThreadWatches,
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
                codex,
                worktrees: crate::worktrees::Worktrees::new(desktop_projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                process_directories: std::sync::Mutex::new(Default::default()),
                desktop_projects,
                router: SessionRouter::new(),
                event_pump_started: OnceLock::new(),
                codex_stopped: tokio_util::sync::CancellationToken::new(),
                files,
                thread_watches: super::thread_watch::ThreadWatches::default(),
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
            self.codex().map_err(|error| error.to_string())?,
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
    ) -> Result<(), String> {
        let claude =
            crate::claude::Claude::load(program, directory, self.inner.router.clone()).await?;
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
        self.inner.thread_watches.clear_session(session);
        self.inner.files.clear_session(session);
        self.inner.router.close_session(session);
    }

    fn codex(&self) -> Result<&CodexAppServer, Failure> {
        if self.inner.codex_stopped.is_cancelled() {
            return Err(Failure::new(
                "codex_unavailable",
                "Codexが終了しました。Hostを再起動すると再接続できます。Claudeの会話は継続できます。",
            ));
        }
        self.inner
            .codex
            .as_deref()
            .map_err(|error| Failure::new("codex_unavailable", error))
    }

    async fn codex_request(&self, line: &str) -> Result<String, Failure> {
        self.codex()?.request_raw(line).await.map_err(Failure::from)
    }

    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        match self.codex() {
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
        let rewritten;
        let line = match message.kind() {
            RpcMessageKind::Request => {
                let response = self
                    .request(session, message)
                    .await
                    .map_err(invalid_message)?;
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
                let Some(id) = message.raw_id() else {
                    return Ok(());
                };
                let ResponseRoute::Forward(upstream_id) =
                    self.inner.router.resolve_response(session, id)
                else {
                    return Ok(());
                };
                rewritten = message.rewrite_id(&upstream_id).map_err(invalid_message)?;
                if crate::claude::is_permission_id(&upstream_id) {
                    if let Some(claude) = self.inner.claude.get() {
                        claude
                            .respond(&RpcMessage::parse(&rewritten).map_err(invalid_message)?)
                            .await?;
                    }
                    return Ok(());
                }
                &rewritten
            }
        };
        let sent = match self.codex() {
            Ok(codex) => codex.send_raw(line).await.map_err(Failure::from),
            Err(error) => Err(error),
        };
        if let Err(error) = sent {
            agent_core::diagnostics::error("host.codex.delivery", &error.to_string());
            self.inner.router.resolve_codex_requests();
        }
        Ok(())
    }

    async fn request(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<String, RpcMessageError> {
        let line = request.line();
        let method = request.method().expect("classified request has a method");
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
        if method == "turn/start"
            && !claude_thread
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return request.error("account_unavailable", &error);
        }
        let response = async {
            if claude_thread && !matches!(method, "host/thread/watch" | "host/thread/unwatch") {
                let result = match self.inner.claude.get() {
                    Some(claude) => claude
                        .request(method, request.params()?)
                        .await
                        .map_err(|error| Failure::new("claude_failed", error)),
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
                | "host/account/login/start"
                | "host/account/login/status"
                | "host/account/login/cancel" => {
                    if let Err(error) = self.codex() {
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
                                            self.codex().map_err(|error| error.to_string())?,
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
                    let line = match self.codex_request(line).await {
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

                HOST_THREAD_LIST_METHOD => {
                    let params: op::ListThreads = request.params()?;
                    request.response(self.host_title_list(params.query).await)?
                }
                "host/thread/item/read" => {
                    request.response(self.host_thread_item_read(request.params()?).await)?
                }
                "host/thread/watch" | "host/thread/unwatch" => request.response(
                    run_handler(
                        serde_json::from_str(line),
                        "thread_watch_failed",
                        |params| async move {
                            self.inner
                                .thread_watches
                                .request(session, self.inner.router.clone(), params)
                                .await
                        },
                    )
                    .await,
                )?,
                HOST_THREAD_READ_METHOD => request.forward_response(
                    self.host_thread_request(request, "thread/read", true).await,
                )?,
                "host/thread/turns/list" | "host/thread/items/list" => request.response(
                    self.host_thread_history_page(
                        request.params()?,
                        method.ends_with("items/list"),
                    )
                    .await,
                )?,
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

                HOST_THREAD_START_METHOD | "thread/start" => request.forward_response(
                    self.host_thread_request(request, "thread/start", false)
                        .await,
                )?,
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
                        match self
                            .codex_request(&request.request("process/spawn", &params)?)
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
                                self.codex().map_err(|error| error.to_string())?,
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
                "host/file/list" | "host/file/read" | "host/file/write" | "host/blob/upload"
                | "host/blob/download" => request.response(
                    run_handler(
                        serde_json::from_str(line).map_err(|_| "invalid file parameters"),
                        "file_operation_failed",
                        |params| async move { self.inner.files.request(session, params).await },
                    )
                    .await,
                )?,
                _ => match self.codex_request(line).await {
                    Ok(response) => response,
                    Err(error) => request.error("codex_unavailable", &error)?,
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

    async fn codex_thread_page(
        &self,
        params: &ThreadListParams<'_>,
    ) -> Result<ThreadPage, Failure> {
        self.codex()?
            .request("thread/list", params)
            .await
            .map_err(Failure::from)?
            .outcome
            .map_err(Failure::Upstream)
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
            Some(claude) => claude.list("").await,
            None => Vec::new(),
        };
        loop {
            let page = match self.codex_thread_page(&params).await {
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
        let mut claude_threads = match self.inner.claude.get() {
            Some(claude) => claude.list(&query.search_term).await,
            None => Vec::new(),
        }
        .into_iter()
        .peekable();
        let mut provider_errors = serde_json::Map::new();
        let mut cursors = std::collections::HashSet::new();
        let mut params = ThreadListParams {
            limit: 100,
            sort_key: "updated_at",
            sort_direction: "desc",
            use_state_db_only: true,
            search_term: (!query.search_term.trim().is_empty())
                .then_some(query.search_term.as_str()),
            cursor: None,
        };
        loop {
            // DB metadata avoids scanning or repairing the rollout. Every page
            // uses one Desktop snapshot and the same membership decisions.
            let page = match self.codex_thread_page(&params).await {
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

    async fn host_thread_request(
        &self,
        request: &RpcMessage<'_>,
        method: &str,
        retain_recent_turns: bool,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
        let mut params: ThreadParams = request.params()?;
        if method == "thread/start"
            && !params
                .extra
                .get("model")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|model| model.starts_with(crate::claude::MODEL_PREFIX))
        {
            self.codex()?;
        }
        if method == "thread/start" {
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
        }
        if method == "thread/start"
            && let Some(model) = params
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
            let mut response = claude
                .create(params.cwd.as_deref().unwrap_or_default(), model)
                .await
                .map_err(|error| Failure::new("claude_unavailable", error))?;
            self.inner
                .desktop_projects
                .enrich_threads(std::slice::from_mut(&mut response.thread))
                .await?;
            return RpcResponse::parse(&request.response::<_, Failure>(Ok(response))?)
                .map_err(Failure::from);
        }
        let hydrate = method == "thread/read" && params.include_turns == Some(true);
        if hydrate {
            params.include_turns = Some(false);
        }
        let response = self
            .codex_request(&request.request(method, &params)?)
            .await?;
        let mut response: RpcResponse<ThreadResponse> = RpcResponse::parse(&response)?;
        if let Ok(result) = &mut response.outcome {
            if hydrate {
                if result.thread.history_mode.as_deref() == Some("paginated") {
                    let history = HistoryParams {
                        thread_id: result.thread.id.as_deref().unwrap_or_default(),
                        turn_id: None,
                        cursor: None,
                        limit: if params.paginate_history {
                            MOBILE_THREAD_PAGE_SIZE
                        } else {
                            10
                        },
                        sort_direction: "desc",
                        items_view: Some(if params.paginate_history {
                            "notLoaded"
                        } else {
                            "full"
                        }),
                    };
                    let line = self
                        .codex_request(&request.request("thread/turns/list", &history)?)
                        .await?;
                    let response: RpcResponse<HistoryPage<Arc<Turn>>> = RpcResponse::parse(&line)?;
                    let mut page = match response.into_result() {
                        Ok(page) => page,
                        Err(response) => return Ok(response),
                    };
                    if params.paginate_history
                        && let Err(error) = self.hydrate_turn_page(&mut page, &history).await
                    {
                        return Err(Failure::new("invalid_thread_history", error));
                    }
                    result.thread.apply_history_page(page);
                } else {
                    params.include_turns = Some(true);
                    let line = self
                        .codex_request(&request.request(method, &params)?)
                        .await?;
                    let history: RpcResponse<ThreadResponse> = RpcResponse::parse(&line)?;
                    *result = match history.into_result() {
                        Ok(result) => result,
                        Err(response) => return Ok(response),
                    };
                    if !params.paginate_history
                        && let Some(turns) = &mut result.thread.turns
                        && turns.len() > 10
                    {
                        turns.drain(..turns.len() - 10);
                    }
                }
            }
            if let Err(error) = self
                .inner
                .desktop_projects
                .enrich_threads(std::slice::from_mut(&mut result.thread))
                .await
            {
                return Err(error.into());
            }
            if retain_recent_turns && params.defer_item_details {
                result.thread.defer_item_details();
            }
        }
        Ok(response)
    }

    // Keep App Server cursors opaque. Both initial hydration and older pages
    // use the desktop five-turn / 500-item initial window, in pages of 100.
    async fn history_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &HistoryParams<'_>,
    ) -> Result<HistoryPage<T>, String> {
        self.codex()
            .map_err(|error| error.to_string())?
            .request(method, params)
            .await
            .map_err(|error| error.to_string())?
            .outcome
            .map_err(|error| error.get().to_owned())
    }

    async fn hydrate_turn_page(
        &self,
        page: &mut HistoryPage<Arc<Turn>>,
        query: &HistoryParams<'_>,
    ) -> Result<(), String> {
        if page.data.len() > MOBILE_THREAD_PAGE_SIZE {
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
        let mut budget = MOBILE_THREAD_PAGE_SIZE * 100;
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
            let view = if has_more && values.is_empty() {
                "notLoaded"
            } else if has_more {
                "summary"
            } else {
                "full"
            };
            turn.items = Some(values);
            turn.items_next_cursor = Some(cursor);
            turn.items_has_more = Some(has_more);
            turn.items_view = Some(view.into());
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

    async fn host_thread_history_page(
        &self,
        params: op::ReadOlder,
        items: bool,
    ) -> Result<ThreadResponse, Failure> {
        let thread_id = params.thread_id.as_str();
        if thread_id.is_empty() {
            return Err(Failure::new("invalid_params", "threadId is required"));
        }
        let cursor = params.cursor.as_deref().filter(|cursor| !cursor.is_empty());
        if !items && cursor.is_none() {
            return Err(Failure::new("invalid_params", "cursor is required"));
        }
        let turn_id = params.turn_id.as_deref().filter(|id| !id.is_empty());
        if items && turn_id.is_none() {
            return Err(Failure::new("invalid_params", "turnId is required"));
        }
        let upstream = HistoryParams {
            thread_id,
            turn_id: if items { turn_id } else { None },
            cursor,
            sort_direction: "desc",
            limit: if items { 100 } else { MOBILE_THREAD_PAGE_SIZE },
            items_view: if items { None } else { Some("notLoaded") },
        };
        let mut result = ThreadResponse {
            thread: Thread {
                id: Some(thread_id.into()),
                ..Default::default()
            },
            model: None,
            extra: Default::default(),
        };
        let invalid = |error| Failure::new("invalid_thread_history", error);
        if items {
            let page = self
                .history_request::<HistoryItem>("thread/items/list", &upstream)
                .await
                .map_err(|error| Failure::new("codex_unavailable", error))?;
            if cursor.is_some() && page.next_cursor.as_deref() == cursor {
                return Err(invalid("history cursor repeated"));
            }
            let (mut values, next_cursor) = page.into_items().map_err(invalid)?;
            values.reverse();
            let mut turn = Turn {
                id: turn_id.unwrap().into(),
                items: Some(values),
                items_has_more: Some(next_cursor.is_some()),
                items_next_cursor: Some(next_cursor),
                ..Default::default()
            };
            if cursor.is_none() {
                self.preserve_opening_question(&mut turn, thread_id)
                    .await
                    .map_err(|error| Failure::new("invalid_thread_history", error))?;
            }
            result.thread.turns = Some(vec![Arc::new(turn)]);
        } else {
            let mut page = self
                .history_request::<Arc<Turn>>("thread/turns/list", &upstream)
                .await
                .map_err(|error| Failure::new("codex_unavailable", error))?;
            if cursor.is_some() && page.next_cursor.as_deref() == cursor {
                return Err(invalid("history cursor repeated"));
            }
            self.hydrate_turn_page(&mut page, &upstream)
                .await
                .map_err(|error| Failure::new("invalid_thread_history", error))?;
            result.thread.apply_history_page(page);
        }
        if params.defer_item_details {
            result.thread.defer_item_details();
        }
        Ok(result)
    }

    async fn host_thread_item_read(
        &self,
        params: op::ReadItem,
    ) -> Result<agent_core::client::ItemResponse, Failure> {
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
                .codex()?
                .request::<_, HistoryPage<HistoryItem>>("thread/items/list", &query)
                .await
                .map_err(Failure::from)?;
            let page = response.outcome.map_err(Failure::Upstream)?;
            if let Some(entry) = page.data.into_iter().find(|entry| {
                entry.turn_id.as_deref() == Some(params.turn_id.as_str())
                    && entry.item.id == params.item_id
            }) {
                return Ok(agent_core::client::ItemResponse {
                    item: Arc::unwrap_or_clone(entry.item),
                    extra: Default::default(),
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

    fn start_codex_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Subscribe before spawning the pump. Otherwise Codex can emit a
        // server request in the scheduling gap and it would be lost before
        // there is a receiver to retain it for the next phone.
        let Ok(codex) = &self.inner.codex else {
            return;
        };
        let mut events = codex.subscribe();
        let router = self.inner.router.clone();
        let stopped = self.inner.codex_stopped.clone();
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut active_turns = std::collections::HashMap::<String, String>::new();
            loop {
                match events.recv().await {
                    Ok(PeerEvent::Message(message)) => {
                        let line = message.value;
                        let Ok(request) = RpcMessage::parse(&line) else {
                            continue;
                        };
                        if matches!(request.method(), Some("turn/started" | "turn/completed"))
                            && let Ok(params) = request.params::<serde_json::Value>()
                            && let (Some(thread), Some(turn)) =
                                (params["threadId"].as_str(), params["turn"]["id"].as_str())
                        {
                            if request.method() == Some("turn/started") {
                                active_turns.insert(thread.into(), turn.into());
                            } else if active_turns
                                .get(thread)
                                .is_some_and(|active| active == turn)
                            {
                                active_turns.remove(thread);
                            }
                        }
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
                                if let Ok(codex) = &inner.codex {
                                    let _ = codex.send_raw(&line).await;
                                }
                            });
                        } else {
                            router.handle_server_message(&request);
                        }
                    }
                    Ok(PeerEvent::Response { .. }) => {}
                    Ok(PeerEvent::Closed(_))
                    | Err(broadcast::error::RecvError::Closed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => {
                        stopped.cancel();
                        if let Some(inner) = inner.upgrade() {
                            if let Ok(codex) = &inner.codex
                                && let Err(error) = codex.shutdown().await
                            {
                                agent_core::diagnostics::error(
                                    "host.codex.shutdown",
                                    &error.to_string(),
                                );
                            }
                            let processes =
                                std::mem::take(&mut *inner.process_directories.lock().unwrap());
                            for handle in processes.into_keys() {
                                let line = serde_json::json!({"method":"host/terminal/failed","params":{
                                    "processHandle":handle,"message":"Codexとの接続が終了しました。Hostを再起動してからターミナルを開き直してください。"
                                }}).to_string();
                                router.handle_server_message(
                                    &RpcMessage::parse(&line)
                                        .expect("Host notification serializes"),
                                );
                            }
                        }
                        router.resolve_codex_requests();
                        for (thread_id, turn_id) in active_turns {
                            let line = serde_json::json!({"method":"turn/completed","params":{"threadId":thread_id,
                                "turn":{"id":turn_id,"status":"failed","error":{"message":"Codexとの接続が終了したため、この実行は継続できません。Hostを再起動してから再送信してください。"}}}}).to_string();
                            router.handle_server_message(
                                &RpcMessage::parse(&line).expect("Host notification serializes"),
                            );
                        }
                        return;
                    }
                }
            }
        });
    }
}

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid raw JSONL message: {error}")
}
