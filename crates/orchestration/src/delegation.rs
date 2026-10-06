//! App-owned delegation and its durable, per-parent-run completion mailbox.
use crate::{decider::DecisionError, *};

fn fail(message: &str) -> DecisionError {
    DecisionError(message.into())
}
fn apply(projection: &ThreadProjection, events: &[DomainEvent]) -> ThreadProjection {
    events
        .iter()
        .filter(|e| e.thread_id == projection.thread.id)
        .fold(projection.clone(), |p, e| {
            projector::apply(Some(&p), e, Default::default()).expect("valid delegated events")
        })
}
fn merge(into: &mut Decision, next: Decision) {
    into.events.extend(next.events);
    into.effects.extend(next.effects);
}
fn task_events(
    task: Subagent,
    projection: &ThreadProjection,
    now: &Timestamp,
) -> Vec<EventPayload> {
    let mut payloads = vec![EventPayload::SubagentUpdated(task.clone())];
    if let Some(node) = projection.nodes.iter().find(|n| n.id == task.id) {
        let mut node = node.clone();
        node.status = task.status;
        node.completed_at = task.completed_at.clone();
        payloads.push(EventPayload::NodeUpdated(node));
    }
    if let Some(item) = projection
        .turn_items
        .iter()
        .find(|i| i.node_id.as_ref() == Some(&task.id))
    {
        let mut item = item.clone();
        item.status = match task.status {
            NodeStatus::Completed => ItemStatus::Completed,
            NodeStatus::Failed => ItemStatus::Failed,
            NodeStatus::Cancelled => ItemStatus::Cancelled,
            NodeStatus::Interrupted => ItemStatus::Interrupted,
            _ => ItemStatus::Running,
        };
        item.completed_at = task.completed_at.clone();
        item.updated_at = now.clone();
        if let TurnItemBody::Subagent {
            progress, result, ..
        } = &mut item.body
        {
            *progress = task.progress.clone();
            *result = task.result.clone();
        }
        payloads.push(EventPayload::TurnItemUpdated(item));
    }
    payloads
}

pub fn request(
    command: &Command,
    parent: &ThreadProjection,
    now: &Timestamp,
) -> Result<Decision, DecisionError> {
    let CommandBody::DelegatedTaskRequest(request) = &command.body else {
        return Err(fail("not a delegation request"));
    };
    let DelegatedTaskRequest {
        parent_run_id,
        parent_node_id,
        task,
        title,
        model_selection,
        runtime_mode,
        interaction_mode,
        completion_wake,
        created_by,
        creation_source,
    } = request.as_ref();
    if parent.thread.deleted_at.is_some()
        || parent.thread.archived_at.is_some()
        || parent.thread.rollback_request_id.is_some()
    {
        return Err(fail("parent thread is unavailable"));
    }
    let run = parent
        .runs
        .iter()
        .find(|r| r.id == *parent_run_id && r.status.is_blocking())
        .ok_or_else(|| fail("parent run is not active"))?;
    let root = run
        .root_node_id
        .as_ref()
        .ok_or_else(|| fail("parent run has no root"))?;
    if task.trim().is_empty() || task.chars().count() > 120_000 {
        return Err(fail("delegated task is empty or too long"));
    }
    if !parent
        .nodes
        .iter()
        .any(|n| n.id == *parent_node_id && n.run_id.as_ref() == Some(&run.id))
    {
        return Err(fail("parent node is not part of the active run"));
    }
    let driver = capabilities::driver(&model_selection.instance_id)
        .ok_or_else(|| fail("unknown delegated provider"))?;
    let task_id =
        NodeId::new(format!("node:delegated:{}", command.command_id)).expect("derived id");
    let child_id =
        ThreadId::new(format!("thread:delegated:{}", command.command_id)).expect("derived id");
    let child_title = title.clone().unwrap_or_else(|| {
        task.trim()
            .lines()
            .next()
            .unwrap_or("Delegated task")
            .chars()
            .take(80)
            .collect()
    });
    let mut thread = parent.thread.clone();
    thread.id = child_id.clone();
    thread.title = child_title.clone();
    thread.model_selection = model_selection.clone();
    thread.provider_instance_id = model_selection.instance_id.clone();
    thread.created_by = *created_by;
    thread.creation_source = *creation_source;
    thread.runtime_mode = *runtime_mode;
    thread.interaction_mode = *interaction_mode;
    thread.lineage.parent_thread_id = Some(parent.thread.id.clone());
    thread.lineage.relationship_to_parent = Some(Relationship::Subagent);
    thread.forked_from = None;
    thread.active_provider_thread_id = None;
    thread.archived_at = None;
    thread.deleted_at = None;
    thread.settled_override = None;
    thread.settled_at = None;
    thread.unsettled_at = None;
    thread.snoozed_until = None;
    thread.snoozed_at = None;
    thread.pinned_at = None;
    thread.pin_order_key = None;
    thread.active_order_key = None;
    thread.last_visited_at = None;
    thread.imported = false;
    thread.rollback_request_id = None;
    thread.rollback_failure = None;
    thread.created_at = now.clone();
    thread.updated_at = now.clone();
    let prompt = task.clone();
    let task = Subagent {
        id: task_id.clone(),
        thread_id: parent.thread.id.clone(),
        run_id: Some(run.id.clone()),
        parent_node_id: parent_node_id.clone(),
        origin: SubagentOrigin::AppOwned,
        created_by: *created_by,
        driver,
        provider_instance_id: model_selection.instance_id.clone(),
        provider_thread_id: None,
        child_thread_id: Some(child_id.clone()),
        native_task_ref: None,
        prompt: task.clone(),
        title: title.clone(),
        model: Some(model_selection.model.clone()),
        completion_wake: *completion_wake,
        completion_delivery: None,
        status: NodeStatus::Running,
        progress: None,
        result: None,
        started_at: Some(now.clone()),
        completed_at: None,
        updated_at: now.clone(),
    };
    let node = ExecutionNode {
        id: task_id.clone(),
        thread_id: parent.thread.id.clone(),
        run_id: Some(run.id.clone()),
        parent_node_id: Some(parent_node_id.clone()),
        root_node_id: root.clone(),
        kind: NodeKind::Subagent,
        status: NodeStatus::Running,
        counts_for_run: false,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(now.clone()),
        completed_at: None,
    };
    let item = TurnItem {
        id: TurnItemId::new(format!("item:delegated:{}", command.command_id)).expect("derived id"),
        thread_id: parent.thread.id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(task_id.clone()),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: parent
            .nodes
            .iter()
            .find(|n| &n.id == root)
            .and_then(|n| n.provider_turn_id.clone()),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: parent
            .turn_items
            .iter()
            .map(|i| i.ordinal)
            .max()
            .unwrap_or(0)
            + 1,
        status: ItemStatus::Running,
        title: Some(child_title),
        started_at: Some(now.clone()),
        completed_at: None,
        updated_at: now.clone(),
        body: TurnItemBody::Subagent {
            subagent_id: task_id,
            origin: SubagentOrigin::AppOwned,
            driver,
            provider_instance_id: model_selection.instance_id.clone(),
            child_thread_id: Some(child_id.clone()),
            prompt: task.prompt.clone(),
            progress: None,
            result: None,
        },
    };
    let mut owner_run = run.clone();
    if owner_run.delegated_completion.is_none() {
        owner_run.delegated_completion = Some(DelegatedCompletionCohort {
            disposition: CohortDisposition::Open,
            next_generation: 1,
            delivery: None,
        });
    }
    if let Some(cohort) = &mut owner_run.delegated_completion
        && cohort.disposition != CohortDisposition::Open
    {
        cohort.disposition = CohortDisposition::Open;
        cohort.delivery = None;
    }
    let mut decision = Decision {
        events: events(
            &parent.thread.id,
            command.command_id.as_str(),
            vec![
                EventPayload::RunUpdated(owner_run),
                EventPayload::NodeUpdated(node),
                EventPayload::SubagentUpdated(task),
                EventPayload::TurnItemUpdated(item),
            ],
            now,
        ),
        ..Default::default()
    };
    decision.events.extend(events(
        &child_id,
        &format!("{}:child", command.command_id),
        vec![EventPayload::ThreadCreated(thread.clone())],
        now,
    ));
    let child = ThreadProjection::empty(thread);
    let input = Command {
        command_id: CommandId::new(format!("{}:input", command.command_id)).expect("derived id"),
        thread_id: child_id.clone(),
        body: CommandBody::MessageDispatch(
            MessageDispatch {
                native_continuation: None,
                delegated_completion: None,
                source_plan_ref: None,
                created_by: *created_by,
                creation_source: *creation_source,
                message_id: MessageId::new(format!("message:delegated:{}", command.command_id))
                    .expect("derived id"),
                text: prompt,
                context: None,
                attachments: vec![],
                model_selection: Some(model_selection.clone()),
                delivery_intent: None,
                dispatch_mode: DispatchMode::StartImmediately,
            }
            .into(),
        ),
    };
    let input_decision = decider::decide(
        &input,
        Some(&child),
        now,
        &capabilities::capabilities(driver).turns,
        driver,
    )?;
    let child = apply(&child, &input_decision.events);
    merge(&mut decision, input_decision);
    let transfer = ContextTransfer {
        id: ContextTransferId::new(format!("transfer:delegated:{}", command.command_id))
            .expect("derived id"),
        kind: TransferKind::SubagentSpawn,
        source_thread_id: parent.thread.id.clone(),
        target_thread_id: child_id.clone(),
        source_point: context::point(parent, run),
        base_point: None,
        source_provider_instance_id: Some(run.provider_instance_id.clone()),
        target_provider_instance_id: Some(model_selection.instance_id.clone()),
        target_run_id: Some(child.runs[0].id.clone()),
        status: TransferStatus::Consumed,
        resolution: None,
        created_by: *created_by,
        error: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        consumed_at: Some(now.clone()),
    };
    decision.events.extend(events(
        &child_id,
        &format!("{}:spawn", command.command_id),
        vec![EventPayload::ContextTransferCreated(transfer)],
        now,
    ));
    Ok(decision)
}

pub fn update(
    command: &Command,
    projection: &ThreadProjection,
    now: &Timestamp,
) -> Result<Decision, DecisionError> {
    let id = match &command.body {
        CommandBody::DelegatedTaskWakePolicy { task_id, .. }
        | CommandBody::DelegatedTaskAcknowledge { task_id, .. }
        | CommandBody::DelegatedTaskDispose { task_id } => task_id,
        CommandBody::NotificationDeliveryAccept { message_id } => {
            let completion = projection
                .messages
                .iter()
                .find(|m| m.id == *message_id)
                .and_then(|m| m.delegated_completion.as_ref())
                .ok_or_else(|| fail("notification delivery missing"))?;
            let payloads = projection
                .subagents
                .iter()
                .filter(|t| completion.task_ids.contains(&t.id))
                .filter(|t| {
                    t.completion_delivery
                        .as_ref()
                        .is_some_and(|d| d.state == DelegatedDeliveryState::Claimed)
                })
                .map(|t| {
                    let mut t = t.clone();
                    t.completion_delivery = Some(DelegatedTaskDelivery {
                        state: DelegatedDeliveryState::Delivered,
                        observed_by_run_id: None,
                    });
                    t.updated_at = now.clone();
                    EventPayload::SubagentUpdated(t)
                })
                .collect();
            return Ok(Decision {
                events: events(
                    &projection.thread.id,
                    command.command_id.as_str(),
                    payloads,
                    now,
                ),
                ..Default::default()
            });
        }
        _ => return Err(fail("not a task update")),
    };
    let mut task = projection
        .subagents
        .iter()
        .find(|t| t.id == *id && t.origin == SubagentOrigin::AppOwned)
        .ok_or_else(|| fail("app-owned task missing"))?
        .clone();
    match &command.body {
        CommandBody::DelegatedTaskWakePolicy {
            completion_wake, ..
        } => task.completion_wake = *completion_wake,
        CommandBody::DelegatedTaskAcknowledge {
            observed_by_run_id, ..
        } => {
            if task
                .completion_delivery
                .as_ref()
                .is_none_or(|d| d.state != DelegatedDeliveryState::Disposed)
            {
                task.completion_delivery = Some(DelegatedTaskDelivery {
                    state: DelegatedDeliveryState::Acknowledged,
                    observed_by_run_id: observed_by_run_id.clone(),
                });
            }
        }
        CommandBody::DelegatedTaskDispose { .. } => {
            task.completion_delivery = Some(DelegatedTaskDelivery {
                state: DelegatedDeliveryState::Disposed,
                observed_by_run_id: None,
            })
        }
        _ => unreachable!(),
    }
    task.updated_at = now.clone();
    Ok(Decision {
        events: events(
            &projection.thread.id,
            command.command_id.as_str(),
            vec![EventPayload::SubagentUpdated(task)],
            now,
        ),
        ..Default::default()
    })
}

fn task_run_ids(child: &ThreadProjection) -> std::collections::BTreeSet<RunId> {
    let Some(original) = child.runs.iter().min_by_key(|r| r.ordinal) else {
        return Default::default();
    };
    let mut continuation_ids = std::collections::BTreeSet::from([original.id.clone()]);
    loop {
        let before = continuation_ids.len();
        for run in &child.runs {
            if child
                .messages
                .iter()
                .find(|m| m.id == run.user_message_id)
                .and_then(|m| m.delegated_completion.as_ref())
                .is_some_and(|d| continuation_ids.contains(&d.parent_run_id))
            {
                continuation_ids.insert(run.id.clone());
            }
        }
        if before == continuation_ids.len() {
            break;
        }
    }
    continuation_ids
}

pub fn finalize(
    parent: &ThreadProjection,
    children: &[ThreadProjection],
    now: &Timestamp,
    trigger: &str,
) -> Decision {
    let mut payloads = vec![];
    for task in parent
        .subagents
        .iter()
        .filter(|t| t.origin == SubagentOrigin::AppOwned)
    {
        let Some(child) = children
            .iter()
            .find(|c| Some(&c.thread.id) == task.child_thread_id.as_ref())
        else {
            continue;
        };
        if matches!(
            task.status,
            NodeStatus::Completed
                | NodeStatus::Failed
                | NodeStatus::Cancelled
                | NodeStatus::Interrupted
        ) {
            continue;
        }
        if child.runs.is_empty() {
            continue;
        }
        let continuation_ids = task_run_ids(child);
        let run = child
            .runs
            .iter()
            .filter(|r| continuation_ids.contains(&r.id))
            .max_by_key(|r| r.ordinal)
            .expect("original run");
        let nested = child.subagents.iter().any(|t| {
            t.run_id
                .as_ref()
                .is_some_and(|id| continuation_ids.contains(id))
                && matches!(
                    t.status,
                    NodeStatus::Pending
                        | NodeStatus::Running
                        | NodeStatus::Waiting
                        | NodeStatus::Idle
                )
        });
        let status = if run.status.is_blocking() || nested {
            NodeStatus::Running
        } else if run.status == RunStatus::Queued {
            NodeStatus::Idle
        } else {
            match run.status {
                RunStatus::Completed => NodeStatus::Completed,
                RunStatus::Waiting => NodeStatus::Waiting,
                RunStatus::Failed => NodeStatus::Failed,
                RunStatus::Cancelled => NodeStatus::Cancelled,
                _ => NodeStatus::Interrupted,
            }
        };
        if task.status == status {
            continue;
        }
        let mut task = task.clone();
        task.status = status;
        task.updated_at = now.clone();
        if matches!(
            status,
            NodeStatus::Completed
                | NodeStatus::Failed
                | NodeStatus::Cancelled
                | NodeStatus::Interrupted
        ) {
            task.completed_at = Some(now.clone());
            task.result = child
                .visible_turn_items
                .iter()
                .rev()
                .filter(|r| {
                    r.item
                        .run_id
                        .as_ref()
                        .is_some_and(|id| continuation_ids.contains(id))
                })
                .find_map(|r| match &r.item.body {
                    TurnItemBody::AssistantMessage { text, .. } => {
                        Some(text.chars().take(16_000).collect())
                    }
                    TurnItemBody::Error { failure, .. } => Some(failure.message.clone()),
                    _ => None,
                });
            let transfer = ContextTransfer {
                id: ContextTransferId::new(format!("transfer:delegated-result:{}", task.id))
                    .expect("derived id"),
                kind: TransferKind::SubagentResult,
                source_thread_id: child.thread.id.clone(),
                target_thread_id: parent.thread.id.clone(),
                source_point: context::point(child, run),
                base_point: None,
                source_provider_instance_id: Some(run.provider_instance_id.clone()),
                target_provider_instance_id: Some(parent.thread.provider_instance_id.clone()),
                target_run_id: task.run_id.clone(),
                status: TransferStatus::Consumed,
                resolution: None,
                created_by: CreatedBy::System,
                error: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                consumed_at: Some(now.clone()),
            };
            payloads.push(EventPayload::ContextTransferCreated(transfer));
            if task.completion_delivery.is_none() {
                task.completion_delivery = Some(DelegatedTaskDelivery {
                    state: DelegatedDeliveryState::Pending,
                    observed_by_run_id: None,
                });
            }
        }
        payloads.extend(task_events(task, parent, now));
    }
    Decision {
        events: events(
            &parent.thread.id,
            &format!("delegated:finalize:{trigger}"),
            payloads,
            now,
        ),
        ..Default::default()
    }
}

/// Reserve one delivery per cohort. Queued siblings amend that reservation;
/// siblings finishing during its live run wait for a successor generation.
pub fn offer(
    parent: &ThreadProjection,
    now: &Timestamp,
    trigger: &str,
) -> Result<Decision, DecisionError> {
    if parent.thread.deleted_at.is_some()
        || parent.thread.archived_at.is_some()
        || parent.thread.rollback_request_id.is_some()
    {
        return Ok(Decision::default());
    }
    let mut decision = Decision::default();
    let mut current = parent.clone();
    for source in parent
        .runs
        .iter()
        .filter(|r| r.delegated_completion.is_some())
    {
        let cohort = source.delegated_completion.as_ref().unwrap();
        if cohort.disposition != CohortDisposition::Open {
            continue;
        }
        let live = current.runs.iter().any(|r| r.status.is_blocking());
        let eligible: Vec<_> = current
            .subagents
            .iter()
            .filter(|t| {
                t.run_id.as_ref() == Some(&source.id)
                    && t.completion_delivery
                        .as_ref()
                        .is_some_and(|d| d.state == DelegatedDeliveryState::Pending)
                    && (t.completion_wake == CompletionWake::Always || !live)
            })
            .cloned()
            .collect();
        if eligible.is_empty() {
            continue;
        }
        let existing = cohort
            .delivery
            .as_ref()
            .and_then(|d| current.messages.iter().find(|m| m.id == d.message_id))
            .and_then(|m| m.run_id.as_ref())
            .and_then(|id| current.runs.iter().find(|r| r.id == *id));
        if existing.is_some_and(|r| r.status.is_blocking()) {
            continue;
        }
        let queued = existing.is_some_and(|r| r.status == RunStatus::Queued);
        let mut owner = source.clone();
        let mut cohort = cohort.clone();
        let generation = if queued {
            cohort.delivery.as_ref().unwrap().generation
        } else {
            cohort.next_generation
        };
        let message_id = if queued {
            cohort.delivery.as_ref().unwrap().message_id.clone()
        } else {
            MessageId::new(format!("message:delegated:{}:{generation}", source.id))
                .expect("derived id")
        };
        let mut ids = if queued {
            cohort.delivery.as_ref().unwrap().task_ids.clone()
        } else {
            vec![]
        };
        ids.extend(eligible.iter().map(|t| t.id.clone()));
        ids.sort();
        ids.dedup();
        let metadata = DelegatedCompletion {
            parent_run_id: source.id.clone(),
            generation,
            task_ids: ids.clone(),
        };
        let text = delivery_text(&current.subagents, &ids);
        let mut payloads = vec![];
        for mut task in eligible {
            task.completion_delivery = Some(DelegatedTaskDelivery {
                state: DelegatedDeliveryState::Claimed,
                observed_by_run_id: None,
            });
            task.updated_at = now.clone();
            payloads.push(EventPayload::SubagentUpdated(task));
        }
        if queued {
            let mut message = current
                .messages
                .iter()
                .find(|m| m.id == message_id)
                .unwrap()
                .clone();
            message.text = text;
            message.delegated_completion = Some(Box::new(metadata));
            message.updated_at = now.clone();
            payloads.push(EventPayload::MessageUpdated(message));
        } else {
            let command = Command {
                command_id: CommandId::new(format!("command:delegated:{}:{generation}", source.id))
                    .expect("derived id"),
                thread_id: current.thread.id.clone(),
                body: CommandBody::MessageDispatch(
                    MessageDispatch {
                        native_continuation: None,
                        delegated_completion: Some(Box::new(metadata)),
                        source_plan_ref: None,
                        created_by: CreatedBy::System,
                        creation_source: CreationSource::Server,
                        message_id: message_id.clone(),
                        text,
                        context: None,
                        attachments: vec![],
                        model_selection: None,
                        delivery_intent: if current.runs.iter().any(|r| {
                            r.status == RunStatus::Running
                                && current.provider_turns.iter().any(|t| {
                                    t.run_attempt_id.as_ref() == r.active_attempt_id.as_ref()
                                        && t.status == TurnStatus::Running
                                })
                        }) && !current
                            .runtime_requests
                            .iter()
                            .any(|r| matches!(r.status, RequestStatus::Pending))
                            && !current.messages.iter().any(|m| {
                                current
                                    .runs
                                    .iter()
                                    .any(|r| r.status.is_blocking() && r.user_message_id == m.id)
                                    && native_maintenance(&m.text, !m.attachments.is_empty())
                            }) {
                            Some(DeliveryIntent::Auto)
                        } else {
                            None
                        },
                        dispatch_mode: DispatchMode::QueueAfterActive,
                    }
                    .into(),
                ),
            };
            let driver = capabilities::driver(&current.thread.provider_instance_id)
                .ok_or_else(|| fail("unknown wake provider"))?;
            let next = decider::decide(
                &command,
                Some(&current),
                now,
                &capabilities::capabilities(driver).turns,
                driver,
            )?;
            current = apply(&current, &next.events);
            merge(&mut decision, next);
            cohort.next_generation = generation + 1;
        }
        cohort.delivery = Some(DelegatedDelivery {
            generation,
            message_id,
            task_ids: ids,
        });
        owner.delegated_completion = Some(cohort);
        payloads.push(EventPayload::RunUpdated(owner));
        let updates = events(
            &current.thread.id,
            &format!("delegated:reserve:{}:{generation}:{trigger}", source.id),
            payloads,
            now,
        );
        current = apply(&current, &updates);
        decision.events.extend(updates);
    }
    Ok(decision)
}
fn delivery_text(tasks: &[Subagent], ids: &[NodeId]) -> String {
    let mut text = String::from(
        "Delegated task results are available. Use task_status or thread_read to inspect the child threads before continuing.\n",
    );
    for task in tasks.iter().filter(|t| ids.contains(&t.id)) {
        text.push_str(&format!(
            "\nTask {}: {} ({})\n{}\n",
            task.id,
            task.title.as_deref().unwrap_or(&task.prompt),
            task.status.as_str(),
            task.result.as_deref().unwrap_or("No final output")
        ));
    }
    text.chars().take(32_000).collect()
}

/// An observation before a queued wake is delivered removes that task from
/// the reservation; a failed preparation releases its claims for a later offer.
pub fn repair(
    parent: &ThreadProjection,
    now: &Timestamp,
    trigger: &str,
) -> Result<Decision, DecisionError> {
    let mut decision = Decision::default();
    for run in &parent.runs {
        let Some(cohort) = &run.delegated_completion else {
            continue;
        };
        let Some(delivery) = &cohort.delivery else {
            continue;
        };
        let message = parent.messages.iter().find(|m| m.id == delivery.message_id);
        let delivery_run = message
            .and_then(|m| m.run_id.as_ref())
            .and_then(|id| parent.runs.iter().find(|r| r.id == *id));
        let unaccepted = delivery_run.is_some_and(|r| {
            matches!(
                r.status,
                RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted
            ) && !parent.provider_turns.iter().any(|t| {
                t.run_attempt_id.as_ref() == r.active_attempt_id.as_ref()
                    && t.native_turn_ref.is_some()
            })
        });
        let queued = delivery_run.is_some_and(|r| r.status == RunStatus::Queued);
        if !queued && !unaccepted {
            continue;
        }
        let ids: Vec<_> = delivery
            .task_ids
            .iter()
            .filter(|id| {
                parent.subagents.iter().any(|t| {
                    t.id == **id
                        && t.completion_delivery
                            .as_ref()
                            .is_some_and(|d| d.state == DelegatedDeliveryState::Claimed)
                })
            })
            .cloned()
            .collect();
        let mut payloads = vec![];
        if unaccepted {
            for task in parent.subagents.iter().filter(|t| ids.contains(&t.id)) {
                let mut task = task.clone();
                task.completion_delivery = Some(DelegatedTaskDelivery {
                    state: DelegatedDeliveryState::Pending,
                    observed_by_run_id: None,
                });
                task.updated_at = now.clone();
                payloads.push(EventPayload::SubagentUpdated(task));
            }
        }
        if ids == delivery.task_ids && !unaccepted {
            continue;
        }
        let mut owner = run.clone();
        let cohort = owner.delegated_completion.as_mut().unwrap();
        if ids.is_empty() || unaccepted {
            cohort.delivery = None;
            if queued {
                let command = Command {
                    command_id: CommandId::new(format!(
                        "command:delegated:cancel:{trigger}:{}",
                        run.id
                    ))
                    .expect("derived id"),
                    thread_id: parent.thread.id.clone(),
                    body: CommandBody::QueuedRunCancel {
                        run_id: delivery_run.unwrap().id.clone(),
                    },
                };
                let driver = capabilities::driver(&parent.thread.provider_instance_id)
                    .ok_or_else(|| fail("unknown provider"))?;
                merge(
                    &mut decision,
                    decider::decide(
                        &command,
                        Some(parent),
                        now,
                        &capabilities::capabilities(driver).turns,
                        driver,
                    )?,
                );
            }
        } else {
            cohort.delivery.as_mut().unwrap().task_ids = ids.clone();
            let mut message = message.unwrap().clone();
            message.text = delivery_text(&parent.subagents, &ids);
            message.delegated_completion.as_mut().unwrap().task_ids = ids;
            message.updated_at = now.clone();
            payloads.push(EventPayload::MessageUpdated(message));
        }
        payloads.push(EventPayload::RunUpdated(owner));
        decision.events.extend(events(
            &parent.thread.id,
            &format!("delegated:repair:{trigger}:{}", run.id),
            payloads,
            now,
        ));
    }
    Ok(decision)
}

/// Stop/deletion owns cancellation of app-owned children as well as the
/// mailbox. Native children are cancelled by their provider process owner.
pub fn cancel_children(
    parent: &ThreadProjection,
    children: &[ThreadProjection],
    now: &Timestamp,
    trigger: &str,
) -> Result<Decision, DecisionError> {
    let mut decision = Decision::default();
    let stopped: Vec<_> = parent
        .subagents
        .iter()
        .filter(|t| {
            t.origin == SubagentOrigin::AppOwned
                && (t
                    .completion_delivery
                    .as_ref()
                    .is_some_and(|d| d.state == DelegatedDeliveryState::Disposed)
                    || parent.thread.deleted_at.is_some()
                    || parent.runs.iter().any(|r| {
                        Some(&r.id) == t.run_id.as_ref()
                            && (r.status == RunStatus::RolledBack
                                || r.delegated_completion
                                    .as_ref()
                                    .is_some_and(|c| c.disposition != CohortDisposition::Open))
                    }))
        })
        .collect();
    for task in stopped {
        if task
            .completion_delivery
            .as_ref()
            .is_none_or(|d| d.state != DelegatedDeliveryState::Disposed)
        {
            let mut task = task.clone();
            task.completion_delivery = Some(DelegatedTaskDelivery {
                state: DelegatedDeliveryState::Disposed,
                observed_by_run_id: None,
            });
            task.updated_at = now.clone();
            decision.events.extend(events(
                &parent.thread.id,
                &format!("delegated:dispose:{trigger}:{}", task.id),
                vec![EventPayload::SubagentUpdated(task)],
                now,
            ));
        }
        let Some(child) = children
            .iter()
            .find(|c| Some(&c.thread.id) == task.child_thread_id.as_ref())
        else {
            continue;
        };
        let driver = capabilities::driver(&child.thread.provider_instance_id)
            .ok_or_else(|| fail("unknown child provider"))?;
        let ids = task_run_ids(child);
        let mut current = child.clone();
        for run in child.runs.iter().filter(|r| ids.contains(&r.id)) {
            if let Some(cohort) = &run.delegated_completion
                && cohort.disposition == CohortDisposition::Open
            {
                let mut run = run.clone();
                run.delegated_completion.as_mut().unwrap().disposition = CohortDisposition::Stopped;
                let updates = events(
                    &child.thread.id,
                    &format!("delegated:stop-cohort:{trigger}:{}", run.id),
                    vec![EventPayload::RunUpdated(run)],
                    now,
                );
                current = apply(&current, &updates);
                decision.events.extend(updates);
            }
            if !run.status.is_blocking() && run.status != RunStatus::Queued {
                continue;
            }
            let command = Command {
                command_id: CommandId::new(format!("command:delegated:stop:{trigger}:{}", run.id))
                    .expect("derived id"),
                thread_id: child.thread.id.clone(),
                body: if run.status == RunStatus::Queued {
                    CommandBody::QueuedRunCancel {
                        run_id: run.id.clone(),
                    }
                } else {
                    CommandBody::RunInterrupt {
                        run_id: run.id.clone(),
                        reason: Some("Parent task stopped".into()),
                        hold_queue: true,
                    }
                },
            };
            let next = decider::decide(
                &command,
                Some(&current),
                now,
                &capabilities::capabilities(driver).turns,
                driver,
            )?;
            current = apply(&current, &next.events);
            merge(&mut decision, next);
        }
    }
    Ok(decision)
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::*;
    use crate::{store::Store, test_support::*};
    fn setup() -> Store {
        let store = Store::memory().unwrap();
        dispatch(&store, &create());
        dispatch(&store, &send("parent", DispatchMode::StartImmediately));
        store
    }
    fn dispatch(store: &Store, command: &Command) {
        let driver = match &command.body {
            CommandBody::DelegatedTaskRequest(request) => {
                capabilities::driver(&request.model_selection.instance_id).unwrap()
            }
            _ => Driver::Codex,
        };
        store
            .dispatch(
                command,
                &now(),
                &capabilities::capabilities(driver).turns,
                driver,
            )
            .unwrap();
    }
    fn request_task(store: &Store, id: &str, wake: CompletionWake) -> Subagent {
        request_task_on(store, &create().thread_id, id, wake)
    }
    fn request_task_on(
        store: &Store,
        parent_id: &ThreadId,
        id: &str,
        wake: CompletionWake,
    ) -> Subagent {
        let parent = store.projection(parent_id).unwrap();
        let run = parent
            .runs
            .iter()
            .filter(|r| r.status.is_blocking())
            .max_by_key(|r| r.ordinal)
            .unwrap();
        let mut request = command(
            id,
            CommandBody::DelegatedTaskRequest(Box::new(DelegatedTaskRequest {
                parent_run_id: run.id.clone(),
                parent_node_id: run.root_node_id.clone().unwrap(),
                task: format!("Inspect {id}"),
                title: Some(id.into()),
                model_selection: parent.thread.model_selection.clone(),
                runtime_mode: RuntimeMode::ApprovalRequired,
                interaction_mode: InteractionMode::Default,
                completion_wake: wake,
                created_by: CreatedBy::Agent,
                creation_source: CreationSource::Mcp,
            })),
        );
        request.thread_id = parent.thread.id.clone();
        dispatch(store, &request);
        store
            .projection(&parent.thread.id)
            .unwrap()
            .subagents
            .iter()
            .find(|s| s.title.as_deref() == Some(id))
            .unwrap()
            .clone()
    }
    fn finish(store: &Store, id: &ThreadId, status: RunStatus) {
        let p = store.projection(id).unwrap();
        let mut run = p
            .runs
            .iter()
            .filter(|r| r.status.is_blocking())
            .max_by_key(|r| r.ordinal)
            .unwrap()
            .clone();
        let guard = run.clone();
        run.status = status;
        run.completed_at = Some(now());
        run.delegated_completion = None;
        store
            .ingest(
                events(
                    id,
                    &format!("finish:{}:{status:?}", run.id),
                    vec![EventPayload::RunUpdated(run)],
                    &now(),
                ),
                Some((&guard.id, guard.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
    }
    #[test]
    fn delegation_is_atomic_idempotent_and_validates_parent_node_ownership() {
        let store = setup();
        let task = request_task(&store, "one", CompletionWake::Always);
        let p = store.projection(&create().thread_id).unwrap();
        assert_eq!(p.subagents.len(), 1);
        assert!(
            !p.nodes
                .iter()
                .find(|n| n.id == task.id)
                .unwrap()
                .counts_for_run
        );
        let child = store
            .projection(task.child_thread_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            child.thread.lineage.relationship_to_parent,
            Some(Relationship::Subagent)
        );
        assert_eq!(child.thread.runtime_mode, RuntimeMode::ApprovalRequired);
        assert_eq!(child.runs.len(), 1);
        assert_eq!(child.context_transfers[0].status, TransferStatus::Consumed);
        let same = Command {
            command_id: CommandId::new("one").unwrap(),
            thread_id: p.thread.id.clone(),
            body: CommandBody::DelegatedTaskDispose {
                task_id: task.id.clone(),
            },
        };
        // The receipt is replayed, and cannot create a second child.
        assert!(
            store
                .dispatch(&same, &now(), &turns(), Driver::Codex)
                .unwrap()
                .replayed
        );
        let invalid = command(
            "foreign",
            CommandBody::DelegatedTaskRequest(Box::new(DelegatedTaskRequest {
                parent_run_id: p.runs[0].id.clone(),
                parent_node_id: NodeId::new("foreign").unwrap(),
                task: "task".into(),
                title: None,
                model_selection: p.thread.model_selection.clone(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                completion_wake: CompletionWake::Always,
                created_by: CreatedBy::Agent,
                creation_source: CreationSource::Mcp,
            })),
        );
        assert!(
            store
                .dispatch(&invalid, &now(), &turns(), Driver::Codex)
                .is_err()
        );
        assert_eq!(store.shell_snapshot().unwrap().threads.len(), 2);
    }
    #[test]
    fn siblings_amend_one_prioritized_queued_wake_and_observation_removes_it() {
        let store = setup();
        let one = request_task(&store, "one", CompletionWake::Always);
        let two = request_task(&store, "two", CompletionWake::Always);
        dispatch(&store, &send("user-queued", DispatchMode::QueueAfterActive));
        finish(
            &store,
            one.child_thread_id.as_ref().unwrap(),
            RunStatus::Completed,
        );
        finish(
            &store,
            two.child_thread_id.as_ref().unwrap(),
            RunStatus::Completed,
        );
        let p = store.projection(&create().thread_id).unwrap();
        let messages: Vec<_> = p
            .messages
            .iter()
            .filter(|m| m.delegated_completion.is_some())
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]
                .delegated_completion
                .as_ref()
                .unwrap()
                .task_ids
                .len(),
            2
        );
        let queue = queued_run_order::queued_runs_in_delivery_order(&p.runs, &p.messages);
        assert_eq!(queue[0].user_message_id, messages[0].id);
        dispatch(
            &store,
            &command(
                "observe-one",
                CommandBody::DelegatedTaskAcknowledge {
                    task_id: one.id.clone(),
                    observed_by_run_id: Some(p.runs[0].id.clone()),
                },
            ),
        );
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(
            p.messages
                .iter()
                .find(|m| m.id == messages[0].id)
                .unwrap()
                .delegated_completion
                .as_ref()
                .unwrap()
                .task_ids,
            vec![two.id.clone()]
        );
        dispatch(
            &store,
            &command(
                "dispose-two",
                CommandBody::DelegatedTaskDispose { task_id: two.id },
            ),
        );
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(
            p.runs
                .iter()
                .find(|r| r.user_message_id == messages[0].id)
                .unwrap()
                .status,
            RunStatus::Cancelled
        );
        assert!(
            p.runs[0]
                .delegated_completion
                .as_ref()
                .unwrap()
                .delivery
                .is_none()
        );
    }
    #[test]
    fn settled_only_waits_and_accepted_delivery_never_reoffers() {
        let store = setup();
        let task = request_task(&store, "waiting", CompletionWake::SettledOnly);
        finish(
            &store,
            task.child_thread_id.as_ref().unwrap(),
            RunStatus::Completed,
        );
        assert!(
            !store
                .projection(&create().thread_id)
                .unwrap()
                .messages
                .iter()
                .any(|m| m.delegated_completion.is_some())
        );
        finish(&store, &create().thread_id, RunStatus::Completed);
        let p = store.projection(&create().thread_id).unwrap();
        assert!(
            p.runs[0].delegated_completion.is_some(),
            "stale adapter lifecycle must retain cohort"
        );
        let wake = p
            .messages
            .iter()
            .find(|m| m.delegated_completion.is_some())
            .unwrap()
            .id
            .clone();
        dispatch(
            &store,
            &command(
                "accepted",
                CommandBody::NotificationDeliveryAccept { message_id: wake },
            ),
        );
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(
            p.subagents[0].completion_delivery.as_ref().unwrap().state,
            DelegatedDeliveryState::Delivered
        );
        dispatch(
            &store,
            &command(
                "upgrade",
                CommandBody::DelegatedTaskWakePolicy {
                    task_id: task.id,
                    completion_wake: CompletionWake::Always,
                },
            ),
        );
        assert_eq!(
            store
                .projection(&p.thread.id)
                .unwrap()
                .messages
                .iter()
                .filter(|m| m.delegated_completion.is_some())
                .count(),
            1
        );
    }
    #[test]
    fn stop_cancels_children_and_recovery_does_not_restart_mailbox_work() {
        let store = setup();
        let task = request_task(&store, "stop", CompletionWake::Always);
        let p = store.projection(&create().thread_id).unwrap();
        dispatch(
            &store,
            &command(
                "stop-parent",
                CommandBody::RunInterrupt {
                    run_id: p.runs[0].id.clone(),
                    reason: None,
                    hold_queue: true,
                },
            ),
        );
        let child = store
            .projection(task.child_thread_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(child.runs[0].status, RunStatus::Interrupted);
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(
            p.runs[0].delegated_completion.as_ref().unwrap().disposition,
            CohortDisposition::Stopped
        );
        assert_eq!(
            p.subagents[0].completion_delivery.as_ref().unwrap().state,
            DelegatedDeliveryState::Disposed
        );
        let fresh = setup();
        let task = request_task(&fresh, "recovery", CompletionWake::Always);
        fresh.recover(&now()).unwrap();
        let p = fresh.projection(&create().thread_id).unwrap();
        assert_eq!(p.subagents[0].status, NodeStatus::Interrupted);
        assert!(!p.messages.iter().any(|m| m.delegated_completion.is_some()));
        assert_eq!(
            fresh
                .projection(task.child_thread_id.as_ref().unwrap())
                .unwrap()
                .runs[0]
                .status,
            RunStatus::Interrupted
        );
    }
    #[test]
    fn nested_work_waits_for_its_continuation_and_published_result_stays_stable() {
        let store = setup();
        let one = request_task(&store, "nested-parent", CompletionWake::SettledOnly);
        let child_id = one.child_thread_id.as_ref().unwrap();
        let nested = request_task_on(&store, child_id, "nested-child", CompletionWake::Always);
        finish(&store, child_id, RunStatus::Completed);
        assert_eq!(
            store.projection(&create().thread_id).unwrap().subagents[0].status,
            NodeStatus::Running
        );
        finish(
            &store,
            nested.child_thread_id.as_ref().unwrap(),
            RunStatus::Completed,
        );
        let child = store.projection(child_id).unwrap();
        assert_eq!(child.runs.len(), 2);
        assert!(child.runs[1].status.is_blocking());
        finish(&store, child_id, RunStatus::Completed);
        let parent = store.projection(&create().thread_id).unwrap();
        assert_eq!(parent.subagents[0].status, NodeStatus::Completed);
        let result = parent
            .context_transfers
            .iter()
            .find(|t| t.kind == TransferKind::SubagentResult)
            .unwrap()
            .clone();
        assert_eq!(result.source_point.run_id.as_ref(), Some(&child.runs[1].id));
        let mut later = send("later-ordinary", DispatchMode::StartImmediately);
        later.thread_id = child_id.clone();
        dispatch(&store, &later);
        dispatch(
            &store,
            &command(
                "cancel-terminal-task",
                CommandBody::DelegatedTaskDispose { task_id: one.id },
            ),
        );
        assert!(
            store
                .projection(child_id)
                .unwrap()
                .runs
                .iter()
                .max_by_key(|r| r.ordinal)
                .unwrap()
                .status
                .is_blocking(),
            "terminal task cancellation must not stop later ordinary turns"
        );
        finish(&store, child_id, RunStatus::Failed);
        let parent = store.projection(&create().thread_id).unwrap();
        assert_eq!(parent.subagents[0].status, NodeStatus::Completed);
        assert_eq!(
            *parent
                .context_transfers
                .iter()
                .find(|t| t.id == result.id)
                .unwrap(),
            result
        );
    }
    #[test]
    fn cancelling_a_task_stops_its_nested_delegations_in_the_same_transaction() {
        let store = setup();
        let one = request_task(&store, "stop-nested-parent", CompletionWake::Always);
        let nested = request_task_on(
            &store,
            one.child_thread_id.as_ref().unwrap(),
            "stop-nested-child",
            CompletionWake::Always,
        );
        dispatch(
            &store,
            &command(
                "dispose-tree",
                CommandBody::DelegatedTaskDispose { task_id: one.id },
            ),
        );
        assert_eq!(
            store
                .projection(nested.child_thread_id.as_ref().unwrap())
                .unwrap()
                .runs[0]
                .status,
            RunStatus::Interrupted
        );
        let child = store
            .projection(one.child_thread_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            child.subagents[0]
                .completion_delivery
                .as_ref()
                .unwrap()
                .state,
            DelegatedDeliveryState::Disposed
        );
        assert!(
            child
                .messages
                .iter()
                .all(|m| m.delegated_completion.is_none())
        );
    }
    #[test]
    fn a_failed_wake_releases_claims_without_spinning_up_another_run() {
        let store = setup();
        let one = request_task(&store, "failed-wake", CompletionWake::Always);
        finish(
            &store,
            one.child_thread_id.as_ref().unwrap(),
            RunStatus::Completed,
        );
        finish(&store, &create().thread_id, RunStatus::Completed);
        finish(&store, &create().thread_id, RunStatus::Failed);
        let parent = store.projection(&create().thread_id).unwrap();
        assert_eq!(parent.runs.len(), 2);
        assert_eq!(
            parent.subagents[0]
                .completion_delivery
                .as_ref()
                .unwrap()
                .state,
            DelegatedDeliveryState::Pending
        );
        assert!(
            parent.runs[0]
                .delegated_completion
                .as_ref()
                .unwrap()
                .delivery
                .is_none()
        );
    }
}
