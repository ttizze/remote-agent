//! Host owns persistence and effects; authenticated connections own only delivery.
#[path = "agent_tools.rs"]
pub mod agent_tools;
use super::{
    connections::{Connections, HostReply, HostSession, HostSubscription, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::ProjectStore;
use agent_protocol::{
    operations as op,
    protocol::{self, Body, Call, Response},
    provider::ProviderKind,
};
use agent_transport::peer::RpcMessageError;
use codex_app_server::CodexAppServer;
use orchestration::{AdapterError, ProviderAdapter, store::Store, worker::EffectWorker, *};
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
        turn_completed: false,
    }
}
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) delivery: agent_protocol::error::Delivery,
}
impl From<Failure> for agent_protocol::error::RpcFailure {
    fn from(error: Failure) -> Self {
        Self {
            code: error.code.into(),
            message: error.message,
            delivery: error.delivery,
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
    launches: Mutex<HashMap<CommandId, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    stop: tokio::sync::watch::Sender<bool>,
}
struct HostResources {
    orchestration: Arc<Store>,
    codex: Arc<CodexResources>,
    codex_adapter: Option<Arc<provider_adapters::codex::CodexAdapter>>,
    claude: OnceLock<Arc<ClaudeResources>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    agent_tools: OnceLock<agent_tools::AgentTools>,
    projects: ProjectStore,
    checkpoints: crate::checkpoints::Checkpoints,
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
            orchestration: store.clone(),
            codex_adapter,
            codex: Arc::new(CodexResources::new(codex.clone())),
            claude: OnceLock::new(),
            startup_errors: Default::default(),
            browser: OnceLock::new(),
            agent_tools: OnceLock::new(),
            files: crate::workspace_files::WorkspaceFiles::new(
                projects.path().with_file_name("bex-attachments"),
            ),
            worktrees: crate::worktrees::Worktrees::new(projects.path()),
            projects,
            checkpoints: Default::default(),
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
                launches: Mutex::new(HashMap::new()),
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
        match agent_tools::AgentTools::start(self) {
            Ok(tools) => {
                let _ = self.inner.resources.agent_tools.set(tools);
            }
            Err(error) => tracing::error!(operation = "orchestration.mcp.start", message = %error),
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
                    batch=receiver.recv()=>{let Some(batch)=batch else{break;};let result=if let Some(owner)=&batch.native_owner {store.ingest_native(batch.events,&batch.thread_id,&batch.run_id,&batch.attempt_id,owner,&batch.occurred_at)}else if let Some(offer)=&batch.native_continuation_offer { store.offer_native_continuation(&batch.thread_id,offer,&batch.occurred_at) }else{store.ingest(batch.events,Some((&batch.run_id,Some(&batch.attempt_id))),&batch.occurred_at)};let accepted=match result{Ok(commit)=>!commit.events.is_empty(),Err(error)=>{tracing::error!(operation="orchestration.provider.ingest",message=%error);false}};if let Some(receipt)=batch.acknowledged{let _=receipt.send(accepted);}}
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
            CommandBody::DelegatedTaskRequest(request) => {
                request.model_selection.instance_id.clone()
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
                &orchestration::capabilities::capabilities(driver).turns,
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
            Call::StartTerminal(_)
                | Call::WriteFile(_)
                | Call::Upload(_)
                | Call::ReviewWorkspace(_)
        ) {
            Some(self.inner.resources.worktree_access.read().await)
        } else {
            None
        };
        let response = match request {
            Call::DispatchCommand(command) => {
                if let CommandBody::CheckpointRollback {
                    scope_id,
                    restore_files: true,
                    ..
                } = &command.body
                {
                    let projection = self
                        .inner
                        .store
                        .projection(&command.thread_id)
                        .map_err(store_failure)?;
                    let scope = projection
                        .checkpoint_scopes
                        .iter()
                        .find(|s| s.id == *scope_id)
                        .ok_or_else(|| Failure::new("checkpoint_unavailable", "scope missing"))?;
                    self.inner
                        .resources
                        .ensure_restore_isolated(&projection.thread, &scope.cwd)
                        .await?;
                }
                if let CommandBody::ThreadCreate { project_id, .. } = &command.body
                    && !self
                        .projects()
                        .await?
                        .iter()
                        .any(|project| project.id == project_id.as_str())
                {
                    return Err(Failure::new(
                        "project_unavailable",
                        "project is not registered",
                    ));
                }
                let _workspace = if matches!(command.body, CommandBody::ThreadCreate { .. }) {
                    Some(self.inner.resources.worktree_access.write().await)
                } else {
                    None
                };
                self.dispatch_command(command)?.into()
            }
            Call::LaunchThread(params) => {
                let service = self.clone();
                let params = params.clone();
                tokio::spawn(async move { service.launch_thread(&params).await })
                    .await
                    .map_err(|e| Failure::new("launch_failed", e))??
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
            Call::GetTurnDiff(params) => {
                if params.from_turn_count > params.to_turn_count {
                    return Err(Failure::new("invalid_query", "turn range is reversed"));
                }
                let projection = self
                    .inner
                    .store
                    .projection(&params.thread_id)
                    .map_err(store_failure)?;
                let scope = projection
                    .checkpoint_scopes
                    .iter()
                    .find(|scope| scope.kind == ScopeKind::RootRun)
                    .ok_or_else(|| {
                        Failure::new("checkpoint_unavailable", "root checkpoint scope missing")
                    })?;
                let checkpoint = |ordinal| {
                    projection
                        .checkpoints
                        .iter()
                        .find(|checkpoint| {
                            checkpoint.scope_id == scope.id
                                && checkpoint.app_run_ordinal == Some(ordinal)
                        })
                        .ok_or_else(|| {
                            Failure::new("checkpoint_unavailable", "turn checkpoint missing")
                        })
                };
                let to = checkpoint(params.to_turn_count)?;
                let from = to
                    .parent_checkpoint_id
                    .as_ref()
                    .and_then(|id| {
                        projection.checkpoints.iter().find(|c| {
                            &c.id == id
                                && c.status == CheckpointStatus::Ready
                                && c.app_run_ordinal == Some(params.from_turn_count)
                        })
                    })
                    .unwrap_or(checkpoint(params.from_turn_count)?);
                let diff = self
                    .inner
                    .resources
                    .checkpoints
                    .diff(&scope.cwd, from, to, params.ignore_whitespace)
                    .await
                    .map_err(|error| Failure::new("checkpoint_unavailable", error))?;
                agent_protocol::orchestration::TurnDiff {
                    thread_id: params.thread_id.clone(),
                    from_turn_count: params.from_turn_count,
                    to_turn_count: params.to_turn_count,
                    diff,
                }
                .into()
            }
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

    fn launch_lock(&self, id: &CommandId) -> Arc<tokio::sync::Mutex<()>> {
        let mut launches = self
            .inner
            .launches
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        launches.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = launches.get(id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        launches.insert(id.clone(), Arc::downgrade(&lock));
        lock
    }
    async fn launch_thread(
        &self,
        params: &agent_protocol::orchestration::LaunchThread,
    ) -> Result<Body, Failure> {
        let _launch = self
            .launch_lock(&params.create.command_id)
            .lock_owned()
            .await;
        if !matches!(params.create.body, CommandBody::ThreadCreate { .. }) {
            return Err(Failure::new(
                "invalid_launch",
                "launch requires thread.create",
            ));
        }
        if let CommandBody::ThreadCreate { project_id, .. } = &params.create.body
            && !self
                .projects()
                .await?
                .iter()
                .any(|project| project.id == project_id.as_str())
        {
            return Err(Failure::new(
                "project_unavailable",
                "project is not registered",
            ));
        }
        let mut create = params.create.clone();
        let selection_driver = if let CommandBody::ThreadCreate {
            model_selection, ..
        } = &create.body
        {
            driver(&model_selection.instance_id)?
        } else {
            unreachable!()
        };
        let capabilities = orchestration::capabilities::capabilities(selection_driver).turns;
        let preview =
            orchestration::decider::decide(&create, None, &now(), &capabilities, selection_driver)
                .map_err(|e| Failure::new("invalid_launch", e))?;
        let projection = preview.events.iter().fold(None, |p, e| {
            orchestration::projector::apply(p.as_ref(), e, Default::default())
        });
        let input_command = Command {
            command_id: CommandId::new(format!("{}:input", create.command_id)).expect("derived id"),
            thread_id: create.thread_id.clone(),
            body: CommandBody::MessageDispatch(params.input.clone().into()),
        };
        orchestration::decider::decide(
            &input_command,
            projection.as_ref(),
            &now(),
            &capabilities,
            selection_driver,
        )
        .map_err(|e| Failure::new("invalid_launch", e))?;
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
                .ok_or_else(|| Failure::new("project_unavailable", "project root missing"))?;
            *worktree_path = self
                .inner
                .resources
                .worktrees
                .prepare(Some(&root.path))
                .await
                .map_err(|error| Failure::new("workspace_preparation_failed", error))?
                .map(|path| path.to_string_lossy().into_owned());
        }
        let _workspace = self.inner.resources.worktree_access.write().await;
        self.dispatch_command(&create)?;
        self.dispatch_command(&input_command)
            .map_err(|mut failure| {
                failure.delivery = agent_protocol::error::Delivery::Unknown;
                failure
            })
            .map(Into::into)
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
            let Ok(cwd) = thread_cwd(&shell.thread, &projects) else {
                continue;
            };
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
            match subscription_frames(&self.inner.store, &target, after, true) {
                Ok(frames) => frames,
                Err(error) => {
                    return Ok(Response::<Body>::Failure {
                        error: error.into(),
                    }
                    .into());
                }
            };
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
            ensure_subscription_frames(&frames)?;
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
            ensure_subscription_frames(&frames)?;
            Ok((frames, subscription.receiver, subscription.cursor))
        }
    }
}
fn ensure_subscription_frames(frames: &[Vec<u8>]) -> Result<(), Failure> {
    if frames
        .iter()
        .any(|frame| frame.len() > protocol::MAX_FRAME_BYTES)
    {
        return Err(Failure::new(
            "response_too_large",
            "conversation snapshot exceeds the frame limit",
        ));
    }
    Ok(())
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
    orchestration::capabilities::driver(instance)
        .ok_or_else(|| Failure::new("provider_unavailable", "unknown provider instance"))
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
impl HostResources {
    async fn ensure_restore_isolated(&self, thread: &AppThread, cwd: &str) -> Result<(), Failure> {
        let fail = || {
            Failure::new(
                "shared_workspace",
                "File restore requires an isolated worktree. Rewind the conversation without restoring files instead.",
            )
        };
        let Some(worktree) = &thread.worktree_path else {
            return Err(fail());
        };
        let cwd = dunce::canonicalize(cwd).map_err(|_| fail())?;
        let worktree = dunce::canonicalize(worktree).map_err(|_| fail())?;
        let checkout = crate::checkpoints::checkout_root(&worktree)
            .await
            .map_err(|_| fail())?;
        if !cwd.starts_with(&worktree)
            || checkout != worktree
            || crate::checkpoints::checkout_root(&cwd)
                .await
                .map_err(|_| fail())?
                != checkout
        {
            return Err(fail());
        }
        let shell = self.orchestration.shell_snapshot().map_err(store_failure)?;
        let projects = self
            .projects
            .load()
            .await
            .map_err(|e| Failure::new("workspace_unavailable", e))?
            .projects;
        for root in projects.iter().flat_map(|project| &project.roots) {
            if crate::checkpoints::checkout_root(Path::new(&root.path))
                .await
                .is_ok_and(|root| root == checkout)
            {
                return Err(fail());
            }
        }
        for other in shell
            .threads
            .iter()
            .chain(&shell.archived_threads)
            .filter(|t| t.thread.id != thread.id)
        {
            let other = self
                .orchestration
                .projection(&other.thread.id)
                .map_err(store_failure)?;
            let mut paths = other
                .checkpoint_scopes
                .iter()
                .map(|s| PathBuf::from(&s.cwd))
                .collect::<Vec<_>>();
            paths.push(if other.thread.project_id.as_str() == "bex:chats" {
                self.projects.chat_directory()
            } else {
                thread_cwd(&other.thread, &projects)?
            });
            paths.extend(
                other
                    .provider_sessions
                    .iter()
                    .filter(|s| {
                        s.status != SessionStatus::Stopped
                            && !s
                                .capabilities
                                .sessions
                                .supports_multiple_provider_threads_per_session
                    })
                    .map(|s| PathBuf::from(&s.cwd)),
            );
            for path in paths {
                match dunce::canonicalize(path) {
                    Ok(path) => {
                        let overlaps = match crate::checkpoints::checkout_root(&path).await {
                            Ok(root) => root == checkout,
                            Err(_) => path.starts_with(&checkout) || checkout.starts_with(&path),
                        };
                        if overlaps {
                            return Err(fail());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(fail()),
                }
            }
        }
        Ok(())
    }
}
impl HostResources {
    async fn resolve_context(
        &self,
        mut projection: ThreadProjection,
        run_id: &RunId,
        cwd: &Path,
    ) -> Result<ThreadProjection, AdapterError> {
        let run = projection
            .runs
            .iter()
            .find(|r| r.id == *run_id)
            .ok_or_else(|| adapter_error("run missing"))?
            .clone();
        let provider = projection
            .provider_threads
            .iter()
            .find(|p| Some(&p.id) == run.provider_thread_id.as_ref())
            .ok_or_else(|| adapter_error("provider missing"))?
            .clone();
        if !run.status.is_blocking()
            || projection
                .messages
                .iter()
                .find(|m| m.id == run.user_message_id)
                .is_some_and(|m| {
                    orchestration::native_maintenance(&m.text, !m.attachments.is_empty())
                })
        {
            return Ok(projection);
        }
        let retry = orchestration::context::retry_unconsumed(
            &projection.context_transfers,
            &projection.runs,
            &projection.context_handoffs,
            &now(),
        );
        if !retry.is_empty() {
            self.orchestration
                .ingest(
                    orchestration::events(
                        &projection.thread.id,
                        &format!("context:{}:retry", run.id),
                        retry,
                        &now(),
                    ),
                    Some((&run.id, run.active_attempt_id.as_ref())),
                    &now(),
                )
                .map_err(adapter_error)?;
            projection = self
                .orchestration
                .projection(&projection.thread.id)
                .map_err(adapter_error)?;
        }
        let pending: Vec<_> = projection
            .context_transfers
            .iter()
            .filter(|t| t.status == TransferStatus::Pending)
            .cloned()
            .collect();
        let mut payloads = vec![];
        for transfer in &pending {
            let source = self
                .orchestration
                .projection(&transfer.source_thread_id)
                .map_err(adapter_error)?;
            let ordinal = transfer
                .source_point
                .run_id
                .as_ref()
                .and_then(|id| source.runs.iter().find(|r| &r.id == id))
                .map_or(0, |r| r.ordinal);
            let native = transfer.kind == TransferKind::Fork
                && provider.native_thread_ref.is_none()
                && transfer.source_provider_instance_id.as_ref() == Some(&run.provider_instance_id)
                && transfer
                    .source_point
                    .provider_thread_ref
                    .as_ref()
                    .is_some_and(|r| r.strength == Strength::Strong && r.native_id.is_some())
                && transfer
                    .source_point
                    .provider_turn_ref
                    .as_ref()
                    .is_some_and(|r| r.native_id.is_some());
            let reference = if native {
                match provider.driver {
                    Driver::Codex => self
                        .codex_adapter
                        .as_ref()
                        .ok_or_else(|| adapter_error("Codex unavailable"))?
                        .fork(&transfer.source_point, cwd, &run.model_selection.model)
                        .await
                        .ok(),
                    Driver::Claude => Some(ProviderRef {
                        driver: Driver::Claude,
                        native_id: Some(uuid::Uuid::new_v4().to_string()),
                        strength: Strength::Strong,
                        fingerprint: None,
                        ordinal: None,
                    }),
                }
            } else {
                None
            };
            if let Some(reference) = reference {
                payloads.extend(orchestration::context::native(
                    transfer,
                    &run,
                    provider.clone(),
                    reference,
                    &now(),
                ));
            } else {
                let strategy = if transfer.kind == TransferKind::MergeBack {
                    HandoffStrategy::ForkDeltaSummary
                } else {
                    HandoffStrategy::FullThreadSummary
                };
                payloads.extend(orchestration::context::portable(
                    &source,
                    &projection,
                    &run,
                    Some(transfer),
                    strategy,
                    if transfer.kind == TransferKind::MergeBack {
                        orchestration::context::merge_from(
                            &source.runs,
                            &projection.context_transfers,
                            &source.thread.id,
                        )
                    } else {
                        1
                    },
                    ordinal,
                    &now(),
                ));
            }
        }
        let lost_native_context = provider.native_thread_ref.is_none()
            && projection.visible_turn_items.iter().any(|row| {
                row.visibility == Visibility::Inherited
                    || matches!(&row.item.body, TurnItemBody::AssistantMessage { .. })
                    || row.item.run_id.is_none()
                        && matches!(&row.item.body, TurnItemBody::UserMessage { .. })
            });
        if pending.is_empty()
            && !projection
                .context_handoffs
                .iter()
                .any(|h| h.target_run_id == run.id)
            && let Some((from, to, strategy)) = orchestration::context::missed_provider_context(
                &projection.runs,
                run.ordinal,
                &provider.id,
                provider.last_run_ordinal.unwrap_or(0),
                lost_native_context,
            )
        {
            payloads.extend(orchestration::context::portable(
                &projection,
                &projection,
                &run,
                None,
                strategy,
                from,
                to,
                &now(),
            ));
        }
        if !payloads.is_empty() {
            let events = orchestration::events(
                &projection.thread.id,
                &format!("context:{}", run.id),
                payloads,
                &now(),
            );
            let commit = self
                .orchestration
                .ingest(
                    events,
                    Some((&run.id, run.active_attempt_id.as_ref())),
                    &now(),
                )
                .map_err(adapter_error)?;
            if commit.events.is_empty() {
                return Err(adapter_error("context transfer superseded"));
            }
            projection = self
                .orchestration
                .projection(&projection.thread.id)
                .map_err(adapter_error)?;
        }
        Ok(projection)
    }
}
#[async_trait::async_trait]
impl ProviderAdapter for HostResources {
    async fn cancel_start(
        &self,
        run_id: &RunId,
        projection: &ThreadProjection,
    ) -> Result<(), AdapterError> {
        let run = projection
            .runs
            .iter()
            .find(|run| run.id == *run_id)
            .ok_or_else(|| adapter_error("run missing"))?;
        match driver(&run.provider_instance_id).map_err(adapter_error)? {
            Driver::Codex => {
                if let Some(adapter) = &self.codex_adapter {
                    adapter.cancel_start(run_id).await?;
                }
            }
            Driver::Claude => {
                if let Some(claude) = self.claude.get() {
                    claude.adapter.cancel_start(run_id).await?;
                }
            }
        }
        Ok(())
    }
    async fn execute(
        &self,
        effect: &Effect,
        mut projection: ThreadProjection,
    ) -> Result<Vec<DomainEvent>, AdapterError> {
        if let EffectBody::Rollback {
            request_id,
            provider_thread_id,
            checkpoint_id,
            scope_id,
            restore_files,
        } = &effect.body
        {
            let _workspace = self.worktree_access.read().await;
            if projection.thread.rollback_request_id.as_ref() != Some(request_id) {
                return Ok(vec![]);
            }
            let (scope, checkpoint, provider, _) =
                orchestration::rollback::target(&projection, scope_id, checkpoint_id)
                    .map_err(adapter_error)?;
            if provider.id != *provider_thread_id {
                return Err(adapter_error("active provider changed before rollback"));
            }
            if *restore_files {
                self.ensure_restore_isolated(&projection.thread, &scope.cwd)
                    .await
                    .map_err(adapter_error)?;
            }
            let cwd = PathBuf::from(&scope.cwd);
            let files = if *restore_files {
                Some(
                    self.checkpoints
                        .prepare_restore(scope, checkpoint)
                        .await
                        .map_err(adapter_error)?,
                )
            } else {
                None
            };
            let provider = match provider.driver {
                Driver::Codex => {
                    self.codex_adapter
                        .as_ref()
                        .ok_or_else(|| adapter_error("Codex unavailable"))?
                        .rollback(&projection, scope_id, checkpoint_id, &cwd)
                        .await?
                }
                Driver::Claude => {
                    self.claude
                        .get()
                        .ok_or_else(|| adapter_error("Claude unavailable"))?
                        .adapter
                        .rollback(&projection, scope_id, checkpoint_id)
                        .await?
                }
            };
            if let Some(files) = files {
                files.commit().map_err(|error| AdapterError {
                    message: error.to_string(),
                    retryable: true,
                    turn_completed: false,
                })?;
            }
            let stale = orchestration::rollback::stale_checkpoints(&projection, checkpoint);
            self.checkpoints
                .delete_stale_refs(scope, &stale)
                .await
                .map_err(|error| AdapterError {
                    message: error.to_string(),
                    retryable: true,
                    turn_completed: false,
                })?;
            return Ok(orchestration::rollback::finish(
                &projection,
                checkpoint,
                provider,
                request_id,
                &now(),
            ));
        }
        if let EffectBody::Start { run_id } = &effect.body {
            let run = projection
                .runs
                .iter()
                .find(|r| r.id == *run_id)
                .ok_or_else(|| adapter_error("run missing"))?;
            if !run.status.is_blocking() {
                return Ok(vec![]);
            }
            let logout = projection
                .messages
                .iter()
                .find(|m| m.id == run.user_message_id)
                .is_some_and(|m| {
                    m.text.trim().eq_ignore_ascii_case("/logout") && m.attachments.is_empty()
                });
            if logout {
                let native = projection
                    .runs
                    .iter()
                    .filter(|r| r.ordinal < run.ordinal)
                    .max_by_key(|r| r.ordinal)
                    .and_then(|r| {
                        projection
                            .provider_threads
                            .iter()
                            .find(|p| Some(&p.id) == r.provider_thread_id.as_ref())
                    });
                let auth_driver = native.map_or(
                    driver(&run.provider_instance_id).map_err(adapter_error)?,
                    |p| p.driver,
                );
                let (kind, identity): (ProviderKind, Arc<dyn Identity>) = match auth_driver {
                    Driver::Codex => (ProviderKind::Codex, self.codex.clone()),
                    Driver::Claude => (
                        ProviderKind::Claude,
                        self.claude
                            .get()
                            .ok_or_else(|| adapter_error("Claude unavailable"))?
                            .clone(),
                    ),
                };
                let accounts = identity.list().await.map_err(adapter_error)?;
                if let Some(id) = accounts.selected.get(&kind) {
                    identity
                        .account(super::identity::AccountCommand::Logout { id: id.clone() })
                        .await
                        .map_err(adapter_error)?;
                }
                let timestamp = now();
                let mut finished = run.clone();
                finished.status = RunStatus::Completed;
                finished.started_at = Some(timestamp.clone());
                finished.completed_at = Some(timestamp.clone());
                let mut payloads = vec![EventPayload::RunUpdated(finished)];
                if let Some(attempt) = projection
                    .attempts
                    .iter()
                    .find(|a| Some(&a.id) == run.active_attempt_id.as_ref())
                {
                    let mut attempt = attempt.clone();
                    attempt.status = AttemptStatus::Completed;
                    attempt.started_at = Some(timestamp.clone());
                    attempt.completed_at = Some(timestamp.clone());
                    payloads.push(EventPayload::RunAttemptUpdated(attempt));
                }
                if let Some(node) = projection
                    .nodes
                    .iter()
                    .find(|n| Some(&n.id) == run.root_node_id.as_ref())
                {
                    let mut node = node.clone();
                    node.status = NodeStatus::Completed;
                    node.completed_at = Some(timestamp.clone());
                    payloads.push(EventPayload::NodeUpdated(node));
                }
                if let Some(item) = projection
                    .turn_items
                    .iter()
                    .find(|i| i.run_id.as_ref() == Some(&run.id))
                {
                    let mut item = item.clone();
                    item.id =
                        TurnItemId::new(format!("item:{}:sign-out", run.id)).expect("derived id");
                    item.ordinal = 0;
                    item.title = Some("Provider signed out".into());
                    item.body = TurnItemBody::CommandExecution {
                        input: "/logout".into(),
                        output: Some("Provider signed out".into()),
                        output_omitted: false,
                        output_indicates_failure: false,
                        exit_code: Some(0),
                    };
                    payloads.push(EventPayload::TurnItemUpdated(item));
                }
                return Ok(orchestration::events(
                    &projection.thread.id,
                    &format!("{}:sign-out", effect.id),
                    payloads,
                    &timestamp,
                ));
            }
            let compact = projection
                .messages
                .iter()
                .find(|m| m.id == run.user_message_id)
                .is_some_and(|m| {
                    m.text.trim().eq_ignore_ascii_case("/compact") && m.attachments.is_empty()
                });
            if compact && !projection.visible_turn_items.iter().any(|row|matches!(&row.item.body,TurnItemBody::AssistantMessage{..}) || matches!(&row.item.body,TurnItemBody::UserMessage{message_id,..} if *message_id!=run.user_message_id)) {
                return Err(adapter_error("Start a conversation before compacting this thread."));
            }
        }
        if let EffectBody::CaptureCheckpoint { run_id } = &effect.body {
            let run = projection
                .runs
                .iter()
                .find(|run| run.id == *run_id)
                .ok_or_else(|| adapter_error("run missing"))?;
            if run.status == RunStatus::RolledBack || run.checkpoint_id.is_some() {
                return Ok(vec![]);
            }
            let node_id = run
                .root_node_id
                .as_ref()
                .ok_or_else(|| adapter_error("checkpoint root missing"))?;
            let scope = projection
                .checkpoint_scopes
                .iter()
                .find(|scope| scope.kind == ScopeKind::RootRun)
                .ok_or_else(|| adapter_error("checkpoint scope missing"))?;
            let checkpoint = self
                .checkpoints
                .capture(scope, run_id, node_id, run.ordinal, &now())
                .await;
            return Ok(orchestration::events(
                &effect.thread_id,
                &effect.id,
                vec![EventPayload::CheckpointCaptured(checkpoint)],
                &now(),
            ));
        }
        if matches!(effect.body, EffectBody::TerminalCleanup) {
            let projects = self.projects.load().await.map_err(adapter_error)?.projects;
            let cwd = if projection.thread.project_id.as_str() == "bex:chats" {
                self.projects.chat_directory()
            } else {
                thread_cwd(&projection.thread, &projects).map_err(adapter_error)?
            };
            let shell = self.orchestration.shell_snapshot().map_err(adapter_error)?;
            let shared = shell
                .threads
                .iter()
                .chain(&shell.archived_threads)
                .filter(|s| s.thread.id != effect.thread_id)
                .any(|s| {
                    let other = if s.thread.project_id.as_str() == "bex:chats" {
                        Some(self.projects.chat_directory())
                    } else {
                        thread_cwd(&s.thread, &projects).ok()
                    };
                    other.is_some_and(|other| other == cwd)
                });
            if !shared {
                self.terminals
                    .cleanup_handle(&agent_protocol::operations::terminal_handle(
                        &cwd.to_string_lossy(),
                    ))
                    .await;
            }
            return Ok(vec![]);
        }
        if matches!(effect.body, EffectBody::AttachmentCleanup) {
            self.files
                .cleanup_thread_attachments(effect.thread_id.as_str())
                .await
                .map_err(adapter_error)?;
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
        if matches!(
            effect.body,
            EffectBody::Interrupt { .. } | EffectBody::Steer { .. }
        ) {
            match driver {
                Driver::Codex => {
                    self.codex_adapter
                        .as_ref()
                        .ok_or_else(|| adapter_error("Codex unavailable"))?
                        .execute(&effect.body, &projection, Path::new(""), None)
                        .await?
                }
                Driver::Claude => {
                    self.claude
                        .get()
                        .ok_or_else(|| adapter_error("Claude unavailable"))?
                        .adapter
                        .execute(
                            &effect.body,
                            &projection,
                            Path::new(""),
                            None,
                            Path::new(""),
                        )
                        .await?
                }
            }
            return Ok(vec![]);
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
        let mut servers = serde_json::Map::new();
        servers.insert(
            "bex_orchestration".into(),
            self.agent_tools
                .get()
                .ok_or_else(|| adapter_error("Orchestration tools unavailable"))?
                .provider_config(
                    &effect.thread_id,
                    &ProviderInstanceId::new(match driver {
                        Driver::Codex => "codex",
                        Driver::Claude => "claude",
                    })
                    .expect("provider id"),
                )
                .map_err(adapter_error)?,
        );
        if let Some(browser) = self.browser.get() {
            servers.insert(
                "bex_browser".into(),
                browser
                    .provider_config(effect.thread_id.as_str())
                    .map_err(adapter_error)?,
            );
        }
        let tool_servers = Some(serde_json::Value::Object(servers));
        if let EffectBody::Start { run_id } | EffectBody::Restart { run_id, .. } = &effect.body {
            projection = self.resolve_context(projection, run_id, &cwd).await?;
            let run = projection
                .runs
                .iter()
                .find(|run| run.id == *run_id)
                .ok_or_else(|| adapter_error("run missing"))?;
            let maintenance = projection
                .messages
                .iter()
                .find(|m| m.id == run.user_message_id)
                .is_some_and(|m| {
                    orchestration::native_maintenance(&m.text, !m.attachments.is_empty())
                });
            if !maintenance {
                let root = run
                    .root_node_id
                    .as_ref()
                    .ok_or_else(|| adapter_error("checkpoint root missing"))?;
                let scope = CheckpointScope {
                    id: projection
                        .checkpoint_scopes
                        .iter()
                        .find(|scope| scope.kind == ScopeKind::RootRun)
                        .map(|scope| scope.id.clone())
                        .unwrap_or(
                            CheckpointScopeId::new(format!(
                                "scope:{}:{}:root",
                                self.orchestration.instance_id().map_err(adapter_error)?,
                                effect.thread_id
                            ))
                            .expect("derived id"),
                        ),
                    thread_id: effect.thread_id.clone(),
                    run_id: Some(run_id.clone()),
                    node_id: root.clone(),
                    parent_scope_id: None,
                    provider_thread_id: run.provider_thread_id.clone(),
                    kind: ScopeKind::RootRun,
                    ordinal_within_parent: 0,
                    advances_app_run_count: true,
                    cwd: cwd.to_string_lossy().into(),
                    created_at: now(),
                };
                let timestamp = now();
                let before = orchestration::checkpoint::before_run_id(&scope.id, run_id);
                if !projection
                    .checkpoints
                    .iter()
                    .any(|c| c.id == before && c.status == CheckpointStatus::Ready)
                {
                    let previous = projection
                        .runs
                        .iter()
                        .filter(|r| {
                            r.ordinal < run.ordinal
                                && r.started_at.is_some()
                                && r.status != RunStatus::RolledBack
                        })
                        .max_by_key(|r| r.ordinal)
                        .and_then(|r| r.checkpoint_id.as_ref())
                        .and_then(|id| projection.checkpoints.iter().find(|c| &c.id == id));
                    let mut payloads = self
                        .checkpoints
                        .prepare_run(&scope, &run.id, run.ordinal, previous, &timestamp)
                        .await;
                    payloads.retain(|payload| !matches!(payload, EventPayload::CheckpointCaptured(checkpoint) if projection.checkpoints.iter().any(|existing| existing.id == checkpoint.id && existing.status == CheckpointStatus::Ready)));
                    let (receipt, completed) = tokio::sync::oneshot::channel();
                    self.provider_output
                        .send(provider_adapters::ProviderBatch {
                            thread_id: effect.thread_id.clone(),
                            run_id: run_id.clone(),
                            attempt_id: run
                                .active_attempt_id
                                .clone()
                                .ok_or_else(|| adapter_error("attempt missing"))?,
                            events: orchestration::events(
                                &effect.thread_id,
                                &format!("{}:baseline", effect.id),
                                payloads,
                                &timestamp,
                            ),
                            occurred_at: timestamp,
                            acknowledged: Some(receipt),
                            native_owner: None,
                            native_continuation_offer: None,
                        })
                        .await
                        .map_err(adapter_error)?;
                    if !completed.await.map_err(adapter_error)? {
                        return Err(adapter_error("checkpoint baseline superseded"));
                    }
                }
            }
        }
        match driver {
            Driver::Codex => {
                self.codex.availability().map_err(adapter_error)?;
                self.codex_adapter
                    .as_ref()
                    .ok_or_else(|| adapter_error("Codex unavailable"))?
                    .execute(&effect.body, &projection, &cwd, tool_servers)
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
                    .execute(&effect.body, &projection, &cwd, tool_servers, &home)
                    .await?;
            }
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(text: &str) -> MessageDispatch {
        MessageDispatch {
            native_continuation: None,
            delegated_completion: None,
            source_plan_ref: None,
            created_by: CreatedBy::User,
            creation_source: CreationSource::Desktop,
            message_id: MessageId::new("input").unwrap(),
            text: text.into(),
            context: None,
            attachments: vec![],
            model_selection: None,
            delivery_intent: None,
            dispatch_mode: DispatchMode::StartImmediately,
        }
    }
    #[test]
    fn oversized_subscription_is_a_typed_failure_before_delivery() {
        let failure =
            ensure_subscription_frames(&[vec![0; protocol::MAX_FRAME_BYTES + 1]]).unwrap_err();
        assert_eq!(failure.code, "response_too_large");
        assert!(ensure_subscription_frames(&[vec![0; protocol::MAX_FRAME_BYTES]]).is_ok());
    }
    #[tokio::test]
    async fn launch_checks_registered_projects_even_with_an_explicit_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("fixture".into()),
            ProjectStore::new(directory.path().join("projects.json")),
        )
        .unwrap();
        let mut create = create();
        if let CommandBody::ThreadCreate {
            project_id,
            worktree_path,
            ..
        } = &mut create.body
        {
            *project_id = ProjectId::new("unregistered").unwrap();
            *worktree_path = Some(directory.path().to_string_lossy().into_owned());
        }
        let failure = service
            .launch_thread(&agent_protocol::orchestration::LaunchThread {
                create,
                input: input("start"),
            })
            .await
            .unwrap_err();
        assert_eq!(failure.code, "project_unavailable");
    }
    #[tokio::test]
    async fn restore_rejects_project_checkouts_and_accepts_a_separate_nested_git_worktree() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        crate::git::text(&root, &["init"]).unwrap();
        crate::git::text(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        let worktree = root.join(".worktree/isolated");
        crate::git::text(
            &root,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        let projects = ProjectStore::new(directory.path().join("projects.json"));
        let project = ProjectId::new(projects.register(&root).await.unwrap()).unwrap();
        let service = HostRpcService::new(Err("fixture".into()), projects).unwrap();
        let mut create = create();
        if let CommandBody::ThreadCreate {
            project_id,
            worktree_path,
            ..
        } = &mut create.body
        {
            *project_id = project;
            *worktree_path = Some(worktree.to_string_lossy().into_owned());
        }
        service.dispatch_command(&create).unwrap();
        let mut thread = service
            .inner
            .store
            .projection(&create.thread_id)
            .unwrap()
            .thread;
        assert!(
            service
                .inner
                .resources
                .ensure_restore_isolated(&thread, worktree.to_str().unwrap())
                .await
                .is_ok()
        );
        let nested = worktree.join("scope");
        std::fs::create_dir(&nested).unwrap();
        assert!(
            service
                .inner
                .resources
                .ensure_restore_isolated(&thread, nested.to_str().unwrap())
                .await
                .is_ok()
        );
        thread.worktree_path = Some(root.to_string_lossy().into_owned());
        assert!(
            service
                .inner
                .resources
                .ensure_restore_isolated(&thread, root.to_str().unwrap())
                .await
                .is_err()
        );
        thread.worktree_path = None;
        assert!(
            service
                .inner
                .resources
                .ensure_restore_isolated(&thread, root.to_str().unwrap())
                .await
                .is_err()
        );
        let mut other = create.clone();
        other.thread_id = ThreadId::new("other").unwrap();
        other.command_id = CommandId::new("other-create").unwrap();
        service.dispatch_command(&other).unwrap();
        thread.worktree_path = Some(worktree.to_string_lossy().into_owned());
        assert!(
            service
                .inner
                .resources
                .ensure_restore_isolated(&thread, worktree.to_str().unwrap())
                .await
                .is_err()
        );
        let first = service.launch_lock(&create.command_id);
        let second = service.launch_lock(&other.command_id);
        let _guard = first.lock().await;
        assert!(second.try_lock().is_ok());
        assert!(service.launch_lock(&create.command_id).try_lock().is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn deleting_threads_keeps_shared_cwd_terminal_until_the_last_thread() {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let directory = tempfile::tempdir().unwrap();
            let projects = ProjectStore::new(directory.path().join("bex-worktrees.json"));
            let cwd = projects.chat_directory();
            tokio::fs::create_dir(&cwd).await.unwrap();
            let service =
                HostRpcService::new(Err("fixture provider unavailable".into()), projects).unwrap();
            let first = create();
            let mut second = first.clone();
            second.command_id = CommandId::new("create-second").unwrap();
            second.thread_id = ThreadId::new("second").unwrap();
            service.dispatch_command(&first).unwrap();
            service.dispatch_command(&second).unwrap();
            let session = service
                .inner
                .connections
                .open_authenticated_session(Some("fixture-device".into()));
            service
                .inner
                .resources
                .terminals
                .start(
                    service.inner.connections.clone(),
                    session.id(),
                    agent_protocol::operations::terminal_handle(&cwd.to_string_lossy()),
                    cwd.to_string_lossy().into_owned(),
                    agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
                )
                .await
                .unwrap();
            for (index, thread) in [first.thread_id, second.thread_id].into_iter().enumerate() {
                service
                    .dispatch_command(&Command {
                        command_id: CommandId::new(format!("delete-{index}")).unwrap(),
                        thread_id: thread.clone(),
                        body: CommandBody::ThreadDelete,
                    })
                    .unwrap();
                let projection = service.inner.store.projection(&thread).unwrap();
                service
                    .inner
                    .resources
                    .execute(
                        &Effect {
                            id: format!("cleanup-{index}"),
                            thread_id: thread,
                            body: EffectBody::TerminalCleanup,
                        },
                        projection,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    service
                        .inner
                        .resources
                        .terminals
                        .in_use(&dunce::canonicalize(&cwd).unwrap()),
                    index == 0
                );
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn stop_and_steer_bypass_unavailable_workspace_state() {
        let directory = tempfile::tempdir().unwrap();
        let projects = ProjectStore::new(directory.path().join("bex-worktrees.json"));
        let service =
            HostRpcService::new(Err("fixture provider unavailable".into()), projects.clone())
                .unwrap();
        service.dispatch_command(&create()).unwrap();
        service
            .dispatch_command(&Command {
                command_id: CommandId::new("send").unwrap(),
                thread_id: create().thread_id,
                body: CommandBody::MessageDispatch(input("start").into()),
            })
            .unwrap();
        let mut projection = service.inner.store.projection(&create().thread_id).unwrap();
        projection.thread.project_id = ProjectId::new("missing-project").unwrap();
        tokio::fs::write(
            projects.path().with_file_name("bex-projects.json"),
            "invalid JSON",
        )
        .await
        .unwrap();
        let run = &projection.runs[0];
        for body in [
            EffectBody::Interrupt {
                run_id: run.id.clone(),
                provider_turn_id: ProviderTurnId::new("turn").unwrap(),
            },
            EffectBody::Steer {
                run_id: run.id.clone(),
                provider_turn_id: ProviderTurnId::new("turn").unwrap(),
                message_id: run.user_message_id.clone(),
            },
        ] {
            let result = service
                .inner
                .resources
                .execute(
                    &Effect {
                        id: "control".into(),
                        thread_id: projection.thread.id.clone(),
                        body,
                    },
                    projection.clone(),
                )
                .await;
            assert_eq!(result.unwrap_err().message, "Codex unavailable");
        }
    }
    #[tokio::test]
    async fn launch_survives_delivery_cancellation_and_resends_share_one_worktree() {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("repository");
            std::fs::create_dir(&root).unwrap();
            crate::git::text(&root, &["init"]).unwrap();
            crate::git::text(
                &root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ],
            )
            .unwrap();
            let projects = ProjectStore::new(directory.path().join("bex-worktrees.json"));
            let project = projects.register(&root).await.unwrap();
            let service =
                HostRpcService::new(Err("fixture provider unavailable".into()), projects).unwrap();
            service
                .inner
                .resources
                .worktrees
                .settings(Some(agent_protocol::models::WorktreeSettings {
                    create_on_new_session: true,
                    ..Default::default()
                }))
                .await
                .unwrap();
            let mut command = create();
            if let CommandBody::ThreadCreate { project_id, .. } = &mut command.body {
                *project_id = ProjectId::new(project).unwrap();
            }
            let mut launch = agent_protocol::orchestration::LaunchThread {
                create: command,
                input: input(" "),
            };
            assert!(service.launch_thread(&launch).await.is_err());
            assert!(
                service
                    .inner
                    .resources
                    .worktrees
                    .list()
                    .await
                    .unwrap()
                    .is_empty()
            );
            launch.input.text = "start".into();
            let guard = service
                .launch_lock(&launch.create.command_id)
                .lock_owned()
                .await;
            let session = service.inner.connections.open_session();
            let delivery_service = service.clone();
            let call = Call::LaunchThread(Box::new(launch.clone()));
            let delivery =
                tokio::spawn(async move { delivery_service.dispatch(session.id(), &call).await });
            while Arc::strong_count(&service.inner) < 3 {
                tokio::task::yield_now().await;
            }
            delivery.abort();
            let _ = delivery.await;
            drop(guard);
            while service
                .inner
                .store
                .projection(&launch.create.thread_id)
                .is_err()
            {
                tokio::task::yield_now().await;
            }
            let (first, second) = tokio::join!(
                service.launch_thread(&launch),
                service.launch_thread(&launch)
            );
            assert!(first.is_ok() && second.is_ok());
            assert_eq!(
                service
                    .inner
                    .resources
                    .worktrees
                    .list()
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                service
                    .inner
                    .store
                    .projection(&launch.create.thread_id)
                    .unwrap()
                    .runs
                    .len(),
                1
            );
        })
        .await
        .unwrap();
    }
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
        let missing = service
            .dispatch(
                session.id(),
                &Call::SubscribeThread(agent_protocol::orchestration::SubscribeThread {
                    thread_id: ThreadId::new("missing").unwrap(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            protocol::decode::<Response<ThreadStreamItem>>(&missing.initial).unwrap(),
            Response::Failure { .. }
        ));
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
