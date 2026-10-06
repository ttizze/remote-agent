use crate::contracts::*;

#[derive(Debug, Clone, PartialEq)]
pub enum MessageDispatchDecision {
    StartRun {
        model_selection: ModelSelection,
    },
    SteerActive {
        target_run_id: RunId,
        provider_turn_id: ProviderTurnId,
    },
    RestartActive {
        target_run_id: RunId,
        interrupt_provider_turn_id: ProviderTurnId,
    },
    QueueAfterActive {
        active_run_id: RunId,
    },
    SwitchProvider {
        from_provider_instance_id: ProviderInstanceId,
        to_model_selection: ModelSelection,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteeringExecutionPolicy {
    ActiveSteering,
    InterruptRestart,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkExecutionPolicy {
    NativeFork,
    PortableContext,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    QueuedMessages,
    ActiveSteering,
    Interrupt,
    InterruptRestartSteering,
    NativeFork,
    ForkFromTurn,
    Rollback,
    RollbackSnapshot,
    ContextHandoff,
    StrongTerminalStatus,
}
impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueuedMessages => "queued_messages",
            Self::ActiveSteering => "active_steering",
            Self::Interrupt => "interrupt",
            Self::InterruptRestartSteering => "interrupt_restart_steering",
            Self::NativeFork => "native_fork",
            Self::ForkFromTurn => "fork_from_turn",
            Self::Rollback => "rollback",
            Self::RollbackSnapshot => "rollback_snapshot",
            Self::ContextHandoff => "context_handoff",
            Self::StrongTerminalStatus => "strong_terminal_status",
        }
    }
}
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandPolicyError {
    #[error("Failed to choose message dispatch policy for command {command_id}.")]
    MessageDispatch {
        command_id: CommandId,
        thread_id: ThreadId,
        cause: Option<String>,
    },
    #[error(
        "{provider_instance_id} cannot satisfy message dispatch mode {requested_mode} for command {command_id}."
    )]
    Unsupported {
        command_id: CommandId,
        thread_id: ThreadId,
        requested_mode: &'static str,
        provider_instance_id: ProviderInstanceId,
    },
    #[error("{provider_instance_id} cannot satisfy {capability_name} for command {command_id}: {detail}", capability_name = capability.as_str())]
    CapabilityUnsupported {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_instance_id: ProviderInstanceId,
        capability: Capability,
        detail: &'static str,
    },
}
pub struct CapabilityCheck<'a> {
    pub command_id: &'a CommandId,
    pub thread_id: &'a ThreadId,
    pub provider_instance_id: &'a ProviderInstanceId,
    pub capabilities: &'a ProviderCapabilities,
}
fn unsupported(
    input: &CapabilityCheck<'_>,
    capability: Capability,
    detail: &'static str,
) -> CommandPolicyError {
    CommandPolicyError::CapabilityUnsupported {
        command_id: input.command_id.clone(),
        thread_id: input.thread_id.clone(),
        provider_instance_id: input.provider_instance_id.clone(),
        capability,
        detail,
    }
}

/// CommandPolicy.ts: resolveMessageDispatchIntent (findLast, not ordinal sorting).
pub fn resolve_message_dispatch_intent(
    projection: &ThreadProjection,
    requested: &DispatchMode,
    delivery_intent: Option<DeliveryIntent>,
) -> DispatchMode {
    let Some(intent) = delivery_intent else {
        return requested.clone();
    };
    let Some(active_run) = projection.runs.iter().rfind(|run| {
        matches!(
            run.status,
            RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting
        )
    }) else {
        return DispatchMode::StartImmediately;
    };
    if intent == DeliveryIntent::Steer {
        return DispatchMode::SteerActive {
            target_run_id: active_run.id.clone(),
        };
    }
    if intent == DeliveryIntent::Restart {
        return DispatchMode::RestartActive {
            target_run_id: active_run.id.clone(),
        };
    }
    if matches!(
        active_run.status,
        RunStatus::Preparing | RunStatus::Starting
    ) {
        return DispatchMode::QueueAfterActive;
    }
    let thread = projection
        .provider_threads
        .iter()
        .find(|p| Some(&p.id) == active_run.provider_thread_id.as_ref());
    let session = thread
        .and_then(|p| p.provider_session_id.as_ref())
        .and_then(|id| projection.provider_sessions.iter().find(|p| &p.id == id));
    let turns = session.map(|s| &s.capabilities.turns);
    if turns.is_some_and(|t| t.supports_active_steering) {
        return DispatchMode::SteerActive {
            target_run_id: active_run.id.clone(),
        };
    }
    if turns.is_some_and(|t| t.supports_queued_messages) {
        return DispatchMode::QueueAfterActive;
    }
    if turns.is_some_and(|t| t.supports_steering_by_interrupt_restart) {
        return DispatchMode::RestartActive {
            target_run_id: active_run.id.clone(),
        };
    }
    DispatchMode::QueueAfterActive
}

pub fn ensure_queued_messages(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if input.capabilities.turns.supports_queued_messages {
        Ok(())
    } else {
        Err(unsupported(
            input,
            Capability::QueuedMessages,
            "providerInstanceId does not support app-owned queued turns",
        ))
    }
}
pub fn decide_steering_execution(
    input: &CapabilityCheck<'_>,
    force_restart: bool,
) -> Result<SteeringExecutionPolicy, CommandPolicyError> {
    if !force_restart && input.capabilities.turns.supports_active_steering {
        return Ok(SteeringExecutionPolicy::ActiveSteering);
    }
    if input.capabilities.turns.supports_interrupt
        && input
            .capabilities
            .turns
            .supports_steering_by_interrupt_restart
    {
        return Ok(SteeringExecutionPolicy::InterruptRestart);
    }
    Err(unsupported(
        input,
        if force_restart || input.capabilities.turns.supports_interrupt {
            Capability::InterruptRestartSteering
        } else {
            Capability::ActiveSteering
        },
        if force_restart {
            "providerInstanceId cannot satisfy a required interrupt-and-restart"
        } else {
            "providerInstanceId cannot steer active turns directly or by interrupt-and-restart"
        },
    ))
}
pub fn ensure_interrupt(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if input.capabilities.turns.supports_interrupt {
        Ok(())
    } else {
        Err(unsupported(
            input,
            Capability::Interrupt,
            "providerInstanceId does not support turn interrupts",
        ))
    }
}
pub fn ensure_native_fork(
    input: &CapabilityCheck<'_>,
    from_specific_turn: bool,
) -> Result<(), CommandPolicyError> {
    if !input.capabilities.threads.can_fork_thread {
        return Err(unsupported(
            input,
            Capability::NativeFork,
            "providerInstanceId does not support native thread forks",
        ));
    }
    if from_specific_turn && !input.capabilities.threads.can_fork_from_turn {
        return Err(unsupported(
            input,
            Capability::ForkFromTurn,
            "providerInstanceId cannot fork from a specific completed turn",
        ));
    }
    if input.capabilities.identity.native_thread_ids != Strength::Strong {
        return Err(unsupported(
            input,
            Capability::NativeFork,
            "providerInstanceId does not expose strong native thread ids",
        ));
    }
    Ok(())
}
pub fn ensure_rollback(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if !input.capabilities.threads.can_rollback_thread
        || !input
            .capabilities
            .checkpointing
            .provider_can_rollback_conversation
    {
        return Err(unsupported(
            input,
            Capability::Rollback,
            "providerInstanceId conversation rollback is unavailable",
        ));
    }
    if !input
        .capabilities
        .checkpointing
        .provider_rollback_returns_snapshot
    {
        return Err(unsupported(
            input,
            Capability::RollbackSnapshot,
            "rollback must return a providerInstanceId thread snapshot",
        ));
    }
    Ok(())
}
#[derive(Debug, Clone, Copy)]
pub enum ContextStrategy {
    ForkDeltaContext,
    DeltaContext,
    FullThreadSummary,
}
pub fn ensure_context_handoff(
    input: &CapabilityCheck<'_>,
    strategy: ContextStrategy,
) -> Result<(), CommandPolicyError> {
    let context = &input.capabilities.context;
    if !context.can_consume_handoff_summaries {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId cannot consume handoff summaries",
        ));
    }
    if !context.accepts_synthetic_user_context {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId cannot receive synthetic user context",
        ));
    }
    if matches!(
        strategy,
        ContextStrategy::ForkDeltaContext | ContextStrategy::DeltaContext
    ) && !context.supports_delta_handoff
    {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId does not support delta handoff",
        ));
    }
    if matches!(strategy, ContextStrategy::FullThreadSummary)
        && !context.supports_full_thread_handoff
    {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId does not support full-thread handoff",
        ));
    }
    Ok(())
}
pub fn decide_fork_execution(
    input: &CapabilityCheck<'_>,
    same_provider: bool,
    has_strong_native_source: bool,
    source_run_status: RunStatus,
    from_specific_turn: bool,
) -> Result<ForkExecutionPolicy, CommandPolicyError> {
    let c = input.capabilities;
    if matches!(source_run_status, RunStatus::Completed | RunStatus::Waiting)
        && same_provider
        && has_strong_native_source
        && c.threads.can_fork_thread
        && (!from_specific_turn || c.threads.can_fork_from_turn)
        && c.identity.native_thread_ids == Strength::Strong
    {
        return Ok(ForkExecutionPolicy::NativeFork);
    }
    ensure_context_handoff(input, ContextStrategy::FullThreadSummary)?;
    Ok(ForkExecutionPolicy::PortableContext)
}
/// The source's decideMessageDispatch input excludes the preflight-only defer_start mode.
pub enum DispatchPolicyMode<'a> {
    SteerActive(&'a RunId),
    RestartActive(&'a RunId),
    QueueAfterActive,
    StartImmediately,
}
pub fn decide_message_dispatch(
    command_id: &CommandId,
    projection: &ThreadProjection,
    requested_model_selection: Option<&ModelSelection>,
    requested_mode: DispatchPolicyMode<'_>,
    capabilities: &ProviderCapabilities,
) -> Result<MessageDispatchDecision, CommandPolicyError> {
    let active_run = projection.runs.iter().find(|run| {
        matches!(
            run.status,
            RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting
        )
    });
    let model_selection = requested_model_selection.unwrap_or(&projection.thread.model_selection);
    let mode_name = match requested_mode {
        DispatchPolicyMode::SteerActive(_) => "steer_active",
        DispatchPolicyMode::RestartActive(_) => "restart_active",
        DispatchPolicyMode::QueueAfterActive => "queue_after_active",
        DispatchPolicyMode::StartImmediately => "start_immediately",
    };
    let unsupported_mode = || CommandPolicyError::Unsupported {
        command_id: command_id.clone(),
        thread_id: projection.thread.id.clone(),
        requested_mode: mode_name,
        provider_instance_id: model_selection.instance_id.clone(),
    };
    match requested_mode {
        DispatchPolicyMode::SteerActive(target) | DispatchPolicyMode::RestartActive(target) => {
            let restart = matches!(requested_mode, DispatchPolicyMode::RestartActive(_));
            if active_run
                .is_none_or(|run| run.id != *target || (restart && run.active_attempt_id.is_none()))
            {
                return Err(unsupported_mode());
            }
            let run = active_run.expect("checked active run");
            let provider_turn = run.active_attempt_id.as_ref().and_then(|id| {
                projection.provider_turns.iter().find(|turn| {
                    turn.run_attempt_id.as_ref() == Some(id) && turn.status == TurnStatus::Running
                })
            });
            let Some(turn) = provider_turn else {
                return Err(CommandPolicyError::MessageDispatch {
                    command_id: command_id.clone(),
                    thread_id: projection.thread.id.clone(),
                    cause: Some(format!(
                        "No running providerInstanceId turn found for active run {}.",
                        run.id
                    )),
                });
            };
            Ok(if restart {
                MessageDispatchDecision::RestartActive {
                    target_run_id: run.id.clone(),
                    interrupt_provider_turn_id: turn.id.clone(),
                }
            } else {
                MessageDispatchDecision::SteerActive {
                    target_run_id: run.id.clone(),
                    provider_turn_id: turn.id.clone(),
                }
            })
        }
        DispatchPolicyMode::QueueAfterActive | DispatchPolicyMode::StartImmediately => {
            if let Some(run) = active_run {
                ensure_queued_messages(&CapabilityCheck {
                    command_id,
                    thread_id: &projection.thread.id,
                    provider_instance_id: &model_selection.instance_id,
                    capabilities,
                })?;
                Ok(MessageDispatchDecision::QueueAfterActive {
                    active_run_id: run.id.clone(),
                })
            } else {
                Ok(MessageDispatchDecision::StartRun {
                    model_selection: model_selection.clone(),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "command_policy_tests.rs"]
mod tests;
