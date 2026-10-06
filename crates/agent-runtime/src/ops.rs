//! Host I/O the runtime's effect executors, recovery and launch depend on.
use agent_domain::{Attachment, RunId, ThreadId};
use futures_util::future::BoxFuture;
use std::io;

/// A registered project.
#[derive(Debug, Clone, PartialEq)]
pub struct HostProject {
    pub id: String,
    pub root: String,
    /// The project row sent to shell subscribers.
    pub payload: serde_json::Value,
}

/// A worktree the Host checks out for a launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRequest {
    pub thread: ThreadId,
    pub project: String,
    pub project_root: String,
    pub base_ref: String,
    /// `None` lets the Host name the branch.
    pub branch: Option<String>,
    pub start_from_origin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub path: String,
    pub branch: Option<String>,
}

/// The project's setup for a thread's workspace, run before its first turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    pub thread: ThreadId,
    pub project: String,
    pub project_root: String,
    pub cwd: String,
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
    /// Whether a run cut by a Host restart continues afterwards.
    fn continue_after_restart(&self, project: &str) -> bool;

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
    fn remove_worktree(
        &self,
        project_root: String,
        path: String,
    ) -> BoxFuture<'_, Result<(), String>>;
    fn run_setup(&self, _request: SetupRequest) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
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
