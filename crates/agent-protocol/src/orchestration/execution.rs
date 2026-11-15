use super::*;
use commands::{effect, id};

/// Loading a physical native resource does not start an app attempt. A late
/// allocation after Stop remains reusable, without reopening or sending input.
pub fn native_resource_loaded(
    state: &State,
    thread_id: &str,
    expected: Option<&ProviderRef>,
    mut session: ProviderSession,
    native: ProviderRef,
    native_fork: bool,
    now: &str,
) -> Result<Decision, String> {
    let thread = state
        .provider_threads
        .get(thread_id)
        .ok_or("native resource is missing")?;
    if state.thread.deleted_at.is_some() || thread.native_thread_ref.as_ref() != expected {
        return Ok(Default::default());
    }
    if thread.app_thread_id.as_ref() != Some(&state.thread.id)
        || thread.provider_instance_id != session.provider_instance_id
        || thread.driver != session.driver
        || native.driver != thread.driver
        || native.native_id.is_none()
    {
        return Err("native resource belongs to another provider or conversation".into());
    }
    if !matches!(
        session.status,
        ProviderSessionStatus::Ready | ProviderSessionStatus::Error
    ) {
        return Err("native allocation has no readiness result".into());
    }
    let ready = session.status == ProviderSessionStatus::Ready;
    session.created_at = state
        .provider_sessions
        .get(&session.id)
        .map_or_else(|| now.into(), |saved| saved.created_at.clone());
    let bound = ProviderThread {
        provider_session_id: ready.then(|| session.id.clone()),
        native_thread_ref: Some(native),
        status: if ready {
            ProviderThreadStatus::Idle
        } else {
            ProviderThreadStatus::Error
        },
        forked_from: if native_fork || state.unconsumed_fork().is_none() {
            thread.forked_from.clone()
        } else {
            None
        },
        updated_at: now.into(),
        ..thread.clone()
    };
    let mut changes = Vec::new();
    if ready {
        changes.push(Change::ProviderSessionAttached(session));
    }
    changes.push(Change::ProviderThread(bound));
    Ok(Decision {
        changes,
        ..Default::default()
    })
}

/// An actual unload result owns resource liveness independently of the input
/// that requested it. Clear every logical alias of that physical resource;
/// other resources and other conversations keep their shared session binding.
pub fn native_resource_unloaded(
    state: &State,
    provider_instance_id: &str,
    native: &ProviderRef,
    now: &str,
) -> Decision {
    let mut sessions = std::collections::BTreeSet::new();
    let mut changes = Vec::new();
    for thread in state.provider_threads.values().filter(|thread| {
        thread.app_thread_id.as_ref() == Some(&state.thread.id)
            && thread.provider_instance_id == provider_instance_id
            && thread.native_thread_ref.as_ref() == Some(native)
            && (thread.provider_session_id.is_some()
                || thread.status != ProviderThreadStatus::NotLoaded)
    }) {
        if let Some(session) = &thread.provider_session_id {
            sessions.insert(session.clone());
        }
        changes.push(Change::ProviderThread(ProviderThread {
            provider_session_id: None,
            status: ProviderThreadStatus::NotLoaded,
            updated_at: now.into(),
            ..thread.clone()
        }));
    }
    let mut decision = Decision {
        changes,
        ..Default::default()
    };
    let released = state.apply(&decision);
    decision.changes.extend(
        sessions
            .into_iter()
            .filter(|session| {
                state.attached_provider_sessions.contains(session)
                    && !released.provider_threads.values().any(|thread| {
                        thread.app_thread_id.as_ref() == Some(&state.thread.id)
                            && thread.provider_session_id.as_ref() == Some(session)
                    })
            })
            .map(|provider_session_id| {
                Change::ProviderSessionDetached(ProviderSessionDetached {
                    provider_session_id,
                    detached_at: now.into(),
                })
            }),
    );
    decision
}

/// Queued work already owns an attempt and root at admission. Activation reuses
/// those identities; only transcript order moves past work started ahead of it.
pub fn activate_queued(
    state: &State,
    run_id: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let run = state
        .runs
        .get(run_id)
        .filter(|run| run.status == RunStatus::Queued && !run.queue_held)
        .ok_or("run is no longer available to start")?;
    if state.active_run().is_some()
        || state.native_work_blocks_delivery()
        || state.has_uncertain_input()
        || state.has_pending_rollback()
    {
        return Err(
            "another run, unobserved native work, or an uncertain write blocks delivery".into(),
        );
    }
    let ordinal = if state.runs.values().any(|other| {
        other.status != RunStatus::Queued
            && other.ordinal > run.ordinal
            && other.started_at.is_some()
    }) {
        context.next_run_ordinal
    } else {
        run.ordinal
    };
    let provider_thread_id = state
        .thread
        .active_provider_thread_id
        .as_ref()
        .and_then(|id| state.provider_threads.get(id))
        .filter(|thread| thread.provider_instance_id == run.provider_instance_id)
        .map(|thread| thread.id.clone())
        .or_else(|| run.provider_thread_id.clone())
        .ok_or("queued run has no provider thread")?;
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .ok_or("queued run has no attempt")?;
    let root = state
        .nodes
        .get(&attempt.root_node_id)
        .ok_or("queued run root is missing")?;
    let preparing = run.workspace_preparation.is_some();
    let activated = Run {
        status: if preparing {
            RunStatus::Preparing
        } else {
            RunStatus::Starting
        },
        ordinal,
        queue_position: None,
        queue_held: false,
        steer_when_ready_run_id: None,
        provider_thread_id: Some(provider_thread_id.clone()),
        ..run.clone()
    };
    let mut changes = vec![
        Change::RunAttempt(RunAttempt {
            provider_thread_id: provider_thread_id.clone(),
            ..attempt.clone()
        }),
        Change::Thread(Thread {
            active_provider_thread_id: Some(provider_thread_id.clone()),
            provider_instance_id: run.provider_instance_id.clone(),
            model_selection: run.model_selection.clone(),
            updated_at: context.now.into(),
            ..state.thread.clone()
        }),
        Change::Node(ExecutionNode {
            provider_thread_id: Some(provider_thread_id.clone()),
            ..root.clone()
        }),
    ];
    changes.extend(state.pending_fork().map(|transfer| {
        Change::ContextTransfer(transfer.for_run(&run.id, &run.provider_instance_id, context.now))
    }));
    for item in state
        .turn_items
        .values()
        .filter(|item| item.run_id.as_deref() == Some(run_id))
    {
        let mut item = item.clone();
        item.provider_thread_id = Some(provider_thread_id.clone());
        item.updated_at = context.now.into();
        if matches!(item.body, TurnItemBody::UserMessage { .. }) {
            item.status = TurnItemStatus::Completed;
            item.started_at = Some(context.now.into());
            item.completed_at = Some(context.now.into());
        }
        changes.push(Change::TurnItem(item));
    }
    if preparing {
        changes.push(Change::TurnItem(preparation::item(run, root, context.now)));
    }
    changes.push(Change::Run(activated));
    Ok(Decision {
        changes,
        effects: if preparing {
            Vec::new()
        } else {
            vec![effect(
                context,
                &state.thread.id,
                EffectRequest::ProviderTurnStart {
                    run_id: run.id.clone(),
                },
            )]
        },
    })
}

/// Both the provider process and attempt must still own the callback. A retry
/// cannot adopt a late acknowledgement from the previous native write.
pub fn started(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    native_turn_ref: ProviderRef,
    context: Context<'_>,
) -> Result<Decision, String> {
    let mut effects = Vec::new();
    if !state.accepts_observation(attempt_id, session_id) {
        return Ok(Decision::default());
    }
    let attempt = &state.run_attempts[attempt_id];
    // A native turn proves acceptance even if its transport acknowledgement
    // arrives later or is lost. Settle the same durable start operation.
    effects.extend(state.effects.values().filter(|effect|matches!(&effect.request,EffectRequest::ProviderTurnStart {run_id} if run_id==&attempt.run_id)
        && effect.status==EffectStatus::Running && effect.lease_owner.is_none() && effect.last_error.is_some()).map(|effect|Effect {
            status:EffectStatus::Succeeded,last_error:None,lease_owner:None,lease_expires_at:None,
            completed_at:Some(context.now.into()),updated_at:context.now.into(),..effect.clone()
        }));
    let mut changes: Vec<_> = effects
        .iter()
        .filter_map(|effect| resolve_delivery_error(state, &effect.id, context.now))
        .collect();
    if state.provider_turns.values().any(|turn| {
        turn.run_attempt_id.as_deref() == Some(attempt_id)
            && turn.native_turn_ref.as_ref() == Some(&native_turn_ref)
    }) {
        return Ok(Decision { changes, effects });
    }
    let run = &state.runs[&attempt.run_id];
    let provider_thread = &state.provider_threads[&attempt.provider_thread_id];
    if native_turn_ref.driver != provider_thread.driver {
        return Err("native turn uses another provider driver".into());
    }
    let turn = ProviderTurn {
        id: id(context.command_id, "provider-turn"),
        provider_thread_id: provider_thread.id.clone(),
        node_id: attempt.root_node_id.clone(),
        run_attempt_id: Some(attempt.id.clone()),
        native_thread_ref: attempt.native_thread_ref.clone(),
        native_turn_ref: Some(native_turn_ref),
        ordinal: state
            .provider_turns
            .values()
            .filter(|turn| turn.provider_thread_id == provider_thread.id)
            .map(|turn| turn.ordinal)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("provider turn ordinal exhausted")?,
        status: ProviderTurnStatus::Running,
        started_at: Some(context.now.into()),
        completed_at: None,
        token_usage: None,
        model: None,
    };
    let root = state
        .nodes
        .get(&attempt.root_node_id)
        .ok_or("attempt root item is missing")?;
    let turn_id = turn.id.clone();
    changes.extend(vec![
        Change::Run(Run {
            status: RunStatus::Running,
            started_at: run.started_at.clone().or_else(|| Some(context.now.into())),
            work_started_at: run
                .work_started_at
                .clone()
                .or_else(|| Some(context.now.into())),
            ..run.clone()
        }),
        Change::RunAttempt(RunAttempt {
            status: RunAttemptStatus::Running,
            provider_turn_id: Some(turn.id.clone()),

            started_at: attempt
                .started_at
                .clone()
                .or_else(|| Some(context.now.into())),
            ..attempt.clone()
        }),
        Change::Node(ExecutionNode {
            status: NodeStatus::Running,
            started_at: root.started_at.clone().or_else(|| Some(context.now.into())),
            provider_turn_id: Some(turn.id.clone()),
            ..root.clone()
        }),
        Change::ProviderThread(ProviderThread {
            status: ProviderThreadStatus::Active,
            first_run_ordinal: provider_thread.first_run_ordinal.or(Some(run.ordinal)),
            last_run_ordinal: Some(run.ordinal),
            updated_at: context.now.into(),
            ..provider_thread.clone()
        }),
        Change::ProviderTurn(turn),
    ]);
    changes.extend(preparation::workspace_result(
        state,
        run,
        crate::workspace::SetupPhase::Done,
        None,
        context.now,
    ));
    if let Some(reference) = &provider_thread.native_thread_ref {
        changes.extend(
            state
                .context_handoffs
                .values()
                .filter(|handoff| {
                    handoff.to_provider_thread_id == provider_thread.id
                        && handoff.delivery.as_ref().is_some_and(|delivery| {
                            Some(&delivery.native_thread_id) == reference.native_id.as_ref()
                                && delivery.status == HandoffDeliveryStatus::Pending
                        })
                })
                .map(|handoff| {
                    Change::ContextHandoff(ContextHandoff {
                        delivery: handoff.delivery.as_ref().map(|delivery| HandoffDelivery {
                            status: HandoffDeliveryStatus::Inline,
                            ..delivery.clone()
                        }),
                        ..handoff.clone()
                    })
                }),
        );
        // ProviderTurnStartService consumes a planned native fork only when
        // the provider accepts a turn, including callback-before-ACK delivery.
        changes.extend(
            state
                .context_transfers
                .values()
                .filter(|transfer| {
                    transfer.kind == TransferType::Fork
                        && transfer.target_thread_id == state.thread.id
                        && transfer.target_run_id.as_ref() == Some(&run.id)
                        && transfer.status == TransferStatus::Pending
                        && transfer.resolution.is_none()
                })
                .map(|transfer| {
                    Change::ContextTransfer(ContextTransfer {
                        target_provider_instance_id: Some(run.provider_instance_id.clone()),
                        status: TransferStatus::Consumed,
                        resolution: Some(ContextTransferResolution::NativeFork {
                            provider_thread_ref: reference.clone(),
                        }),
                        error: None,
                        updated_at: context.now.into(),
                        consumed_at: Some(context.now.into()),
                        ..transfer.clone()
                    })
                }),
        );
    }
    if state.turn_items.values().any(|item| {
        item.run_id.as_deref() == Some(&run.id)
            && matches!(item.body, TurnItemBody::RunInterruptRequest { .. })
            && item.status == TurnItemStatus::Pending
    }) {
        effects.push(effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderTurnInterrupt {
                provider_session_id: session_id.into(),
                provider_thread_id: provider_thread.id.clone(),
                provider_turn_id: turn_id,
            },
        ));
    }
    Ok(Decision { changes, effects })
}

pub fn observe_item(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    item: TurnItem,
) -> Result<Decision, String> {
    if !state.accepts_observation(attempt_id, session_id)
        && !accepts_background_item(state, attempt_id, session_id, &item)
    {
        return Ok(Decision::default());
    }
    let attempt = &state.run_attempts[attempt_id];
    if item.thread_id != state.thread.id
        || item.run_id.as_deref() != Some(&attempt.run_id)
        || !item_provider_matches(state, &item, &attempt.provider_thread_id)
        || item.provider_turn_id.as_ref().is_some_and(|id| {
            !state
                .provider_turns
                .get(id)
                .is_some_and(|turn| turn.run_attempt_id.as_deref() == Some(attempt_id))
        })
    {
        return Err("item does not belong to this attempt".into());
    }
    if state
        .turn_items
        .get(&item.id)
        .is_some_and(|saved| saved.thread_id != item.thread_id || saved.run_id != item.run_id)
    {
        return Err("item identity is already owned by another run".into());
    }
    Ok(commit_item(state, &attempt.provider_thread_id, item))
}

pub(super) fn item_provider_matches(state: &State, item: &TurnItem, parent: &str) -> bool {
    item.provider_thread_id.as_deref() == Some(parent)
        || matches!(&item.body,TurnItemBody::Subagent {child_thread_id:Some(child),..}
            if item.provider_thread_id.as_ref().and_then(|id|state.provider_threads.get(id))
                .is_some_and(|thread|thread.app_thread_id.as_ref()==Some(child)))
}

pub(super) fn commit_item(state: &State, provider_thread_id: &str, item: TurnItem) -> Decision {
    let mut changes = vec![];
    if item.status.is_terminal()
        && let Some(thread) = state.provider_threads.get(provider_thread_id)
    {
        let pending: Vec<_> = thread
            .pending_background_tasks
            .iter()
            .filter(|task| !background_task_matches(task, &item))
            .cloned()
            .collect();
        if pending.len() != thread.pending_background_tasks.len() {
            changes.push(Change::ProviderThread(ProviderThread {
                pending_background_tasks: pending,
                updated_at: item.updated_at.clone(),
                ..thread.clone()
            }));
        }
    }
    changes.push(Change::TurnItem(item));
    Decision {
        changes,
        ..Default::default()
    }
}

pub fn observe_plan(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    plan: Plan,
    item: TurnItem,
) -> Result<Decision, String> {
    validate_plan(&plan, &item)?;
    let mut decision = observe_item(state, attempt_id, session_id, item)?;
    if !decision.changes.is_empty() {
        decision.changes.push(Change::Plan(plan));
    }
    Ok(decision)
}

pub(super) fn validate_plan(plan: &Plan, item: &TurnItem) -> Result<(), String> {
    let matching = match &item.body {
        TurnItemBody::ProposedPlan { plan_id, .. } => {
            plan.kind == PlanKind::ProposedPlan && plan_id == &plan.id
        }
        TurnItemBody::TodoList { plan_id, .. } => {
            plan.kind == PlanKind::TodoList && plan_id == &plan.id
        }
        _ => false,
    };
    if !matching
        || plan.thread_id != item.thread_id
        || plan.run_id != item.run_id
        || item.node_id.as_ref() != Some(&plan.node_id)
    {
        return Err("plan and item identities do not match".into());
    }
    Ok(())
}

pub fn observe_subagent(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    subagent: Subagent,
    item: TurnItem,
) -> Result<Decision, String> {
    validate_subagent(&subagent, &item)?;
    let mut decision = observe_item(state, attempt_id, session_id, item)?;
    if !decision.changes.is_empty() {
        decision.changes.push(Change::Subagent(subagent));
    }
    Ok(decision)
}

/// A provider child reports its own work independently of its parent's
/// foreground turn. Only a real new child turn may reopen a settled task.
pub fn observe_subagent_progress(
    state: &State,
    subagent_id: &str,
    status: TurnItemStatus,
    result: Option<&str>,
    new_turn: bool,
    started_at: Option<&str>,
    now: &str,
) -> Result<Decision, String> {
    let Some(task) = state.subagents.get(subagent_id) else {
        return Ok(Decision::default());
    };
    let saved = state.turn_items.values().find(|item|
        matches!(&item.body,TurnItemBody::Subagent {subagent_id,..} if subagent_id==&task.id));
    if let Some(saved) = saved {
        validate_subagent(task, saved)?;
    } else if task.native_task_ref.is_some() {
        return Err("native subagent item is missing".into());
    }
    let node = state
        .nodes
        .get(&task.id)
        .ok_or("subagent node is missing")?;
    if saved.is_some_and(|saved| saved.node_id.as_deref() != Some(&task.id))
        || node.thread_id != task.thread_id
        || node.run_id != task.run_id
    {
        return Err("subagent node identity does not match".into());
    }
    let active = !status.is_terminal();
    let was_active = matches!(
        task.status,
        SubagentStatus::Idle
            | SubagentStatus::Pending
            | SubagentStatus::Running
            | SubagentStatus::Waiting
    );
    if !was_active && active && !new_turn {
        return Ok(Decision::default());
    }
    let advance = was_active || new_turn;
    let started_at = if new_turn {
        started_at
            .map(str::to_owned)
            .or_else(|| task.started_at.clone())
    } else {
        task.started_at.clone()
    };
    let completed_at = if advance {
        (!active).then(|| task.completed_at.clone().unwrap_or_else(|| now.into()))
    } else {
        task.completed_at.clone()
    };
    let (task_status, node_status) = if !advance {
        (task.status, node.status)
    } else {
        match status {
            TurnItemStatus::Idle => (SubagentStatus::Idle, NodeStatus::Idle),
            TurnItemStatus::Pending => (SubagentStatus::Pending, NodeStatus::Pending),
            TurnItemStatus::Running => (SubagentStatus::Running, NodeStatus::Running),
            TurnItemStatus::Waiting => (SubagentStatus::Waiting, NodeStatus::Waiting),
            TurnItemStatus::Completed => (SubagentStatus::Completed, NodeStatus::Completed),
            TurnItemStatus::Failed => (SubagentStatus::Failed, NodeStatus::Failed),
            TurnItemStatus::Cancelled => (SubagentStatus::Cancelled, NodeStatus::Cancelled),
            TurnItemStatus::Interrupted => (SubagentStatus::Interrupted, NodeStatus::Interrupted),
        }
    };
    let mut decision = Decision::default();
    if let Some(saved) = saved {
        let mut item = TurnItem {
            status: if advance { status } else { saved.status },
            started_at: started_at.clone(),
            completed_at: completed_at.clone(),
            updated_at: saved.updated_at.clone(),
            ..saved.clone()
        };
        if let TurnItemBody::Subagent {
            result: saved_result,
            progress,
            ..
        } = &mut item.body
        {
            if new_turn {
                *progress = None;
                *saved_result = result.map(str::to_owned);
            } else if let Some(result) = result {
                *saved_result = Some(result.into());
            }
        }
        if &item != saved {
            item.updated_at = now.into();
            decision = execution::commit_item(
                state,
                saved
                    .provider_turn_id
                    .as_ref()
                    .and_then(|id| state.provider_turns.get(id))
                    .map(|turn| turn.provider_thread_id.as_str())
                    .ok_or("subagent parent turn is missing")?,
                item,
            );
        }
    }
    let next_node = ExecutionNode {
        status: node_status,
        started_at: started_at.clone(),
        completed_at: completed_at.clone(),
        ..node.clone()
    };
    if &next_node != node {
        decision.changes.push(Change::Node(next_node));
    }
    let mut next_task = Subagent {
        status: task_status,
        started_at,
        completed_at,
        ..task.clone()
    };
    if &next_task != task {
        next_task.updated_at = now.into();
        decision.changes.push(Change::Subagent(next_task));
    }
    Ok(decision)
}

pub(super) fn validate_subagent(subagent: &Subagent, item: &TurnItem) -> Result<(), String> {
    let matching = matches!(&item.body,TurnItemBody::Subagent {subagent_id,origin,driver,provider_instance_id,child_thread_id,..}
        if subagent_id==&subagent.id && origin==&subagent.origin && driver==&subagent.driver
            && provider_instance_id==&subagent.provider_instance_id && child_thread_id==&subagent.child_thread_id);
    if !matching || subagent.thread_id != item.thread_id || subagent.run_id != item.run_id {
        return Err("subagent and item identities do not match".into());
    }
    Ok(())
}

fn accepts_background_item(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    item: &TurnItem,
) -> bool {
    let Some(attempt) = state
        .owned_attempt(attempt_id, session_id)
        .filter(|attempt| attempt.status == RunAttemptStatus::Completed)
    else {
        return false;
    };
    is_background_item(
        state,
        &attempt.provider_thread_id,
        state.turn_items.get(&item.id).unwrap_or(item),
    )
}

fn background_task_matches(task: &PendingBackgroundTask, item: &TurnItem) -> bool {
    if let Some(id) = &task.item_id {
        return id == &item.id;
    }
    item.native_item_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_ref())
        .is_some_and(|native| task.native_item_id.as_ref().unwrap_or(&task.task_id) == native)
}

pub(super) fn is_background_item(state: &State, provider_thread_id: &str, item: &TurnItem) -> bool {
    (state.turn_items.contains_key(&item.id) && is_pending_background_item(item))
        || state
            .provider_threads
            .get(provider_thread_id)
            .is_some_and(|thread| {
                thread
                    .pending_background_tasks
                    .iter()
                    .any(|task| background_task_matches(task, item))
            })
}

/// Image work can finish after the native turn's foreground terminal event.
pub(super) fn is_pending_background_item(item: &TurnItem) -> bool {
    matches!(
        item.status,
        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
    ) && (matches!(item.body, TurnItemBody::Subagent { .. })
        || matches!(&item.body,TurnItemBody::DynamicTool {tool_name:Some(name),..} if name=="image_gen.imagegen"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Completion {
    Completed,
    Interrupted,
    Cancelled,
    Failed(ProviderFailure),
}

pub fn finish_provider_turn(
    state: &State,
    provider_turn_id: &str,
    session_id: &str,
    completion: Completion,
    now: &str,
) -> Decision {
    let Some(turn) = state.provider_turns.get(provider_turn_id) else {
        return Decision::default();
    };
    if turn
        .run_attempt_id
        .as_ref()
        .and_then(|id| state.owned_attempt(id, session_id))
        .is_none()
    {
        return Decision::default();
    }
    let status = match completion {
        Completion::Completed => ProviderTurnStatus::Completed,
        Completion::Interrupted => ProviderTurnStatus::Interrupted,
        Completion::Cancelled => ProviderTurnStatus::Cancelled,
        Completion::Failed(_) => ProviderTurnStatus::Failed,
    };
    Decision {
        changes: vec![Change::ProviderTurn(ProviderTurn {
            status,
            completed_at: Some(now.into()),
            ..turn.clone()
        })],
        ..Default::default()
    }
}

pub fn complete(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    completion: Completion,
    context: Context<'_>,
) -> Result<Decision, String> {
    if !state.accepts_observation(attempt_id, session_id) {
        return Ok(Decision::default());
    }
    finish_run(state, attempt_id, completion, context)
}

/// A failed native start belongs to the outbox worker, before a provider session
/// or native turn necessarily exists. It cannot terminate an attempt that has
/// since started or been cancelled.
pub fn fail_start(
    state: &State,
    attempt_id: &str,
    failure: ProviderFailure,
    context: Context<'_>,
) -> Result<Decision, String> {
    let Some(attempt) = state.run_attempts.get(attempt_id) else {
        return Ok(Decision::default());
    };
    let Some(run) = state.runs.get(&attempt.run_id) else {
        return Ok(Decision::default());
    };
    if run.status != RunStatus::Starting
        || run.active_attempt_id.as_deref() != Some(attempt_id)
        || attempt.status != RunAttemptStatus::Pending
        || attempt.provider_turn_id.is_some()
    {
        return Ok(Decision::default());
    }
    finish_run(state, attempt_id, Completion::Failed(failure), context)
}

/// Only the I/O owner can prove a claimed start has not written to the native
/// process yet. Stop can then cancel it without waiting for a native turn ID.
pub fn cancel_start(
    state: &State,
    attempt_id: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let Some(attempt) = state.run_attempts.get(attempt_id) else {
        return Ok(Decision::default());
    };
    if attempt.status != RunAttemptStatus::Pending
        || attempt.provider_turn_id.is_some()
        || !state.runs.get(&attempt.run_id).is_some_and(|run| {
            run.status == RunStatus::Starting
                && run.active_attempt_id.as_deref() == Some(attempt_id)
        })
        || !state.turn_items.values().any(|item| {
            item.run_id.as_deref() == Some(&attempt.run_id)
                && item.status == TurnItemStatus::Pending
                && matches!(item.body, TurnItemBody::RunInterruptRequest { .. })
        })
    {
        return Ok(Decision::default());
    }
    finish_run(state, attempt_id, Completion::Cancelled, context)
}

/// Delivery uncertainty occupies the existing outbox lane. The transcript has
/// an error attached to the admitted message, without a second input state.
pub fn uncertain_delivery(
    state: &State,
    effect_id: &str,
    message: &str,
    now: &str,
) -> Result<Decision, String> {
    let effect = state
        .effects
        .get(effect_id)
        .ok_or("native effect is missing")?;
    let (run, message_id) = effect_input(state, effect).ok_or("effect has no input")?;
    if !matches!(effect.request, EffectRequest::ProviderTurnSteer { .. })
        && state
            .active_attempt(run.active_attempt_id.as_deref())
            .is_some_and(|attempt| attempt.provider_turn_id.is_some())
    {
        return Ok(Decision {
            changes: resolve_delivery_error(state, effect_id, now)
                .into_iter()
                .collect(),
            effects: vec![Effect {
                status: EffectStatus::Succeeded,
                lease_owner: None,
                lease_expires_at: None,
                completed_at: Some(now.into()),
                last_error: None,
                updated_at: now.into(),
                ..effect.clone()
            }],
        });
    }
    let item = TurnItem {
        id: format!("turn-item:delivery-uncertain:{effect_id}"),
        thread_id: state.thread.id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone(),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: Some(user_item_id(&state.thread.id, message_id)),
        ordinal: 0,
        status: TurnItemStatus::Failed,
        title: Some("Input delivery not confirmed".into()),
        started_at: Some(now.into()),
        completed_at: Some(now.into()),
        updated_at: now.into(),
        body: TurnItemBody::Error {
            failure: ProviderFailure {
                class: "delivery_uncertain".into(),
                message: message.into(),
                code: None,
                retryable: Some(false),
                reset_at: None,
            },
            retry: None,
        },
    };
    Ok(Decision {
        changes: vec![Change::TurnItem(item)],
        effects: vec![Effect {
            status: if matches!(effect.request, EffectRequest::ProviderTurnSteer { .. }) {
                EffectStatus::Failed
            } else {
                EffectStatus::Running
            },
            completed_at: matches!(effect.request, EffectRequest::ProviderTurnSteer { .. })
                .then(|| now.into()),
            lease_owner: None,
            lease_expires_at: None,
            last_error: Some(message.into()),
            updated_at: now.into(),
            ..effect.clone()
        }],
    })
}

fn resolve_delivery_error(state: &State, effect_id: &str, now: &str) -> Option<Change> {
    state
        .turn_items
        .get(&format!("turn-item:delivery-uncertain:{effect_id}"))
        .map(|item| {
            Change::TurnItem(TurnItem {
                status: TurnItemStatus::Cancelled,
                updated_at: now.into(),
                ..item.clone()
            })
        })
}

fn effect_input<'a>(state: &'a State, effect: &'a Effect) -> Option<(&'a Run, &'a str)> {
    match &effect.request {
        EffectRequest::ProviderTurnStart { run_id }
        | EffectRequest::ProviderTurnRestart { run_id, .. } => {
            let run = state.runs.get(run_id)?;
            Some((run, &run.user_message_id))
        }
        EffectRequest::ProviderTurnSteer {
            provider_turn_id,
            message_id,
            ..
        } => {
            let turn = state.provider_turns.get(provider_turn_id)?;
            let attempt = state.run_attempts.get(turn.run_attempt_id.as_ref()?)?;
            Some((state.runs.get(&attempt.run_id)?, message_id))
        }
        _ => None,
    }
}

pub fn cancel_uncertain(
    state: &State,
    message_id: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let user_item_id = user_item_id(&state.thread.id, message_id);
    if state.turn_items.get(&user_item_id).is_some_and(|item| matches!(&item.body,TurnItemBody::UserMessage {input_intent,..} if input_intent=="steer")) {
        let item=state.turn_items.values().find(|item| item.parent_item_id.as_deref()==Some(&user_item_id)
            && item.status==TurnItemStatus::Failed && matches!(&item.body,TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain"))
            .ok_or("input delivery is no longer uncertain")?;
        return Ok(Decision {changes:vec![
            Change::TurnItem(TurnItem {status:TurnItemStatus::Cancelled,updated_at:context.now.into(),..item.clone()}),
            Change::TurnItem(TurnItem {status:TurnItemStatus::Cancelled,updated_at:context.now.into(),..state.turn_items[&user_item_id].clone()})
        ],effects:vec![]});
    }
    let uncertain_effect = state
        .effects
        .values()
        .find(|effect| {
            effect.status == EffectStatus::Running
                && effect.lease_owner.is_none()
                && effect.last_error.is_some()
                && effect_input(state, effect).is_some_and(|(_, id)| id == message_id)
        })
        .ok_or("input delivery is no longer uncertain")?;
    let (run, _) = effect_input(state, uncertain_effect).ok_or("effect has no input")?;
    let error = resolve_delivery_error(state, &uncertain_effect.id, context.now);
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .ok_or("input has no active attempt")?;
    let mut decision = finish_run(state, &attempt.id, Completion::Cancelled, context)?;
    decision.changes.extend(error);
    if let Some(session) = state.provider_session(&attempt.provider_thread_id) {
        decision
            .changes
            .push(Change::ProviderSessionDetached(ProviderSessionDetached {
                provider_session_id: session.id.clone(),
                detached_at: context.now.into(),
            }));
        decision.effects.push(effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderSessionDetach {
                provider_session_id: session.id.clone(),
                detail: Some("Unconfirmed input cancelled by the user".into()),
            },
        ));
    }
    Ok(decision)
}

pub(super) fn finish_run(
    state: &State,
    attempt_id: &str,
    completion: Completion,
    context: Context<'_>,
) -> Result<Decision, String> {
    let mut effects = Vec::new();
    let attempt = &state.run_attempts[attempt_id];
    let run = &state.runs[&attempt.run_id];
    let (run_status, attempt_status, turn_status, item_status) = match &completion {
        Completion::Completed => (
            RunStatus::Completed,
            RunAttemptStatus::Completed,
            ProviderTurnStatus::Completed,
            TurnItemStatus::Completed,
        ),
        Completion::Interrupted => (
            RunStatus::Interrupted,
            RunAttemptStatus::Interrupted,
            ProviderTurnStatus::Interrupted,
            TurnItemStatus::Interrupted,
        ),
        Completion::Cancelled => (
            RunStatus::Cancelled,
            RunAttemptStatus::Cancelled,
            ProviderTurnStatus::Cancelled,
            TurnItemStatus::Cancelled,
        ),
        Completion::Failed(_) => (
            RunStatus::Failed,
            RunAttemptStatus::Failed,
            ProviderTurnStatus::Failed,
            TurnItemStatus::Failed,
        ),
    };
    let scope_id = run
        .root_node_id
        .as_ref()
        .and_then(|id| state.nodes.get(id))
        .and_then(|root| root.checkpoint_scope_id.clone());
    let capturing = run_status == RunStatus::Completed && scope_id.is_some();
    let mut changes = vec![
        Change::Run(Run {
            status: if capturing {
                RunStatus::Waiting
            } else {
                run_status
            },
            completed_at: if capturing {
                None
            } else {
                Some(context.now.into())
            },
            ..run.clone()
        }),
        Change::RunAttempt(RunAttempt {
            status: attempt_status,
            completed_at: Some(context.now.into()),
            ..attempt.clone()
        }),
    ];
    if let Some(thread) = state.provider_threads.get(&attempt.provider_thread_id) {
        changes.push(Change::ProviderThread(ProviderThread {
            status: ProviderThreadStatus::Idle,
            updated_at: context.now.into(),
            ..thread.clone()
        }));
    }
    if let Some(turn) = attempt
        .provider_turn_id
        .as_ref()
        .and_then(|id| state.provider_turns.get(id))
        .filter(|turn| {
            matches!(
                turn.status,
                ProviderTurnStatus::Pending | ProviderTurnStatus::Running
            )
        })
    {
        changes.push(Change::ProviderTurn(ProviderTurn {
            status: turn_status,
            completed_at: Some(context.now.into()),
            ..turn.clone()
        }));
    }
    for item in state.turn_items.values().filter(|item| {
        item.run_id.as_deref() == Some(&run.id)
            && !(run_status == RunStatus::Completed
                && is_background_item(state, &attempt.provider_thread_id, item))
            && matches!(
                item.status,
                TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
            )
    }) {
        let item = settle_item(item, item_status, context.now);
        changes.push(Change::TurnItem(item));
    }
    for node in state.nodes.values().filter(|node| {
        node.run_id.as_deref() == Some(&run.id)
            && !(run_status == RunStatus::Completed
                && state.turn_items.values().any(|item| {
                    item.node_id.as_ref() == Some(&node.id)
                        && is_background_item(state, &attempt.provider_thread_id, item)
                }))
            && matches!(
                node.status,
                NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
            )
    }) {
        let waiting = capturing && run.root_node_id.as_ref() == Some(&node.id);
        let status = if waiting {
            NodeStatus::Waiting
        } else {
            match run_status {
                RunStatus::Completed => NodeStatus::Completed,
                RunStatus::Interrupted => NodeStatus::Interrupted,
                RunStatus::Cancelled => NodeStatus::Cancelled,
                _ => NodeStatus::Failed,
            }
        };
        changes.push(Change::Node(ExecutionNode {
            status,
            completed_at: if waiting {
                None
            } else {
                Some(context.now.into())
            },
            ..node.clone()
        }));
    }
    for request in state.runtime_requests.values().filter(|request| {
        request.status == RequestStatus::Pending
            && state
                .nodes
                .get(&request.node_id)
                .is_some_and(|node| node.run_id.as_deref() == Some(&run.id))
    }) {
        changes.push(Change::RuntimeRequest(RuntimeRequest {
            status: if run_status == RunStatus::Interrupted {
                RequestStatus::Cancelled
            } else {
                RequestStatus::Expired
            },
            resolved_at: Some(context.now.into()),
            ..request.clone()
        }));
    }
    let workspace_failure = match &completion {
        Completion::Failed(failure) => Some((
            crate::workspace::SetupPhase::Failed,
            Some(failure.message.clone()),
        )),
        Completion::Cancelled | Completion::Interrupted => {
            Some((crate::workspace::SetupPhase::Cancelled, None))
        }
        Completion::Completed => None,
    };
    if let Some((phase, error)) = workspace_failure {
        changes.extend(preparation::workspace_result(
            state,
            run,
            phase,
            error,
            context.now,
        ));
    }
    if let Completion::Failed(failure) = completion {
        let error_id = id(context.command_id, "error-item");
        changes.push(Change::TurnItem(TurnItem {
            id: error_id,
            thread_id: state.thread.id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(attempt.root_node_id.clone()),
            provider_thread_id: Some(attempt.provider_thread_id.clone()),
            provider_turn_id: attempt.provider_turn_id.clone(),
            native_item_ref: None,
            parent_item_id: None,
            ordinal: 0,
            status: TurnItemStatus::Failed,
            title: None,
            started_at: Some(context.now.into()),
            completed_at: Some(context.now.into()),
            updated_at: context.now.into(),
            body: TurnItemBody::Error {
                failure,
                retry: None,
            },
        }));
    }
    if matches!(run_status, RunStatus::Completed | RunStatus::Interrupted) {
        if let Some(scope_id) = scope_id {
            effects.push(checkpoints::capture_effect(
                context,
                &state.thread.id,
                &run.id,
                &scope_id,
            ));
        }
    }
    if run_status == RunStatus::Cancelled {
        effects.extend(state.effects.values().filter(|effect|matches!(&effect.request,EffectRequest::ProviderTurnStart {run_id} if run_id==&run.id)
            && matches!(effect.status,EffectStatus::Pending|EffectStatus::Running)).map(|effect|Effect {status:EffectStatus::Cancelled,lease_owner:None,lease_expires_at:None,completed_at:Some(context.now.into()),updated_at:context.now.into(),..effect.clone()}));
    }
    if run_status == RunStatus::Completed && !capturing {
        let projected = state.apply(&Decision {
            changes: changes.clone(),
            effects: effects.clone(),
        });
        if let Some(next) = projected.next_queued_run() {
            let activation = activate_queued(&projected, &next.id, context)?;
            changes.extend(activation.changes);
            effects.extend(activation.effects);
        }
    } else if run_status != RunStatus::Completed {
        changes.extend(hold_queue(state, true).changes);
    }
    Ok(Decision { changes, effects })
}

pub fn interrupt(state: &State, run_id: &str, context: Context<'_>) -> Result<Decision, String> {
    let mut effects = Vec::new();
    let run = state
        .active_run()
        .filter(|run| run.id == run_id)
        .ok_or("run is no longer active")?;
    if state.turn_items.values().any(|item| {
        item.run_id.as_deref() == Some(run_id)
            && matches!(item.body, TurnItemBody::RunInterruptRequest { .. })
            && item.status == TurnItemStatus::Pending
    }) {
        return Ok(hold_queue(state, true));
    }
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .ok_or("run has no attempt")?;
    if run.status == RunStatus::Starting
        && run.workspace_preparation.is_some()
        && state
            .thread
            .workspace_launch
            .as_ref()
            .and_then(|workspace| workspace.setup.as_ref())
            .is_some_and(|setup| {
                !setup.can_cancel && setup.phase == crate::workspace::SetupPhase::Running
            })
    {
        return Err("agent startup has passed the cancellation boundary".into());
    }
    let start_in_flight = state.effects.values().any(|effect| {
        matches!(&effect.request,
        EffectRequest::ProviderTurnStart {run_id} if run_id==&run.id)
            && effect.status != EffectStatus::Pending
            && effect.status != EffectStatus::Cancelled
    });
    if attempt.status == RunAttemptStatus::Pending
        && attempt.provider_turn_id.is_none()
        && !start_in_flight
    {
        return finish_run(state, &attempt.id, Completion::Cancelled, context);
    }
    let request = TurnItem {
        id: id(context.command_id, "interrupt-request"),
        thread_id: state.thread.id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(attempt.root_node_id.clone()),
        provider_thread_id: Some(attempt.provider_thread_id.clone()),
        provider_turn_id: attempt.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: TurnItemStatus::Pending,
        title: None,
        started_at: Some(context.now.into()),
        completed_at: None,
        updated_at: context.now.into(),
        body: TurnItemBody::RunInterruptRequest {
            message: "Interrupt requested".into(),
        },
    };
    let mut changes = hold_queue(state, true).changes;
    if let (Some(turn_id), Some(session)) = (
        &attempt.provider_turn_id,
        state.provider_session(&attempt.provider_thread_id),
    ) {
        effects.push(effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderTurnInterrupt {
                provider_session_id: session.id.clone(),
                provider_thread_id: attempt.provider_thread_id.clone(),
                provider_turn_id: turn_id.clone(),
            },
        ));
    }
    changes.push(Change::TurnItem(request));
    Ok(Decision { changes, effects })
}

/// A process cannot carry its old background roster into a later attachment.
pub(super) fn lose_background_work(state: &State, provider: &str, now: &str) -> Vec<Change> {
    let mut changes = vec![];
    for thread in state
        .provider_threads
        .values()
        .filter(|thread| thread.provider_instance_id == provider)
    {
        for item in state.turn_items.values().filter(|item| {
            item.provider_thread_id.as_ref() == Some(&thread.id)
                && matches!(
                    item.status,
                    TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
                )
                && (thread
                    .pending_background_tasks
                    .iter()
                    .any(|task| background_task_matches(task, item))
                    || matches!(&item.body,TurnItemBody::Subagent {origin,..} if origin=="native")
                    || matches!(&item.body,TurnItemBody::DynamicTool {tool_name:Some(name),..} if name=="image_gen.imagegen"))
        }) {
            changes.push(Change::TurnItem(TurnItem {
                status: TurnItemStatus::Interrupted,
                completed_at: Some(now.into()),
                updated_at: now.into(),
                ..item.clone()
            }));
            if let Some(node) = item.node_id.as_ref().and_then(|id| state.nodes.get(id)) {
                changes.push(Change::Node(ExecutionNode {
                    status: NodeStatus::Interrupted,
                    completed_at: Some(now.into()),
                    ..node.clone()
                }));
            }
        }
        if !thread.pending_background_tasks.is_empty() {
            changes.push(Change::ProviderThread(ProviderThread {
                pending_background_tasks: vec![],
                status: ProviderThreadStatus::Error,
                updated_at: now.into(),
                ..thread.clone()
            }));
        }
    }
    changes
}

/// Status observations cannot move an inactive provider or superseded native
/// source. Unconsumed queued input can enter preparation once that source is
/// no longer observed running. Unknown activity never permits replaying an
/// uncertain write: that is a separate delivery fence.
pub fn observe_native_status(
    state: &State,
    provider_instance_id: &str,
    driver: &str,
    native_id: &str,
    status: ProviderThreadStatus,
    context: Context<'_>,
) -> Result<Decision, String> {
    let Some(thread) = state.selected_native_thread().filter(|thread| {
        thread.provider_instance_id == provider_instance_id
            && thread.driver == driver
            && thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some(native_id)
    }) else {
        return Ok(Decision::default());
    };
    let mut decision = Decision::default();
    if thread.status != status {
        decision
            .changes
            .push(Change::ProviderThread(ProviderThread {
                status,
                updated_at: context.now.into(),
                ..thread.clone()
            }));
    }
    if status != ProviderThreadStatus::Active
        && state.active_run().is_none()
        && !state.has_uncertain_input()
    {
        let projected = state.apply(&decision);
        if let Some(next) = projected.next_queued_run() {
            let activation = activate_queued(&projected, &next.id, context)?;
            decision.changes.extend(activation.changes);
            decision.effects.extend(activation.effects);
        }
    }
    Ok(decision)
}

pub(super) fn settle_item(item: &TurnItem, status: TurnItemStatus, now: &str) -> TurnItem {
    let mut item = TurnItem {
        status: status,
        completed_at: Some(now.into()),
        updated_at: now.into(),
        ..item.clone()
    };
    match &mut item.body {
        TurnItemBody::AssistantMessage { streaming, .. }
        | TurnItemBody::Reasoning { streaming, .. }
        | TurnItemBody::ProposedPlan { streaming, .. } => *streaming = false,
        _ => {}
    }
    item
}
