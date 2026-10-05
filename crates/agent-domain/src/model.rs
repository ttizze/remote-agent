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
    pub source_plan: Option<PlanId>,
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
        retrying: bool,
        code: Option<String>,
        class: Option<String>,
        retryable: Option<bool>,
    },
    SystemNotice {
        message: String,
    },
    Notification {
        notification: Notification,
    },
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
}
impl Task {
    pub fn app_owned(&self) -> bool {
        self.original_message.is_some()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transfer {
    pub native_fork: Option<String>,
    pub instance: String,
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
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingFork {
    pub target: ThreadId,
    pub child_command: Box<Command>,
    pub instance: String,
    pub head: Option<String>,
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
    pub pending_forks: BTreeMap<CommandId, PendingFork>,
    pub native_sessions: BTreeMap<String, String>,
    pub handoff_token_cap: Option<u64>,
    pub context_windows: BTreeMap<String, u64>,
    pub usage_baselines: BTreeMap<String, UsageCounters>,
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
                !self
                    .messages
                    .iter()
                    .any(|m| m.id == r.message && m.created_by == MessageAuthor::Agent),
                r.queue_position.unwrap_or(r.ordinal),
                r.ordinal,
            )
        });
        runs
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
            .map(|item| {
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
            })
            .collect()
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
    pub source_plan: Option<PlanId>,
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
    },
    Rename {
        title: String,
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
    Send(SendMessage),
    ReleasePrepared {
        run: RunId,
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
    },
    Fork {
        target: ThreadId,
        through_run: RunId,
        title: Option<String>,
    },
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
        checkpoint_scope: Option<CheckpointScope>,
        context: HistoricalContext,
        native: Option<NativeBinding>,
    },
    /// Without `through_run`, the latest completed run is the boundary.
    MergeBack {
        target: ThreadId,
        through_run: Option<RunId>,
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
        selection: ModelSelection,
        wake: CompletionWake,
    },
    AcceptDelegation {
        thread: ThreadId,
        project: String,
        title: String,
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        origin: Delegation,
        message: SendMessage,
    },
    TaskProgress {
        task: NodeId,
        progress: Option<String>,
        model: Option<String>,
    },
    TaskResult {
        source_message: Option<MessageId>,
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
    },
    NativeInput {
        attempt: RunAttemptId,
        event: Box<ProviderEvent>,
    },
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
    Error {
        message: String,
        retrying: bool,
        code: Option<String>,
        class: Option<String>,
        retryable: Option<bool>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderEvent {
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
    },
    Wake {
        text: String,
    },
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
    SetModel {
        selection: ModelSelection,
    },
    SetRuntimeMode {
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
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
    ForkNative {
        command: CommandId,
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
    /// Detach this thread's provider sessions, which stops their background work.
    DetachSessions {
        reason: String,
        revoke_credentials: bool,
    },
    CleanupTerminals,
    GenerateTitle {
        text: String,
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
        command: CommandId,
        native_thread: String,
    },
    ForkFailed {
        command: CommandId,
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
    },
    RollbackFinished {
        bindings: Vec<NativeBinding>,
        command: CommandId,
    },
    RollbackFailed {
        command: CommandId,
        message: String,
    },
    TitleGenerated {
        title: String,
    },
}
values! { ProviderOperation { Start, Steer, Interrupt, Respond, Compact, SetModel } }
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
