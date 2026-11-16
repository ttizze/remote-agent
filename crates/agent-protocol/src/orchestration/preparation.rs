use super::*;
use commands::{effect, id};

const INPUT: &str = "Preparing workspace";
const FAILURE_CODE: &str = "workspace_preparation_failed";

/// Resource callbacks are fenced by launch ownership, independently of app or
/// native turn IDs. Expiry is an idempotent timer; stale I/O must stop instead
/// of allocating a checkout or starting a script for a replaced launch.
pub fn workspace_progress(
    state: &State,
    generation: uuid::Uuid,
    update: crate::workspace::SetupChange,
    now: &str,
) -> Result<Decision, String> {
    use crate::workspace::{SetupChange, SetupPhase};
    let timer = matches!(update, SetupChange::Expire { .. });
    let Some(saved) = state
        .thread
        .workspace_launch
        .as_ref()
        .filter(|workspace| workspace.generation == generation)
    else {
        return if timer {
            Ok(Default::default())
        } else {
            Err("workspace launch no longer owns this callback".into())
        };
    };
    let mut workspace = saved.clone();
    let at = chrono::DateTime::parse_from_rfc3339(now)
        .map_err(|error| error.to_string())?
        .timestamp_millis()
        .max(0) as u64;
    let binding = match &update {
        SetupChange::Created { path, branch } => Some((Some(path.clone()), Some(branch.clone()))),
        SetupChange::Discarded if workspace.created_worktree => {
            Some((Some(workspace.project_cwd.clone()), None))
        }
        _ => None,
    };
    match update {
        SetupChange::Begin { workspace: next } => {
            let preparing = state.active_run().is_some_and(|run| {
                run.status == RunStatus::Preparing && workspace.run_id.as_deref() == Some(&run.id)
            });
            let empty = workspace.run_id.is_none()
                && workspace
                    .setup
                    .as_ref()
                    .is_some_and(|setup| setup.phase == SetupPhase::Running && setup.can_cancel);
            if next.generation != generation
                || next.run_id != workspace.run_id
                || (!preparing && !empty)
            {
                return Err("workspace preparation was cancelled".into());
            }
            workspace = next;
        }
        SetupChange::Discarded if workspace.created_worktree => {
            workspace.created_worktree = false;
            if let Some(setup) = &mut workspace.setup {
                setup.worktree_path = None;
                setup.branch = None;
                setup.finish(SetupPhase::Cancelled, None, at);
            }
        }
        SetupChange::Expire { ended_at } => {
            if workspace
                .setup
                .as_ref()
                .is_some_and(|setup| setup.ended_at_ms == Some(ended_at))
            {
                workspace.visible = false;
            }
        }
        update => {
            let Some(setup) = &mut workspace.setup else {
                return Ok(Default::default());
            };
            if setup.phase != SetupPhase::Running
                && matches!(
                    update,
                    SetupChange::Script { .. }
                        | SetupChange::Created { .. }
                        | SetupChange::Base(_)
                        | SetupChange::Checkout(_)
                )
            {
                return Err("workspace preparation was cancelled".into());
            }
            setup.apply(update, at);
        }
    }
    if workspace == *saved {
        return Ok(Default::default());
    }
    Ok(Decision {
        changes: vec![Change::Thread(Thread {
            worktree_path: binding
                .as_ref()
                .map(|(path, _)| path.clone())
                .unwrap_or_else(|| state.thread.worktree_path.clone()),
            branch: binding
                .map(|(_, branch)| branch)
                .unwrap_or_else(|| state.thread.branch.clone()),
            workspace_launch: Some(workspace),
            updated_at: now.into(),
            ..state.thread.clone()
        })],
        ..Default::default()
    })
}

pub(super) fn workspace_result(
    state: &State,
    run: &Run,
    phase: crate::workspace::SetupPhase,
    error: Option<String>,
    now: &str,
) -> Option<Change> {
    use crate::workspace::{SetupPhase, SetupStageId, SetupStageStatus};
    run.workspace_preparation.as_ref()?;
    let mut workspace = state
        .thread
        .workspace_launch
        .clone()
        .filter(|workspace| workspace.run_id.as_deref() == Some(&run.id))?;
    let setup = workspace
        .setup
        .as_mut()
        .filter(|setup| setup.phase == SetupPhase::Running)?;
    let at = chrono::DateTime::parse_from_rfc3339(now)
        .ok()?
        .timestamp_millis()
        .max(0) as u64;
    if phase == SetupPhase::Done {
        setup.stage(SetupStageId::Agent, SetupStageStatus::Done, at);
        setup.finish_if_ready(at);
    } else {
        setup.finish(phase, error, at);
    }
    Some(Change::Thread(Thread {
        workspace_launch: Some(workspace),
        updated_at: now.into(),
        ..state.thread.clone()
    }))
}

pub(super) fn item(run: &Run, root: &ExecutionNode, now: &str) -> TurnItem {
    TurnItem {
        detail_omitted: false,
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

pub fn retry_input(
    state: &State,
    run_id: &str,
    work_locally: bool,
    context: Context<'_>,
) -> Result<Decision, String> {
    if state.thread.archived_at.is_some()
        || state.thread.deleted_at.is_some()
        || state.active_run().is_some_and(|run| {
            !work_locally || run.id != run_id || run.status != RunStatus::Preparing
        })
    {
        return Err("thread cannot retry preparation".into());
    }
    let run = state.runs.get(run_id).ok_or("run is missing")?;
    if !state.turn_items.values().any(|item|matches!(&item.body,TurnItemBody::UserMessage{message_id,..} if message_id==&run.user_message_id) && item.status!=TurnItemStatus::Cancelled) {
        return Err("input was removed".into());
    }
    let failure=state.turn_items.values().find(|item|item.run_id.as_deref()==Some(run_id)
        && item.status==TurnItemStatus::Failed
        && matches!(&item.body,TurnItemBody::Error {failure,..} if failure.code.as_deref()==Some(FAILURE_CODE)));
    let mut decision = if failure.is_some()
        || (run.status == RunStatus::Cancelled && run.workspace_preparation.is_some())
    {
        retry_preparation(state, run_id, failure, context)?
    } else if work_locally && run.status == RunStatus::Preparing {
        Decision::default()
    } else {
        return if work_locally {
            Err("run has no workspace preparation to recover".into())
        } else {
            super::recovery::retry_unsent_start(state, run_id, context)
        };
    };
    if work_locally {
        let attempt = state
            .active_attempt(run.active_attempt_id.as_deref())
            .ok_or("run has no attempt")?;
        if attempt.provider_turn_id.is_some()
            || state
                .provider_turns
                .values()
                .any(|turn| turn.run_attempt_id.as_deref() == Some(&attempt.id))
        {
            return Err("native input has already been accepted".into());
        }
        let mut workspace = state
            .thread
            .workspace_launch
            .clone()
            .ok_or("workspace preparation is missing")?;
        let running = run.status == RunStatus::Preparing;
        if running
            && workspace
                .setup
                .as_ref()
                .is_none_or(|setup| !setup.can_cancel)
        {
            return Err("workspace preparation cannot be cancelled".into());
        }
        workspace.generation =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, context.command_id.as_bytes());
        workspace.strategy = Some(crate::workspace::WorkspaceStrategy::Root);
        workspace.created_worktree &= running;
        workspace.visible = false;
        if running && let Some(setup) = &mut workspace.setup {
            setup.finish(
                crate::workspace::SetupPhase::Cancelled,
                None,
                chrono::DateTime::parse_from_rfc3339(context.now)
                    .map_err(|error| error.to_string())?
                    .timestamp_millis()
                    .max(0) as u64,
            );
        }
        let current = state.apply(&decision);
        decision
            .changes
            .retain(|change| !matches!(change, Change::Thread(_) | Change::Run(_)));
        decision.changes.extend([
            Change::Run(Run {
                workspace_preparation: Some(WorkspacePreparation::Root { branch: None }),
                ..current.runs[run_id].clone()
            }),
            Change::Thread(Thread {
                workspace_launch: Some(workspace),
                updated_at: context.now.into(),
                ..current.thread
            }),
        ]);
    }
    Ok(decision)
}

fn retry_preparation(
    state: &State,
    run_id: &str,
    failure: Option<&TurnItem>,
    context: Context<'_>,
) -> Result<Decision, String> {
    let now = context.now;
    let status = state.runs.get(run_id).ok_or("run is missing")?.status;
    if !matches!(status, RunStatus::Failed | RunStatus::Cancelled) {
        return Err("run is not failed or cancelled".into());
    }
    let (run, attempt, item) = prepared(state, run_id, status)?;
    if !matches!(
        item.status,
        TurnItemStatus::Failed | TurnItemStatus::Cancelled
    ) || attempt.provider_turn_id.is_some()
        || state
            .provider_turns
            .values()
            .any(|turn| turn.run_attempt_id.as_deref() == Some(&attempt.id))
    {
        return Err("run has no failed workspace preparation to retry".into());
    }
    let root = state
        .nodes
        .get(&attempt.root_node_id)
        .ok_or("run root is missing")?;
    let mut changes = vec![
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
    changes.extend(failure.map(|failure| {
        Change::TurnItem(TurnItem {
            status: TurnItemStatus::Cancelled,
            updated_at: now.into(),
            ..failure.clone()
        })
    }));
    if let Some(mut workspace) = state.thread.workspace_launch.clone() {
        workspace.generation =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, context.command_id.as_bytes());
        workspace.visible = true;
        if let Some(setup) = &mut workspace.setup {
            if setup.phase != crate::workspace::SetupPhase::Cancelled {
                setup.phase = crate::workspace::SetupPhase::Running;
            }
            setup.ended_at_ms = None;
            setup.error = None;
            setup.can_cancel = true;
        }
        changes.push(Change::Thread(Thread {
            workspace_launch: Some(workspace),
            updated_at: now.into(),
            ..state.thread.clone()
        }));
    }
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
