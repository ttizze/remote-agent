//! Pure command planning. Time, ids and provider capabilities are explicit inputs.
use crate::contracts::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DecisionError(pub String);
fn require(condition: bool, message: &str) -> Result<(), DecisionError> {
    if condition {
        Ok(())
    } else {
        Err(DecisionError(message.into()))
    }
}
fn emit(decision: &mut Decision, command: &Command, now: &Timestamp, payload: EventPayload) {
    decision.events.push(DomainEvent {
        id: EventId::new(format!(
            "event:{}:{}",
            command.command_id,
            decision.events.len()
        ))
        .expect("derived id"),
        thread_id: command.thread_id.clone(),
        occurred_at: now.clone(),
        payload,
    });
}
fn effect(decision: &mut Decision, command: &Command, body: EffectBody) {
    decision.effects.push(Effect {
        id: format!("effect:{}:{}", command.command_id, decision.effects.len()),
        thread_id: command.thread_id.clone(),
        body,
    });
}
fn run<'a>(runs: &'a [Run], id: &RunId) -> Result<&'a Run, DecisionError> {
    runs.iter()
        .find(|run| run.id == *id)
        .ok_or_else(|| DecisionError("run not found".into()))
}
fn active(runs: &[Run]) -> Option<&Run> {
    runs.iter()
        .filter(|run| run.status.is_blocking())
        .max_by_key(|run| run.ordinal)
}
pub fn interruptible_run<'a>(runs: &'a [Run], tasks: &[Subagent]) -> Option<&'a Run> {
    active(runs).or_else(|| {
        runs.iter()
            .filter(|r| {
                r.status == RunStatus::Completed
                    && tasks.iter().any(|t| {
                        t.run_id.as_ref() == Some(&r.id)
                            && matches!(
                                t.status,
                                NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                            )
                    })
            })
            .max_by_key(|r| r.ordinal)
    })
}
pub fn editable_message(native_continuation: bool, delegated_completion: bool) -> bool {
    !native_continuation && !delegated_completion
}
pub fn queued_runs<'a>(runs: &'a [Run], messages: &[ConversationMessage]) -> Vec<&'a Run> {
    let mut queue: Vec<_> = runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .collect();
    queue.sort_by_key(|run| {
        (
            !messages
                .iter()
                .any(|m| m.id == run.user_message_id && m.delegated_completion.is_some()),
            run.queue_position.unwrap_or(run.ordinal),
            run.ordinal,
        )
    });
    queue
}

pub fn resolve_dispatch_mode(
    requested: &DispatchMode,
    intent: Option<DeliveryIntent>,
    active_run: Option<&Run>,
    turns: &TurnCapabilities,
) -> DispatchMode {
    let Some(intent) = intent else {
        return requested.clone();
    };
    let Some(run) = active_run else {
        return DispatchMode::StartImmediately;
    };
    match intent {
        DeliveryIntent::Steer => DispatchMode::SteerActive {
            target_run_id: run.id.clone(),
        },
        DeliveryIntent::Restart => DispatchMode::RestartActive {
            target_run_id: run.id.clone(),
        },
        DeliveryIntent::Auto
            if matches!(run.status, RunStatus::Preparing | RunStatus::Starting) =>
        {
            DispatchMode::QueueAfterActive
        }
        DeliveryIntent::Auto if turns.supports_active_steering => DispatchMode::SteerActive {
            target_run_id: run.id.clone(),
        },
        DeliveryIntent::Auto if turns.supports_queued_messages => DispatchMode::QueueAfterActive,
        DeliveryIntent::Auto if turns.supports_steering_by_interrupt_restart => {
            DispatchMode::RestartActive {
                target_run_id: run.id.clone(),
            }
        }
        DeliveryIntent::Auto => DispatchMode::QueueAfterActive,
    }
}

pub fn decide(
    command: &Command,
    projection: Option<&ThreadProjection>,
    now: &Timestamp,
    capabilities: &TurnCapabilities,
    driver: Driver,
) -> Result<Decision, DecisionError> {
    if let Some(projection) = projection {
        if matches!(command.body, CommandBody::DelegatedTaskRequest(_)) {
            return crate::delegation::request(command, projection, now);
        }
        if matches!(
            command.body,
            CommandBody::DelegatedTaskWakePolicy { .. }
                | CommandBody::DelegatedTaskAcknowledge { .. }
                | CommandBody::DelegatedTaskDispose { .. }
                | CommandBody::NotificationDeliveryAccept { .. }
        ) {
            return crate::delegation::update(command, projection, now);
        }
    }
    let mut decision = Decision::default();
    if let CommandBody::ThreadCreate {
        created_by,
        creation_source,
        project_id,
        title,
        model_selection,
        runtime_mode,
        interaction_mode,
        branch,
        worktree_path,
    } = &command.body
    {
        require(projection.is_none(), "thread already exists")?;
        require(
            !title.trim().is_empty() && !model_selection.model.trim().is_empty(),
            "title and model are required",
        )?;
        let thread = AppThread {
            created_by: *created_by,
            creation_source: *creation_source,
            id: command.thread_id.clone(),
            project_id: project_id.clone(),
            title: title.trim().into(),
            provider_instance_id: model_selection.instance_id.clone(),
            model_selection: model_selection.clone(),
            runtime_mode: *runtime_mode,
            interaction_mode: *interaction_mode,
            branch: branch.clone(),
            worktree_path: worktree_path.clone(),
            active_provider_thread_id: None,
            lineage: Lineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: command.thread_id.clone(),
            },
            forked_from: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            last_visited_at: None,
            deleted_at: None,
            imported: false,
            rollback_request_id: None,
            rollback_failure: None,
        };
        emit(
            &mut decision,
            command,
            now,
            EventPayload::ThreadCreated(thread),
        );
        return Ok(decision);
    }
    let projection = projection.ok_or_else(|| DecisionError("thread not found".into()))?;
    require(
        projection.thread.id == command.thread_id && projection.thread.deleted_at.is_none(),
        "thread not found",
    )?;
    let mut thread = projection.thread.clone();
    use CommandBody::*;
    match &command.body {
        ThreadCreate { .. } => unreachable!(),
        ThreadFork { .. } | ThreadMergeBack { .. } => {
            return Err(DecisionError(
                "cross-thread command requires related projection".into(),
            ));
        }
        CheckpointRollback {
            scope_id,
            checkpoint_id,
            restore_files,
        } => {
            return crate::rollback::request(
                command,
                projection,
                scope_id,
                checkpoint_id,
                *restore_files,
                now,
            );
        }
        MessageDispatch(message) => dispatch(
            &mut decision,
            command,
            projection,
            message,
            now,
            capabilities,
            driver,
            InputIntent::Steer,
        )?,
        RunInterrupt {
            run_id,
            reason,
            hold_queue,
        } => {
            let target = run(&projection.runs, run_id)?;
            let background = !target.status.is_blocking()
                && target.status == RunStatus::Completed
                && projection.subagents.iter().any(|t| {
                    t.run_id.as_ref() == Some(run_id)
                        && matches!(
                            t.status,
                            NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                        )
                });
            require(
                target.status.is_blocking() || background,
                "run is not interruptible",
            )?;
            require(
                capabilities.supports_interrupt,
                "provider does not support interrupts",
            )?;
            let text = reason
                .clone()
                .unwrap_or_else(|| "Interrupt requested".into());
            emit(
                &mut decision,
                command,
                now,
                EventPayload::TurnItemUpdated(notice_item(
                    command,
                    target,
                    now,
                    TurnItemBody::RunInterruptRequest { message: text },
                    "interrupt-request",
                    next_ordinal(projection),
                )),
            );
            if *hold_queue {
                for queued in queued_runs(&projection.runs, &projection.messages) {
                    let mut queued = queued.clone();
                    queued.queue_held = true;
                    emit(
                        &mut decision,
                        command,
                        now,
                        EventPayload::RunUpdated(queued),
                    );
                }
            }
            if background {
                let mut owner = target.clone();
                owner
                    .delegated_completion
                    .get_or_insert(DelegatedCompletionCohort {
                        disposition: CohortDisposition::Stopped,
                        next_generation: 1,
                        delivery: None,
                    })
                    .disposition = CohortDisposition::Stopped;
                emit(&mut decision, command, now, EventPayload::RunUpdated(owner));
                if projection.subagents.iter().any(|t| {
                    t.run_id.as_ref() == Some(run_id)
                        && t.origin == SubagentOrigin::ProviderNative
                        && matches!(
                            t.status,
                            NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                        )
                }) && let Some(turn) = projection
                    .provider_turns
                    .iter()
                    .find(|t| t.run_attempt_id == target.active_attempt_id)
                {
                    effect(
                        &mut decision,
                        command,
                        EffectBody::Interrupt {
                            run_id: run_id.clone(),
                            provider_turn_id: turn.id.clone(),
                        },
                    );
                }
            } else if let Some(turn) = projection.provider_turns.iter().find(|turn| {
                turn.run_attempt_id == target.active_attempt_id
                    && matches!(turn.status, TurnStatus::Pending | TurnStatus::Running)
            }) {
                effect(
                    &mut decision,
                    command,
                    EffectBody::Interrupt {
                        run_id: target.id.clone(),
                        provider_turn_id: turn.id.clone(),
                    },
                );
            } else {
                terminalize(
                    &mut decision,
                    command,
                    target,
                    &projection.attempts,
                    &projection.nodes,
                    now,
                    RunStatus::Interrupted,
                );
                decision.cancel_unsettled_effects = true;
                emit(
                    &mut decision,
                    command,
                    now,
                    EventPayload::TurnItemUpdated(notice_item(
                        command,
                        target,
                        now,
                        TurnItemBody::RunInterruptResult {
                            message: "Interrupted before provider start".into(),
                        },
                        "interrupt-result",
                        next_ordinal(projection) + 1,
                    )),
                );
                if !hold_queue {
                    promote_next(
                        &mut decision,
                        command,
                        (&projection.runs, &projection.messages),
                        has_native_turn(&projection.provider_turns),
                        next_ordinal(projection),
                        now,
                        Some(run_id),
                    );
                }
            }
        }
        QueueResume => {
            require(thread.archived_at.is_none(), "thread is archived")?;
            let mut runs = projection.runs.clone();
            for queued in runs
                .iter_mut()
                .filter(|run| run.status == RunStatus::Queued)
            {
                queued.queue_held = false;
                emit(
                    &mut decision,
                    command,
                    now,
                    EventPayload::RunUpdated(queued.clone()),
                );
            }
            if active(&runs).is_none() {
                promote_next(
                    &mut decision,
                    command,
                    (&runs, &projection.messages),
                    has_native_turn(&projection.provider_turns),
                    next_ordinal(projection),
                    now,
                    None,
                );
            }
            if decision.events.is_empty() {
                thread.updated_at = now.clone();
                emit(
                    &mut decision,
                    command,
                    now,
                    EventPayload::ThreadMetadataUpdated(thread),
                );
            }
        }
        QueuedRunReorder {
            run_id,
            before_run_id,
        } => {
            require(
                run(&projection.runs, run_id)?.status == RunStatus::Queued,
                "run is not queued",
            )?;
            require(
                before_run_id.as_ref() != Some(run_id),
                "cannot reorder run before itself",
            )?;
            require(
                !projection.messages.iter().any(|m| {
                    m.id == run(&projection.runs, run_id)
                        .expect("known queued run")
                        .user_message_id
                        && m.delegated_completion.is_some()
                }),
                "delegated completion cannot be reordered",
            )?;
            if let Some(before) = before_run_id {
                require(
                    !projection.messages.iter().any(|m| {
                        projection
                            .runs
                            .iter()
                            .any(|r| &r.id == before && r.user_message_id == m.id)
                            && m.delegated_completion.is_some()
                    }),
                    "cannot reorder before a delegated completion",
                )?;
            }
            let mut queue = queued_runs(&projection.runs, &projection.messages);
            queue.retain(|candidate| candidate.id != *run_id);
            let position = match before_run_id {
                Some(id) => queue
                    .iter()
                    .position(|run| run.id == *id)
                    .ok_or_else(|| DecisionError("destination is not queued".into()))?,
                None => queue.len(),
            };
            queue.insert(position, run(&projection.runs, run_id)?);
            for (position, run) in queue.into_iter().enumerate() {
                let mut run = run.clone();
                run.queue_position = Some(position as u64 + 1);
                emit(&mut decision, command, now, EventPayload::RunUpdated(run));
            }
        }
        QueuedRunCancel { run_id } => {
            let target = run(&projection.runs, run_id)?;
            require(target.status == RunStatus::Queued, "run is not queued")?;
            terminalize(
                &mut decision,
                command,
                target,
                &projection.attempts,
                &projection.nodes,
                now,
                RunStatus::Cancelled,
            );
        }
        QueuedRunEdit {
            run_id,
            text,
            context,
            attachments,
        } => {
            let target = run(&projection.runs, run_id)?;
            require(target.status == RunStatus::Queued, "run is not queued")?;
            require(!text.trim().is_empty(), "queued message cannot be empty")?;
            let mut message = projection
                .messages
                .iter()
                .find(|message| message.id == target.user_message_id)
                .cloned()
                .ok_or_else(|| DecisionError("queued message not found".into()))?;
            require(
                editable_message(
                    message.native_continuation.is_some(),
                    message.delegated_completion.is_some(),
                ),
                "notifications cannot be edited",
            )?;
            message.text = text.clone();
            message.context = context.clone();
            message.updated_at = now.clone();
            if let Some(attachments) = attachments {
                message.attachments = attachments.clone();
            }
            emit(
                &mut decision,
                command,
                now,
                EventPayload::MessageUpdated(message.clone()),
            );
            for item in &projection.turn_items {
                if matches!(&item.body, TurnItemBody::UserMessage { message_id, .. } if *message_id == message.id)
                {
                    let mut item = item.clone();
                    item.updated_at = now.clone();
                    if let TurnItemBody::UserMessage {
                        text,
                        context,
                        attachments,
                        ..
                    } = &mut item.body
                    {
                        *text = message.text.clone();
                        *context = message.context.clone();
                        *attachments = message.attachments.clone();
                    }
                    emit(
                        &mut decision,
                        command,
                        now,
                        EventPayload::TurnItemUpdated(item),
                    );
                }
            }
        }
        QueuedMessagePromoteToSteer {
            queued_run_id,
            target_run_id,
        } => {
            let queued = run(&projection.runs, queued_run_id)?;
            require(queued.status == RunStatus::Queued, "run is not queued")?;
            let message = projection
                .messages
                .iter()
                .find(|message| message.id == queued.user_message_id)
                .ok_or_else(|| DecisionError("queued message not found".into()))?;
            require(
                message.native_continuation.is_none(),
                "native continuations cannot be promoted to steer",
            )?;
            let input = crate::MessageDispatch {
                native_continuation: message.native_continuation.clone(),
                delegated_completion: message.delegated_completion.clone(),
                source_plan_ref: None,
                created_by: message.created_by,
                creation_source: message.creation_source,
                message_id: message.id.clone(),
                text: message.text.clone(),
                context: message.context.clone(),
                attachments: message.attachments.clone(),
                model_selection: None,
                delivery_intent: None,
                dispatch_mode: DispatchMode::SteerActive {
                    target_run_id: target_run_id.clone(),
                },
            };
            terminalize(
                &mut decision,
                command,
                queued,
                &projection.attempts,
                &projection.nodes,
                now,
                RunStatus::Cancelled,
            );
            dispatch(
                &mut decision,
                command,
                projection,
                &input,
                now,
                capabilities,
                driver,
                InputIntent::PromotedQueuedToSteer,
            )?;
        }
        RuntimeRequestRespond {
            request_id,
            decision: response,
            answers,
        } => respond(
            &mut decision,
            command,
            projection,
            request_id,
            *response,
            answers.as_ref(),
            now,
            capabilities,
            driver,
        )?,
        ThreadUserInputDismiss { request_id } => {
            let request = projection
                .runtime_requests
                .iter()
                .find(|request| request.id == *request_id)
                .ok_or_else(|| DecisionError("request not found".into()))?;
            require(
                request.kind == RequestKind::UserInput
                    && request.response_capability == ResponseCapability::Message,
                "this question needs an answer or a turn interrupt",
            )?;
            respond(
                &mut decision,
                command,
                projection,
                request_id,
                Some(ApprovalDecision::Cancel),
                None,
                now,
                capabilities,
                driver,
            )?;
        }
        PreparedRunRelease { run_id } => {
            let mut target = run(&projection.runs, run_id)?.clone();
            require(
                target.status == RunStatus::Preparing,
                "run is not preparing",
            )?;
            target.status = RunStatus::Starting;
            target.started_at = Some(now.clone());
            emit(
                &mut decision,
                command,
                now,
                EventPayload::RunUpdated(target),
            );
            effect(
                &mut decision,
                command,
                EffectBody::Start {
                    run_id: run_id.clone(),
                },
            );
        }
        PreparedRunFail { run_id, failure } => {
            let target = run(&projection.runs, run_id)?;
            require(
                target.status == RunStatus::Preparing,
                "run is not preparing",
            )?;
            terminalize(
                &mut decision,
                command,
                target,
                &projection.attempts,
                &projection.nodes,
                now,
                RunStatus::Failed,
            );
            emit(
                &mut decision,
                command,
                now,
                EventPayload::TurnItemUpdated(notice_item(
                    command,
                    target,
                    now,
                    TurnItemBody::Error {
                        failure: failure.clone(),
                        retry: None,
                    },
                    "preparation-error",
                    next_ordinal(projection),
                )),
            );
            promote_next(
                &mut decision,
                command,
                (&projection.runs, &projection.messages),
                has_native_turn(&projection.provider_turns),
                next_ordinal(projection),
                now,
                Some(run_id),
            );
        }
        PreparedRunRetry { run_id } => {
            require(active(&projection.runs).is_none(), "another run is active")?;
            let mut target = run(&projection.runs, run_id)?.clone();
            require(
                target.status == RunStatus::Failed && target.workspace_preparation.is_some(),
                "run cannot retry preparation",
            )?;
            target.status = RunStatus::Preparing;
            target.completed_at = None;
            emit(
                &mut decision,
                command,
                now,
                EventPayload::RunUpdated(target),
            );
        }
        ProviderSessionDetach {
            provider_session_id,
        } => {
            require(
                projection
                    .provider_sessions
                    .iter()
                    .any(|session| session.id == *provider_session_id),
                "session not found",
            )?;
            emit(
                &mut decision,
                command,
                now,
                EventPayload::ProviderSessionDetached(provider_session_id.clone()),
            );
            effect(
                &mut decision,
                command,
                EffectBody::Detach {
                    driver: projection
                        .provider_sessions
                        .iter()
                        .find(|session| session.id == *provider_session_id)
                        .expect("session validated")
                        .driver,
                    provider_session_id: provider_session_id.clone(),
                },
            );
        }
        _ => {
            if matches!(
                command.body,
                ThreadSettle { .. }
                    | ThreadUnsettle
                    | ThreadSnooze { .. }
                    | ThreadUnsnooze
                    | ThreadAutoSettleSet { .. }
                    | ThreadPin { .. }
                    | ThreadUnpin
                    | ThreadPinReorder { .. }
                    | ThreadActiveReorder { .. }
            ) {
                require(thread.archived_at.is_none(), "thread is archived")?;
            }
            let payload = match &command.body {
                ThreadArchive => {
                    require(thread.archived_at.is_none(), "thread already archived")?;
                    thread.archived_at = Some(now.clone());
                    thread.updated_at = now.clone();
                    EventPayload::ThreadArchived(thread)
                }
                ThreadUnarchive => {
                    require(thread.archived_at.is_some(), "thread not archived")?;
                    thread.archived_at = None;
                    thread.updated_at = now.clone();
                    EventPayload::ThreadUnarchived(thread)
                }
                ThreadDelete => {
                    require(
                        active(&projection.runs).is_none(),
                        "stop active work before deleting",
                    )?;
                    thread.deleted_at = Some(now.clone());
                    thread.updated_at = now.clone();
                    decision.cancel_unsettled_effects = true;
                    for session in &projection.provider_sessions {
                        emit(
                            &mut decision,
                            command,
                            now,
                            EventPayload::ProviderSessionDetached(session.id.clone()),
                        );
                        effect(
                            &mut decision,
                            command,
                            EffectBody::Detach {
                                provider_session_id: session.id.clone(),
                                driver: session.driver,
                            },
                        );
                    }
                    effect(&mut decision, command, EffectBody::TerminalCleanup);
                    effect(&mut decision, command, EffectBody::AttachmentCleanup);
                    EventPayload::ThreadDeleted(thread)
                }
                ThreadSettle { settled_at } => {
                    require(
                        !projection
                            .runs
                            .iter()
                            .any(|run| run.status.is_blocking() || run.status == RunStatus::Queued),
                        "active or queued work cannot be settled",
                    )?;
                    require(
                        !projection.runtime_requests.iter().any(|request| {
                            request.status == RequestStatus::Pending
                                && (request.kind != RequestKind::UserInput
                                    || request.response_capability != ResponseCapability::Message)
                        }),
                        "pending request prevents settling",
                    )?;
                    for request in projection
                        .runtime_requests
                        .iter()
                        .filter(|request| request.status == RequestStatus::Pending)
                    {
                        respond(
                            &mut decision,
                            command,
                            projection,
                            &request.id,
                            Some(ApprovalDecision::Cancel),
                            None,
                            now,
                            capabilities,
                            driver,
                        )?;
                    }
                    let already_settled = thread.settled_override == Some(SettledOverride::Settled)
                        && thread.settled_at.is_some()
                        && thread.pinned_at.is_none();
                    thread.settled_override = Some(SettledOverride::Settled);
                    if !already_settled {
                        thread.settled_at = Some(settled_at.clone().unwrap_or_else(|| now.clone()));
                        thread.updated_at = now.clone();
                    }
                    thread.unsettled_at = None;
                    thread.pinned_at = None;
                    thread.pin_order_key = None;
                    thread.active_order_key = None;
                    EventPayload::ThreadSettled(thread)
                }
                ThreadUnsettle => {
                    thread.settled_override = Some(SettledOverride::Active);
                    thread.settled_at = None;
                    thread.unsettled_at = Some(now.clone());
                    thread.updated_at = now.clone();
                    EventPayload::ThreadUnsettled(thread)
                }
                ThreadSnooze { snoozed_until } => {
                    require(
                        snoozed_until > now,
                        "snooze wake time must be in the future",
                    )?;
                    require(
                        !projection
                            .runtime_requests
                            .iter()
                            .any(|request| request.status == RequestStatus::Pending)
                            && queued_runs(&projection.runs, &projection.messages).is_empty(),
                        "pending or queued work cannot be snoozed",
                    )?;
                    thread.snoozed_until = Some(snoozed_until.clone());
                    thread.snoozed_at = Some(now.clone());
                    thread.updated_at = now.clone();
                    EventPayload::ThreadSnoozed(thread)
                }
                ThreadUnsnooze => {
                    thread.snoozed_until = None;
                    thread.snoozed_at = None;
                    thread.updated_at = now.clone();
                    EventPayload::ThreadUnsnoozed(thread)
                }
                ThreadAutoSettleSet { enabled } => {
                    thread.auto_settle_disabled_at =
                        if *enabled { None } else { Some(now.clone()) };
                    thread.updated_at = now.clone();
                    EventPayload::ThreadAutoSettleSet(thread)
                }
                ThreadPin { order_key } => {
                    let promotes = thread.settled_override == Some(SettledOverride::Settled)
                        || thread.snoozed_until.is_some();
                    if thread.pinned_at.is_none() {
                        thread.pinned_at = Some(now.clone());
                        thread.pin_order_key = order_key.clone();
                        thread.updated_at = now.clone();
                    }
                    if promotes {
                        thread.updated_at = now.clone();
                    }
                    if thread.settled_override == Some(SettledOverride::Settled) {
                        thread.settled_override = Some(SettledOverride::Active);
                        thread.settled_at = None;
                    }
                    thread.snoozed_until = None;
                    thread.snoozed_at = None;
                    EventPayload::ThreadPinned(thread)
                }
                ThreadUnpin => {
                    thread.pinned_at = None;
                    thread.pin_order_key = None;
                    thread.updated_at = now.clone();
                    EventPayload::ThreadUnpinned(thread)
                }
                ThreadPinReorder { order_key } => {
                    require(thread.pinned_at.is_some(), "thread is not pinned")?;
                    require(!order_key.is_empty(), "order key is required")?;
                    thread.pin_order_key = Some(order_key.clone());
                    thread.updated_at = now.clone();
                    EventPayload::ThreadPinReordered(thread)
                }
                ThreadActiveReorder { order_key } => {
                    require(
                        thread.pinned_at.is_none()
                            && thread.settled_override != Some(SettledOverride::Settled),
                        "thread is not active",
                    )?;
                    require(!order_key.is_empty(), "order key is required")?;
                    thread.active_order_key = Some(order_key.clone());
                    thread.updated_at = now.clone();
                    EventPayload::ThreadActiveReordered(thread)
                }
                ThreadVisit { visited_at } => {
                    thread.last_visited_at = Some(
                        thread
                            .last_visited_at
                            .as_ref()
                            .map_or(visited_at, |old| old.max(visited_at))
                            .clone(),
                    );
                    EventPayload::ThreadVisited(thread)
                }
                ThreadMarkUnread => {
                    let completed = projection
                        .runs
                        .iter()
                        .filter(|run| run.completed_at.is_some())
                        .max_by_key(|run| run.ordinal)
                        .and_then(|run| run.completed_at.as_ref())
                        .ok_or_else(|| DecisionError("no completed run to mark unread".into()))?;
                    let millis = chrono::DateTime::parse_from_rfc3339(completed.as_str())
                        .expect("validated time")
                        .timestamp_millis()
                        - 1;
                    thread.last_visited_at = Some(
                        Timestamp::from_millis(millis).map_err(|e| DecisionError(e.to_string()))?,
                    );
                    EventPayload::ThreadMarkedUnread(thread)
                }
                ThreadMetadataUpdate { title } => {
                    require(!title.trim().is_empty(), "title is required")?;
                    thread.title = title.trim().into();
                    thread.updated_at = now.clone();
                    EventPayload::ThreadMetadataUpdated(thread)
                }
                ThreadRuntimeModeSet { runtime_mode } => {
                    thread.runtime_mode = *runtime_mode;
                    thread.updated_at = now.clone();
                    EventPayload::ThreadRuntimeModeUpdated(thread)
                }
                ThreadInteractionModeSet { interaction_mode } => {
                    thread.interaction_mode = *interaction_mode;
                    thread.updated_at = now.clone();
                    EventPayload::ThreadInteractionModeUpdated(thread)
                }
                ThreadModelSelectionSet { model_selection }
                | ProviderSwitch { model_selection } => {
                    require(
                        !model_selection.model.trim().is_empty(),
                        "model is required",
                    )?;
                    thread.provider_instance_id = model_selection.instance_id.clone();
                    thread.model_selection = model_selection.clone();
                    thread.updated_at = now.clone();
                    if matches!(command.body, ProviderSwitch { .. }) {
                        EventPayload::ThreadProviderSwitched(thread)
                    } else {
                        EventPayload::ThreadModelSelectionUpdated(thread)
                    }
                }
                _ => unreachable!(),
            };
            emit(&mut decision, command, now, payload);
        }
    }
    if let CommandBody::RunInterrupt { run_id, .. } = &command.body {
        for event in &mut decision.events {
            if let EventPayload::RunUpdated(run) = &mut event.payload
                && run.id == *run_id
                && let Some(cohort) = &mut run.delegated_completion
            {
                cohort.disposition = CohortDisposition::Stopped;
            }
        }
        if !decision
            .events
            .iter()
            .any(|e| matches!(&e.payload,EventPayload::RunUpdated(r) if r.id == *run_id))
            && let Some(run) = projection
                .runs
                .iter()
                .find(|r| r.id == *run_id && r.delegated_completion.is_some())
        {
            let mut run = run.clone();
            run.delegated_completion.as_mut().unwrap().disposition = CohortDisposition::Stopped;
            emit(&mut decision, command, now, EventPayload::RunUpdated(run));
        }
    }
    let capture = crate::checkpoint::await_capture(
        std::mem::take(&mut decision.events),
        &projection.checkpoint_scopes,
    );
    decision.events = capture.events;
    // Capture must precede a promoted Start in the per-thread outbox.
    decision.effects.splice(0..0, capture.effects);
    require(!decision.events.is_empty(), "command must produce an event")?;
    Ok(decision)
}

fn next_ordinal(projection: &ThreadProjection) -> u64 {
    projection
        .turn_items
        .iter()
        .map(|item| item.ordinal)
        .max()
        .unwrap_or(0)
        + 1
}
fn running_turn<'a>(projection: &'a ThreadProjection, target: &Run) -> Option<&'a ProviderTurn> {
    target.active_attempt_id.as_ref().and_then(|id| {
        projection.provider_turns.iter().find(|turn| {
            turn.run_attempt_id.as_ref() == Some(id) && turn.status == TurnStatus::Running
        })
    })
}
fn notice_item(
    command: &Command,
    target: &Run,
    now: &Timestamp,
    body: TurnItemBody,
    suffix: &str,
    ordinal: u64,
) -> TurnItem {
    TurnItem {
        id: TurnItemId::new(format!("item:{}:{suffix}", command.command_id)).expect("derived id"),
        thread_id: command.thread_id.clone(),
        run_id: Some(target.id.clone()),
        node_id: target.root_node_id.clone(),
        provider_thread_id: target.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: ItemStatus::Completed,
        title: None,
        started_at: Some(now.clone()),
        completed_at: Some(now.clone()),
        updated_at: now.clone(),
        body,
    }
}
fn terminalize(
    decision: &mut Decision,
    command: &Command,
    target: &Run,
    attempts: &[RunAttempt],
    nodes: &[ExecutionNode],
    now: &Timestamp,
    status: RunStatus,
) {
    let mut run = target.clone();
    run.status = status;
    run.completed_at = Some(now.clone());
    run.queue_position = None;
    emit(decision, command, now, EventPayload::RunUpdated(run));
    if let Some(id) = &target.active_attempt_id
        && let Some(attempt) = attempts.iter().find(|attempt| attempt.id == *id)
    {
        let mut attempt = attempt.clone();
        attempt.status = match status {
            RunStatus::Interrupted => AttemptStatus::Interrupted,
            RunStatus::Failed => AttemptStatus::Failed,
            _ => AttemptStatus::Cancelled,
        };
        attempt.completed_at = Some(now.clone());
        emit(
            decision,
            command,
            now,
            EventPayload::RunAttemptUpdated(attempt),
        );
    }
    if let Some(id) = &target.root_node_id {
        for node in finish_nodes(
            id,
            nodes,
            match status {
                RunStatus::Interrupted => NodeStatus::Interrupted,
                RunStatus::Failed => NodeStatus::Failed,
                _ => NodeStatus::Cancelled,
            },
            now,
        ) {
            emit(decision, command, now, EventPayload::NodeUpdated(node));
        }
    }
}
fn finish_nodes(
    root: &NodeId,
    nodes: &[ExecutionNode],
    status: NodeStatus,
    now: &Timestamp,
) -> Vec<ExecutionNode> {
    nodes
        .iter()
        .filter(|node| {
            node.root_node_id == *root
                && matches!(
                    node.status,
                    NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                )
        })
        .map(|node| {
            let mut node = node.clone();
            node.status = status;
            node.completed_at = Some(now.clone());
            node
        })
        .collect()
}
fn finish_provider_turn(
    turn: &ProviderTurn,
    requests: &[RuntimeRequest],
    items: &[TurnItem],
    now: &Timestamp,
    status: TurnStatus,
) -> Vec<EventPayload> {
    let mut result = vec![];
    for request in requests.iter().filter(|request| {
        request.provider_turn_id.as_ref() == Some(&turn.id)
            && request.status == RequestStatus::Pending
    }) {
        let mut request = request.clone();
        request.status = RequestStatus::Cancelled;
        request.resolved_at = Some(now.clone());
        result.push(EventPayload::RuntimeRequestUpdated(request));
    }
    for item in items.iter().filter(|item| {
        item.provider_turn_id.as_ref() == Some(&turn.id)
            && matches!(
                item.status,
                ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting
            )
    }) {
        let status = if matches!(
            item.body,
            TurnItemBody::ApprovalRequest { .. } | TurnItemBody::UserInputRequest { .. }
        ) {
            ItemStatus::Cancelled
        } else if status == TurnStatus::Interrupted {
            ItemStatus::Interrupted
        } else {
            ItemStatus::Failed
        };
        result.push(EventPayload::TurnItemUpdated(
            crate::projector::finished_item(item, status, now),
        ));
    }
    let mut turn = turn.clone();
    turn.status = status;
    turn.completed_at = Some(now.clone());
    result.push(EventPayload::ProviderTurnUpdated(turn));
    result
}
/// A permanent effect failure settles every record owned by the current attempt.
pub fn failed_effect(
    projection: &ThreadProjection,
    target: &Run,
    effect_id: &str,
    message: &str,
    now: &Timestamp,
) -> Decision {
    let command = Command {
        command_id: CommandId::new(format!("effect-failed:{effect_id}")).expect("derived id"),
        thread_id: projection.thread.id.clone(),
        body: CommandBody::PreparedRunFail {
            run_id: target.id.clone(),
            failure: ProviderFailure {
                class: FailureClass::ProviderError,
                message: message.into(),
                code: None,
                retryable: Some(false),
                reset_at: None,
            },
        },
    };
    let CommandBody::PreparedRunFail { failure, .. } = &command.body else {
        unreachable!()
    };
    let mut decision = Decision::default();
    terminalize(
        &mut decision,
        &command,
        target,
        &projection.attempts,
        &projection.nodes,
        now,
        RunStatus::Failed,
    );
    if let Some(turn) = projection
        .provider_turns
        .iter()
        .find(|turn| turn.run_attempt_id.as_ref() == target.active_attempt_id.as_ref())
    {
        for payload in finish_provider_turn(
            turn,
            &projection.runtime_requests,
            &projection.turn_items,
            now,
            TurnStatus::Failed,
        ) {
            emit(&mut decision, &command, now, payload);
        }
    }
    emit(
        &mut decision,
        &command,
        now,
        EventPayload::TurnItemUpdated(notice_item(
            &command,
            target,
            now,
            TurnItemBody::Error {
                failure: failure.clone(),
                retry: None,
            },
            "error",
            next_ordinal(projection),
        )),
    );
    decision
}
/// Native child histories have no application run; input waits for their real turn.
pub fn has_native_turn(turns: &[ProviderTurn]) -> bool {
    turns.iter().any(|t| {
        t.run_attempt_id.is_none() && matches!(t.status, TurnStatus::Pending | TurnStatus::Running)
    })
}
fn promote_next(
    decision: &mut Decision,
    command: &Command,
    queue: (&[Run], &[ConversationMessage]),
    native_busy: bool,
    ordinal: u64,
    now: &Timestamp,
    excluding: Option<&RunId>,
) {
    let (runs, messages) = queue;
    if native_busy || active(runs).is_some_and(|run| Some(&run.id) != excluding) {
        return;
    }
    if let Some(next) = queued_runs(runs, messages)
        .first()
        .filter(|run| !run.queue_held)
    {
        let mut run = (*next).clone();
        run.ordinal = runs
            .iter()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        run.status = RunStatus::Starting;
        run.started_at = Some(now.clone());
        run.queue_position = None;
        effect(
            decision,
            command,
            EffectBody::Start {
                run_id: run.id.clone(),
            },
        );
        if let Some(message) = messages.iter().find(|m| m.id == run.user_message_id) {
            emit(
                decision,
                command,
                now,
                EventPayload::TurnItemUpdated(user_item(
                    message,
                    &run,
                    InputIntent::QueuedTurn,
                    ordinal,
                    None,
                    now,
                )),
            );
        }
        emit(decision, command, now, EventPayload::RunUpdated(run));
    }
}

fn user_item(
    message: &ConversationMessage,
    run: &Run,
    input_intent: InputIntent,
    ordinal: u64,
    provider_turn_id: Option<ProviderTurnId>,
    now: &Timestamp,
) -> TurnItem {
    TurnItem {
        id: TurnItemId::new(format!("item:user:{}", message.id)).expect("derived id"),
        thread_id: message.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone(),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: ItemStatus::Completed,
        title: None,
        started_at: Some(now.clone()),
        completed_at: Some(now.clone()),
        updated_at: now.clone(),
        body: if message.native_continuation.is_some() || message.delegated_completion.is_some() {
            TurnItemBody::Notification {
                source: if message.native_continuation.is_some() {
                    "background_task"
                } else {
                    "delegated_task"
                }
                .into(),
                outcome: ItemStatus::Completed,
                summary: message.text.clone(),
                detail: None,
            }
        } else {
            TurnItemBody::UserMessage {
                created_by: message.created_by,
                creation_source: message.creation_source,
                message_id: message.id.clone(),
                input_intent,
                text: message.text.clone(),
                context: message.context.clone(),
                attachments: message.attachments.clone(),
            }
        },
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "pure planning receives explicit provider values and owns only its local event accumulator"
)]
fn dispatch(
    decision: &mut Decision,
    command: &Command,
    projection: &ThreadProjection,
    message: &MessageDispatch,
    now: &Timestamp,
    capabilities: &TurnCapabilities,
    driver: Driver,
    steer_intent: InputIntent,
) -> Result<(), DecisionError> {
    if let Some(source) = &message.native_continuation {
        require(
            message.created_by == CreatedBy::Agent
                && message.creation_source == CreationSource::Provider
                && message.attachments.is_empty()
                && message.context.is_none()
                && message.delegated_completion.is_none(),
            "invalid native continuation",
        )?;
        require(
            projection.thread.active_provider_thread_id.as_ref()
                == Some(&source.provider_thread_id)
                && projection.provider_threads.iter().any(|p| {
                    p.id == source.provider_thread_id
                        && p.provider_instance_id == projection.thread.provider_instance_id
                })
                && projection.runs.iter().any(|r| {
                    r.id == source.run_id
                        && r.active_attempt_id.as_ref() == Some(&source.attempt_id)
                        && r.provider_thread_id.as_ref() == Some(&source.provider_thread_id)
                        && (r.status.is_blocking() || r.status == RunStatus::Completed)
                        && r.delegated_completion
                            .as_ref()
                            .is_none_or(|c| c.disposition == CohortDisposition::Open)
                })
                && projection.subagents.iter().any(|t| {
                    t.id == source.task_id
                        && t.origin == SubagentOrigin::ProviderNative
                        && matches!(t.status, NodeStatus::Completed | NodeStatus::Failed)
                }),
            "native continuation is no longer owned",
        )?;
    }
    require(
        projection.thread.archived_at.is_none(),
        "thread is archived",
    )?;
    require(
        projection.thread.rollback_request_id.is_none(),
        "rollback is pending",
    )?;
    require(
        !message.text.trim().is_empty() || !message.attachments.is_empty(),
        "message cannot be empty",
    )?;
    require(
        message.attachments.len() <= 100
            && message.attachments.iter().all(|a| {
                a.size_bytes > 0
                    && a.size_bytes
                        <= match a.kind {
                            AttachmentKind::Image => 10 * 1024 * 1024,
                            AttachmentKind::File => 50 * 1024 * 1024,
                        }
            }),
        "invalid attachments",
    )?;
    require(
        message
            .context
            .as_ref()
            .is_none_or(|context| context.version == 1 && context.records.len() <= 200),
        "invalid context",
    )?;
    let active = active(&projection.runs);
    require(
        active.is_none()
            || !projection
                .context_transfers
                .iter()
                .any(|t| t.kind == TransferKind::MergeBack && t.status == TransferStatus::Pending),
        "wait for the active run before consuming merge back",
    )?;
    let mode = resolve_dispatch_mode(
        &message.dispatch_mode,
        message.delivery_intent,
        active,
        capabilities,
    );
    if matches!(
        mode,
        DispatchMode::SteerActive { .. } | DispatchMode::RestartActive { .. }
    ) {
        require(
            !crate::native_maintenance(&message.text, !message.attachments.is_empty()),
            "maintenance must run as a separate turn; queue it or wait",
        )?;
        require(
            active
                .and_then(|run| {
                    projection
                        .messages
                        .iter()
                        .find(|m| m.id == run.user_message_id)
                })
                .is_none_or(|m| !crate::native_maintenance(&m.text, !m.attachments.is_empty())),
            "wait for maintenance to finish before steering",
        )?;
    }
    require(
        !has_native_turn(&projection.provider_turns)
            || matches!(mode, DispatchMode::QueueAfterActive),
        "native child turn is active; queue input until it returns",
    )?;
    let target = match &mode {
        DispatchMode::SteerActive { target_run_id }
        | DispatchMode::RestartActive { target_run_id } => {
            let target = run(&projection.runs, target_run_id)?;
            if target.status.is_terminal() {
                None
            } else {
                require(
                    active.is_some_and(|run| run.id == *target_run_id),
                    "steer target is not active",
                )?;
                Some(target)
            }
        }
        _ => None,
    };
    let mut target = if let Some(target) = target {
        target.clone()
    } else {
        let ordinal = projection
            .runs
            .iter()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let model = message
            .model_selection
            .clone()
            .unwrap_or_else(|| projection.thread.model_selection.clone());
        let queue = active.is_some() || has_native_turn(&projection.provider_turns);
        if queue {
            require(
                capabilities.supports_queued_messages,
                "provider cannot queue messages",
            )?;
        }
        let preparing = !queue && matches!(mode, DispatchMode::DeferStart { .. });
        Run {
            delegated_completion: None,
            id: RunId::new(format!("run:{}", command.command_id)).expect("derived id"),
            thread_id: command.thread_id.clone(),
            ordinal,
            provider_instance_id: model.instance_id.clone(),
            model_selection: model,
            provider_thread_id: projection.thread.active_provider_thread_id.clone(),
            user_message_id: message.message_id.clone(),
            root_node_id: None,
            active_attempt_id: None,
            status: if queue {
                RunStatus::Queued
            } else if preparing {
                RunStatus::Preparing
            } else {
                RunStatus::Starting
            },
            queue_position: if queue {
                Some(
                    projection
                        .runs
                        .iter()
                        .filter(|run| run.status == RunStatus::Queued)
                        .map(|run| run.queue_position.unwrap_or(run.ordinal))
                        .max()
                        .unwrap_or(0)
                        + 1,
                )
            } else {
                None
            },
            queue_held: queue
                && projection
                    .runs
                    .iter()
                    .any(|run| run.status == RunStatus::Queued && run.queue_held),
            requested_at: now.clone(),
            started_at: None,
            completed_at: None,
            checkpoint_id: None,
            context_handoff_id: None,
            source_plan_ref: message.source_plan_ref.clone(),
            workspace_preparation: match &mode {
                DispatchMode::DeferStart { workspace_strategy } => workspace_strategy.clone(),
                _ => None,
            },
        }
    };
    let steering = target.status.is_blocking()
        && matches!(
            mode,
            DispatchMode::SteerActive { .. } | DispatchMode::RestartActive { .. }
        )
        && run(&projection.runs, &target.id).is_ok();
    require(
        steer_intent == InputIntent::PromotedQueuedToSteer
            || !projection
                .messages
                .iter()
                .any(|old| old.id == message.message_id),
        "message id already exists",
    )?;
    if !steering {
        let provider_thread = projection
            .provider_threads
            .iter()
            .find(|thread| {
                thread.provider_instance_id == target.provider_instance_id
                    && thread.owner_node_id.is_none()
            })
            .cloned()
            .unwrap_or_else(|| ProviderThread {
                id: ProviderThreadId::new(format!("provider-thread:pending:{}", target.id))
                    .expect("derived id"),
                driver,
                provider_instance_id: target.provider_instance_id.clone(),
                provider_session_id: None,
                app_thread_id: Some(command.thread_id.clone()),
                owner_node_id: None,
                native_thread_ref: None,
                native_conversation_head_ref: None,
                status: ProviderThreadStatus::NotLoaded,
                first_run_ordinal: None,
                last_run_ordinal: None,
                handoff_ids: vec![],
                forked_from: None,
                pending_background_tasks: vec![],
                context_usage: None,
                native_metadata: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            });
        target.provider_thread_id = Some(provider_thread.id.clone());
        let root_node_id = NodeId::new(format!("node:{}:root", target.id)).expect("derived id");
        let attempt_id = RunAttemptId::new(format!("attempt:{}:1", target.id)).expect("derived id");
        target.root_node_id = Some(root_node_id.clone());
        target.active_attempt_id = Some(attempt_id.clone());
        emit(
            decision,
            command,
            now,
            EventPayload::ProviderThreadUpdated(provider_thread.clone()),
        );
        emit(
            decision,
            command,
            now,
            EventPayload::RunAttemptCreated(RunAttempt {
                id: attempt_id,
                run_id: target.id.clone(),
                attempt_ordinal: 1,
                root_node_id: root_node_id.clone(),
                provider_instance_id: target.provider_instance_id.clone(),
                provider_thread_id: provider_thread.id.clone(),
                provider_turn_id: None,
                reason: AttemptReason::Initial,
                status: AttemptStatus::Pending,
                started_at: None,
                completed_at: None,
            }),
        );
        emit(
            decision,
            command,
            now,
            EventPayload::NodeUpdated(ExecutionNode {
                id: root_node_id.clone(),
                thread_id: command.thread_id.clone(),
                run_id: Some(target.id.clone()),
                parent_node_id: None,
                root_node_id,
                kind: NodeKind::RootTurn,
                status: NodeStatus::Pending,
                counts_for_run: true,
                provider_thread_id: Some(provider_thread.id),
                provider_turn_id: None,
                native_item_ref: None,
                runtime_request_id: None,
                checkpoint_scope_id: None,
                started_at: None,
                completed_at: None,
            }),
        );
    }
    let input_intent = if steering {
        steer_intent
    } else if target.status == RunStatus::Queued {
        InputIntent::QueuedTurn
    } else {
        InputIntent::TurnStart
    };
    let conversation = ConversationMessage {
        native_continuation: message.native_continuation.clone(),
        delegated_completion: message.delegated_completion.clone(),
        created_by: message.created_by,
        creation_source: message.creation_source,
        id: message.message_id.clone(),
        thread_id: command.thread_id.clone(),
        run_id: Some(target.id.clone()),
        node_id: target.root_node_id.clone(),
        role: Role::User,
        text: message.text.clone(),
        context: message.context.clone(),
        attachments: message.attachments.clone(),
        streaming: false,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let item = (target.status != RunStatus::Queued).then(|| {
        user_item(
            &conversation,
            &target,
            input_intent,
            next_ordinal(projection),
            running_turn(projection, &target).map(|t| t.id.clone()),
            now,
        )
    });
    if steering {
        let turn = running_turn(projection, &target)
            .ok_or_else(|| DecisionError("no running provider turn to steer".into()))?;
        let restart = matches!(mode, DispatchMode::RestartActive { .. })
            || !capabilities.supports_active_steering;
        if restart {
            require(
                capabilities.supports_interrupt
                    && capabilities.supports_steering_by_interrupt_restart,
                "provider cannot restart for steering",
            )?;
            if let Some(attempt) = projection
                .attempts
                .iter()
                .find(|attempt| Some(&attempt.id) == target.active_attempt_id.as_ref())
            {
                let mut attempt = attempt.clone();
                attempt.status = AttemptStatus::Superseded;
                attempt.completed_at = Some(now.clone());
                emit(
                    decision,
                    command,
                    now,
                    EventPayload::RunAttemptUpdated(attempt),
                );
            }
            for payload in finish_provider_turn(
                turn,
                &projection.runtime_requests,
                &projection.turn_items,
                now,
                TurnStatus::Interrupted,
            ) {
                emit(decision, command, now, payload);
            }
            for node in finish_nodes(
                &turn.node_id,
                &projection.nodes,
                NodeStatus::Interrupted,
                now,
            ) {
                emit(decision, command, now, EventPayload::NodeUpdated(node));
            }
            let attempt_id = RunAttemptId::new(format!("attempt:{}:restart", command.command_id))
                .expect("derived id");
            let root_node_id =
                NodeId::new(format!("node:{}:restart", command.command_id)).expect("derived id");
            let attempt = RunAttempt {
                id: attempt_id.clone(),
                run_id: target.id.clone(),
                attempt_ordinal: projection
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.run_id == target.id)
                    .map(|attempt| attempt.attempt_ordinal)
                    .max()
                    .unwrap_or(0)
                    + 1,
                root_node_id: root_node_id.clone(),
                provider_instance_id: target.provider_instance_id.clone(),
                provider_thread_id: turn.provider_thread_id.clone(),
                provider_turn_id: None,
                reason: AttemptReason::SteeringRestart,
                status: AttemptStatus::Pending,
                started_at: None,
                completed_at: None,
            };
            if let Some(cohort) = &mut target.delegated_completion {
                cohort.disposition = CohortDisposition::Stopped;
            }
            target.active_attempt_id = Some(attempt_id.clone());
            target.root_node_id = Some(root_node_id.clone());
            target.status = RunStatus::Starting;
            emit(
                decision,
                command,
                now,
                EventPayload::RunAttemptCreated(attempt),
            );
            emit(
                decision,
                command,
                now,
                EventPayload::NodeUpdated(ExecutionNode {
                    id: root_node_id.clone(),
                    thread_id: command.thread_id.clone(),
                    run_id: Some(target.id.clone()),
                    parent_node_id: None,
                    root_node_id,
                    kind: NodeKind::RootTurn,
                    status: NodeStatus::Pending,
                    counts_for_run: true,
                    provider_thread_id: target.provider_thread_id.clone(),
                    provider_turn_id: None,
                    native_item_ref: None,
                    runtime_request_id: None,
                    checkpoint_scope_id: None,
                    started_at: None,
                    completed_at: None,
                }),
            );
            emit(
                decision,
                command,
                now,
                EventPayload::RunUpdated(target.clone()),
            );
            effect(
                decision,
                command,
                EffectBody::Restart {
                    run_id: target.id.clone(),
                    provider_turn_id: turn.id.clone(),
                    message_id: message.message_id.clone(),
                    attempt_id,
                },
            );
        } else {
            effect(
                decision,
                command,
                EffectBody::Steer {
                    run_id: target.id.clone(),
                    provider_turn_id: turn.id.clone(),
                    message_id: message.message_id.clone(),
                },
            );
        }
    } else {
        emit(
            decision,
            command,
            now,
            EventPayload::RunCreated(target.clone()),
        );
        if target.status == RunStatus::Starting {
            effect(
                decision,
                command,
                EffectBody::Start {
                    run_id: target.id.clone(),
                },
            );
        }
    }
    emit(
        decision,
        command,
        now,
        EventPayload::MessageUpdated(conversation),
    );
    if let Some(item) = item {
        emit(decision, command, now, EventPayload::TurnItemUpdated(item));
    }
    let mut thread = projection.thread.clone();
    thread.settled_override = Some(SettledOverride::Active);
    thread.settled_at = None;
    thread.snoozed_until = None;
    thread.snoozed_at = None;
    thread.updated_at = now.clone();
    emit(
        decision,
        command,
        now,
        EventPayload::ThreadMetadataUpdated(thread),
    );
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "response planning uses explicit response and provider values without a mutable runtime context"
)]
fn respond(
    decision: &mut Decision,
    command: &Command,
    projection: &ThreadProjection,
    request_id: &RuntimeRequestId,
    response: Option<ApprovalDecision>,
    answers: Option<&Answers>,
    now: &Timestamp,
    capabilities: &TurnCapabilities,
    driver: Driver,
) -> Result<(), DecisionError> {
    let mut request = projection
        .runtime_requests
        .iter()
        .find(|request| request.id == *request_id)
        .cloned()
        .ok_or_else(|| DecisionError("runtime request not found".into()))?;
    require(
        request.status == RequestStatus::Pending,
        "runtime request is no longer pending",
    )?;
    require(
        !matches!(
            request.response_capability,
            ResponseCapability::NotResumable { .. }
        ),
        "runtime request is not resumable",
    )?;
    if let ResponseCapability::Live {
        provider_session_id,
    } = &request.response_capability
    {
        require(
            projection
                .provider_sessions
                .iter()
                .any(|session| session.id == *provider_session_id),
            "live provider session not found",
        )?;
    }
    let cancelled = matches!(
        response,
        Some(ApprovalDecision::Decline | ApprovalDecision::Cancel)
    );
    require(
        cancelled || response.is_some() || answers.is_some(),
        "a response is required",
    )?;
    request.status = RequestStatus::Resolved;
    request.resolved_at = Some(now.clone());
    request.decision = response;
    request.answers = answers.cloned();
    emit(
        decision,
        command,
        now,
        EventPayload::RuntimeRequestUpdated(request.clone()),
    );
    if let Some(node) = projection
        .nodes
        .iter()
        .find(|node| node.id == request.node_id)
    {
        let mut node = node.clone();
        node.status = if cancelled {
            NodeStatus::Cancelled
        } else {
            NodeStatus::Completed
        };
        node.completed_at = Some(now.clone());
        emit(decision, command, now, EventPayload::NodeUpdated(node));
    }
    let source_item = projection.turn_items.iter().find(|item| match &item.body {
        TurnItemBody::ApprovalRequest { request_id: id, .. }
        | TurnItemBody::UserInputRequest { request_id: id, .. } => *id == *request_id,
        _ => false,
    });
    if let Some(item) = source_item {
        let mut item = item.clone();
        item.status = if cancelled {
            ItemStatus::Cancelled
        } else {
            ItemStatus::Completed
        };
        item.completed_at = Some(now.clone());
        item.updated_at = now.clone();
        if let TurnItemBody::UserInputRequest {
            question_answer, ..
        } = &mut item.body
        {
            *question_answer = answers.cloned();
        }
        emit(decision, command, now, EventPayload::TurnItemUpdated(item));
    }
    match request.response_capability {
        ResponseCapability::Live { .. } => effect(
            decision,
            command,
            EffectBody::Respond {
                request_id: request_id.clone(),
                decision: response,
                answers: answers.cloned(),
            },
        ),
        ResponseCapability::Message if !cancelled => {
            let Some(TurnItem {
                body: TurnItemBody::UserInputRequest { questions, .. },
                ..
            }) = source_item
            else {
                return Err(DecisionError("question not found".into()));
            };
            let mut replies = vec![];
            for question in questions {
                let answer = answers
                    .and_then(|answers| answers.get(&question.id))
                    .and_then(|answer| answer.0.as_str())
                    .map(str::trim)
                    .filter(|answer| !answer.is_empty());
                require(
                    answer.is_some() || !question.required,
                    "answer each question before sending",
                )?;
                if let Some(answer) = answer {
                    replies.push(format!("{}\n{answer}", question.question));
                }
            }
            require(!replies.is_empty(), "enter an answer before sending")?;
            let mode = active(&projection.runs)
                .filter(|target| {
                    running_turn(projection, target).is_some()
                        && capabilities.supports_active_steering
                })
                .map_or(DispatchMode::QueueAfterActive, |target| {
                    DispatchMode::SteerActive {
                        target_run_id: target.id.clone(),
                    }
                });
            let input = MessageDispatch {
                native_continuation: None,
                delegated_completion: None,
                source_plan_ref: None,
                created_by: CreatedBy::User,
                creation_source: CreationSource::Server,
                message_id: MessageId::new(format!("async-answer:{request_id}"))
                    .expect("derived id"),
                text: replies.join("\n\n"),
                context: None,
                attachments: vec![],
                model_selection: None,
                delivery_intent: None,
                dispatch_mode: mode,
            };
            dispatch(
                decision,
                command,
                projection,
                &input,
                now,
                capabilities,
                driver,
                InputIntent::Steer,
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// A stopped startup must not leave provider I/O detached from run ownership.
pub fn interrupted_start(
    projection: &ThreadProjection,
    run_id: &RunId,
    key: &str,
    now: &Timestamp,
) -> Decision {
    let Some(run) = projection
        .runs
        .iter()
        .find(|run| run.id == *run_id && run.status.is_blocking())
    else {
        return Decision::default();
    };
    let command = Command {
        command_id: CommandId::new(format!("command:cancel-start:{key}")).expect("derived id"),
        thread_id: projection.thread.id.clone(),
        body: CommandBody::RunInterrupt {
            run_id: run_id.clone(),
            reason: None,
            hold_queue: true,
        },
    };
    let mut decision = Decision::default();
    terminalize(
        &mut decision,
        &command,
        run,
        &projection.attempts,
        &projection.nodes,
        now,
        RunStatus::Interrupted,
    );
    for turn in projection.provider_turns.iter().filter(|turn| {
        turn.run_attempt_id == run.active_attempt_id
            && matches!(turn.status, TurnStatus::Pending | TurnStatus::Running)
    }) {
        for payload in finish_provider_turn(
            turn,
            &projection.runtime_requests,
            &projection.turn_items,
            now,
            TurnStatus::Interrupted,
        ) {
            emit(&mut decision, &command, now, payload);
        }
    }
    decision
}

/// Startup recovery never silently runs a held queue or replays process-bound I/O.
pub fn recover(
    projection: &ThreadProjection,
    replayable_captures: &[RunId],
    now: &Timestamp,
) -> Vec<EventPayload> {
    let mut result = vec![];
    for run in &projection.runs {
        let mut run = run.clone();
        if run.status == RunStatus::Queued {
            if projection
                .messages
                .iter()
                .any(|m| m.id == run.user_message_id && m.native_continuation.is_some())
            {
                run.status = RunStatus::Cancelled;
                run.completed_at = Some(now.clone());
            } else {
                run.queue_held = true;
            }
            result.push(EventPayload::RunUpdated(run));
        } else if run.status.is_blocking() && !replayable_captures.contains(&run.id) {
            run.status = RunStatus::Interrupted;
            if let Some(cohort) = &mut run.delegated_completion {
                cohort.disposition = CohortDisposition::Stopped;
            }
            run.completed_at = Some(now.clone());
            result.push(EventPayload::RunUpdated(run));
        }
    }
    for attempt in &projection.attempts {
        if matches!(
            attempt.status,
            AttemptStatus::Pending | AttemptStatus::Running
        ) {
            let mut attempt = attempt.clone();
            attempt.status = AttemptStatus::Interrupted;
            attempt.completed_at = Some(now.clone());
            result.push(EventPayload::RunAttemptUpdated(attempt));
        }
    }
    for node in &projection.nodes {
        if matches!(
            node.status,
            NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
        ) && !node
            .run_id
            .as_ref()
            .is_some_and(|id| replayable_captures.contains(id))
        {
            let mut node = node.clone();
            node.status = NodeStatus::Interrupted;
            node.completed_at = Some(now.clone());
            result.push(EventPayload::NodeUpdated(node));
        }
    }
    for item in &projection.turn_items {
        if matches!(
            item.status,
            ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting
        ) {
            result.push(EventPayload::TurnItemUpdated(
                crate::projector::finished_item(item, ItemStatus::Interrupted, now),
            ));
        }
    }
    for message in &projection.messages {
        if message.streaming {
            let mut message = message.clone();
            message.streaming = false;
            message.updated_at = now.clone();
            result.push(EventPayload::MessageUpdated(message));
        }
    }
    for request in &projection.runtime_requests {
        if request.status == RequestStatus::Pending {
            let mut request = request.clone();
            request.status = RequestStatus::Cancelled;
            request.resolved_at = Some(now.clone());
            result.push(EventPayload::RuntimeRequestUpdated(request));
        }
    }
    for task in &projection.subagents {
        if task.origin == SubagentOrigin::ProviderNative
            && matches!(
                task.status,
                NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
            )
        {
            let mut task = task.clone();
            task.status = NodeStatus::Interrupted;
            task.completed_at = Some(now.clone());
            task.updated_at = now.clone();
            result.push(EventPayload::SubagentUpdated(task));
        }
    }
    for session in &projection.provider_sessions {
        result.push(EventPayload::ProviderSessionDetached(session.id.clone()));
    }
    for turn in &projection.provider_turns {
        if matches!(turn.status, TurnStatus::Pending | TurnStatus::Running) {
            let mut turn = turn.clone();
            turn.status = TurnStatus::Interrupted;
            turn.completed_at = Some(now.clone());
            result.push(EventPayload::ProviderTurnUpdated(turn));
        }
    }
    result
}

/// Normal terminal events advance the app-owned queue in the same transaction.
pub fn after_terminal(
    projection: &ThreadProjection,
    now: &Timestamp,
    trigger_id: &str,
) -> Decision {
    let command = Command {
        command_id: CommandId::new(format!("command:queue-next:{trigger_id}")).expect("derived id"),
        thread_id: projection.thread.id.clone(),
        body: CommandBody::QueueResume,
    };
    let mut decision = Decision::default();
    if projection.thread.archived_at.is_none() && projection.thread.deleted_at.is_none() {
        promote_next(
            &mut decision,
            &command,
            (&projection.runs, &projection.messages),
            has_native_turn(&projection.provider_turns),
            next_ordinal(projection),
            now,
            None,
        );
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use proptest::prelude::*;
    #[test]
    fn stopping_a_scoped_run_enqueues_capture_before_the_next_start() {
        let (mut p, _) = apply(
            &projection(),
            &send("first", DispatchMode::StartImmediately),
        );
        let run = p.runs[0].clone();
        p.checkpoint_scopes.push(checkpoint_scope(&run));
        let (p, _) = apply(&p, &send("queued", DispatchMode::QueueAfterActive));
        let (_, decision) = apply(
            &p,
            &command(
                "stop",
                CommandBody::RunInterrupt {
                    run_id: run.id,
                    reason: None,
                    hold_queue: false,
                },
            ),
        );
        assert!(matches!(
            decision.effects[0].body,
            EffectBody::CaptureCheckpoint { .. }
        ));
        assert!(matches!(decision.effects[1].body, EffectBody::Start { .. }));
    }
    #[test]
    fn failed_preparation_promotes_queue_and_cannot_retry_over_an_active_run() {
        let (p, _) = apply(
            &projection(),
            &send(
                "prepared",
                DispatchMode::DeferStart {
                    workspace_strategy: None,
                },
            ),
        );
        let prepared = p.runs[0].id.clone();
        let (p, _) = apply(&p, &send("queued", DispatchMode::QueueAfterActive));
        let failure = ProviderFailure {
            class: FailureClass::ProviderError,
            message: "prepare failed".into(),
            retryable: Some(false),
            code: None,
            reset_at: None,
        };
        let (p, _) = apply(
            &p,
            &command(
                "failed",
                CommandBody::PreparedRunFail {
                    run_id: prepared.clone(),
                    failure,
                },
            ),
        );
        assert_eq!(p.runs[1].status, RunStatus::Starting);
        assert!(
            decide(
                &command("retry", CommandBody::PreparedRunRetry { run_id: prepared }),
                Some(&p),
                &now(),
                &turns(),
                Driver::Codex
            )
            .is_err()
        );
    }
    #[test]
    fn recovery_finishes_streaming_items_and_messages() {
        let mut p = running();
        let user = p.turn_items[0].clone();
        let mut item = user.clone();
        item.id = TurnItemId::new("reply").unwrap();
        item.status = ItemStatus::Running;
        item.body = TurnItemBody::AssistantMessage {
            message_id: MessageId::new("reply-message").unwrap(),
            text: "Partial".into(),
            attachments: vec![],
            streaming: true,
        };
        p.turn_items.push(item);
        let mut message = p.messages[0].clone();
        message.streaming = true;
        p.messages.push(message);
        let events = recover(&p, &[], &now());
        assert!(events.iter().any(|event| matches!(event, EventPayload::TurnItemUpdated(item) if item.status == ItemStatus::Interrupted && matches!(item.body, TurnItemBody::AssistantMessage { streaming: false, .. }))));
        assert!(events.iter().any(
            |event| matches!(event, EventPayload::MessageUpdated(message) if !message.streaming)
        ));
    }
    #[test]
    fn unread_uses_latest_completed_run_even_when_newer_run_is_queued() {
        let mut p = running();
        p.runs[0].status = RunStatus::Completed;
        p.runs[0].completed_at = Some(now());
        let mut queued = p.runs[0].clone();
        queued.id = RunId::new("queued").unwrap();
        queued.ordinal += 1;
        queued.status = RunStatus::Queued;
        queued.completed_at = None;
        p.runs.push(queued);
        let decision = decide(
            &command("unread", CommandBody::ThreadMarkUnread),
            Some(&p),
            &now(),
            &turns(),
            Driver::Codex,
        )
        .unwrap();
        assert!(
            matches!(&decision.events[0].payload, EventPayload::ThreadMarkedUnread(thread) if thread.last_visited_at.as_ref().unwrap().millis() == now().millis() - 1)
        );
    }
    #[test]
    fn maintenance_can_queue_but_cannot_be_steered_or_receive_steering() {
        let p = running();
        let run_id = p.runs[0].id.clone();
        let mut c = send(
            "compact",
            DispatchMode::SteerActive {
                target_run_id: run_id.clone(),
            },
        );
        let CommandBody::MessageDispatch(input) = &mut c.body else {
            panic!()
        };
        input.text = "/compact".into();
        assert!(decide(&c, Some(&p), &now(), &turns(), Driver::Codex).is_err());
        let CommandBody::MessageDispatch(input) = &mut c.body else {
            panic!()
        };
        input.dispatch_mode = DispatchMode::QueueAfterActive;
        assert!(decide(&c, Some(&p), &now(), &turns(), Driver::Codex).is_ok());
        let mut p = p;
        let message = p
            .messages
            .iter_mut()
            .find(|m| m.id == p.runs[0].user_message_id)
            .unwrap();
        message.text = "/compact".into();
        let c = send(
            "steer",
            DispatchMode::SteerActive {
                target_run_id: run_id,
            },
        );
        assert!(decide(&c, Some(&p), &now(), &turns(), Driver::Codex).is_err());
    }
    #[test]
    fn queued_runs_keep_execution_records_and_stop_holds_queue() {
        let (projection, _) = apply(&projection(), &send("one", DispatchMode::StartImmediately));
        let (projection, _) = apply(&projection, &send("two", DispatchMode::QueueAfterActive));
        assert_eq!(projection.runs[1].status, RunStatus::Queued);
        assert_eq!(projection.runs[1].queue_position, Some(1));
        assert_eq!(projection.attempts.len(), 2);
        assert_eq!(projection.nodes.len(), 2);
        let stop_command = command(
            "stop",
            CommandBody::RunInterrupt {
                run_id: projection.runs[0].id.clone(),
                reason: None,
                hold_queue: true,
            },
        );
        let (projection, decision) = apply(&projection, &stop_command);
        assert_eq!(projection.runs[0].status, RunStatus::Interrupted);
        assert!(projection.runs[1].queue_held);
        assert!(decision.cancel_unsettled_effects);
        let (projection, decision) =
            apply(&projection, &command("resume", CommandBody::QueueResume));
        assert_eq!(projection.runs[1].status, RunStatus::Starting);
        assert!(matches!(decision.effects[0].body, EffectBody::Start { .. }));
    }
    #[test]
    fn live_stop_waits_for_provider_terminal_event() {
        let projection = running();
        let (_, decision) = apply(
            &projection,
            &command(
                "stop",
                CommandBody::RunInterrupt {
                    run_id: projection.runs[0].id.clone(),
                    reason: None,
                    hold_queue: true,
                },
            ),
        );
        assert!(matches!(
            decision.effects[0].body,
            EffectBody::Interrupt { .. }
        ));
        assert!(!decision.events.iter().any(|event| matches!(&event.payload, EventPayload::RunUpdated(run) if run.status.is_terminal())));
    }
    #[test]
    fn steer_is_a_message_in_the_active_run_not_another_run() {
        let projection = running();
        let id = projection.runs[0].id.clone();
        let (projection, decision) = apply(
            &projection,
            &send(
                "steer",
                DispatchMode::SteerActive {
                    target_run_id: id.clone(),
                },
            ),
        );
        assert_eq!(projection.runs.len(), 1);
        assert_eq!(
            projection.messages.last().unwrap().run_id.as_ref(),
            Some(&id)
        );
        assert!(matches!(decision.effects[0].body, EffectBody::Steer { .. }));
    }
    #[test]
    fn restart_stops_the_old_delegation_cohort_before_the_new_attempt() {
        let mut p = running();
        p.runs[0].delegated_completion = Some(DelegatedCompletionCohort {
            disposition: CohortDisposition::Open,
            next_generation: 7,
            delivery: None,
        });
        let (p, _) = apply(
            &p,
            &send(
                "restart-cohort",
                DispatchMode::RestartActive {
                    target_run_id: p.runs[0].id.clone(),
                },
            ),
        );
        assert_eq!(
            p.runs[0].delegated_completion.as_ref().unwrap().disposition,
            CohortDisposition::Stopped
        );
        assert_eq!(
            p.runs[0]
                .delegated_completion
                .as_ref()
                .unwrap()
                .next_generation,
            7
        );
    }
    #[test]
    fn restart_supersedes_attempt_and_creates_another_root() {
        let projection = running();
        let (projection, decision) = apply(
            &projection,
            &send(
                "restart",
                DispatchMode::RestartActive {
                    target_run_id: projection.runs[0].id.clone(),
                },
            ),
        );
        assert_eq!(projection.attempts[0].status, AttemptStatus::Superseded);
        assert_eq!(projection.provider_turns[0].status, TurnStatus::Interrupted);
        assert_eq!(projection.nodes[0].status, NodeStatus::Interrupted);
        assert_eq!(
            projection.attempts[1].reason,
            AttemptReason::SteeringRestart
        );
        assert_eq!(
            projection.runs[0].active_attempt_id.as_ref(),
            Some(&projection.attempts[1].id)
        );
        assert!(matches!(
            decision.effects[0].body,
            EffectBody::Restart { .. }
        ));
    }
    #[test]
    fn pin_promotes_parked_threads_and_settle_clears_pin() {
        let (projection, _) = apply(
            &projection(),
            &command("settle", CommandBody::ThreadSettle { settled_at: None }),
        );
        let (projection, _) = apply(
            &projection,
            &command(
                "pin",
                CommandBody::ThreadPin {
                    order_key: Some("a".into()),
                },
            ),
        );
        assert_eq!(
            projection.thread.settled_override,
            Some(SettledOverride::Active)
        );
        let (projection, _) = apply(
            &projection,
            &command(
                "settle-again",
                CommandBody::ThreadSettle { settled_at: None },
            ),
        );
        assert!(projection.thread.pinned_at.is_none());
        assert!(projection.thread.pin_order_key.is_none());
    }
    #[test]
    fn recovery_terminalizes_live_work_and_holds_queued_work() {
        let (projection, _) = apply(&running(), &send("queued", DispatchMode::QueueAfterActive));
        let events = recover(&projection, &[], &now());
        assert!(events.iter().any(|event| matches!(event, EventPayload::RunUpdated(run) if run.status == RunStatus::Interrupted)));
        assert!(events.iter().any(|event| matches!(event, EventPayload::RunUpdated(run) if run.status == RunStatus::Queued && run.queue_held)));
        assert!(events.iter().any(|event| matches!(event, EventPayload::ProviderTurnUpdated(turn) if turn.status == TurnStatus::Interrupted)));
    }
    #[test]
    fn promotion_cancels_the_queue_and_steers_the_same_message() {
        let (projection, _) = apply(&running(), &send("queued", DispatchMode::QueueAfterActive));
        let queued = projection.runs[1].id.clone();
        let target = projection.runs[0].id.clone();
        let (projection, decision) = apply(
            &projection,
            &command(
                "promote",
                CommandBody::QueuedMessagePromoteToSteer {
                    queued_run_id: queued,
                    target_run_id: target.clone(),
                },
            ),
        );
        assert_eq!(projection.runs[1].status, RunStatus::Cancelled);
        assert_eq!(
            projection.messages.last().unwrap().run_id.as_ref(),
            Some(&target)
        );
        assert!(matches!(decision.effects[0].body, EffectBody::Steer { .. }));
    }
    fn question(
        projection: &mut ThreadProjection,
        capability: ResponseCapability,
    ) -> RuntimeRequestId {
        let request_id = RuntimeRequestId::new("question").unwrap();
        let node_id = NodeId::new("question-node").unwrap();
        projection.runtime_requests.push(RuntimeRequest {
            id: request_id.clone(),
            node_id: node_id.clone(),
            provider_turn_id: None,
            native_request_ref: None,
            kind: RequestKind::UserInput,
            status: RequestStatus::Pending,
            response_capability: capability,
            created_at: now(),
            resolved_at: None,
            decision: None,
            answers: None,
        });
        let mut item = notice_item(
            &command("question", CommandBody::ThreadMarkUnread),
            &projection.runs[0],
            &now(),
            TurnItemBody::UserInputRequest {
                request_id: request_id.clone(),
                questions: vec![UserInputQuestion {
                    id: "q".into(),
                    header: "Choice".into(),
                    question: "Which option?".into(),
                    options: vec![],
                    multi_select: false,
                    allow_custom_answer: true,
                    required: true,
                }],
                question_answer: None,
                response_mode_message: true,
            },
            "question",
            1_000_002,
        );
        item.node_id = Some(node_id);
        item.status = ItemStatus::Waiting;
        item.completed_at = None;
        projection.turn_items.push(item);
        request_id
    }
    #[test]
    fn message_capable_question_resolves_and_dispatches_in_one_decision() {
        let mut projection = running();
        let request_id = question(&mut projection, ResponseCapability::Message);
        let answers = Answers::from([("q".into(), Json(serde_json::json!("Option A")))]);
        let (projection, decision) = apply(
            &projection,
            &command(
                "answer",
                CommandBody::RuntimeRequestRespond {
                    request_id,
                    decision: None,
                    answers: Some(answers),
                },
            ),
        );
        assert_eq!(
            projection.runtime_requests[0].status,
            RequestStatus::Resolved
        );
        assert!(matches!(decision.effects[0].body, EffectBody::Steer { .. }));
        assert!(
            projection
                .messages
                .last()
                .unwrap()
                .text
                .contains("Which option?\nOption A")
        );
    }
    #[test]
    fn question_dismissal_is_only_for_message_capable_requests() {
        let mut projection = running();
        let request_id = question(
            &mut projection,
            ResponseCapability::NotResumable {
                reason: "process lost".into(),
            },
        );
        let dismiss = command(
            "dismiss",
            CommandBody::ThreadUserInputDismiss {
                request_id: request_id.clone(),
            },
        );
        assert!(decide(&dismiss, Some(&projection), &now(), &turns(), Driver::Codex).is_err());
        projection.runtime_requests[0].response_capability = ResponseCapability::Message;
        let (projection, decision) = apply(&projection, &dismiss);
        assert_eq!(
            projection.runtime_requests[0].decision,
            Some(ApprovalDecision::Cancel)
        );
        assert!(decision.effects.is_empty());
    }
    #[test]
    fn question_validation_failure_does_not_change_the_projection() {
        let mut projection = running();
        let request_id = question(&mut projection, ResponseCapability::Message);
        let before = projection.clone();
        let answer = command(
            "answer",
            CommandBody::RuntimeRequestRespond {
                request_id,
                decision: None,
                answers: Some(Answers::new()),
            },
        );
        assert!(decide(&answer, Some(&projection), &now(), &turns(), Driver::Codex).is_err());
        assert_eq!(projection, before);
    }
    proptest! {
        #[test]
        fn queue_transitions_never_create_two_blocking_runs(count in 1usize..30, cancel in proptest::collection::vec(any::<bool>(), 30)) {
            let mut projection = projection();
            for i in 0..count { projection = apply(&projection, &send(&format!("send-{i}"), DispatchMode::StartImmediately)).0; }
            for (i, cancelled) in cancel.iter().enumerate().take(count).skip(1) { if *cancelled { projection = apply(&projection, &command(&format!("cancel-{i}"), CommandBody::QueuedRunCancel { run_id: projection.runs[i].id.clone() })).0; } }
            prop_assert_eq!(projection.runs.iter().filter(|run| run.status.is_blocking()).count(), 1);
            let positions: Vec<_> = queued_runs(&projection.runs, &projection.messages).iter().map(|run| run.queue_position.unwrap()).collect();
            prop_assert!(positions.windows(2).all(|positions| positions[0] < positions[1]));
        }
        #[test]
        fn decisions_are_deterministic_and_leave_inputs_unchanged(text in "[a-zA-Z0-9 ]{1,80}") {
            let projection = projection(); let before = projection.clone();
            let mut command = send("input", DispatchMode::StartImmediately);
            if let CommandBody::MessageDispatch(message) = &mut command.body {message.text = format!("x{text}");}
            let first = decide(&command, Some(&projection), &now(), &turns(), Driver::Codex).unwrap();
            let second = decide(&command, Some(&projection), &now(), &turns(), Driver::Codex).unwrap();
            prop_assert_eq!(first, second); prop_assert_eq!(projection, before);
        }
    }
}
