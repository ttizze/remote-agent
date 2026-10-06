//! Provider processes for the runtime's sessions: Codex app-server and Claude CLI
//! launches under the process supervisor, their MCP tools, images and transcripts.
use super::{ClaudeCredentials, ProjectCatalog, tools::ToolBridge};
use crate::claude::control::ClaudeProgram;
use crate::{workspace_files::WorkspaceFiles, worktrees::Worktrees};
use agent_domain::{Attachment, AttachmentKind, Driver, Json, ThreadId};
use agent_providers::{PreparedImage, WireContext, claude_project_key};
use agent_runtime::{
    ClaudeSettings, LaunchTarget, ProviderProcess, Runtime, SessionHost, SessionKey, SpawnRequest,
};
use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, OnceLock, Weak},
};

/// One provider process to start.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProcessSpec {
    pub(crate) driver: Driver,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    /// Added to the Host's environment, or the whole environment when `clear_env`.
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) clear_env: bool,
    pub(crate) cwd: PathBuf,
}

/// Starts provider processes; production runs them under the process supervisor.
pub(crate) trait Spawner: Send + Sync {
    fn spawn(&self, spec: ProcessSpec) -> io::Result<ProviderProcess>;
}

pub(crate) struct SupervisedSpawner;
impl Spawner for SupervisedSpawner {
    fn spawn(&self, spec: ProcessSpec) -> io::Result<ProviderProcess> {
        let mut command = bex_process::command(&spec.program)?;
        if spec.clear_env {
            command.env_clear();
        }
        command
            .args(&spec.args)
            .envs(&spec.env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ProviderProcess::from_child(command.spawn()?)
    }
}

/// The provider programs this Host can run.
#[derive(Clone, Default)]
pub(crate) struct ProviderPrograms {
    pub(crate) codex: Option<PathBuf>,
    pub(crate) codex_home: Option<PathBuf>,
    pub(crate) claude: Option<(ClaudeProgram, Arc<dyn ClaudeCredentials>)>,
}

/// The browser bridge's MCP server for a thread, when the browser is enabled.
pub(crate) type BrowserConfig =
    Arc<dyn Fn(&ThreadId) -> Option<Result<Value, String>> + Send + Sync>;

pub(crate) struct ProviderHost {
    pub(crate) spawner: Arc<dyn Spawner>,
    pub(crate) programs: ProviderPrograms,
    pub(crate) tools: Arc<ToolBridge>,
    pub(crate) browser: BrowserConfig,
    pub(crate) files: WorkspaceFiles,
    pub(crate) projects: Arc<ProjectCatalog>,
    pub(crate) worktrees: Arc<Worktrees>,
    /// Set once the runtime is open; a thread without a workspace runs in its
    /// project's root.
    pub(crate) runtime: OnceLock<Weak<Runtime>>,
}

impl ProviderHost {
    /// The thread's working directory, recreating a deleted managed checkout.
    async fn cwd(&self, target: &LaunchTarget) -> Result<PathBuf, String> {
        let cwd = match &target.workspace {
            Some(workspace) => workspace.cwd.clone(),
            None => {
                let runtime = self
                    .runtime
                    .get()
                    .and_then(Weak::upgrade)
                    .ok_or("The conversation runtime is not running.")?;
                let state = runtime
                    .state(&target.key.thread)
                    .await
                    .map_err(|error| error.to_string())?
                    .state;
                let project = state.thread.as_ref().map(|thread| thread.project.clone());
                self.projects
                    .list()
                    .into_iter()
                    .find(|candidate| Some(&candidate.id) == project.as_ref())
                    .map(|project| project.root)
                    .ok_or("The thread has no working directory.")?
            }
        };
        self.worktrees
            .ensure_available(&cwd)
            .await
            .map_err(|error| format!("{error:#}"))?;
        Ok(PathBuf::from(cwd))
    }

    fn mcp_servers(&self, key: &SessionKey) -> Result<BTreeMap<String, Value>, String> {
        let mut servers = BTreeMap::from([(
            "orchestration".to_owned(),
            self.tools.provider_config(&key.thread, &key.instance)?,
        )]);
        if let Some(browser) = (self.browser)(&key.thread) {
            servers.insert("browser".into(), browser?);
        }
        Ok(servers)
    }

    fn claude(&self) -> Result<&(ClaudeProgram, Arc<dyn ClaudeCredentials>), String> {
        self.programs
            .claude
            .as_ref()
            .ok_or_else(|| "Claude is unavailable on this Host.".to_owned())
    }

    /// `<config home>/projects/<key of the real cwd>/<session>.jsonl`.
    async fn transcript(&self, target: &LaunchTarget, session: &str) -> io::Result<PathBuf> {
        let (claude, _) = self.claude().map_err(io::Error::other)?;
        let cwd = self.cwd(target).await.map_err(io::Error::other)?;
        let real = tokio::fs::canonicalize(&cwd).await.unwrap_or(cwd);
        let key = claude_project_key(&dunce::simplified(&real).to_string_lossy());
        if session.is_empty()
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io::Error::other("invalid Claude session id"));
        }
        Ok(claude
            .config_home
            .join("projects")
            .join(key)
            .join(format!("{session}.jsonl")))
    }
}

impl SessionHost for ProviderHost {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, io::Result<ProviderProcess>> {
        Box::pin(async move {
            let cwd = self.cwd(&request.target).await.map_err(io::Error::other)?;
            let spec = match &request.claude {
                Some(launch) => {
                    let (claude, credentials) = self.claude().map_err(io::Error::other)?;
                    let home = credentials.claude_home().await.map_err(io::Error::other)?;
                    ProcessSpec {
                        driver: Driver::Claude,
                        program: claude.program.clone(),
                        args: launch.args(),
                        env: claude.environment(&home),
                        clear_env: true,
                        cwd,
                    }
                }
                None => {
                    let program =
                        self.programs.codex.clone().ok_or_else(|| {
                            io::Error::other("Codex is unavailable on this Host.")
                        })?;
                    ProcessSpec {
                        driver: Driver::Codex,
                        program,
                        args: ["app-server", "--listen", "stdio://"]
                            .map(str::to_owned)
                            .to_vec(),
                        env: self
                            .programs
                            .codex_home
                            .iter()
                            .map(|home| {
                                ("CODEX_HOME".to_owned(), home.to_string_lossy().into_owned())
                            })
                            .collect(),
                        clear_env: false,
                        cwd,
                    }
                }
            };
            self.spawner.spawn(spec)
        })
    }

    fn codex_context(&self, target: LaunchTarget) -> BoxFuture<'_, Result<WireContext, String>> {
        Box::pin(async move {
            let cwd = self.cwd(&target).await?;
            let servers = self.mcp_servers(&target.key)?;
            Ok(WireContext {
                cwd: cwd.to_string_lossy().into_owned(),
                client_name: "remote_agent_host".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                thread_config: BTreeMap::from([(
                    "mcp_servers".to_owned(),
                    Json(Value::Object(servers.into_iter().collect())),
                )]),
                ..WireContext::default()
            })
        })
    }

    fn claude_settings(
        &self,
        target: LaunchTarget,
    ) -> BoxFuture<'_, Result<ClaudeSettings, String>> {
        Box::pin(async move {
            self.claude()?;
            Ok(ClaudeSettings {
                mcp_servers: self.mcp_servers(&target.key)?,
                ..ClaudeSettings::default()
            })
        })
    }

    fn images(
        &self,
        _thread: ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Vec<PreparedImage>, String>> {
        Box::pin(async move {
            let files = self.files.clone();
            tokio::task::spawn_blocking(move || {
                attachments
                    .iter()
                    .filter(|attachment| attachment.kind == AttachmentKind::Image)
                    .map(|attachment| {
                        let bytes = files
                            .image(attachment)
                            .map_err(|error| format!("{error:#}"))?;
                        Ok(PreparedImage {
                            attachment_id: attachment.id.clone(),
                            mime_type: attachment.mime_type.clone(),
                            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                        })
                    })
                    .collect()
            })
            .await
            .map_err(|error| error.to_string())?
        })
    }

    fn read_claude_session(
        &self,
        target: LaunchTarget,
        session: String,
    ) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async move {
            tokio::fs::read_to_string(self.transcript(&target, &session).await?).await
        })
    }

    fn write_claude_session(
        &self,
        target: LaunchTarget,
        session: String,
        transcript: String,
    ) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let path = self.transcript(&target, &session).await?;
            tokio::task::spawn_blocking(move || {
                crate::platform::save_private_bytes(&path, transcript.as_bytes())
                    .map_err(io::Error::other)
            })
            .await
            .map_err(io::Error::other)?
        })
    }

    fn released(&self, key: &SessionKey, revoke_credentials: bool) {
        if revoke_credentials {
            self.tools.revoke(&key.thread, &key.instance);
        }
    }
}
