//! Live progress of a launch that prepares a worktree. The Host keeps it in
//! memory only; clients render it as the setup card.
use crate::{ThreadId, Timestamp};
use serde::{Deserialize, Serialize};

pub const WORKTREE_SETUP_DETAIL_MAX_LENGTH: usize = 200;
pub const WORKTREE_SETUP_TAIL_LINE_MAX_LENGTH: usize = 400;
pub const WORKTREE_SETUP_ERROR_MAX_LENGTH: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeSetupStageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}
pub const WORKTREE_SETUP_STAGE_ORDER: [WorktreeSetupStageId; 5] = [
    WorktreeSetupStageId::Fetch,
    WorktreeSetupStageId::Checkout,
    WorktreeSetupStageId::Submodules,
    WorktreeSetupStageId::SetupScript,
    WorktreeSetupStageId::Agent,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeSetupStageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupStage {
    pub id: WorktreeSetupStageId,
    pub status: WorktreeSetupStageStatus,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// Only the checkout stage reports a percentage.
    pub percent: Option<u8>,
    /// Short trailing text: a file count, an exit code.
    pub detail: Option<String>,
    /// The last few output lines of the setup script, newest last.
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeSetupPhase {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupScript {
    pub name: String,
    pub command: String,
    /// The thread's terminal the script runs in.
    pub terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupSnapshot {
    pub thread: ThreadId,
    pub phase: WorktreeSetupPhase,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub worktree_path: Option<String>,
    pub setup_script: Option<WorktreeSetupScript>,
    pub stages: Vec<WorktreeSetupStage>,
    /// Why the setup failed.
    pub error: Option<String>,
    pub sequence: u64,
}
