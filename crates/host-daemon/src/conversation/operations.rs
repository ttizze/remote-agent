//! Host I/O behind the conversation runtime: projects, Git checkpoints, worktrees,
//! attachments, terminals and title text generation.
use crate::checkpoints::{Checkpoints, RestoredFiles};
use crate::claude::control::ClaudeProgram;
use crate::{
    ProjectStore, terminals::Terminals, workspace_files::WorkspaceFiles, worktrees::Worktrees,
};
use agent_domain::{AttachmentKind, ThreadId};
use agent_runtime::{
    CreatedWorktree, HostOperations, HostProject, PreparedRestore, TextGenerationRequest,
    WorktreeRequest,
};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

/// The always-present project for conversations outside a repository.
pub(crate) const CHATS_PROJECT: &str = "chats";

/// Registered projects, cached for the runtime's synchronous reads.
pub(crate) struct ProjectCatalog {
    store: ProjectStore,
    projects: RwLock<Vec<HostProject>>,
}

impl ProjectCatalog {
    pub(crate) fn new(store: ProjectStore) -> Self {
        Self {
            store,
            projects: RwLock::new(vec![]),
        }
    }
    pub(crate) fn store(&self) -> &ProjectStore {
        &self.store
    }
    pub(crate) fn list(&self) -> Vec<HostProject> {
        self.projects
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    /// Rereads the registered projects; the first root is the project's root.
    pub(crate) async fn refresh(&self) -> anyhow::Result<Vec<HostProject>> {
        let chats = self.store.chat_directory();
        crate::platform::create_state_directory(&chats)?;
        let mut projects: Vec<HostProject> = self
            .store
            .load()
            .await?
            .into_iter()
            .filter_map(|project| {
                Some(HostProject {
                    root: project.roots.first()?.path.clone(),
                    id: project.id,
                    name: project.name,
                })
            })
            .collect();
        projects.push(HostProject {
            id: CHATS_PROJECT.into(),
            name: "Chats".into(),
            root: chats.to_string_lossy().into_owned(),
        });
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
}

/// T3 `DEFAULT_TEXT_GENERATION_MODEL` and its reasoning effort.
const CODEX_TEXT_MODEL: &str = "gpt-6-luna";
const CODEX_TEXT_EFFORT: &str = "low";
/// T3 `DEFAULT_TEXT_GENERATION_MODEL_BY_PROVIDER` for Claude.
const CLAUDE_TEXT_MODEL: &str = "claude-haiku-4-5";
const TEXT_TIMEOUT: Duration = Duration::from_secs(180);

impl TextGenerator {
    async fn generate(&self, request: TextGenerationRequest) -> Result<String, String> {
        if let Some(codex) = &self.codex {
            return self.codex(codex, request).await;
        }
        if let Some((claude, credentials)) = &self.claude {
            return Self::claude(claude, credentials.as_ref(), request).await;
        }
        Err("No provider can generate text.".into())
    }

    /// T3 `CodexTextGeneration`: `codex exec` with an output schema, prompt on stdin.
    async fn codex(
        &self,
        program: &Path,
        request: TextGenerationRequest,
    ) -> Result<String, String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let schema = directory.path().join("schema.json");
        let output = directory.path().join("output.json");
        std::fs::write(&schema, request.output_schema.to_string()).map_err(|e| e.to_string())?;
        let mut args: Vec<String> = [
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--model",
            CODEX_TEXT_MODEL,
            "--config",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(format!("model_reasoning_effort=\"{CODEX_TEXT_EFFORT}\""));
        args.extend(["--output-schema".into(), schema.to_string_lossy().into()]);
        args.extend([
            "--output-last-message".into(),
            output.to_string_lossy().into(),
        ]);
        for attachment in &request.attachments {
            if attachment.kind == AttachmentKind::Image && Path::new(&attachment.path).is_file() {
                args.extend(["--image".into(), attachment.path.clone()]);
            }
        }
        args.push("-".into());
        let mut command = bex_process::command(program).map_err(|e| e.to_string())?;
        if let Some(home) = &self.codex_home {
            command.env("CODEX_HOME", home);
        }
        let cwd = Some(PathBuf::from(&request.cwd))
            .filter(|cwd| cwd.is_dir())
            .unwrap_or_else(|| directory.path().to_path_buf());
        command.args(&args).current_dir(cwd);
        run_with_stdin(command, &request.prompt)
            .await
            .map_err(|detail| format!("Codex CLI command failed: {detail}"))?;
        std::fs::read_to_string(&output).map_err(|_| "Failed to read Codex output file.".into())
    }

    /// T3 `ClaudeTextGeneration`: `claude -p` with a JSON schema and no tools.
    async fn claude(
        program: &ClaudeProgram,
        credentials: &dyn super::ClaudeCredentials,
        request: TextGenerationRequest,
    ) -> Result<String, String> {
        let home = credentials.claude_home().await?;
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let args: Vec<String> = vec![
            "-p".into(),
            "--output-format".into(),
            "json".into(),
            "--json-schema".into(),
            request.output_schema.to_string(),
            "--model".into(),
            CLAUDE_TEXT_MODEL.into(),
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
        let stdout = run_with_stdin(command, &request.prompt)
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
    /// T3 `continueThreadsAfterServerUpdate`, which defaults to off; the Host has no
    /// setting to turn it on.
    fn continue_after_restart(&self, _project: &str) -> bool {
        false
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
            let (path, branch) = self
                .worktrees
                .create(
                    &request.project_root,
                    &request.base_ref,
                    request.branch,
                    request.start_from_origin,
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
        Box::pin(async move { self.worktrees.remove(path, false).await.map_err(error) })
    }
    fn delete_attachments(
        &self,
        _thread: ThreadId,
        paths: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.files.delete_claimed(paths).await.map_err(error) })
    }
    fn cleanup_terminals(&self, thread: ThreadId) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.terminals
                .cleanup_handle(&agent_protocol::operations::thread_terminal_handle(
                    thread.as_str(),
                ))
                .await;
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
}
