//! Codex wire adaptation and resources owned by its App Server connection.
use super::super::run_handler;
use super::super::{
    routing::{SessionId, SessionRouter},
    thread_watch::ThreadWatches,
};
use super::{Failure, Provider};
use agent_core::{
    models::{
        HistoryItem, HistoryPage, HistoryParams, Thread, ThreadListParams, ThreadParams,
        ThreadResponse, Turn,
    },
    peer::{PeerEvent, RpcMessage, RpcMessageError, RpcMessageKind, RpcResponse},
    state::operations as op,
};
use codex_app_server::{CodexAppServer, Error as AppServerError};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

impl From<AppServerError> for Failure {
    fn from(error: AppServerError) -> Self {
        Self::new("codex_unavailable", error)
    }
}
pub(super) struct Codex {
    inner: Arc<Inner>,
}
struct Inner {
    codex: Result<Arc<CodexAppServer>, String>,
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    event_pump_started: OnceLock<()>,
    codex_stopped: tokio_util::sync::CancellationToken,
    router: SessionRouter,
    process_directories: std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
    thread_watches: ThreadWatches,
}
const MOBILE_THREAD_PAGE_SIZE: usize = 5;
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

impl Codex {
    pub(super) async fn request(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<String, RpcMessageError> {
        let line = request.line();
        let method = request.method().expect("classified request has a method");
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
        if method == "turn/start"
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return request.error("account_unavailable", &error);
        }
        let response = async {
            Ok::<_, RpcMessageError>(match method {
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
                "host/thread/item/read" => {
                    request.response(self.thread_item_read(request.params()?).await)?
                }
                "host/thread/turns/list" | "host/thread/items/list" => request.response(
                    self.thread_history_page(request.params()?, method.ends_with("items/list"))
                        .await,
                )?,
                _ => match self.codex_request(line).await {
                    Ok(response) => response,
                    Err(error) => request.error("codex_unavailable", &error)?,
                },
            })
        }
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => request.error("invalid_params", &super::invalid_message(error))?,
        };
        if let Ok(response) = RpcMessage::parse(&response)
            && response.raw_error().is_some()
            && let Some(handle) = registered_process
        {
            self.inner
                .process_directories
                .lock()
                .unwrap()
                .remove(&handle);
        }
        Ok(response)
    }
    pub(super) fn close_session(&self, session: SessionId) {
        self.inner.thread_watches.clear_session(session);
    }
    pub(super) fn process_directories(&self) -> Vec<std::path::PathBuf> {
        self.inner
            .process_directories
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }
    pub(super) async fn send(&self, line: &str) {
        let sent = match self.codex() {
            Ok(codex) => codex.send_raw(line).await.map_err(Failure::from),
            Err(error) => Err(error),
        };
        if let Err(error) = sent {
            agent_core::diagnostics::error("host.codex.delivery", &error.to_string());
            self.inner.router.resolve_provider_requests(Provider::Codex);
        }
    }

    pub(super) fn new(codex: Result<Arc<CodexAppServer>, String>, router: SessionRouter) -> Self {
        Self {
            inner: Arc::new(Inner {
                codex,
                router,
                accounts: tokio::sync::Mutex::new(None),
                restoration_error: tokio::sync::watch::channel(None).0,
                event_pump_started: OnceLock::new(),
                codex_stopped: tokio_util::sync::CancellationToken::new(),
                process_directories: std::sync::Mutex::new(Default::default()),
                thread_watches: ThreadWatches::default(),
            }),
        }
    }
    pub(super) async fn enable_accounts(
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

    pub(super) fn codex(&self) -> Result<&CodexAppServer, Failure> {
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

    pub(super) async fn codex_request(&self, line: &str) -> Result<String, Failure> {
        self.codex()?.request_raw(line).await.map_err(Failure::from)
    }

    pub(super) async fn thread_page(
        &self,
        params: &ThreadListParams<'_>,
    ) -> Result<crate::desktop_projects::ThreadPage, Failure> {
        self.codex()?
            .request("thread/list", params)
            .await
            .map_err(Failure::from)?
            .outcome
            .map_err(Failure::Upstream)
    }

    pub(super) async fn thread_request(
        &self,
        request: &RpcMessage<'_>,
        method: &str,
        mut params: ThreadParams,
        retain_recent_turns: bool,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
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

    async fn thread_history_page(
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

    async fn thread_item_read(
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

    pub(super) fn start(&self) {
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
                            router.handle_server_message(Provider::Codex, &request);
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
                                    Provider::Codex,
                                    &RpcMessage::parse(&line)
                                        .expect("Host notification serializes"),
                                );
                            }
                        }
                        router.resolve_provider_requests(Provider::Codex);
                        for (thread_id, turn_id) in active_turns {
                            let line = serde_json::json!({"method":"turn/completed","params":{"threadId":thread_id,
                                "turn":{"id":turn_id,"status":"failed","error":{"message":"Codexとの接続が終了したため、この実行は継続できません。Hostを再起動してから再送信してください。"}}}}).to_string();
                            router.handle_server_message(
                                Provider::Codex,
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
