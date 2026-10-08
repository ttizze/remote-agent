//! The conversation runtime served by this Host: its I/O, provider processes,
//! RPC handlers and the agent tools of provider sessions.
mod diff;
mod operations;
mod rpc;
mod sessions;
mod setup;
mod title_links;
pub mod tools;

#[cfg(test)]
mod tests;

pub(crate) use operations::{HostIo, ProjectCatalog, TextGenerator};
pub(crate) use rpc::stream;
pub(crate) use sessions::{
    BrowserConfig, ProcessSpec, ProviderHost, ProviderPrograms, Spawner, SupervisedSpawner,
};

use crate::{
    checkpoints::Checkpoints, terminals::Terminals, workspace_files::WorkspaceFiles,
    worktrees::Worktrees,
};
use agent_runtime::{Runtime, RuntimeConfig};
use futures_util::future::BoxFuture;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tools::{AgentTools, HostOrchestration, ModelCatalog, ToolBridge};

/// The selected Claude account's credential storage.
pub(crate) trait ClaudeCredentials: Send + Sync {
    fn claude_home(&self) -> BoxFuture<'_, Result<PathBuf, String>>;
}

/// The managed Codex account conversation app-servers run as.
pub(crate) trait CodexCredentials: Send + Sync {
    /// `account/login/start` parameters, or `None` for CODEX_HOME's own login.
    fn login(&self) -> BoxFuture<'_, Result<Option<serde_json::Value>, String>>;
    /// Whether the login shares ChatGPT tokens, which do not accept service tiers.
    fn shares_tokens(&self) -> BoxFuture<'_, bool>;
    /// Answers `account/chatgptAuthTokens/refresh`.
    fn refresh(
        &self,
        previous_account: Option<String>,
    ) -> BoxFuture<'_, Result<serde_json::Value, String>>;
}

/// Host resources the conversation shares with the other RPCs.
#[derive(Clone)]
pub(crate) struct SharedResources {
    pub(crate) projects: Arc<ProjectCatalog>,
    pub(crate) checkpoints: Arc<Checkpoints>,
    pub(crate) worktrees: Arc<Worktrees>,
    pub(crate) files: WorkspaceFiles,
    pub(crate) terminals: Arc<Terminals>,
}

pub(crate) struct ConversationConfig {
    pub(crate) runtime: RuntimeConfig,
    pub(crate) programs: ProviderPrograms,
    pub(crate) spawner: Arc<dyn Spawner>,
    pub(crate) browser: BrowserConfig,
    pub(crate) models: Arc<dyn ModelCatalog>,
}

pub(crate) struct Conversation {
    pub(crate) runtime: Arc<Runtime>,
    resources: SharedResources,
    bridge: Arc<ToolBridge>,
    tools: Arc<AgentTools>,
    settled_terminals: OnceLock<tokio_util::task::AbortOnDropHandle<()>>,
    identity_updates: std::sync::Mutex<Option<tokio_util::task::AbortOnDropHandle<()>>>,
}

impl Conversation {
    /// Opens the store and wires the runtime to this Host. Nothing runs until `start`.
    pub(crate) async fn open(
        config: ConversationConfig,
        resources: SharedResources,
    ) -> anyhow::Result<Arc<Self>> {
        let bridge = Arc::new(ToolBridge::bind().map_err(anyhow::Error::msg)?);
        let host = Arc::new(ProviderHost {
            spawner: config.spawner,
            programs: config.programs.clone(),
            tools: bridge.clone(),
            browser: config.browser,
            files: resources.files.clone(),
            projects: resources.projects.clone(),
            worktrees: resources.worktrees.clone(),
            runtime: OnceLock::new(),
        });
        let io = Arc::new(HostIo {
            projects: resources.projects.clone(),
            checkpoints: resources.checkpoints.clone(),
            worktrees: resources.worktrees.clone(),
            files: resources.files.clone(),
            terminals: resources.terminals.clone(),
            text: TextGenerator {
                codex: config.programs.codex.clone(),
                codex_home: config.programs.codex_home.clone(),
                claude: config.programs.claude.clone(),
            },
        });
        let runtime = Arc::new(Runtime::open(config.runtime, io, host.clone()).await?);
        let _ = host.runtime.set(Arc::downgrade(&runtime));
        let tools = Arc::new(AgentTools::new(Arc::new(HostOrchestration {
            runtime: runtime.clone(),
            projects: resources.projects.clone(),
            files: resources.files.clone(),
            models: config.models,
        })));
        Ok(Arc::new(Self {
            runtime,
            resources,
            bridge,
            tools,
            settled_terminals: OnceLock::new(),
            identity_updates: Default::default(),
        }))
    }

    /// Recovers unfinished threads, then starts effects, import and the agent tools.
    pub(crate) async fn start(&self) -> anyhow::Result<()> {
        self.resources.projects.refresh().await?;
        self.resources.worktrees.host_settings(None).await?;
        self.runtime.start().await?;
        let _ = self
            .settled_terminals
            .set(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                close_idle_terminals_when_settled(
                    Arc::downgrade(&self.runtime),
                    self.resources.terminals.clone(),
                ),
            )));
        *self
            .identity_updates
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(
            tokio_util::task::AbortOnDropHandle::new(tokio::spawn(identity_updates(
                Arc::downgrade(&self.runtime),
                self.resources.projects.clone(),
                self.resources.projects.identities().subscribe(),
            ))),
        );
        self.bridge
            .serve(Arc::downgrade(&self.tools))
            .map_err(anyhow::Error::msg)
    }

    /// Stops effects and provider processes; unfinished threads record the shutdown.
    pub(crate) async fn shutdown(&self) {
        self.identity_updates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.runtime.shutdown().await;
    }

    /// The conversation settings changed: automatic settlement sweeps again.
    pub(crate) fn settings_changed(&self) {
        self.runtime.settings_changed();
    }

    /// A project's settings changed: shell subscribers see the update.
    pub(crate) async fn project_updated(&self, project: &str) {
        if let Err(error) = self.runtime.project_changed(project).await {
            tracing::warn!(operation = "conversation.projects", message = %error);
        }
    }

    /// A project was registered: shell subscribers see it and its recent sessions
    /// are imported.
    pub(crate) async fn project_added(&self, project: &str) {
        project_added(&self.runtime, &self.resources.projects, project).await;
    }
}

/// Settling a thread closes its shells that sit at an idle prompt, so they stop
/// holding the worktree; a shell running a command stays.
async fn close_idle_terminals_when_settled(
    runtime: std::sync::Weak<Runtime>,
    terminals: Arc<Terminals>,
) {
    use agent_runtime::{ShellSubscribe, ShellUpdate};
    loop {
        let Some(subscription) = (match runtime.upgrade() {
            Some(runtime) => runtime
                .subscribe_shell(ShellSubscribe::default())
                .await
                .ok(),
            None => return,
        }) else {
            return;
        };
        let mut updates = subscription.updates;
        let mut settled = std::collections::HashMap::new();
        while let Some(update) = updates.recv().await {
            let thread = match update {
                ShellUpdate::Snapshot(snapshot) => {
                    settled = snapshot
                        .threads
                        .into_iter()
                        .map(|thread| (thread.thread, thread.row.summary.settled))
                        .collect();
                    continue;
                }
                ShellUpdate::ThreadUpdated { thread, .. } => thread,
                _ => continue,
            };
            let now = thread.row.summary.settled;
            if settled.insert(thread.thread.clone(), now) != Some(Some(true)) && now == Some(true) {
                let terminals = terminals.clone();
                tokio::spawn(async move { terminals.close_idle(&thread.thread, None).await });
            }
        }
        if !updates.overflowed() {
            return;
        }
    }
}

pub(crate) async fn project_added(
    runtime: &Arc<Runtime>,
    projects: &ProjectCatalog,
    project: &str,
) {
    if let Err(error) = projects.refresh().await {
        tracing::warn!(operation = "conversation.projects", message = %format_args!("{error:#}"));
    }
    if let Some(added) = projects
        .list()
        .into_iter()
        .find(|added| added.id == project)
    {
        projects.identities().invalidate([added.root.as_str()]);
    }
    if let Err(error) = runtime.project_changed(project).await {
        tracing::warn!(operation = "conversation.projects", message = %error);
    }
    let (runtime, project) = (runtime.clone(), project.to_owned());
    tokio::spawn(async move {
        if let Err(error) = runtime.import(&project, None).await {
            tracing::warn!(operation = "conversation.import", message = %error);
        }
    });
}

/// Shell subscribers see each project whose repository identity changed.
async fn identity_updates(
    runtime: std::sync::Weak<Runtime>,
    projects: Arc<ProjectCatalog>,
    mut changes: tokio::sync::broadcast::Receiver<String>,
) {
    loop {
        let root = match changes.recv().await {
            Ok(root) => root,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let Some(runtime) = runtime.upgrade() else {
            return;
        };
        for project in projects
            .list()
            .into_iter()
            .filter(|project| project.root == root)
        {
            if let Err(error) = runtime.project_changed(&project.id).await {
                tracing::warn!(operation = "conversation.projects", message = %error);
            }
        }
    }
}
