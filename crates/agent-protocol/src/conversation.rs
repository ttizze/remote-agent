//! Conversation RPCs over agent-domain records. Clients send commands and fold the
//! facts the Host commits; every sequence is the Host's global fact sequence.
use crate::error::{Delivery, RpcFailure};
use crate::models::Project;
use agent_domain::{
    Attachment, Command, CommandId, Driver, Fact, InteractionMode, Item, Message, MessageContext,
    MessageId, ModelSelection, Plan, Reply, RuntimeMode, State, ThreadId, ThreadShell, Timestamp,
    TurnItemId, host_only_command,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Room left in a frame for the response and stream item envelopes around facts.
pub const FACTS_FRAME_BUDGET: usize = crate::protocol::MAX_FRAME_BYTES - 1024;

/// `conversation/dispatch`: one command for one thread, idempotent by `command_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dispatch {
    pub thread_id: ThreadId,
    pub command_id: CommandId,
    pub command: Command,
}
impl Dispatch {
    /// Host-only commands are refused before they reach the thread.
    pub fn validate(&self) -> Result<(), ConversationError> {
        if host_only_command(&self.command) {
            return Err(ConversationError::InternalCommand);
        }
        Ok(())
    }
}

/// The thread's reply once its facts are durable. Clients show the result after
/// `sequence` arrives on the thread stream; a rejection carries the current head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Committed {
    pub reply: Reply,
    pub thread_sequence: u64,
    pub sequence: u64,
    /// A resend of an already committed command; nothing new was written.
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WorkspaceStrategy {
    Root {
        branch: Option<String>,
    },
    ExistingWorktree {
        worktree_path: String,
        branch: Option<String>,
    },
    Worktree {
        base_ref: String,
        branch: Option<String>,
        start_from_origin: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMessage {
    /// Derived from the launch's command id when absent.
    pub id: Option<MessageId>,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub creation_source: String,
    /// Shown as the title while a title is generated from this message.
    pub title_seed: Option<String>,
    /// Inline context records the text links to (T3 `initialMessage.context`).
    pub context: Option<MessageContext>,
}

/// `conversation/launch`: create a thread, prepare its workspace and send the first
/// message. Resending the same `command_id` resumes the same launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub command_id: CommandId,
    /// Derived from `command_id` when absent.
    pub thread_id: Option<ThreadId>,
    pub project_id: String,
    pub title: String,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: WorkspaceStrategy,
    pub message: Option<LaunchMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launched {
    pub thread_id: ThreadId,
    /// The last commit of the launch: the first message, or the creation.
    pub committed: Committed,
    /// An earlier request with this command id already started the launch.
    pub resumed: bool,
}

/// `conversation/subscribeThread`. The first frame is the response; the stream
/// stays open with later updates until the client falls behind or disconnects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeThread {
    pub thread_id: ThreadId,
    /// The last sequence the client folded; `None` asks for a snapshot.
    pub after_sequence: Option<u64>,
    /// Send `Synchronized` once the snapshot or replay is out.
    pub request_completion_marker: bool,
    /// Accept a recent window plus a history cursor instead of the full projection.
    pub accept_bounded_snapshot: bool,
}

/// The older history a bounded snapshot leaves out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotWindow {
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_ordinal: Option<u64>,
    pub payload_budget_exceeded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshot {
    /// Facts after this sequence follow.
    pub snapshot_sequence: u64,
    pub thread_sequence: u64,
    pub state: Arc<State>,
    /// Present when the snapshot is bounded.
    pub window: Option<SnapshotWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedFact {
    pub sequence: u64,
    pub thread_sequence: u64,
    pub fact: Fact,
}

/// Snapshots, facts and history pages carry the delivery projection (T3
/// WireProjection): command and tool output is withheld with `output_omitted` /
/// `output_indicates_failure`, file bodies are dropped, subagent text is cut at
/// 32 KiB, and a fact that changes a tool item arrives as `ItemProjected`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadUpdate {
    /// Replaces what the client holds and resets its cursor.
    Snapshot(ThreadSnapshot),
    /// Facts in sequence order: one committed step, a replayed gap, or part of either.
    Facts(Vec<SequencedFact>),
    Synchronized,
    /// The last item: the stream ended, e.g. `live_buffer_full` when the client fell
    /// behind. Resubscribe from the last applied sequence.
    Failed(RpcFailure),
}

/// Groups facts in order into stream items that each fit `budget` encoded bytes.
/// Text is split where facts are created and tool output is withheld on delivery,
/// so a single fact fits a frame.
pub fn fact_updates(facts: Vec<SequencedFact>, budget: usize) -> Vec<ThreadUpdate> {
    let mut updates = vec![];
    let (mut batch, mut used) = (vec![], 0usize);
    for fact in facts {
        let size = postcard::experimental::serialized_size(&fact).unwrap_or(usize::MAX);
        if !batch.is_empty() && used.saturating_add(size) > budget {
            updates.push(ThreadUpdate::Facts(std::mem::take(&mut batch)));
            used = 0;
        }
        used = used.saturating_add(size);
        batch.push(fact);
    }
    if !batch.is_empty() {
        updates.push(ThreadUpdate::Facts(batch));
    }
    updates
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShellLocation {
    #[default]
    Active,
    Archived,
}

/// `conversation/subscribeShell`: the thread list of one location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeShell {
    /// The last sequence the client applied; `None` asks for a snapshot.
    pub after_sequence: Option<u64>,
    pub request_completion_marker: bool,
    pub location: ShellLocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub snapshot_sequence: u64,
    pub projects: Vec<Project>,
    pub threads: Vec<ThreadShell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ShellUpdate {
    Snapshot(ShellSnapshot),
    /// Every live project; the client drops the ones it holds that are missing.
    /// Opens a resumed stream, since project changes are not replayed.
    Projects {
        sequence: u64,
        projects: Vec<Project>,
    },
    ThreadUpdated {
        sequence: u64,
        thread: Box<ThreadShell>,
    },
    /// The thread left this location: archived, unarchived or deleted.
    ThreadRemoved {
        sequence: u64,
        thread_id: ThreadId,
    },
    ProjectUpdated {
        sequence: u64,
        project: Box<Project>,
    },
    ProjectRemoved {
        sequence: u64,
        project_id: String,
    },
    Synchronized,
    /// The last item: the stream ended, e.g. `live_buffer_full` when the client fell
    /// behind. Resubscribe from the last applied sequence.
    Failed(RpcFailure),
}

/// `conversation/getThread`: the projection a subscription would start from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetThread {
    pub thread_id: ThreadId,
    pub bounded: bool,
}

/// One visible timeline item with its message and plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    pub position: u64,
    /// The thread that owns the item; a fork parent for inherited items.
    pub source: ThreadId,
    pub inherited: bool,
    pub item: Item,
    pub message: Option<Message>,
    pub plan: Option<Plan>,
}

/// `conversation/getTurnItem`: one item with the output the timeline withholds
/// (each part bounded to 256 KiB), or `None` when it is not visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurnItem {
    pub thread_id: ThreadId,
    pub item_id: TurnItemId,
}

/// `conversation/readHistory`: the page before `cursor`, or the newest page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadHistory {
    pub thread_id: ThreadId,
    /// Opaque; from a bounded snapshot or an earlier page.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub rows: Vec<HistoryRow>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// `conversation/search`: 2 to 200 UTF-16 units, at most 50 matches (default 50).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchSource {
    User,
    Assistant,
}

/// The best finished message of one thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub thread_id: ThreadId,
    pub project_id: String,
    pub source: SearchSource,
    /// At most 240 UTF-16 units around the first match.
    pub snippet: String,
    pub message_created_at: Option<Timestamp>,
}

/// `conversation/turnDiff`: changes between the checkpoints after two runs. Ordinal
/// 0 is the workspace before the first run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurnDiff {
    pub thread_id: ThreadId,
    pub from_run_ordinal: u64,
    pub to_run_ordinal: u64,
    /// Defaults to true (T3 `CheckpointDiffQuery`).
    pub ignore_whitespace: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiff {
    pub thread_id: ThreadId,
    pub from_run_ordinal: u64,
    pub to_run_ordinal: u64,
    pub diff: String,
}

/// `conversation/subscribeWorktreeSetup`: the thread's setup card, `None` while no
/// setup is tracked; sent first, then after every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeSetup {
    pub thread_id: ThreadId,
}

/// `conversation/cancelWorktreeSetup`: stops a setup before its turn starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSetup {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCancelled {
    /// False when nothing ran or the setup was past cancellation.
    pub cancelled: bool,
}

/// `conversation/agentSessions/scan`: directories with Codex or Claude transcripts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanAgentSessions {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGit {
    /// The normalized origin URL, shared by every clone of one repository.
    pub remote_key: Option<String>,
    /// GitHub `owner/name` when the origin is on GitHub.
    pub repository: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCandidate {
    pub path: String,
    pub title: String,
    /// The registered project with this root.
    pub project_id: Option<String>,
    pub sources: Vec<Driver>,
    pub thread_count: u64,
    pub last_active_at: Option<Timestamp>,
    pub already_imported: bool,
    /// `None` when the directory is not the root of a git repository.
    pub git: Option<ProjectGit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScan {
    pub candidates: Vec<SessionCandidate>,
    pub scanned_at: Timestamp,
    pub truncated: bool,
}

/// `conversation/agentSessions/import`: recent sessions of one registered project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportAgentSessions {
    pub project_id: String,
    /// The root the client scanned; a project that moved since is refused.
    pub expected_root: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCounts {
    pub imported: u64,
    pub skipped: u64,
}

/// Stable codes of conversation failures; clients classify by code, not message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ResponseTooLarge,
    ThreadNotFound,
    CommandIdConflict,
    InternalCommand,
    InvalidCursor,
    InvalidSearch,
    ProjectNotFound,
    ProjectChanged,
    CheckpointUnavailable,
    AttachmentUnavailable,
    Unavailable,
    LiveBufferFull,
    DiffFailed,
}
impl ErrorCode {
    pub const ALL: [Self; 13] = [
        Self::ResponseTooLarge,
        Self::ThreadNotFound,
        Self::CommandIdConflict,
        Self::InternalCommand,
        Self::InvalidCursor,
        Self::InvalidSearch,
        Self::ProjectNotFound,
        Self::ProjectChanged,
        Self::CheckpointUnavailable,
        Self::AttachmentUnavailable,
        Self::Unavailable,
        Self::LiveBufferFull,
        Self::DiffFailed,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResponseTooLarge => "response_too_large",
            Self::ThreadNotFound => "thread_not_found",
            Self::CommandIdConflict => "command_id_conflict",
            Self::InternalCommand => "internal_command",
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidSearch => "invalid_search",
            Self::ProjectNotFound => "project_not_found",
            Self::ProjectChanged => "project_changed",
            Self::CheckpointUnavailable => "checkpoint_unavailable",
            Self::AttachmentUnavailable => "attachment_unavailable",
            Self::Unavailable => "conversation_unavailable",
            Self::LiveBufferFull => "live_buffer_full",
            Self::DiffFailed => "diff_failed",
        }
    }
    pub fn of(failure: &RpcFailure) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|code| code.as_str() == failure.code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConversationError {
    #[error("RPC response exceeds the transfer limit")]
    ResponseTooLarge,
    #[error("thread {0} does not exist")]
    ThreadNotFound(ThreadId),
    /// The id was committed for another thread or a different launch.
    #[error("command id {0} is already used by another request")]
    CommandIdConflict(CommandId),
    #[error("clients cannot send this command")]
    InternalCommand,
    #[error("invalid thread history cursor")]
    InvalidCursor,
    #[error("search needs 2 to 200 characters and a limit of 1 to 50")]
    InvalidSearch,
    #[error("project {0} does not exist")]
    ProjectNotFound(String),
    #[error("project {0} changed directories; scan again before importing history")]
    ProjectChanged(String),
    #[error("run {0} has no ready checkpoint")]
    CheckpointUnavailable(u64),
    #[error("attachment is unavailable: {0}")]
    AttachmentUnavailable(String),
    /// Storage or runtime failure; the request may or may not have taken effect.
    #[error("{0}")]
    Unavailable(String),
    /// T3 `LiveStreamBufferError`.
    #[error("The live event buffer is full. Resume from the last received sequence.")]
    LiveBufferFull,
    /// The checkpoints could not be diffed; carries the diff tool's detail.
    #[error("{0}")]
    DiffFailed(String),
}
impl ConversationError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ResponseTooLarge => ErrorCode::ResponseTooLarge,
            Self::ThreadNotFound(_) => ErrorCode::ThreadNotFound,
            Self::CommandIdConflict(_) => ErrorCode::CommandIdConflict,
            Self::InternalCommand => ErrorCode::InternalCommand,
            Self::InvalidCursor => ErrorCode::InvalidCursor,
            Self::InvalidSearch => ErrorCode::InvalidSearch,
            Self::ProjectNotFound(_) => ErrorCode::ProjectNotFound,
            Self::ProjectChanged(_) => ErrorCode::ProjectChanged,
            Self::CheckpointUnavailable(_) => ErrorCode::CheckpointUnavailable,
            Self::AttachmentUnavailable(_) => ErrorCode::AttachmentUnavailable,
            Self::Unavailable(_) => ErrorCode::Unavailable,
            Self::LiveBufferFull => ErrorCode::LiveBufferFull,
            Self::DiffFailed(_) => ErrorCode::DiffFailed,
        }
    }
    /// Whether a commit may have happened before the failure.
    pub fn delivery(&self) -> Delivery {
        match self {
            Self::ResponseTooLarge | Self::Unavailable(_) => Delivery::Unknown,
            _ => Delivery::NotSent,
        }
    }
}
impl From<ConversationError> for RpcFailure {
    fn from(error: ConversationError) -> Self {
        Self {
            code: error.code().as_str().into(),
            message: error.to_string(),
            delivery: error.delivery(),
        }
    }
}

#[cfg(test)]
mod tests;
