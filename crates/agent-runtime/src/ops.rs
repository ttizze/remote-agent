//! Host I/O the runtime's effect executors, recovery and launch depend on.
use agent_domain::{
    Attachment, CheckpointFile, RunId, ThreadId, WorktreeSetupStageId, WorktreeSetupStageStatus,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;

/// What the Host reports while it prepares a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupEvent {
    Stage(WorktreeSetupStageId, WorktreeSetupStageStatus),
    /// A cleaned output line of the setup script.
    Output(String),
}

/// Where a tracked preparation's progress goes; empty for an untracked one.
#[derive(Clone, Default)]
pub struct SetupProgress(Option<Arc<dyn Fn(SetupEvent) + Send + Sync>>);
impl SetupProgress {
    pub fn new(report: impl Fn(SetupEvent) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(report)))
    }
    pub fn tracked(&self) -> bool {
        self.0.is_some()
    }
    pub fn report(&self, event: SetupEvent) {
        if let Some(report) = &self.0 {
            report(event);
        }
    }
}
impl std::fmt::Debug for SetupProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.tracked() {
            "tracked"
        } else {
            "untracked"
        })
    }
}
impl PartialEq for SetupProgress {
    fn eq(&self, other: &Self) -> bool {
        self.tracked() == other.tracked()
    }
}
impl Eq for SetupProgress {}

/// A registered project, as shell subscribers see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProject {
    pub id: String,
    pub name: String,
    pub root: String,
}

/// A worktree the Host checks out for a launch.
#[derive(Debug, Clone)]
pub struct WorktreeRequest {
    pub thread: ThreadId,
    pub project: String,
    pub project_root: String,
    pub base_ref: String,
    /// `None` lets the Host name the branch.
    pub branch: Option<String>,
    pub start_from_origin: bool,
    /// Receives the fetch and checkout stages.
    pub progress: SetupProgress,
    /// Stops the checkout: the Host stops its Git command and removes what it
    /// created before the call returns.
    pub cancel: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub path: String,
    pub branch: Option<String>,
}

/// The project's setup for a thread's workspace, started before its first turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    pub thread: ThreadId,
    pub project: String,
    pub project_root: String,
    pub cwd: String,
    /// Set for a launch that prepares a worktree: the Host forwards the script's
    /// output and returns its completion.
    pub observe: SetupProgress,
}

/// A setup script the Host started.
pub struct StartedSetup {
    pub name: String,
    pub command: String,
    /// False when the agent waits for the script.
    pub run_async: bool,
    /// Observed runs only: the exit code, `None` when the script was stopped.
    /// Dropping it before the script exits stops the script.
    pub completion: Option<BoxFuture<'static, Option<i32>>>,
}

pub enum SetupRun {
    NoScript,
    Started(StartedSetup),
}

/// Conversation settings for one project, with project overrides applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSettings {
    /// Settle a thread this many days after its last activity; `None` never.
    pub auto_settle_after_days: Option<u64>,
    /// Continue a turn a Host restart cut.
    pub continue_after_restart: bool,
    pub snooze_limited_threads: bool,
    pub auto_resume_limited_threads: bool,
}
impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            auto_settle_after_days: Some(3),
            continue_after_restart: false,
            snooze_limited_threads: false,
            auto_resume_limited_threads: false,
        }
    }
}

/// One structured text generation call (thread titles).
#[derive(Debug, Clone, PartialEq)]
pub struct TextGenerationRequest {
    pub operation: &'static str,
    pub project: String,
    pub cwd: String,
    pub prompt: String,
    pub attachments: Vec<Attachment>,
    /// JSON Schema of the expected output object.
    pub output_schema: serde_json::Value,
}

/// Restored checkpoint files staged in place while the originals are kept aside.
/// Dropping it without `commit` or `undo` (a cancelled effect) puts the originals back.
pub trait PreparedRestore: Send {
    /// Keeps the restored files and discards the originals. A failed commit still
    /// keeps the restored files; `HostOperations::finish_restore` discards the rest.
    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), String>>;
    /// Puts the original files back.
    fn undo(self: Box<Self>) -> BoxFuture<'static, Result<(), String>>;
}

/// Host I/O behind the conversation runtime. Git checkpoint references are hidden
/// refs named by the runtime (`checkpoint_reference`).
pub trait HostOperations: Send + Sync {
    fn projects(&self) -> Vec<HostProject>;
    fn project(&self, id: &str) -> Option<HostProject> {
        self.projects().into_iter().find(|project| project.id == id)
    }
    /// The settings that apply to threads of `project`.
    fn settings(&self, _project: &str) -> ConversationSettings {
        ConversationSettings::default()
    }
    /// How launches into `project` name the worktree branches they generate.
    fn branch_naming(&self, _project: &str) -> agent_domain::BranchNaming {
        agent_domain::BranchNaming::default()
    }
    /// Renames the branch checked out at `cwd` from `old` to `new`, or, unless
    /// `exact`, to the first of `new`, `new-1` … `new-100` no branch has.
    /// Returns the name it got.
    fn rename_branch(
        &self,
        _cwd: String,
        _old: String,
        _new: String,
        _exact: bool,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async { Err("branch renames are unavailable".into()) })
    }
    /// Whether a run cut by a Host restart continues afterwards.
    fn continue_after_restart(&self, project: &str) -> bool {
        self.settings(project).continue_after_restart
    }

    /// The canonical path, or `None` when nothing exists there.
    fn real_path(&self, path: String) -> BoxFuture<'_, io::Result<Option<String>>>;

    /// False also when Git cannot tell.
    fn is_git_repository(&self, cwd: String) -> BoxFuture<'_, bool>;
    /// Snapshots the checkout (tracked and untracked files) into `reference`
    /// without touching the user's index or HEAD.
    fn capture_checkpoint(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<(), String>>;
    fn has_checkpoint(&self, cwd: String, reference: String)
    -> BoxFuture<'_, Result<bool, String>>;
    /// Files changed from `from` to `to`, sorted by path.
    fn checkpoint_files(
        &self,
        cwd: String,
        from: String,
        to: String,
    ) -> BoxFuture<'_, Result<Vec<CheckpointFile>, String>>;
    /// Puts back the originals a previous process left aside before restoring.
    fn prepare_restore(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<Box<dyn PreparedRestore>, String>>;
    /// Discards originals that a restore in `cwd` still keeps aside because its
    /// process stopped, or its commit failed, after the rollback was recorded.
    /// Nothing happens when none are left.
    fn finish_restore(&self, cwd: String) -> BoxFuture<'_, Result<(), String>>;
    /// Missing references are ignored.
    fn delete_checkpoints(
        &self,
        cwd: String,
        references: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>>;

    /// Idempotent per `request.thread`: after a crash before the runtime recorded
    /// the result, the same request returns the checkout created for the thread.
    fn create_worktree(
        &self,
        request: WorktreeRequest,
    ) -> BoxFuture<'_, Result<CreatedWorktree, String>>;
    /// Removes a checkout a launch gives up, with whatever its setup changed;
    /// its branch stays.
    fn remove_worktree(
        &self,
        project_root: String,
        path: String,
    ) -> BoxFuture<'_, Result<(), String>>;
    /// Claims a new folder of its own for a thread launched at the root of a project
    /// whose threads each get one, named from `text`. `None` for every other
    /// project.
    fn thread_folder(
        &self,
        _project: String,
        _thread: ThreadId,
        _text: String,
    ) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }
    fn run_setup(&self, _request: SetupRequest) -> BoxFuture<'_, Result<SetupRun, String>> {
        Box::pin(async { Ok(SetupRun::NoScript) })
    }
    /// A run's checkpoint was captured; the Host refreshes what it shows of the workspace.
    fn run_finalized(&self, _thread: &ThreadId, _run: &RunId, _cwd: &str) {}

    /// Missing files are ignored.
    fn delete_attachments(
        &self,
        thread: ThreadId,
        paths: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>>;
    /// Closes the thread's terminals and deletes their history.
    fn cleanup_terminals(&self, thread: ThreadId) -> BoxFuture<'_, Result<(), String>>;

    /// The raw model output (a JSON object matching the schema, or plain text).
    fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> BoxFuture<'_, Result<String, String>>;
    /// Summaries of linked issues or pull requests, used as title context.
    fn title_link_context(
        &self,
        _cwd: String,
        _links: Vec<String>,
    ) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
    fn title_instructions(&self, _project: &str) -> Option<String> {
        None
    }
}
