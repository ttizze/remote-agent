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

use super::routing::{CodexSession, ResponseRoute, SessionId, SessionRouter};
use crate::{
    DesktopProjectStore, HOST_THREAD_LIST_METHOD, HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};

#[derive(Serialize)]
#[serde(untagged)]
enum Failure {
    Host { code: &'static str, message: String },
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
pub struct CodexRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    app_server: Arc<CodexAppServer>,
    desktop_projects: DesktopProjectStore,
    router: SessionRouter,
    event_pump_started: OnceLock<()>,
    stopped: tokio_util::sync::CancellationToken,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    thread_watches: super::thread_watch::ThreadWatches,
}

impl CodexRpcService {
    pub fn new(app_server: Arc<CodexAppServer>, desktop_projects: DesktopProjectStore) -> Self {
        let files = crate::workspace_files::WorkspaceFiles::new(
            app_server
                .initialize_response()
                .codex_home
                .join("bex-attachments"),
        );
        Self {
            inner: Arc::new(ServiceInner {
                accounts: tokio::sync::Mutex::new(None),
                restoration_error: tokio::sync::watch::channel(None).0,
                app_server,
                worktrees: crate::worktrees::Worktrees::new(desktop_projects.path()),
                desktop_projects,
                router: SessionRouter::new(),
                event_pump_started: OnceLock::new(),
                stopped: tokio_util::sync::CancellationToken::new(),
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
            &self.inner.app_server,
            self.inner.restoration_error.clone(),
        )
        .await?;
        *self.inner.accounts.lock().await = Some(accounts);
        Ok(())
    }

    pub fn open_session(&self, capacity: usize) -> CodexSession {
        self.start_event_pump();
        self.inner.router.open_session(capacity)
    }

    pub fn close_session(&self, session: SessionId) {
        self.inner.thread_watches.clear_session(session);
        self.inner.files.clear_session(session);
        self.inner.router.close_session(session);
    }

    /// Begin consuming Codex-originated lines before the first phone connects.
    /// This keeps server requests replayable across phone disconnects.
    pub(crate) async fn stopped(&self) {
        self.inner.stopped.cancelled().await;
    }

    pub fn start(&self) {
        self.start_event_pump();
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
                &rewritten
            }
        };
        self.inner
            .app_server
            .send_raw(line)
            .await
            .map_err(|error| format!("Codex App Server failed: {error}"))
    }

    async fn request(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<String, RpcMessageError> {
        let line = request.line();
        let method = request.method().expect("classified request has a method");
        if method == "turn/start"
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return request.error("account_unavailable", &error);
        }
        let response = async {
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
                    let mut accounts = self.inner.accounts.lock().await;
                    let result = match accounts.as_mut() {
                        Some(accounts) => {
                            run_handler(
                                serde_json::from_str(line),
                                "account_operation_failed",
                                |params| async move {
                                    accounts.request(&self.inner.app_server, params).await
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
                            .inner
                            .app_server
                            .request_raw(&request.request("process/spawn", &params)?)
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
                            crate::dictation::transcribe(&self.inner.app_server, &params).await
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
                _ => match self.inner.app_server.request_raw(line).await {
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
            let response = self
                .inner
                .app_server
                .request::<_, ThreadPage>("thread/list", &params)
                .await
                .map_err(Failure::from)?;
            let page = response.outcome.map_err(Failure::Upstream)?;
            for mut thread in page.data {
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
        Ok(titles.finish())
    }

    async fn host_thread_request(
        &self,
        request: &RpcMessage<'_>,
        method: &str,
        retain_recent_turns: bool,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
        let mut params: ThreadParams = request.params()?;
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
        let hydrate = method == "thread/read" && params.include_turns == Some(true);
        if hydrate {
            params.include_turns = Some(false);
        }
        let response = match self
            .inner
            .app_server
            .request_raw(&request.request(method, &params)?)
            .await
        {
            Ok(response) => response,
            Err(error) => return Err(error.into()),
        };
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
                    let line = match self
                        .inner
                        .app_server
                        .request_raw(&request.request("thread/turns/list", &history)?)
                        .await
                    {
                        Ok(line) => line,
                        Err(error) => return Err(error.into()),
                    };
                    let history: RpcResponse<HistoryPage<Arc<Turn>>> = RpcResponse::parse(&line)?;
                    let mut page = match history.into_result() {
                        Ok(page) => page,
                        Err(response) => return Ok(response),
                    };
                    if params.paginate_history
                        && let Err(error) = self
                            .hydrate_turn_page(
                                &mut page,
                                result.thread.id.as_deref().unwrap_or_default(),
                            )
                            .await
                    {
                        return Err(Failure::new("invalid_thread_history", error));
                    }
                    result.thread.apply_history_page(page);
                } else {
                    params.include_turns = Some(true);
                    let line = match self
                        .inner
                        .app_server
                        .request_raw(&request.request(method, &params)?)
                        .await
                    {
                        Ok(line) => line,
                        Err(error) => return Err(error.into()),
                    };
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
        self.inner
            .app_server
            .request(method, params)
            .await
            .map_err(|error| error.to_string())?
            .outcome
            .map_err(|error| error.get().to_owned())
    }

    async fn hydrate_turn_page(
        &self,
        page: &mut HistoryPage<Arc<Turn>>,
        thread_id: &str,
    ) -> Result<(), String> {
        if page.data.len() > MOBILE_THREAD_PAGE_SIZE {
            return Err("turn page exceeds requested size".into());
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
            turn.items = Some(values);
            turn.items_next_cursor = Some(cursor);
            turn.items_has_more = Some(has_more);
            turn.items_view = Some(if has_more { "summary" } else { "full" }.into());
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
            self.hydrate_turn_page(&mut page, thread_id)
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
                .inner
                .app_server
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

    fn start_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Subscribe before spawning the pump. Otherwise Codex can emit a
        // server request in the scheduling gap and it would be lost before
        // there is a receiver to retain it for the next phone.
        let mut events = self.inner.app_server.subscribe();
        let router = self.inner.router.clone();
        let thread_watches = self.inner.thread_watches.clone();
        let stopped = self.inner.stopped.clone();
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(PeerEvent::Message(message)) => {
                        let line = message.value;
                        let Ok(request) = RpcMessage::parse(&line) else {
                            continue;
                        };
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
                                let _ = inner.app_server.send_raw(&line).await;
                            });
                        } else {
                            router.handle_server_message(&request);
                        }
                    }
                    Ok(PeerEvent::Response { .. }) => {}
                    Ok(PeerEvent::Closed(_))
                    | Err(broadcast::error::RecvError::Closed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => {
                        thread_watches.clear_all();
                        router.close_all();
                        stopped.cancel();
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
