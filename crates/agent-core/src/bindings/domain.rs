//! ABI converters for the domain values view models carry. Ids cross as their
//! strings, timestamps as epoch milliseconds and open JSON as its text.
use agent_domain::*;

#[uniffi::remote(Enum)]
enum BranchNamingMode {
    Static,
    Semantic,
    Custom,
}

macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$(
        uniffi::custom_type!($name, String, {
            remote,
            lower: |value| value.as_str().to_owned(),
            try_lift: |value| Ok($name::new(value)?),
        });
    )+};
}
ids!(
    ThreadId,
    CommandId,
    MessageId,
    RunId,
    RunAttemptId,
    TurnItemId,
    RuntimeRequestId,
    PlanId,
    CheckpointId,
    ContextTransferId,
    NodeId,
);
uniffi::custom_type!(Timestamp, i64, {
    remote,
    lower: |value| value.millis(),
    try_lift: |value| Ok(Timestamp::from_millis(value)?),
});
uniffi::custom_type!(Json, String, {
    remote,
    lower: |value| value.0.to_string(),
    try_lift: |value| Ok(Json(serde_json::from_str(&value)?)),
});
uniffi::custom_type!(MessageContext, String, {
    remote,
    lower: |value| serde_json::to_string(&value).expect("message context serializes"),
    try_lift: |value| Ok(serde_json::from_str(&value)?),
});
#[uniffi::remote(Enum)]
enum RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}
#[uniffi::remote(Enum)]
enum ItemStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}
#[uniffi::remote(Enum)]
enum CheckpointStatus {
    Ready,
    Missing,
    Error,
    Stale,
}
#[uniffi::remote(Record)]
struct CheckpointFile {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}
#[uniffi::remote(Record)]
struct LimitRecoveryUpdate {
    pub run: RunId,
    pub reset_at: Timestamp,
    pub auto_resume: Option<bool>,
    pub snooze: Option<bool>,
}
#[uniffi::remote(Enum)]
enum Reply {
    Accepted,
    Run(RunId),
    Thread(ThreadId),
    Request(RuntimeRequestId),
    Rejected { reason: String },
    Ignored,
}
#[uniffi::remote(Enum)]
enum AttachmentKind {
    Image,
    File,
}
#[uniffi::remote(Record)]
struct Attachment {
    pub kind: AttachmentKind,
    pub source: Option<CapturedWindow>,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub path: String,
    pub size: u64,
}
#[uniffi::remote(Record)]
struct CapturedWindow {
    pub app_name: String,
    pub window_title: String,
    pub accessible_text: Option<String>,
    pub accessibility: Option<Accessibility>,
}
#[uniffi::remote(Enum)]
enum Accessibility {
    FlatText {
        text: String,
        truncated: bool,
    },
    ElementTree {
        coordinate_space: String,
        image_size: ImageSize,
        truncated: bool,
        root: Box<AccessibilityNode>,
    },
}
#[uniffi::remote(Record)]
struct ImageSize {
    pub width: u64,
    pub height: u64,
}
#[uniffi::remote(Record)]
struct Bounds {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}
#[uniffi::remote(Record)]
struct AccessibilityNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub bounds: Option<Bounds>,
    pub state: Option<Json>,
    pub actions: Vec<String>,
    pub children: Vec<AccessibilityNode>,
}
#[uniffi::remote(Enum)]
enum WorktreeSetupStageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}
#[uniffi::remote(Enum)]
enum WorktreeSetupStageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}
#[uniffi::remote(Record)]
struct WorktreeSetupStage {
    pub id: WorktreeSetupStageId,
    pub status: WorktreeSetupStageStatus,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub percent: Option<u8>,
    pub detail: Option<String>,
    pub tail: Vec<String>,
}
#[uniffi::remote(Enum)]
enum WorktreeSetupPhase {
    Running,
    Done,
    Failed,
    Cancelled,
}
#[uniffi::remote(Record)]
struct WorktreeSetupScript {
    pub name: String,
    pub command: String,
    pub terminal_id: Option<String>,
}
#[uniffi::remote(Record)]
struct WorktreeSetupSnapshot {
    pub thread: ThreadId,
    pub phase: WorktreeSetupPhase,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub worktree_path: Option<String>,
    pub setup_script: Option<WorktreeSetupScript>,
    pub stages: Vec<WorktreeSetupStage>,
    pub error: Option<String>,
    pub sequence: u64,
}
