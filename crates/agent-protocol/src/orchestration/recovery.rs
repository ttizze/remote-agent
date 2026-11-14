//! Provider and server process recovery, independent of native I/O.
use super::execution::finish_run;
use super::*;

fn restart_blocked(state: &State, run: &Run) -> bool {
    state.thread.archived_at.is_some()
        || state.thread.deleted_at.is_some()
        || state.thread.settled_override.as_deref() == Some("settled")
        || state.thread.lineage.parent_thread_id.is_some()
        || state.thread.provider_instance_id != run.provider_instance_id
        || state.runtime_requests.values().any(|request| {
            request.status == RequestStatus::Pending
                && !matches!(request.kind.as_str(), "dynamic_tool_call" | "auth_refresh")
                && !matches!(request.response_capability, ResponseCapability::Message)
        })
        || state.turn_items.values().any(|item| {
            item.run_id.as_ref() == Some(&run.id)
                && (matches!(item.body, TurnItemBody::RunInterruptRequest {..})
                    || matches!(&item.body, TurnItemBody::UserMessage {text, attachments,..} if attachments.is_empty() && ["/compact", "/logout"].iter().any(|command| text.trim().eq_ignore_ascii_case(command))))
        })
}

fn restart_source(state: &State) -> Option<&Run> {
    let run = state.active_run().filter(|run| {
        run.status == RunStatus::Running
            || run.status == RunStatus::Starting && run.restart_continuation_of_run_id.is_some()
    })?;
    let prepared = run.status == RunStatus::Starting;
    if restart_blocked(state, run) {
        return None;
    }
    let thread = state
        .provider_threads
        .get(run.provider_thread_id.as_ref()?)?;
    let session = state
        .provider_sessions
        .get(thread.provider_session_id.as_ref()?)?;
    (thread.owner_node_id.is_none()
        && thread.app_thread_id.as_ref() == Some(&state.thread.id)
        && thread.provider_instance_id == run.provider_instance_id
        && (prepared || thread.status == ProviderThreadStatus::Active)
        && !matches!(
            thread.status,
            ProviderThreadStatus::Closed | ProviderThreadStatus::Archived
        )
        && thread.native_thread_ref.as_ref().is_some_and(|native| {
            native.native_id.is_some()
                && native.strength == NativeRefStrength::Strong
                && native.driver == thread.driver
        })
        && session.provider_instance_id == run.provider_instance_id
        && session.driver == thread.driver
        && (prepared
            || !matches!(
                session.status,
                ProviderSessionStatus::Stopped | ProviderSessionStatus::Error
            ))
        && (prepared
            || state.provider_turns.values().any(|turn| {
                turn.run_attempt_id == run.active_attempt_id
                    && turn.provider_thread_id == thread.id
                    && turn.status == ProviderTurnStatus::Running
            })))
    .then_some(run)
}

/// Only accepted native work is eligible for a restart continuation. A native
/// write whose acknowledgement is unknown stays fenced for explicit recovery.
pub fn runtime_restarted(
    state: &State,
    continue_after_restart: bool,
    context: Context<'_>,
) -> Result<Decision, String> {
    let continuation = (continue_after_restart && !state.has_uncertain_input())
        .then(|| restart_source(state))
        .flatten();
    let mut decision = if let Some(run) = state.active_run()
        && !state.has_uncertain_input()
        && let Some(attempt) = state
            .active_attempt(run.active_attempt_id.as_deref())
            .filter(|attempt| {
                matches!(
                    attempt.status,
                    RunAttemptStatus::Pending | RunAttemptStatus::Running
                )
            }) {
        finish_run(state, &attempt.id, Completion::Cancelled, context)?
    } else {
        Decision::default()
    };
    decision.changes.extend(
        state
            .provider_sessions
            .values()
            .filter(|session| {
                !matches!(
                    session.status,
                    ProviderSessionStatus::Stopped | ProviderSessionStatus::Error
                )
            })
            .map(|session| {
                Change::ProviderSession(ProviderSession {
                    status: ProviderSessionStatus::Stopped,
                    updated_at: context.now.into(),
                    last_error: None,
                    ..session.clone()
                })
            }),
    );
    decision.changes.extend(
        state
            .runtime_requests
            .values()
            .filter(|request| {
                request.status == RequestStatus::Pending
                    && !matches!(request.response_capability, ResponseCapability::Message)
            })
            .map(|request| {
                Change::RuntimeRequest(RuntimeRequest {
                    status: RequestStatus::Expired,
                    response_capability: ResponseCapability::NotResumable {
                        reason: "The server restarted before this runtime request was resolved."
                            .into(),
                    },
                    resolved_at: Some(context.now.into()),
                    ..request.clone()
                })
            }),
    );
    if let Some(source) = continuation {
        let mut pending = commands::effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderRuntimeContinue {
                source_run_id: source.id.clone(),
            },
        );
        pending.id = format!("effect:restart-continuation:{}", source.id);
        decision.effects.push(pending);
    }
    Ok(decision)
}

/// Admission and its receipt are committed by the writer. Repeated effects
/// and stale continuations therefore cannot replace or repeat newer work.
pub fn continue_restarted(
    state: &State,
    source_run_id: &str,
    text: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let source = state
        .runs
        .get(source_run_id)
        .ok_or("restart source is missing")?;
    let message_id = format!("message:restart-continuation:{source_run_id}");
    if source.status != RunStatus::Cancelled
        || restart_blocked(state, source)
        || state.runs.values().any(|run| {
            run.user_message_id == message_id
                || run.ordinal > source.ordinal
                    && run.status != RunStatus::Queued
                    && !(run.status == RunStatus::Cancelled && run.started_at.is_none())
        })
    {
        return Ok(Decision::default());
    }
    let cutoff = chrono::DateTime::parse_from_rfc3339(
        source
            .completed_at
            .as_deref()
            .unwrap_or(&source.requested_at),
    )
    .map_err(|error| error.to_string())?
        - chrono::Duration::minutes(5);
    let mut current = source;
    let mut chain = 0;
    while let Some(parent) = &current.restart_continuation_of_run_id {
        if chrono::DateTime::parse_from_rfc3339(&current.requested_at)
            .map_err(|error| error.to_string())?
            < cutoff
        {
            break;
        }
        chain += 1;
        if chain >= 3 {
            return Ok(Decision::default());
        }
        let Some(parent) = state.runs.get(parent) else {
            break;
        };
        current = parent;
    }
    let provider_thread = source
        .provider_thread_id
        .as_ref()
        .and_then(|id| state.provider_threads.get(id))
        .ok_or("restart native thread is missing")?;
    let mut decision = admit(
        state,
        Input {
            message_id: message_id.clone(),
            created_by: Actor::Agent,
            creation_source: CreationSource::Server,
            text: text.into(),
            attachments: Vec::new(),
            context: None,
            model_selection: source.model_selection.clone(),
            driver: provider_thread.driver.clone(),
            workspace_preparation: None,
            fork_origin: None,
        },
        Admission::Queue,
        context,
    )?;
    for change in &mut decision.changes {
        if let Change::RunCreated(run) = change
            && run.user_message_id == message_id
        {
            run.restart_continuation_of_run_id = Some(source.id.clone());
            run.work_started_at = source
                .work_started_at
                .clone()
                .or_else(|| source.started_at.clone());
        }
    }
    Ok(decision)
}

/// A dead provider settles only its own work. This is a process observation,
/// so it can release an uncertain native write without replaying that input.
pub fn provider_stopped(
    state: &State,
    provider_instance_id: &str,
    failure: &ProviderFailure,
    context: Context<'_>,
) -> Result<Decision, String> {
    let mut decision = if let Some(run) = state
        .active_run()
        .filter(|run| run.provider_instance_id == provider_instance_id)
        && let Some(attempt) = state
            .active_attempt(run.active_attempt_id.as_deref())
            .filter(|attempt| {
                matches!(
                    attempt.status,
                    RunAttemptStatus::Pending | RunAttemptStatus::Running
                )
            }) {
        finish_run(
            state,
            &attempt.id,
            Completion::Failed(failure.clone()),
            context,
        )?
    } else {
        Decision::default()
    };
    for change in &mut decision.changes {
        if let Change::ProviderThread(thread) = change {
            thread.status = ProviderThreadStatus::Error;
        }
    }
    decision.changes.extend(
        state
            .provider_sessions
            .values()
            .filter(|session| {
                session.provider_instance_id == provider_instance_id
                    && !matches!(
                        session.status,
                        ProviderSessionStatus::Stopped | ProviderSessionStatus::Error
                    )
            })
            .map(|session| {
                Change::ProviderSession(ProviderSession {
                    status: ProviderSessionStatus::Error,
                    last_error: Some(failure.message.clone()),
                    updated_at: context.now.into(),
                    ..session.clone()
                })
            }),
    );
    decision.effects.extend(
        state
            .effects
            .values()
            .filter(|effect| {
                if !matches!(effect.status, EffectStatus::Pending | EffectStatus::Running) {
                    return false;
                }
                match &effect.request {
                    EffectRequest::ProviderTurnStart { run_id }
                    | EffectRequest::ProviderRuntimeContinue {
                        source_run_id: run_id,
                    } => state
                        .runs
                        .get(run_id)
                        .is_some_and(|run| run.provider_instance_id == provider_instance_id),
                    EffectRequest::ProviderSessionDetach {
                        provider_session_id,
                        ..
                    }
                    | EffectRequest::ProviderSessionApplyModelSelection {
                        provider_session_id,
                        ..
                    }
                    | EffectRequest::ProviderTurnInterrupt {
                        provider_session_id,
                        ..
                    }
                    | EffectRequest::ProviderTurnSteer {
                        provider_session_id,
                        ..
                    }
                    | EffectRequest::ProviderTurnRestart {
                        provider_session_id,
                        ..
                    }
                    | EffectRequest::RuntimeRequestRespond {
                        provider_session_id,
                        ..
                    } => state
                        .provider_sessions
                        .get(provider_session_id)
                        .is_some_and(|session| {
                            session.provider_instance_id == provider_instance_id
                        }),
                    EffectRequest::ProviderThreadRollback {
                        provider_thread_id, ..
                    } => state
                        .provider_threads
                        .get(provider_thread_id)
                        .is_some_and(|thread| thread.provider_instance_id == provider_instance_id),
                    EffectRequest::CheckpointCapture { .. } => false,
                }
            })
            .map(|effect| Effect {
                status: EffectStatus::Failed,
                lease_owner: None,
                lease_expires_at: None,
                last_error: Some(failure.message.clone()),
                updated_at: context.now.into(),
                completed_at: Some(context.now.into()),
                ..effect.clone()
            }),
    );
    Ok(decision)
}
