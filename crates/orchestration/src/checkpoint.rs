//! Checkpoint finalization is an explicit, durable step before queue promotion.
use crate::*;

pub fn await_capture(events: Vec<DomainEvent>, scopes: &[CheckpointScope]) -> Decision {
    let mut decision = Decision {
        events,
        ..Decision::default()
    };
    for event in &mut decision.events {
        let EventPayload::RunUpdated(run) = &mut event.payload else {
            continue;
        };
        if run.checkpoint_id.is_some()
            || !matches!(
                run.status,
                RunStatus::Completed | RunStatus::Interrupted | RunStatus::Cancelled
            )
            || !scopes.iter().any(|scope| {
                scope.thread_id == run.thread_id
                    && scope.kind == ScopeKind::RootRun
                    && scope.run_id.as_ref() == Some(&run.id)
                    && run.root_node_id.as_ref() == Some(&scope.node_id)
            })
        {
            continue;
        }
        if run.status == RunStatus::Completed {
            run.status = RunStatus::Waiting;
            run.completed_at = None;
        }
        decision.effects.push(Effect {
            id: format!(
                "checkpoint:capture:{}:{}",
                run.id,
                run.active_attempt_id.as_ref().map_or("", |id| id.as_str())
            ),
            thread_id: run.thread_id.clone(),
            body: EffectBody::CaptureCheckpoint {
                run_id: run.id.clone(),
            },
        });
    }
    decision
}

pub fn capture_finalization(
    events: &[DomainEvent],
    runs: &[Run],
    nodes: &[ExecutionNode],
    now: &Timestamp,
) -> Vec<DomainEvent> {
    let mut finalized = vec![];
    for event in events {
        let EventPayload::CheckpointCaptured(checkpoint) = &event.payload else {
            continue;
        };
        let Some(run_id) = &checkpoint.run_id else {
            continue;
        };
        let event_id = &event.id;
        let Some(run) = runs
            .iter()
            .find(|run| &run.id == run_id && run.status != RunStatus::RolledBack)
        else {
            continue;
        };
        let mut run = run.clone();
        let waiting = run.status == RunStatus::Waiting;
        run.checkpoint_id = Some(checkpoint.id.clone());
        if waiting {
            run.status = RunStatus::Completed;
            run.completed_at = Some(now.clone());
        }
        finalized.push(DomainEvent {
            id: EventId::new(format!("{event_id}:run")).expect("derived id"),
            thread_id: run.thread_id.clone(),
            occurred_at: now.clone(),
            payload: EventPayload::RunUpdated(run.clone()),
        });
        let node = nodes.iter().find(|node| node.id == checkpoint.node_id);
        if let Some(node) = node {
            let mut node = node.clone();
            node.checkpoint_scope_id = Some(checkpoint.scope_id.clone());
            if waiting {
                node.status = NodeStatus::Completed;
                node.completed_at = Some(now.clone());
            }
            finalized.push(DomainEvent {
                id: EventId::new(format!("{event_id}:node")).expect("derived id"),
                thread_id: checkpoint.thread_id.clone(),
                occurred_at: now.clone(),
                payload: EventPayload::NodeUpdated(node),
            });
        }
        finalized.push(DomainEvent {
            id: EventId::new(format!("{event_id}:item")).expect("derived id"),
            thread_id: run.thread_id.clone(),
            occurred_at: now.clone(),
            payload: EventPayload::TurnItemUpdated(TurnItem {
                id: TurnItemId::new(format!("item:{}", checkpoint.id)).expect("derived id"),
                thread_id: run.thread_id,
                run_id: Some(run.id),
                node_id: Some(checkpoint.node_id.clone()),
                provider_thread_id: run.provider_thread_id,
                provider_turn_id: node.and_then(|node| node.provider_turn_id.clone()),
                native_item_ref: None,
                parent_item_id: None,
                ordinal: 0,
                status: ItemStatus::Completed,
                title: None,
                started_at: Some(now.clone()),
                completed_at: Some(now.clone()),
                updated_at: now.clone(),
                body: TurnItemBody::Checkpoint {
                    checkpoint_id: checkpoint.id.clone(),
                    scope_id: checkpoint.scope_id.clone(),
                    files: checkpoint.files.clone(),
                },
            }),
        });
    }
    finalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn finalization_preserves_stopped_status_and_completes_waiting(
            status in prop::sample::select(vec![RunStatus::Waiting, RunStatus::Interrupted, RunStatus::Cancelled]),
            capture_status in prop::sample::select(vec![CheckpointStatus::Ready, CheckpointStatus::Missing, CheckpointStatus::Error]),
        ) {
            let mut projection = running();
            let run = &mut projection.runs[0];
            run.status = status;
            let checkpoint = checkpoint(run, capture_status);
            let mut events = vec![DomainEvent { id: EventId::new("capture").unwrap(),
                thread_id: run.thread_id.clone(), occurred_at: now(), payload: EventPayload::CheckpointCaptured(checkpoint.clone()) }];
            events.extend(capture_finalization(&events, &projection.runs, &projection.nodes, &now()));
            let updated = events.iter().find_map(|event| match &event.payload { EventPayload::RunUpdated(run) => Some(run), _ => None }).unwrap();
            prop_assert_eq!(updated.status, if status == RunStatus::Waiting { RunStatus::Completed } else { status });
            prop_assert_eq!(updated.checkpoint_id.as_ref(), Some(&checkpoint.id));
            prop_assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::TurnItemUpdated(item) if matches!(item.body, TurnItemBody::Checkpoint { .. }))), "checkpoint summary is projected");
        }
    }
    #[test]
    fn only_scoped_finished_or_stopped_runs_schedule_capture() {
        for status in [
            RunStatus::Running,
            RunStatus::Completed,
            RunStatus::Interrupted,
            RunStatus::Cancelled,
            RunStatus::Failed,
        ] {
            let mut run = running().runs.remove(0);
            run.status = status;
            run.completed_at = Some(now());
            let event = DomainEvent {
                id: EventId::new("finish").unwrap(),
                thread_id: run.thread_id.clone(),
                occurred_at: now(),
                payload: EventPayload::RunUpdated(run.clone()),
            };
            assert!(await_capture(vec![event.clone()], &[]).effects.is_empty());
            let mut other_run = checkpoint_scope(&run);
            other_run.run_id = Some(RunId::new("queued-run").unwrap());
            assert!(
                await_capture(vec![event.clone()], &[other_run])
                    .effects
                    .is_empty()
            );
            let decision = await_capture(vec![event], &[checkpoint_scope(&run)]);
            assert_eq!(
                decision.effects.len(),
                usize::from(matches!(
                    status,
                    RunStatus::Completed | RunStatus::Interrupted | RunStatus::Cancelled
                ))
            );
            if status == RunStatus::Completed {
                assert!(
                    matches!(&decision.events[0].payload, EventPayload::RunUpdated(run) if run.status == RunStatus::Waiting && run.completed_at.is_none())
                );
            }
        }
    }
}
