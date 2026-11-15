//! Provider capability records from T3 Code 1a12c4af.
use super::NativeRefStrength;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeEnforcement {
    Native,
    #[default]
    ClientBoundary,
}

impl Default for NativeRefStrength {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    pub supports_multiple_provider_threads_per_session: bool,
    pub supports_model_switch_in_session: bool,
    pub supports_provider_switching_via_handoff: bool,
    pub supports_runtime_mode_switch_in_session: bool,
    pub pending_requests_survive_restart: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCapabilities {
    pub can_create_empty_thread: bool,
    pub can_read_thread_snapshot: bool,
    pub can_rollback_thread: bool,
    pub can_fork_thread: bool,
    pub can_fork_from_turn: bool,
    pub can_fork_from_subagent_thread: bool,
    pub exposes_native_thread_id: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_native_goals: Option<bool>,
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_steering_interrupts_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_steering_accepts_attachments: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_steering_during_compaction: Option<bool>,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: NativeRefStrength,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingCapabilities {
    pub streams_assistant_text: bool,
    pub streams_reasoning: bool,
    pub streams_tool_output: bool,
    pub streams_plan_text: bool,
    pub emits_message_completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    pub exposes_tool_item_ids: bool,
    pub emits_tool_started: bool,
    pub emits_tool_completed: bool,
    pub emits_tool_output: bool,
    pub supports_mcp_tools: bool,
    pub supports_dynamic_tool_callbacks: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCapabilities {
    pub supports_command_approval: bool,
    pub supports_file_read_approval: bool,
    pub supports_file_change_approval: bool,
    pub supports_apply_patch_approval: bool,
    pub approvals_have_native_request_ids: bool,
    pub approval_callbacks_are_live_only: bool,
    pub approvals_can_originate_from_subagents: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCapabilities {
    pub emits_plan_updated: bool,
    pub emits_todo_list: bool,
    pub emits_proposed_plan: bool,
    pub supports_structured_questions: bool,
    pub plan_deltas_have_item_ids: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCapabilities {
    pub supports_subagents: bool,
    pub exposes_subagent_thread_ids: bool,
    pub emits_subagent_lifecycle: bool,
    pub can_wait_for_subagents: bool,
    pub can_close_subagents: bool,
    pub can_fork_subagent_thread: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    pub max_recommended_handoff_chars: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointCapabilities {
    pub app_can_checkpoint_filesystem: bool,
    pub supports_nested_checkpoint_scopes: bool,
    pub provider_can_rollback_conversation: bool,
    pub provider_rollback_returns_snapshot: bool,
    pub provider_can_read_conversation_snapshot: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicyCapabilities {
    pub enforcement: RuntimeEnforcement,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapabilities {
    pub native_thread_ids: NativeRefStrength,
    pub native_turn_ids: NativeRefStrength,
    pub native_item_ids: NativeRefStrength,
    pub native_request_ids: NativeRefStrength,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub runtime_policy: RuntimePolicyCapabilities,
}

impl ProviderCapabilities {
    pub fn can_rollback_conversation(&self) -> bool {
        self.threads.can_rollback_thread
            && self.checkpointing.provider_can_rollback_conversation
            && self.checkpointing.provider_rollback_returns_snapshot
    }
}
