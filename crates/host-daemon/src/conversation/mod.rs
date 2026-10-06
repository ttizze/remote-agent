//! The conversation runtime served by this Host: its I/O, provider processes,
//! RPC handlers and the agent tools of provider sessions.
mod diff;
mod operations;
mod rpc;
mod sessions;
mod title_links;
mod setup;
pub mod tools;

#[cfg(test)]
mod tests;

pub(crate) use operations::{HostIo, ProjectCatalog, TextGenerator};
pub(crate) use sessions::{
    BrowserConfig, ProviderHost, ProviderPrograms, Spawner, SupervisedSpawner,
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
use tools::{AgentTools, ModelCatalog, ToolBridge};

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
            setups: Default::default(),
            text: TextGenerator {
                codex: config.programs.codex.clone(),
                codex_home: config.programs.codex_home.clone(),
                claude: config.programs.claude.clone(),
            },
        });
        let runtime = Arc::new(Runtime::open(config.runtime, io, host.clone()).await?);
        let _ = host.runtime.set(Arc::downgrade(&runtime));
        let tools = Arc::new(AgentTools {
            runtime: runtime.clone(),
            models: config.models,
        });
        Ok(Arc::new(Self {
            runtime,
            resources,
            bridge,
            tools,
        }))
    }

    /// Recovers unfinished threads, then starts effects, import and the agent tools.
    pub(crate) async fn start(&self) -> anyhow::Result<()> {
        self.resources.projects.refresh().await?;
        self.runtime.start().await?;
        self.bridge
            .serve(Arc::downgrade(&self.tools))
            .map_err(anyhow::Error::msg)
    }

    /// Stops effects and provider processes; unfinished threads record the shutdown.
    pub(crate) async fn shutdown(&self) {
        self.runtime.shutdown().await;
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
        if let Err(error) = self.resources.projects.refresh().await {
            tracing::warn!(operation = "conversation.projects", message = %format_args!("{error:#}"));
        }
        if let Err(error) = self.runtime.project_changed(project).await {
            tracing::warn!(operation = "conversation.projects", message = %error);
        }
        let (runtime, project) = (self.runtime.clone(), project.to_owned());
        tokio::spawn(async move {
            if let Err(error) = runtime.import(&project, None).await {
                tracing::warn!(operation = "conversation.import", message = %error);
            }
        });
    }
}
