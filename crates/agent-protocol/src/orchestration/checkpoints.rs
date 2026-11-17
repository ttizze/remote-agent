use super::*;
use commands::{effect, id};

/// The resource policy supplies the resolved path, before the provider write.
/// The root scope is stable across runs, matching T3's root checkpoint scope.
pub fn prepare_checkpoint_scope(
    state: &State,
    run_id: &str,
    cwd: &str,
    now: &str,
) -> Result<Decision, String> {
    let run = state.runs.get(run_id).ok_or("run does not exist")?;
    let root = run
        .root_node_id
        .as_ref()
        .and_then(|id| state.nodes.get(id))
        .ok_or("run root is missing")?;
    if cwd.is_empty() {
        return Err("checkpoint workspace path is empty".into());
    }
    let scope = CheckpointScope {
        id: id(&state.thread.id, "checkpoint-scope:root"),
        thread_id: state.thread.id.clone(),
        run_id: Some(run.id.clone()),
        node_id: root.id.clone(),
        parent_scope_id: None,
        provider_thread_id: run.provider_thread_id.clone(),
        kind: "root_run".into(),
        ordinal_within_parent: 0,
        advances_app_run_count: true,
        cwd: cwd.into(),
        created_at: now.into(),
    };
    Ok(Decision {
        changes: vec![
            Change::Node(ExecutionNode {
                checkpoint_scope_id: Some(scope.id.clone()),
                ..root.clone()
            }),
            Change::CheckpointScope(scope),
        ],
        ..Default::default()
    })
}

pub(super) fn capture_effect(
    context: Context<'_>,
    thread_id: &str,
    run_id: &str,
    scope_id: &str,
) -> Effect {
    let mut capture = effect(
        context,
        thread_id,
        EffectRequest::CheckpointCapture {
            run_id: run_id.into(),
            scope_id: scope_id.into(),
        },
    );
    capture.id = format!("effect:checkpoint.capture:{run_id}");
    capture.command_id = format!("command:effect:checkpoint.capture:{run_id}");
    capture
}

/// Native completion waits for the checkpoint effect. A stopped run keeps its
/// terminal status; a rollback that won the lane makes a late capture a no-op.
pub fn checkpoint_captured(
    state: &State,
    run_id: &str,
    checkpoint: Checkpoint,
    context: Context<'_>,
) -> Result<Decision, String> {
    let run = state.runs.get(run_id).ok_or("run does not exist")?;
    if run.status == RunStatus::RolledBack || run.checkpoint_id.is_some() {
        return Ok(Decision::default());
    }
    if !matches!(
        run.status,
        RunStatus::Waiting | RunStatus::Interrupted | RunStatus::Cancelled
    ) {
        return Err("run is not awaiting checkpoint capture".into());
    }
    let root = run
        .root_node_id
        .as_ref()
        .and_then(|id| state.nodes.get(id))
        .ok_or("run root is missing")?;
    if checkpoint.thread_id != state.thread.id
        || checkpoint.run_id.as_deref() != Some(run_id)
        || checkpoint.node_id != root.id
        || root.checkpoint_scope_id.as_ref() != Some(&checkpoint.scope_id)
        || checkpoint.ordinal_within_scope != run.ordinal
        || checkpoint.app_run_ordinal != Some(run.ordinal)
    {
        return Err("checkpoint does not belong to this run and scope".into());
    }
    let stopped = run.status != RunStatus::Waiting;
    let mut changes = vec![
        Change::Checkpoint(checkpoint.clone()),
        Change::TurnItem(TurnItem {
            detail_omitted: false,
            id: id(&checkpoint.id, "turn-item"),
            thread_id: state.thread.id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root.id.clone()),
            provider_thread_id: root.provider_thread_id.clone(),
            provider_turn_id: root.provider_turn_id.clone(),
            native_item_ref: None,
            parent_item_id: None,
            ordinal: 0,
            status: TurnItemStatus::Completed,
            title: None,
            started_at: Some(context.now.into()),
            completed_at: Some(context.now.into()),
            updated_at: context.now.into(),
            body: TurnItemBody::Checkpoint {
                checkpoint_id: checkpoint.id.clone(),
                scope_id: checkpoint.scope_id.clone(),
                files: checkpoint.files.clone(),
                files_total: checkpoint.files_total.clone(),
            },
        }),
        Change::Run(Run {
            checkpoint_id: Some(checkpoint.id),
            status: if stopped {
                run.status
            } else {
                RunStatus::Completed
            },
            completed_at: if stopped {
                run.completed_at.clone()
            } else {
                Some(context.now.into())
            },
            ..run.clone()
        }),
    ];
    if !stopped {
        changes.push(Change::Node(ExecutionNode {
            status: NodeStatus::Completed,
            completed_at: Some(context.now.into()),
            ..root.clone()
        }));
    }
    let mut effects = Vec::new();
    if !stopped {
        let projected = state.apply(&Decision {
            changes: changes.clone(),
            effects: Vec::new(),
        });
        if !projected.native_work_blocks_delivery()
            && let Some(next) = projected.next_queued_run()
        {
            let activation = activate_queued(&projected, &next.id, context)?;
            changes.extend(activation.changes);
            effects = activation.effects;
        }
    }
    Ok(Decision { changes, effects })
}
