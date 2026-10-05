//! Native contracts for T3 Code 4ee6bfd's orchestration-v2. No retired formats.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                if value.is_empty() || value.trim() != value {
                    return Err(ContractError::InvalidId);
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
    )+};
}
ids!(
    ThreadId,
    ProjectId,
    CommandId,
    EventId,
    MessageId,
    RunId,
    RunAttemptId,
    NodeId,
    ProviderSessionId,
    ProviderThreadId,
    ProviderTurnId,
    TurnItemId,
    RuntimeRequestId,
    PlanId,
    CheckpointScopeId,
    CheckpointId,
    CheckpointRef,
    ContextHandoffId,
    ContextTransferId,
    ProviderInstanceId,
    ScheduledTaskId
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("id must be a nonempty trimmed string")]
    InvalidId,
    #[error("timestamp must be RFC 3339")]
    InvalidTimestamp,
}

/// Normalize timestamps so lexical ordering agrees with chronological ordering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);
impl Timestamp {
    pub fn parse(value: &str) -> Result<Self, ContractError> {
        let time = chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| ContractError::InvalidTimestamp)?;
        Self::from_millis(time.timestamp_millis())
    }
    pub fn from_millis(value: i64) -> Result<Self, ContractError> {
        use chrono::Datelike;
        let time = chrono::DateTime::from_timestamp_millis(value)
            .filter(|time| (0..=9999).contains(&time.year()))
            .ok_or(ContractError::InvalidTimestamp)?;
        Ok(Self(
            time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn millis(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.0)
            .expect("validated timestamp")
            .timestamp_millis()
    }
}
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// JSON is open only at provider/tool boundaries; Postcard carries a string.
#[derive(Debug, Clone, PartialEq)]
pub struct Json(pub serde_json::Value);
impl Serialize for Json {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            self.0.serialize(s)
        } else {
            serde_json::to_string(&self.0)
                .map_err(serde::ser::Error::custom)?
                .serialize(s)
        }
    }
}
impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            serde_json::Value::deserialize(d).map(Self)
        } else {
            serde_json::from_str(&String::deserialize(d)?)
                .map(Self)
                .map_err(serde::de::Error::custom)
        }
    }
}

macro_rules! enums {
    ($($name:ident { $($variant:ident => $value:literal),+ $(,)? })+) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $value)] $variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $value),+ } }
        }
    )+};
}
enums! {
    Driver { Codex => "codex", Claude => "claude" }
    CreatedBy { User => "user", Agent => "agent", System => "system" }
    CreationSource { Desktop => "desktop", Mobile => "mobile", Mcp => "mcp", Provider => "provider", Server => "server" }
    RuntimeMode { ApprovalRequired => "approval-required", AutoAcceptEdits => "auto-accept-edits", Auto => "auto", FullAccess => "full-access" }
    InteractionMode { Default => "default", Plan => "plan" }
    Strength { Strong => "strong", Weak => "weak", None => "none" }
    Enforcement { Native => "native", ClientBoundary => "client-boundary" }
    SettledOverride { Settled => "settled", Active => "active" }
    Role { User => "user", Assistant => "assistant", System => "system" }
    RunStatus { Preparing => "preparing", Queued => "queued", Starting => "starting", Running => "running", Waiting => "waiting", Completed => "completed", Interrupted => "interrupted", Failed => "failed", Cancelled => "cancelled", RolledBack => "rolled_back" }
    AttemptStatus { Pending => "pending", Running => "running", Completed => "completed", Interrupted => "interrupted", Failed => "failed", Cancelled => "cancelled", Superseded => "superseded" }
    AttemptReason { Initial => "initial", SteeringRestart => "steering_restart", Retry => "retry", ProviderRecovery => "provider_recovery" }
    NodeKind { RootTurn => "root_turn", AssistantMessage => "assistant_message", Reasoning => "reasoning", Plan => "plan", TodoList => "todo_list", ToolCall => "tool_call", ApprovalRequest => "approval_request", UserInputRequest => "user_input_request", Subagent => "subagent", Hook => "hook", System => "system" }
    NodeStatus { Idle => "idle", Pending => "pending", Running => "running", Waiting => "waiting", Completed => "completed", Interrupted => "interrupted", Failed => "failed", Cancelled => "cancelled", RolledBack => "rolled_back" }
    ItemStatus { Idle => "idle", Pending => "pending", Running => "running", Waiting => "waiting", Completed => "completed", Failed => "failed", Cancelled => "cancelled", Interrupted => "interrupted" }
    SessionStatus { Starting => "starting", Ready => "ready", Running => "running", Waiting => "waiting", Stopped => "stopped", Error => "error" }
    ProviderThreadStatus { NotLoaded => "not_loaded", Idle => "idle", Active => "active", Archived => "archived", Closed => "closed", Error => "error" }
    TurnStatus { Pending => "pending", Running => "running", Completed => "completed", Interrupted => "interrupted", Failed => "failed", Cancelled => "cancelled" }
    RequestKind { Command => "command", FileRead => "file-read", FileChange => "file-change", McpElicitation => "mcp-elicitation", Permission => "permission", DynamicToolCall => "dynamic_tool_call", UserInput => "user_input", AuthRefresh => "auth_refresh" }
    RequestStatus { Pending => "pending", Resolved => "resolved", Expired => "expired", Cancelled => "cancelled" }
    ApprovalDecision { Accept => "accept", AcceptForSession => "acceptForSession", AcceptAlways => "acceptAlways", Decline => "decline", Cancel => "cancel" }
    InputIntent { TurnStart => "turn_start", QueuedTurn => "queued_turn", Steer => "steer", PromotedQueuedToSteer => "promoted_queued_to_steer" }
    PlanStatus { Draft => "draft", Active => "active", Completed => "completed", Superseded => "superseded" }
    StepStatus { Pending => "pending", Running => "running", Completed => "completed" }
    FailureClass { UsageLimit => "usage_limit", ProviderError => "provider_error", TransportError => "transport_error", PermissionError => "permission_error", ValidationError => "validation_error", Unknown => "unknown" }
    CheckpointStatus { Ready => "ready", Missing => "missing", Error => "error", Stale => "stale" }
    ScopeKind { RootRun => "root_run", Subagent => "subagent", Tool => "tool", ProviderThread => "provider_thread", Manual => "manual" }
    Visibility { Local => "local", Inherited => "inherited", Synthetic => "synthetic" }
    TransferKind { Fork => "fork", ProviderHandoff => "provider_handoff", MergeBack => "merge_back", SubagentSpawn => "subagent_spawn", SubagentResult => "subagent_result" }
    TransferStatus { Pending => "pending", ResolvedNative => "resolved_native", ResolvedPortable => "resolved_portable", Failed => "failed", Consumed => "consumed", Superseded => "superseded" }
    HandoffStrategy { DeltaSinceTargetLastSeen => "delta_since_target_last_seen", ForkDeltaSummary => "fork_delta_summary", FullThreadSummary => "full_thread_summary", CheckpointSummary => "checkpoint_summary", ManualContext => "manual_context" }
    HandoffStatus { Pending => "pending", Ready => "ready", Failed => "failed", Superseded => "superseded" }
    SubagentOrigin { ProviderNative => "provider_native", AppOwned => "app_owned" }
    CompletionWake { Always => "always", SettledOnly => "settled_only" }
    DeliveryIntent { Auto => "auto", Steer => "steer", Restart => "restart" }
}
impl RunStatus {
    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled | Self::RolledBack
        )
    }
}

macro_rules! flags {
    ($($name:ident { $($field:ident),+ $(,)? })+) => {$ (
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $(pub $field: bool),+ }
    )+};
}
flags! {
    SessionCapabilities { supports_multiple_provider_threads_per_session, supports_model_switch_in_session, supports_provider_switching_via_handoff, supports_runtime_mode_switch_in_session, pending_requests_survive_restart }
    ThreadCapabilities { can_create_empty_thread, can_read_thread_snapshot, can_rollback_thread, can_fork_thread, can_fork_from_turn, can_fork_from_subagent_thread, exposes_native_thread_id }
    StreamingCapabilities { streams_assistant_text, streams_reasoning, streams_tool_output, streams_plan_text, emits_message_completed }
    ToolCapabilities { exposes_tool_item_ids, emits_tool_started, emits_tool_completed, emits_tool_output, supports_mcp_tools, supports_dynamic_tool_callbacks }
    ApprovalCapabilities { supports_command_approval, supports_file_read_approval, supports_file_change_approval, supports_apply_patch_approval, approvals_have_native_request_ids, approval_callbacks_are_live_only, approvals_can_originate_from_subagents }
    PlanningCapabilities { emits_plan_updated, emits_todo_list, emits_proposed_plan, supports_structured_questions, plan_deltas_have_item_ids }
    SubagentCapabilities { supports_subagents, exposes_subagent_thread_ids, emits_subagent_lifecycle, can_wait_for_subagents, can_close_subagents, can_fork_subagent_thread }
    CheckpointCapabilities { app_can_checkpoint_filesystem, supports_nested_checkpoint_scopes, provider_can_rollback_conversation, provider_rollback_returns_snapshot, provider_can_read_conversation_snapshot }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCapabilities {
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: Strength,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    pub max_recommended_handoff_chars: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapabilities {
    pub native_thread_ids: Strength,
    pub native_turn_ids: Strength,
    pub native_item_ids: Strength,
    pub native_request_ids: Strength,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub sessions: SessionCapabilities,
    pub threads: ThreadCapabilities,
    pub turns: TurnCapabilities,
    pub streaming: StreamingCapabilities,
    pub tools: ToolCapabilities,
    pub approvals: ApprovalCapabilities,
    pub planning: PlanningCapabilities,
    pub subagents: SubagentCapabilities,
    pub context: ContextCapabilities,
    pub checkpointing: CheckpointCapabilities,
    pub identity: IdentityCapabilities,
    pub runtime_policy: Enforcement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: ProviderInstanceId,
    pub model: String,
    pub options: BTreeMap<String, Json>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub driver: Driver,
    pub native_id: Option<String>,
    pub strength: Strength,
    pub fingerprint: Option<String>,
    pub ordinal: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relationship {
    Fork,
    Subagent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<Relationship>,
    pub root_thread_id: ThreadId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkSource {
    Run {
        thread_id: ThreadId,
        run_id: RunId,
    },
    Node {
        node_id: NodeId,
    },
    ProviderThread {
        provider_thread_id: ProviderThreadId,
        provider_turn_id: Option<ProviderTurnId>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
    pub created_by: CreatedBy,
    pub creation_source: CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    pub lineage: Lineage,
    pub forked_from: Option<ForkSource>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<Timestamp>,
    pub unsettled_at: Option<Timestamp>,
    pub snoozed_until: Option<Timestamp>,
    pub snoozed_at: Option<Timestamp>,
    pub pinned_at: Option<Timestamp>,
    pub auto_settle_disabled_at: Option<Timestamp>,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub last_visited_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    pub imported: bool,
    pub rollback_request_id: Option<CommandId>,
    pub rollback_failure: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
pub struct Run {
    pub delegated_completion: Option<DelegatedCompletionCohort>,
    pub id: RunId,
    pub thread_id: ThreadId,
    pub ordinal: u64,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: RunStatus,
    pub queue_position: Option<u64>,
    pub queue_held: bool,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub checkpoint_id: Option<CheckpointId>,
    pub context_handoff_id: Option<ContextHandoffId>,
    pub source_plan_ref: Option<SourcePlanRef>,
    pub workspace_preparation: Option<WorkspaceStrategy>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedDeliveryState {
    Pending,
    Claimed,
    Acknowledged,
    Delivered,
    Disposed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedTaskDelivery {
    pub state: DelegatedDeliveryState,
    pub observed_by_run_id: Option<RunId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortDisposition {
    Open,
    Stopped,
    Disposed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletion {
    pub parent_run_id: RunId,
    pub generation: u64,
    pub task_ids: Vec<NodeId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedDelivery {
    pub generation: u64,
    pub message_id: MessageId,
    pub task_ids: Vec<NodeId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionCohort {
    pub disposition: CohortDisposition,
    pub next_generation: u64,
    pub delivery: Option<DelegatedDelivery>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlanRef {
    pub thread_id: ThreadId,
    pub plan_id: PlanId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: RunAttemptId,
    pub run_id: RunId,
    pub attempt_ordinal: u64,
    pub root_node_id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: AttemptReason,
    pub status: AttemptStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub checkpoint_scope_id: Option<CheckpointScopeId>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: NodeId,
    pub origin: SubagentOrigin,
    pub created_by: CreatedBy,
    pub driver: Driver,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub child_thread_id: Option<ThreadId>,
    pub native_task_ref: Option<ProviderRef>,
    pub prompt: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub completion_wake: CompletionWake,
    pub completion_delivery: Option<DelegatedTaskDelivery>,
    pub status: NodeStatus,
    pub progress: Option<String>,
    pub result: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: ProviderSessionId,
    pub driver: Driver,
    pub provider_instance_id: ProviderInstanceId,
    pub status: SessionStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub used_tokens: u64,
    pub max_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    pub task_id: String,
    pub description: Option<String>,
    pub kind: BackgroundTaskKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    Subagent { child_thread_id: Option<ThreadId> },
    Command,
    Monitor,
    BackgroundTask,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: ProviderThreadId,
    pub driver: Driver,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: Option<ThreadId>,
    pub owner_node_id: Option<NodeId>,
    pub native_thread_ref: Option<ProviderRef>,
    pub native_conversation_head_ref: Option<ProviderRef>,
    pub status: ProviderThreadStatus,
    pub first_run_ordinal: Option<u64>,
    pub last_run_ordinal: Option<u64>,
    pub handoff_ids: Vec<ContextHandoffId>,
    pub forked_from: Option<ProviderFork>,
    pub pending_background_tasks: Vec<BackgroundTask>,
    pub context_usage: Option<TokenUsage>,
    pub native_metadata: Option<NativeMetadata>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFork {
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub checkpoint_id: Option<CheckpointId>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeMetadata {
    pub model_selection: Option<ModelSelection>,
    pub title: Option<String>,
    pub updated_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub node_id: NodeId,
    pub run_attempt_id: Option<RunAttemptId>,
    pub native_turn_ref: Option<ProviderRef>,
    pub ordinal: u64,
    pub status: TurnStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub token_usage: Option<TokenUsage>,
    pub turn_token_usage: Option<TokenUsage>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseCapability {
    Live {
        provider_session_id: ProviderSessionId,
    },
    Message,
    NotResumable {
        reason: String,
    },
}
pub type Answers = BTreeMap<String, Json>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_request_ref: Option<ProviderRef>,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub response_capability: ResponseCapability,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    pub decision: Option<ApprovalDecision>,
    pub answers: Option<Answers>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContext {
    pub version: u32,
    pub records: Vec<ContextRecord>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRecord {
    Image { attachment_id: String },
    File { path: String, text: Option<String> },
    Terminal { text: String },
    Element { text: String },
    PreviewAnnotation { text: String },
    ReviewComment { path: String, text: String },
    Mention { name: String },
    Skill { name: String },
    Thread { thread_id: ThreadId },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub native_continuation: Option<Box<NativeContinuationRef>>,
    pub delegated_completion: Option<Box<DelegatedCompletion>>,
    pub created_by: CreatedBy,
    pub creation_source: CreationSource,
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: Role,
    pub text: String,
    pub context: Option<MessageContext>,
    pub attachments: Vec<Attachment>,
    pub streaming: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: StepStatus,
    pub duration_anchor_at: Option<Timestamp>,
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
    pub allow_custom_answer: bool,
    pub required: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    pub value: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub decision: ApprovalDecision,
    pub label: String,
    pub warning: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: FailureClass,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    pub reset_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub attempt: u32,
    pub max_attempts: Option<u32>,
    pub retry_delay_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub operation: String,
    pub path: String,
    pub old_path: Option<String>,
    pub file_type: Option<String>,
    pub mime_type: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub file_name: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub preview: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebResult {
    pub title: Option<String>,
    pub url: Option<String>,
    pub snippet: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub parent_item_id: Option<TurnItemId>,
    pub ordinal: u64,
    pub status: ItemStatus,
    pub title: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub body: TurnItemBody,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemBody {
    Notification {
        source: String,
        outcome: ItemStatus,
        summary: String,
        detail: Option<String>,
    },
    UserMessage {
        created_by: CreatedBy,
        creation_source: CreationSource,
        message_id: MessageId,
        input_intent: InputIntent,
        text: String,
        context: Option<MessageContext>,
        attachments: Vec<Attachment>,
    },
    AssistantMessage {
        message_id: MessageId,
        text: String,
        attachments: Vec<Attachment>,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        plan_id: PlanId,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        plan_id: PlanId,
        steps: Vec<PlanStep>,
        explanation: Option<String>,
    },
    UserInputRequest {
        request_id: RuntimeRequestId,
        questions: Vec<UserInputQuestion>,
        question_answer: Option<Answers>,
        response_mode_message: bool,
    },
    FileChange {
        file_name: String,
        additions: Option<u64>,
        deletions: Option<u64>,
        diff_str: Option<String>,
        old_str: Option<String>,
        new_str: Option<String>,
        changes: Vec<FileChange>,
    },
    CommandExecution {
        input: String,
        output: Option<String>,
        output_omitted: bool,
        output_indicates_failure: bool,
        exit_code: Option<i32>,
    },
    FileSearch {
        pattern: Option<String>,
        results: Vec<SearchResult>,
    },
    WebSearch {
        patterns: Vec<String>,
        results: Vec<WebResult>,
    },
    ApprovalRequest {
        request_id: RuntimeRequestId,
        request_kind: RequestKind,
        prompt: Option<String>,
        app_name: Option<String>,
        options: Vec<ApprovalOption>,
    },
    Checkpoint {
        checkpoint_id: CheckpointId,
        scope_id: CheckpointScopeId,
        files: Vec<CheckpointFileSummary>,
    },
    RunInterruptRequest {
        message: String,
    },
    RunInterruptResult {
        message: String,
    },
    SystemNotice {
        message: String,
    },
    Error {
        failure: ProviderFailure,
        retry: Option<Retry>,
    },
    Compaction {
        driver: Option<Driver>,
        summary: Option<String>,
        before_token_count: Option<u64>,
        after_token_count: Option<u64>,
    },
    Handoff {
        context_handoff_id: ContextHandoffId,
        from_provider_thread_ids: Vec<ProviderThreadId>,
        to_provider_thread_id: ProviderThreadId,
        from_provider_instance_ids: Vec<ProviderInstanceId>,
        to_provider_instance_id: ProviderInstanceId,
        strategy: HandoffStrategy,
        summary: Option<String>,
    },
    Fork {
        source: ForkSource,
        target_thread_id: ThreadId,
        provider_thread_id: Option<ProviderThreadId>,
    },
    ThreadCreated {
        target_thread_id: ThreadId,
        target_run_id: Option<RunId>,
        target_provider_instance_id: ProviderInstanceId,
        target_model: String,
    },
    Subagent {
        subagent_id: NodeId,
        origin: SubagentOrigin,
        driver: Driver,
        provider_instance_id: ProviderInstanceId,
        child_thread_id: Option<ThreadId>,
        prompt: String,
        progress: Option<String>,
        result: Option<String>,
    },
    DynamicTool {
        tool_name: Option<String>,
        viewed_image_path: Option<String>,
        input: Json,
        output: Option<Json>,
        output_omitted: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanArtifact {
    pub id: PlanId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub status: PlanStatus,
    pub detail_in_turn_item: bool,
    pub body: PlanBody,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanBody {
    ProposedPlan {
        markdown: String,
    },
    TodoList {
        steps: Vec<PlanStep>,
        explanation: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointScope {
    pub id: CheckpointScopeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_scope_id: Option<CheckpointScopeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub kind: ScopeKind,
    pub ordinal_within_parent: u64,
    pub advances_app_run_count: bool,
    pub cwd: String,
    pub created_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFileSummary {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}
/// A scope ordinal is independent of the application's root-run ordinal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointCapture {
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub attempt_id: Option<RunAttemptId>,
    pub node_id: NodeId,
    pub ordinal_within_scope: u64,
    pub app_run_ordinal: Option<u64>,
    pub parent_checkpoint_id: Option<CheckpointId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub thread_id: ThreadId,
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub ordinal_within_scope: u64,
    pub app_run_ordinal: Option<u64>,
    pub reference: CheckpointRef,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    pub captured_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourcePoint {
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub checkpoint_id: Option<CheckpointId>,
    pub turn_item_id: Option<TurnItemId>,
    pub provider_thread_ref: Option<ProviderRef>,
    pub provider_turn_ref: Option<ProviderRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferResolution {
    NativeFork {
        provider_thread_ref: ProviderRef,
    },
    PortableContext {
        context_handoff_id: ContextHandoffId,
    },
    DeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    ForkDeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    CheckpointContext {
        context_handoff_id: ContextHandoffId,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransfer {
    pub id: ContextTransferId,
    pub kind: TransferKind,
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub source_point: ContextSourcePoint,
    pub base_point: Option<ContextSourcePoint>,
    pub source_provider_instance_id: Option<ProviderInstanceId>,
    pub target_provider_instance_id: Option<ProviderInstanceId>,
    pub target_run_id: Option<RunId>,
    pub status: TransferStatus,
    pub resolution: Option<TransferResolution>,
    pub created_by: CreatedBy,
    pub error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub consumed_at: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHandoff {
    pub id: ContextHandoffId,
    pub transfer_id: Option<ContextTransferId>,
    pub thread_id: ThreadId,
    pub target_run_id: RunId,
    pub from_provider_thread_ids: Vec<ProviderThreadId>,
    pub to_provider_thread_id: ProviderThreadId,
    pub covered_run_ordinals: (u64, u64),
    pub strategy: HandoffStrategy,
    pub status: HandoffStatus,
    pub summary_message_id: Option<MessageId>,
    pub summary_text: String,
    pub created_by_provider_instance_id: Option<ProviderInstanceId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    pub runs: crate::Shared<Vec<Run>>,
    pub attempts: crate::Shared<Vec<RunAttempt>>,
    pub nodes: crate::Shared<Vec<ExecutionNode>>,
    pub subagents: crate::Shared<Vec<Subagent>>,
    pub provider_sessions: crate::Shared<Vec<ProviderSession>>,
    pub provider_threads: crate::Shared<Vec<ProviderThread>>,
    pub provider_turns: crate::Shared<Vec<ProviderTurn>>,
    pub runtime_requests: crate::Shared<Vec<RuntimeRequest>>,
    pub messages: crate::Shared<Vec<ConversationMessage>>,
    pub plans: crate::Shared<Vec<PlanArtifact>>,
    pub turn_items: crate::Shared<Vec<TurnItem>>,
    pub checkpoint_scopes: crate::Shared<Vec<CheckpointScope>>,
    pub checkpoints: crate::Shared<Vec<Checkpoint>>,
    pub context_handoffs: crate::Shared<Vec<ContextHandoff>>,
    pub context_transfers: crate::Shared<Vec<ContextTransfer>>,
    pub visible_turn_items: crate::Shared<Vec<ProjectedTurnItem>>,
    pub updated_at: Timestamp,
}
impl ThreadProjection {
    pub fn empty(thread: AppThread) -> Self {
        Self {
            updated_at: thread.updated_at.clone(),
            thread,
            runs: vec![].into(),
            attempts: vec![].into(),
            nodes: vec![].into(),
            subagents: vec![].into(),
            provider_sessions: vec![].into(),
            provider_threads: vec![].into(),
            provider_turns: vec![].into(),
            runtime_requests: vec![].into(),
            messages: vec![].into(),
            plans: vec![].into(),
            turn_items: vec![].into(),
            checkpoint_scopes: vec![].into(),
            checkpoints: vec![].into(),
            context_handoffs: vec![].into(),
            context_transfers: vec![].into(),
            visible_turn_items: vec![].into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: Visibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: TurnItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub occurred_at: Timestamp,
    pub payload: EventPayload,
}
macro_rules! events {
    ($($variant:ident($ty:ty) => $name:literal),+ $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub enum EventPayload { $(#[serde(rename = $name)] $variant($ty)),+ }
        impl EventPayload { pub fn event_type(&self) -> &'static str { match self { $(Self::$variant(_) => $name),+ } } }
    };
}
events! {
    ThreadCreated(AppThread) => "thread.created", ThreadArchived(AppThread) => "thread.archived",
    ThreadUnarchived(AppThread) => "thread.unarchived", ThreadDeleted(AppThread) => "thread.deleted",
    ThreadSettled(AppThread) => "thread.settled", ThreadUnsettled(AppThread) => "thread.unsettled",
    ThreadSnoozed(AppThread) => "thread.snoozed", ThreadUnsnoozed(AppThread) => "thread.unsnoozed",
    ThreadPinned(AppThread) => "thread.pinned", ThreadUnpinned(AppThread) => "thread.unpinned",
    ThreadAutoSettleSet(AppThread) => "thread.auto-settle-set", ThreadPinReordered(AppThread) => "thread.pin-reordered",
    ThreadActiveReordered(AppThread) => "thread.active-reordered", ThreadVisited(AppThread) => "thread.visited",
    ThreadMarkedUnread(AppThread) => "thread.marked-unread", ThreadMetadataUpdated(AppThread) => "thread.metadata-updated",
    ThreadRuntimeModeUpdated(AppThread) => "thread.runtime-mode-updated", ThreadInteractionModeUpdated(AppThread) => "thread.interaction-mode-updated",
    ThreadModelSelectionUpdated(AppThread) => "thread.model-selection-updated", ThreadProviderSwitched(AppThread) => "thread.provider-switched",
    RunCreated(Run) => "run.created", RunUpdated(Run) => "run.updated",
    RunAttemptCreated(RunAttempt) => "run-attempt.created", RunAttemptUpdated(RunAttempt) => "run-attempt.updated",
    NodeUpdated(ExecutionNode) => "node.updated", SubagentUpdated(Subagent) => "subagent.updated",
    ProviderSessionAttached(ProviderSession) => "provider-session.attached", ProviderSessionUpdated(ProviderSession) => "provider-session.updated",
    ProviderSessionDetached(ProviderSessionId) => "provider-session.detached", ProviderThreadUpdated(ProviderThread) => "provider-thread.updated",
    ProviderTurnUpdated(ProviderTurn) => "provider-turn.updated", RuntimeRequestUpdated(RuntimeRequest) => "runtime-request.updated",
    MessageUpdated(ConversationMessage) => "message.updated", TurnItemUpdated(TurnItem) => "turn-item.updated",
    TurnItemTextDelta(TurnItemTextDelta) => "turn-item.text-delta",
    PlanUpdated(PlanArtifact) => "plan.updated", CheckpointScopeCreated(CheckpointScope) => "checkpoint-scope.created",
    CheckpointCaptured(Checkpoint) => "checkpoint.captured", CheckpointRollbackRequested(CheckpointRollbackRequest) => "checkpoint.rollback-requested",
    ContextHandoffUpdated(ContextHandoff) => "context-handoff.updated", ContextTransferCreated(ContextTransfer) => "context-transfer.created",
    ContextTransferUpdated(ContextTransfer) => "context-transfer.updated"
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItemTextDelta {
    pub item_id: TurnItemId,
    pub run_id: Option<RunId>,
    pub offset: usize,
    pub text: String,
}
/// A provider-native child belongs to an accepted parent attempt, even after
/// that attempt has returned while the child is still working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSubagentOwner {
    pub parent_thread_id: ThreadId,
    pub task_id: NodeId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeContinuationRef {
    pub provider_thread_id: ProviderThreadId,
    pub run_id: RunId,
    pub attempt_id: RunAttemptId,
    pub task_id: NodeId,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeContinuationOffer {
    pub message_id: MessageId,
    pub source: NativeContinuationRef,
    pub summary: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRollbackRequest {
    pub scope_id: CheckpointScopeId,
    pub checkpoint_id: CheckpointId,
    pub requested_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvent {
    pub sequence: u64,
    pub command_id: Option<CommandId>,
    pub event: DomainEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPoint {
    LatestStable,
    Run { run_id: RunId },
    Checkpoint { checkpoint_id: CheckpointId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub body: CommandBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedTaskRequest {
    pub parent_run_id: RunId,
    pub parent_node_id: NodeId,
    pub task: String,
    pub title: Option<String>,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub completion_wake: CompletionWake,
    pub created_by: CreatedBy,
    pub creation_source: CreationSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandBody {
    #[serde(rename = "thread.create")]
    ThreadCreate {
        created_by: CreatedBy,
        creation_source: CreationSource,
        project_id: ProjectId,
        title: String,
        model_selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        branch: Option<String>,
        worktree_path: Option<String>,
    },
    #[serde(rename = "thread.archive")]
    ThreadArchive,
    #[serde(rename = "thread.unarchive")]
    ThreadUnarchive,
    #[serde(rename = "thread.delete")]
    ThreadDelete,
    #[serde(rename = "thread.settle")]
    ThreadSettle { settled_at: Option<Timestamp> },
    #[serde(rename = "thread.unsettle")]
    ThreadUnsettle,
    #[serde(rename = "thread.snooze")]
    ThreadSnooze { snoozed_until: Timestamp },
    #[serde(rename = "thread.unsnooze")]
    ThreadUnsnooze,
    #[serde(rename = "thread.auto-settle.set")]
    ThreadAutoSettleSet { enabled: bool },
    #[serde(rename = "thread.pin")]
    ThreadPin { order_key: Option<String> },
    #[serde(rename = "thread.unpin")]
    ThreadUnpin,
    #[serde(rename = "thread.pin.reorder")]
    ThreadPinReorder { order_key: String },
    #[serde(rename = "thread.active.reorder")]
    ThreadActiveReorder { order_key: String },
    #[serde(rename = "thread.visit")]
    ThreadVisit { visited_at: Timestamp },
    #[serde(rename = "thread.mark-unread")]
    ThreadMarkUnread,
    #[serde(rename = "thread.metadata.update")]
    ThreadMetadataUpdate { title: String },
    #[serde(rename = "thread.runtime-mode.set")]
    ThreadRuntimeModeSet { runtime_mode: RuntimeMode },
    #[serde(rename = "thread.interaction-mode.set")]
    ThreadInteractionModeSet { interaction_mode: InteractionMode },
    #[serde(rename = "thread.model-selection.set")]
    ThreadModelSelectionSet { model_selection: ModelSelection },
    #[serde(rename = "provider.switch")]
    ProviderSwitch { model_selection: ModelSelection },
    #[serde(rename = "provider-session.detach")]
    ProviderSessionDetach {
        provider_session_id: ProviderSessionId,
    },
    #[serde(rename = "message.dispatch")]
    MessageDispatch(Box<MessageDispatch>),
    #[serde(rename = "prepared-run.release")]
    PreparedRunRelease { run_id: RunId },
    #[serde(rename = "prepared-run.fail")]
    PreparedRunFail {
        run_id: RunId,
        failure: ProviderFailure,
    },
    #[serde(rename = "prepared-run.retry")]
    PreparedRunRetry { run_id: RunId },
    #[serde(rename = "run.interrupt")]
    RunInterrupt {
        run_id: RunId,
        reason: Option<String>,
        hold_queue: bool,
    },
    #[serde(rename = "queue.resume")]
    QueueResume,
    #[serde(rename = "queued-run.reorder")]
    QueuedRunReorder {
        run_id: RunId,
        before_run_id: Option<RunId>,
    },
    #[serde(rename = "queued-run.cancel")]
    QueuedRunCancel { run_id: RunId },
    #[serde(rename = "queued-run.edit")]
    QueuedRunEdit {
        run_id: RunId,
        text: String,
        context: Option<MessageContext>,
        attachments: Option<Vec<Attachment>>,
    },
    #[serde(rename = "queued-message.promote-to-steer")]
    QueuedMessagePromoteToSteer {
        queued_run_id: RunId,
        target_run_id: RunId,
    },
    #[serde(rename = "runtime-request.respond")]
    RuntimeRequestRespond {
        request_id: RuntimeRequestId,
        decision: Option<ApprovalDecision>,
        answers: Option<Answers>,
    },
    #[serde(rename = "thread.fork")]
    ThreadFork {
        target_thread_id: ThreadId,
        source_point: ForkPoint,
        title: Option<String>,
        created_by: CreatedBy,
        creation_source: CreationSource,
    },
    #[serde(rename = "thread.merge_back")]
    ThreadMergeBack {
        target_thread_id: ThreadId,
        source_point: ForkPoint,
        created_by: CreatedBy,
    },
    #[serde(rename = "checkpoint.rollback")]
    CheckpointRollback {
        scope_id: CheckpointScopeId,
        checkpoint_id: CheckpointId,
        restore_files: bool,
    },
    #[serde(rename = "delegated_task.request")]
    DelegatedTaskRequest(Box<DelegatedTaskRequest>),
    #[serde(rename = "delegated_task.wake-policy")]
    DelegatedTaskWakePolicy {
        task_id: NodeId,
        completion_wake: CompletionWake,
    },
    #[serde(rename = "delegated_task.completion-delivery.acknowledge")]
    DelegatedTaskAcknowledge {
        task_id: NodeId,
        observed_by_run_id: Option<RunId>,
    },
    #[serde(rename = "delegated_task.completion-delivery.dispose")]
    DelegatedTaskDispose { task_id: NodeId },
    #[serde(rename = "notification.delivery.accept")]
    NotificationDeliveryAccept { message_id: MessageId },
    #[serde(rename = "thread.user-input.dismiss")]
    ThreadUserInputDismiss { request_id: RuntimeRequestId },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDispatch {
    pub native_continuation: Option<Box<NativeContinuationRef>>,
    pub delegated_completion: Option<Box<DelegatedCompletion>>,
    pub source_plan_ref: Option<SourcePlanRef>,
    pub created_by: CreatedBy,
    pub creation_source: CreationSource,
    pub message_id: MessageId,
    pub text: String,
    pub context: Option<MessageContext>,
    pub attachments: Vec<Attachment>,
    pub model_selection: Option<ModelSelection>,
    pub delivery_intent: Option<DeliveryIntent>,
    pub dispatch_mode: DispatchMode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    DeferStart {
        workspace_strategy: Option<WorkspaceStrategy>,
    },
    SteerActive {
        target_run_id: RunId,
    },
    RestartActive {
        target_run_id: RunId,
    },
    QueueAfterActive,
    StartImmediately,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub id: String,
    pub thread_id: ThreadId,
    pub body: EffectBody,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectBody {
    #[serde(rename = "provider-thread.rollback")]
    Rollback {
        request_id: CommandId,
        provider_thread_id: ProviderThreadId,
        checkpoint_id: CheckpointId,
        scope_id: CheckpointScopeId,
        restore_files: bool,
    },
    #[serde(rename = "checkpoint.capture")]
    CaptureCheckpoint { run_id: RunId },
    #[serde(rename = "checkpoint.capture-scoped")]
    CaptureScopedCheckpoint { capture: Box<CheckpointCapture> },
    #[serde(rename = "provider-turn.start")]
    Start { run_id: RunId },
    #[serde(rename = "provider-turn.interrupt")]
    Interrupt {
        run_id: RunId,
        provider_turn_id: ProviderTurnId,
    },
    #[serde(rename = "provider-turn.steer")]
    Steer {
        run_id: RunId,
        provider_turn_id: ProviderTurnId,
        message_id: MessageId,
    },
    #[serde(rename = "provider-turn.restart")]
    Restart {
        run_id: RunId,
        provider_turn_id: ProviderTurnId,
        message_id: MessageId,
        attempt_id: RunAttemptId,
    },
    #[serde(rename = "runtime-request.respond")]
    Respond {
        request_id: RuntimeRequestId,
        decision: Option<ApprovalDecision>,
        answers: Option<Answers>,
    },
    #[serde(rename = "provider-session.detach")]
    Detach {
        provider_session_id: ProviderSessionId,
        driver: Driver,
    },
    #[serde(rename = "terminal.cleanup")]
    TerminalCleanup,
    #[serde(rename = "attachment.cleanup")]
    AttachmentCleanup,
}
impl EffectBody {
    pub fn process_bound(&self) -> bool {
        matches!(
            self,
            Self::Start { .. }
                | Self::Interrupt { .. }
                | Self::Steer { .. }
                | Self::Restart { .. }
                | Self::Respond { .. }
        )
    }
    pub fn run_id(&self) -> Option<&RunId> {
        match self {
            Self::Start { run_id }
            | Self::CaptureCheckpoint { run_id }
            | Self::Interrupt { run_id, .. }
            | Self::Steer { run_id, .. }
            | Self::Restart { run_id, .. } => Some(run_id),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decision {
    pub events: Vec<DomainEvent>,
    pub effects: Vec<Effect>,
    pub cancel_unsettled_effects: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub thread: AppThread,
    pub latest_run_id: Option<RunId>,
    pub active_run_id: Option<RunId>,
    pub status: Option<RunStatus>,
    pub pending_runtime_request: Option<PendingRuntimeRequest>,
    pub latest_visible_message: Option<VisibleMessage>,
    pub latest_user_message_at: Option<Timestamp>,
    pub latest_run_requested_at: Option<Timestamp>,
    pub latest_run_started_at: Option<Timestamp>,
    pub latest_run_completed_at: Option<Timestamp>,
    pub active_run_started_at: Option<Timestamp>,
    pub has_actionable_proposed_plan: bool,
    pub pending_background_tasks: Vec<BackgroundTask>,
    pub provider_instance_history: Vec<ProviderInstanceId>,
    pub item_count: u64,
    pub visible_item_count: u64,
    pub last_error: Option<ProviderFailure>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequest {
    pub id: RuntimeRequestId,
    pub kind: RequestKind,
    pub created_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleMessage {
    pub id: MessageId,
    pub role: Role,
    pub text: String,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub schema_version: u32,
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellStreamItem {
    Synchronized,
    Snapshot(ShellSnapshot),
    ThreadUpdated {
        sequence: u64,
        archived: bool,
        thread: Box<ThreadShell>,
    },
    ThreadRemoved {
        sequence: u64,
        thread_id: ThreadId,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStreamItem {
    Synchronized,
    Snapshot {
        snapshot_sequence: u64,
        projection: Box<ThreadProjection>,
        history_cursor: Option<HistoryCursor>,
        has_more_history: bool,
        latest_local_turn_ordinal: Option<u64>,
    },
    Event(Box<StoredEvent>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCursor {
    pub position: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadHistoryPage {
    pub snapshot_sequence: u64,
    pub items: Vec<ProjectedTurnItem>,
    pub next_cursor: Option<HistoryCursor>,
    pub has_more_history: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub source: Role,
    pub snippet: String,
    pub message_created_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    #[test]
    fn validates_native_and_json_ids() {
        assert!(ThreadId::new(" ").is_err());
        assert!(serde_json::from_str::<ThreadId>("\" thread\"").is_err());
        let bytes = postcard::to_allocvec(&" ").unwrap();
        assert!(postcard::from_bytes::<ThreadId>(&bytes).is_err());
    }
    #[test]
    fn timestamp_offsets_are_normalized_before_comparison() {
        assert_eq!(
            Timestamp::parse("2026-10-05T09:00:00+09:00").unwrap(),
            now()
        );
        assert!(Timestamp::parse("invalid").is_err());
        assert!(Timestamp::parse("9999-12-31T23:59:59-01:00").is_err());
        assert!(Timestamp::parse("0000-01-01T00:00:00+01:00").is_err());
        assert!(Timestamp::from_millis(253402300800000).is_err());
        let edge = Timestamp::parse("9999-12-31T23:59:59Z").unwrap();
        assert_eq!(
            serde_json::from_str::<Timestamp>(&serde_json::to_string(&edge).unwrap()).unwrap(),
            edge
        );
    }
    #[test]
    fn contracts_roundtrip_over_postcard_including_dynamic_json() {
        let command = send(
            "send",
            DispatchMode::SteerActive {
                target_run_id: RunId::new("run").unwrap(),
            },
        );
        assert_eq!(
            postcard::from_bytes::<Command>(&postcard::to_allocvec(&command).unwrap()).unwrap(),
            command
        );
        let mut projection = running();
        projection.thread.model_selection.options.insert(
            "effort".into(),
            Json(serde_json::json!({"level": [1, "high", null]})),
        );
        assert_eq!(
            postcard::from_bytes::<ThreadProjection>(&postcard::to_allocvec(&projection).unwrap())
                .unwrap(),
            projection
        );
    }
}
