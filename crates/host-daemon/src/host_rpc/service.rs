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
    conversation: OnceLock<Arc<Conversation>>,
    shared: SharedResources,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    dictation: crate::dictation::Dictation,
    auth_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
}

/// The live model catalog for the agent tools, without keeping the service alive.
struct ServiceModels(Weak<ServiceInner>);
impl ModelCatalog for ServiceModels {
    fn models(&self) -> BoxFuture<'_, Result<Vec<agent_protocol::models::Model>, String>> {
        Box::pin(async move {
            let inner = self.0.upgrade().ok_or("the Host is shutting down")?;
            HostRpcService { inner }.live_models().await
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
        let shared = SharedResources {
            files: crate::workspace_files::WorkspaceFiles::new(
                projects.path().with_file_name("attachments"),
            ),
            worktrees: Arc::new(crate::worktrees::Worktrees::new(projects.path())),
            projects: Arc::new(ProjectCatalog::new(projects)),
            checkpoints: Arc::default(),
            terminals: Arc::default(),
        };
        let resources = Arc::new(HostResources {
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
        });
        Ok(Self {
            inner: Arc::new(ServiceInner {
                resources,
                connections: Connections::new(),
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
        Ok(Response::from_result(self.request(session, call).await).into())
    }
    async fn request(&self, session: SessionId, request: &Call) -> Result<Body, Failure> {
        let resources = &self.inner.resources;
        let _workspace = if matches!(
            request,
            Call::StartTerminal(_)
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
                        .register(Path::new(&params.cwd))
                        .await
                        .map_err(|error| Failure::new("project_add_failed", error))?;
                    if let Ok(conversation) = self.conversation() {
                        conversation.project_added(&id).await;
                    }
                    id.into()
                }
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
                | Call::DetachTerminal(_) => (resources
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
            // T3 closes an instance's sessions when it signs out.
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
        Ok(self
            .inner
            .resources
            .shared
            .projects
            .refresh()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))?
            .into_iter()
            .map(crate::conversation::wire_project)
            .collect())
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
    /// Every page of the live model catalog.
    async fn live_models(&self) -> Result<Vec<agent_protocol::models::Model>, String> {
        let mut cursor = None;
        let mut result = vec![];
        loop {
            let page = self
                .models(&op::ListModels {
                    limit: 100,
                    cursor: cursor.clone(),
                })
                .await
                .map_err(|e| e.message)?;
            result.extend(page.data);
            if page.next_cursor.is_none() {
                break;
            }
            if cursor == page.next_cursor {
                return Err("Model catalog repeated its cursor".into());
            }
            cursor = page.next_cursor;
        }
        Ok(result)
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
                    id: legacy_thread_id(&shell.id)?,
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
                let thread = agent_domain::ThreadId::new(thread.id.as_str())
                    .map_err(|error| Failure::new("worktree_remove_failed", error))?;
                conversation.runtime.sessions().detach(&thread, true).await;
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

/// The worktree list still names threads with the previous wire identity.
fn legacy_thread_id(thread: &agent_domain::ThreadId) -> Result<orchestration::ThreadId, Failure> {
    orchestration::ThreadId::new(thread.as_str())
        .map_err(|error| Failure::new("worktree_list_failed", error))
}

fn provider_key(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
    .into()
}
