//! Host RPCs. Conversations run in the conversation runtime; authenticated
//! connections own only delivery.
use super::{
    connections::{Connections, HostReply, HostSession, SessionId},
    identity::Identity,
    resources::{ClaudeResources, CodexResources},
};
use crate::background::BackgroundOwner;
use crate::ProjectStore;
use crate::github::pulls::{GitHubPullRequestService, supports_github_host};
use crate::claude::control::ClaudeProgram;
use crate::conversation::{
    ClaudeCredentials, Conversation, ConversationConfig, ProjectCatalog, ProviderPrograms,
    SharedResources, SupervisedSpawner, TextGenerator, tools::ModelCatalog,
};
use agent_domain::{
    BackgroundKind, Command, DispatchMode, Driver, MessageAuthor, MessageId, Notification,
    NotificationOutcome, NotificationSource, PullRequestKey, PullRequestLink,
    PullRequestLinkSource, SendMessage,
    ThreadId, Timestamp, background_work_due,
};
use agent_domain::{RunStatus, RuntimeMode};
use agent_protocol::{
    models::{
        AgentActivityPhase, AwarenessActivity, AwarenessRegistration, AwarenessRegistrationResult,
        AwarenessSnapshot, EnvironmentDescriptor, UpdateTarget,
    },
    operations as op,
    pull_requests as pr,
    protocol::{Body, Call, Response},
    provider::ProviderKind,
    scheduled_tasks as st,
};
use agent_runtime::{
    ImportHome, ImportSettings, OsFs, RuntimeConfig, ScanConfig, ScheduledTaskError,
    ScheduledTaskInput, SetupProgress, SetupRequest, SetupRun, WorkspaceStrategy,
};
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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    awareness: AwarenessRegistry,
    updater: crate::UpdateManager,
    started: AtomicBool,
    handoff_draining: AtomicBool,
}

#[derive(Default)]
struct AwarenessRegistry {
    registrations: std::sync::Mutex<HashMap<SessionId, AwarenessRegistration>>,
}
struct HostResources {
    codex: Arc<CodexResources>,
    claude: OnceLock<Arc<ClaudeResources>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    preview: Arc<crate::preview::PreviewManager>,
    preview_ports: Arc<crate::preview::PortScanner>,
    conversation: OnceLock<Arc<Conversation>>,
    text: OnceLock<TextGenerator>,
    vcs: crate::vcs::VcsStatusBroadcaster,
    source_control_auto_fetch_interval_seconds: Arc<AtomicU64>,
    shared: SharedResources,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    dictation: crate::dictation::Dictation,
    auth_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    commands: super::commands::CommandCache,
    search: crate::workspace_search::WorkspaceSearch,
    keybindings: Arc<crate::keybindings::Keybindings>,
    devices: Arc<crate::device::DeviceService>,
    usage: crate::usage::UsageService,
    pull_requests: Arc<GitHubPullRequestService>,
    pull_request_watch_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    background: Arc<BackgroundOwner>,
    background_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    background_stop: tokio_util::sync::CancellationToken,
    background_consumers_task: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    provider_cache: tokio::sync::RwLock<Option<ProviderHealthCache>>,
    provider_refresh: tokio::sync::Mutex<()>,
}

struct ProviderHealthCache {
    refreshed_at: Timestamp,
    providers: Vec<agent_protocol::models::ProviderInstance>,
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
    fn scheduled_task(task: agent_runtime::ScheduledTask) -> st::ScheduledTask {
        st::ScheduledTask {
            id: task.id,
            title: task.title,
            prompt: task.prompt,
            enabled: task.enabled,
            schedule: task.schedule,
            project_id: task.project,
            thread_id: task.thread,
            workspace: Self::wire_workspace(task.workspace),
            selection: task.selection,
            runtime_mode: task.runtime_mode,
            interaction_mode: task.interaction_mode,
            created_by: task.created_by,
            creation_source: task.creation_source,
            created_at: task.created_at,
            updated_at: task.updated_at,
            next_run_at: task.next_run_at,
            last_run_at: task.last_run_at,
            last_run_status: match task.last_run_status {
                agent_runtime::ScheduledTaskRunStatus::Never => st::ScheduledTaskRunStatus::Never,
                agent_runtime::ScheduledTaskRunStatus::Running => st::ScheduledTaskRunStatus::Running,
                agent_runtime::ScheduledTaskRunStatus::Succeeded => st::ScheduledTaskRunStatus::Succeeded,
                agent_runtime::ScheduledTaskRunStatus::Failed => st::ScheduledTaskRunStatus::Failed,
            },
            last_run_error: task.last_run_error,
            run_count: task.run_count,
        }
    }

    fn runtime_workspace(workspace: &agent_protocol::conversation::WorkspaceStrategy) -> WorkspaceStrategy {
        match workspace {
            agent_protocol::conversation::WorkspaceStrategy::Root { branch } => {
                WorkspaceStrategy::Root { branch: branch.clone() }
            }
            agent_protocol::conversation::WorkspaceStrategy::ExistingWorktree {
                worktree_path,
                branch,
            } => WorkspaceStrategy::ExistingWorktree {
                path: worktree_path.clone(),
                branch: branch.clone(),
            },
            agent_protocol::conversation::WorkspaceStrategy::Worktree {
                base_ref,
                branch,
                start_from_origin,
            } => WorkspaceStrategy::Worktree {
                base_ref: base_ref.clone(),
                branch: branch.clone(),
                start_from_origin: *start_from_origin,
            },
        }
    }

    fn wire_workspace(workspace: WorkspaceStrategy) -> agent_protocol::conversation::WorkspaceStrategy {
        match workspace {
            WorkspaceStrategy::Root { branch } => {
                agent_protocol::conversation::WorkspaceStrategy::Root { branch }
            }
            WorkspaceStrategy::ExistingWorktree { path, branch } => {
                agent_protocol::conversation::WorkspaceStrategy::ExistingWorktree {
                    worktree_path: path,
                    branch,
                }
            }
            WorkspaceStrategy::Worktree {
                base_ref,
                branch,
                start_from_origin,
            } => agent_protocol::conversation::WorkspaceStrategy::Worktree {
                base_ref,
                branch,
                start_from_origin,
            },
        }
    }

    fn scheduled_input(params: &st::UpsertScheduledTask) -> ScheduledTaskInput {
        ScheduledTaskInput {
            id: params.id.clone(),
            require_existing: params.require_existing,
            command_id: params.command_id.clone(),
            title: params.title.clone(),
            prompt: params.prompt.clone(),
            enabled: params.enabled,
            schedule: params.schedule.clone(),
            project: params.project_id.clone(),
            thread: params.thread_id.clone(),
            workspace: Self::runtime_workspace(&params.workspace),
            selection: params.selection.clone(),
            runtime_mode: params.runtime_mode,
            interaction_mode: params.interaction_mode,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: params.creation_source.clone(),
        }
    }

    fn scheduled_failure(error: ScheduledTaskError) -> Failure {
        match error {
            ScheduledTaskError::NotFound(id) => {
                Failure::new("scheduled_task_not_found", format!("Schedule task {id} not found."))
            }
            ScheduledTaskError::AlreadyRunning(id) => Failure::new(
                "scheduled_task_already_running",
                format!("Schedule task {id} is already running."),
            ),
            ScheduledTaskError::Store(error) => Failure::new("scheduled_task_failed", error),
        }
    }

    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        projects: ProjectStore,
    ) -> anyhow::Result<Self> {
        let update_dir = projects
            .path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Host project state path has no parent"))?
            .to_owned();
        let connections = Connections::new();
        let state_path = projects.path().to_owned();
        let terminal_history = projects.path().with_file_name("terminals");
        let keybindings = Arc::new(crate::keybindings::Keybindings::new(
            projects.path().with_file_name("keybindings.json"),
        ));
        let devices = crate::device::DeviceService::new(
            projects.path().with_file_name("device"),
        );
        let pull_requests = Arc::new(GitHubPullRequestService::new(
            projects.path().with_file_name("pull-requests.sqlite"),
        )?);
        let state_directory = projects
            .path()
            .parent()
            .map(Path::to_owned)
            .unwrap_or_else(|| PathBuf::from("."));
        let background = BackgroundOwner::new(state_directory);
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
        let source_control_auto_fetch_interval_seconds = Arc::new(AtomicU64::new(30));
        let interval = source_control_auto_fetch_interval_seconds.clone();
        let resources = Arc::new(HostResources {
            codex: Arc::new(CodexResources::new(codex.clone())),
            claude: OnceLock::new(),
            startup_errors: Default::default(),
            browser: OnceLock::new(),
            preview: Arc::new(crate::preview::PreviewManager::new()),
            preview_ports: crate::preview::PortScanner::new(),
            conversation: OnceLock::new(),
            text: OnceLock::new(),
            vcs: crate::vcs::VcsStatusBroadcaster::new(
                crate::github::cli::GitHubCli::locate(),
                Arc::new(move || {
                    Duration::from_secs(interval.load(Ordering::Acquire))
                }),
            ),
            source_control_auto_fetch_interval_seconds,
            shared,
            worktree_access: Default::default(),
            permission_settings_access: Default::default(),
            dictation: crate::dictation::Dictation::new(codex),
            auth_task: OnceLock::new(),
            commands: Default::default(),
            search: Default::default(),
            keybindings,
            usage: crate::usage::UsageService::new(&state_path),
            pull_requests,
            pull_request_watch_task: OnceLock::new(),
            background,
            background_task: OnceLock::new(),
            background_stop: tokio_util::sync::CancellationToken::new(),
            background_consumers_task: OnceLock::new(),
            provider_cache: tokio::sync::RwLock::new(None),
            provider_refresh: tokio::sync::Mutex::new(()),
            devices,
        });
        Ok(Self {
            inner: Arc::new(ServiceInner {
                resources,
                connections,
                awareness: AwarenessRegistry::default(),
                updater: crate::UpdateManager::new(update_dir),
                started: AtomicBool::new(false),
                handoff_draining: AtomicBool::new(false),
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
        resources.shared.worktrees.host_settings(None).await?;
        let codex = codex_app_server::resolve_executable(&settings.codex).ok();
        let claude = resources.claude.get().map(|claude| {
            (
                claude.program(),
                claude.clone() as Arc<dyn ClaudeCredentials>,
            )
        });
        let text = TextGenerator {
            codex: codex.clone(),
            codex_home: settings.codex_home.clone(),
            claude: claude.clone(),
            worktrees: Some(resources.shared.worktrees.clone()),
        };
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
                background: resources.background.clone(),
            },
            resources.shared.clone(),
        )
        .await?;
        resources
            .conversation
            .set(conversation)
            .map_err(|_| anyhow::anyhow!("the conversation runtime is already open"))?;
        resources
            .text
            .set(text)
            .map_err(|_| anyhow::anyhow!("the text generator is already configured"))
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
        let _ = self.inner.resources.background_task.set(
            self.inner
                .resources
                .background
                .spawn(self.inner.resources.background_stop.clone()),
        );
        let _ = self.inner.resources.background_consumers_task.set(
            self.spawn_background_consumers(),
        );
        if let Err(error) = self.inner.updater.acknowledge_current(UpdateTarget::Host) {
            tracing::warn!(
                target: "bex",
                operation = "host.update.acknowledge",
                message = %format_args!("{error:#}")
            );
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

    /// A handoff is safe only after conversation runs and terminal
    /// subprocesses have settled. The Host owns this decision because a
    /// Desktop process cannot observe provider work in another process.
    pub(crate) fn has_active_tasks(&self) -> bool {
        if let Some(conversation) = self.inner.resources.conversation.get() {
            let threads = match conversation.runtime.store().thread_shells() {
                Ok(threads) => threads,
                Err(error) => {
                    tracing::warn!(
                        target: "bex",
                        operation = "host.update.handoff",
                        message = %format_args!("cannot inspect active tasks: {error:#}")
                    );
                    return true;
                }
            };
            if threads.into_iter().any(|thread| {
                thread.row.summary.active_run.is_some()
                    || !thread.row.summary.pending_background_work.is_empty()
            }) {
                return true;
            }
        }
        self.inner
            .resources
            .shared
            .terminals
            .summaries_now()
            .into_iter()
            .any(|terminal| {
                terminal.status == agent_protocol::operations::TerminalStatus::Starting
                    || terminal.has_running_subprocess
            })
    }

    pub(crate) async fn accept_handoff_if_idle(&self) -> anyhow::Result<bool> {
        if self.has_active_tasks() {
            return Ok(false);
        }
        if self
            .inner
            .handoff_draining
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(false);
        }
        if self.has_active_tasks() {
            self.inner.handoff_draining.store(false, Ordering::Release);
            return Ok(false);
        }
        match self.inner.updater.accept_handoff_if_ready().await {
            Ok(accepted) => {
                if !accepted {
                    self.inner.handoff_draining.store(false, Ordering::Release);
                }
                Ok(accepted)
            }
            Err(error) => {
                self.inner.handoff_draining.store(false, Ordering::Release);
                Err(error)
            }
        }
    }
    pub fn open_session(&self) -> HostSession {
        self.inner.connections.open_session()
    }
    pub(crate) fn open_authenticated_session(&self, principal: String) -> HostSession {
        self.inner
            .connections
            .open_authenticated_session(Some(principal))
    }
    pub(crate) fn cancellation(
        &self,
        session: SessionId,
    ) -> Result<tokio_util::sync::CancellationToken, String> {
        self.inner.connections.cancellation(session)
    }
    pub fn close_session(&self, session: SessionId) {
        self.inner.connections.close_session(session);
        self.inner
            .awareness
            .registrations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&session);
        self.inner.resources.shared.terminals.close_session(session);
        self.inner.resources.shared.files.clear_session(session);
        self.inner.resources.dictation.close_session(session);
        let background = self.inner.resources.background.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { background.close_session(session).await });
        }
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
        self.inner.resources.background_stop.cancel();
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
        if self.inner.handoff_draining.load(Ordering::Acquire) {
            return Err("Host is waiting for its installed update to start".into());
        }
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
        if let Call::DeviceSubscribe(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.device_subscribe(params, cancel).await);
        }
        if let Call::SubscribeVcsStatus(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.vcs_status(params, cancel).await);
        }
        if let Call::RunStackedAction(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.stacked_action(params, cancel));
        }
        if let Call::SubscribeScheduledTasks(_) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.scheduled_tasks(cancel).await);
        }
        if let Call::PreviewSubscribe(params) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.preview_subscribe(params, cancel).await);
        }
        if let Call::SubscribeBackground(_) = call {
            let cancel = self.inner.connections.cancellation(session)?;
            return Ok(self.background_stream(cancel).await);
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
        let receiver = Arc::new(tokio::sync::Mutex::new(devices.subscribe()));
        let initial = agent_protocol::device::DeviceEvent::State(devices.state_async().await);
        crate::conversation::stream(
            std::collections::VecDeque::from([initial]),
            agent_protocol::device::DeviceEvent::State(
                agent_protocol::device::DeviceServiceState::default(),
            ),
            move || {
                let (receiver, devices, thread) =
                    (receiver.clone(), devices.clone(), thread.clone());
                Box::pin(async move {
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
                            Ok(Ok(event)) => return Some(vec![event]),
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                                return Some(vec![agent_protocol::device::DeviceEvent::State(
                                    devices.state_async().await,
                                )]);
                            }
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return None,
                            Err(_) => {
                                let frames = devices.frames_for_thread(&thread, prefer_mjpeg).await;
                                if !frames.is_empty() {
                                    return Some(frames);
                                }
                            }
                        }
                    }
                })
            },
            cancel,
        )
    }

    /// The current policy snapshot, followed by semantic power or lease
    /// changes. A lagging subscriber receives a fresh snapshot.
    async fn background_stream(&self, cancel: tokio_util::sync::CancellationToken) -> HostReply {
        let background = self.inner.resources.background.clone();
        let (receiver, first) = background.subscribe_with_snapshot().await;
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        let empty = first.clone();
        crate::conversation::stream(
            std::collections::VecDeque::from([first]),
            empty,
            move || {
                let (receiver, background) = (receiver.clone(), background.clone());
                Box::pin(async move {
                    let mut receiver = receiver.lock().await;
                    match receiver.recv().await {
                        Ok(snapshot) => Some(vec![snapshot]),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            *receiver = background.subscribe();
                            Some(vec![background.snapshot().await])
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                    }
                })
            },
            cancel,
        )
    }

    async fn vcs_status(
        &self,
        params: &agent_protocol::vcs::SubscribeVcsStatus,
        cancel: tokio_util::sync::CancellationToken,
    ) -> HostReply {
        let broadcaster = self.inner.resources.vcs.clone();
        let (first, subscription) = match broadcaster.subscribe(&params.cwd).await {
            Ok(value) => value,
            Err(error) => return Response::error("vcs_status_failed", &format!("{error:#}")).into(),
        };
        let cwd = subscription.cwd().to_owned();
        let subscription = Arc::new(tokio::sync::Mutex::new(subscription));
        crate::conversation::stream(
            std::collections::VecDeque::from([first]),
            agent_protocol::vcs::VcsStatusStreamEvent::Snapshot {
                local: agent_protocol::vcs::VcsStatusLocal::not_repository(),
                remote: None,
            },
            move || {
                let (subscription, broadcaster, cwd) =
                    (subscription.clone(), broadcaster.clone(), cwd.clone());
                Box::pin(async move {
                    loop {
                        let result = {
                            let mut subscription = subscription.lock().await;
                            subscription.receiver.recv().await
                        };
                        match result {
                            Ok(change) if change.cwd == cwd => return Some(vec![change.event]),
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                return broadcaster.snapshot_event(&cwd).await.ok().map(|event| vec![event]);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                        }
                    }
                })
            },
            cancel,
        )
    }

    pub(crate) fn register_awareness(
        &self,
        session: SessionId,
        registration: AwarenessRegistration,
    ) -> Result<AwarenessRegistrationResult, Failure> {
        // HostRuntime reaches this method only after iroh authorization. Keep
        // the session principal check here as well so the registry cannot be
        // used through an unauthenticated service handle.
        self.inner
            .connections
            .principal(session)
            .map_err(|error| Failure::new("connection_closed", error))?;
        if registration.device_id.trim().is_empty() {
            return Err(Failure::new(
                "invalid_awareness",
                "device id must not be empty",
            ));
        }
        if registration.label.trim().is_empty() {
            return Err(Failure::new(
                "invalid_awareness",
                "device label must not be empty",
            ));
        }
        if registration.platform.trim().is_empty() {
            return Err(Failure::new(
                "invalid_awareness",
                "device platform must not be empty",
            ));
        }
        self.inner
            .awareness
            .registrations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session, registration);
        Ok(AwarenessRegistrationResult {
            accepted: true,
            registered_at_ms: epoch_ms(),
        })
    }

    pub(crate) fn awareness(
        &self,
        descriptor: EnvironmentDescriptor,
        cancel: tokio_util::sync::CancellationToken,
    ) -> HostReply {
        let first = self.awareness_snapshot(descriptor.clone());
        let service = self.clone();
        let previous = Arc::new(std::sync::Mutex::new(first.clone()));
        crate::conversation::stream(
            std::collections::VecDeque::from([first.clone()]),
            first,
            move || {
                let service = service.clone();
                let descriptor = descriptor.clone();
                let previous = previous.clone();
                Box::pin(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let next = service.awareness_snapshot(descriptor.clone());
                        let changed = {
                            let mut last =
                                previous.lock().unwrap_or_else(|error| error.into_inner());
                            if *last == next {
                                false
                            } else {
                                *last = next.clone();
                                true
                            }
                        };
                        if changed {
                            return Some(vec![next]);
                        }
                    }
                })
            },
            cancel,
        )
    }

    fn stacked_action(
        &self,
        params: &agent_protocol::vcs::RunStackedAction,
        cancel: tokio_util::sync::CancellationToken,
    ) -> HostReply {
        let resources = &self.inner.resources;
        let (first, receiver) = crate::vcs::start_action(
            params.clone(),
            resources.vcs.github().cloned(),
            resources.text.get().cloned(),
            resources.vcs.clone(),
        );
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        let empty = agent_protocol::vcs::ActionProgressEvent {
            action_id: params.action_id.clone(),
            cwd: params.cwd.clone(),
            action: params.action,
            kind: agent_protocol::vcs::ActionProgressKind::ActionFailed {
                phase: None,
                message: "Git action stream ended.".into(),
            },
        };
        crate::conversation::stream(
            std::collections::VecDeque::from([first]),
            empty,
            move || {
                let receiver = receiver.clone();
                Box::pin(async move {
                    let mut receiver = receiver.lock().await;
                    receiver.recv().await.map(|event| vec![event])
                })
            },
            cancel,
        )
    }

    fn awareness_snapshot(&self, descriptor: EnvironmentDescriptor) -> AwarenessSnapshot {
        let projects = self
            .inner
            .resources
            .shared
            .projects
            .list()
            .into_iter()
            .map(|project| (project.id, project.name))
            .collect::<HashMap<_, _>>();
        let activities = self
            .inner
            .resources
            .conversation
            .get()
            .and_then(|conversation| conversation.runtime.store().thread_shells().ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|thread| {
                let summary = thread.row.summary;
                let status = summary.activity_run_status.or(summary.status);
                let waiting_request = summary.pending_request.as_ref();
                let waiting_background = summary.pending_background_work.first();
                let live = summary.active_run.is_some()
                    || status.is_some_and(RunStatus::blocking)
                    || waiting_request.is_some()
                    || waiting_background.is_some();
                if !live {
                    return None;
                }
                let phase = activity_phase(status, waiting_request.is_some());
                let detail = waiting_request
                    .map(|request| format!("Waiting for {}", request.kind))
                    .or_else(|| waiting_background.map(|work| work.description.clone()))
                    .or_else(|| summary.last_error.clone());
                let updated_at_ms = summary.updated_at.millis();
                Some(AwarenessActivity {
                    environment_id: descriptor.environment_id.clone(),
                    thread_id: summary.id.to_string(),
                    project_title: projects
                        .get(&thread.row.project)
                        .cloned()
                        .unwrap_or(thread.row.project),
                    thread_title: summary.title.clone(),
                    phase,
                    headline: activity_headline(status, waiting_request.is_some()),
                    detail,
                    model_title: (!summary.selection.model.is_empty())
                        .then_some(summary.selection.model.clone()),
                    updated_at_ms,
                })
            })
            .collect::<Vec<_>>();
        let updated_at_ms = activities
            .iter()
            .map(|activity| activity.updated_at_ms)
            .max()
            .unwrap_or(0);
        AwarenessSnapshot {
            environment: descriptor,
            activities,
            updated_at_ms,
        }
    }

    pub(crate) async fn update(&self, call: &Call) -> Result<Body, Failure> {
        let updater = &self.inner.updater;
        match call {
            Call::ReadUpdateStatus(request) => Ok(updater.status(request).await.into()),
            Call::CheckUpdate(request) => updater
                .check(request)
                .await
                .map(Into::into)
                .map_err(|error| Failure::new("update_check_failed", error)),
            Call::DownloadUpdate(request) => updater
                .download(request)
                .await
                .map(Into::into)
                .map_err(|error| Failure::new("update_download_failed", error)),
            Call::InstallUpdate(request) => updater
                .install(request)
                .await
                .map(Into::into)
                .map_err(|error| Failure::new("update_install_failed", error)),
            Call::SetUpdateChannel(request) => updater
                .set_channel(request)
                .await
                .map(Into::into)
                .map_err(|error| Failure::new("update_channel_failed", error)),
            Call::ReadNativeUpdate(request) => updater
                .native(request)
                .await
                .map(Into::into)
                .map_err(|error| Failure::new("native_update_check_failed", error)),
            _ => Err(Failure::new("invalid_method", "not an update request")),
        }
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
    /// The complete scheduled-task list first, then a fresh list after each
    /// durable change. The revision watch is deliberately read before the
    /// first list so a concurrent write cannot be lost.
    async fn scheduled_tasks(&self, cancel: tokio_util::sync::CancellationToken) -> HostReply {
        let conversation = match self.conversation() {
            Ok(conversation) => conversation.clone(),
            Err(error) => return Response::from_result::<(), _>(Err(error)).into(),
        };
        let tasks = conversation.runtime.scheduled_tasks().clone();
        let receiver = Arc::new(tokio::sync::Mutex::new(tasks.subscribe()));
        let first = match tasks.list().await {
            Ok(tasks) => st::ScheduledTaskList {
                tasks: tasks.into_iter().map(Self::scheduled_task).collect(),
            },
            Err(error) => {
                return Response::error("scheduled_tasks_unavailable", &error).into();
            }
        };
        crate::conversation::stream(
            std::collections::VecDeque::from([first]),
            st::ScheduledTaskList { tasks: vec![] },
            move || {
                let (receiver, tasks) = (receiver.clone(), tasks.clone());
                Box::pin(async move {
                    let mut receiver = receiver.lock().await;
                    receiver.changed().await.ok()?;
                    let list = tasks.list().await.ok()?;
                    Some(vec![st::ScheduledTaskList {
                        tasks: list.into_iter().map(Self::scheduled_task).collect(),
                    }])
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
                Call::ReadUsageSummary(params) => resources
                    .usage
                    .summary(params.input.clone(), self.usage_homes())
                    .await
                    .map_err(|error| Failure::new("usage_read_failed", error))?
                    .into(),
                Call::RefreshUsageRates(_) => resources.usage.refresh_rates().await.into(),
                Call::ConsumeResetCredit(params) => self
                    .identity(params.provider)?
                    .consume_reset_credit(&params.account_id, params.credit_id.as_deref())
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
                Call::GetPullRequestDiffFileContents(params) => {
                    params.validate().map_err(|error| Failure::new("invalid_params", error))?;
                    ensure_github_host(params.reference.host.as_deref())?;
                    let root = self.project_root(&params.reference.project_id)?;
                    resources
                        .pull_requests
                        .diff_file_contents(&root, params)
                        .await
                        .map_err(|error| Failure::new("pull_request_diff_file_contents_failed", error))?
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
                Call::SearchContents(params) => resources
                    .search
                    .search_contents(params.clone())
                    .await
                    .map_err(|error| Failure::new("search_contents_failed", error))?
                    .into(),
                Call::VcsStatus(params) => resources
                    .vcs
                    .get_status(&params.cwd)
                    .await
                    .map_err(|error| Failure::new("vcs_status_failed", error))?
                    .into(),
                Call::ListRefs(params) => crate::vcs::refs(params.clone())
                    .await
                    .map_err(|error| Failure::new("vcs_refs_failed", error))?
                    .into(),
                Call::CreateRef(params) => {
                    let result = crate::vcs::create_ref(params.clone())
                        .await
                        .map_err(|error| Failure::new("vcs_create_ref_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
                Call::SwitchRef(params) => {
                    let result = crate::vcs::switch_ref(params.clone())
                        .await
                        .map_err(|error| Failure::new("vcs_switch_ref_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
                Call::DiffPreview(params) => crate::vcs::diff_preview(params.clone())
                    .await
                    .map_err(|error| Failure::new("diff_preview_failed", error))?
                    .into(),
                Call::RefreshVcsStatus(params) => resources
                    .vcs
                    .refresh_status(&params.cwd)
                    .await
                    .map_err(|error| Failure::new("vcs_status_refresh_failed", error))?
                    .into(),
                Call::Pull(params) => {
                    let result = crate::vcs::pull_current_branch(&params.cwd)
                        .await
                        .map_err(|error| Failure::new("vcs_pull_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
                Call::InitRepository(params) => {
                    crate::vcs::init_repository(&params.cwd)
                        .await
                        .map_err(|error| Failure::new("vcs_init_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    agent_protocol::models::Empty {}.into()
                }
                Call::CreateWorktree(params) => {
                    let settings = resources
                        .shared
                        .worktrees
                        .settings(None)
                        .await
                        .map_err(|error| Failure::new("vcs_worktree_settings_failed", error))?;
                    let result = crate::vcs::create_worktree(params, &settings.worktree_directory)
                        .await
                        .map_err(|error| Failure::new("vcs_worktree_create_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
                Call::RemoveWorktreeCheckout(params) => {
                    crate::vcs::remove_worktree(&params.cwd, &params.path, params.force)
                        .await
                        .map_err(|error| Failure::new("vcs_worktree_remove_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    agent_protocol::models::Empty {}.into()
                }
                Call::ResolvePullRequest(params) => crate::vcs::resolve_pull_request(
                    resources.vcs.github(),
                    params,
                )
                .await
                .map_err(|error| Failure::new("pull_request_resolve_failed", error))?
                .into(),
                Call::PreparePullRequestThread(params) => {
                    let settings = resources
                        .shared
                        .worktrees
                        .settings(None)
                        .await
                        .map_err(|error| Failure::new("vcs_worktree_settings_failed", error))?;
                    let result = crate::vcs::prepare_pull_request_thread(
                        resources.vcs.github(),
                        params,
                        &settings.worktree_directory,
                    )
                    .await
                    .map_err(|error| Failure::new("pull_request_checkout_failed", error))?;
                    if let Some(thread) = params.thread_id.clone()
                        && result.worktree_path.is_some()
                        && result.is_on_pull_request_head
                    {
                        let project = resources
                            .shared
                            .projects
                            .list()
                            .into_iter()
                            .filter(|project| {
                                Path::new(&params.cwd).starts_with(Path::new(&project.root))
                            })
                            .max_by_key(|project| project.root.len());
                        if let Some(project) = project {
                            let cwd = result
                                .worktree_path
                                .clone()
                                .unwrap_or_else(|| params.cwd.clone());
                            let setup = crate::conversation::run_project_setup(
                                &resources.shared,
                                SetupRequest {
                                    thread,
                                    project: project.id,
                                    project_root: project.root,
                                    cwd,
                                    observe: SetupProgress::new(|_| {}),
                                },
                            )
                            .await;
                            match setup {
                                Ok(SetupRun::Started(started)) => {
                                    if !started.run_async {
                                        if let Some(completion) = started.completion {
                                            if completion.await != Some(0) {
                                                tracing::warn!(
                                                    operation = "host.vcs.pull_request_setup",
                                                    "pull request setup script did not complete successfully"
                                                );
                                            }
                                        }
                                    }
                                }
                                Ok(SetupRun::NoScript) => {}
                                Err(error) => tracing::warn!(
                                    operation = "host.vcs.pull_request_setup",
                                    message = %error,
                                ),
                            }
                        }
                    }
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
                Call::PublishRepository(params) => {
                    let result = crate::vcs::publish(resources.vcs.github(), params)
                        .await
                        .map_err(|error| Failure::new("repository_publish_failed", error))?;
                    resources.vcs.spawn_refresh(&params.cwd);
                    result.into()
                }
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
                        .map_err(|error| Failure::new("device_input_failed", error))?;
                    agent_protocol::models::Empty {}.into()
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
                Call::PreviewList(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
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
                        )
                        .map_err(|error| Failure::new("preview_open_failed", error))?;
                    browser.report_preview_frame(&params.thread_id.to_string(), &frame);
                    resources
                        .preview
                        .get(&params.thread_id, &frame.tab_id)
                        .unwrap_or(session)
                        .into()
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
                    let ids: Vec<String> = resources
                        .preview
                        .list(&params.thread_id)
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
                        .start_preview_recording(&params.thread_id.to_string(), &params.tab_id)
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
                Call::ConnectionPerformance(params) => {
                    let params = params.clone();
                    tokio::task::spawn_blocking(move || {
                        agent_transport::diagnostics::connection_performance(&params)
                    })
                    .await
                    .map_err(|error| Failure::new("diagnostic_write_failed", error))?;
                    agent_protocol::models::Empty {}.into()
                }
                Call::ReadUpdateStatus(_)
                | Call::CheckUpdate(_)
                | Call::DownloadUpdate(_)
                | Call::InstallUpdate(_)
                | Call::SetUpdateChannel(_)
                | Call::ReadNativeUpdate(_) => self.update(request).await?,
                Call::ReadBackground(_) => resources.background.snapshot().await.into(),
                Call::UpdateBackgroundPolicy(params) => resources
                    .background
                    .set_policy(params.policy.clone())
                    .await
                    .into(),
                Call::ReportClientActivity(params) => resources
                    .background
                    .report_activity(session, params.clone())
                    .await
                    .map_err(|error| Failure::new("invalid_params", error))?
                    .into(),
                Call::ReportHostPowerState(params) => {
                    resources.background.report_power(params.clone()).await;
                    agent_protocol::models::Empty {}.into()
                }
                Call::RemoveClientActivity(params) => resources
                    .background
                    .remove_activity(session, params.rpc_client_id)
                    .await
                    .into(),
                Call::ReadHostResources(_) => resources.background.host_resources().await.into(),
                Call::ReadProcessDiagnostics(_) => {
                    resources.background.process_diagnostics().await.into()
                }
                Call::ReadProcessResourceHistory(params) => resources
                    .background
                    .process_history(params.window_ms, params.bucket_ms)
                    .await
                    .into(),
                Call::ReadTraceDiagnostics(params) => resources
                    .background
                    .trace_diagnostics(params)
                    .await
                    .map_err(|error| Failure::new("diagnostics_unavailable", error))?
                    .into(),
                Call::SubscribeBackground(_) => {
                    return Err(Failure::new(
                        "stream_only",
                        "background subscriptions must use a stream",
                    ));
                }
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
                    let fetch_ms = settings
                        .background_activity
                        .resolved()
                        .automatic_git_fetch_interval_ms;
                    let fetch_seconds = if fetch_ms == 0 {
                        0
                    } else {
                        fetch_ms.saturating_add(999) / 1_000
                    };
                    resources
                        .source_control_auto_fetch_interval_seconds
                        .store(fetch_seconds, Ordering::Release);
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
                Call::UpsertScheduledTask(params) => {
                    params
                        .validate()
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let task = self
                        .conversation()?
                        .runtime
                        .scheduled_tasks()
                        .upsert(Self::scheduled_input(params))
                        .await
                        .map_err(Self::scheduled_failure)?;
                    Self::scheduled_task(task).into()
                }
                Call::ListScheduledTasks(_) => {
                    let tasks = self
                        .conversation()?
                        .runtime
                        .scheduled_tasks()
                        .list()
                        .await
                        .map_err(|error| Failure::new("scheduled_tasks_unavailable", error))?;
                    st::ScheduledTaskList {
                        tasks: tasks.into_iter().map(Self::scheduled_task).collect(),
                    }
                    .into()
                }
                Call::SetScheduledTaskEnabled(params) => {
                    let task = self
                        .conversation()?
                        .runtime
                        .scheduled_tasks()
                        .set_enabled(&params.id, params.enabled)
                        .await
                        .map_err(Self::scheduled_failure)?;
                    Self::scheduled_task(task).into()
                }
                Call::DeleteScheduledTask(params) => {
                    self.conversation()?
                        .runtime
                        .scheduled_tasks()
                        .delete(&params.id)
                        .await
                        .map_err(|error| Failure::new("scheduled_task_failed", error))?;
                    st::ScheduledTaskRef {
                        id: params.id.clone(),
                    }
                    .into()
                }
                Call::RunScheduledTaskNow(params) => {
                    let task = self
                        .conversation()?
                        .runtime
                        .scheduled_tasks()
                        .run_now(&params.id)
                        .await
                        .map_err(Self::scheduled_failure)?;
                    Self::scheduled_task(task).into()
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
                Call::SubscribeVcsStatus(_) | Call::RunStackedAction(_) => {
                    return Err(Failure::new(
                        "stream_dispatch_failed",
                        "Git stream requests must be dispatched as streams.",
                    ));
                }
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
    fn usage_homes(&self) -> Vec<(ProviderKind, PathBuf)> {
        let resources = &self.inner.resources;
        let mut homes = vec![(ProviderKind::Codex, resources.codex.directory.clone())];
        if let Some(claude) = resources.claude.get() {
            homes.push((ProviderKind::Claude, claude.native_home.clone()));
        }
        homes
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
                    Ok(mut accounts) => {
                        for account in &mut accounts.accounts {
                            account.usage = agent.usage(&account.id).await.ok();
                        }
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
    fn spawn_background_consumers(
        &self,
    ) -> tokio_util::task::AbortOnDropHandle<()> {
        let service = self.clone();
        let stop = self.inner.resources.background_stop.clone();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut last_git_fetch = std::collections::BTreeMap::<String, Timestamp>::new();
            let mut last_provider_refresh = None::<Timestamp>;
            let mut last_resource_sample = None::<Timestamp>;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                }

                let policy = service.inner.resources.background.policy().await;
                let now = Self::host_now();

                if policy.provider_health_refresh_interval_ms > 0
                    && background_work_due(
                        last_provider_refresh.as_ref(),
                        &now,
                        policy.provider_health_refresh_interval_ms,
                    )
                    && service.inner.resources.background.has_provider_status_demand().await
                {
                    // Record the attempt before starting the read. A failed
                    // provider probe therefore observes the configured cadence
                    // instead of creating a tight retry loop.
                    last_provider_refresh = Some(now.clone());
                    let _ = service.refresh_provider_cache().await;
                }

                if policy.automatic_git_fetch_interval_ms > 0 {
                    let demanded = service
                        .inner
                        .resources
                        .background
                        .demanded_vcs_workspaces()
                        .await;
                    last_git_fetch.retain(|cwd, _| demanded.iter().any(|candidate| candidate == cwd));
                    for cwd in demanded {
                        if !background_work_due(
                            last_git_fetch.get(&cwd),
                            &now,
                            policy.automatic_git_fetch_interval_ms,
                        ) {
                            continue;
                        }
                        last_git_fetch.insert(cwd.clone(), now.clone());
                        let started = std::time::Instant::now();
                        let refresh = service.inner.resources.vcs.refresh_status(&cwd);
                        let result = tokio::select! {
                            _ = stop.cancelled() => return,
                            result = refresh => result,
                        };
                        service.inner.resources.background.record_attribution(
                            "git",
                            "remote.fetch",
                            0,
                            0,
                            1,
                            started.elapsed().as_millis() as u64,
                        );
                        if let Err(error) = result {
                            tracing::debug!(target: "bex", operation = "host.vcs.background_refresh", cwd = %cwd, message = %error);
                        }
                    }
                } else {
                    last_git_fetch.clear();
                }

                if background_work_due(
                    last_resource_sample.as_ref(),
                    &now,
                    5_000,
                ) {
                    last_resource_sample = Some(now);
                    service
                        .inner
                        .resources
                        .background
                        .sample_resources_if_demanded()
                        .await;
                }
            }
        }))
    }

    fn host_now() -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        Timestamp::from_millis(millis).expect("system time is within timestamp range")
    }

    async fn refresh_provider_cache(&self) -> Vec<agent_protocol::models::ProviderInstance> {
        let _refresh = self.inner.resources.provider_refresh.lock().await;
        let started = std::time::Instant::now();
        let providers = self.providers_uncached().await;
        let logical_read_bytes = serde_json::to_vec(&providers)
            .map(|value| value.len() as u64)
            .unwrap_or(0);
        self.inner.resources.background.record_attribution(
            "provider",
            "health.refresh",
            logical_read_bytes,
            0,
            1,
            started.elapsed().as_millis() as u64,
        );
        *self.inner.resources.provider_cache.write().await = Some(ProviderHealthCache {
            refreshed_at: Self::host_now(),
            providers: providers.clone(),
        });
        providers
    }

    /// Codex and Claude as the composer offers them, with their models. A
    /// background health refresh owns the cache when a client has demand;
    /// direct reads refresh it when there is no usable cached result.
    async fn providers(&self) -> Vec<agent_protocol::models::ProviderInstance> {
        let policy = self.inner.resources.background.policy().await;
        let now = Self::host_now();
        if let Some(cache) = self.inner.resources.provider_cache.read().await.as_ref()
            && policy.provider_health_refresh_interval_ms > 0
            && !background_work_due(
                Some(&cache.refreshed_at),
                &now,
                policy.provider_health_refresh_interval_ms,
            )
        {
            return cache.providers.clone();
        }
        self.refresh_provider_cache().await
    }

    /// Reads both provider installations and their model catalogs without
    /// consulting the cache. The periodic owner calls this after its policy
    /// gate fires so it remains the sole health refresh scheduler.
    async fn providers_uncached(&self) -> Vec<agent_protocol::models::ProviderInstance> {
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
            status: ProviderStatus::Ready,
            message: None,
            unavailable_reason: None,
            show_interaction_mode_toggle: true,
            reports_context_window: true,
            supported_runtime_modes: supported_runtime_modes(driver),
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

fn preview_event_thread(event: &agent_protocol::preview::PreviewEvent) -> &agent_domain::ThreadId {
    match event {
        agent_protocol::preview::PreviewEvent::Opened { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Navigated { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Resized { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Failed { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::Closed { thread_id, .. }
        | agent_protocol::preview::PreviewEvent::RecordingChanged { thread_id, .. } => thread_id,
    }
}

/// The two built-in providers expose the same four user-selectable permission
/// modes. Keep this capability in the Host catalog so core and every native
/// client render the actual provider contract rather than reconstructing it.
fn supported_runtime_modes(driver: Driver) -> Vec<RuntimeMode> {
    match driver {
        Driver::Codex | Driver::Claude => vec![
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ],
    }
}

fn activity_phase(status: Option<RunStatus>, waiting_request: bool) -> AgentActivityPhase {
    if waiting_request {
        return AgentActivityPhase::WaitingInput;
    }
    match status {
        Some(RunStatus::Preparing | RunStatus::Starting | RunStatus::Queued) => {
            AgentActivityPhase::Starting
        }
        Some(RunStatus::Running) => AgentActivityPhase::Running,
        Some(RunStatus::Waiting) => AgentActivityPhase::WaitingApproval,
        Some(RunStatus::Completed) => AgentActivityPhase::Completed,
        Some(RunStatus::Failed) => AgentActivityPhase::Failed,
        Some(RunStatus::Interrupted | RunStatus::Cancelled | RunStatus::RolledBack) => {
            AgentActivityPhase::Stale
        }
        None => AgentActivityPhase::Running,
    }
}

fn activity_headline(status: Option<RunStatus>, waiting_request: bool) -> String {
    if waiting_request {
        return "Waiting for input".into();
    }
    match activity_phase(status, false) {
        AgentActivityPhase::Starting => "Starting".into(),
        AgentActivityPhase::Running => "Working".into(),
        AgentActivityPhase::WaitingApproval => "Waiting for approval".into(),
        AgentActivityPhase::WaitingInput => "Waiting for input".into(),
        AgentActivityPhase::Completed => "Completed".into(),
        AgentActivityPhase::Failed => "Failed".into(),
        AgentActivityPhase::Stale => "Stopped".into(),
    }
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
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
