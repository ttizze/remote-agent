use super::*;
use commands::{effect, id};

const INPUT: &str = "Preparing workspace";
const FAILURE_CODE: &str = "workspace_preparation_failed";

pub(super) fn item(run: &Run, root: &ExecutionNode, now: &str) -> TurnItem {
    TurnItem {
        id: id(&run.id, "workspace-preparation"),
        thread_id: root.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(root.id.clone()),
        provider_thread_id: root.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: TurnItemStatus::Running,
        title: Some(INPUT.into()),
        started_at: Some(now.into()),
        completed_at: None,
        updated_at: now.into(),
        body: TurnItemBody::CommandExecution {
            input: INPUT.into(),
            output: None,
            exit_code: None,
            output_omitted: None,
            output_indicates_failure: None,
        },
    }
}

fn prepared<'a>(
    state: &'a State,
    run_id: &str,
    status: RunStatus,
) -> Result<(&'a Run, &'a RunAttempt, &'a TurnItem), String> {
    let run = state
        .runs
        .get(run_id)
        .filter(|run| run.status == status && run.workspace_preparation.is_some())
        .ok_or("run is not awaiting workspace preparation")?;
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .ok_or("run has no attempt")?;
    let item = state
        .turn_items
        .values()
        .find(|item| {
            item.run_id.as_deref() == Some(run_id)
                && matches!(&item.body,TurnItemBody::CommandExecution {input,..} if input==INPUT)
        })
        .ok_or("workspace preparation item is missing")?;
    Ok((run, attempt, item))
}

#[derive(Debug, Clone, Copy)]
pub enum PreparationPhase {
    Worktree,
    SetupScript,
}

pub fn preparation_progress(
    state: &State,
    run_id: &str,
    phase: PreparationPhase,
    now: &str,
) -> Result<Decision, String> {
    let (_, _, item) = prepared(state, run_id, RunStatus::Preparing)?;
    Ok(Decision {
        changes: vec![Change::TurnItem(TurnItem {
            title: Some(
                match phase {
                    PreparationPhase::Worktree => "Preparing worktree",
                    PreparationPhase::SetupScript => "Starting setup script",
                }
                .into(),
            ),
            updated_at: now.into(),
            ..item.clone()
        })],
        ..Default::default()
    })
}

pub fn preparation_release(
    state: &State,
    run_id: &str,
    cwd: &str,
    branch: Option<&str>,
    context: Context<'_>,
) -> Result<Decision, String> {
    let (run, _, item) = prepared(state, run_id, RunStatus::Preparing)?;
    if cwd.is_empty() {
        return Err("prepared workspace path is empty".into());
    }
    let mut decision = Decision {
        changes: vec![
            Change::Thread(Thread {
                worktree_path: Some(cwd.into()),
                branch: branch.map(str::to_owned),
                updated_at: context.now.into(),
                ..state.thread.clone()
            }),
            Change::TurnItem(TurnItem {
                status: TurnItemStatus::Completed,
                title: Some("Workspace ready".into()),
                completed_at: Some(context.now.into()),
                updated_at: context.now.into(),
                body: TurnItemBody::CommandExecution {
                    input: INPUT.into(),
                    output: Some("Workspace preparation completed.".into()),
                    exit_code: Some(0),
                    output_omitted: None,
                    output_indicates_failure: None,
                },
                ..item.clone()
            }),
            Change::Run(Run {
                status: RunStatus::Starting,
                ..run.clone()
            }),
        ],
        effects: vec![effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderTurnStart {
                run_id: run.id.clone(),
            },
        )],
    };
    decision
        .changes
        .extend(prepare_checkpoint_scope(state, run_id, cwd, context.now)?.changes);
    Ok(decision)
}

pub fn preparation_fail(
    state: &State,
    run_id: &str,
    mut failure: ProviderFailure,
    context: Context<'_>,
) -> Result<Decision, String> {
    let (run, attempt, item) = prepared(state, run_id, RunStatus::Preparing)?;
    let root = state
        .nodes
        .get(&attempt.root_node_id)
        .ok_or("run root is missing")?;
    failure.code = Some(FAILURE_CODE.into());
    let failed = TurnItem {
        status: TurnItemStatus::Failed,
        title: Some("Workspace preparation failed".into()),
        completed_at: Some(context.now.into()),
        updated_at: context.now.into(),
        ..item.clone()
    };
    let mut changes = vec![
        Change::RunAttempt(RunAttempt {
            status: RunAttemptStatus::Failed,
            completed_at: Some(context.now.into()),
            ..attempt.clone()
        }),
        Change::Node(ExecutionNode {
            status: NodeStatus::Failed,
            completed_at: Some(context.now.into()),
            ..root.clone()
        }),
        Change::TurnItem(TurnItem {
            body: TurnItemBody::CommandExecution {
                input: INPUT.into(),
                output: Some(failure.message.clone()),
                exit_code: Some(1),
                output_omitted: None,
                output_indicates_failure: None,
            },
            ..failed.clone()
        }),
        Change::TurnItem(TurnItem {
            id: id(&run.id, "workspace-preparation-failure"),
            body: TurnItemBody::Error {
                failure,
                retry: None,
            },
            ..failed
        }),
        Change::Run(Run {
            status: RunStatus::Failed,
            completed_at: Some(context.now.into()),
            ..run.clone()
        }),
    ];
    changes.extend(hold_queue(state, true).changes);
    Ok(Decision {
        changes,
        ..Default::default()
    })
}

pub fn preparation_retry(state: &State, run_id: &str, now: &str) -> Result<Decision, String> {
    let (run, attempt, item) = prepared(state, run_id, RunStatus::Failed)?;
    if state.thread.archived_at.is_some()
        || state.thread.deleted_at.is_some()
        || state.active_run().is_some()
    {
        return Err("thread cannot retry workspace preparation".into());
    }
    let failure=state.turn_items.values().find(|item|item.run_id.as_deref()==Some(run_id)
        && item.status==TurnItemStatus::Failed && matches!(&item.body,TurnItemBody::Error {failure,..} if failure.code.as_deref()==Some(FAILURE_CODE)))
        .ok_or("run has no failed workspace preparation to retry")?;
    let root = state
        .nodes
        .get(&attempt.root_node_id)
        .ok_or("run root is missing")?;
    let mut changes = vec![
        Change::TurnItem(TurnItem {
            status: TurnItemStatus::Cancelled,
            updated_at: now.into(),
            ..failure.clone()
        }),
        Change::TurnItem(TurnItem {
            status: TurnItemStatus::Running,
            title: Some(INPUT.into()),
            completed_at: None,
            updated_at: now.into(),
            body: TurnItemBody::CommandExecution {
                input: INPUT.into(),
                output: None,
                exit_code: None,
                output_omitted: None,
                output_indicates_failure: None,
            },
            ..item.clone()
        }),
        Change::RunAttempt(RunAttempt {
            status: RunAttemptStatus::Pending,
            completed_at: None,
            ..attempt.clone()
        }),
        Change::Node(ExecutionNode {
            status: NodeStatus::Pending,
            completed_at: None,
            ..root.clone()
        }),
        Change::Run(Run {
            status: RunStatus::Preparing,
            completed_at: None,
            ..run.clone()
        }),
    ];
    if state
        .runs
        .values()
        .filter(|run| run.status != RunStatus::Queued)
        .max_by_key(|run| run.ordinal)
        .is_some_and(|latest| latest.id == run_id)
    {
        changes.extend(hold_queue(state, false).changes);
    }
    Ok(Decision {
        changes,
        ..Default::default()
    })
}
