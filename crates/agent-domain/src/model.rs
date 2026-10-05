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
    TransferKind { Fork, MergeBack, ProviderHandoff, SubagentSpawn, SubagentResult }
    BackgroundKind { Command, Monitor, Subagent, BackgroundTask }
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
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub path: String,
    pub size: u64,
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
    pub usage: Option<TokenUsage>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemKind {
    UserMessage {
        message: MessageId,
    },
    AssistantMessage {
        message: MessageId,
    },
    Reasoning,
    CommandExecution {
        command: String,
        cwd: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        changes: Json,
    },
    DynamicTool {
        name: String,
        input: Json,
        output: Option<Json>,
    },
    WebSearch {
        query: String,
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
    BackgroundNotification {
        summary: String,
        outcome: ItemStatus,
        source: BackgroundKind,
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
pub type Answers = BTreeMap<String, Vec<String>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseCapability {
    Live,
    Message,
    NotResumable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
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
pub struct Checkpoint {
    pub id: CheckpointId,
    pub run: Option<RunId>,
    pub run_ordinal: u64,
    pub native_heads: BTreeMap<String, Option<String>>,
    pub file_ref: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: NodeId,
    pub native_key: String,
    pub run: Option<RunId>,
    pub attempt: RunAttemptId,
    pub child_thread: ThreadId,
    pub parent_task: Option<NodeId>,
    pub app_owned: bool,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transfer {
    pub id: ContextTransferId,
    pub kind: TransferKind,
    pub source: ThreadId,
    pub target: ThreadId,
    pub boundary: u64,
    pub text: String,
    pub consumed_by: Option<RunId>,
    pub superseded: bool,
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
/// Receipts are passed back to step by the owning actor, separately from projection facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub command: CommandId,
    pub fingerprint: String,
    pub reply: Reply,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub thread: Option<Thread>,
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
    pub rollback: Option<PendingRollback>,
    /// Captures that can be replayed after process loss, including their terminal status.
    pub captures: BTreeMap<RunId, RunStatus>,
    pub native_heads: BTreeMap<String, Option<String>>,
    pub rollback_failure: Option<String>,
    pub pending_children: BTreeMap<String, Vec<ProviderEvent>>,
    pub stopping: BTreeSet<RunAttemptId>,
    pub native_owner: Option<RunAttemptId>,
    pub native_parent: Option<(ThreadId, NodeId)>,
    pub prompt_echo_mode: PromptEchoMode,
    pub pending_prompt: Option<PendingPrompt>,
    pub native_continuations: BTreeMap<RunId, Vec<ProviderEvent>>,
    pub background_work: BTreeMap<String, BackgroundWork>,
    pub wake_reports: BTreeMap<String, String>,
    pub pending_forks: BTreeMap<CommandId, PendingFork>,
    pub native_sessions: BTreeMap<String, String>,
}
impl State {
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
        context: String,
        native: Option<NativeBinding>,
    },
    MergeBack {
        target: ThreadId,
    },
    AcceptTransfer {
        id: ContextTransferId,
        kind: TransferKind,
        source: ThreadId,
        boundary: u64,
        text: String,
    },
    Delegate {
        task: NodeId,
        child: ThreadId,
        prompt: String,
        selection: ModelSelection,
        wake: CompletionWake,
    },
    TaskResult {
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
        name: String,
        input: Json,
        output: Option<Json>,
    },
    WebSearch {
        query: String,
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
        key: String,
        body: RequestBody,
        capability: ResponseCapability,
    },
    RequestClosed {
        key: String,
    },
    Plan {
        key: String,
        markdown: String,
        steps: Vec<PlanStep>,
    },
    Usage(TokenUsage),
    SubagentStarted {
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
        selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        text: String,
        attachments: Vec<Attachment>,
        native_thread: Option<String>,
        resume_at: Option<String>,
        context: String,
    },
    Steer {
        text: String,
        attachments: Vec<Attachment>,
    },
    Interrupt,
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
        run: RunId,
    },
    RestoreCheckpoint {
        checkpoint: CheckpointId,
        file_ref: String,
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
    GenerateTitle {
        text: String,
    },
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
    },
    CheckpointCaptured {
        run: RunId,
        attempt: Option<RunAttemptId>,
        checkpoint: CheckpointId,
        file_ref: String,
    },
    CheckpointFailed {
        run: RunId,
        attempt: Option<RunAttemptId>,
        message: String,
    },
    RollbackFinished {
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
    Command {
        id: CommandId,
        command: Box<Command>,
        receipt: Option<Receipt>,
    },
    Provider {
        attempt: RunAttemptId,
        event: ProviderEvent,
    },
    Effect(EffectResult),
    Recover {
        trigger: RecoveryTrigger,
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
