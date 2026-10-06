//! Capability records from the pinned adapters.
use crate::contracts::*;

pub fn capabilities(driver: Driver) -> ProviderCapabilities {
    let mut value = ProviderCapabilities {
        sessions: SessionCapabilities {
            supports_multiple_provider_threads_per_session: true,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: true,
            pending_requests_survive_restart: false,
        },
        threads: ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: true,
            can_rollback_thread: true,
            can_fork_thread: true,
            can_fork_from_turn: true,
            can_fork_from_subagent_thread: true,
            exposes_native_thread_id: true,
        },
        turns: TurnCapabilities {
            exposes_native_turn_id: true,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_active_steering: true,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: Strength::Strong,
        },
        streaming: StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: true,
            streams_plan_text: true,
            emits_message_completed: true,
        },
        tools: ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: true,
            supports_dynamic_tool_callbacks: true,
        },
        approvals: ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: true,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: true,
        },
        planning: PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: true,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: true,
        },
        subagents: SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: true,
            emits_subagent_lifecycle: true,
            can_wait_for_subagents: true,
            can_close_subagents: true,
            can_fork_subagent_thread: true,
        },
        context: ContextCapabilities {
            accepts_system_context: true,
            accepts_developer_context: true,
            accepts_synthetic_user_context: true,
            can_generate_summaries: true,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
        checkpointing: CheckpointCapabilities {
            app_can_checkpoint_filesystem: true,
            supports_nested_checkpoint_scopes: true,
            provider_can_rollback_conversation: true,
            provider_rollback_returns_snapshot: true,
            provider_can_read_conversation_snapshot: true,
        },
        identity: IdentityCapabilities {
            native_thread_ids: Strength::Strong,
            native_turn_ids: Strength::Strong,
            native_item_ids: Strength::Strong,
            native_request_ids: Strength::Strong,
        },
        runtime_policy: Enforcement::Native,
    };
    if driver == Driver::Claude {
        value
            .sessions
            .supports_multiple_provider_threads_per_session = false;
        value.sessions.supports_runtime_mode_switch_in_session = false;
        value.threads.can_read_thread_snapshot = false;
        value.threads.can_fork_from_subagent_thread = false;
        value.turns.exposes_native_turn_id = false;
        value.turns.supports_steering_by_interrupt_restart = false;
        value.streaming.streams_tool_output = false;
        value.streaming.streams_plan_text = false;
        value.approvals.supports_apply_patch_approval = false;
        value.approvals.approvals_can_originate_from_subagents = false;
        value.planning.plan_deltas_have_item_ids = false;
        value.subagents.exposes_subagent_thread_ids = false;
        value.subagents.can_wait_for_subagents = false;
        value.subagents.can_close_subagents = false;
        value.subagents.can_fork_subagent_thread = false;
        value.checkpointing.provider_can_read_conversation_snapshot = false;
        value.identity.native_turn_ids = Strength::Weak;
    }
    value
}

/// The supported provider instance identifiers are shared by admission and UI.
pub fn driver(instance: &ProviderInstanceId) -> Option<Driver> {
    match instance.as_str() {
        "codex" => Some(Driver::Codex),
        "claude" => Some(Driver::Claude),
        _ => None,
    }
}
