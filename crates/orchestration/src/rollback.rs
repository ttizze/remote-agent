//! Rewind planning and finalization; provider and filesystem I/O belong to Host.
use crate::{decider::DecisionError, *};

pub fn target<'a>(
    projection: &'a ThreadProjection,
    scope_id: &CheckpointScopeId,
    checkpoint_id: &CheckpointId,
) -> Result<
    (
        &'a CheckpointScope,
        &'a Checkpoint,
        &'a ProviderThread,
        Option<&'a ProviderTurn>,
    ),
    DecisionError,
> {
    let checkpoint = projection
        .checkpoints
        .iter()
        .find(|c| {
            c.id == *checkpoint_id && c.scope_id == *scope_id && c.status == CheckpointStatus::Ready
        })
        .ok_or_else(|| DecisionError("checkpoint is unavailable".into()))?;
    let scope = projection
        .checkpoint_scopes
        .iter()
        .find(|s| s.id == *scope_id)
        .ok_or_else(|| DecisionError("checkpoint scope is unavailable".into()))?;
    let thread = projection
        .provider_threads
        .iter()
        .find(|t| Some(&t.id) == projection.thread.active_provider_thread_id.as_ref())
        .filter(|t| t.provider_instance_id == projection.thread.provider_instance_id)
        .ok_or_else(|| DecisionError("active provider thread is unavailable".into()))?;
    let session = projection
        .provider_sessions
        .iter()
        .find(|s| Some(&s.id) == thread.provider_session_id.as_ref())
        .ok_or_else(|| DecisionError("provider session is unavailable".into()))?;
    if !session.capabilities.threads.can_rollback_thread
        || projection.runs.iter().any(|r| r.status.is_blocking())
    {
        return Err(DecisionError(
            "provider cannot roll back while a run is active".into(),
        ));
    }
    let turn = if let Some(ordinal) = checkpoint.app_run_ordinal.filter(|n| *n > 0) {
        let run = projection
            .runs
            .iter()
            .find(|r| r.ordinal == ordinal && r.status != RunStatus::RolledBack)
            .ok_or_else(|| DecisionError("checkpoint run is unavailable".into()))?;
        let attempt = projection
            .attempts
            .iter()
            .find(|a| Some(&a.id) == run.active_attempt_id.as_ref());
        Some(
            projection
                .provider_turns
                .iter()
                .find(|t| {
                    t.provider_thread_id == thread.id
                        && attempt.is_some_and(|a| {
                            a.provider_turn_id.as_ref() == Some(&t.id)
                                || t.run_attempt_id.as_ref() == Some(&a.id)
                        })
                })
                .ok_or_else(|| {
                    DecisionError("checkpoint belongs to another provider conversation".into())
                })?,
        )
    } else if scope.kind != ScopeKind::RootRun {
        projection
            .nodes
            .iter()
            .find(|n| n.id == checkpoint.node_id)
            .and_then(|n| n.provider_turn_id.as_ref())
            .and_then(|id| {
                projection
                    .provider_turns
                    .iter()
                    .find(|t| &t.id == id && t.provider_thread_id == thread.id)
            })
    } else {
        None
    };
    Ok((scope, checkpoint, thread, turn))
}

pub fn request(
    command: &Command,
    projection: &ThreadProjection,
    scope_id: &CheckpointScopeId,
    checkpoint_id: &CheckpointId,
    restore_files: bool,
    now: &Timestamp,
) -> Result<Decision, DecisionError> {
    if projection.thread.rollback_request_id.is_some() {
        return Err(DecisionError("rollback is already in progress".into()));
    }
    let (_, _, provider, _) = target(projection, scope_id, checkpoint_id)?;
    let mut thread = projection.thread.clone();
    thread.rollback_request_id = Some(command.command_id.clone());
    thread.rollback_failure = None;
    thread.updated_at = now.clone();
    Ok(Decision {
        events: events(
            &command.thread_id,
            command.command_id.as_str(),
            vec![
                EventPayload::ThreadMetadataUpdated(thread),
                EventPayload::CheckpointRollbackRequested(CheckpointRollbackRequest {
                    scope_id: scope_id.clone(),
                    checkpoint_id: checkpoint_id.clone(),
                    requested_at: now.clone(),
                }),
            ],
            now,
        ),
        cancel_unsettled_effects: false,
        effects: vec![Effect {
            id: format!("effect:{}:rollback", command.command_id),
            thread_id: command.thread_id.clone(),
            body: EffectBody::Rollback {
                request_id: command.command_id.clone(),
                provider_thread_id: provider.id.clone(),
                checkpoint_id: checkpoint_id.clone(),
                scope_id: scope_id.clone(),
                restore_files,
            },
        }],
    })
}

pub fn stale_checkpoints(projection: &ThreadProjection, target: &Checkpoint) -> Vec<Checkpoint> {
    let ordinal = target.app_run_ordinal.unwrap_or(0);
    let removed: std::collections::BTreeSet<_> = projection
        .runs
        .iter()
        .filter(|r| {
            r.ordinal > ordinal && r.status.is_terminal() && r.status != RunStatus::RolledBack
        })
        .map(|r| &r.id)
        .collect();
    projection
        .checkpoints
        .iter()
        .filter(|c| {
            c.id != target.id
                && c.scope_id == target.scope_id
                && c.status == CheckpointStatus::Ready
                && (c.app_run_ordinal.is_some_and(|n| n > ordinal)
                    || c.run_id.as_ref().is_some_and(|id| removed.contains(id)))
        })
        .cloned()
        .collect()
}

pub fn finish(
    projection: &ThreadProjection,
    checkpoint: &Checkpoint,
    provider: ProviderThread,
    request_id: &CommandId,
    now: &Timestamp,
) -> Vec<DomainEvent> {
    let ordinal = checkpoint.app_run_ordinal.unwrap_or(0);
    let mut payloads = vec![EventPayload::ProviderThreadUpdated(provider)];
    for other in projection.provider_threads.iter().filter(|p| {
        Some(&p.id) != projection.thread.active_provider_thread_id.as_ref()
            && p.last_run_ordinal.is_some_and(|n| n > ordinal)
    }) {
        let mut other = other.clone();
        other.native_thread_ref = None;
        other.native_conversation_head_ref = None;
        other.last_run_ordinal = None;
        other.status = ProviderThreadStatus::Closed;
        other.updated_at = now.clone();
        payloads.push(EventPayload::ProviderThreadUpdated(other));
    }
    for run in &projection.runs {
        if run.ordinal > ordinal && run.status.is_terminal() && run.status != RunStatus::RolledBack
        {
            let mut run = run.clone();
            run.status = RunStatus::RolledBack;
            let mut nodes = projection
                .nodes
                .iter()
                .filter(|n| n.run_id.as_ref() == Some(&run.id))
                .cloned()
                .collect::<Vec<_>>();
            payloads.push(EventPayload::RunUpdated(run));
            for node in &mut nodes {
                node.status = NodeStatus::RolledBack;
                node.completed_at = Some(now.clone());
                payloads.push(EventPayload::NodeUpdated(node.clone()));
            }
        }
    }
    for mut stale in stale_checkpoints(projection, checkpoint) {
        stale.status = CheckpointStatus::Stale;
        payloads.push(EventPayload::CheckpointCaptured(stale));
    }
    let mut thread = projection.thread.clone();
    thread.rollback_request_id = None;
    thread.rollback_failure = None;
    thread.updated_at = now.clone();
    payloads.push(EventPayload::ThreadMetadataUpdated(thread));
    events(
        &projection.thread.id,
        &format!("rollback:{request_id}"),
        payloads,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn concurrent_rollback_is_rejected_before_replacing_the_request() {
        let mut projection = projection();
        projection.thread.rollback_request_id = Some(CommandId::new("first").unwrap());
        let command = command("second", CommandBody::QueueResume);
        let error = request(
            &command,
            &projection,
            &CheckpointScopeId::new("scope").unwrap(),
            &CheckpointId::new("checkpoint").unwrap(),
            false,
            &now(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("already in progress"));
        assert_eq!(
            projection
                .thread
                .rollback_request_id
                .as_ref()
                .unwrap()
                .as_str(),
            "first"
        );
    }

    #[test]
    fn rewind_preserves_audit_records_and_hides_only_later_completed_work() {
        let mut projection = running();
        let provider = projection.provider_threads[0].clone();
        projection.runs[0].status = RunStatus::Completed;
        projection.nodes[0].status = NodeStatus::Completed;
        let checkpoint = checkpoint(&projection.runs[0], CheckpointStatus::Ready);
        let mut baseline = checkpoint.clone();
        baseline.id = CheckpointId::new("baseline").unwrap();
        baseline.app_run_ordinal = Some(0);
        baseline.ordinal_within_scope = 0;
        projection.checkpoints.push(checkpoint);
        let events = finish(
            &projection,
            &baseline,
            provider,
            &CommandId::new("rewind").unwrap(),
            &now(),
        );
        let mut next = projection.clone();
        for event in &events {
            next = projector::apply(Some(&next), event, projector::ProjectionOptions::default())
                .unwrap();
        }
        assert_eq!(next.runs[0].status, RunStatus::RolledBack);
        assert_eq!(next.nodes[0].status, NodeStatus::RolledBack);
        assert_eq!(next.checkpoints[0].status, CheckpointStatus::Stale);
        assert!(!next.turn_items.is_empty());
        assert!(next.visible_turn_items.is_empty());
        assert_eq!(projection.runs[0].status, RunStatus::Completed);
        for event in &events {
            next = projector::apply(Some(&next), event, projector::ProjectionOptions::default())
                .unwrap();
        }
        assert_eq!(next.runs.len(), 1);
    }

    #[test]
    fn rewind_invalidates_other_provider_native_history_without_activating_it() {
        let mut projection = running();
        projection.runs[0].status = RunStatus::Completed;
        let active = projection.provider_threads[0].clone();
        projection.thread.active_provider_thread_id = Some(active.id.clone());
        let mut other = active.clone();
        other.id = ProviderThreadId::new("other-provider").unwrap();
        other.last_run_ordinal = Some(1);
        other.native_thread_ref = Some(ProviderRef {
            driver: Driver::Claude,
            native_id: Some("stale-native".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        other.native_conversation_head_ref = other.native_thread_ref.clone();
        projection.provider_threads.push(other.clone());
        let mut baseline = checkpoint(&projection.runs[0], CheckpointStatus::Ready);
        baseline.app_run_ordinal = Some(0);
        for event in finish(
            &projection.clone(),
            &baseline,
            active.clone(),
            &CommandId::new("rewind").unwrap(),
            &now(),
        ) {
            projection = projector::apply(Some(&projection), &event, Default::default()).unwrap();
        }
        let invalidated = projection
            .provider_threads
            .iter()
            .find(|p| p.id == other.id)
            .unwrap();
        assert!(invalidated.native_thread_ref.is_none());
        assert!(invalidated.native_conversation_head_ref.is_none());
        assert_eq!(invalidated.status, ProviderThreadStatus::Closed);
        assert_eq!(projection.thread.active_provider_thread_id, Some(active.id));
    }
    #[test]
    fn missing_stale_or_foreign_scope_is_rejected_before_provider_work() {
        let mut projection = running();
        let checkpoint = checkpoint(&projection.runs[0], CheckpointStatus::Ready);
        projection
            .checkpoint_scopes
            .push(checkpoint_scope(&projection.runs[0]));
        projection.checkpoints.push(checkpoint.clone());
        assert!(
            target(
                &projection,
                &CheckpointScopeId::new("other").unwrap(),
                &checkpoint.id
            )
            .is_err()
        );
        projection.checkpoints[0].status = CheckpointStatus::Stale;
        assert!(target(&projection, &checkpoint.scope_id, &checkpoint.id).is_err());
    }
}
