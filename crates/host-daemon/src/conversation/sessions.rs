//! Provider processes for the runtime's sessions: Codex app-server and Claude CLI
//! launches under the process supervisor, their MCP tools, images and transcripts.
use super::{ClaudeCredentials, CodexCredentials, ProjectCatalog, tools, tools::ToolBridge};
use crate::claude::control::ClaudeProgram;
use crate::claude::skills::user_invocable_skills;
use crate::{workspace_files::WorkspaceFiles, worktrees::Worktrees};
use agent_domain::{Attachment, AttachmentKind, Driver, Json, ThreadId};
use agent_providers::{
    CLAUDE_MCP_TOOL_TIMEOUT_MS, PreparedImage, WireContext, claude_append_system_prompt,
    claude_project_key, codex_additional_context, codex_developer_instructions,
};
use agent_runtime::{
    ClaudeSettings, LaunchTarget, ProviderProcess, Runtime, SessionHost, SessionKey, SpawnRequest,
};
use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
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
    pub(crate) codex_accounts: Option<Arc<dyn CodexCredentials>>,
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
            tools::SERVER_NAME.to_owned(),
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
            let omit_service_tier = match &self.programs.codex_accounts {
                Some(accounts) => accounts.shares_tokens().await,
                None => false,
            };
            let effort = target
                .selection
                .options
                .get("reasoningEffort")
                .map_or("medium", String::as_str);
            Ok(WireContext {
                cwd: cwd.to_string_lossy().into_owned(),
                client_name: "remote_agent_host".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                omit_service_tier,
                developer_instructions: Some(
                    codex_developer_instructions(target.interaction_mode).to_owned(),
                ),
                additional_context: Some(Json(codex_additional_context(
                    &target.selection.model,
                    effort,
                    servers.contains_key("browser"),
                ))),
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
            let config = self.claude()?.0.config_home.clone();
            let cwd = self.cwd(&target).await?;
            // The app's tools are pre-approved, and a waiting tool may block for
            // up to an hour.
            let mut mcp_servers = self.mcp_servers(&target.key)?;
            for server in mcp_servers.values_mut() {
                server["timeout"] = json!(CLAUDE_MCP_TOOL_TIMEOUT_MS);
            }
            let mcp_allowed_tools = mcp_servers
                .keys()
                .map(|name| format!("mcp__{name}__*"))
                .collect();
            let mcp_read_only_tools = tools::read_only_tools()
                .into_iter()
                .map(|tool| format!("mcp__{}__{tool}", tools::SERVER_NAME))
                .collect();
            let skills = {
                let cwd = cwd.clone();
                tokio::task::spawn_blocking(move || user_invocable_skills(&config, Some(&cwd)))
                    .await
                    .map_err(|error| error.to_string())?
            };
            Ok(ClaudeSettings {
                mcp_servers,
                mcp_allowed_tools,
                mcp_read_only_tools,
                append_system_prompt: claude_append_system_prompt(true),
                additional_directories: vec![
                    cwd.to_string_lossy().into_owned(),
                    self.files.attachment_root().to_string_lossy().into_owned(),
                ],
                skills,
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

    fn revoke_credentials(&self, thread: &ThreadId, instance: Option<&str>) {
        self.tools.revoke(thread, instance);
    }

    fn codex_account(&self, _instance: String) -> BoxFuture<'_, Result<Option<Value>, String>> {
        Box::pin(async move {
            match &self.programs.codex_accounts {
                Some(accounts) => accounts.login().await,
                None => Ok(None),
            }
        })
    }

    fn refresh_codex_account(
        &self,
        _instance: String,
        previous_account: Option<String>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            match &self.programs.codex_accounts {
                Some(accounts) => accounts.refresh(previous_account).await,
                None => Err("Select an account before refreshing credentials".into()),
            }
        })
    }
}
