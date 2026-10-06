use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

macro_rules! values {
    ($($name:ident { $($variant:ident),+ $(,)? })+) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    )+};
}
values! {
    Driver { Codex, Claude }
    RuntimeMode { ApprovalRequired, AutoAcceptEdits, Auto, FullAccess }
    InteractionMode { Default, Plan }
    RunStatus { Preparing, Queued, Starting, Running, Waiting, Completed, Interrupted, Failed, Cancelled, RolledBack }
    AttemptStatus { Pending, Running, Completed, Interrupted, Failed, Cancelled, Superseded }
    ItemStatus { Pending, Running, Waiting, Completed, Interrupted, Failed, Cancelled }
    RequestStatus { Pending, Resolved, Expired, Cancelled }
    Role { User, Assistant, System }
    MessageAuthor { User, Agent }
    InputIntent { TurnStart, QueuedTurn, Steer, PromotedQueuedToSteer }
    RecoveryTrigger { Startup, Shutdown }
    CompletionWake { Always, SettledOnly }
    DeliveryState { Pending, Claimed, Acknowledged, Delivered, Disposed }
    TransferKind { Fork, MergeBack, ProviderHandoff, ProviderHandoffDelta, SubagentSpawn, SubagentResult }
    BackgroundKind { Command, Monitor, Subagent, BackgroundTask }
    AttachmentKind { Image, File }
    PlanKind { Proposed, Todo }
    CheckpointStatus { Ready, Missing, Error, Stale }
}
impl RunStatus {
    pub fn blocking(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }
    pub fn terminal(self) -> bool {
        !self.blocking() && self != Self::Queued
    }
}
impl ItemStatus {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub instance: String,
    pub driver: Driver,
    pub model: String,
    pub options: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub source: Option<CapturedWindow>,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub path: String,
    pub size: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedWindow {
    pub app_name: String,
    pub window_title: String,
    pub accessible_text: Option<String>,
    pub accessibility: Option<Accessibility>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Accessibility {
    FlatText {
        text: String,
        truncated: bool,
    },
    #[serde(rename_all = "camelCase")]
    ElementTree {
        coordinate_space: String,
        image_size: ImageSize,
        truncated: bool,
        root: Box<AccessibilityNode>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSize {
    pub width: u64,
    pub height: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub bounds: Option<Bounds>,
    pub state: Option<Json>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub children: Vec<AccessibilityNode>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub project: String,
    pub title: String,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    pub settled: Option<bool>,
    pub settled_at: Option<Timestamp>,
    pub snoozed_until: Option<Timestamp>,
    pub pinned_at: Option<Timestamp>,
    pub pin_order: Option<String>,
    pub active_order: Option<String>,
    pub last_visited_at: Option<Timestamp>,
    pub auto_settle: bool,
    pub parent: Option<ThreadId>,
    pub fork_boundary: Option<u64>,
    pub workspace: Option<Workspace>,
    /// The pending title generation; a rename or a newer request supersedes it.
    pub title_request: Option<CommandId>,
    pub imported: bool,
    pub snoozed_at: Option<Timestamp>,
    pub limit_recovery: Option<LimitRecovery>,
    pub linked_pull_request: Option<LinkedPullRequest>,
}
/// What to do once a usage limit resets (T3 OrchestrationV2LimitRecovery).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRecovery {
    /// The metadata command that set it; an automatic resume names it.
    pub request: Option<CommandId>,
    pub run: RunId,
    pub reset_at: Timestamp,
    pub auto_resume: bool,
    pub snooze: bool,
}
/// A recovery choice; omitted options keep their value for the same run and reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRecoveryUpdate {
    pub run: RunId,
    pub reset_at: Timestamp,
    pub auto_resume: Option<bool>,
    pub snooze: Option<bool>,
}
/// T3 ThreadLinkedPullRequest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedPullRequest {
    pub project: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}
/// A proposed plan a message implements, possibly on another thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRef {
    pub thread: ThreadId,
    pub plan: PlanId,
}
/// Another thread's plan as the Host read it before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPlan {
    pub project: String,
    pub kind: PlanKind,
    pub implemented: bool,
}
/// T3 message.dispatch continuations of a stopped run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Continuation {
    /// The user continues an interrupted or usage-limited run.
    Manual { run: RunId },
    /// The limit recovery resumes the limited run once its reset passed.
    UsageLimit {
        run: RunId,
        recovery: Option<CommandId>,
    },
}
/// Sidebar state a fork or delegated child copies from its parent thread
/// (T3 ThreadForkService and makeSubagentChildThread spread the parent row).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadArrangement {
    pub pinned_at: Option<Timestamp>,
    pub pin_order: Option<String>,
    pub active_order: Option<String>,
    pub auto_settle: bool,
    pub title_request: Option<CommandId>,
}
impl ThreadArrangement {
    pub fn of(thread: &Thread) -> Self {
        Self {
            pinned_at: thread.pinned_at.clone(),
            pin_order: thread.pin_order.clone(),
            active_order: thread.active_order.clone(),
            auto_settle: thread.auto_settle,
            title_request: thread.title_request.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedMessage {
    pub role: Role,
    pub text: String,
    pub at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub notification: Option<Notification>,
    pub id: MessageId,
    pub run: Option<RunId>,
    pub role: Role,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub intent: InputIntent,
    pub streaming: bool,
    pub created_by: MessageAuthor,
    pub creation_source: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Inline context records the text links to (T3 message `context`).
    pub context: Option<MessageContext>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub restart_of: Option<RunId>,
    pub restart_cancelled_work: Vec<CancelledBackgroundWork>,
    pub checkpoint_scope: Option<CheckpointScope>,
    pub native_baseline_heads: BTreeMap<String, Option<String>>,
    pub id: RunId,
    pub ordinal: u64,
    pub message: MessageId,
    pub selection: ModelSelection,
    pub status: RunStatus,
    pub attempt: Option<RunAttemptId>,
    pub queue_position: Option<u64>,
    pub queue_held: bool,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub source_plan: Option<PlanRef>,
    pub checkpoint: Option<CheckpointId>,
    pub continuation: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: RunAttemptId,
    pub run: RunId,
    pub ordinal: u64,
    pub status: AttemptStatus,
    pub native_thread: Option<String>,
    pub native_turn: Option<String>,
    pub native_head: Option<String>,
    /// The provider accepted the turn, so its input reached native history.
    pub accepted: bool,
    pub usage: Option<TokenUsage>,
    pub context_usage: Option<ContextUsage>,
    pub turn_usage: Option<TurnTokenUsage>,
    pub usage_accumulator: Option<UsageCounters>,
    pub usage_observed: bool,
    /// Usage windows the provider rejected during this turn, with their resets.
    pub rejected_limits: BTreeMap<String, Option<i64>>,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
    pub reasoning_output: u64,
    pub total: u64,
    pub max: Option<u64>,
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPresentation {
    pub title: Option<String>,
    pub source: Option<Json>,
    /// `browser` or `computer` for tools that act on those surfaces.
    pub surface: Option<String>,
    pub icon: Option<Json>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemKind {
    Fork {
        parent: ThreadId,
        boundary: u64,
    },
    UserMessage {
        message: MessageId,
    },
    AssistantMessage {
        message: MessageId,
    },
    Reasoning,
    RunInterruptRequest,
    RunInterruptResult {
        request: TurnItemId,
    },
    CommandExecution {
        command: String,
        cwd: Option<String>,
        exit_code: Option<i64>,
        title: Option<String>,
    },
    FileChange {
        changes: Json,
    },
    DynamicTool {
        presentation: ToolPresentation,
        name: String,
        input: Json,
        output: Option<Json>,
    },
    WebSearch {
        query: String,
        results: Option<Json>,
    },
    ProposedPlan {
        plan: PlanId,
    },
    TodoList {
        plan: PlanId,
    },
    ApprovalRequest {
        request: RuntimeRequestId,
    },
    UserInputRequest {
        request: RuntimeRequestId,
    },
    Subagent {
        task: NodeId,
    },
    Compaction {
        before: Option<u64>,
        after: Option<u64>,
    },
    Error {
        message: String,
        retry: Option<RetryProgress>,
        code: Option<String>,
        class: Option<String>,
        retryable: Option<bool>,
        /// When a usage limit resets (T3 ProviderFailure resetAt).
        reset_at: Option<Timestamp>,
    },
    SystemNotice {
        message: String,
    },
    Notification {
        notification: Notification,
    },
}
/// A provider retry that is still in progress or has resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryProgress {
    pub attempt: u64,
    pub max_attempts: Option<u64>,
    pub delay_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: TurnItemId,
    pub run: Option<RunId>,
    pub attempt: Option<RunAttemptId>,
    pub native_key: String,
    pub ordinal: u64,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub text: String,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    /// Set only on delivery (T3 WireProjection): the output stays on the Host and
    /// `getTurnItem` reads it.
    pub output_omitted: bool,
    /// Set only on delivery: the exit code or the withheld output shows a failure.
    pub output_indicates_failure: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RequestBody {
    Approval {
        kind: String,
        title: String,
        detail: Option<String>,
        options: Vec<ApprovalOption>,
        input: Json,
    },
    Questions {
        questions: Vec<Question>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub label: String,
    pub decision: ApprovalDecision,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "acceptAlways")]
    AcceptAlways,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub required: bool,
    pub id: String,
    pub header: String,
    pub question: String,
    pub multiple: bool,
    pub options: Vec<QuestionOption>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Text(String),
    Choices(Vec<String>),
}
impl Answer {
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Choices(choices) => choices.join(", "),
        }
    }
    pub fn choices(&self) -> Vec<String> {
        match self {
            Self::Text(text) => vec![text.clone()],
            Self::Choices(choices) => choices.clone(),
        }
    }
}
pub type Answers = BTreeMap<String, Answer>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseCapability {
    Live,
    Message,
    NotResumable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub owner_path: Vec<String>,
    pub id: RuntimeRequestId,
    pub attempt: RunAttemptId,
    pub native_key: String,
    pub body: RequestBody,
    pub capability: ResponseCapability,
    pub status: RequestStatus,
    pub decision: Option<ApprovalDecision>,
    pub answers: Option<Answers>,
    pub attachments: BTreeMap<String, Vec<Attachment>>,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub kind: PlanKind,
    pub id: PlanId,
    pub run: RunId,
    pub native_key: String,
    pub markdown: String,
    pub steps: Vec<PlanStep>,
    pub implemented_by: Option<RunId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub text: String,
    pub status: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointScope {
    pub id: CheckpointScopeId,
    pub cwd: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedBaseline {
    pub status: CheckpointStatus,
    pub checkpoint: CheckpointId,
    pub ordinal: u64,
    pub file_ref: String,
    pub native_heads: BTreeMap<String, Option<String>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub status: CheckpointStatus,
    pub scope: Option<CheckpointScope>,
    pub id: CheckpointId,
    pub run: Option<RunId>,
    pub run_ordinal: u64,
    pub native_heads: BTreeMap<String, Option<String>>,
    pub file_ref: String,
    pub files: Vec<CheckpointFile>,
}
/// One file a checkpoint changed since the scope's previous checkpoint (T3
/// `OrchestrationV2CheckpointFileSummary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub original_message: Option<MessageId>,
    pub native_task: Option<String>,
    pub background: bool,
    pub id: NodeId,
    pub native_key: String,
    pub run: Option<RunId>,
    pub attempt: RunAttemptId,
    pub child_thread: ThreadId,
    pub parent_task: Option<NodeId>,
    pub prompt: String,
    pub title: Option<String>,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub model: Option<String>,
    pub status: ItemStatus,
    pub result: Option<String>,
    pub progress: Option<String>,
    pub wake: CompletionWake,
    pub delivery: DeliveryState,
    /// Incremented when a native task is resumed; results of an earlier
    /// generation are stale.
    pub generation: u64,
}
impl Task {
    pub fn app_owned(&self) -> bool {
        self.original_message.is_some()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transfer {
    /// A fork's source native thread and boundary, forked natively when the
    /// consuming turn runs on the same instance.
    pub native_source: Option<NativeBinding>,
    /// The provider instance it is for; a fork or merge-back takes the
    /// instance of the turn that consumes it.
    pub instance: Option<String>,
    /// The run a delegated result was handed to (T3 targetRunId).
    pub target_run: Option<RunId>,
    pub delivery: Option<ContextDelivery>,
    pub id: ContextTransferId,
    pub kind: TransferKind,
    pub source: ThreadId,
    pub target: ThreadId,
    pub boundary: u64,
    pub history: HistoricalContext,
    pub superseded: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResultContext {
    pub boundary: u64,
    pub history: HistoricalContext,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub parent: ThreadId,
    pub task: NodeId,
    pub message: MessageId,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRollback {
    pub command: CommandId,
    pub checkpoint: CheckpointId,
    pub restore_files: bool,
    /// Provider instances whose native history the rollback may already have
    /// rewound; a failed rollback resets their native sessions.
    pub rewinding: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeBinding {
    pub instance: String,
    pub thread: String,
    pub head: Option<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptEchoMode {
    #[default]
    Unknown,
    Acknowledged,
    Early,
    ResultOnly,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingPrompt {
    pub attempt: RunAttemptId,
    pub key: String,
    pub confirmed: bool,
    pub frames_before_echo: u64,
    pub held: Vec<ProviderEvent>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundWork {
    pub key: String,
    pub tool: String,
    pub description: String,
    pub kind: BackgroundKind,
    pub attempt: RunAttemptId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeReport {
    pub key: String,
    pub report: WorkReport,
    pub text: String,
    pub prompt_ordinal: u64,
}
/// Receipts are passed back to step by the owning actor, separately from projection facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub command: CommandId,
    pub fingerprint: String,
    pub reply: Reply,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub delegation: Option<Delegation>,
    pub thread: Option<Thread>,
    pub checkpoint_scope: Option<CheckpointScope>,
    pub runs: Vec<Run>,
    pub attempts: Vec<Attempt>,
    pub messages: Vec<Message>,
    pub items: Vec<Item>,
    pub requests: Vec<Request>,
    pub plans: Vec<Plan>,
    pub checkpoints: Vec<Checkpoint>,
    pub tasks: Vec<Task>,
    pub transfers: Vec<Transfer>,
    pub inherited_items: Vec<Item>,
    pub inherited_messages: Vec<Message>,
    pub rollback: Option<PendingRollback>,
    /// Captures that can be replayed after process loss, including their terminal status.
    pub captures: BTreeMap<RunId, RunStatus>,
    pub native_heads: BTreeMap<String, Option<String>>,
    pub rollback_failure: Option<String>,
    pub pending_children: BTreeMap<String, Vec<ProviderEvent>>,
    pub stopping: BTreeSet<RunAttemptId>,
    pub native_owner: Option<RunAttemptId>,
    pub native_parent: Option<(ThreadId, NodeId)>,
    pub native_generation: u64,
    pub native_child_thread: Option<String>,
    pub native_child_turn: Option<String>,
    pub native_context_usage: Option<ContextUsage>,
    pub native_turn_usage: Option<TurnTokenUsage>,
    pub native_usage_accumulator: Option<UsageCounters>,
    pub native_usage_observed: bool,
    pub prompt_echo_mode: PromptEchoMode,
    pub pending_prompt: Option<PendingPrompt>,
    pub native_continuations: BTreeMap<RunId, Vec<ProviderEvent>>,
    pub background_work: BTreeMap<String, BackgroundWork>,
    pub wake_reports: Vec<WakeReport>,
    pub prompt_ordinal: u64,
    pub native_sessions: BTreeMap<String, String>,
    pub handoff_token_cap: Option<u64>,
    pub context_windows: BTreeMap<String, u64>,
    pub usage_baselines: BTreeMap<String, UsageCounters>,
    /// The latest account usage-limit reset each provider instance reported.
    pub rate_limit_resets: BTreeMap<String, Option<i64>>,
}
impl State {
    /// A local message, or one referenced by an inherited fork item.
    pub fn message(&self, id: &MessageId) -> Option<&Message> {
        self.messages
            .iter()
            .chain(&self.inherited_messages)
            .find(|message| &message.id == id)
    }
    pub fn active_run(&self) -> Option<&Run> {
        self.runs
            .iter()
            .filter(|r| r.status.blocking())
            .max_by_key(|r| r.ordinal)
    }
    pub fn queued_runs(&self) -> Vec<&Run> {
        let mut runs: Vec<_> = self
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::Queued)
            .collect();
        runs.sort_by_key(|r| {
            (
                !self.delegated_delivery(&r.message),
                r.queue_position.unwrap_or(r.ordinal),
                r.ordinal,
            )
        });
        runs
    }
    /// A delegated completion delivery; T3 delivers only these ahead of the queue.
    pub fn delegated_delivery(&self, message: &MessageId) -> bool {
        self.message(message)
            .and_then(|m| m.notification.as_ref())
            .is_some_and(|n| matches!(n.source, NotificationSource::Delegated { .. }))
    }
    pub fn visible_items(&self) -> Vec<&Item> {
        let visible_runs: BTreeSet<_> = self
            .runs
            .iter()
            .filter(|r| r.status != RunStatus::RolledBack)
            .map(|r| &r.id)
            .collect();
        let mut items: Vec<_> = self
            .inherited_items
            .iter()
            .chain(
                self.items
                    .iter()
                    .filter(|item| item.run.as_ref().is_none_or(|id| visible_runs.contains(id))),
            )
            .collect();
        items.sort_by_key(|i| (i.ordinal, &i.id));
        items
    }
    pub fn activity_items(&self) -> Vec<std::borrow::Cow<'_, Item>> {
        self.visible_items()
            .into_iter()
            .map(|item| self.notification_card(item))
            .collect()
    }
    /// A user item that carries a notification shows as its notification card.
    pub fn notification_card<'a>(&self, item: &'a Item) -> std::borrow::Cow<'a, Item> {
        if let ItemKind::UserMessage { message } = &item.kind
            && let Some(notification) = self
                .messages
                .iter()
                .find(|candidate| &candidate.id == message)
                .and_then(|message| message.notification.as_ref())
        {
            let mut item = item.clone();
            item.kind = ItemKind::Notification {
                notification: notification.clone(),
            };
            item.text.clear();
            std::borrow::Cow::Owned(item)
        } else {
            std::borrow::Cow::Borrowed(item)
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMessage {
    pub created_by: MessageAuthor,
    pub creation_source: String,
    pub id: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub selection: Option<ModelSelection>,
    pub mode: DispatchMode,
    pub intent: Option<DeliveryIntent>,
    pub source_plan: Option<PlanRef>,
    /// Filled by the Host for a plan on another thread.
    pub resolved_plan: Option<ResolvedPlan>,
    pub continuation: Option<Continuation>,
    /// Shown as the title while the first message's title is generated.
    pub title_seed: Option<String>,
    pub context: Option<MessageContext>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DispatchMode {
    StartImmediately,
    QueueAfterActive,
    DeferStart,
    SteerActive { run: RunId },
    RestartActive { run: RunId },
}
values! { DeliveryIntent { Auto, Steer, Restart } }
/// T3 thread fork and merge-back source points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourcePoint {
    LatestStable,
    Run(RunId),
    Checkpoint(CheckpointId),
}
values! { PreparationPhase { Worktree, Setup } }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    ContinueRestart {
        source: RunId,
        enabled: bool,
    },
    Create {
        thread: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        workspace: Option<Workspace>,
    },
    /// A native session imported with its original message times. It is
    /// settled and bound to the native session.
    Import {
        thread: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        workspace: Option<Workspace>,
        created_at: Timestamp,
        updated_at: Timestamp,
        messages: Vec<ImportedMessage>,
        native: NativeBinding,
    },
    Rename {
        title: String,
    },
    RegenerateTitle,
    /// T3 thread.metadata.update. `project_root` is filled by the Host and
    /// becomes the working directory when the worktree is cleared.
    UpdateMetadata {
        title: Option<String>,
        regenerate_title: Option<bool>,
        branch: Option<Option<String>>,
        worktree_path: Option<Option<String>>,
        expected_worktree_path: Option<Option<String>>,
        expected_empty: bool,
        limit_recovery: Option<Option<LimitRecoveryUpdate>>,
        linked_pull_request: Option<Option<LinkedPullRequest>>,
        project_root: Option<String>,
    },
    /// T3 thread.auto-settle from the Host's settlement sweep.
    SettleAutomatically {
        snapshot_at: Timestamp,
        settled_at: Option<Timestamp>,
    },
    /// Another thread's run implements this thread's proposed plan.
    ImplementPlan {
        plan: PlanId,
        run: RunId,
    },
    Archive {
        archived: bool,
    },
    Delete,
    Settle {
        settled: bool,
        at: Option<Timestamp>,
    },
    Snooze {
        until: Option<Timestamp>,
    },
    Pin {
        pinned: bool,
        order: Option<String>,
    },
    ReorderPinned {
        order: String,
    },
    ReorderActive {
        order: String,
    },
    Visit {
        at: Timestamp,
    },
    MarkUnread,
    AutoSettle {
        enabled: bool,
    },
    RuntimeMode {
        mode: RuntimeMode,
    },
    InteractionMode {
        mode: InteractionMode,
    },
    SelectModel {
        selection: ModelSelection,
    },
    SwitchProvider {
        selection: ModelSelection,
    },
    /// T3 provider-session.detach: stop this thread's session of an instance.
    DetachProviderSession {
        instance: String,
        reason: Option<String>,
    },
    Send(SendMessage),
    ReleasePrepared {
        run: RunId,
    },
    PreparedRunProgress {
        run: RunId,
        phase: PreparationPhase,
    },
    FailPrepared {
        run: RunId,
        message: String,
    },
    RetryPrepared {
        run: RunId,
    },
    Interrupt {
        run: RunId,
        hold_queue: bool,
        reason: Option<String>,
    },
    ResumeQueue,
    ReorderQueued {
        run: RunId,
        before: Option<RunId>,
    },
    CancelQueued {
        run: RunId,
    },
    EditQueued {
        run: RunId,
        text: String,
        attachments: Option<Vec<Attachment>>,
        /// Replaces the message's context when present.
        context: Option<MessageContext>,
    },
    PromoteToSteer {
        queued: RunId,
        active: RunId,
    },
    Respond {
        request: RuntimeRequestId,
        decision: Option<ApprovalDecision>,
        answers: Option<Answers>,
        attachments: BTreeMap<String, Vec<Attachment>>,
    },
    DismissQuestion {
        request: RuntimeRequestId,
    },
    Rollback {
        checkpoint: CheckpointId,
        restore_files: bool,
        /// Filled by the Host: why the checkpoint's files cannot be restored now
        /// (another thread shares the workspace). Not part of the command's identity.
        restore_refusal: Option<String>,
    },
    Fork {
        target: ThreadId,
        source: SourcePoint,
        title: Option<String>,
    },
    /// `native` is the source's native thread and boundary, if it has one.
    AcceptFork {
        thread: ThreadId,
        parent: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        boundary: u64,
        history: Vec<Item>,
        messages: Vec<Message>,
        workspace: Option<Workspace>,
        arrangement: Box<ThreadArrangement>,
        context: HistoricalContext,
        native: Option<NativeBinding>,
    },
    MergeBack {
        target: ThreadId,
        source: SourcePoint,
    },
    AcceptTransfer {
        id: ContextTransferId,
        kind: TransferKind,
        source: ThreadId,
        boundary: u64,
        history: HistoricalContext,
    },
    Delegate {
        task: NodeId,
        child: ThreadId,
        prompt: String,
        title: Option<String>,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        wake: CompletionWake,
    },
    AcceptDelegation {
        thread: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        workspace: Option<Workspace>,
        arrangement: Box<ThreadArrangement>,
        origin: Delegation,
        message: Box<SendMessage>,
    },
    TaskProgress {
        task: NodeId,
        progress: Option<String>,
        model: Option<String>,
    },
    TaskResult {
        source_message: Option<MessageId>,
        /// The native child generation that produced this result.
        generation: Option<u64>,
        context: Option<TaskResultContext>,
        task: NodeId,
        status: ItemStatus,
        result: String,
    },
    SetTaskWake {
        task: NodeId,
        wake: CompletionWake,
    },
    AcknowledgeTask {
        task: NodeId,
    },
    DisposeTask {
        task: NodeId,
    },
    AcceptTaskWake {
        task_ids: Vec<NodeId>,
    },
    Compact,
    Stop,
    BindNativeChild {
        native_thread: Option<String>,
        owner: RunAttemptId,
        parent: ThreadId,
        task: NodeId,
        generation: u64,
    },
    NativeInput {
        attempt: RunAttemptId,
        event: Box<ProviderEvent>,
    },
}
/// Commands only the Host itself (sagas, sessions, executors, import) may send.
/// Every variant is listed so a new command must be classified.
pub fn host_only_command(command: &Command) -> bool {
    match command {
        Command::NativeInput { .. }
        | Command::BindNativeChild { .. }
        | Command::Import { .. }
        | Command::AcceptFork { .. }
        | Command::AcceptDelegation { .. }
        | Command::AcceptTransfer { .. }
        | Command::TaskResult { .. }
        | Command::TaskProgress { .. }
        | Command::AcceptTaskWake { .. }
        | Command::ContinueRestart { .. }
        | Command::ReleasePrepared { .. }
        | Command::PreparedRunProgress { .. }
        | Command::SettleAutomatically { .. }
        | Command::ImplementPlan { .. }
        | Command::FailPrepared { .. } => true,
        Command::Create { .. }
        | Command::Rename { .. }
        | Command::RegenerateTitle
        | Command::UpdateMetadata { .. }
        | Command::Archive { .. }
        | Command::Delete
        | Command::Settle { .. }
        | Command::Snooze { .. }
        | Command::Pin { .. }
        | Command::ReorderPinned { .. }
        | Command::ReorderActive { .. }
        | Command::Visit { .. }
        | Command::MarkUnread
        | Command::AutoSettle { .. }
        | Command::RuntimeMode { .. }
        | Command::InteractionMode { .. }
        | Command::SelectModel { .. }
        | Command::SwitchProvider { .. }
        | Command::DetachProviderSession { .. }
        | Command::Send(_)
        | Command::RetryPrepared { .. }
        | Command::Interrupt { .. }
        | Command::ResumeQueue
        | Command::ReorderQueued { .. }
        | Command::CancelQueued { .. }
        | Command::EditQueued { .. }
        | Command::PromoteToSteer { .. }
        | Command::Respond { .. }
        | Command::DismissQuestion { .. }
        | Command::Rollback { .. }
        | Command::Fork { .. }
        | Command::MergeBack { .. }
        | Command::Delegate { .. }
        | Command::SetTaskWake { .. }
        | Command::AcknowledgeTask { .. }
        | Command::DisposeTask { .. }
        | Command::Compact
        | Command::Stop => false,
    }
}
/// Provider keys are native identifiers, never application entity IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderItem {
    Text,
    Reasoning,
    Command {
        command: String,
        cwd: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        changes: Json,
    },
    Tool {
        presentation: ToolPresentation,
        name: String,
        input: Json,
        output: Option<Json>,
    },
    WebSearch {
        query: String,
        results: Option<Json>,
    },
    Compaction {
        before: Option<u64>,
        after: Option<u64>,
    },
    Notice {
        message: String,
    },
    /// A rejected usage window; `resets_at` is in Unix seconds.
    UsageLimit {
        limit: Option<String>,
        resets_at: Option<i64>,
    },
    Error {
        message: String,
        retry: Option<RetryProgress>,
        code: Option<String>,
        class: Option<String>,
        retryable: Option<bool>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderEvent {
    /// The account's usage-limit reset (Unix seconds), when every exhausted
    /// window reports one (T3 codexUsageLimitResetAt).
    RateLimits {
        resets_at: Option<i64>,
    },
    UsageTotals {
        native_thread: String,
        native_turn: String,
        total: UsageCounters,
        last: UsageCounters,
    },
    TurnUsage(TurnTokenUsage),
    ContextUsage(ContextUsage),
    ContextInjected,
    AssistantCursor {
        key: String,
    },
    ResultText {
        key: String,
        text: String,
        status: ItemStatus,
    },
    SubagentNamed {
        key: String,
        title: String,
    },
    PromptOffered {
        key: String,
    },
    NativeOutput {
        echoed_prompts: Vec<String>,
        acknowledged_prompt: Option<String>,
        root: bool,
        result: Option<NativeResult>,
        events: Vec<ProviderEvent>,
    },
    SessionClosed {
        error: Option<String>,
    },
    TurnAborted {
        reason: String,
    },
    PlanDelta {
        key: String,
        text: String,
    },
    UserMessage {
        key: String,
        text: String,
    },
    SessionReady {
        native_thread: String,
    },
    TurnStarted {
        native_turn: Option<String>,
    },
    TurnFinished {
        status: RunStatus,
        native_head: Option<String>,
    },
    ItemStarted {
        key: String,
        kind: ProviderItem,
    },
    TextDelta {
        key: String,
        kind: ProviderItem,
        text: String,
    },
    ItemFinished {
        key: String,
        kind: ProviderItem,
        text: Option<String>,
        status: ItemStatus,
    },
    RequestOpened {
        owner_path: Vec<String>,
        key: String,
        body: RequestBody,
        capability: ResponseCapability,
    },
    RequestClosed {
        key: String,
    },
    Plan {
        kind: PlanKind,
        key: String,
        markdown: String,
        steps: Vec<PlanStep>,
    },
    Usage(TokenUsage),
    ModelObserved {
        model: String,
    },
    SubagentNativeBound {
        key: String,
        native_task: String,
    },
    SubagentStarted {
        background: bool,
        native_thread: Option<String>,
        key: String,
        parent: Option<String>,
        prompt: String,
        model: Option<String>,
    },
    SubagentProgress {
        key: String,
        progress: String,
        model: Option<String>,
    },
    SubagentFinished {
        key: String,
        status: ItemStatus,
        result: String,
    },
    Child {
        key: String,
        event: Box<ProviderEvent>,
    },
    BackgroundTask {
        key: String,
        tool: String,
        kind: BackgroundKind,
        description: String,
        status: Option<ItemStatus>,
        summary: Option<String>,
        exit_code: Option<i64>,
    },
    /// The complete background roster of the native session.
    BackgroundRoster {
        tasks: Vec<BackgroundEntry>,
    },
    /// The provider asks for a turn that reports finished background work.
    Wake {
        text: String,
        detail: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundEntry {
    pub key: String,
    pub tool: String,
    pub kind: BackgroundKind,
    pub description: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeResult {
    pub origin: Option<String>,
    pub turn_count: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderCommand {
    Start {
        resume_interrupted_turn: bool,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        /// The user's text; compose the native prompt with `provider_prompt`.
        text: String,
        note: Option<String>,
        attachments: Vec<Attachment>,
        native_thread: Option<String>,
        resume_at: Option<String>,
        context: Option<HistoricalContext>,
    },
    Steer {
        message: MessageId,
        text: String,
        attachments: Vec<Attachment>,
    },
    Interrupt {
        native_thread: Option<String>,
        native_turn: Option<String>,
    },
    Respond {
        native_key: String,
        decision: Option<ApprovalDecision>,
        answers: Option<Answers>,
        input: Option<Json>,
    },
    Rollback {
        native_thread: String,
        absolute_head: Option<String>,
    },
    Fork {
        native_thread: String,
        through_turn: Option<String>,
    },
    Compact {
        native_thread: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub attempt: Option<RunAttemptId>,
    pub body: EffectBody,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectBody {
    /// Forks a source native thread for the attempt that consumes a fork.
    ForkNative {
        instance: String,
        provider: ProviderCommand,
    },
    Provider(ProviderCommand),
    CaptureCheckpoint {
        scope: CheckpointScope,
        native_baseline_heads: BTreeMap<String, Option<String>>,
        run: RunId,
    },
    /// One rollback request: provider rewinds, then the optional file
    /// restore, then removal of stale checkpoint references. It reports one
    /// `RollbackFinished` or `RollbackFailed`.
    Rollback {
        command: CommandId,
        providers: Vec<ProviderRollback>,
        restore: Option<RestoreFiles>,
        stale_file_refs: Vec<String>,
    },
    PrepareWorkspace {
        run: RunId,
    },
    SendToThread {
        thread: ThreadId,
        command: Box<Command>,
    },
    DeleteAttachments {
        paths: Vec<String>,
    },
    /// Detach this thread's provider sessions, or those of one instance, which
    /// stops their background work.
    DetachSessions {
        reason: String,
        revoke_credentials: bool,
        instance: Option<String>,
    },
    CleanupTerminals,
    /// Generate a title from the initial message, or from the conversation
    /// when `message` is absent.
    GenerateTitle {
        request: CommandId,
        message: Option<MessageId>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRollback {
    pub instance: String,
    pub command: ProviderCommand,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreFiles {
    pub scope: Option<CheckpointScope>,
    pub checkpoint: CheckpointId,
    pub file_ref: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectResult {
    NativeForked {
        attempt: RunAttemptId,
        native_thread: String,
    },
    ForkFailed {
        attempt: RunAttemptId,
        message: String,
    },
    ProviderFailed {
        attempt: RunAttemptId,
        operation: ProviderOperation,
        message: String,
        message_id: Option<MessageId>,
        turn_completed: bool,
        /// A start could not resume its native session; start a fresh one.
        session_lost: bool,
    },
    /// A capture that could not read the workspace reports `Missing` or
    /// `Error`; the run still finishes.
    CheckpointCaptured {
        status: CheckpointStatus,
        baselines: Vec<CapturedBaseline>,
        run: RunId,
        attempt: Option<RunAttemptId>,
        checkpoint: CheckpointId,
        file_ref: String,
        files: Vec<CheckpointFile>,
    },
    RollbackFinished {
        bindings: Vec<NativeBinding>,
        command: CommandId,
    },
    RollbackFailed {
        command: CommandId,
        message: String,
    },
    /// `None` keeps the current title.
    TitleGenerated {
        request: CommandId,
        title: Option<String>,
    },
    /// A `SendToThread` command that the target rejected or that could not be
    /// delivered, reported to the thread that sent it.
    ThreadCommandFailed {
        thread: ThreadId,
        command: Box<Command>,
        reason: String,
    },
}
values! { ProviderOperation { Start, Steer, Interrupt, Respond, Compact } }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Input {
    RuntimeOpened {
        instance: String,
        attempt: Option<RunAttemptId>,
    },
    CheckpointScope {
        run: Option<RunId>,
        attempt: Option<RunAttemptId>,
        scope: Option<CheckpointScope>,
    },
    HandoffPolicy {
        instance: String,
        model_window: Option<u64>,
        token_cap: u64,
    },
    NativeSessionReset {
        instance: String,
    },
    /// The rollback effect is about to rewind these provider instances.
    RollbackRewindStarted {
        command: CommandId,
        instances: Vec<String>,
    },
    /// The native session a pending fork will create, recorded before its
    /// transcript exists so no import can adopt it.
    NativeForkReserved {
        attempt: RunAttemptId,
        native_thread: String,
    },
    Workspace {
        workspace: Option<Workspace>,
    },
    Command {
        id: CommandId,
        command: Box<Command>,
        receipt: Option<Receipt>,
    },
    Provider {
        attempt: RunAttemptId,
        event: Box<ProviderEvent>,
    },
    Effect(EffectResult),
    Recover {
        trigger: RecoveryTrigger,
        continue_after_restart: bool,
        /// Waiting runs whose checkpoint capture is still queued to run.
        capturing: BTreeSet<RunId>,
    },
    Timer,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Accepted,
    Run(RunId),
    Thread(ThreadId),
    Request(RuntimeRequestId),
    Rejected { reason: String },
    Ignored,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub facts: Vec<Fact>,
    pub effects: Vec<Effect>,
    pub reply: Reply,
    pub receipt: Option<Receipt>,
}
