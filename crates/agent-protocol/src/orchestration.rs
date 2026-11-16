//! Canonical orchestration records, following T3 Code 1a12c4af's contracts.
//! SQL projections and their events serialize these same records. Message and
//! tool bodies belong to turn items; other records refer to those item IDs.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// T3's IdAllocator.joinId encodes each component once with encodeURIComponent.
pub fn derived_id(prefix: &str, parts: &[&str]) -> String {
    let mut id = prefix.to_owned();
    for part in parts {
        id.push(':');
        for byte in part.bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
                )
            {
                id.push(char::from(byte));
            } else {
                use std::fmt::Write;
                write!(id, "%{byte:02X}").expect("String writes do not fail");
            }
        }
    }
    id
}

mod capabilities;
pub use capabilities::*;
pub mod projection;
pub mod shell;
mod state;
pub mod wire;
pub use state::{Change, State};
mod commands;
pub use commands::{
    Admission, Context, Decision, Input, admit, edit_queued, hold_queue, move_queued,
    promote_queued, remove_input, user_item_id,
};
mod rollback;
pub use rollback::{
    RollbackTarget, retained_rollback_boundary, rewind, rollback_completed, rollback_failed,
    rollback_target,
};
mod execution;
mod native_observation;
pub use native_observation::{
    observe_external_item, observe_external_plan, observe_external_subagent, observe_external_turn,
    observe_native_cursor,
};
mod forks;
pub mod handoffs;
pub use execution::{
    Completion, activate_queued, cancel_start, cancel_uncertain, complete, fail_start,
    finish_provider_turn, interrupt, native_resource_loaded, native_resource_unloaded,
    observe_item, observe_native_status, observe_plan, observe_subagent, observe_subagent_progress,
    started, uncertain_delivery,
};
pub use forks::{fork_native_thread, fork_thread};
mod generated_images;
mod recovery;
pub use generated_images::project_generated_images;
pub use recovery::{continue_restarted, provider_stopped, runtime_restarted};
mod checkpoints;
pub use checkpoints::{checkpoint_captured, prepare_checkpoint_scope};
mod preparation;
pub use preparation::{preparation_fail, preparation_release, retry_input, workspace_progress};
mod requests;
pub use requests::{
    RequestResponse, observe_external_request, observe_request, request_source_closed,
    resolve_request, respond, response_failed,
};
#[cfg(test)]
mod tests;

macro_rules! status {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

status!(RunStatus {
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
});
status!(RunAttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    Superseded,
});
status!(AttemptReason {
    Initial,
    SteeringRestart,
    Retry,
    ProviderRecovery
});
status!(ProviderSessionStatus {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error
});
// Bex also observes native work outside app runs. Unknown cannot certify that
// native work ended; NotLoaded only describes a released runtime resource.
status!(ProviderThreadStatus {
    Unknown,
    NotLoaded,
    Idle,
    Active,
    Archived,
    Closed,
    Error
});
status!(ProviderTurnStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled
});

impl ProviderTurnStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }
}
status!(HandoffStatus {
    Pending,
    Ready,
    Failed,
    Superseded
});
status!(HandoffStrategy {
    DeltaSinceTargetLastSeen,
    ForkDeltaSummary,
    FullThreadSummary,
    CheckpointSummary,
    ManualContext,
});
status!(HandoffDeliveryStatus {
    Pending,
    Injected,
    Inline
});
status!(TransferType {
    Fork,
    ProviderHandoff,
    MergeBack,
    SubagentSpawn,
    SubagentResult
});
status!(TransferStatus {
    Pending,
    ResolvedNative,
    ResolvedPortable,
    Failed,
    Consumed,
    Superseded,
});
status!(EffectStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled
});
status!(CheckpointStatus {
    Ready,
    Missing,
    Error,
    Stale
});
status!(TurnItemStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted
});
impl TurnItemStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}
status!(PlanStatus {
    Draft,
    Active,
    Completed,
    Superseded
});
status!(PlanKind {
    ProposedPlan,
    TodoList
});
status!(SubagentStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted
});
status!(RequestStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled
});
status!(Actor {
    User,
    Agent,
    System
});
status!(CreationSource {
    Web,
    Mobile,
    Mcp,
    Provider,
    Server
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ProviderOptionSelection>,
}

impl ModelSelection {
    pub fn text_option(&self, id: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|option| option.id == id)
            .and_then(|option| match &option.value {
                ProviderOptionSelectionValue::Text(value) => Some(value.as_str()),
                ProviderOptionSelectionValue::Boolean(_) => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOptionSelection {
    pub id: String,
    pub value: ProviderOptionSelectionValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionSelectionValue {
    Text(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: String,
    pub run_on_worktree_create: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_on_settle: Option<bool>,
    #[serde(rename = "async", skip_serializing_if = "Option::is_none")]
    pub asynchronous: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

status!(ThreadHistoryOrigin { Native, V1Import });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    pub parent_thread_id: Option<String>,
    pub relationship_to_parent: Option<String>,
    pub root_thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_chat: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<ThreadHistoryOrigin>,
    /// Bex native clients retain detailed setup/cancellation UI beyond T3's run card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_launch: Option<Box<crate::workspace::WorkspacePreparation>>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub project_id: Option<String>,
    pub title: String,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub active_provider_thread_id: Option<String>,
    pub lineage: Lineage,
    pub forked_from: Option<ThreadForkSource>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<String>,
    pub settled_at: Option<String>,
    pub rollback_request_id: Option<String>,
    pub rollback_failure: Option<RollbackFailure>,
    pub last_visited_at: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ThreadForkSource {
    Run {
        thread_id: String,
        run_id: String,
    },
    Node {
        node_id: String,
    },
    ProviderThread {
        provider_thread_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_turn_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub thread_id: String,
    pub ordinal: u64,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<String>,
    pub user_message_id: String,
    pub root_node_id: Option<String>,
    pub active_attempt_id: Option<String>,
    pub status: RunStatus,
    pub queue_position: Option<u64>,
    pub queue_held: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer_when_ready_run_id: Option<String>,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub checkpoint_id: Option<String>,
    pub context_handoff_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_limit_continuation_of_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_preparation: Option<WorkspacePreparation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkspacePreparation {
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

impl WorkspacePreparation {
    pub fn for_strategy(strategy: &crate::workspace::WorkspaceStrategy) -> Option<Self> {
        match strategy {
            crate::workspace::WorkspaceStrategy::Root => None,
            crate::workspace::WorkspaceStrategy::ExistingWorktree { path } => {
                Some(Self::ExistingWorktree {
                    worktree_path: path.clone(),
                    branch: None,
                })
            }
            crate::workspace::WorkspaceStrategy::Worktree {
                base_ref,
                start_from_origin,
            } => Some(Self::Worktree {
                base_ref: base_ref.clone(),
                start_from_origin: *start_from_origin,
                branch: None,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    // Recovery may replace the selected native source; this attempt keeps its binding.
    pub native_thread_ref: Option<ProviderRef>,
    pub run_id: String,
    pub attempt_ordinal: u64,
    pub root_node_id: String,
    pub provider_instance_id: String,
    pub provider_thread_id: String,
    pub provider_turn_id: Option<String>,
    pub reason: AttemptReason,
    pub status: RunAttemptStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Native references can reuse an identifier; the occurrence ordinal is part
/// of the reference, rather than a second private native-position column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub driver: String,
    pub native_id: Option<String>,
    pub strength: NativeRefStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
}

pub fn native_head_id<'a>(
    driver: &str,
    head: Option<&'a ProviderRef>,
) -> Result<Option<&'a str>, String> {
    let Some(head) = head else {
        return Ok(None);
    };
    if head.driver != driver {
        return Err("native conversation head belongs to another driver".into());
    }
    head.native_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(Some)
        .ok_or_else(|| "native conversation head has no identity".into())
}

/// Claude's raw user/control turn handle is not an SDK history position.
pub fn is_claude_assistant_cursor(id: &str) -> bool {
    !id.is_empty() && !id.starts_with("turn:")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: String,
    pub driver: String,
    pub provider_instance_id: String,
    pub status: ProviderSessionStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
    pub created_at: String,
    pub updated_at: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionDetached {
    pub provider_session_id: String,
    pub detached_at: String,
}

/// This is the persisted and replayed domain event. Projection-only details
/// such as body-fetch cursors are not a second causal synchronization protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_event_id: Option<String>,
    pub occurred_at: String,
    #[serde(flatten)]
    pub change: Change,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvent {
    pub sequence: u64,
    pub command_id: Option<String>,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadFork {
    pub provider_thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadNativeMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_store_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_identity_version: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_head_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: String,
    pub driver: String,
    pub provider_instance_id: String,
    pub provider_session_id: Option<String>,
    pub app_thread_id: Option<String>,
    pub owner_node_id: Option<String>,
    pub native_thread_ref: Option<ProviderRef>,
    pub native_conversation_head_ref: Option<ProviderRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_metadata: Option<ProviderThreadNativeMetadata>,
    pub status: ProviderThreadStatus,
    pub first_run_ordinal: Option<u64>,
    pub last_run_ordinal: Option<u64>,
    pub handoff_ids: Vec<String>,
    pub forked_from: Option<ProviderThreadFork>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: String,
    pub provider_thread_id: String,
    pub node_id: String,
    pub run_attempt_id: Option<String>,
    /// The physical source fences callbacks even after a logical thread is
    /// rebound, including native work with no application attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_thread_ref: Option<ProviderRef>,
    pub native_turn_ref: Option<ProviderRef>,
    /// Claude controls a query by the echoed user handle; native_turn_ref is
    /// its assistant history cursor. Other drivers use native_turn_ref for both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_control_ref: Option<ProviderRef>,
    pub ordinal: u64,
    pub status: ProviderTurnStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<ProviderTurnTokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurnTokenUsage {
    pub used_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<String>,
    pub updated_at: String,
}

status!(NodeKind {
    RootTurn,
    AssistantMessage,
    Reasoning,
    Plan,
    TodoList,
    ToolCall,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    Hook,
    System
});
status!(NodeStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub parent_node_id: Option<String>,
    pub root_node_id: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub native_item_ref: Option<ProviderRef>,
    pub runtime_request_id: Option<String>,
    pub checkpoint_scope_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    /// Transport-only: the full database row remains available through hydration.
    /// Ordinary T3 bodies have no omission marker; native clients also bound
    /// authored text that exceeds their binary RPC frame budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detail_omitted: bool,
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub native_item_ref: Option<ProviderRef>,
    pub parent_item_id: Option<String>,
    pub ordinal: u64,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
    #[serde(flatten)]
    pub body: TurnItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TurnItemBody {
    UserMessage {
        created_by: Actor,
        creation_source: CreationSource,
        message_id: String,
        input_intent: String,
        text: String,
        attachments: Vec<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    AssistantMessage {
        message_id: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Value>,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    WebSearch {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        patterns: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results: Vec<Value>,
    },
    Notification {
        source: Value,
        outcome: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    CommandExecution {
        input: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_omitted: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_indicates_failure: Option<bool>,
    },
    FileChange {
        file_name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        diff_str: Option<String>,
    },
    ProposedPlan {
        plan_id: String,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        plan_id: String,
        steps: Vec<PlanStep>,
        #[serde(skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
    Subagent {
        subagent_id: String,
        origin: String,
        driver: String,
        provider_instance_id: String,
        child_thread_id: Option<String>,
        prompt: String,
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        progress: Option<String>,
    },
    DynamicTool {
        tool_name: Option<String>,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_omitted: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        viewed_image_path: Option<String>,
    },
    UserInputRequest {
        request_id: String,
        questions: Vec<UserInputQuestion>,
    },
    ApprovalRequest {
        request_id: String,
        request_kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ApprovalOption>,
    },
    Checkpoint {
        checkpoint_id: String,
        scope_id: String,
        files: Vec<CheckpointFileSummary>,
        #[serde(skip_serializing_if = "Option::is_none")]
        files_total: Option<CheckpointFilesTotal>,
    },
    Handoff {
        context_handoff_id: String,
        from_provider_thread_ids: Vec<String>,
        to_provider_thread_id: String,
        from_provider_instance_ids: Vec<String>,
        to_provider_instance_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_model_selections: Option<Vec<ModelSelection>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_model: Option<String>,
        strategy: HandoffStrategy,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Error {
        failure: ProviderFailure,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry: Option<ProviderRetry>,
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
    Compaction {
        driver: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        before_token_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_token_count: Option<u64>,
    },
    Fork {
        source: ThreadForkSource,
        target_thread_id: String,
    },
}

impl TurnItemBody {
    pub fn has_omitted_detail(&self) -> bool {
        match self {
            Self::CommandExecution { output_omitted, .. } => *output_omitted == Some(true),
            Self::DynamicTool {
                output_omitted,
                viewed_image_path,
                ..
            } => viewed_image_path.is_none() && *output_omitted == Some(true),
            Self::FileChange { diff_str, .. } => diff_str.is_none(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: String,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetry {
    pub attempt: u64,
    pub max_attempts: Option<u64>,
    pub retry_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBackgroundTask {
    pub kind: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awaited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_anchor_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalOption {
    pub decision: ApprovalDecision,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
    // Bex native clients also honor the SDK's password-input hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: String,
    pub node_id: String,
    pub provider_turn_id: Option<String>,
    pub kind: String,
    pub native_request_ref: Option<ProviderRef>,
    pub status: RequestStatus,
    pub response_capability: ResponseCapability,
    pub created_at: String,
    pub resolved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ResponseCapability {
    Live {
        provider_session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        supports_feedback: Option<bool>,
    },
    Message,
    NotResumable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveredRunOrdinals {
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffDelivery {
    pub native_thread_id: String,
    pub status: HandoffDeliveryStatus,
    pub item_ids: Vec<String>,
    pub omitted_item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHandoff {
    pub id: String,
    pub transfer_id: Option<String>,
    pub thread_id: String,
    pub target_run_id: String,
    pub from_provider_thread_ids: Vec<String>,
    pub to_provider_thread_id: String,
    pub covered_run_ordinals: CoveredRunOrdinals,
    pub strategy: HandoffStrategy,
    pub status: HandoffStatus,
    pub summary_message_id: Option<String>,
    /// Summary text is owned by the handoff turn item. History entries name
    /// their source items; native injection reads those bodies at delivery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<HandoffHistory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<HandoffDelivery>,
    pub detail_in_turn_item: bool,
    pub created_by_provider_instance_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffHistory {
    pub messages: Vec<HistoricalMessage>,
    pub coverage: String,
    pub omitted_items: u64,
    pub omitted_item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub item_id: String,
    pub provider_thread_id: Option<String>,
    pub status: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourcePoint {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_thread_ref: Option<ProviderRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_turn_ref: Option<ProviderRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "strategy",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ContextTransferResolution {
    NativeFork { provider_thread_ref: ProviderRef },
    PortableContext { context_handoff_id: String },
    DeltaContext { context_handoff_id: String },
    ForkDeltaContext { context_handoff_id: String },
    CheckpointContext { context_handoff_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransfer {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: TransferType,
    pub source_thread_id: String,
    pub target_thread_id: String,
    pub source_point: ContextSourcePoint,
    pub base_point: Option<ContextSourcePoint>,
    pub source_provider_instance_id: Option<String>,
    pub target_provider_instance_id: Option<String>,
    pub target_run_id: Option<String>,
    pub status: TransferStatus,
    pub resolution: Option<ContextTransferResolution>,
    pub created_by: Actor,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub consumed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub id: String,
    pub command_id: String,
    pub thread_id: String,
    pub request: EffectRequest,
    pub status: EffectStatus,
    pub attempt_count: u64,
    pub available_at: String,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum EffectRequest {
    #[serde(rename = "provider-runtime.continue")]
    ProviderRuntimeContinue { source_run_id: String },
    #[serde(rename = "provider-session.detach")]
    ProviderSessionDetach {
        provider_session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename = "provider-session.apply-model-selection")]
    ProviderSessionApplyModelSelection {
        provider_session_id: String,
        provider_thread_id: String,
        model_selection: ModelSelection,
    },
    #[serde(rename = "provider-turn.start")]
    ProviderTurnStart { run_id: String },
    #[serde(rename = "provider-turn.interrupt")]
    ProviderTurnInterrupt {
        provider_session_id: String,
        provider_thread_id: String,
        provider_turn_id: String,
    },
    #[serde(rename = "provider-turn.steer")]
    ProviderTurnSteer {
        provider_session_id: String,
        provider_thread_id: String,
        provider_turn_id: String,
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_selection: Option<ModelSelection>,
    },
    #[serde(rename = "provider-turn.restart")]
    ProviderTurnRestart {
        provider_session_id: String,
        provider_thread_id: String,
        provider_turn_id: String,
        interrupted_attempt_id: String,
        run_id: String,
    },
    #[serde(rename = "runtime-request.respond")]
    RuntimeRequestRespond {
        provider_session_id: String,
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<ApprovalDecision>,
        #[serde(skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        answers: Option<BTreeMap<String, Value>>,
    },
    #[serde(rename = "provider-thread.rollback")]
    ProviderThreadRollback {
        provider_thread_id: String,
        checkpoint_id: String,
        scope_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        restore_files: Option<bool>,
    },
    #[serde(rename = "checkpoint.capture")]
    CheckpointCapture { run_id: String, scope_id: String },
}

/// Plan metadata refers to its node/item; its markdown or steps are stored
/// only on that turn item, as authorized for the T3 storage alignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: String,
    pub kind: PlanKind,
    pub status: PlanStatus,
    pub detail_in_turn_item: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub parent_node_id: String,
    pub origin: String,
    pub created_by: String,
    pub driver: String,
    pub provider_instance_id: String,
    pub provider_thread_id: Option<String>,
    pub child_thread_id: Option<String>,
    pub native_task_ref: Option<ProviderRef>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub status: SubagentStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointScope {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: String,
    pub parent_scope_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub kind: String,
    pub ordinal_within_parent: u64,
    pub advances_app_run_count: bool,
    pub cwd: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileSummary {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFilesTotal {
    pub count: usize,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub thread_id: String,
    pub scope_id: String,
    pub run_id: Option<String>,
    pub node_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub ordinal_within_scope: u64,
    pub app_run_ordinal: Option<u64>,
    #[serde(rename = "ref")]
    pub reference: String,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_total: Option<CheckpointFilesTotal>,
    pub captured_at: String,
}
