//! A rewind changes visible history only after the native resource confirms it.
//! CheckpointRollbackService owns the same boundary in the pinned T3 source.
use super::*;
use commands::{effect, hold_queue};

#[derive(Debug, Clone, PartialEq)]
pub struct RollbackTarget {
    pub ordinal: u64,
    pub retained_turn: Option<ProviderRef>,
    pub first_removed_turn: Option<ProviderRef>,
}

fn terminal(turn: &ProviderTurn) -> bool {
    matches!(
        turn.status,
        ProviderTurnStatus::Completed
            | ProviderTurnStatus::Interrupted
            | ProviderTurnStatus::Failed
            | ProviderTurnStatus::Cancelled
    )
}

/// Read actual checkpoint and provider-turn identities, never inferred native IDs.
pub fn rollback_target(
    state: &State,
    provider_thread_id: &str,
    checkpoint_id: &str,
    scope_id: &str,
) -> Result<RollbackTarget, String> {
    let thread = state
        .selected_native_thread()
        .filter(|thread| thread.id == provider_thread_id)
        .ok_or("active provider changed before rewind")?;
    if thread.status == ProviderThreadStatus::Active
        || !thread.pending_background_tasks.is_empty()
        || state.nodes.values().any(|node| {
            node.provider_thread_id.as_ref() == Some(&thread.id)
                && node.kind != NodeKind::RootTurn
                && matches!(
                    node.status,
                    NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                )
                && node
                    .run_id
                    .as_ref()
                    .and_then(|id| state.runs.get(id))
                    .is_none_or(|run| run.status != RunStatus::Queued)
        })
    {
        return Err("native work is still running".into());
    }
    let session = thread
        .provider_session_id
        .as_ref()
        .and_then(|id| state.provider_sessions.get(id))
        .ok_or("rewind provider session is unavailable")?;
    if session.driver != thread.driver
        || session.provider_instance_id != thread.provider_instance_id
        || state.thread.model_selection.instance_id != thread.provider_instance_id
    {
        return Err("rewind provider selection is inconsistent".into());
    }
    if !session.capabilities.can_rollback_conversation() {
        return Err("provider conversation rollback is unavailable".into());
    }
    let scope = state
        .checkpoint_scopes
        .get(scope_id)
        .filter(|scope| scope.thread_id == state.thread.id)
        .ok_or("rewind checkpoint scope is unavailable")?;
    let checkpoint = state
        .checkpoints
        .get(checkpoint_id)
        .filter(|checkpoint| {
            checkpoint.thread_id == state.thread.id
                && checkpoint.scope_id == scope.id
                && checkpoint.is_conversation_rewind_ready()
        })
        .ok_or("rewind checkpoint is unavailable")?;
    let ordinal = checkpoint.app_run_ordinal.unwrap_or(0);
    let retained = if ordinal == 0 {
        None
    } else {
        let run = state
            .runs
            .values()
            .find(|run| run.ordinal == ordinal && run.status != RunStatus::RolledBack)
            .ok_or("rewind retained run is unavailable")?;
        let attempt = run
            .active_attempt_id
            .as_ref()
            .and_then(|id| state.run_attempts.get(id));
        let latest = run.active_attempt_id.as_ref().and_then(|attempt| {
            state
                .provider_turns
                .values()
                .filter(|turn| turn.run_attempt_id.as_ref() == Some(attempt))
                .max_by_key(|turn| turn.ordinal)
        });
        let turn = latest.or_else(|| {
            attempt
                .and_then(|attempt| attempt.provider_turn_id.as_ref())
                .and_then(|id| state.provider_turns.get(id))
        });
        Some(retained_rollback_boundary(run, attempt, thread, turn)?)
    };
    let after = retained.as_ref().map_or(0, |(ordinal, _)| *ordinal);
    let first_removed = state
        .provider_turns
        .values()
        .filter(|turn| {
            turn.provider_thread_id == thread.id
                && turn.ordinal > after
                && terminal(turn)
                && turn
                    .run_attempt_id
                    .as_ref()
                    .and_then(|id| state.run_attempts.get(id))
                    .and_then(|attempt| state.runs.get(&attempt.run_id))
                    .is_none_or(|run| run.status != RunStatus::RolledBack)
        })
        .min_by_key(|turn| turn.ordinal);
    Ok(RollbackTarget {
        ordinal,
        retained_turn: retained.map(|(_, reference)| reference),
        first_removed_turn: first_removed
            .map(|turn| native_boundary(thread, turn))
            .transpose()?,
    })
}

/// The retained app run must name a real owned attempt and its latest native
/// turn. An external turn with no attempt never supplies a missing app cursor.
/// UI projections may index these values differently from the DB-owned State.
pub fn retained_rollback_boundary(
    run: &Run,
    attempt: Option<&RunAttempt>,
    thread: &ProviderThread,
    turn: Option<&ProviderTurn>,
) -> Result<(u64, ProviderRef), String> {
    let attempt = attempt
        .filter(|attempt| {
            Some(&attempt.id) == run.active_attempt_id.as_ref()
                && attempt.run_id == run.id
                && attempt.provider_thread_id == thread.id
        })
        .ok_or("rewind retained attempt is unavailable")?;
    let turn = turn
        .filter(|turn| {
            turn.provider_thread_id == thread.id
                && terminal(turn)
                && (turn.run_attempt_id.as_ref() == Some(&attempt.id)
                    || (turn.run_attempt_id.is_none()
                        && attempt.provider_turn_id.as_ref() == Some(&turn.id)))
        })
        .ok_or("rewind retained provider turn is unavailable")?;
    Ok((turn.ordinal, native_boundary(thread, turn)?))
}

impl Checkpoint {
    pub fn is_conversation_rewind_ready(&self) -> bool {
        matches!(
            self.status,
            CheckpointStatus::Ready | CheckpointStatus::Missing
        ) && (self.app_run_ordinal.unwrap_or(0) != 0 || self.ordinal_within_scope == 0)
    }
}

fn native_boundary(thread: &ProviderThread, turn: &ProviderTurn) -> Result<ProviderRef, String> {
    if turn.native_thread_ref != thread.native_thread_ref {
        return Err("rewind turn belongs to another physical native thread".into());
    }
    turn.native_turn_ref
        .as_ref()
        .filter(|reference| {
            reference.driver == thread.driver
                && reference
                    .native_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .ok_or_else(|| "rewind native turn reference is unavailable".into())
}

pub fn rewind(state: &State, run_id: &str, context: Context<'_>) -> Result<Decision, String> {
    if state.active_run().is_some()
        || state.has_uncertain_input()
        || state.has_pending_rollback()
        || state
            .runtime_requests
            .values()
            .any(|request| request.status == RequestStatus::Pending)
    {
        return Err("conversation is busy".into());
    }
    let run = state
        .runs
        .get(run_id)
        .filter(|run| !matches!(run.status, RunStatus::Queued | RunStatus::RolledBack))
        .ok_or("run is unavailable")?;
    let scope_id = run
        .root_node_id
        .as_ref()
        .and_then(|id| state.nodes.get(id))
        .and_then(|node| node.checkpoint_scope_id.as_ref())
        .ok_or("rewind checkpoint scope is unavailable")?;
    let retained_ordinal = state
        .runs
        .values()
        .filter(|candidate| {
            candidate.ordinal < run.ordinal
                && !matches!(candidate.status, RunStatus::Queued | RunStatus::RolledBack)
        })
        .map(|run| run.ordinal)
        .max()
        .unwrap_or(0);
    let checkpoint = state
        .checkpoints
        .values()
        .find(|checkpoint| {
            checkpoint.scope_id == *scope_id
                && checkpoint.app_run_ordinal.unwrap_or(0) == retained_ordinal
                && checkpoint.is_conversation_rewind_ready()
        })
        .ok_or("rewind checkpoint is unavailable")?;
    let provider = state
        .selected_native_thread()
        .ok_or("rewind provider thread is unavailable")?;
    rollback_target(state, &provider.id, &checkpoint.id, scope_id)?;
    let mut changes = hold_queue(state, true).changes;
    changes.push(Change::Thread(Thread {
        rollback_request_id: Some(context.command_id.into()),
        rollback_failure: None,
        updated_at: context.now.into(),
        ..state.thread.clone()
    }));
    Ok(Decision {
        changes,
        effects: vec![effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderThreadRollback {
                provider_thread_id: provider.id.clone(),
                checkpoint_id: checkpoint.id.clone(),
                scope_id: scope_id.clone(),
                restore_files: Some(false),
            },
        )],
    })
}

/// The I/O owner supplies the actual returned native identity and cursor. Audit
/// records remain addressable; only this successful result cuts visible history.
pub fn rollback_completed(
    state: &State,
    effect_id: &str,
    native_thread: ProviderRef,
    native_head: Option<ProviderRef>,
    context: Context<'_>,
) -> Result<Decision, String> {
    let action = state
        .effects
        .get(effect_id)
        .ok_or("rewind effect is unavailable")?;
    let EffectRequest::ProviderThreadRollback {
        provider_thread_id,
        checkpoint_id,
        scope_id,
        ..
    } = &action.request
    else {
        return Err("effect is not a rewind".into());
    };
    if state.thread.rollback_request_id.as_deref() != Some(action.command_id.as_str()) {
        return Err("rollback request is no longer current".into());
    }
    if native_head.as_ref().is_some_and(|head| {
        head.driver != native_thread.driver
            || head.native_id.as_ref().is_none_or(|id| id.is_empty())
    }) {
        return Err("rewind returned an invalid native cursor".into());
    }
    let target = rollback_target(state, provider_thread_id, checkpoint_id, scope_id)?;
    let provider = &state.provider_threads[provider_thread_id];
    if native_thread.driver != provider.driver
        || native_thread
            .native_id
            .as_ref()
            .is_none_or(|id| id.is_empty())
    {
        return Err("rewind returned an invalid native identity".into());
    }
    let mut decision = Decision {
        changes: vec![Change::ProviderThread(ProviderThread {
            native_thread_ref: Some(native_thread),
            native_conversation_head_ref: native_head,
            status: ProviderThreadStatus::Idle,
            last_run_ordinal: (target.ordinal != 0).then_some(target.ordinal),
            updated_at: context.now.into(),
            ..provider.clone()
        })],
        effects: vec![Effect {
            status: EffectStatus::Succeeded,
            lease_owner: None,
            lease_expires_at: None,
            completed_at: Some(context.now.into()),
            updated_at: context.now.into(),
            last_error: None,
            ..action.clone()
        }],
    };
    for run in state.runs.values().filter(|run| {
        run.ordinal > target.ordinal
            && !matches!(run.status, RunStatus::Queued | RunStatus::RolledBack)
    }) {
        decision.changes.push(Change::Run(Run {
            status: RunStatus::RolledBack,
            completed_at: Some(context.now.into()),
            ..run.clone()
        }));
        if let Some(root) = run.root_node_id.as_ref().and_then(|id| state.nodes.get(id)) {
            decision.changes.push(Change::Node(ExecutionNode {
                status: NodeStatus::RolledBack,
                completed_at: Some(context.now.into()),
                ..root.clone()
            }));
        }
    }
    decision.changes.extend(
        state
            .checkpoints
            .values()
            .filter(|checkpoint| {
                checkpoint.scope_id == *scope_id
                    && checkpoint.ordinal_within_scope > target.ordinal
                    && checkpoint.status != CheckpointStatus::Stale
            })
            .map(|checkpoint| {
                Change::Checkpoint(Checkpoint {
                    status: CheckpointStatus::Stale,
                    ..checkpoint.clone()
                })
            }),
    );
    let projected = state.apply(&decision);
    if let Some(next) = projected.next_queued_run() {
        let activated = activate_queued(&projected, &next.id, context)?;
        decision.changes.extend(activated.changes);
        decision.effects.extend(activated.effects);
    }
    Ok(decision)
}

/// Both worker exhaustion and provider exit publish through this request fence.
pub(super) fn failure_metadata(
    thread: &Thread,
    request_id: &str,
    message: &str,
    now: &str,
) -> Option<Change> {
    if thread.deleted_at.is_some() || thread.rollback_request_id.as_deref() != Some(request_id) {
        return None;
    }
    Some(Change::Thread(Thread {
        rollback_failure: Some(RollbackFailure {
            request_id: request_id.into(),
            message: message.into(),
        }),
        updated_at: now.into(),
        ..thread.clone()
    }))
}

pub fn rollback_failed(state: &State, effect_id: &str, message: &str, now: &str) -> Decision {
    let Some(effect) = state.effects.get(effect_id).filter(|effect| {
        matches!(effect.request, EffectRequest::ProviderThreadRollback { .. })
            && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
    }) else {
        return Decision::default();
    };
    Decision {
        changes: failure_metadata(&state.thread, &effect.command_id, message, now)
            .into_iter()
            .collect(),
        effects: vec![Effect {
            status: EffectStatus::Failed,
            lease_owner: None,
            lease_expires_at: None,
            completed_at: Some(now.into()),
            updated_at: now.into(),
            last_error: Some(message.into()),
            ..effect.clone()
        }],
    }
}
