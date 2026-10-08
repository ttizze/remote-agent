//! Host RPCs. Conversations run in the conversation runtime; authenticated
//! connections own only delivery.
use super::{
    connections::{Connections, HostReply, HostSession, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::ProjectStore;
use crate::conversation::{
    ClaudeCredentials, Conversation, ConversationConfig, ProjectCatalog, ProviderPrograms,
    SharedResources, SupervisedSpawner, tools::ModelCatalog,
};
use agent_domain::Driver;
use agent_protocol::{
    operations as op,
    protocol::{Body, Call, Response},
    provider::ProviderKind,
};
use agent_runtime::{ImportHome, ImportSettings, OsFs, RuntimeConfig, ScanConfig};
use agent_transport::peer::RpcMessageError;
use codex_app_server::CodexAppServer;
use futures_util::future::BoxFuture;
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

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

/// Where the conversation runtime finds providers and transcripts.
pub struct ConversationSettings {
    pub database: PathBuf,
    pub codex: PathBuf,
    pub codex_home: Option<PathBuf>,
}

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}
struct ServiceInner {
    resources: Arc<HostResources>,
    connections: Connections,
    started: AtomicBool,
}
struct HostResources {
    codex: Arc<CodexResources>,
    claude: OnceLock<Arc<ClaudeResources>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    preview: Arc<crate::preview::PreviewManager>,
    preview_ports: Arc<crate::preview::PortScanner>,
    conversation: OnceLock<Arc<Conversation>>,
    shared: SharedResources,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    dictation: crate::dictation::Dictation,
    auth_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    commands: super::commands::CommandCache,
    search: crate::workspace_search::WorkspaceSearch,
    keybindings: Arc<crate::keybindings::Keybindings>,
    devices: Arc<crate::device::DeviceService>,
}

/// The live model catalog for the agent tools, without keeping the service alive.
struct ServiceModels(Weak<ServiceInner>);
impl ModelCatalog for ServiceModels {
    fn providers(
        &self,
    ) -> BoxFuture<'_, Result<Vec<agent_protocol::models::ProviderInstance>, String>> {
        Box::pin(async move {
            let inner = self.0.upgrade().ok_or("the Host is shutting down")?;
            Ok(HostRpcService { inner }.providers().await)
        })
    }
}

impl ClaudeCredentials for ClaudeResources {
    fn claude_home(&self) -> BoxFuture<'_, Result<PathBuf, String>> {
        Box::pin(async move { self.credentials_home().await.map_err(|error| error.message) })
    }
}

impl HostRpcService {
    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        projects: ProjectStore,
    ) -> anyhow::Result<Self> {
        let connections = Connections::new();
        let terminal_history = projects.path().with_file_name("terminals");
        let keybindings = Arc::new(crate::keybindings::Keybindings::new(
            projects.path().with_file_name("keybindings.json"),
        ));
        let devices = crate::device::DeviceService::new(
            projects.path().with_file_name("device"),
        );
        let shared = SharedResources {
            files: crate::workspace_files::WorkspaceFiles::new(
                projects.path().with_file_name("attachments"),
            ),
            worktrees: Arc::new(crate::worktrees::Worktrees::new(projects.path())),
            projects: Arc::new(ProjectCatalog::new(projects)),
            checkpoints: Arc::default(),
            terminals: Arc::new(crate::terminals::Terminals::new(
                connections.clone(),
                terminal_history,
            )),
            devices: devices.clone(),
        };
        let resources = Arc::new(HostResources {
            codex: Arc::new(CodexResources::new(codex.clone())),
            claude: OnceLock::new(),
            startup_errors: Default::default(),
            browser: OnceLock::new(),
            preview: Arc::new(crate::preview::PreviewManager::new()),
            preview_ports: crate::preview::PortScanner::new(),
            conversation: OnceLock::new(),
            shared,
            worktree_access: Default::default(),
            permission_settings_access: Default::default(),
            dictation: crate::dictation::Dictation::new(codex),
            auth_task: OnceLock::new(),
            commands: Default::default(),
            search: Default::default(),
            keybindings,
            devices,
        });
        Ok(Self {
            inner: Arc::new(ServiceInner {
                resources,
                connections,
                started: AtomicBool::new(false),
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
        let resources = &self.inner.resources;
        let browser = crate::browser::Browser::start(profile).await?;
        browser.set_preview_resources(
            resources.preview.clone(),
            resources.preview_ports.clone(),
            resources.shared.terminals.clone(),
        )?;
        self.inner
            .resources
            .browser
            .set(browser)
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
        match ClaudeResources::load(program, directory, native_home).await {
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

    /// Opens the conversation store with this Host's providers. Call after the
    /// providers are enabled and before `start`.
    pub async fn enable_conversation(&self, settings: ConversationSettings) -> anyhow::Result<()> {
        let resources = &self.inner.resources;
        let codex = codex_app_server::resolve_executable(&settings.codex).ok();
        let claude = resources.claude.get().map(|claude| {
            (
                claude.program(),
                claude.clone() as Arc<dyn ClaudeCredentials>,
            )
        });
        let mut homes = vec![];
        if codex.is_some() {
            homes.push(ImportHome {
                driver: Driver::Codex,
                instance: "codex".into(),
                path: resources.codex.directory.clone(),
            });
        }
        if let Some((program, _)) = &claude {
            homes.push(ImportHome {
                driver: Driver::Claude,
                instance: "claude".into(),
                path: program.config_home.clone(),
            });
        }
        let mut runtime = RuntimeConfig::new(settings.database.clone());
        runtime.handoff = Arc::new(agent_runtime::ProviderHandoffCatalog);
        runtime.import = Some(ImportSettings {
            scan: ScanConfig {
                homes,
                home_dir: directories::BaseDirs::new()
                    .map(|dirs| dirs.home_dir().to_owned())
                    .unwrap_or_default(),
                temp_dir: std::env::temp_dir(),
                managed_dirs: settings
                    .database
                    .parent()
                    .into_iter()
                    .map(Path::to_owned)
                    .collect(),
            },
            fs: Arc::new(OsFs),
        });
        let browser = Arc::downgrade(resources);
        let conversation = Conversation::open(
            ConversationConfig {
                runtime,
                programs: ProviderPrograms {
                    codex,
                    codex_home: settings.codex_home,
                    codex_accounts: Some(
                        resources.codex.clone() as Arc<dyn crate::conversation::CodexCredentials>
                    ),
                    claude,
                },
                spawner: Arc::new(SupervisedSpawner),
                browser: Arc::new(move |thread| {
                    let resources = browser.upgrade()?;
                    let browser = resources.browser.get()?;
                    Some(browser.provider_config(thread.as_str()))
                }),
                models: Arc::new(ServiceModels(Arc::downgrade(&self.inner))),
            },
            resources.shared.clone(),
        )
        .await?;
        resources
            .conversation
            .set(conversation)
            .map_err(|_| anyhow::anyhow!("the conversation runtime is already open"))
    }

    #[cfg(test)]
    pub(crate) fn install_conversation(&self, conversation: Arc<Conversation>) {
        let _ = self.inner.resources.conversation.set(conversation);
    }
    #[cfg(test)]
    pub(crate) fn shared(&self) -> SharedResources {
        self.inner.resources.shared.clone()
    }

    /// Recovers and starts the conversation runtime; idempotent.
    pub async fn start(&self) -> anyhow::Result<()> {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Some(task) = self.inner.resources.codex.auth_requests() {
            let _ = self.inner.resources.auth_task.set(task);
        }
        // A broken keybindings file must not keep the Host from starting;
        // clients read its issues instead.
        if let Err(error) = self.inner.resources.keybindings.start().await {
            tracing::warn!(target: "keybindings", error = %format!("{error:#}"), "Could not start keybindings");
        }
        if let Some(conversation) = self.inner.resources.conversation.get() {
            conversation.start().await?;
        }
        Ok(())
    }
    pub fn open_session(&self) -> HostSession {
        self.inner.connections.open_session()
    }
    pub(crate) fn open_authenticated_session(&self, principal: String) -> HostSession {
        self.inner
            .connections
            .open_authenticated_session(Some(principal))
    }
    pub fn close_session(&self, session: SessionId) {
        self.inner.connections.close_session(session);
        self.inner.resources.shared.terminals.close_session(session);
        self.inner.resources.shared.files.clear_session(session);
        self.inner.resources.dictation.close_session(session);
    }
    pub(crate) fn revoke_device(&self, principal: &str) {
        self.inner
            .resources
            .shared
            .terminals
            .revoke_device(principal);
    }
    pub(crate) fn files(&self) -> &crate::workspace_files::WorkspaceFiles {
        &self.inner.resources.shared.files
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
    /// Stops provider processes after the conversation records the shutdown.
    pub(crate) async fn shutdown_owned_processes(&self) {
        if let Some(conversation) = self.inner.resources.conversation.get() {
            conversation.shutdown().await;
        }
        if let Some(browser) = self.inner.resources.browser.get() {
            browser.shutdown().await;
        }
        self.inner.resources.shared.terminals.shutdown().await;
        self.inner.resources.devices.shutdown_owned().await;
    }
    fn conversation(&self) -> Result<&Arc<Conversation>, Failure> {
        self.inner.resources.conversation.get().ok_or_else(|| {
            Failure::new("conversation_unavailable", "conversations are unavailable")
        })
    }
    pub async fn dispatch(&self, session: SessionId, call: &Call) -> Result<HostReply, String> {
        self.inner.connections.ensure_session(session)?;
        if let Some(conversation) = self.inner.resources.conversation.get() {
            let cancel = self.inner.connections.cancellation(session)?;
            if let Some(reply) = conversation.call(call, cancel).await {
                return Ok(reply);
            }
        }
        if let Call::TerminalMetadata(_) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.terminal_metadata(cancel));
        }
        if let Call::Keybindings(_) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.keybindings(cancel).await);
        }
        if let Call::PreviewSubscribe(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.preview_subscribe(params, cancel).await);
        }
        if let Call::DeviceSubscribe(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.device_subscribe(params, cancel).await);
        }
        Ok(Response::from_result(self.request(session, call).await).into())
    }

    async fn device_subscribe(
        &self,
        params: &agent_protocol::device::DeviceSubscribeInput,
        cancel: tokio_util::sync::CancellationToken,
    ) -> HostReply {
        let devices = self.inner.resources.devices.clone();
        let thread = params.thread_id.clone();
        let prefer_mjpeg = params.prefer_mjpeg;
        let lease = std::sync::Arc::new(devices.acquire_stream(&thread, prefer_mjpeg).await);
        let receiver = Arc::new(tokio::sync::Mutex::new(devices.subscribe()));
        let initial = agent_protocol::device::DeviceEvent::State(devices.state_async().await);
        crate::conversation::stream(
            std::collections::VecDeque::from([initial]),
            agent_protocol::device::DeviceEvent::State(agent_protocol::device::DeviceServiceState::default()),
            move || {
                let (receiver, devices, thread, lease) = (receiver.clone(), devices.clone(), thread.clone(), lease.clone());
                Box::pin(async move {
                    let _lease = lease;
                    loop {
                        let result = {
                            let mut receiver = receiver.lock().await;
                            tokio::time::timeout(
                                std::time::Duration::from_millis(500),
                                receiver.recv(),
                            )
                            .await
                        };
                        match result {
                            Ok(Ok(event)) if crate::device::DeviceService::event_belongs_to_thread(&event, &thread) => return Some(vec![event]),
                            Ok(Ok(_)) => continue,
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                                return Some(vec![agent_protocol::device::DeviceEvent::State(
                                    devices.state_async().await,
                                )]);
                            }
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return None,
                            Err(_) => {}
                        }
                    }
                })
            },
            cancel,
        )
    }
    /// The keybindings in effect, then each change; a subscriber that fell
    /// behind gets the latest.
    async fn keybindings(&self, cancel: tokio_util::sync::CancellationToken) -> HostReply {
        let keybindings = self.inner.resources.keybindings.clone();
        let receiver = keybindings.subscribe();
        let first = match keybindings.config().await {
            Ok(config) => config,
            Err(error) => {
                return Response::error("keybindings_unavailable", &format!("{error:#}")).into();
            }
        };
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        crate::conversation::stream(
            std::collections::VecDeque::from([first]),
            agent_protocol::keybindings::KeybindingsConfig::default(),
            move || {
                let (receiver, keybindings) = (receiver.clone(), keybindings.clone());
                Box::pin(async move {
                    let mut receiver = receiver.lock().await;
                    match receiver.recv().await {
                        Ok(config) => Some(vec![config]),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            *receiver = receiver.resubscribe();
                            keybindings.config().await.ok().map(|config| vec![config])
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                    }
                })
            },
            cancel,
        )
    }
    /// Every terminal first, then upserts and removals; a subscriber that fell
    /// behind gets a fresh snapshot.
    fn terminal_metadata(&self, cancel: tokio_util::sync::CancellationToken) -> HostReply {
        let terminals = self.inner.resources.shared.terminals.clone();
        let (snapshot, receiver) = terminals.subscribe_metadata();
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        crate::conversation::stream(
            std::collections::VecDeque::from([op::TerminalMetadataEvent::Snapshot {
                terminals: snapshot,
            }]),
            op::TerminalMetadataEvent::Snapshot { terminals: vec![] },
            move || {
                let (receiver, terminals) = (receiver.clone(), terminals.clone());
                Box::pin(async move {
                    let mut receiver = receiver.lock().await;
                    match receiver.recv().await {
                        Ok(event) => Some(vec![event]),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            *receiver = receiver.resubscribe();
                            Some(vec![op::TerminalMetadataEvent::Snapshot {
                                terminals: terminals.summaries_now(),
                            }])
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                    }
                })
            },
            cancel,
        )
    }
    /// Streams the complete Preview snapshot whenever either tab metadata or
    /// local-server discovery changes. Keeping the scanner lease inside the
    /// forwarding task makes disposal release the shared three-second poll.
    async fn preview_subscribe(
        &self,
        params: &agent_protocol::preview::PreviewSubscribe,
        cancel: tokio_util::sync::CancellationToken,
    ) -> HostReply {
        if let Err(error) = params.validate() {
            return Response::error("invalid_params", &error).into();
        }
        let resources = self.inner.resources.clone();
        let mut metadata = resources.preview.subscribe();
        resources
            .preview_ports
            .set_terminal_owners(resources.shared.terminals.preview_process_owners());
        let terminals = resources.shared.terminals.summaries_now();
        let configured_urls = params.configured_urls.clone();
        let initial_scan = resources
            .preview_ports
            .scan_snapshot(&configured_urls, &terminals)
            .await
            .unwrap_or_else(|_| crate::preview::ports::PortScanSnapshot {
                servers: Vec::new(),
                scanned_at: String::new(),
                epoch: String::new(),
                revision: 0,
            });
        let mut initial = resources.preview.list(&params.thread_id);
        initial.local_servers = initial_scan.servers.clone();
        initial.scanned_at = initial_scan.scanned_at.clone();
        initial.scanner_epoch = initial_scan.epoch.clone();
        initial.scanner_revision = initial_scan.revision;
        let (mut scanner, scanner_lease) = resources
            .preview_ports
            .subscribe(configured_urls, resources.shared.terminals.clone());
        let thread_id = params.thread_id.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let forward_cancel = cancel.clone();
        tokio::spawn(async move {
            let _scanner_lease = scanner_lease;
            let mut scan = initial_scan;
            loop {
                tokio::select! {
                    biased;
                    _ = forward_cancel.cancelled() => break,
                    servers = scanner.recv() => {
                        let Some(next_scan) = servers else { break };
                        scan = next_scan;
                        let mut snapshot = resources.preview.list(&thread_id);
                        snapshot.local_servers = scan.servers.clone();
                        snapshot.scanned_at = scan.scanned_at.clone();
                        snapshot.scanner_epoch = scan.epoch.clone();
                        snapshot.scanner_revision = scan.revision;
                        let sent = tokio::select! {
                            biased;
                            _ = forward_cancel.cancelled() => false,
                            result = sender.send(snapshot) => result.is_ok(),
                        };
                        if !sent { break; }
                    }
                    event = metadata.recv() => {
                        let event = match event {
                            Ok(event) => event,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                metadata = resources.preview.subscribe();
                                let mut snapshot = resources.preview.list(&thread_id);
                                snapshot.local_servers = scan.servers.clone();
                                snapshot.scanned_at = scan.scanned_at.clone();
                                snapshot.scanner_epoch = scan.epoch.clone();
                                snapshot.scanner_revision = scan.revision;
                                let sent = tokio::select! {
                                    biased;
                                    _ = forward_cancel.cancelled() => false,
                                    result = sender.send(snapshot) => result.is_ok(),
                                };
                                if !sent { break; }
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        };
                        if preview_event_thread(&event) != &thread_id { continue; }
                        let mut snapshot = resources.preview.list(&thread_id);
                        snapshot.local_servers = scan.servers.clone();
                        snapshot.scanned_at = scan.scanned_at.clone();
                        snapshot.scanner_epoch = scan.epoch.clone();
                        snapshot.scanner_revision = scan.revision;
                        let sent = tokio::select! {
                            biased;
                            _ = forward_cancel.cancelled() => false,
                            result = sender.send(snapshot) => result.is_ok(),
                        };
                        if !sent { break; }
                    }
                }
            }
        });
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        crate::conversation::stream(
            std::collections::VecDeque::from([initial.clone()]),
            initial,
            move || {
                let receiver = receiver.clone();
                Box::pin(async move {
                    receiver.lock().await.recv().await.map(|snapshot| vec![snapshot])
                })
            },
            cancel,
        )
    }
    async fn request(&self, session: SessionId, request: &Call) -> Result<Body, Failure> {
        let resources = &self.inner.resources;
        let _workspace = if matches!(
            request,
            Call::StartTerminal(_)
                | Call::RestartTerminal(_)
                | Call::WriteFile(_)
                | Call::Upload(_)
                | Call::AttachmentPath(_)
                | Call::ReviewWorkspace(_)
        ) {
            Some(resources.worktree_access.read().await)
        } else {
            None
        };
        let response =
            match request {
                Call::AddProject(params) => {
                    let id = resources
                        .shared
                        .projects
                        .store()
                        .register(&crate::projects::expand_home(params.cwd.trim()))
                        .await
                        .map_err(|error| Failure::new("project_add_failed", error))?;
                    if let Ok(conversation) = self.conversation() {
                        conversation.project_added(&id).await;
                    }
                    id.into()
                }
                Call::UpdateProject(params) => {
                    resources
                        .shared
                        .projects
                        .update(
                            &params.project_id,
                            params.scripts.clone(),
                            params.favicon_path.clone(),
                        )
                        .await
                        .map_err(|error| Failure::new("project_update_failed", error))?;
                    if let Ok(conversation) = self.conversation() {
                        conversation.project_updated(&params.project_id).await;
                    }
                    agent_protocol::models::Empty {}.into()
                }
                Call::ListProjects(_) => self.projects().await?.into(),
                Call::ProjectFavicon(params) => {
                    match resources
                        .shared
                        .projects
                        .favicon_source(&params.project_id)
                        .await
                    {
                        None => None::<agent_protocol::models::ProjectFavicon>.into(),
                        Some((root, saved)) => {
                            let known_hash = params.known_hash.clone();
                            tokio::task::spawn_blocking(move || {
                                crate::favicon::read(&root, saved.as_deref(), known_hash.as_deref())
                            })
                            .await
                            .map_err(|error| Failure::new("project_favicon_failed", error))?
                            .map_err(|error| Failure::new("project_favicon_failed", error))?
                            .into()
                        }
                    }
                }
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
                Call::ListProviders(_) => self.providers().await.into(),
                Call::ProviderCommands(params) => self.provider_commands(params).await?.into(),
                Call::SearchEntries(params) => resources
                    .search
                    .search(params.clone())
                    .await
                    .map_err(|error| Failure::new("search_entries_failed", error))?
                    .into(),
                Call::SearchContents(params) => resources
                    .search
                    .search_contents(params.clone())
                    .await
                    .map_err(|error| Failure::new("search_contents_failed", error))?
                    .into(),
                Call::VcsStatus(params) => crate::vcs::read_status(params.cwd.clone())
                    .await
                    .map_err(|error| Failure::new("vcs_status_failed", error))?
                    .into(),
                Call::ListRefs(params) => crate::vcs::refs(params.clone())
                    .await
                    .map_err(|error| Failure::new("vcs_refs_failed", error))?
                    .into(),
                Call::CreateRef(params) => crate::vcs::create_ref(params.clone())
                    .await
                    .map_err(|error| Failure::new("vcs_create_ref_failed", error))?
                    .into(),
                Call::SwitchRef(params) => crate::vcs::switch_ref(params.clone())
                    .await
                    .map_err(|error| Failure::new("vcs_switch_ref_failed", error))?
                    .into(),
                Call::DiffPreview(params) => crate::vcs::diff_preview(params.clone())
                    .await
                    .map_err(|error| Failure::new("diff_preview_failed", error))?
                    .into(),
                Call::ReadPermissionSettings(params) => {
                    let _guard = resources.permission_settings_access.lock().await;
                    match params.provider {
                        ProviderKind::Codex => resources.codex.read_permissions().await?,
                        ProviderKind::Claude => super::permissions::read_claude_permissions(
                            &self.claude()?.native_home,
                        )?,
                    }
                    .into()
                }
                Call::UpdatePermissionSettings(params) => {
                    let _guard = resources.permission_settings_access.lock().await;
                    match params.provider {
                        ProviderKind::Codex => {
                            resources
                                .codex
                                .update_permissions(params.mode, &params.version)
                                .await?
                        }
                        ProviderKind::Claude => super::permissions::update_claude_permissions(
                            &self.claude()?.native_home,
                            params.mode,
                            &params.version,
                        )?,
                    }
                    .into()
                }
                Call::Browser(params) => {
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    let frame = browser
                        .request(params)
                        .await
                        .map_err(|error| Failure::new("browser_failed", error))?;
                    frame.into()
                }
                Call::PreviewList(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    // Reconcile the browser target set before returning the
                    // metadata owner’s snapshot. This closes sessions whose
                    // targets disappeared outside the Preview UI.
                    let _ = browser
                        .preview_list(&params.thread_id.to_string())
                        .await
                        .map_err(|error| Failure::new("preview_list_failed", error))?;
                    resources
                        .preview_ports
                        .set_terminal_owners(resources.shared.terminals.preview_process_owners());
                    let terminals = resources.shared.terminals.summaries_now();
                    let scan = resources
                        .preview_ports
                        .scan_snapshot(&params.configured_urls, &terminals)
                        .await
                        .map_err(|error| Failure::new("preview_scan_failed", error))?;
                    let mut result = resources.preview.list(&params.thread_id);
                    result.local_servers = scan.servers;
                    result.scanned_at = scan.scanned_at;
                    result.scanner_epoch = scan.epoch;
                    result.scanner_revision = scan.revision;
                    result.into()
                }
                Call::PreviewOpen(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    let url = params
                        .url
                        .as_deref()
                        .map(agent_protocol::preview::normalize_preview_url)
                        .transpose()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let frame = browser
                        .open_preview_tab(
                            &params.thread_id.to_string(),
                            url.as_deref(),
                            params.viewport,
                            params.appearance,
                            params.zoom,
                            params.rendered_size,
                            params.profile_id.clone(),
                        )
                        .await
                        .map_err(|error| Failure::new("preview_open_failed", error))?;
                    let session = resources
                        .preview
                        .open(
                            params.thread_id.clone(),
                            frame.tab_id.clone(),
                            url.as_deref(),
                            params.viewport,
                            params.appearance,
                            params.zoom,
                            params.profile_id.clone(),
                        )
                        .map_err(|error| Failure::new("preview_open_failed", error))?;
                    browser.report_preview_frame(&params.thread_id.to_string(), &frame);
                    resources
                        .preview
                        .get(&params.thread_id, &frame.tab_id)
                        .unwrap_or(session)
                        .into()
                }
                Call::PreviewClearProfileData(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .clear_preview_profile(&params.profile_id)
                        .await
                        .map_err(|error| Failure::new("preview_profile_clear_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::PreviewNavigate(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let url = agent_protocol::preview::normalize_preview_url(&params.url)
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .request(&agent_protocol::browser::BrowserRequest {
                            thread_id: params.thread_id.clone(),
                            tab_id: params.tab_id.clone(),
                            image_id: String::new(),
                            action: agent_protocol::browser::BrowserAction::SelectTab {
                                id: params.tab_id.clone(),
                            },
                        })
                        .await
                        .map_err(|error| Failure::new("preview_navigation_failed", error))?;
                    browser
                        .request(&agent_protocol::browser::BrowserRequest {
                            thread_id: params.thread_id.clone(),
                            tab_id: params.tab_id.clone(),
                            image_id: String::new(),
                            action: agent_protocol::browser::BrowserAction::Navigate { url: url.clone() },
                        })
                        .await
                        .map_err(|error| Failure::new("preview_navigation_failed", error))?;
                    resources
                        .preview
                        .navigate(&params.thread_id, &params.tab_id, &url)
                        .map_err(|error| Failure::new("preview_navigation_failed", error))?
                        .into()
                }
                Call::PreviewResize(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .resize_preview_tab(
                            &params.thread_id.to_string(),
                            &params.tab_id,
                            params.viewport,
                            params.rendered_size,
                        )
                        .await
                        .map_err(|error| Failure::new("preview_resize_failed", error))?;
                    resources
                        .preview
                        .resize(&params.thread_id, &params.tab_id, params.viewport)
                        .map_err(|error| Failure::new("preview_resize_failed", error))?
                        .into()
                }
                Call::PreviewSetAppearance(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .set_preview_appearance(
                            &params.thread_id.to_string(),
                            &params.tab_id,
                            params.appearance,
                        )
                        .await
                        .map_err(|error| Failure::new("preview_appearance_failed", error))?;
                    resources
                        .preview
                        .appearance(&params.thread_id, &params.tab_id, params.appearance)
                        .map_err(|error| Failure::new("preview_appearance_failed", error))?
                        .into()
                }
                Call::PreviewSetZoom(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .set_preview_zoom(
                            &params.thread_id.to_string(),
                            &params.tab_id,
                            params.zoom,
                        )
                        .await
                        .map_err(|error| Failure::new("preview_zoom_failed", error))?;
                    resources
                        .preview
                        .zoom(&params.thread_id, &params.tab_id, params.zoom)
                        .map_err(|error| Failure::new("preview_zoom_failed", error))?
                        .into()
                }
                Call::PreviewReportStatus(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    resources
                        .preview
                        .report_status(
                            &params.thread_id,
                            &params.tab_id,
                            params.nav_status.clone(),
                            params.can_go_back,
                            params.can_go_forward,
                        )
                        .map_err(|error| Failure::new("preview_status_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::PreviewClose(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    // Reconcile Chrome targets first. A tab closed from the
                    // browser UI must not remain in the metadata owner and
                    // cause this close request to target a dead session.
                    let live = browser
                        .preview_list(&params.thread_id.to_string())
                        .await
                        .map_err(|error| Failure::new("preview_close_failed", error))?;
                    let ids: Vec<String> = live
                        .sessions
                        .into_iter()
                        .filter(|session| {
                            params
                                .tab_id
                                .as_deref()
                                .is_none_or(|tab_id| tab_id == session.tab_id)
                        })
                        .map(|session| session.tab_id)
                        .collect();
                    for id in ids {
                        browser
                            .close_preview_tab(&params.thread_id.to_string(), &id)
                            .await
                            .map_err(|error| Failure::new("preview_close_failed", error))?;
                    }
                    agent_protocol::models::Empty {}.into()
                }
                Call::PreviewRefresh(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    resources
                        .preview
                        .refresh(&params.thread_id, &params.tab_id)
                        .map_err(|error| Failure::new("preview_refresh_failed", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .request(&agent_protocol::browser::BrowserRequest {
                            thread_id: params.thread_id.clone(),
                            tab_id: params.tab_id.clone(),
                            image_id: String::new(),
                            action: agent_protocol::browser::BrowserAction::SelectTab {
                                id: params.tab_id.clone(),
                            },
                        })
                        .await
                        .map_err(|error| Failure::new("preview_refresh_failed", error))?;
                    browser
                        .request(&agent_protocol::browser::BrowserRequest {
                            thread_id: params.thread_id.clone(),
                            tab_id: params.tab_id.clone(),
                            image_id: String::new(),
                            action: agent_protocol::browser::BrowserAction::Reload,
                        })
                        .await
                        .map_err(|error| Failure::new("preview_refresh_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::PreviewRecordingStart(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .start_preview_recording(
                            &params.thread_id.to_string(),
                            &params.tab_id,
                            params.options,
                        )
                        .await
                        .map_err(|error| Failure::new("preview_recording_start_failed", error))?
                        .into()
                }
                Call::PreviewRecordingStop(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let browser = resources
                        .browser
                        .get()
                        .ok_or_else(|| Failure::new("browser_unavailable", "browser unavailable"))?;
                    browser
                        .stop_preview_recording(&params.thread_id.to_string(), &params.tab_id)
                        .await
                        .map_err(|error| Failure::new("preview_recording_stop_failed", error))?
                        .into()
                }
                Call::DeviceList(params) => resources
                    .devices
                    .list(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_list_failed", error))?
                    .into(),
                Call::DeviceConfigure(params) => resources
                    .devices
                    .configure(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_configure_failed", error))?
                    .into(),
                Call::DeviceHosts(params) => resources
                    .devices
                    .update_hosts(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_hosts_failed", error))?
                    .into(),
                Call::DeviceOpen(params) => resources
                    .devices
                    .open(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_open_failed", error))?
                    .into(),
                Call::DeviceClose(params) => {
                    resources
                        .devices
                        .close(params.clone())
                        .await
                        .map_err(|error| Failure::new("device_close_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::DeviceShutdown(params) => {
                    resources
                        .devices
                        .shutdown(params.clone())
                        .await
                        .map_err(|error| Failure::new("device_shutdown_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::DeviceDetail(params) => resources
                    .devices
                    .detail(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_detail_failed", error))?
                    .into(),
                Call::DeviceAction(params) => resources
                    .devices
                    .action(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_action_failed", error))?
                    .into(),
                Call::DeviceScreenshot(params) => resources
                    .devices
                    .screenshot(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_screenshot_failed", error))?
                    .into(),
                Call::DeviceInput(params) => {
                    resources
                        .devices
                        .input(params.clone())
                        .await
                        .map_err(|error| Failure::new("device_input_failed", error))?
                        .into()
                }
                Call::DeviceAccessibility(params) => resources
                    .devices
                    .accessibility(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_accessibility_failed", error))?
                    .into(),
                Call::DeviceEventLog(params) => resources
                    .devices
                    .event_log(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_event_log_failed", error))?
                    .into(),
                Call::DeviceRecordingStart(params) => resources
                    .devices
                    .start_recording(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_recording_start_failed", error))?
                    .into(),
                Call::DeviceRecordingStop(params) => resources
                    .devices
                    .stop_recording(params.clone())
                    .await
                    .map_err(|error| Failure::new("device_recording_stop_failed", error))?
                    .into(),
                Call::DeviceSubscribe(_) => unreachable!("device subscription is handled above"),
                Call::ConnectionPerformance(params) => {
                    let params = params.clone();
                    tokio::task::spawn_blocking(move || {
                        agent_transport::diagnostics::connection_performance(&params)
                    })
                    .await
                    .map_err(|error| Failure::new("diagnostic_write_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::ReadConversationSettings(_) | Call::UpdateConversationSettings(_) => {
                    let update = match request {
                        Call::UpdateConversationSettings(settings) => Some(settings.clone()),
                        _ => None,
                    };
                    let changed = update.is_some();
                    let settings = resources
                        .shared
                        .worktrees
                        .conversation_settings(update)
                        .await
                        .map_err(|error| Failure::new("settings_update_failed", error))?;
                    if changed && let Ok(conversation) = self.conversation() {
                        conversation.settings_changed();
                    }
                    settings.into()
                }
                Call::UpsertKeybinding(params) => (resources
                    .keybindings
                    .upsert(params.clone())
                    .await
                    .map_err(|error| Failure::new("keybindings_update_failed", error))?)
                .into(),
                Call::RemoveKeybinding(params) => (resources
                    .keybindings
                    .remove(params.clone())
                    .await
                    .map_err(|error| Failure::new("keybindings_update_failed", error))?)
                .into(),
                Call::ReadWorktreeSettings(_) | Call::UpdateWorktreeSettings(_) => {
                    let update = if let Call::UpdateWorktreeSettings(settings) = request {
                        Some(settings.clone())
                    } else {
                        None
                    };
                    (resources
                        .shared
                        .worktrees
                        .settings(update)
                        .await
                        .map_err(|error| Failure::new("worktree_settings_failed", error))?)
                    .into()
                }
                Call::ListWorktrees(_) => (self.worktree_list().await?).into(),
                Call::RemoveWorktree(params) => {
                    let _exclusive = resources.worktree_access.write().await;
                    (self.remove_worktree(params.path.clone()).await?).into()
                }
                Call::StartTerminal(params) => (resources
                    .shared
                    .terminals
                    .attach(session, params)
                    .await
                    .map_err(|error| Failure::new("terminal_start_failed", error))?)
                .into(),
                Call::WriteTerminal(_)
                | Call::ResizeTerminal(_)
                | Call::KillTerminal(_)
                | Call::DetachTerminal(_)
                | Call::ClearTerminal(_)
                | Call::RestartTerminal(_) => (resources
                    .shared
                    .terminals
                    .request(session, request)
                    .await
                    .map_err(|error| Failure::new("terminal_operation_failed", error))?)
                .into(),
                Call::PrepareDictation(params) => {
                    resources
                        .dictation
                        .prepare(session, params.id.clone())
                        .map_err(|error| Failure::new("dictation_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::CancelDictation(params) => {
                    resources.dictation.cancel(session, &params.id);
                    agent_protocol::models::Empty {}.into()
                }
                Call::Transcribe(params) => (resources
                    .dictation
                    .transcribe(session, params.preparation.as_deref(), &params.audio)
                    .await
                    .map_err(|error| Failure::new("dictation_failed", error))?)
                .into(),
                Call::ReviewWorkspace(params) => (crate::inspect_workspace(params.cwd.clone())
                    .await
                    .map_err(|error| Failure::new("workspace_review_failed", error))?)
                .into(),
                Call::ListFiles(_)
                | Call::ReadFile(_)
                | Call::WriteFile(_)
                | Call::Upload(_)
                | Call::AttachmentPath(_)
                | Call::Download(_)
                | Call::ReadVisualization(_) => resources
                    .shared
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
    fn claude(&self) -> Result<&Arc<ClaudeResources>, Failure> {
        self.inner
            .resources
            .claude
            .get()
            .ok_or_else(|| Failure::new("provider_unavailable", "Claude unavailable"))
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
        let (select, logout) = (
            matches!(command, Command::Select { .. }),
            matches!(command, Command::Logout { .. }),
        );
        let reply = self.identity(provider)?.account(command).await?;
        if provider == ProviderKind::Codex
            && let Ok(conversation) = self.conversation()
        {
            let sessions = conversation.runtime.sessions();
            if select && let Err(error) = sessions.apply_codex_account("codex").await {
                tracing::warn!(operation = "conversation.codex_account", message = %error);
                return Err(Failure::new("account_operation_failed", error));
            }
            // Closes an instance's sessions when it signs out.
            if logout && self.inner.resources.codex.signed_out() {
                sessions.close_instance("codex").await;
            }
        }
        Ok(match reply {
            AccountReply::Selection(value) => value.into(),
            AccountReply::Login(value) => value.into(),
            AccountReply::Status(value) => value.into(),
            AccountReply::Complete => agent_protocol::models::Empty {}.into(),
        })
    }

    pub(crate) async fn cleanup_merged_worktrees(&self) -> anyhow::Result<()> {
        let worktrees = &self.inner.resources.shared.worktrees;
        if !worktrees.settings(None).await?.delete_merged {
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
                && let Err(error) = worktrees.remove(source.cwd, true).await
            {
                tracing::warn!(operation = "host.worktree.cleanup", message = %error);
            }
        }
        Ok(())
    }

    async fn projects(&self) -> Result<Vec<agent_protocol::models::Project>, Failure> {
        let catalog = &self.inner.resources.shared.projects;
        Ok(catalog
            .refresh()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))?
            .into_iter()
            .map(|project| catalog.wire(project))
            .collect())
    }
    /// The skills and slash commands of one instance in one directory.
    async fn provider_commands(
        &self,
        params: &agent_protocol::workspace::ListProviderCommands,
    ) -> Result<agent_protocol::workspace::ProviderCommands, Failure> {
        use super::commands;
        let resources = &self.inner.resources;
        if !params.fresh
            && let Some(cached) = resources.commands.get(&params.instance, &params.cwd)
        {
            return Ok(cached);
        }
        let cwd = tokio::fs::canonicalize(&params.cwd)
            .await
            .map_err(|error| Failure::new("invalid_params", error))?;
        let cwd = dunce::simplified(&cwd).to_owned();
        let mut scan = agent_protocol::workspace::ProviderCommands {
            instance: params.instance.clone(),
            cwd: params.cwd.clone(),
            slash_commands: vec![],
            slash_commands_pending: false,
            skills: vec![],
        };
        match params.instance.as_str() {
            "codex" => {
                let shares_tokens =
                    crate::conversation::CodexCredentials::shares_tokens(resources.codex.as_ref())
                        .await;
                scan.slash_commands = commands::codex_commands(shares_tokens);
                if resources.codex.availability().is_ok() {
                    let listed: Result<serde_json::Value, _> = tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        resources
                            .codex
                            .request("skills/list", &serde_json::json!({"cwds": [params.cwd]})),
                    )
                    .await
                    .map_err(|_| Failure::new("provider_failed", "skills/list timed out"))
                    .and_then(|result| result);
                    match listed {
                        Ok(listed) => scan.skills = commands::codex_skills(&listed, &params.cwd),
                        Err(error) => {
                            tracing::warn!(operation = "host.provider.skills", message = %error)
                        }
                    }
                }
            }
            "claude" => {
                let claude = self.claude()?;
                scan.skills = crate::claude::skills::discover(&claude.native_home, Some(&cwd));
                let initialized = match claude.credentials_home().await {
                    Ok(home) => claude.program().query_control(&home, &cwd, None).await.ok(),
                    Err(_) => None,
                };
                match initialized {
                    Some(initialized) => {
                        scan.slash_commands = commands::claude_commands(&initialized)
                    }
                    None => {
                        scan.slash_commands = vec![commands::compact()];
                        scan.slash_commands_pending = true;
                    }
                }
            }
            other => {
                return Err(Failure::new(
                    "provider_unavailable",
                    format!("unknown provider instance {other}"),
                ));
            }
        }
        Ok(resources.commands.put(scan))
    }
    /// Codex and Claude as the composer offers them, with their models.
    async fn providers(&self) -> Vec<agent_protocol::models::ProviderInstance> {
        use agent_protocol::models::{ProviderInstance, ProviderStatus};
        let resources = &self.inner.resources;
        let instance = |driver: Driver, name: &str| ProviderInstance {
            instance: name.to_lowercase(),
            driver,
            display_name: name.into(),
            accent_color: None,
            enabled: true,
            installed: true,
            version: None,
            status: ProviderStatus::Ready,
            message: None,
            unavailable_reason: None,
            show_interaction_mode_toggle: true,
            reports_context_window: true,
            supported_runtime_modes: vec![],
            models: vec![],
        };
        let mut codex = instance(Driver::Codex, "Codex");
        codex.installed = resources.codex.server().is_ok();
        match resources.codex.availability() {
            Err(error) => {
                codex.status = ProviderStatus::Error;
                codex.message = Some(error.message);
            }
            Ok(()) => match resources.codex.models().await {
                Ok(models) => codex.models = models,
                Err(error) => {
                    codex.status = ProviderStatus::Error;
                    codex.message = Some(error.message);
                }
            },
        }
        let mut claude = instance(Driver::Claude, "Claude");
        match resources.claude.get() {
            Some(resources) => {
                claude.version = resources.version().await;
                claude.message = agent_providers::claude_upgrade_message(claude.version.as_deref());
            }
            None => {
                claude.installed = false;
                claude.status = ProviderStatus::Error;
                claude.message = Some(
                    resources
                        .startup_errors
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&ProviderKind::Claude)
                        .map(|error| error.message.clone())
                        .unwrap_or_else(|| {
                            "Claude Agent CLI (`claude`) was not found on PATH.".into()
                        }),
                );
            }
        }
        claude.models = agent_providers::claude_catalog(claude.version.as_deref())
            .into_iter()
            .map(super::resources::wire_model)
            .collect();
        vec![codex, claude]
    }
    async fn worktree_list(&self) -> Result<Vec<agent_protocol::models::Worktree>, Failure> {
        let shared = &self.inner.resources.shared;
        let mut entries = shared
            .worktrees
            .list()
            .await
            .map_err(|error| Failure::new("worktree_list_failed", error))?;
        let threads = match self.conversation() {
            Ok(conversation) => conversation
                .runtime
                .store()
                .thread_shells()
                .map_err(|error| Failure::new("worktree_list_failed", error))?,
            Err(_) => vec![],
        };
        let projects = shared.projects.list();
        for thread in threads {
            let shell = thread.row.summary;
            let Some(cwd) = shell.workspace.as_ref().map(|w| w.cwd.clone()).or_else(|| {
                projects
                    .iter()
                    .find(|project| project.id == shell.project)
                    .map(|project| project.root.clone())
            }) else {
                continue;
            };
            let active = shell.active_run.is_some() || !shell.pending_background_work.is_empty();
            for entry in entries
                .iter_mut()
                .filter(|entry| Path::new(&cwd).starts_with(&entry.path))
            {
                entry.threads.push(agent_protocol::models::WorktreeThread {
                    id: shell.id.clone(),
                    name: shell.title.clone(),
                    active,
                });
                if active {
                    entry.blocked_reason=Some("このワークツリーで作業を実行中です。完了または停止してから削除してください。".into());
                }
            }
        }
        for entry in &mut entries {
            if shared.terminals.in_use(Path::new(&entry.path)) {
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
        if let Ok(conversation) = self.conversation() {
            for thread in entry.threads {
                conversation
                    .runtime
                    .sessions()
                    .detach(&thread.id, true)
                    .await;
            }
        }
        self.inner
            .resources
            .shared
            .worktrees
            .remove(path, false)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }
}

fn preview_event_thread(event: &agent_protocol::preview::PreviewEvent) -> &agent_domain::ThreadId {
    match event {
        agent_protocol::preview::PreviewEvent::Opened { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Navigated { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Resized { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Failed { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Closed { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::RecordingChanged { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::RecordingArtifactRemoved { thread_id, .. } => thread_id,
}
}

fn provider_key(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
    .into()
}
