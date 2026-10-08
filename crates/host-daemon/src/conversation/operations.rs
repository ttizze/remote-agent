//! Host I/O behind the conversation runtime: projects, Git checkpoints, worktrees,
//! attachments, terminals and title text generation.
use crate::checkpoints::{Checkpoints, RestoredFiles};
use crate::claude::control::ClaudeProgram;
use crate::{
    ProjectStore, terminals::Terminals, workspace_files::WorkspaceFiles, worktrees::Worktrees,
};
use agent_domain::{AttachmentKind, BranchNaming, CheckpointFile, ThreadId};
use agent_protocol::models::{
    AutoSettle, Project, ProjectRoot, ProjectScript, SourceControlWritingStyleMode,
    WorktreeSubmodules,
};
use agent_runtime::{
    ConversationSettings, CreatedWorktree, HostOperations, HostProject, PreparedRestore,
    SetupRequest, SetupRun, TextGenerationRequest, TextGenerationSettings, WorktreeRequest,
};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

/// The project for conversations outside a repository; each thread gets a folder.
pub(crate) const CHATS_PROJECT: &str = "chats";

/// Registered projects, cached for the runtime's synchronous reads.
pub(crate) struct ProjectCatalog {
    store: ProjectStore,
    projects: RwLock<Vec<HostProject>>,
    scripts: RwLock<HashMap<String, Vec<ProjectScript>>>,
    identities: crate::repository::ProjectIdentities,
    stored: RwLock<HashMap<String, crate::projects::StoredProject>>,
    chats: tokio::sync::OnceCell<bool>,
}

impl ProjectCatalog {
    pub(crate) fn new(store: ProjectStore) -> Self {
        Self {
            store,
            projects: RwLock::new(vec![]),
            scripts: RwLock::default(),
            identities: crate::repository::ProjectIdentities::system(Default::default()),
            stored: RwLock::default(),
            chats: tokio::sync::OnceCell::new(),
        }
    }
    pub(crate) fn store(&self) -> &ProjectStore {
        &self.store
    }
    /// The projects' repository identities, resolved in the background.
    pub(crate) fn identities(&self) -> &crate::repository::ProjectIdentities {
        &self.identities
    }
    /// The chats folder, offered only when the Host's data directory is outside any
    /// Git work tree, whose status and checkpoints its folders would inherit.
    /// Probed once.
    pub(crate) async fn chats_root(&self) -> Option<PathBuf> {
        let chats = self.store.chat_directory();
        let data = chats.parent()?.to_path_buf();
        self.chats
            .get_or_init(|| async move { !Checkpoints::is_git_repository(&data).await })
            .await
            .then_some(chats)
    }
    pub(crate) fn list(&self) -> Vec<HostProject> {
        self.projects
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub(crate) fn scripts(&self, project: &str) -> Vec<ProjectScript> {
        self.scripts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(project)
            .cloned()
            .unwrap_or_default()
    }
    /// Applies a project update and rereads the projects. The chats project is not
    /// registered, so its settings are stored on their own.
    pub(crate) async fn update(
        &self,
        project: &str,
        scripts: Option<Vec<ProjectScript>>,
        favicon_path: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        let rootless = project == CHATS_PROJECT && self.chats_root().await.is_some();
        self.store
            .update(project, scripts, favicon_path, rootless)
            .await?;
        self.refresh().await?;
        Ok(())
    }
    fn stored(&self, project: &str) -> Option<crate::projects::StoredProject> {
        self.stored
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(project)
            .cloned()
    }
    /// A project's root and saved icon path; an unknown project is looked up
    /// again after rereading the projects.
    pub(crate) async fn favicon_source(&self, project: &str) -> Option<(String, Option<String>)> {
        let find = || {
            self.list()
                .into_iter()
                .find(|listed| listed.id == project)
                .map(|listed| {
                    let saved = self.stored(project).and_then(|stored| stored.favicon_path);
                    (listed.root, saved)
                })
        };
        if let Some(found) = find() {
            return Some(found);
        }
        self.refresh().await.ok()?;
        find()
    }
    /// The project as clients see it, with the repository identity at hand; a
    /// missing or expired one resolves in the background.
    pub(crate) fn wire(&self, project: HostProject) -> Project {
        let stored = self.stored(&project.id);
        Project {
            favicon_path: stored
                .as_ref()
                .and_then(|stored| stored.favicon_path.clone()),
            created_at: stored.as_ref().map(|stored| stored.created_at.clone()),
            updated_at: stored.map(|stored| stored.updated_at),
            repository_identity: (project.id != CHATS_PROJECT)
                .then(|| self.identities.available(&project.root))
                .flatten(),
            scripts: self.scripts(&project.id),
            id: project.id,
            name: project.name,
            roots: vec![ProjectRoot { path: project.root }],
        }
    }
    /// Rereads the registered projects; the first root is the project's root.
    pub(crate) async fn refresh(&self) -> anyhow::Result<Vec<HostProject>> {
        let chats = self.chats_root().await;
        let stored = self.store.load().await?;
        *self
            .scripts
            .write()
            .unwrap_or_else(|error| error.into_inner()) = stored
            .iter()
            .map(|project| (project.id.clone(), project.scripts.clone()))
            .collect();
        *self
            .stored
            .write()
            .unwrap_or_else(|error| error.into_inner()) = stored
            .iter()
            .map(|project| (project.id.clone(), project.clone()))
            .collect();
        let mut projects: Vec<HostProject> = stored
            .into_iter()
            .filter_map(|project| {
                Some(HostProject {
                    root: project.roots.first()?.path.clone(),
                    id: project.id,
                    name: project.name,
                })
            })
            .collect();
        if let Some(chats) = chats {
            crate::platform::create_state_directory(&chats)?;
            projects.push(HostProject {
                id: CHATS_PROJECT.into(),
                name: "Chats".into(),
                root: chats.to_string_lossy().into_owned(),
            });
        }
        *self
            .projects
            .write()
            .unwrap_or_else(|error| error.into_inner()) = projects.clone();
        Ok(projects)
    }
}

/// Programs for one-shot structured text generation (thread titles).
#[derive(Clone, Default)]
pub(crate) struct TextGenerator {
    pub(crate) codex: Option<PathBuf>,
    pub(crate) codex_home: Option<PathBuf>,
    pub(crate) claude: Option<(ClaudeProgram, Arc<dyn super::ClaudeCredentials>)>,
    /// The settings owner is read for every generation so a provider-instance
    /// executable or home change applies to the next title/Git request.
    pub(crate) worktrees: Option<Arc<Worktrees>>,
}

/// The default Codex text-generation model and its reasoning effort.
const CODEX_TEXT_MODEL: &str = "gpt-6-luna";
const CODEX_TEXT_EFFORT: &str = "low";
/// The default Claude text-generation model.
const CLAUDE_TEXT_MODEL: &str = "claude-haiku-4-5";
const TEXT_TIMEOUT: Duration = Duration::from_secs(180);

impl TextGenerator {
    pub(crate) fn generation_settings(
        &self,
        project: &str,
        operation: &str,
    ) -> TextGenerationSettings {
        self.worktrees
            .as_ref()
            .map(|worktrees| {
                resolve_text_generation_settings(
                    &worktrees.latest_host_settings(),
                    project,
                    operation,
                )
            })
            .unwrap_or_default()
    }

    pub(crate) async fn generate(&self, request: TextGenerationRequest) -> Result<String, String> {
        let driver = request.model.as_ref().map(|selection| selection.driver);
        let configured = request.model.as_ref().and_then(|selection| {
            self.worktrees.as_ref().and_then(|worktrees| {
                worktrees
                    .latest_host_settings()
                    .provider_instances
                    .get(&selection.instance)
                    .cloned()
            })
        });
        if let Some(selection) = request.model.as_ref() {
            if configured.is_none()
                && selection.instance != "codex"
                && selection.instance != "claude"
            {
                return Err(format!(
                    "The selected provider instance {} is not configured.",
                    selection.instance
                ));
            }
            if configured
                .as_ref()
                .is_some_and(|config| Some(config.driver) != driver)
            {
                return Err(format!(
                    "The selected provider instance {} uses a different driver.",
                    selection.instance
                ));
            }
        }
        let configured = configured.filter(|config| Some(config.driver) == driver);
        if configured.as_ref().is_some_and(|config| !config.enabled) {
            return Err(format!(
                "The selected provider instance {} is disabled.",
                request
                    .model
                    .as_ref()
                    .map_or("unknown", |selection| selection.instance.as_str())
            ));
        }
        if (driver.is_none() || driver == Some(agent_domain::Driver::Codex))
            && let Some(program) = configured
                .as_ref()
                .and_then(|config| config.binary_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| self.codex.clone())
        {
            let home = configured
                .as_ref()
                .and_then(|config| config.home_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| self.codex_home.clone());
            let launch_args = configured
                .as_ref()
                .map_or(&[][..], |config| config.launch_args.as_slice());
            let environment = configured
                .as_ref()
                .map_or_else(std::collections::BTreeMap::new, |config| {
                    config.environment.clone()
                });
            return Self::codex(
                &program,
                home.as_deref(),
                launch_args,
                &environment,
                request,
            )
            .await;
        }
        if (driver.is_none() || driver == Some(agent_domain::Driver::Claude))
            && let Some(program) = configured
                .as_ref()
                .and_then(|config| config.binary_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| {
                    self.claude
                        .as_ref()
                        .map(|(claude, _)| claude.program.clone())
                })
        {
            let config_home = configured
                .as_ref()
                .and_then(|config| config.home_path.as_deref())
                .map(crate::projects::expand_home)
                .or_else(|| {
                    self.claude
                        .as_ref()
                        .map(|(claude, _)| claude.config_home.clone())
                });
            let Some(config_home) = config_home else {
                return Err("Claude needs a configuration directory for text generation.".into());
            };
            let use_base_credentials = configured
                .as_ref()
                .is_none_or(|config| config.home_path.is_none());
            let credentials = self
                .claude
                .as_ref()
                .filter(|_| use_base_credentials)
                .map(|(_, credentials)| credentials.as_ref());
            return Self::claude(
                &ClaudeProgram {
                    program,
                    config_home,
                    environment: configured
                        .as_ref()
                        .map_or_else(std::collections::BTreeMap::new, |config| {
                            config.environment.clone()
                        }),
                    launch_args: configured
                        .as_ref()
                        .map_or_else(Vec::new, |config| config.launch_args.clone()),
                },
                credentials,
                request,
            )
            .await;
        }
        Err(format!(
            "The selected text-generation provider is unavailable: {}.",
            driver.map_or("none", |driver| match driver {
                agent_domain::Driver::Codex => "Codex",
                agent_domain::Driver::Claude => "Claude",
            })
        ))
    }

    /// `codex exec` with an output schema, prompt on stdin.
    async fn codex(
        program: &Path,
        codex_home: Option<&Path>,
        launch_args: &[String],
        environment: &std::collections::BTreeMap<String, String>,
        request: TextGenerationRequest,
    ) -> Result<String, String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let schema = directory.path().join("schema.json");
        let output = directory.path().join("output.json");
        std::fs::write(&schema, request.output_schema.to_string()).map_err(|e| e.to_string())?;
        let model = request
            .model
            .as_ref()
            .filter(|selection| selection.driver == agent_domain::Driver::Codex)
            .map_or(CODEX_TEXT_MODEL, |selection| selection.model.as_str());
        let effort = request
            .model
            .as_ref()
            .filter(|selection| selection.driver == agent_domain::Driver::Codex)
            .and_then(|selection| selection.options.get("reasoningEffort"))
            .map_or(CODEX_TEXT_EFFORT, String::as_str);
        let mut args: Vec<String> = [
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--model",
            model,
            "--config",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(format!("model_reasoning_effort=\"{effort}\""));
        args.extend(["--output-schema".into(), schema.to_string_lossy().into()]);
        args.extend([
            "--output-last-message".into(),
            output.to_string_lossy().into(),
        ]);
        args.extend(launch_args.iter().cloned());
        for attachment in &request.attachments {
            if attachment.kind == AttachmentKind::Image && Path::new(&attachment.path).is_file() {
                args.extend(["--image".into(), attachment.path.clone()]);
            }
        }
        args.push("-".into());
        let mut command = bex_process::command(program).map_err(|e| e.to_string())?;
        command.envs(environment);
        if let Some(home) = codex_home {
            command.env("CODEX_HOME", home);
        }
        let cwd = Some(PathBuf::from(&request.cwd))
            .filter(|cwd| cwd.is_dir())
            .unwrap_or_else(|| directory.path().to_path_buf());
        command.args(&args).current_dir(cwd);
        run_with_stdin(command, &generation_prompt(&request))
            .await
            .map_err(|detail| format!("Codex CLI command failed: {detail}"))?;
        std::fs::read_to_string(&output).map_err(|_| "Failed to read Codex output file.".into())
    }

    /// `claude -p` with a JSON schema and no tools.
    async fn claude(
        program: &ClaudeProgram,
        credentials: Option<&dyn super::ClaudeCredentials>,
        request: TextGenerationRequest,
    ) -> Result<String, String> {
        let home = match credentials {
            Some(credentials) => credentials.claude_home().await?,
            None => program.config_home.clone(),
        };
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let args: Vec<String> = vec![
            "-p".into(),
            "--output-format".into(),
            "json".into(),
            "--json-schema".into(),
            request.output_schema.to_string(),
            "--model".into(),
            request
                .model
                .as_ref()
                .filter(|selection| selection.driver == agent_domain::Driver::Claude)
                .map_or_else(
                    || CLAUDE_TEXT_MODEL.to_owned(),
                    |selection| selection.model.clone(),
                ),
            "--settings".into(),
            r#"{"disableAllHooks":true}"#.into(),
            "--tools".into(),
            String::new(),
            "--disable-slash-commands".into(),
            "--strict-mcp-config".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
        ];
        let command = program
            .command(&args, &home, directory.path())
            .map_err(|e| e.to_string())?;
        let stdout = run_with_stdin(command, &generation_prompt(&request))
            .await
            .map_err(|detail| format!("Claude CLI command failed: {detail}"))?;
        let output: Value = serde_json::from_str(&stdout)
            .map_err(|_| "Claude CLI returned unexpected output format.".to_owned())?;
        let envelope = match &output {
            Value::Array(messages) => messages
                .iter()
                .rev()
                .find(|message| message["type"] == "result")
                .cloned()
                .unwrap_or_default(),
            _ => output,
        };
        Ok(envelope["structured_output"].to_string())
    }
}

fn generation_prompt(request: &TextGenerationRequest) -> String {
    let Some(instructions) = request
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
    else {
        return request.prompt.clone();
    };
    format!(
        "{}\n\nAdditional source-control instructions:\n{}",
        request.prompt, instructions
    )
}

/// Runs a supervised CLI with `input` on stdin and returns its stdout.
async fn run_with_stdin(
    mut command: tokio::process::Command,
    input: &str,
) -> Result<String, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("stdin missing")?;
    let input = input.to_owned();
    let writing = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let output = tokio::time::timeout(TEXT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "request timed out.".to_owned())?
        .map_err(|e| e.to_string())?;
    let _ = writing.await;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if !stderr.is_empty() {
            stderr
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_owned()
        } else {
            format!("exited with {}", output.status)
        });
    }
    Ok(stdout)
}

/// Restored checkpoint files; their Git work runs off the async workers.
struct Restore(RestoredFiles);
impl PreparedRestore for Restore {
    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || self.0.commit().map_err(|e| format!("{e:#}")))
                .await
                .map_err(|e| e.to_string())?
        })
    }
    fn undo(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || self.0.undo().map_err(|e| format!("{e:#}")))
                .await
                .map_err(|e| e.to_string())?
        })
    }
}

/// A project's overrides over the Host's values.
fn resolve_settings(
    saved: &agent_protocol::models::HostSettings,
    project: &str,
) -> ConversationSettings {
    let overrides = saved.project_overrides.get(project);
    let auto_settle = overrides
        .and_then(|project| project.auto_settle)
        .unwrap_or(saved.auto_settle);
    ConversationSettings {
        auto_settle_after_days: match auto_settle {
            AutoSettle::Never => None,
            AutoSettle::AfterDays(days) => Some(days.into()),
        },
        auto_settle_on_merge: overrides
            .and_then(|project| project.auto_settle_on_merge)
            .unwrap_or(saved.auto_settle_on_merge),
        continue_after_restart: overrides
            .and_then(|project| project.continue_after_restart)
            .unwrap_or(saved.continue_after_restart),
        snooze_limited_threads: saved.snooze_limited_threads,
        auto_resume_limited_threads: saved.auto_resume_limited_threads,
    }
}

/// The branch naming of `project`: its overrides, then the Host's settings.
fn resolve_branch_naming(
    saved: &agent_protocol::models::HostSettings,
    project: &str,
) -> BranchNaming {
    let overrides = saved.project_overrides.get(project);
    let text = |value: Option<&String>, inherited: &str| {
        value.map_or(inherited, String::as_str).to_owned()
    };
    BranchNaming {
        mode: overrides
            .and_then(|project| project.branch_naming_mode)
            .unwrap_or(saved.branch_naming_mode),
        prefix: text(
            overrides.and_then(|project| project.branch_name_prefix.as_ref()),
            &saved.branch_name_prefix,
        ),
        instructions: text(
            overrides.and_then(|project| project.branch_name_instructions.as_ref()),
            &saved.branch_name_instructions,
        ),
    }
}

/// The submodule depth for a new worktree: project override, Host preference,
/// then recursive initialization.
fn resolve_worktree_submodules(
    saved: &agent_protocol::models::HostSettings,
    project: &str,
) -> WorktreeSubmodules {
    saved
        .project_overrides
        .get(project)
        .and_then(|project| project.worktree_submodules)
        .or(saved.worktree_submodules)
        .unwrap_or(WorktreeSubmodules::Recursive)
}

fn resolve_default_auto_pull(saved: &agent_protocol::models::HostSettings, project: &str) -> bool {
    saved
        .project_overrides
        .get(project)
        .and_then(|project| project.default_auto_pull)
        .unwrap_or(saved.default_auto_pull)
}

fn resolve_text_generation_settings(
    saved: &agent_protocol::models::HostSettings,
    project: &str,
    operation: &str,
) -> TextGenerationSettings {
    let overrides = saved.project_overrides.get(project);
    let text_model = overrides
        .and_then(|project| project.text_generation_model_selection.clone())
        .or_else(|| saved.text_generation_model_selection.clone());
    let source_style = overrides
        .and_then(|project| project.source_control_writing_style.clone())
        .unwrap_or_else(|| saved.source_control_writing_style.clone());
    let writer_model =
        match overrides.and_then(|project| project.source_control_writer_model_selection.clone()) {
            Some(agent_protocol::models::Nullable::Value(model)) => Some(model),
            Some(agent_protocol::models::Nullable::Null) => text_model.clone(),
            None => saved
                .source_control_writer_model_selection
                .clone()
                .or(text_model.clone()),
        };
    if operation == "generateThreadTitle" {
        return TextGenerationSettings {
            model: text_model,
            instructions: None,
        };
    }
    if operation == "generateBranchName" {
        return TextGenerationSettings {
            model: writer_model,
            instructions: None,
        };
    }
    let mut instructions = match source_style.mode {
        SourceControlWritingStyleMode::RepoConventions => {
            "Follow the repository's established source-control writing conventions.".to_owned()
        }
        SourceControlWritingStyleMode::ConventionalCommits => {
            "Use Conventional Commits style where it fits the generated source-control text."
                .to_owned()
        }
        SourceControlWritingStyleMode::Custom => source_style.custom_instructions,
    };
    if source_style.follow_change_request_templates {
        instructions.push_str(
            " When source-control text has a change-request template, preserve and fill its sections.",
        );
    }
    let instructions = (!instructions.trim().is_empty()).then_some(instructions);
    TextGenerationSettings {
        model: writer_model,
        instructions,
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use agent_domain::{BranchNamingMode, Driver, ModelSelection};
    use agent_protocol::models::{
        Nullable, ProjectSettingsOverrides, SourceControlWritingStyle,
        SourceControlWritingStyleMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn project_overrides_take_precedence_and_absent_values_inherit() {
        let mut saved = agent_protocol::models::HostSettings::default();
        assert_eq!(
            resolve_settings(&saved, "any"),
            ConversationSettings::default()
        );
        saved.auto_settle = AutoSettle::Never;
        saved.auto_settle_on_merge = false;
        saved.auto_resume_limited_threads = true;
        saved.project_overrides.insert(
            "opted-in".into(),
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::AfterDays(2)),
                auto_settle_on_merge: Some(true),
                continue_after_restart: Some(true),
                branch_naming_mode: Some(BranchNamingMode::Custom),
                branch_name_instructions: Some("Use ABC-123.".into()),
                ..Default::default()
            },
        );
        saved.branch_name_prefix = "team/".into();
        let inherited = resolve_settings(&saved, "other");
        assert_eq!(inherited.auto_settle_after_days, None);
        assert!(!inherited.auto_settle_on_merge);
        assert!(!inherited.continue_after_restart && inherited.auto_resume_limited_threads);
        let opted_in = resolve_settings(&saved, "opted-in");
        assert_eq!(opted_in.auto_settle_after_days, Some(2));
        assert!(opted_in.auto_settle_on_merge);
        assert!(opted_in.continue_after_restart);
        assert_eq!(
            resolve_branch_naming(&saved, "other"),
            BranchNaming {
                mode: BranchNamingMode::Static,
                prefix: "team/".into(),
                instructions: String::new(),
            }
        );
        assert_eq!(
            resolve_branch_naming(&saved, "opted-in"),
            BranchNaming {
                mode: BranchNamingMode::Custom,
                prefix: "team/".into(),
                instructions: "Use ABC-123.".into(),
            }
        );
        assert_eq!(
            resolve_branch_naming(&Default::default(), "any"),
            BranchNaming::default()
        );
    }

    #[test]
    fn worktree_submodules_resolve_project_then_host_then_recursive() {
        let mut saved = agent_protocol::models::HostSettings::default();
        assert_eq!(
            resolve_worktree_submodules(&saved, "project"),
            WorktreeSubmodules::Recursive
        );
        saved.worktree_submodules = Some(WorktreeSubmodules::TopLevel);
        assert_eq!(
            resolve_worktree_submodules(&saved, "project"),
            WorktreeSubmodules::TopLevel
        );
        saved.project_overrides.insert(
            "project".into(),
            ProjectSettingsOverrides {
                worktree_submodules: Some(WorktreeSubmodules::None),
                ..Default::default()
            },
        );
        assert_eq!(
            resolve_worktree_submodules(&saved, "project"),
            WorktreeSubmodules::None
        );
    }

    #[test]
    fn automatic_pull_resolves_a_project_override() {
        let mut saved = agent_protocol::models::HostSettings::default();
        saved.default_auto_pull = true;
        assert!(resolve_default_auto_pull(&saved, "other"));
        saved.project_overrides.insert(
            "project".into(),
            ProjectSettingsOverrides {
                default_auto_pull: Some(false),
                ..Default::default()
            },
        );
        assert!(!resolve_default_auto_pull(&saved, "project"));
    }

    #[test]
    fn text_generation_resolves_title_and_source_control_models_separately() {
        let model = |driver: Driver, name: &str| ModelSelection {
            instance: name.into(),
            driver,
            model: name.into(),
            options: BTreeMap::new(),
        };
        let mut saved = agent_protocol::models::HostSettings {
            text_generation_model_selection: Some(model(Driver::Codex, "title")),
            source_control_writer_model_selection: Some(model(Driver::Claude, "writer")),
            source_control_writing_style: SourceControlWritingStyle {
                mode: SourceControlWritingStyleMode::Custom,
                custom_instructions: "Use short imperative sentences.".into(),
                follow_change_request_templates: true,
            },
            ..Default::default()
        };
        saved.project_overrides.insert(
            "project".into(),
            ProjectSettingsOverrides {
                text_generation_model_selection: Some(model(Driver::Codex, "project-title")),
                source_control_writer_model_selection: Some(Nullable::Value(model(
                    Driver::Claude,
                    "project-writer",
                ))),
                ..Default::default()
            },
        );
        let title = resolve_text_generation_settings(&saved, "project", "generateThreadTitle");
        assert_eq!(
            title.model.as_ref().map(|model| model.model.as_str()),
            Some("project-title")
        );
        assert_eq!(title.instructions, None);
        let branch = resolve_text_generation_settings(&saved, "project", "generateBranchName");
        assert_eq!(
            branch.model.as_ref().map(|model| model.model.as_str()),
            Some("project-writer")
        );
        assert_eq!(branch.instructions, None);
        let commit = resolve_text_generation_settings(&saved, "other", "generateCommitMessage");
        assert_eq!(
            commit.model.as_ref().map(|model| model.model.as_str()),
            Some("writer")
        );
        let mut inherited = saved.clone();
        inherited.project_overrides.insert(
            "project".into(),
            ProjectSettingsOverrides {
                text_generation_model_selection: Some(model(Driver::Codex, "project-title")),
                source_control_writer_model_selection: Some(Nullable::Null),
                ..Default::default()
            },
        );
        let project_text_writer =
            resolve_text_generation_settings(&inherited, "project", "generateCommitMessage");
        assert_eq!(
            project_text_writer
                .model
                .as_ref()
                .map(|model| model.model.as_str()),
            Some("project-title")
        );
        assert!(
            project_text_writer.instructions.as_deref().is_some_and(
                |instructions| instructions.contains("Use short imperative sentences.")
            )
        );
        assert!(
            generation_prompt(&TextGenerationRequest {
                operation: "generateCommitMessage",
                project: "project".into(),
                model: commit.model,
                instructions: commit.instructions,
                cwd: "/tmp/project".into(),
                prompt: "Write a commit message.".into(),
                attachments: vec![],
                output_schema: serde_json::json!({}),
            })
            .contains("Use short imperative sentences.")
        );
    }
}

pub(crate) struct HostIo {
    pub(crate) projects: Arc<ProjectCatalog>,
    pub(crate) checkpoints: Arc<Checkpoints>,
    pub(crate) worktrees: Arc<Worktrees>,
    pub(crate) files: WorkspaceFiles,
    pub(crate) terminals: Arc<Terminals>,
    pub(crate) text: TextGenerator,
}

fn error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

impl HostOperations for HostIo {
    fn projects(&self) -> Vec<HostProject> {
        self.projects.list()
    }
    fn settings(&self, project: &str) -> ConversationSettings {
        resolve_settings(&self.worktrees.latest_host_settings(), project)
    }
    fn branch_naming(&self, project: &str) -> BranchNaming {
        resolve_branch_naming(&self.worktrees.latest_host_settings(), project)
    }
    fn text_generation_settings(&self, project: &str, operation: &str) -> TextGenerationSettings {
        let host = self.worktrees.latest_host_settings();
        let mut settings = resolve_text_generation_settings(&host, project, operation);
        if settings.model.as_ref().is_some_and(|selection| {
            let configured = host.provider_instances.get(&selection.instance);
            (configured.is_none()
                && selection.instance != "codex"
                && selection.instance != "claude")
                || configured.is_some_and(|config| !config.enabled)
                || configured.is_some_and(|config| config.driver != selection.driver)
                || match selection.driver {
                    agent_domain::Driver::Codex => {
                        self.text.codex.is_none()
                            && configured
                                .and_then(|config| config.binary_path.as_ref())
                                .is_none()
                    }
                    agent_domain::Driver::Claude => {
                        if self.text.claude.is_some() {
                            false
                        } else {
                            configured.is_none_or(|config| {
                                config.binary_path.is_none() || config.home_path.is_none()
                            })
                        }
                    }
                }
        }) {
            settings.model = None;
        }
        settings
    }
    fn rename_branch(
        &self,
        cwd: String,
        old: String,
        new: String,
        exact: bool,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.worktrees
                .rename_branch(cwd, old, new, exact)
                .await
                .map_err(error)
        })
    }
    fn real_path(&self, path: String) -> BoxFuture<'_, io::Result<Option<String>>> {
        Box::pin(async move {
            match tokio::fs::canonicalize(&path).await {
                Ok(real) => Ok(Some(
                    dunce::simplified(&real).to_string_lossy().into_owned(),
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
    fn is_git_repository(&self, cwd: String) -> BoxFuture<'_, bool> {
        Box::pin(async move { Checkpoints::is_git_repository(Path::new(&cwd)).await })
    }
    fn capture_checkpoint(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.checkpoints
                .capture(Path::new(&cwd), &reference)
                .await
                .map_err(error)
        })
    }
    fn has_checkpoint(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<bool, String>> {
        Box::pin(async move {
            self.checkpoints
                .has(Path::new(&cwd), &reference)
                .await
                .map_err(error)
        })
    }
    fn checkpoint_files(
        &self,
        cwd: String,
        from: String,
        to: String,
    ) -> BoxFuture<'_, Result<Vec<CheckpointFile>, String>> {
        Box::pin(async move {
            self.checkpoints
                .files(Path::new(&cwd), &from, &to)
                .await
                .map_err(error)
        })
    }
    fn prepare_restore(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<Box<dyn PreparedRestore>, String>> {
        Box::pin(async move {
            let restored = self
                .checkpoints
                .prepare_restore(Path::new(&cwd), &reference)
                .await
                .map_err(error)?;
            Ok(Box::new(Restore(restored)) as Box<dyn PreparedRestore>)
        })
    }
    fn delete_checkpoints(
        &self,
        cwd: String,
        references: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.checkpoints
                .delete(Path::new(&cwd), &references)
                .await
                .map_err(error)
        })
    }
    fn create_worktree(
        &self,
        request: WorktreeRequest,
    ) -> BoxFuture<'_, Result<CreatedWorktree, String>> {
        Box::pin(async move {
            let saved = self.worktrees.latest_host_settings();
            let auto_pull = resolve_default_auto_pull(&saved, &request.project);
            let submodules = resolve_worktree_submodules(&saved, &request.project);
            let (path, branch) = self
                .worktrees
                .create_with_submodules(
                    request.thread.as_str(),
                    &request.project_root,
                    &request.base_ref,
                    request.branch,
                    request.start_from_origin,
                    auto_pull,
                    submodules,
                    request.progress,
                    request.cancel,
                )
                .await
                .map_err(error)?;
            Ok(CreatedWorktree {
                path: path.to_string_lossy().into_owned(),
                branch: Some(branch),
            })
        })
    }
    fn remove_worktree(
        &self,
        _project_root: String,
        path: String,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.worktrees.abandon(path).await.map_err(error) })
    }
    fn thread_folder(
        &self,
        project: String,
        thread: ThreadId,
        text: String,
    ) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            if project != CHATS_PROJECT {
                return Ok(None);
            }
            let Some(root) = self.projects.chats_root().await else {
                return Ok(None);
            };
            let date = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .format("%Y-%m-%d")
                .to_string();
            let claimed = tokio::task::spawn_blocking(move || {
                crate::projects::claim_thread_folder(&root, thread.as_str(), &text, &date)
            })
            .await;
            match claimed {
                Ok(Ok(folder)) => Ok(Some(folder.to_string_lossy().into_owned())),
                failure => {
                    tracing::warn!(?failure, "could not create a chat thread folder");
                    Err("Failed to create the folder for threads without a project.".into())
                }
            }
        })
    }
    fn delete_attachments(
        &self,
        _thread: ThreadId,
        paths: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.files.delete_claimed(paths).await.map_err(error) })
    }
    /// Runs the setup script in the thread's `setup-{id}` terminal.
    fn run_setup(&self, request: SetupRequest) -> BoxFuture<'_, Result<SetupRun, String>> {
        Box::pin(async move {
            let scripts = self.projects.scripts(&request.project);
            let Some(script) = super::setup::setup_script(&scripts) else {
                return Ok(SetupRun::NoScript);
            };
            super::setup::run(&self.terminals, &request, script)
                .await
                .map(SetupRun::Started)
        })
    }
    fn cleanup_terminals(&self, thread: ThreadId) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.terminals.close_thread(&thread).await;
            Ok(())
        })
    }
    fn finish_restore(&self, cwd: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.checkpoints
                .finish_restore(Path::new(&cwd))
                .await
                .map_err(error)
        })
    }
    fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(self.text.generate(request))
    }
    fn title_link_context(&self, cwd: String, links: Vec<String>) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            super::title_links::title_link_context(&cwd, &links, &super::title_links::resolve_link)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    // ManagedProjectFolders.test.ts "offers a Scratch folder under the data dir when
    // it is outside a checkout" and "creates one Scratch project".
    #[tokio::test]
    async fn offers_chats_under_the_data_directory_outside_a_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let state = dunce::canonicalize(directory.path()).unwrap();
        let catalog = Arc::new(ProjectCatalog::new(ProjectStore::new(
            state.join("worktrees.json"),
        )));
        let refreshed = futures_util::future::join_all((0..8).map(|_| {
            let catalog = catalog.clone();
            async move { catalog.refresh().await.unwrap() }
        }))
        .await;
        for projects in refreshed.into_iter().chain([catalog.list()]) {
            let chats: Vec<_> = projects
                .iter()
                .filter(|project| project.id == CHATS_PROJECT)
                .collect();
            assert_eq!(chats.len(), 1);
            assert_eq!(Path::new(&chats[0].root), state.join("chats"));
        }
        assert!(state.join("chats").is_dir());
    }

    // "offers nothing when the data dir sits inside a Git checkout"
    #[tokio::test]
    async fn offers_no_chats_when_the_data_directory_sits_inside_a_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = dunce::canonicalize(directory.path()).unwrap();
        git(&checkout, &["init", "--quiet"]);
        let state = checkout.join(".state");
        std::fs::create_dir(&state).unwrap();
        let catalog = ProjectCatalog::new(ProjectStore::new(state.join("worktrees.json")));
        assert_eq!(catalog.chats_root().await, None);
        assert!(
            catalog
                .refresh()
                .await
                .unwrap()
                .iter()
                .all(|project| project.id != CHATS_PROJECT)
        );
        assert!(!state.join("chats").exists());
    }
}
