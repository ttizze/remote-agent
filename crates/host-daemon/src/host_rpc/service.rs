use super::{
    Failure, invalid_message,
    providers::{ProviderResponse, Providers, ThreadCatalog},
    routing::{HostSession, ResponseRoute, SessionId, SessionRouter},
    run_handler,
};
use crate::{DesktopProjectStore, HOST_THREAD_LIST_METHOD, HOST_THREAD_START_METHOD};
use agent_core::{
    models::{ListQuery, ThreadParams, ThreadResponse},
    peer::{RpcMessage, RpcMessageError, RpcMessageKind, RpcResponse},
    state::operations as op,
};
use codex_app_server::CodexAppServer;
use std::sync::Arc;

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}
struct ServiceInner {
    providers: Providers,
    desktop_projects: DesktopProjectStore,
    router: SessionRouter,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
}
impl HostRpcService {
    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        desktop_projects: DesktopProjectStore,
    ) -> Self {
        let router = SessionRouter::new();
        let files = crate::workspace_files::WorkspaceFiles::new(
            desktop_projects.path().with_file_name("bex-attachments"),
        );
        Self {
            inner: Arc::new(ServiceInner {
                providers: Providers::new(codex, router.clone()),
                router,
                files,
                worktrees: crate::worktrees::Worktrees::new(desktop_projects.path()),
                desktop_projects,
                worktree_access: tokio::sync::RwLock::new(()),
            }),
        }
    }
    pub async fn enable_accounts(
        &self,
        directory: std::path::PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        self.inner
            .providers
            .enable_accounts(directory, config)
            .await
    }
    pub async fn enable_claude(
        &self,
        program: std::path::PathBuf,
        directory: std::path::PathBuf,
    ) -> Result<(), String> {
        self.inner.providers.enable_claude(program, directory).await
    }
    pub(crate) async fn shutdown_claude(&self) {
        self.inner.providers.shutdown_claude().await;
    }
    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        self.inner.providers.errors()
    }
    pub fn start(&self) {
        self.inner.providers.start();
    }
    pub fn open_session(&self, capacity: usize) -> HostSession {
        self.start();
        self.inner.router.open_session(capacity)
    }
    pub fn close_session(&self, session: SessionId) {
        self.inner.providers.close_session(session);
        self.inner.files.clear_session(session);
        self.inner.router.close_session(session);
    }
    pub async fn dispatch(
        &self,
        session: SessionId,
        message: &RpcMessage<'_>,
    ) -> Result<(), String> {
        self.inner.router.ensure_session(session)?;
        match message.kind() {
            RpcMessageKind::Request => {
                let response = self
                    .request(session, message)
                    .await
                    .map_err(invalid_message)?;
                self.inner.router.send_line(session, response)
            }
            RpcMessageKind::Notification => {
                if matches!(message.method(), Some("initialize" | "initialized")) {
                    return Err(format!(
                        "method {} is owned by the Host daemon",
                        message.method().unwrap_or_default()
                    ));
                }
                self.inner.providers.notify(message.line()).await;
                Ok(())
            }
            RpcMessageKind::Response => {
                let Some(id) = message.raw_id() else {
                    return Ok(());
                };
                let ResponseRoute::Forward(upstream) =
                    self.inner.router.resolve_response(session, id)
                else {
                    return Ok(());
                };
                self.inner.providers.respond(upstream, message).await
            }
        }
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
        let response = async {
            Ok::<_, RpcMessageError>(match method {
                "initialize" | "initialized" => request.error(
                    "daemon_owned_method",
                    &format_args!("{method} is managed by the Host daemon"),
                )?,
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

                HOST_THREAD_START_METHOD | "thread/start" => {
                    request.forward_response(self.start_thread(request).await)?
                }
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
                _ => {
                    let response = self.inner.providers.request(session, request).await?;
                    match response {
                        ProviderResponse::Raw(line) => line,
                        ProviderResponse::Thread(response) => {
                            request.forward_response(self.enrich_thread(*response).await)?
                        }
                    }
                }
            })
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
        let mut catalog = ThreadCatalog::new(&self.inner.providers, "").await;
        let mut threads = Vec::new();
        while let Some(page) = catalog.next_page().await? {
            threads.extend(page);
        }
        if let Some(reason) = catalog.unavailable_reason() {
            for worktree in &mut worktrees {
                worktree.blocked_reason = Some(format!(
                    "エージェントの稼働状況を確認できないため削除できません: {reason}"
                ));
            }
        }
        threads.extend(catalog.remaining());
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
        let processes = self.inner.providers.process_directories();
        for worktree in &mut worktrees {
            if processes.iter().any(|cwd| cwd.starts_with(&worktree.path)) {
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
        let mut catalog = ThreadCatalog::new(&self.inner.providers, &query.search_term).await;
        while let Some(threads) = catalog.next_page().await? {
            for mut thread in threads {
                snapshot.enrich_thread(&mut thread);
                titles.push(thread);
            }
            if titles.complete() {
                break;
            }
        }
        let (remaining, provider_errors) = catalog.finish()?;
        for mut thread in remaining {
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

    async fn start_thread(
        &self,
        request: &RpcMessage<'_>,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
        let mut params: ThreadParams = request.params()?;
        self.inner.providers.check_start(
            params
                .extra
                .get("model")
                .and_then(serde_json::Value::as_str),
        )?;
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
        let response = self.inner.providers.start_thread(request, params).await?;
        self.enrich_thread(response).await
    }
    async fn enrich_thread(
        &self,
        mut response: RpcResponse<ThreadResponse>,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
        if let Ok(result) = &mut response.outcome {
            self.inner
                .desktop_projects
                .enrich_threads(std::slice::from_mut(&mut result.thread))
                .await?;
        }
        Ok(response)
    }
}
