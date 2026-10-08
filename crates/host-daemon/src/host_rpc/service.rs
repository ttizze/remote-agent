//! Host RPCs. Conversations run in the conversation runtime; authenticated
//! connections own only delivery.
use super::{
    connections::{Connections, HostReply, HostSession, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::ProjectStore;
use crate::claude::control::ClaudeProgram;
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
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
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
    state_directory: PathBuf,
    codex: Arc<CodexResources>,
    claude: OnceLock<Arc<ClaudeResources>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    conversation: OnceLock<Arc<Conversation>>,
    shared: SharedResources,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    dictation: crate::dictation::Dictation,
    auth_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    commands: super::commands::CommandCache,
    search: crate::workspace_search::WorkspaceSearch,
    keybindings: Arc<crate::keybindings::Keybindings>,
    provider_update_locks: tokio::sync::Mutex<HashSet<String>>,
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

fn cleanup_old_files(root: &Path, days: u32) -> usize {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(days) * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0;
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            removed += cleanup_old_files(&path, days);
            let _ = fs::remove_dir(&path);
        } else if metadata.is_file()
            && metadata.modified().is_ok_and(|modified| modified <= cutoff)
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
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
        let state_directory = projects
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let connections = Connections::new();
        let terminal_history = projects.path().with_file_name("terminals");
        let keybindings = Arc::new(crate::keybindings::Keybindings::new(
            projects.path().with_file_name("keybindings.json"),
        ));
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
        };
        let resources = Arc::new(HostResources {
            state_directory,
            codex: Arc::new(CodexResources::new(codex.clone())),
            claude: OnceLock::new(),
            startup_errors: Default::default(),
            browser: OnceLock::new(),
            conversation: OnceLock::new(),
            shared,
            worktree_access: Default::default(),
            permission_settings_access: Default::default(),
            dictation: crate::dictation::Dictation::new(codex),
            auth_task: OnceLock::new(),
            commands: Default::default(),
            search: Default::default(),
            keybindings,
            provider_update_locks: tokio::sync::Mutex::new(HashSet::new()),
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
        resources.shared.worktrees.host_settings(None).await?;
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
        for (instance, config) in resources
            .shared
            .worktrees
            .latest_host_settings()
            .provider_instances
        {
            if !config.enabled {
                continue;
            }
            let Some(path) = config
                .home_path
                .map(|path| crate::projects::expand_home(&path))
            else {
                continue;
            };
            if homes
                .iter()
                .any(|home| home.instance == instance && home.path == path)
            {
                continue;
            }
            homes.push(ImportHome {
                driver: config.driver,
                instance,
                path,
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
                browser: Arc::new(move |thread, project| {
                    let resources = browser.upgrade()?;
                    let settings = resources.shared.worktrees.latest_host_settings();
                    let enabled = project
                        .and_then(|project| {
                            settings
                                .project_overrides
                                .get(project)
                                .and_then(|overrides| overrides.enable_agent_browser_access)
                        })
                        .unwrap_or(settings.enable_agent_browser_access);
                    if !enabled {
                        return None;
                    }
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
        Ok(Response::from_result(self.request(session, call).await).into())
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
                Call::UpdateProvider(params) => self.update_provider(params).await?.into(),
                Call::SearchAcpRegistry(params) => crate::acp_registry::search(params)
                    .await
                    .map_err(|error| Failure::new("acp_registry_unavailable", error))?
                    .into(),
                Call::PrepareAcpAgent(params) => {
                    crate::acp_registry::prepare(params, &resources.state_directory)
                        .await
                        .map_err(|error| Failure::new("acp_prepare_failed", error))?
                        .into()
                }
                Call::UninstallAcpAgent(params) => crate::acp_registry::uninstall_managed_binary(
                    &params.agent_id,
                    &resources.state_directory,
                )
                .await
                .map_err(|error| Failure::new("acp_uninstall_failed", error))?
                .into(),
                Call::ProbeAcpAgent(params) => {
                    crate::acp_registry::probe(params, &resources.state_directory)
                        .await
                        .map_err(|error| Failure::new("acp_probe_failed", error))?
                        .into()
                }
                Call::SearchEntries(params) => resources
                    .search
                    .search(params.clone())
                    .await
                    .map_err(|error| Failure::new("search_entries_failed", error))?
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
                Call::Browser(params) => resources
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
                Call::ReadHostResources(_) => super::resources::host_resources().await.into(),
                Call::ReadSettings(_) | Call::UpdateSettings(_) => {
                    let update = match request {
                        Call::UpdateSettings(settings) => Some((**settings).clone()),
                        _ => None,
                    };
                    let changed = update.is_some();
                    let settings = resources
                        .shared
                        .worktrees
                        .host_settings(update)
                        .await
                        .map_err(|error| Failure::new("settings_update_failed", error))?;
                    let _ = self.cleanup_storage().await;
                    if changed && let Ok(conversation) = self.conversation() {
                        conversation.settings_changed();
                    }
                    if changed {
                        resources.commands.clear();
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

    async fn cleanup_storage(&self) -> anyhow::Result<()> {
        let entries = self.worktree_list().await?;
        let settings = self.inner.resources.shared.worktrees.latest_host_settings();
        let project_rules = self
            .inner
            .resources
            .shared
            .projects
            .list()
            .into_iter()
            .filter_map(|project| {
                settings
                    .project_overrides
                    .get(&project.id)
                    .and_then(|overrides| overrides.worktree_cleanup.clone())
                    .map(|rules| (project.root, rules))
            })
            .collect::<HashMap<_, _>>();
        let protected: HashSet<String> = entries
            .iter()
            .filter(|entry| entry.blocked_reason.is_some())
            .map(|entry| entry.path.clone())
            .collect();
        let live_threads = self.conversation().ok().map(|_| {
            entries
                .iter()
                .flat_map(|entry| entry.threads.iter())
                .map(|thread| thread.id.as_str().to_owned())
                .collect::<HashSet<_>>()
        });
        let removed = self
            .inner
            .resources
            .shared
            .worktrees
            .cleanup_storage(&protected, live_threads.as_ref(), &project_rules)
            .await?;
        if removed > 0 {
            tracing::info!(removed, "cleaned stored worktrees");
        }
        let storage = settings.storage_cleanup;
        let browser_root = self
            .inner
            .resources
            .browser
            .get()
            .map(|browser| browser.profile().join("artifacts"));
        let logs_root = self
            .inner
            .resources
            .shared
            .projects
            .store()
            .path()
            .parent()
            .map(|parent| parent.join("logs"));
        let browser_days = storage.browser_artifacts_after_days;
        let logs_days = storage.logs_after_days;
        let removed_files = tokio::task::spawn_blocking(move || {
            let browser = match (browser_root, browser_days) {
                (Some(root), Some(days)) => cleanup_old_files(&root, days),
                _ => 0,
            };
            let logs = match (logs_root, logs_days) {
                (Some(root), Some(days)) => cleanup_old_files(&root, days),
                _ => 0,
            };
            browser + logs
        })
        .await??;
        if removed_files > 0 {
            tracing::info!(
                removed = removed_files,
                "cleaned stored browser artifacts and logs"
            );
        }
        Ok(())
    }

    pub(crate) async fn cleanup_merged_worktrees(&self) -> anyhow::Result<()> {
        let worktrees = &self.inner.resources.shared.worktrees;
        let _exclusive = self.inner.resources.worktree_access.write().await;
        self.cleanup_storage().await?;
        if !worktrees.settings(None).await?.delete_merged {
            return Ok(());
        }
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

    pub(crate) async fn background_activity_tick(
        &self,
        fetch_origins: bool,
        refresh_providers: bool,
    ) -> anyhow::Result<()> {
        if fetch_origins {
            let project_roots = self
                .inner
                .resources
                .shared
                .projects
                .list()
                .into_iter()
                .map(|project| project.root)
                .collect();
            let fetched = self
                .inner
                .resources
                .shared
                .worktrees
                .fetch_origins(project_roots)
                .await?;
            if fetched > 0 {
                tracing::debug!(fetched, "refreshed managed Git remotes");
            }
        }
        if refresh_providers {
            let providers = self.providers().await;
            tracing::debug!(providers = providers.len(), "refreshed provider health");
        }
        Ok(())
    }

    pub(crate) fn background_activity(&self) -> agent_protocol::models::ResolvedBackgroundActivity {
        self.inner
            .resources
            .shared
            .worktrees
            .latest_host_settings()
            .background_activity
            .resolved()
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
        let configured = resources
            .shared
            .worktrees
            .latest_host_settings()
            .provider_instances
            .get(&params.instance)
            .cloned();
        if configured.as_ref().is_some_and(|config| !config.enabled) {
            return Err(Failure::new(
                "provider_unavailable",
                format!("provider instance {} is disabled", params.instance),
            ));
        }
        let driver = configured
            .as_ref()
            .map(|config| config.driver)
            .or_else(|| match params.instance.as_str() {
                "codex" => Some(Driver::Codex),
                "claude" => Some(Driver::Claude),
                _ => None,
            })
            .ok_or_else(|| {
                Failure::new(
                    "provider_unavailable",
                    format!("unknown provider instance {}", params.instance),
                )
            })?;
        match driver {
            Driver::Codex => {
                let shares_tokens = if params.instance == "codex" {
                    crate::conversation::CodexCredentials::shares_tokens(resources.codex.as_ref())
                        .await
                } else {
                    false
                };
                scan.slash_commands = commands::codex_commands(shares_tokens);
                // Every configured instance gets its own app-server probe,
                // even when only its model catalogue is customized. Reusing
                // the built-in Codex session here would make slash commands
                // depend on whichever instance happened to be selected last.
                let custom = configured.as_ref();
                let listed: Result<serde_json::Value, Failure> = if let Some(config) = custom {
                    let app_server = CodexAppServer::spawn(codex_app_server::AppServerConfig {
                        program: config
                            .binary_path
                            .as_deref()
                            .map(crate::projects::expand_home)
                            .unwrap_or_else(|| PathBuf::from("codex")),
                        codex_home: config
                            .home_path
                            .as_deref()
                            .map(crate::projects::expand_home),
                        environment: config.environment.clone(),
                        launch_args: config.launch_args.clone(),
                        ..Default::default()
                    })
                    .await
                    .map_err(|error| Failure::new("provider_unavailable", error))?;
                    let timed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                        app_server
                            .request::<_, serde_json::Value>(
                                "skills/list",
                                &serde_json::json!({"cwds": [params.cwd]}),
                            )
                            .await
                            .map_err(|error| Failure::new("provider_failed", error))?
                            .outcome
                            .map_err(|error| Failure::new("provider_failed", error.get()))
                    })
                    .await
                    .map_err(|_| Failure::new("provider_failed", "skills/list timed out"));
                    let _ = app_server.shutdown().await;
                    timed.and_then(|result| result)
                } else if resources.codex.availability().is_ok() {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        resources
                            .codex
                            .request("skills/list", &serde_json::json!({"cwds": [params.cwd]})),
                    )
                    .await
                    .map_err(|_| Failure::new("provider_failed", "skills/list timed out"))
                    .and_then(|result| result)
                } else {
                    Err(Failure::new("provider_unavailable", "Codex is unavailable"))
                };
                {
                    match listed {
                        Ok(listed) => scan.skills = commands::codex_skills(&listed, &params.cwd),
                        Err(error) => {
                            tracing::warn!(operation = "host.provider.skills", message = %error)
                        }
                    }
                }
            }
            Driver::Claude => {
                let configured_home = configured
                    .as_ref()
                    .and_then(|config| config.home_path.as_deref())
                    .map(crate::projects::expand_home);
                let configured_program = configured
                    .as_ref()
                    .and_then(|config| config.binary_path.as_deref())
                    .map(crate::projects::expand_home);
                let launch_args = configured
                    .as_ref()
                    .map_or_else(Vec::new, |config| config.launch_args.clone());
                let (program, credentials_home) = match self.inner.resources.claude.get() {
                    Some(claude) => {
                        let base = claude.program();
                        let program = ClaudeProgram {
                            program: configured_program.unwrap_or_else(|| base.program.clone()),
                            config_home: configured_home
                                .clone()
                                .unwrap_or_else(|| base.config_home.clone()),
                            environment: configured
                                .as_ref()
                                .map_or_else(BTreeMap::new, |config| config.environment.clone()),
                            launch_args: launch_args.clone(),
                        };
                        let home = match configured_home {
                            Some(home) => Some(home),
                            None => claude.credentials_home().await.ok(),
                        };
                        (Some(program), home)
                    }
                    None => configured_program
                        .zip(configured_home.clone())
                        .map(|(program, home)| {
                            (
                                Some(ClaudeProgram {
                                    program,
                                    config_home: home.clone(),
                                    environment: configured
                                        .as_ref()
                                        .map_or_else(BTreeMap::new, |config| {
                                            config.environment.clone()
                                        }),
                                    launch_args,
                                }),
                                Some(home),
                            )
                        })
                        .unwrap_or((None, None)),
                };
                if let Some(program) = &program {
                    scan.skills = crate::claude::skills::discover(&program.config_home, Some(&cwd));
                }
                let initialized = match (program, credentials_home) {
                    (Some(program), Some(home)) => {
                        program.query_control(&home, &cwd, None).await.ok()
                    }
                    _ => None,
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
        }
        Ok(resources.commands.put(scan))
    }

    async fn update_provider(
        &self,
        params: &agent_protocol::operations::UpdateProvider,
    ) -> Result<agent_protocol::operations::ProviderUpdate, Failure> {
        let resources = &self.inner.resources;
        let settings = resources.shared.worktrees.latest_host_settings();
        let configured = settings.provider_instances.get(&params.instance).cloned();
        if configured.as_ref().is_some_and(|config| !config.enabled) {
            return Err(Failure::new(
                "provider_unavailable",
                format!("provider instance {} is disabled", params.instance),
            ));
        }
        let driver = configured
            .as_ref()
            .map(|config| config.driver)
            .or_else(|| match params.instance.as_str() {
                "codex" => Some(Driver::Codex),
                "claude" => Some(Driver::Claude),
                _ => None,
            })
            .ok_or_else(|| {
                Failure::new(
                    "provider_unavailable",
                    format!("unknown provider instance {}", params.instance),
                )
            })?;
        let binary = configured
            .as_ref()
            .and_then(|config| config.binary_path.as_deref())
            .map(crate::projects::expand_home)
            .or_else(|| match driver {
                Driver::Codex => Some(PathBuf::from("codex")),
                Driver::Claude => resources
                    .claude
                    .get()
                    .map(|claude| claude.program().program.clone()),
            })
            .ok_or_else(|| Failure::new("provider_unavailable", "provider is not installed"))?;
        let home = configured
            .as_ref()
            .and_then(|config| config.home_path.as_deref())
            .map(crate::projects::expand_home);
        let environment = configured
            .as_ref()
            .map(|config| config.environment.clone())
            .unwrap_or_default();
        let lock_keys = [
            format!("instance:{}", params.instance),
            format!(
                "installation:{}",
                crate::provider_maintenance::installation_lock_key(
                    driver,
                    &binary,
                    home.as_deref()
                )
            ),
        ];
        {
            let mut locks = resources.provider_update_locks.lock().await;
            if lock_keys.iter().any(|key| locks.contains(key)) {
                return Err(Failure::new(
                    "provider_update_running",
                    "another update is already running for this provider instance",
                ));
            }
            locks.extend(lock_keys.iter().cloned());
        }
        let result = async {
            crate::provider_maintenance::update(
                params.instance.clone(),
                driver,
                binary,
                home,
                environment,
                params.target_version.clone(),
            )
            .await
            .map_err(|error| Failure::new("provider_update_failed", error))
        }
        .await;
        let mut locks = resources.provider_update_locks.lock().await;
        for key in lock_keys {
            locks.remove(&key);
        }
        result
    }

    /// Codex and Claude as the composer offers them, with their models.
    async fn providers(&self) -> Vec<agent_protocol::models::ProviderInstance> {
        use agent_protocol::models::{ProviderInstance, ProviderStatus};
        let resources = &self.inner.resources;
        let settings = resources.shared.worktrees.latest_host_settings();
        let check_provider_updates = settings.enable_provider_update_checks;
        let instance = |driver: Driver, name: &str| ProviderInstance {
            instance: name.to_lowercase(),
            driver,
            display_name: name.into(),
            accent_color: None,
            enabled: true,
            installed: true,
            version: None,
            version_advisory: None,
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
        codex.version = resources
            .codex
            .server()
            .ok()
            .and_then(|server| agent_providers::cli_version(&server.initialize_response().user_agent));
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
                if check_provider_updates {
                    claude.message =
                        agent_providers::claude_upgrade_message(claude.version.as_deref());
                }
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
        let mut builtins = std::collections::BTreeMap::from([
            (codex.instance.clone(), codex),
            (claude.instance.clone(), claude),
        ]);
        let configured_instances = settings.provider_instances.clone();
        let mut custom = Vec::new();
        for (id, config) in settings.provider_instances {
            let Some(base) = builtins
                .get(&match config.driver {
                    Driver::Codex => "codex",
                    Driver::Claude => "claude",
                })
                .cloned()
            else {
                continue;
            };
            let mut provider = if let Some(existing) = builtins.remove(&id) {
                existing
            } else {
                let name = if config.display_name.trim().is_empty() {
                    base.display_name.as_str()
                } else {
                    config.display_name.as_str()
                };
                let mut provider = instance(config.driver, &id);
                provider.display_name = name.into();
                provider.version = base.version.clone();
                provider.status = base.status;
                provider.message = base.message.clone();
                provider.unavailable_reason = base.unavailable_reason.clone();
                provider.models = base.models.clone();
                provider
            };
            provider.display_name = if config.display_name.trim().is_empty() {
                base.display_name.clone()
            } else {
                config.display_name.clone()
            };
            provider.accent_color = config.accent_color.clone();
            provider.enabled = config.enabled;
            if !config.enabled {
                provider.status = ProviderStatus::Disabled;
                provider.message = Some("This provider instance is disabled.".into());
            } else if let Some(path) = config.binary_path.as_deref() {
                let path = crate::projects::expand_home(path);
                let available = provider_executable_available(&path);
                if !available {
                    provider.installed = false;
                    provider.status = ProviderStatus::Error;
                    provider.message = Some(format!(
                        "Provider executable was not found: {}",
                        path.display()
                    ));
                } else {
                    provider.installed = true;
                    provider.status = ProviderStatus::Ready;
                    provider.message = None;
                }
            }
            if config.driver == Driver::Codex && provider.installed {
                match custom_codex_models(&config).await {
                    Ok(models) => provider.models = models,
                    Err(error) => {
                        provider.status = ProviderStatus::Error;
                        provider.message = Some(error);
                    }
                }
            }
            merge_custom_models(&mut provider.models, config.custom_models);
            if provider.instance != id {
                provider.instance = id.clone();
            }
            if id == "codex" || id == "claude" {
                builtins.insert(id, provider);
            } else {
                custom.push(provider);
            }
        }
        let mut result: Vec<_> = builtins.into_values().collect();
        result.extend(custom);
        for provider in &mut result {
            if !provider.installed || !provider.enabled {
                continue;
            }
            let configured = configured_instances.get(&provider.instance);
            let binary = configured
                .and_then(|config| config.binary_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| match provider.driver {
                    Driver::Codex => Some(PathBuf::from("codex")),
                    Driver::Claude => resources
                        .claude
                        .get()
                        .map(|claude| claude.program().program),
                });
            let Some(binary) = binary else {
                continue;
            };
            let home = configured
                .and_then(|config| config.home_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| match provider.driver {
                    Driver::Codex => Some(resources.codex.directory.clone()),
                    Driver::Claude => resources
                        .claude
                        .get()
                        .map(|claude| claude.native_home.clone()),
                });
            let environment = configured
                .map(|config| {
                    config
                        .environment
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            provider.version_advisory = Some(
                crate::provider_maintenance::advisory(
                    provider.driver,
                    &binary,
                    home.as_deref(),
                    &environment,
                    provider.version.clone(),
                    check_provider_updates,
                )
                .await,
            );
        }
        result
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

fn provider_key(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
    .into()
}

async fn custom_codex_models(
    config: &agent_protocol::models::ProviderInstanceConfig,
) -> Result<Vec<agent_protocol::models::Model>, String> {
    let server = codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig {
        program: config
            .binary_path
            .as_deref()
            .map(crate::projects::expand_home)
            .unwrap_or_else(|| PathBuf::from("codex")),
        codex_home: config
            .home_path
            .as_deref()
            .map(crate::projects::expand_home),
        environment: config.environment.clone(),
        launch_args: config.launch_args.clone(),
        ..Default::default()
    })
    .await
    .map_err(|error| format!("configured Codex instance could not start: {error}"))?;
    let result = async {
        let mut native = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page: serde_json::Value = server
                .request(
                    "model/list",
                    &serde_json::json!({"limit": 100, "cursor": cursor}),
                )
                .await
                .map_err(|error| format!("configured Codex model list failed: {error}"))?
                .outcome
                .map_err(|error| error.get().to_owned())?;
            native.extend(
                page["data"]
                    .as_array()
                    .ok_or_else(|| "configured Codex model list is invalid".to_owned())?
                    .iter()
                    .cloned(),
            );
            let next = page["nextCursor"].as_str().map(str::to_owned);
            if next.is_none() {
                break;
            }
            if next == cursor {
                return Err("configured Codex model list repeated its cursor".into());
            }
            cursor = next;
        }
        agent_providers::codex_catalog(&native, false)
            .map_err(|error| format!("configured Codex models are invalid: {error}"))
            .map(|models| {
                models
                    .into_iter()
                    .map(super::resources::wire_model)
                    .collect()
            })
    }
    .await;
    let _ = server.shutdown().await;
    result
}

/// A bare command is resolved by the child process through `PATH`; a path
/// containing a directory must already name a file so the provider catalogue
/// can report a useful error before a thread is launched.
fn provider_executable_available(path: &Path) -> bool {
    path.parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
        || path.is_file()
}

/// Applies user-defined models after the live provider catalogue. A custom
/// model with the same slug intentionally replaces the live descriptor so a
/// saved display name or option schema is effective in every picker.
fn merge_custom_models(
    models: &mut Vec<agent_protocol::models::Model>,
    custom: Vec<agent_protocol::models::ProviderCustomModel>,
) {
    for model in custom {
        let model = agent_protocol::models::Model {
            slug: model.slug,
            name: model.name,
            aliases: model.aliases,
            badge: model.badge,
            is_default: model.is_default,
            is_legacy: model.is_legacy,
            option_descriptors: model.option_descriptors,
        };
        if model.is_default {
            for existing in models.iter_mut() {
                existing.is_default = false;
            }
        }
        if let Some(existing) = models
            .iter_mut()
            .find(|existing| existing.slug == model.slug)
        {
            *existing = model;
        } else {
            models.push(model);
        }
    }
}

#[cfg(test)]
mod provider_settings_tests {
    use super::{merge_custom_models, provider_executable_available};
    use agent_protocol::models::{Model, ProviderCustomModel};
    use std::path::Path;

    #[test]
    fn custom_models_replace_live_slugs_and_select_one_default() {
        let mut live = vec![
            Model {
                slug: "live".into(),
                name: "Live model".into(),
                aliases: vec![],
                badge: None,
                is_default: true,
                is_legacy: false,
                option_descriptors: vec![],
            },
            Model {
                slug: "other".into(),
                name: "Other model".into(),
                aliases: vec![],
                badge: None,
                is_default: false,
                is_legacy: false,
                option_descriptors: vec![],
            },
        ];
        merge_custom_models(
            &mut live,
            vec![ProviderCustomModel {
                slug: "custom".into(),
                name: "Custom model".into(),
                is_default: true,
                ..Default::default()
            }],
        );
        assert_eq!(live.len(), 3);
        assert_eq!(live.iter().filter(|model| model.is_default).count(), 1);
        assert_eq!(live[2].slug, "custom");
        assert!(live[2].is_default);

        merge_custom_models(
            &mut live,
            vec![ProviderCustomModel {
                slug: "live".into(),
                name: "Renamed live".into(),
                ..Default::default()
            }],
        );
        assert_eq!(
            live.iter().find(|model| model.slug == "live").unwrap().name,
            "Renamed live"
        );
        assert_eq!(live.iter().filter(|model| model.is_default).count(), 1);
    }

    #[test]
    fn provider_paths_use_path_lookup_only_for_bare_commands() {
        assert!(provider_executable_available(Path::new("codex")));
        assert!(!provider_executable_available(Path::new("./codex")));
        assert!(!provider_executable_available(Path::new("/missing/codex")));
    }
}
