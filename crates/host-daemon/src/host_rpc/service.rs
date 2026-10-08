//! Host RPCs. Conversations run in the conversation runtime; authenticated
//! connections own only delivery.
use super::{
    connections::{Connections, HostReply, HostSession, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::ProjectStore;
use crate::github::pulls::{GitHubPullRequestService, supports_github_host};
use crate::conversation::{
    ClaudeCredentials, Conversation, ConversationConfig, ProjectCatalog, ProviderPrograms,
    SharedResources, SupervisedSpawner, tools::ModelCatalog,
};
use agent_domain::{
    BackgroundKind, Command, DispatchMode, Driver, MessageAuthor, MessageId, Notification,
    NotificationOutcome, NotificationSource, PullRequestKey, PullRequestLink,
    PullRequestLinkSource, SendMessage,
    ThreadId, Timestamp,
};
use agent_protocol::{
    operations as op,
    pull_requests as pr,
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
    commands: super::commands::CommandCache,
    search: crate::workspace_search::WorkspaceSearch,
    keybindings: Arc<crate::keybindings::Keybindings>,
    pull_requests: Arc<GitHubPullRequestService>,
    pull_request_watch_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
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
        let pull_requests = Arc::new(GitHubPullRequestService::new(
            projects.path().with_file_name("pull-requests.sqlite"),
        )?);
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
            pull_requests,
            pull_request_watch_task: OnceLock::new(),
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
        self.start_pull_request_watch();
        Ok(())
    }

    fn start_pull_request_watch(&self) {
        if self.inner.resources.pull_request_watch_task.get().is_some() {
            return;
        }
        let Some(conversation) = self.inner.resources.conversation.get().cloned() else {
            return;
        };
        let service = self.inner.resources.pull_requests.clone();
        let projects = self.inner.resources.shared.projects.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(agent_runtime::DEFAULT_WATCH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let now = timestamp_now();
                // Resolve the checked-out branch for each persisted thread. A
                // successful empty result clears only the automatic source;
                // an unavailable CLI leaves the last result intact.
                let branch_threads = match conversation.runtime.store().thread_shells() {
                    Ok(threads) => threads,
                    Err(error) => {
                        tracing::warn!(error = %error, "pull request branch discovery failed");
                        vec![]
                    }
                };
                for shell in branch_threads {
                    let summary = shell.row.summary;
                    let Some(workspace) = summary.workspace.clone() else {
                        continue;
                    };
                    let Some(branch) = workspace.branch.as_deref().filter(|branch| !branch.trim().is_empty()) else {
                        continue;
                    };
                    let cwd = if workspace.cwd.trim().is_empty() {
                        let Some(project) = projects
                            .list()
                            .into_iter()
                            .find(|project| project.id.as_str() == summary.project.as_str())
                        else {
                            continue;
                        };
                        project.root
                    } else {
                        workspace.cwd.clone()
                    };
                    let discovery = service.discover(Path::new(&cwd), false).await;
                    let Some(host) = discovery.host.as_deref() else {
                        continue;
                    };
                    if !supports_github_host(Some(host)) {
                        continue;
                    }
                    let Ok(detected) = service
                        .branch_pull_request(
                            Path::new(&cwd),
                            &summary.project,
                            branch,
                            discovery.repository.as_deref(),
                            Some(host),
                        )
                        .await
                    else {
                        continue;
                    };
                    let current = summary
                        .pull_requests
                        .iter()
                        .find(|link| link.source == PullRequestLinkSource::Agent);
                    let same = match (current, detected.as_ref()) {
                        (None, None) => true,
                        (Some(current), Some(detected)) => {
                            current.key() == detected.key()
                                && current.url == detected.url
                                && current.snapshot.as_ref().and_then(|summary| summary.head_sha.as_deref())
                                    == detected.snapshot.as_ref().and_then(|summary| summary.head_sha.as_deref())
                        }
                        _ => false,
                    };
                    if same {
                        continue;
                    }
                    let Ok(mut links) = service.links.links(&summary.id) else {
                        continue;
                    };
                    links.retain(|link| link.source != PullRequestLinkSource::Agent);
                    if let Some(detected) = detected.clone() {
                        links.push(detected);
                    }
                    if service
                        .links
                        .sync_thread(&summary.id, &links, now.as_str())
                        .is_err()
                    {
                        continue;
                    }
                    let _ = conversation
                        .dispatch_host_command(
                            summary.id.clone(),
                            Command::ResolveBranchPullRequest { link: detected },
                        )
                        .await;
                }
                let watched = match service.links.watched_links() {
                    Ok(watched) => watched,
                    Err(error) => {
                        tracing::warn!(error = %error, "pull request watch store read failed");
                        continue;
                    }
                };
                for (thread, link) in watched {
                    if !agent_runtime::watch_is_due(
                        &link,
                        &now,
                        agent_runtime::DEFAULT_WATCH_INTERVAL,
                    ) {
                        continue;
                    }
                    let Some(project_id) = link
                        .snapshot
                        .as_ref()
                        .and_then(|summary| summary.project.clone())
                    else {
                        continue;
                    };
                    let Some(project) = projects
                        .list()
                        .into_iter()
                        .find(|project| project.id == project_id)
                    else {
                        continue;
                    };
                    let reference = pr::PullRequestRef {
                        project_id,
                        repository: link.repository.clone(),
                        number: link.number,
                        host: Some(link.host.clone()),
                        allow_stale: false,
                    };
                    let Ok(detail) = service
                        .get(Path::new(&project.root), &reference.project_id, &reference)
                        .await
                    else {
                        continue;
                    };
                    let (links, wake) = agent_runtime::merge_pull_request_detail(
                        std::slice::from_ref(&link),
                        &detail,
                        now.clone(),
                    );
                    let Some(updated) = links.into_iter().next() else {
                        continue;
                    };
                    let mut updated_links = match service.links.links(&thread) {
                        Ok(links) => links,
                        Err(error) => {
                            tracing::warn!(error = %error, "pull request watch store read failed");
                            continue;
                        }
                    };
                    updated_links.retain(|candidate| candidate.key() != updated.key());
                    updated_links.push(updated.clone());
                    if service
                        .links
                        .sync_thread(&thread, &updated_links, now.as_str())
                        .is_err()
                    {
                        continue;
                    }
                    let _ = conversation
                        .dispatch_host_command(
                            thread.clone(),
                            Command::SyncPullRequestLink {
                                link: updated.clone(),
                            },
                        )
                        .await;
                    if detail.summary.state == agent_domain::PullRequestState::Open {
                        if let Some(wake) = wake.filter(|wake| !wake.text.trim().is_empty()) {
                            let message_id = MessageId::new(format!(
                                "pull-request-watch:{}",
                                uuid::Uuid::new_v4()
                            ))
                            .expect("generated watch message ids are nonempty");
                            let notification = Notification {
                                source: NotificationSource::Native(BackgroundKind::Monitor),
                                child_thread: None,
                                outcome: if wake.failed {
                                    NotificationOutcome::Failed
                                } else {
                                    NotificationOutcome::Updated
                                },
                                summary: wake.detail.clone(),
                                detail: Some(wake.detail.clone()),
                            };
                            let _ = conversation
                                .dispatch_host_command(
                                    thread,
                                    Command::PullRequestWake {
                                        message: SendMessage {
                                            context: None,
                                            created_by: MessageAuthor::Agent,
                                            creation_source: "pull-request-watch".into(),
                                            id: message_id,
                                            text: wake.text,
                                            attachments: vec![],
                                            selection: None,
                                            mode: DispatchMode::QueueAfterActive,
                                            intent: None,
                                            source_plan: None,
                                            resolved_plan: None,
                                            continuation: None,
                                            title_seed: None,
                                        },
                                        notification,
                                    },
                                )
                                .await;
                        }
                    }
                }
            }
        });
        let _ = self
            .inner
            .resources
            .pull_request_watch_task
            .set(tokio_util::task::AbortOnDropHandle::new(task));
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
        let _workspace = if matches!(request, Call::CloneRepository(_)) {
            Some(resources.worktree_access.write().await)
        } else if matches!(
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
                Call::ListPullRequests(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    let root = self.project_root(&params.project_id)?;
                    let discovery = (params.repository.is_none() || params.host.is_none())
                        .then(|| resources.pull_requests.discover(&root, false));
                    let discovery = match discovery {
                        Some(discovery) => Some(discovery.await),
                        None => None,
                    };
                    let host = params
                        .host
                        .as_deref()
                        .or_else(|| discovery.as_ref().and_then(|discovery| discovery.host.as_deref()));
                    ensure_github_host(host)?;
                    let repository = params.repository.clone().or_else(|| {
                        discovery
                            .as_ref()
                            .and_then(|discovery| discovery.repository.clone())
                    });
                    let mut request = params.clone();
                    request.host = host.map(str::to_owned);
                    resources
                        .pull_requests
                        .list(
                            &root,
                            &request.project_id,
                            &request,
                            repository.as_deref(),
                            host,
                        )
                        .await
                        .map_err(|error| Failure::new("pull_request_list_failed", error))?
                        .into()
                }
                Call::GetPullRequest(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    resources
                        .pull_requests
                        .get(&root, &params.reference.project_id, &params.reference)
                        .await
                        .map_err(|error| Failure::new("pull_request_get_failed", error))?
                        .into()
                }
                Call::GetPullRequestDiff(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    resources
                        .pull_requests
                        .diff(&root, params)
                        .await
                        .map_err(|error| Failure::new("pull_request_diff_failed", error))?
                        .into()
                }
                Call::GetPullRequestFile(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    resources
                        .pull_requests
                        .file(&root, &params.reference, &params.path, params.max_bytes as usize)
                        .await
                        .map_err(|error| Failure::new("pull_request_file_failed", error))?
                        .into()
                }
                Call::GetPullRequestViewedFiles(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let viewed = resources
                        .pull_requests
                        .links
                        .viewed_files(&params.reference.key(), params.limit as usize)
                        .map_err(|error| Failure::new("pull_request_viewed_files_failed", error))?;
                    pr::PullRequestViewedFiles {
                        reference: params.reference.clone(),
                        files: viewed
                            .0
                            .into_iter()
                            .map(|(path, viewed)| pr::PullRequestViewedFile { path, viewed })
                            .collect(),
                        truncated: viewed.1,
                    }
                    .into()
                }
                Call::SetPullRequestFilesViewed(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let key = params.reference.key();
                    let files = params
                        .files
                        .iter()
                        .map(|file| (file.path.as_str(), file.viewed))
                        .collect::<Vec<_>>();
                    resources
                        .pull_requests
                        .links
                        .set_viewed_files(&key, &files, timestamp_now().as_str())
                        .map_err(|error| Failure::new("pull_request_viewed_files_failed", error))?;
                    let viewed = resources
                        .pull_requests
                        .links
                        .viewed_files(&key, 1_000)
                        .map_err(|error| Failure::new("pull_request_viewed_files_failed", error))?;
                    pr::PullRequestViewedFiles {
                        reference: params.reference.clone(),
                        files: viewed
                            .0
                            .into_iter()
                            .map(|(path, viewed)| pr::PullRequestViewedFile { path, viewed })
                            .collect(),
                        truncated: viewed.1,
                    }
                    .into()
                }
                Call::LinkPullRequest(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(Some(&params.host))?;
                    self.link_pull_request(params).await?.into()
                }
                Call::UnlinkPullRequest(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(Some(&params.host))?;
                    self.unlink_pull_request(params).await?.into()
                }
                Call::SetPullRequestWatch(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(Some(&params.link.host))?;
                    self.set_pull_request_watch(params).await?.into()
                }
                Call::PullRequestAction(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    if let Some(stack_number) = params.stack_number {
                        resources
                            .pull_requests
                            .stack_action(
                                &root,
                                &params.reference,
                                params.action,
                                stack_number,
                                params.expected_stack_heads.as_deref().unwrap_or(&[]),
                                params.merge_method,
                            )
                            .await
                            .map_err(|error| Failure::new("pull_request_stack_action_failed", error))?;
                    } else {
                        resources
                            .pull_requests
                            .action(
                                &root,
                                &params.reference,
                                params.action,
                                params.merge_method,
                            )
                            .await
                            .map_err(|error| Failure::new("pull_request_action_failed", error))?;
                    }
                    let detail = resources
                        .pull_requests
                        .get(&root, &params.reference.project_id, &params.reference)
                        .await
                        .ok();
                    pr::PullRequestOperation {
                        reference: params.reference.clone(),
                        detail,
                        linked: vec![],
                    }
                    .into()
                }
                Call::SubmitPullRequestReview(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    resources
                        .pull_requests
                        .review(&root, &params.reference, params.verdict, &params.body)
                        .await
                        .map_err(|error| Failure::new("pull_request_review_failed", error))?;
                    let detail = resources
                        .pull_requests
                        .get(&root, &params.reference.project_id, &params.reference)
                        .await
                        .ok();
                    pr::PullRequestOperation {
                        reference: params.reference.clone(),
                        detail,
                        linked: vec![],
                    }
                    .into()
                }
                Call::SourceControlAuth(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    resources
                        .pull_requests
                        .auth(
                            Path::new(params.cwd.as_deref().unwrap_or(".")),
                            params.host.as_deref(),
                            params.fresh,
                        )
                        .await
                        .into()
                }
                Call::SourceControlDiscovery(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    resources
                        .pull_requests
                        .discover(Path::new(&params.cwd), params.fresh)
                        .await
                        .into()
                }
                Call::CloneRepository(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    resources
                        .pull_requests
                        .clone_repository(params)
                        .await
                        .map_err(|error| Failure::new("repository_clone_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
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
    fn project_root(&self, project_id: &str) -> Result<PathBuf, Failure> {
        self.inner
            .resources
            .shared
            .projects
            .list()
            .into_iter()
            .find(|project| project.id == project_id)
            .map(|project| PathBuf::from(project.root))
            .ok_or_else(|| Failure::new("project_not_found", project_id))
    }

    async fn link_pull_request(
        &self,
        params: &pr::LinkPullRequest,
    ) -> Result<pr::PullRequestOperation, Failure> {
        let resources = &self.inner.resources;
        let root = self.project_root(&params.project_id)?;
        let reference = pr::PullRequestRef {
            project_id: params.project_id.clone(),
            repository: params.repository.clone(),
            number: params.number,
            host: Some(params.host.clone()),
            allow_stale: false,
        };
        let detail = resources
            .pull_requests
            .get(&root, &params.project_id, &reference)
            .await
            .map_err(|error| Failure::new("pull_request_get_failed", error))?;
        let now = timestamp_now();
        let link = PullRequestLink {
            host: params.host.clone(),
            repository: params.repository.clone(),
            number: params.number,
            url: params.url.clone(),
            source: params.source,
            linked_at: now.clone(),
            snapshot: Some(detail.summary.clone()),
            stack: detail.summary.stack.clone(),
            watch: None,
        };
        let thread = ThreadId::new(params.thread_id.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        let mut links = resources
            .pull_requests
            .links
            .links(&thread)
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        let existing_keys = links
            .iter()
            .map(|candidate| candidate.key().canonical())
            .collect::<std::collections::BTreeSet<_>>();
        links.retain(|candidate| candidate.key() != link.key());
        links.push(link.clone());
        if let Some(stack) = detail.summary.stack.as_ref() {
            for layer in &stack.layers {
                let layer_key = PullRequestKey::new(
                    &params.host,
                    &params.repository,
                    layer.number,
                );
                if layer.number == params.number
                    || links.iter().any(|candidate| candidate.key() == layer_key)
                {
                    continue;
                }
                links.push(PullRequestLink {
                    host: params.host.clone(),
                    repository: params.repository.clone(),
                    number: layer.number,
                    url: agent_domain::github_browser_url(
                        &params.host,
                        &params.repository,
                        layer.number,
                    ),
                    source: agent_domain::PullRequestLinkSource::Stack,
                    linked_at: now.clone(),
                    snapshot: None,
                    stack: Some(stack.clone()),
                    watch: None,
                });
            }
        }
        resources
            .pull_requests
            .links
            .sync_thread(&thread, &links, now.as_str())
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        let conversation = self.conversation()?;
        for linked in links.iter().filter(|candidate| {
            candidate.key() == link.key()
                || (candidate.source == agent_domain::PullRequestLinkSource::Stack
                    && !existing_keys.contains(&candidate.key().canonical()))
        }) {
            conversation
                .dispatch_host_command(
                    thread.clone(),
                    Command::LinkPullRequest {
                        link: linked.clone(),
                    },
                )
                .await
                .map_err(|error| Failure::new("pull_request_link_failed", error))?;
        }
        Ok(pr::PullRequestOperation {
            reference,
            detail: Some(detail),
            linked: links,
        })
    }

    async fn unlink_pull_request(
        &self,
        params: &pr::UnlinkPullRequest,
    ) -> Result<pr::PullRequestOperation, Failure> {
        let resources = &self.inner.resources;
        let thread = ThreadId::new(params.thread_id.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        let key = params.key();
        let mut links = resources
            .pull_requests
            .links
            .links(&thread)
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        links.retain(|candidate| candidate.key() != key);
        let now = timestamp_now();
        resources
            .pull_requests
            .links
            .sync_thread(&thread, &links, now.as_str())
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        self.conversation()?
            .dispatch_host_command(thread, Command::UnlinkPullRequest { key: key.clone() })
            .await
            .map_err(|error| Failure::new("pull_request_unlink_failed", error))?;
        Ok(pr::PullRequestOperation {
            reference: pr::PullRequestRef {
                project_id: params.project_id.clone(),
                repository: key.repository,
                number: key.number,
                host: Some(key.host),
                allow_stale: false,
            },
            detail: None,
            linked: links,
        })
    }

    async fn set_pull_request_watch(
        &self,
        params: &pr::SetPullRequestWatch,
    ) -> Result<pr::PullRequestOperation, Failure> {
        let resources = &self.inner.resources;
        let thread = ThreadId::new(params.thread_id.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        let key = params.link.key();
        let now = timestamp_now();
        let mut links = resources
            .pull_requests
            .links
            .links(&thread)
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        let link = links.iter_mut().find(|candidate| candidate.key() == key);
        let Some(link) = link else {
            return Err(Failure::new("pull_request_not_linked", key.canonical()));
        };
        let head_sha = link
            .snapshot
            .as_ref()
            .and_then(|summary| summary.head_sha.clone());
        link.watch = params
            .enabled
            .then(|| agent_runtime::start_watch(now.clone(), head_sha));
        resources
            .pull_requests
            .links
            .set_watch(&thread, &key, link.watch.as_ref(), now.as_str())
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        resources
            .pull_requests
            .links
            .sync_thread(&thread, &links, now.as_str())
            .map_err(|error| Failure::new("pull_request_store_failed", error))?;
        self.conversation()
            .map_err(|error| Failure::new("conversation_unavailable", error))?
            .dispatch_host_command(
                thread,
                Command::SetPullRequestWatch {
                    key: key.clone(),
                    watch: links
                        .iter()
                        .find(|candidate| candidate.key() == key)
                        .and_then(|candidate| candidate.watch.clone()),
                },
            )
            .await
            .map_err(|error| Failure::new("pull_request_watch_failed", error))?;
        Ok(pr::PullRequestOperation {
            reference: pr::PullRequestRef {
                project_id: params.project_id.clone(),
                repository: key.repository,
                number: key.number,
                host: Some(key.host),
                allow_stale: false,
            },
            detail: None,
            linked: links,
        })
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

fn provider_key(provider: ProviderKind) -> String {
    match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
    .into()
}

fn timestamp_now() -> Timestamp {
    Timestamp::from_millis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64),
    )
    .expect("the system clock is within the supported timestamp range")
}

fn ensure_github_host(host: Option<&str>) -> Result<(), Failure> {
    if supports_github_host(host) {
        Ok(())
    } else {
        Err(Failure::new(
            "pull_request_provider_unsupported",
            "Pull request operations are supported only for GitHub repositories.",
        ))
    }
}
