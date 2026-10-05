//! Host owns persistence and effects; authenticated connections own only delivery.
use super::{
    connections::{Connections, HostReply, HostSession, HostSubscription, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::ProjectStore;
use agent_protocol::{
    operations as op,
    protocol::{self, Body, Call, Response},
    session::ProviderKind,
};
use agent_transport::peer::RpcMessageError;
use codex_app_server::CodexAppServer;
use orchestration::{
    store::Store,
    worker::{AdapterError, EffectWorker, ProviderAdapter},
    *,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
fn now() -> Timestamp {
    Timestamp::from_millis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis() as i64,
    )
    .expect("current timestamp")
}
fn adapter_error(error: impl std::fmt::Display) -> AdapterError {
    AdapterError {
        message: error.to_string(),
        retryable: false,
    }
}
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) delivery: agent_protocol::error::Delivery,
    pub(crate) execution: Option<Box<agent_protocol::execution::ExecutionError>>,
}
impl From<Failure> for agent_protocol::error::RpcFailure {
    fn from(error: Failure) -> Self {
        Self {
            code: error.code.into(),
            message: error.message,
            delivery: error.delivery,
            execution: error.execution,
        }
    }
}
impl From<RpcMessageError> for Failure {
    fn from(error: RpcMessageError) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new("invalid_params", error)
    }
}

impl Failure {
    pub(crate) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: format!("{error:#}"),
            delivery: agent_protocol::error::Delivery::NotSent,
            execution: None,
        }
    }
}

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}
struct ServiceInner {
    store: Arc<Store>,
    resources: Arc<HostResources>,
    connections: Connections,
    provider_receiver: Mutex<Option<tokio::sync::mpsc::Receiver<provider_adapters::ProviderBatch>>>,
    started: AtomicBool,
    stop: tokio::sync::watch::Sender<bool>,
}
struct HostResources {
    codex: Arc<CodexResources>,
    codex_adapter: Option<Arc<provider_adapters::codex::CodexAdapter>>,
    claude: OnceLock<Arc<ClaudeResources>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    projects: ProjectStore,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
    provider_output: tokio::sync::mpsc::Sender<provider_adapters::ProviderBatch>,
    auth_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
}
impl Drop for HostResources {
    fn drop(&mut self) {
        if let Some(adapter) = &self.codex_adapter {
            adapter.shutdown();
        }
    }
}
#[derive(Clone)]
enum StreamTarget {
    Shell,
    Thread(ThreadId),
}
impl HostRpcService {
    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        projects: ProjectStore,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(Store::open(
            projects.path().with_file_name("orchestration-v2.sqlite"),
        )?);
        store.recover(&now())?;
        let (output, receiver) = tokio::sync::mpsc::channel(256);
        let codex_adapter = codex.as_ref().ok().map(|server| {
            provider_adapters::codex::CodexAdapter::new(server.clone(), output.clone())
        });
        let resources = Arc::new(HostResources {
            codex_adapter,
            codex: Arc::new(CodexResources::new(codex.clone())),
            claude: OnceLock::new(),
            startup_errors: Default::default(),
            browser: OnceLock::new(),
            files: crate::workspace_files::WorkspaceFiles::new(
                projects.path().with_file_name("bex-attachments"),
            ),
            worktrees: crate::worktrees::Worktrees::new(projects.path()),
            projects,
            worktree_access: Default::default(),
            permission_settings_access: Default::default(),
            terminals: Default::default(),
            dictation: crate::dictation::Dictation::new(codex),
            provider_output: output,
            auth_task: OnceLock::new(),
        });
        Ok(Self {
            inner: Arc::new(ServiceInner {
                store,
                resources,
                connections: Connections::new(),
                provider_receiver: Mutex::new(Some(receiver)),
                started: AtomicBool::new(false),
                stop: tokio::sync::watch::channel(false).0,
            }),
        })
    }
    fn identities(&self) -> Vec<(ProviderKind, Arc<dyn Identity>)> {
        let mut values = vec![(
            ProviderKind::Codex,
            self.inner.resources.codex.clone() as Arc<dyn Identity>,
        )];
        if let Some(claude) = self.inner.resources.claude.get() {
            values.push((ProviderKind::Claude, claude.clone()));
        }
        values
    }
    fn identity(&self, provider: ProviderKind) -> Result<Arc<dyn Identity>, Failure> {
        self.identities()
            .into_iter()
            .find(|(p, _)| *p == provider)
            .map(|(_, identity)| identity)
            .ok_or_else(|| Failure::new("provider_unavailable", "provider unavailable"))
    }
    pub async fn enable_browser(&self, profile: PathBuf) -> Result<(), String> {
        self.inner
            .resources
            .browser
            .set(crate::browser::Browser::start(profile).await?)
            .map_err(|_| "browser already configured".into())
    }
    pub async fn enable_accounts(
        &self,
        directory: PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        self.inner
            .resources
            .codex
            .enable_accounts(directory, config)
            .await
    }
    pub async fn enable_claude(
        &self,
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let result = ClaudeResources::load(
            program,
            directory,
            native_home,
            self.inner.resources.provider_output.clone(),
        )
        .await;
        match result {
            Ok(claude) => {
                self.inner
                    .resources
                    .claude
                    .set(Arc::new(claude))
                    .map_err(|_| anyhow::anyhow!("Claude already configured"))?;
                Ok(())
            }
            Err(error) => {
                self.inner
                    .resources
                    .startup_errors
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        ProviderKind::Claude,
                        Failure::new("provider_unavailable", &error),
                    );
                Err(error)
            }
        }
    }
    pub fn start(&self) {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(task) = self.inner.resources.codex.auth_requests() {
            let _ = self.inner.resources.auth_task.set(task);
        }
        let mut roots = vec![(
            Driver::Codex,
            self.inner.resources.codex.directory.join("sessions"),
        )];
        if let Some(claude) = self.inner.resources.claude.get() {
            roots.push((Driver::Claude, claude.native_home.join("projects")));
        }
        tokio::spawn(super::import::run(
            self.inner.store.clone(),
            self.inner.resources.projects.clone(),
            roots,
            self.inner.stop.subscribe(),
        ));
        let mut receiver = self
            .inner
            .provider_receiver
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("provider receiver owned once");
        let store = self.inner.store.clone();
        let mut shutdown = self.inner.stop.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed=shutdown.changed()=>{if changed.is_err()||*shutdown.borrow(){break;}}
                    batch=receiver.recv()=>{let Some(batch)=batch else{break;};let result=store.ingest(batch.events,Some((&batch.run_id,Some(&batch.attempt_id))),&batch.occurred_at);let accepted=match result{Ok(commit)=>!commit.events.is_empty(),Err(error)=>{tracing::error!(operation="orchestration.provider.ingest",message=%error);false}};if let Some(receipt)=batch.acknowledged{let _=receipt.send(accepted);}}
                }
            }
        });
        let worker = EffectWorker {
            store: self.inner.store.clone(),
            adapter: self.inner.resources.clone(),
            owner: format!("host:{}", uuid::Uuid::new_v4()),
        };
        let shutdown = self.inner.stop.subscribe();
        tokio::spawn(async move {
            if let Err(error) = worker.run(shutdown).await {
                tracing::error!(operation="orchestration.effects",message=%error);
            }
        });
    }
    pub fn open_session(&self) -> HostSession {
        self.start();
        self.inner.connections.open_session()
    }
    pub(crate) fn open_authenticated_session(&self, principal: String) -> HostSession {
        self.start();
        self.inner
            .connections
            .open_authenticated_session(Some(principal))
    }
    pub fn close_session(&self, session: SessionId) {
        self.inner.connections.close_session(session);
        self.inner.resources.terminals.close_session(session);
        self.inner.resources.files.clear_session(session);
        self.inner.resources.dictation.close_session(session);
    }
    pub(crate) fn revoke_device(&self, principal: &str) {
        self.inner.resources.terminals.revoke_device(principal);
    }
    pub(crate) fn files(&self) -> &crate::workspace_files::WorkspaceFiles {
        &self.inner.resources.files
    }
    pub(crate) fn data_recipients(&self) -> (Vec<String>, Option<String>) {
        let codex = self.inner.resources.codex.availability().is_ok();
        let mut recipients = vec![];
        if codex {
            recipients.push("OpenAI".into());
        }
        if self.inner.resources.claude.get().is_some() {
            recipients.push("Anthropic".into());
        }
        (recipients, codex.then(|| "OpenAI".into()))
    }
    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        let mut errors = serde_json::Map::new();
        if let Err(error) = self.inner.resources.codex.availability() {
            errors.insert(
                "codex".into(),
                serde_json::to_value(error).expect("failure serializes"),
            );
        }
        for (provider, error) in self
            .inner
            .resources
            .startup_errors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            errors.insert(
                provider_key(*provider),
                serde_json::to_value(error).expect("failure serializes"),
            );
        }
        serde_json::Value::Object(errors)
    }
    pub(crate) async fn shutdown_owned_processes(&self) {
        self.inner.stop.send_replace(true);
        if let Some(adapter) = &self.inner.resources.codex_adapter {
            adapter.shutdown();
        }
        if let Some(claude) = self.inner.resources.claude.get() {
            claude.adapter.shutdown().await;
        }
        if let Some(browser) = self.inner.resources.browser.get() {
            browser.shutdown().await;
        }
        self.inner.resources.terminals.shutdown().await;
    }
    fn dispatch_command(
        &self,
        command: &Command,
    ) -> Result<agent_protocol::orchestration::DispatchReceipt, Failure> {
        let instance = match &command.body {
            CommandBody::ThreadCreate {
                model_selection, ..
            }
            | CommandBody::ProviderSwitch { model_selection }
            | CommandBody::ThreadModelSelectionSet { model_selection } => {
                model_selection.instance_id.clone()
            }
            CommandBody::MessageDispatch(message) if message.model_selection.is_some() => message
                .model_selection
                .as_ref()
                .expect("selection exists")
                .instance_id
                .clone(),
            _ => {
                self.inner
                    .store
                    .projection(&command.thread_id)
                    .map_err(store_failure)?
                    .thread
                    .provider_instance_id
            }
        };
        let driver = driver(&instance)?;
        let commit = self
            .inner
            .store
            .dispatch(
                command,
                &now(),
                &provider_adapters::capabilities::capabilities(driver).turns,
                driver,
            )
            .map_err(store_failure)?;
        Ok(agent_protocol::orchestration::DispatchReceipt {
            thread_id: command.thread_id.clone(),
            sequence: commit.sequence,
            replayed: commit.replayed,
        })
    }
    pub async fn dispatch(&self, session: SessionId, call: &Call) -> Result<HostReply, String> {
        self.inner.connections.ensure_session(session)?;
        match call {
            Call::SubscribeShell(params) => {
                return self.subscribe(session, StreamTarget::Shell, params.after_sequence);
            }
            Call::SubscribeThread(params) => {
                return self.subscribe(
                    session,
                    StreamTarget::Thread(params.thread_id.clone()),
                    params.after_sequence,
                );
            }
            _ => {}
        }
        Ok(Response::from_result(self.request(session, call).await).into())
    }
    async fn request(&self, session: SessionId, request: &Call) -> Result<Body, Failure> {
        let _workspace = if matches!(
            request,
            Call::DispatchCommand(_)
                | Call::LaunchThread(_)
                | Call::StartTerminal(_)
                | Call::WriteFile(_)
                | Call::Upload(_)
                | Call::ReviewWorkspace(_)
        ) {
            Some(self.inner.resources.worktree_access.read().await)
        } else {
            None
        };
        let response = match request {
            Call::DispatchCommand(command) => self.dispatch_command(command)?.into(),
            Call::LaunchThread(params) => {
                let mut create = params.create.clone();
                if self.inner.store.projection(&create.thread_id).is_err()
                    && let CommandBody::ThreadCreate {
                        project_id,
                        worktree_path,
                        ..
                    } = &mut create.body
                    && worktree_path.is_none()
                    && project_id.as_str() != "bex:chats"
                {
                    let projects = self.projects().await?;
                    let root = projects
                        .iter()
                        .find(|project| project.id == project_id.as_str())
                        .and_then(|project| project.roots.first())
                        .ok_or_else(|| {
                            Failure::new("project_unavailable", "project root missing")
                        })?;
                    *worktree_path = self
                        .inner
                        .resources
                        .worktrees
                        .prepare(Some(&root.path))
                        .await
                        .map_err(|error| Failure::new("workspace_preparation_failed", error))?
                        .map(|path| path.to_string_lossy().into_owned());
                }
                if !matches!(params.create.body, CommandBody::ThreadCreate { .. }) {
                    return Err(Failure::new(
                        "invalid_launch",
                        "launch requires thread.create",
                    ));
                }
                self.dispatch_command(&create)?;
                self.dispatch_command(&Command {
                    command_id: CommandId::new(format!("{}:input", params.create.command_id))
                        .expect("derived id"),
                    thread_id: params.create.thread_id.clone(),
                    body: CommandBody::MessageDispatch(params.input.clone()),
                })?
                .into()
            }
            Call::GetThreadProjection(params) => self
                .inner
                .store
                .projection(&params.thread_id)
                .map_err(store_failure)?
                .into(),
            Call::GetTurnItem(params) => self
                .inner
                .store
                .turn_item(&params.thread_id, &params.item_id)
                .map_err(store_failure)?
                .into(),
            Call::ReadThreadHistory(params) => self
                .inner
                .store
                .history(
                    &params.thread_id,
                    params.cursor.as_ref(),
                    params.limit as usize,
                )
                .map_err(store_failure)?
                .into(),
            Call::SearchThreads(params) => self
                .inner
                .store
                .search(&params.query, params.limit as usize)
                .map_err(store_failure)?
                .into(),
            Call::AddProject(params) => self
                .inner
                .resources
                .projects
                .register(Path::new(&params.cwd))
                .await
                .map_err(|error| Failure::new("project_add_failed", error))?
                .into(),
            Call::ListProjects(_) => self.projects().await?.into(),
            Call::SessionScope(_) => self
                .inner
                .resources
                .projects
                .path()
                .with_file_name("orchestration-v2.sqlite")
                .to_string_lossy()
                .into_owned()
                .into(),
            Call::ListAccounts(_)
            | Call::SelectAccount(_)
            | Call::LogoutAccount(_)
            | Call::StartAccountLogin(_)
            | Call::ReadAccountLogin(_)
            | Call::SubmitAccountLogin(_)
            | Call::CancelAccountLogin(_) => self.account_request(request.clone()).await?,
            Call::ReadAccountUsage(params) => self
                .identity(params.provider)?
                .usage(&params.id)
                .await?
                .into(),
            Call::ListModels(params) => self.models(params).await?.into(),
            Call::ComposerCatalog(params) => self
                .inner
                .resources
                .codex
                .composer_catalog(&params.cwd)
                .await
                .into(),
            Call::ReadPermissionSettings(params) => {
                let _guard = self.inner.resources.permission_settings_access.lock().await;
                match params.provider {
                    ProviderKind::Codex => self.inner.resources.codex.read_permissions().await?,
                    ProviderKind::Claude => super::permissions::read_claude_permissions(
                        &self
                            .inner
                            .resources
                            .claude
                            .get()
                            .ok_or_else(|| {
                                Failure::new("provider_unavailable", "Claude unavailable")
                            })?
                            .native_home,
                    )?,
                }
                .into()
            }
            Call::UpdatePermissionSettings(params) => {
                let _guard = self.inner.resources.permission_settings_access.lock().await;
                match params.provider {
                    ProviderKind::Codex => {
                        self.inner
                            .resources
                            .codex
                            .update_permissions(params.mode, &params.version)
                            .await?
                    }
                    ProviderKind::Claude => super::permissions::update_claude_permissions(
                        &self
                            .inner
                            .resources
                            .claude
                            .get()
                            .ok_or_else(|| {
                                Failure::new("provider_unavailable", "Claude unavailable")
                            })?
                            .native_home,
                        params.mode,
                        &params.version,
                    )?,
                }
                .into()
            }
            Call::Browser(params) => self
                .inner
                .resources
                .browser
                .get()
                .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?
                .request(params)
                .await
                .map_err(|error| Failure::new("browser_failed", error))?
                .into(),
            Call::ConnectionPerformance(params) => {
                let params = params.clone();
                tokio::task::spawn_blocking(move || {
                    agent_transport::diagnostics::connection_performance(&params)
                })
                .await
                .map_err(|error| Failure::new("diagnostic_write_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadWorktreeSettings(_) | Call::UpdateWorktreeSettings(_) => {
                let update = if let Call::UpdateWorktreeSettings(settings) = request {
                    Some(settings.clone())
                } else {
                    None
                };
                (self
                    .inner
                    .resources
                    .worktrees
                    .settings(update)
                    .await
                    .map_err(|error| Failure::new("worktree_settings_failed", error))?)
                .into()
            }
            Call::ListWorktrees(_) => (self.worktree_list().await?).into(),
            Call::RemoveWorktree(params) => {
                let _exclusive = self.inner.resources.worktree_access.write().await;
                (self.remove_worktree(params.path.clone()).await?).into()
            }

            Call::StartTerminal(params) => (self
                .inner
                .resources
                .terminals
                .start(
                    self.inner.connections.clone(),
                    session,
                    params.handle.clone(),
                    params.cwd.clone(),
                    params.size,
                )
                .await
                .map_err(|error| Failure::new("terminal_start_failed", error))?)
            .into(),
            Call::WriteTerminal(_)
            | Call::ResizeTerminal(_)
            | Call::KillTerminal(_)
            | Call::DetachTerminal(_) => (self
                .inner
                .resources
                .terminals
                .request(session, request)
                .await
                .map_err(|error| Failure::new("terminal_operation_failed", error))?)
            .into(),
            Call::PrepareDictation(params) => {
                self.inner
                    .resources
                    .dictation
                    .prepare(session, params.id.clone())
                    .map_err(|error| Failure::new("dictation_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::CancelDictation(params) => {
                self.inner.resources.dictation.cancel(session, &params.id);
                agent_protocol::models::Empty {}.into()
            }
            Call::Transcribe(params) => (self
                .inner
                .resources
                .dictation
                .transcribe(session, params.preparation.as_deref(), &params.audio)
                .await
                .map_err(|error| Failure::new("dictation_failed", error))?)
            .into(),
            Call::ReviewWorkspace(params) => {
                (crate::inspect_workspace(params.cwd.clone())
                    .await
                    .map_err(|error| Failure::new("workspace_review_failed", error))?)
                .into()
            }
            Call::ListFiles(_)
            | Call::ReadFile(_)
            | Call::WriteFile(_)
            | Call::Upload(_)
            | Call::Download(_)
            | Call::ReadVisualization(_) => self
                .inner
                .resources
                .files
                .request(session, request.clone())
                .await
                .map_err(|error| Failure::new("file_operation_failed", error))?,

            _ => {
                return Err(Failure::new(
                    "method_not_found",
                    format!("unregistered method: {}", request.method()),
                ));
            }
        };
        Ok(response)
    }
    async fn account_request(&self, request: Call) -> Result<Body, Failure> {
        if matches!(request, Call::ListAccounts(_)) {
            let mut combined = op::Accounts {
                accounts: Vec::new(),
                selected: HashMap::new(),
                error: None,
            };
            let mut errors = self
                .inner
                .resources
                .startup_errors
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            for (_, agent) in self.identities() {
                match Identity::list(agent.as_ref()).await {
                    Ok(accounts) => {
                        combined.accounts.extend(accounts.accounts);
                        combined.selected.extend(accounts.selected);
                        errors.extend(accounts.error);
                    }
                    Err(error) => errors.push(error.to_string()),
                }
            }
            combined.error = (!errors.is_empty()).then(|| errors.join("\n"));
            return Ok(combined.into());
        }
        use super::identity::{AccountCommand as Command, AccountReply};
        let (provider, command) = match request {
            Call::StartAccountLogin(p) => (p.provider, Command::StartLogin),
            Call::SelectAccount(p) => (p.provider, Command::Select { id: p.id }),
            Call::LogoutAccount(p) => (p.provider, Command::Logout { id: p.id }),
            Call::ReadAccountLogin(p) => (p.provider, Command::ReadLogin { id: p.id }),
            Call::CancelAccountLogin(p) => (p.provider, Command::CancelLogin { id: p.id }),
            Call::SubmitAccountLogin(p) => (
                p.provider,
                Command::SubmitLogin {
                    id: p.id,
                    code: p.code,
                },
            ),
            _ => return Err(Failure::new("invalid_params", "not an account request")),
        };
        Ok(match self.identity(provider)?.account(command).await? {
            AccountReply::Selection(value) => value.into(),
            AccountReply::Login(value) => value.into(),
            AccountReply::Status(value) => value.into(),
            AccountReply::Complete => agent_protocol::models::Empty {}.into(),
        })
    }

    pub(crate) async fn cleanup_merged_worktrees(&self) -> anyhow::Result<()> {
        if !self
            .inner
            .resources
            .worktrees
            .settings(None)
            .await?
            .delete_merged
        {
            return Ok(());
        }
        let _exclusive = self.inner.resources.worktree_access.write().await;
        let entries = self.worktree_list().await?;
        let statuses = crate::worktrees::directory_statuses(
            entries
                .iter()
                .filter(|entry| entry.blocked_reason.is_none())
                .map(|entry| crate::worktrees::StatusSource {
                    cwd: entry.path.clone(),
                    checkout: None,
                    repository: None,
                    branch: None,
                })
                .collect(),
        )
        .await?;
        for (source, status) in statuses {
            if status == agent_protocol::models::WorktreeStatus::Merged
                && let Err(error) = self
                    .inner
                    .resources
                    .worktrees
                    .remove(source.cwd, true)
                    .await
            {
                tracing::warn!(target: "bex", operation = "host.worktree.cleanup", message = %error);
            }
        }
        Ok(())
    }

    async fn projects(&self) -> Result<Vec<agent_protocol::models::Project>, Failure> {
        let mut projects = self
            .inner
            .resources
            .projects
            .load()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))?
            .projects;
        projects.push(agent_protocol::models::Project {
            id: "bex:chats".into(),
            name: "Chats".into(),
            roots: vec![agent_protocol::models::ProjectRoot {
                path: self
                    .inner
                    .resources
                    .projects
                    .chat_directory()
                    .to_string_lossy()
                    .into_owned(),
            }],
        });
        Ok(projects)
    }
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        let mut page = match self.inner.resources.codex.models(params).await {
            Ok(page) => page,
            Err(error) => op::ModelPage {
                data: vec![],
                next_cursor: None,
                provider_errors: Some(serde_json::Map::from_iter([(
                    "codex".into(),
                    serde_json::to_value(error)?,
                )])),
            },
        };
        if params.cursor.is_none()
            && let Some(claude) = self.inner.resources.claude.get()
        {
            match claude.models().await {
                Ok(models) => page.data.extend(models),
                Err(error) => {
                    page.provider_errors
                        .get_or_insert_default()
                        .insert("claude".into(), serde_json::json!({"message":error}));
                }
            }
        }
        Ok(page)
    }
    async fn worktree_list(&self) -> Result<Vec<agent_protocol::models::Worktree>, Failure> {
        let mut entries = self
            .inner
            .resources
            .worktrees
            .list()
            .await
            .map_err(|error| Failure::new("worktree_list_failed", error))?;
        let snapshot = self.inner.store.shell_snapshot().map_err(store_failure)?;
        let projects = self.projects().await?;
        for shell in snapshot.threads.iter().chain(&snapshot.archived_threads) {
            let cwd = thread_cwd(&shell.thread, &projects)?;
            let active =
                shell.active_run_id.is_some() || !shell.pending_background_tasks.is_empty();
            for entry in entries
                .iter_mut()
                .filter(|entry| cwd.starts_with(&entry.path))
            {
                entry.threads.push(agent_protocol::models::WorktreeThread {
                    id: shell.thread.id.clone(),
                    name: shell.thread.title.clone(),
                    active,
                });
                if active {
                    entry.blocked_reason=Some("このワークツリーで作業を実行中です。完了または停止してから削除してください。".into());
                }
            }
        }
        for entry in &mut entries {
            if self
                .inner
                .resources
                .terminals
                .in_use(Path::new(&entry.path))
            {
                entry.blocked_reason = Some(
                    "このワークツリーで terminal を実行中です。終了してから削除してください。"
                        .into(),
                );
            }
        }
        Ok(entries)
    }
    async fn remove_worktree(&self, path: String) -> Result<(), Failure> {
        let entries = self.worktree_list().await?;
        let entry = entries
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| Failure::new("worktree_remove_failed", "worktree not found"))?;
        if let Some(reason) = entry.blocked_reason {
            return Err(Failure::new("worktree_remove_failed", reason));
        }
        for thread in entry.threads {
            let projection = self
                .inner
                .store
                .projection(&thread.id)
                .map_err(store_failure)?;
            for session in &projection.provider_sessions {
                let effect = Effect {
                    id: format!("detach:{}", uuid::Uuid::new_v4()),
                    thread_id: thread.id.clone(),
                    body: EffectBody::Detach {
                        provider_session_id: session.id.clone(),
                        driver: session.driver,
                    },
                };
                self.inner
                    .resources
                    .execute(&effect, projection.clone())
                    .await
                    .map_err(|error| Failure::new("worktree_remove_failed", error))?;
                self.dispatch_command(&Command {
                    command_id: CommandId::new(format!("detach:{}", uuid::Uuid::new_v4()))
                        .expect("derived id"),
                    thread_id: thread.id.clone(),
                    body: CommandBody::ProviderSessionDetach {
                        provider_session_id: session.id.clone(),
                    },
                })?;
            }
        }
        self.inner
            .resources
            .worktrees
            .remove(path, false)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }
    fn subscribe(
        &self,
        session: SessionId,
        target: StreamTarget,
        after: Option<u64>,
    ) -> Result<HostReply, String> {
        let cancellation = self.inner.connections.cancellation(session)?;
        let (mut frames, mut receiver, mut cursor) =
            subscription_frames(&self.inner.store, &target, after, true)
                .map_err(|error| error.to_string())?;
        let initial = frames.remove(0);
        let (sender, receiver_frames) = tokio::sync::mpsc::channel(2);
        let store = self.inner.store.clone();
        let task = tokio::spawn(async move {
            loop {
                for frame in frames.drain(..) {
                    tokio::select! {biased;_ = cancellation.cancelled()=>return,result=sender.send(frame)=>if result.is_err(){return;}}
                }
                let event = tokio::select! {biased;_=cancellation.cancelled()=>return,event=receiver.recv()=>event};
                match event {
                    Ok(event) => {
                        if event.sequence <= cursor {
                            continue;
                        }
                        cursor = event.sequence;
                        let frame = match &target {
                            StreamTarget::Shell => store
                                .shell_update(&event)
                                .map_err(|error| error.to_string())
                                .and_then(|frame| {
                                    protocol::encode(frame).map_err(|error| error.to_string())
                                }),
                            StreamTarget::Thread(id) if event.event.thread_id == *id => {
                                protocol::encode(ThreadStreamItem::Event(Box::new(event)))
                                    .map_err(|error| error.to_string())
                            }
                            StreamTarget::Thread(_) => continue,
                        };
                        match frame {
                            Ok(frame) => frames.push(frame),
                            Err(error) => {
                                tracing::error!(operation="orchestration.subscription",message=%error);
                                return;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        match subscription_frames(&store, &target, Some(cursor), false) {
                            Ok((replay, new_receiver, sequence)) => {
                                frames = replay;
                                receiver = new_receiver;
                                cursor = sequence;
                            }
                            Err(error) => {
                                tracing::error!(operation="orchestration.subscription.resync",message=%error);
                                return;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(HostReply {
            initial,
            updates: Some(HostSubscription {
                receiver: receiver_frames,
                _task: tokio_util::task::AbortOnDropHandle::new(task),
            }),
        })
    }
}
type SubscriptionFrames = (
    Vec<Vec<u8>>,
    tokio::sync::broadcast::Receiver<StoredEvent>,
    u64,
);
fn subscription_frames(
    store: &Store,
    target: &StreamTarget,
    after: Option<u64>,
    first_response: bool,
) -> Result<SubscriptionFrames, Failure> {
    match target {
        StreamTarget::Shell => {
            let subscription = store.subscribe_shell(after).map_err(store_failure)?;
            let frames = subscription
                .initial
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    if first_response && index == 0 {
                        protocol::encode(Response::Success { result: item })
                    } else {
                        protocol::encode(item)
                    }
                })
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|error| Failure::new("encode_failed", error))?;
            Ok((frames, subscription.receiver, subscription.cursor))
        }
        StreamTarget::Thread(id) => {
            let subscription = store.subscribe_thread(id, after).map_err(store_failure)?;
            let frames = subscription
                .initial
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    if first_response && index == 0 {
                        protocol::encode(Response::Success { result: item })
                    } else {
                        protocol::encode(item)
                    }
                })
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|error| Failure::new("encode_failed", error))?;
            Ok((frames, subscription.receiver, subscription.cursor))
        }
    }
}
fn store_failure(error: orchestration::store::StoreError) -> Failure {
    Failure::new("orchestration_failed", error)
}
fn provider_key(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
    .into()
}
fn driver(instance: &ProviderInstanceId) -> Result<Driver, Failure> {
    match instance.as_str() {
        "codex" => Ok(Driver::Codex),
        "claude" => Ok(Driver::Claude),
        _ => Err(Failure::new(
            "provider_unavailable",
            "unknown provider instance",
        )),
    }
}
fn thread_cwd(
    thread: &AppThread,
    projects: &[agent_protocol::models::Project],
) -> Result<PathBuf, Failure> {
    if let Some(path) = &thread.worktree_path {
        return Ok(PathBuf::from(path));
    }
    projects
        .iter()
        .find(|project| project.id == thread.project_id.as_str())
        .and_then(|project| project.roots.first())
        .map(|root| PathBuf::from(&root.path))
        .ok_or_else(|| Failure::new("workspace_unavailable", "project root missing"))
}
#[async_trait::async_trait]
impl ProviderAdapter for HostResources {
    async fn execute(
        &self,
        effect: &Effect,
        projection: ThreadProjection,
    ) -> Result<Vec<DomainEvent>, AdapterError> {
        if matches!(effect.body, EffectBody::TerminalCleanup) {
            self.terminals
                .cleanup_handle(&format!("terminal:{}", effect.thread_id))
                .await;
            return Ok(vec![]);
        }
        if matches!(effect.body, EffectBody::AttachmentCleanup) {
            return Ok(vec![]);
        }
        let driver = match &effect.body {
            EffectBody::Detach { driver, .. } => *driver,
            EffectBody::Respond { request_id, .. } => {
                let request = projection
                    .runtime_requests
                    .iter()
                    .find(|request| request.id == *request_id)
                    .ok_or_else(|| adapter_error("request missing"))?;
                let ResponseCapability::Live {
                    provider_session_id,
                } = &request.response_capability
                else {
                    return Err(adapter_error("request callback is no longer live"));
                };
                projection
                    .provider_sessions
                    .iter()
                    .find(|session| session.id == *provider_session_id)
                    .ok_or_else(|| adapter_error("provider session missing"))?
                    .driver
            }
            _ => driver(
                &projection
                    .runs
                    .iter()
                    .find(|run| Some(&run.id) == effect.body.run_id())
                    .ok_or_else(|| adapter_error("run missing"))?
                    .provider_instance_id,
            )
            .map_err(adapter_error)?,
        };
        match &effect.body {
            EffectBody::Detach {
                provider_session_id,
                ..
            } => {
                match driver {
                    Driver::Codex => {
                        self.codex_adapter
                            .as_ref()
                            .ok_or_else(|| adapter_error("Codex unavailable"))?
                            .detach(provider_session_id)
                            .await?
                    }
                    Driver::Claude => {
                        self.claude
                            .get()
                            .ok_or_else(|| adapter_error("Claude unavailable"))?
                            .adapter
                            .detach(provider_session_id)
                            .await?
                    }
                };
                return Ok(vec![]);
            }
            EffectBody::Respond {
                request_id,
                decision,
                answers,
            } => {
                match driver {
                    Driver::Codex => {
                        self.codex_adapter
                            .as_ref()
                            .ok_or_else(|| adapter_error("Codex unavailable"))?
                            .respond(request_id, *decision, answers.as_ref())
                            .await?
                    }
                    Driver::Claude => {
                        self.claude
                            .get()
                            .ok_or_else(|| adapter_error("Claude unavailable"))?
                            .adapter
                            .respond(request_id, *decision, answers.as_ref())
                            .await?
                    }
                };
                return Ok(vec![]);
            }
            _ => {}
        }
        let cwd = if projection.thread.project_id.as_str() == "bex:chats" {
            self.projects.chat_directory()
        } else {
            let projects = self.projects.load().await.map_err(adapter_error)?.projects;
            thread_cwd(&projection.thread, &projects).map_err(adapter_error)?
        };
        crate::platform::create_state_directory(&self.projects.chat_directory())
            .map_err(adapter_error)?;
        self.worktrees
            .ensure_available(&cwd.to_string_lossy())
            .await
            .map_err(adapter_error)?;
        let browser = self
            .browser
            .get()
            .map(|browser| browser.provider_config(effect.thread_id.as_str()))
            .transpose()
            .map_err(adapter_error)?;
        match driver {
            Driver::Codex => {
                self.codex.availability().map_err(adapter_error)?;
                self.codex_adapter
                    .as_ref()
                    .ok_or_else(|| adapter_error("Codex unavailable"))?
                    .execute(&effect.body, &projection, &cwd, browser)
                    .await?;
            }
            Driver::Claude => {
                let claude = self
                    .claude
                    .get()
                    .ok_or_else(|| adapter_error("Claude unavailable"))?;
                let home = claude.credentials_home().await.map_err(adapter_error)?;
                claude
                    .adapter
                    .execute(&effect.body, &projection, &cwd, browser, &home)
                    .await?;
            }
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn create() -> Command {
        Command {
            command_id: CommandId::new("create").unwrap(),
            thread_id: ThreadId::new("thread").unwrap(),
            body: CommandBody::ThreadCreate {
                created_by: CreatedBy::User,
                creation_source: CreationSource::Desktop,
                project_id: ProjectId::new("bex:chats").unwrap(),
                title: "Conversation".into(),
                model_selection: ModelSelection {
                    instance_id: ProviderInstanceId::new("codex").unwrap(),
                    model: "test-model".into(),
                    options: Default::default(),
                },
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                branch: None,
                worktree_path: None,
            },
        }
    }
    #[tokio::test]
    async fn binary_rpc_subscription_reconnect_and_command_receipts_use_v2_store() {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("fixture provider unavailable".into()),
            ProjectStore::new(directory.path().join("bex-worktrees.json")),
        )
        .unwrap();
        // Own just delivery in this unit test; no live provider or native-home scanner is started.
        let session = service.inner.connections.open_session();
        let mut shell = service
            .dispatch(
                session.id(),
                &Call::SubscribeShell(agent_protocol::orchestration::SubscribeShell {
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            protocol::decode::<Response<ShellStreamItem>>(&shell.initial).unwrap(),
            Response::Success {
                result: ShellStreamItem::Snapshot(_)
            }
        ));
        assert!(matches!(
            protocol::decode::<ShellStreamItem>(
                &shell.updates.as_mut().unwrap().recv().await.unwrap()
            )
            .unwrap(),
            ShellStreamItem::Synchronized
        ));
        let command = create();
        let reply = service
            .dispatch(session.id(), &Call::DispatchCommand(command.clone()))
            .await
            .unwrap();
        let Response::Success { result: receipt } = protocol::decode::<
            Response<agent_protocol::orchestration::DispatchReceipt>,
        >(&reply.initial)
        .unwrap() else {
            panic!("creation failed")
        };
        assert!(!receipt.replayed);
        let update = shell.updates.as_mut().unwrap().recv().await.unwrap();
        assert!(matches!(
            protocol::decode::<ShellStreamItem>(&update).unwrap(),
            ShellStreamItem::ThreadUpdated { .. }
        ));
        let repeat = service
            .dispatch(session.id(), &Call::DispatchCommand(command.clone()))
            .await
            .unwrap();
        assert!(
            matches!(protocol::decode::<Response<agent_protocol::orchestration::DispatchReceipt>>(&repeat.initial).unwrap(),Response::Success{result} if result.replayed)
        );
        let mut thread = service
            .dispatch(
                session.id(),
                &Call::SubscribeThread(agent_protocol::orchestration::SubscribeThread {
                    thread_id: command.thread_id.clone(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            protocol::decode::<Response<ThreadStreamItem>>(&thread.initial).unwrap(),
            Response::Success {
                result: ThreadStreamItem::Snapshot { .. }
            }
        ));
        assert!(matches!(
            protocol::decode::<ThreadStreamItem>(
                &thread.updates.as_mut().unwrap().recv().await.unwrap()
            )
            .unwrap(),
            ThreadStreamItem::Synchronized
        ));
        let pin = Command {
            command_id: CommandId::new("pin").unwrap(),
            thread_id: command.thread_id.clone(),
            body: CommandBody::ThreadPin { order_key: None },
        };
        service
            .dispatch(session.id(), &Call::DispatchCommand(pin))
            .await
            .unwrap();
        assert!(matches!(
            protocol::decode::<ThreadStreamItem>(
                &thread.updates.as_mut().unwrap().recv().await.unwrap()
            )
            .unwrap(),
            ThreadStreamItem::Event(_)
        ));
        service.close_session(session.id());
        assert!(thread.updates.as_mut().unwrap().recv().await.is_none());
        let reconnect = service.inner.connections.open_session();
        let replay = service
            .dispatch(
                reconnect.id(),
                &Call::SubscribeThread(agent_protocol::orchestration::SubscribeThread {
                    thread_id: command.thread_id.clone(),
                    after_sequence: Some(receipt.sequence),
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            protocol::decode::<Response<ThreadStreamItem>>(&replay.initial).unwrap(),
            Response::Success {
                result: ThreadStreamItem::Event(_)
            }
        ));
        drop(replay);
        drop(thread);
        drop(shell);
        drop(service);
        let reopened = Store::open(directory.path().join("orchestration-v2.sqlite")).unwrap();
        assert!(
            reopened
                .projection(&command.thread_id)
                .unwrap()
                .thread
                .pinned_at
                .is_some()
        );
    }
}
