//! Provider processes for the runtime's sessions: Codex app-server and Claude CLI
//! launches under the process supervisor, their MCP tools, images and transcripts.
use super::{ClaudeCredentials, CodexCredentials, ProjectCatalog, tools, tools::ToolBridge};
use crate::claude::control::ClaudeProgram;
use crate::claude::skills::user_invocable_skills;
use crate::{workspace_files::WorkspaceFiles, worktrees::Worktrees};
use agent_domain::{Attachment, AttachmentKind, Driver, Json, ThreadId};
use agent_protocol::models::ProviderInstanceConfig;
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
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock, Weak},
};

fn claude_transcript_path(config_home: &Path, cwd: &Path, session: &str) -> io::Result<PathBuf> {
    if session.is_empty()
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(io::Error::other("invalid Claude session id"));
    }
    let cwd = dunce::simplified(cwd).to_string_lossy();
    #[cfg(target_os = "macos")]
    let cwd = icu_normalizer::ComposingNormalizer::new_nfc().normalize(&cwd);
    Ok(config_home
        .join("projects")
        .join(claude_project_key(&cwd))
        .join(format!("{session}.jsonl")))
}

#[cfg(test)]
mod transcript_paths {
    use super::*;

    #[test]
    fn fork_transcripts_use_the_native_normalized_project_directory() {
        let path = claude_transcript_path(
            Path::new("fixture-home"),
            Path::new("/tmp/cafe\u{301}"),
            "123e4567-e89b-12d3-a456-426614174000",
        )
        .unwrap();
        let project = if cfg!(target_os = "macos") {
            "-tmp-caf-"
        } else {
            "-tmp-cafe-"
        };
        assert_eq!(
            path,
            Path::new("fixture-home")
                .join("projects")
                .join(project)
                .join("123e4567-e89b-12d3-a456-426614174000.jsonl")
        );
    }

    #[test]
    fn a_session_id_cannot_escape_its_project_directory() {
        for session in ["", "../other", "/other", "a/b", "a\\b", ".", "a:other"] {
            assert!(
                claude_transcript_path(Path::new("fixture-home"), Path::new("/tmp"), session)
                    .is_err()
            );
        }
    }
}

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
    Arc<dyn Fn(&ThreadId, Option<&str>) -> Option<Result<Value, String>> + Send + Sync>;

type CodexLaunchConfig = (
    PathBuf,
    Option<PathBuf>,
    Vec<String>,
    BTreeMap<String, String>,
);

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
    /// Reads the owner-managed configuration for each new provider process.
    /// Existing sessions keep their already spawned process, while the next
    /// session observes an updated executable, home or argument list.
    fn configured_provider(&self, instance: &str) -> Option<ProviderInstanceConfig> {
        self.worktrees
            .latest_host_settings()
            .provider_instances
            .get(instance)
            .cloned()
    }

    fn codex_launch(&self, instance: &str) -> Result<CodexLaunchConfig, String> {
        let configured = self.configured_provider(instance);
        if configured
            .as_ref()
            .is_some_and(|config| config.driver != Driver::Codex)
        {
            return Err(format!(
                "Provider instance {instance} is configured for a different driver."
            ));
        }
        if configured.is_none() && instance != "codex" {
            return Err(format!("Provider instance {instance} is not configured."));
        }
        if configured.as_ref().is_some_and(|config| !config.enabled) {
            return Err(format!("Provider instance {instance} is disabled."));
        }
        let program = configured
            .as_ref()
            .and_then(|config| config.binary_path.as_deref())
            .map(crate::projects::expand_home)
            .or_else(|| self.programs.codex.clone())
            .ok_or_else(|| "Codex is unavailable on this Host.".to_owned())?;
        let home = configured
            .as_ref()
            .and_then(|config| config.home_path.as_deref())
            .map(crate::projects::expand_home)
            .or_else(|| self.programs.codex_home.clone());
        let (args, environment) = configured
            .map(|config| (config.launch_args, config.environment))
            .unwrap_or_default();
        Ok((program, home, args, environment))
    }

    fn claude_launch(
        &self,
        instance: &str,
    ) -> Result<(ClaudeProgram, Option<Arc<dyn ClaudeCredentials>>), String> {
        let configured = self.configured_provider(instance);
        if configured
            .as_ref()
            .is_some_and(|config| config.driver != Driver::Claude)
        {
            return Err(format!(
                "Provider instance {instance} is configured for a different driver."
            ));
        }
        if configured.is_none() && instance != "claude" {
            return Err(format!("Provider instance {instance} is not configured."));
        }
        if configured.as_ref().is_some_and(|config| !config.enabled) {
            return Err(format!("Provider instance {instance} is disabled."));
        }
        if let Some((base, credentials)) = self.programs.claude.clone() {
            let use_base_credentials = configured
                .as_ref()
                .is_none_or(|config| config.home_path.is_none());
            let program = configured
                .as_ref()
                .and_then(|config| config.binary_path.as_deref())
                .map(crate::projects::expand_home)
                .unwrap_or(base.program);
            let config_home = configured
                .as_ref()
                .and_then(|config| config.home_path.as_deref())
                .map(crate::projects::expand_home)
                .unwrap_or(base.config_home);
            return Ok((
                ClaudeProgram {
                    program,
                    config_home,
                    environment: configured
                        .as_ref()
                        .map_or_else(BTreeMap::new, |config| config.environment.clone()),
                    launch_args: configured
                        .as_ref()
                        .map_or_else(Vec::new, |config| config.launch_args.clone()),
                },
                use_base_credentials.then_some(credentials),
            ));
        }
        let config = configured.ok_or_else(|| "Claude is unavailable on this Host.".to_owned())?;
        let program = config
            .binary_path
            .map(|path| crate::projects::expand_home(&path))
            .ok_or_else(|| "Claude needs an executable path for this instance.".to_owned())?;
        let config_home = config
            .home_path
            .map(|path| crate::projects::expand_home(&path))
            .ok_or_else(|| {
                "Claude needs a configuration directory for this instance.".to_owned()
            })?;
        Ok((
            ClaudeProgram {
                program,
                config_home,
                environment: config.environment,
                launch_args: config.launch_args,
            },
            None,
        ))
    }

    async fn claude_home(
        &self,
        program: &ClaudeProgram,
        credentials: Option<&Arc<dyn ClaudeCredentials>>,
    ) -> Result<PathBuf, String> {
        match credentials {
            Some(credentials) => credentials.claude_home().await,
            None => Ok(program.config_home.clone()),
        }
    }

    fn managed_codex_accounts(&self, instance: &str) -> Option<Arc<dyn CodexCredentials>> {
        (instance == "codex")
            .then(|| self.programs.codex_accounts.clone())
            .flatten()
    }

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

    async fn mcp_servers(&self, key: &SessionKey) -> Result<BTreeMap<String, Value>, String> {
        let mut servers = BTreeMap::from([(
            tools::SERVER_NAME.to_owned(),
            self.tools.provider_config(&key.thread, &key.instance)?,
        )]);
        let project = if let Some(runtime) = self.runtime.get().and_then(Weak::upgrade) {
            runtime.state(&key.thread).await.ok().and_then(|state| {
                state
                    .state
                    .thread
                    .as_ref()
                    .map(|thread| thread.project.clone())
            })
        } else {
            None
        };
        if let Some(browser) = (self.browser)(&key.thread, project.as_deref()) {
            servers.insert("browser".into(), browser?);
        }
        Ok(servers)
    }

    /// `<config home>/projects/<key of the real cwd>/<session>.jsonl`.
    async fn transcript(&self, target: &LaunchTarget, session: &str) -> io::Result<PathBuf> {
        let (claude, _) = self
            .claude_launch(&target.key.instance)
            .map_err(io::Error::other)?;
        let cwd = self.cwd(target).await.map_err(io::Error::other)?;
        let real = tokio::fs::canonicalize(&cwd).await.unwrap_or(cwd);
        claude_transcript_path(&claude.config_home, &real, session)
    }
}

impl SessionHost for ProviderHost {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, io::Result<ProviderProcess>> {
        Box::pin(async move {
            let cwd = self.cwd(&request.target).await.map_err(io::Error::other)?;
            let spec = match &request.claude {
                Some(_) => {
                    let (claude, credentials) = self
                        .claude_launch(&request.target.key.instance)
                        .map_err(io::Error::other)?;
                    let home = self
                        .claude_home(&claude, credentials.as_ref())
                        .await
                        .map_err(io::Error::other)?;
                    claude.sdk_process(&home, &cwd).await?
                }
                None => {
                    let (program, home, launch_args, environment) = self
                        .codex_launch(&request.target.key.instance)
                        .map_err(io::Error::other)?;
                    let mut args = ["app-server", "--listen", "stdio://"]
                        .map(str::to_owned)
                        .to_vec();
                    args.extend(launch_args);
                    ProcessSpec {
                        driver: Driver::Codex,
                        program,
                        args,
                        env: {
                            let mut environment = environment;
                            if let Some(home) = home {
                                environment.insert(
                                    "CODEX_HOME".to_owned(),
                                    home.to_string_lossy().into_owned(),
                                );
                            }
                            environment
                        },
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
            let servers = self.mcp_servers(&target.key).await?;
            let omit_service_tier = match self.managed_codex_accounts(&target.key.instance) {
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
            let (claude, _) = self.claude_launch(&target.key.instance)?;
            let config = claude.config_home.clone();
            let cwd = self.cwd(&target).await?;
            // The app's tools are pre-approved, and a waiting tool may block for
            // up to an hour.
            let mut mcp_servers = self.mcp_servers(&target.key).await?;
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

    fn prepare_claude_fork(
        &self,
        source: LaunchTarget,
        session: String,
        target: String,
        through: Option<String>,
    ) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async move {
            let (claude, credentials) = self
                .claude_launch(&source.key.instance)
                .map_err(io::Error::other)?;
            let home = self
                .claude_home(&claude, credentials.as_ref())
                .await
                .map_err(io::Error::other)?;
            let cwd = self.cwd(&source).await.map_err(io::Error::other)?;
            let path = self.transcript(&source, &session).await?;
            let transcript = tokio::fs::read_to_string(path).await?;
            let response = claude.sdk_requests(&home, &cwd, vec![json!({
                "subtype":"fork_session", "transcript":transcript, "session":session, "target":target, "through":through,
            })]).await?;
            response["transcript"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Claude SDK fork transcript missing",
                    )
                })
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

    fn codex_account(&self, instance: String) -> BoxFuture<'_, Result<Option<Value>, String>> {
        Box::pin(async move {
            match self.managed_codex_accounts(&instance) {
                Some(accounts) => accounts.login().await,
                None => Ok(None),
            }
        })
    }

    fn refresh_codex_account(
        &self,
        instance: String,
        previous_account: Option<String>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            match self.managed_codex_accounts(&instance) {
                Some(accounts) => accounts.refresh(previous_account).await,
                None => Err("Select an account before refreshing credentials".into()),
            }
        })
    }
}
