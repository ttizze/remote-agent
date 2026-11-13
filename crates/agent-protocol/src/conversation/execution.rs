use super::*;

pub(super) fn event(state: &State, event: Event, context: Context) -> Step {
    match event {
        Event::AligningProvider { id, action, source } => {
            if source.is_empty() || state.source_in_use(state.thread.provider,&source,Uuid::nil()) || !state.inputs.get(&id).is_some_and(|input|
                matches!(input.state, InputState::Preparing { stage: PreparationStage::Provider, action: current, .. } if current == action)) {
                return Step::default();
            }
            Step {
                changes: vec![Change::Binding {
                    provider: state.thread.provider,
                    value: Some(Binding::Aligning { source }),
                }],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        Event::ExecutionLost { attempt, error } => {
            provider(state, attempt, Observation::Failed(Some(error)), context)
        }
        Event::Title(name) => {
            if !state.thread.name.trim().is_empty() || name.trim().is_empty() {
                return Step::default();
            }
            Step {
                changes: vec![Change::Thread(Thread {
                    name,
                    ..state.thread.clone()
                })],
                outcome: Outcome::Accepted,
                ..Default::default()
            }
        }
        Event::Wake => wake(state, context),
        Event::NativeStatus {
            provider, source, ..
        } => {
            if current_source(
                state.thread.provider,
                state.bindings.get(&provider),
                provider,
                &source,
            ) {
                wake(state, context)
            } else {
                Step::default()
            }
        }
        Event::ObservedStart {
            execution,
            provider,
            source,
        } => observed_start(state, execution, provider, source, context),
        Event::Prepared {
            id,
            action,
            cwd,
            source,
        } => prepared(state, &id, action, cwd, source, context),
        Event::PreparationFailed { id, action, error } => {
            preparation_failed(state, &id, action, error)
        }
        Event::Delivered { id, action, .. } => {
            let result = delivered(state, &id, action, context);
            if result.outcome == Outcome::Accepted {
                return commands::append_advance(state, result, context);
            }
            result
        }
        Event::InterruptResult {
            attempt,
            action,
            error,
            background,
        } => {
            let Some(attempt) = state
                .attempts
                .values()
                .find(|current| current.id == attempt)
            else {
                return Step::default();
            };
            let Some(turn) = state.turns.get(&attempt.turn).filter(|turn| matches!(turn.state, TurnState::Running { interrupt: Some(current) } if current == action)) else { return Step::default(); };
            if error.is_none() && background.is_empty() {
                return finish(state, turn, TurnState::Completed, context.now);
            }
            Step {
                changes: vec![Change::Turn(Turn {
                    state: if background.is_empty() {
                        TurnState::Running { interrupt: None }
                    } else {
                        TurnState::Waiting(Wait::Background { tasks: background })
                    },
                    error,
                    ..turn.clone()
                })],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        Event::DeliveryDeferred { id, action } => {
            let Some(input) = state.inputs.get(&id) else {
                return Step::default();
            };
            let InputState::Delivering(delivery) = &input.state else {
                return Step::default();
            };
            if delivery.action != action || !delivery.steer {
                return Step::default();
            }
            let result = Step {
                changes: vec![Change::Input(PendingInput {
                    state: InputState::Queued {
                        steer: Some(delivery.attempt.id),
                    },
                    ..input.clone()
                })],
                outcome: Outcome::Accepted,
                ..Step::default()
            };
            commands::append_advance(state, result, context)
        }
        Event::DeliveryFailed {
            id,
            action,
            error,
            unknown,
        } => delivery_failed(state, &id, action, error, unknown),
        Event::DeliveryFenced { id, action } => {
            let Some(input) = state.inputs.get(&id).filter(|input| {
                input
                    .live_delivery()
                    .is_some_and(|delivery| delivery.action == action)
            }) else {
                return Step::default();
            };
            let delivery = input.live_delivery().expect("selected live delivery");
            let mut result = state
                .turns
                .get(&delivery.attempt.turn)
                .filter(|turn| {
                    state.attempts.get(&turn.id) == Some(&delivery.attempt)
                        && matches!(
                            turn.state,
                            TurnState::Running { .. } | TurnState::Waiting(_)
                        )
                })
                .map(|turn| finish(state, turn, TurnState::Interrupted, context.now))
                .unwrap_or_default();
            result.changes.extend([
                input.fenced().expect("selected live delivery"),
                Change::Thread(Thread {
                    queue_held: state.has_queued_inputs(),
                    ..state.thread.clone()
                }),
            ]);
            if !state.source_in_use(delivery.attempt.provider, &delivery.attempt.source, delivery.attempt.id)
                && state.bindings.get(&delivery.attempt.provider).is_some_and(|binding| matches!(binding, Binding::Ready { source, .. } if source == &delivery.attempt.source)) {
                result.changes.push(Change::Binding { provider: delivery.attempt.provider, value: None });
            }
            result.outcome = Outcome::Accepted;
            result
        }
        Event::Provider {
            native_items: _,
            native_turn: _,
            attempt,
            observation,
        } => provider(state, attempt, observation, context),
        Event::Answered { id, action, result } => {
            let Some(request) = state.requests.get(&id).filter(|request|
                matches!(request.state, RequestState::Sending { action: expected, .. } if expected == action)) else { return Step::default(); };
            Step {
                changes: vec![match result {
                    Ok(()) => Change::RemoveRequest(id),
                    Err(delivery) => Change::Request(Request {
                        state: match delivery {
                            crate::error::Delivery::NotSent => RequestState::Awaiting,
                            crate::error::Delivery::Unknown => RequestState::Unknown,
                        },
                        ..request.clone()
                    }),
                }],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        Event::Command { .. } | Event::RejectedCommand { .. } | Event::Restart { .. } => {
            unreachable!("handled by step")
        }
    }
}

fn wake(state: &State, context: Context) -> Step {
    let Some(turn) = state.active_turn() else {
        return commands::advance(state, context);
    };
    let TurnState::Waiting(Wait::Retry { at }) = turn.state else {
        return commands::advance(state, context);
    };
    if context.now < at {
        return Step {
            actions: vec![Action::WaitUntil(at)],
            ..Step::default()
        };
    }
    let binding = state.bindings.get(&state.thread.provider);
    let (Workspace::Ready(cwd), Some(Binding::Ready { source, .. })) =
        (&state.thread.workspace, binding)
    else {
        return finish(
            state,
            turn,
            TurnState::Failed(ExecutionError {
                message: "再試行の接続先を復元できませんでした。入力を確認して再送してください。"
                    .into(),
                ..Default::default()
            }),
            context.now,
        );
    };
    let attempt = Attempt {
        id: allocate(context.seed, "attempt"),
        turn: turn.id.clone(),
        provider: state.thread.provider,
        source: source.clone(),
    };
    Step {
        changes: vec![
            Change::Turn(Turn {
                state: TurnState::Running { interrupt: None },
                error: None,
                ..turn.clone()
            }),
            Change::Attempt {
                turn: turn.id.clone(),
                value: Some(attempt.clone()),
            },
        ],
        actions: vec![Action::Resume {
            attempt,
            cwd: cwd.clone(),
            error: turn.error.clone(),
        }],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn preparation_failed(
    state: &State,
    id: &ClientInputId,
    action: Uuid,
    error: ExecutionError,
) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return Step::default();
    };
    let InputState::Preparing {
        action: expected, ..
    } = input.state
    else {
        return Step::default();
    };
    if action != expected {
        return Step::default();
    }
    Step {
        changes: vec![
            Change::Input(PendingInput {
                state: InputState::Failed(error),
                ..input.clone()
            }),
            Change::Thread(Thread {
                queue_held: state.has_queued_inputs(),
                ..state.thread.clone()
            }),
        ],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn prepared(
    state: &State,
    id: &ClientInputId,
    action: Uuid,
    cwd: Option<String>,
    source: Option<String>,
    context: Context,
) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return Step::default();
    };
    let InputState::Preparing {
        stage,
        action: expected,
        mode,
    } = input.state
    else {
        return Step::default();
    };
    if action != expected {
        return Step::default();
    }
    if stage == PreparationStage::Provider && source.as_ref().is_none_or(|source| source.is_empty())
    {
        return preparation_failed(
            state,
            id,
            action,
            ExecutionError {
                message: "provider preparation returned no session".into(),
                ..ExecutionError::default()
            },
        );
    }
    let cwd = cwd
        .or_else(|| state.thread.workspace.cwd().map(str::to_owned))
        .filter(|cwd| !cwd.trim().is_empty());
    let Some(cwd) = cwd else {
        return preparation_failed(
            state,
            id,
            action,
            ExecutionError {
                message: "workspace preparation returned no directory".into(),
                ..ExecutionError::default()
            },
        );
    };
    let next_stage = match stage {
        PreparationStage::Worktree => Some(PreparationStage::SetupScript),
        PreparationStage::Workspace | PreparationStage::SetupScript => {
            Some(PreparationStage::Provider)
        }
        PreparationStage::Provider => None,
    };
    let workspace = match stage {
        // The default chat directory is already prepared. Only an explicitly
        // selected folder goes through automatic worktree creation.
        PreparationStage::Workspace
        | PreparationStage::Worktree
        | PreparationStage::SetupScript => Workspace::Ready(cwd.clone()),
        PreparationStage::Provider => {
            if state.thread.workspace.cwd() != Some(cwd.as_str()) {
                return preparation_failed(
                    state,
                    id,
                    action,
                    ExecutionError {
                        message: "provider preparation changed the working directory".into(),
                        ..ExecutionError::default()
                    },
                );
            }
            state.thread.workspace.clone()
        }
    };
    let action = allocate(context.seed, "prepare");
    let mut result = Step {
        changes: vec![
            Change::Thread(Thread {
                workspace,
                ..state.thread.clone()
            }),
            Change::Input(PendingInput {
                state: next_stage.map_or(InputState::Queued { steer: None }, |stage| {
                    InputState::Preparing {
                        stage,
                        action,
                        mode,
                    }
                }),
                ..input.clone()
            }),
        ],
        outcome: Outcome::Accepted,
        ..Step::default()
    };
    if let Some(stage) = next_stage {
        result.actions.push(Action::Prepare {
            id: id.clone(),
            action,
            stage,
            cwd: Some(cwd),
            provider: state.thread.provider,
        });
    } else {
        result.changes.push(Change::Binding {
            provider: state.thread.provider,
            value: Some(Binding::Ready {
                source: source.expect("provider source checked above"),
                covered: state.history_end(),
            }),
        });
        let next = state.apply(&result.changes);
        let mut delivery = commands::dispatch(&next, id, context, mode);
        if delivery.actions.is_empty() {
            delivery = commands::advance(&next, context);
        }
        result.changes.extend(delivery.changes);
        result.actions.extend(delivery.actions);
    }
    result
}

fn delivered(state: &State, id: &ClientInputId, action: Uuid, context: Context) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return Step::default();
    };
    let Some(delivery) = input.live_delivery() else {
        return Step::default();
    };
    let attempt = &delivery.attempt;
    let turn = &attempt.turn;
    if delivery.action != action {
        return Step::default();
    }
    if delivery.steer {
        if !state.turns.contains_key(turn) || state.attempts.get(turn) != Some(attempt) {
            return Step::default();
        }
    } else if state.turns.contains_key(turn) {
        return Step::default();
    }
    let Some(position) = state.counters.item.checked_add(1) else {
        return Step::default();
    };
    let mut changes = vec![
        Change::RemoveInput(id.clone()),
        Change::Item(Item {
            id: ItemId(id.0.clone()),
            turn: turn.clone(),
            position,
            status: ItemStatus::Completed,
            deferred: false,
            input: None,
            body: ItemBody::UserMessage {
                text: None,
                content: InputPart::message_parts(&input.content.parts),
            },
        }),
        Change::Counters(Counters {
            item: position,
            ..state.counters.clone()
        }),
    ];
    if !delivery.steer {
        changes.push(Change::Turn(Turn {
            id: turn.clone(),
            ordinal: delivery.ordinal,
            provider: attempt.provider,
            queued: delivery.queued,
            observed: false,
            started_at: Some(context.now),
            completed_at: None,
            duration_ms: None,
            retry_count: 0,
            error: None,
            state: TurnState::Running {
                interrupt: delivery.interrupt,
            },
        }));
        changes.push(Change::Attempt {
            turn: turn.clone(),
            value: Some(attempt.clone()),
        });
        changes.push(Change::Binding {
            provider: attempt.provider,
            value: Some(Binding::Ready {
                source: attempt.source.clone(),
                covered: delivery.ordinal,
            }),
        });
    }
    Step {
        changes,
        actions: delivery
            .interrupt
            .map(|action| Action::Interrupt {
                attempt: attempt.clone(),
                action,
                background: false,
            })
            .into_iter()
            .collect(),
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn delivery_failed(
    state: &State,
    id: &ClientInputId,
    action: Uuid,
    error: ExecutionError,
    unknown: bool,
) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return Step::default();
    };
    let InputState::Delivering(delivery) = &input.state else {
        return Step::default();
    };
    if delivery.action != action {
        return Step::default();
    }
    let changes = vec![
        Change::Input(PendingInput {
            state: if unknown {
                InputState::Unknown {
                    delivery: delivery.clone(),
                    status: UnknownState::Live,
                }
            } else {
                InputState::Failed(error)
            },
            ..input.clone()
        }),
        Change::Thread(Thread {
            queue_held: state.has_queued_inputs(),
            ..state.thread.clone()
        }),
    ];
    Step {
        changes,
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn provider(state: &State, attempt: Uuid, observation: Observation, context: Context) -> Step {
    if let Observation::RequestResolved(id) = &observation {
        return if state.requests.contains_key(id) {
            Step {
                changes: vec![Change::RemoveRequest(id.clone())],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        } else {
            Step::default()
        };
    }
    if let Some(execution) = state
        .attempts
        .values()
        .find(|execution| execution.id == attempt)
    {
        return state
            .turns
            .get(&execution.turn)
            .map_or_else(Step::default, |turn| {
                observe(state, turn, observation, context)
            });
    }
    let Some(input) = state.inputs.values().find(|input| {
        input
            .live_delivery()
            .is_some_and(|delivery| delivery.attempt.id == attempt && !delivery.steer)
    }) else {
        return Step::default();
    };
    let delivery = input.live_delivery().expect("selected live start");
    let mut result = delivered(state, &input.id, delivery.action, context);
    if result.outcome != Outcome::Accepted {
        return Step::default();
    }
    let next = state.apply(&result.changes);
    let Some(turn) = next.turns.get(&delivery.attempt.turn) else {
        return result;
    };
    let observed = observe(&next, turn, observation, context);
    result.changes.extend(observed.changes);
    result.actions.extend(observed.actions);
    if observed.outcome != Outcome::Ignored {
        result.outcome = observed.outcome;
    }
    result
}

fn observe(state: &State, turn: &Turn, observation: Observation, context: Context) -> Step {
    if !matches!(
        turn.state,
        TurnState::Running { .. } | TurnState::Waiting(_)
    ) && !(turn.state == TurnState::Completed
        && matches!(
            observation,
            Observation::Item { .. } | Observation::RemoveItem(_) | Observation::Timing { .. }
        ))
    {
        return Step::default();
    }
    let completed = matches!(observation, Observation::Completed);
    let stopping = matches!(turn.state, TurnState::Running { interrupt: Some(_) });
    let result = match observation {
        Observation::Timing {
            started_at,
            completed_at,
            duration_ms,
        } => Step {
            changes: vec![Change::Turn(Turn {
                started_at: started_at.or(turn.started_at),
                completed_at: completed_at.or(turn.completed_at),
                duration_ms: duration_ms.or(turn.duration_ms),
                ..turn.clone()
            })],
            outcome: Outcome::Accepted,
            ..Step::default()
        },
        Observation::Retry { .. } if stopping => {
            finish(state, turn, TurnState::Interrupted, context.now)
        }
        Observation::Background { tasks }
            if stopping && crate::execution::background_holds_completion(&tasks) =>
        {
            Step::default()
        }
        Observation::Completed => finish(state, turn, TurnState::Completed, context.now),
        Observation::Failed(error) => failed(state, turn, error, context),
        Observation::Notice(error) => Step {
            changes: vec![Change::Turn(Turn {
                error: Some(error),
                ..turn.clone()
            })],
            outcome: Outcome::Accepted,
            ..Default::default()
        },
        Observation::Interrupted => finish(state, turn, TurnState::Interrupted, context.now),
        Observation::Retry { at } => retry(state, turn, at, turn.error.clone()),
        Observation::Background { tasks } => {
            if tasks.is_empty() {
                return commands::append_advance(
                    state,
                    finish(state, turn, TurnState::Completed, context.now),
                    context,
                );
            }
            let result = Step {
                changes: vec![Change::Turn(Turn {
                    state: TurnState::Waiting(Wait::Background { tasks }),
                    ..turn.clone()
                })],
                outcome: Outcome::Accepted,
                ..Step::default()
            };
            commands::append_advance(state, result, context)
        }
        Observation::RemoveItem(id) => {
            if state.items.get(&id).is_some_and(|item| {
                item.turn == turn.id && !matches!(item.body, ItemBody::UserMessage { .. })
            }) {
                Step {
                    changes: vec![Change::RemoveItem(id)],
                    outcome: Outcome::Accepted,
                    ..Default::default()
                }
            } else {
                Step::default()
            }
        }
        Observation::Item {
            id,
            status,
            deferred,
            body,
        } => item(state, turn, id, status, deferred, body),
        Observation::Request { id, body } => {
            if state.requests.contains_key(&id) {
                return Step::default();
            }
            Step {
                changes: vec![Change::Request(Request {
                    id,
                    turn: turn.id.clone(),
                    body,
                    state: RequestState::Awaiting,
                })],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        Observation::RequestResolved(_) => {
            unreachable!("resolved requests are independent of the active attempt")
        }
    };
    if completed {
        return commands::append_advance(state, result, context);
    }
    result
}

fn item(
    state: &State,
    turn: &Turn,
    id: ItemId,
    status: ItemStatus,
    deferred: bool,
    body: ItemBody,
) -> Step {
    if state
        .items
        .get(&id)
        .is_some_and(|item| item.turn == turn.id && item.status == status && item.body == body)
    {
        return Step::default();
    }
    if state.items.get(&id).is_some_and(|item| {
        item.turn != turn.id || matches!(item.body, ItemBody::UserMessage { .. })
    }) {
        return Step::default();
    }
    if !turn.observed && matches!(body, ItemBody::UserMessage { .. }) {
        return Step::default();
    }
    let position = if let Some(item) = state.items.get(&id) {
        item.position
    } else if let Some(position) = state.counters.item.checked_add(1) {
        position
    } else {
        return Step::default();
    };
    Step {
        changes: vec![
            Change::Item(Item {
                id,
                turn: turn.id.clone(),
                position,
                status,
                deferred,
                input: None,
                body,
            }),
            Change::Counters(Counters {
                item: state.counters.item.max(position),
                ..state.counters.clone()
            }),
        ],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

pub(super) fn finish(state: &State, turn: &Turn, terminal: TurnState, now: i64) -> Step {
    let held = !matches!(terminal, TurnState::Completed);
    let item_status = match &terminal {
        TurnState::Completed => ItemStatus::Completed,
        TurnState::Failed(_) => ItemStatus::Failed,
        _ => ItemStatus::Interrupted,
    };
    let mut changes = vec![Change::Turn(Turn {
        state: terminal,
        error: None,
        completed_at: turn.completed_at.or(Some(now)),
        duration_ms: turn.duration_ms.or_else(|| {
            turn.started_at
                .and_then(|started| now.checked_sub(started))
                .and_then(|elapsed| u64::try_from(elapsed).ok())
        }),
        ..turn.clone()
    })];
    if held {
        changes.push(Change::Attempt {
            turn: turn.id.clone(),
            value: None,
        });
        changes.push(Change::Thread(Thread {
            queue_held: state.has_queued_inputs(),
            ..state.thread.clone()
        }));
    }
    // A pending steer can still receive its own acknowledgement after normal
    // completion. An ambiguous steer has no such callback left to await.
    changes.extend(fence_inputs(&state.inputs, &turn.id, held));
    changes.extend(
        state
            .requests
            .values()
            .filter(|request| request.turn == turn.id)
            .map(|request| Change::RemoveRequest(request.id.clone())),
    );
    changes.extend(
        state
            .items
            .values()
            .filter(|item| item.turn == turn.id && item.status == ItemStatus::Running)
            .map(|item| {
                Change::Item(Item {
                    status: item_status,
                    ..item.clone()
                })
            }),
    );
    // A retry timer may have fired while a newer foreground turn was running.
    // Finishing that turn must wake the older retry again.
    let retry = state
        .turns
        .values()
        .filter(|other| {
            other.id != turn.id
                && matches!(
                    other.state,
                    TurnState::Running { .. } | TurnState::Waiting(_)
                )
        })
        .max_by_key(|other| other.ordinal)
        .and_then(|other| match other.state {
            TurnState::Waiting(Wait::Retry { at }) => Some(Action::WaitUntil(at)),
            _ => None,
        });
    Step {
        changes,
        actions: retry.into_iter().collect(),
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn fence_inputs(
    inputs: &BTreeMap<ClientInputId, PendingInput>,
    turn: &TurnId,
    include_pending: bool,
) -> Vec<Change> {
    inputs
        .values()
        .filter_map(|input| {
            if &input.live_delivery()?.attempt.turn != turn
                || (!include_pending && !matches!(input.state, InputState::Unknown { .. }))
            {
                return None;
            }
            input.fenced()
        })
        .collect()
}

fn retry(state: &State, turn: &Turn, at: i64, error: Option<ExecutionError>) -> Step {
    let mut changes = vec![
        Change::Turn(Turn {
            state: TurnState::Waiting(Wait::Retry { at }),
            retry_count: turn.retry_count.saturating_add(1),
            error,
            ..turn.clone()
        }),
        Change::Attempt {
            turn: turn.id.clone(),
            value: None,
        },
    ];
    changes.extend(
        state
            .requests
            .values()
            .filter(|request| request.turn == turn.id)
            .filter_map(|request| match request.state {
                RequestState::Awaiting => Some(Change::RemoveRequest(request.id.clone())),
                RequestState::Sending { .. } => Some(Change::Request(Request {
                    state: RequestState::Unknown,
                    ..request.clone()
                })),
                RequestState::Unknown => None,
            }),
    );
    changes.extend(fence_inputs(&state.inputs, &turn.id, true));
    changes.push(Change::Thread(Thread {
        queue_held: state.has_queued_inputs(),
        ..state.thread.clone()
    }));
    Step {
        changes,
        actions: vec![Action::WaitUntil(at)],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn is_compaction(body: &ItemBody) -> bool {
    matches!(body, ItemBody::UserMessage { content, .. }
        if matches!(&content[..], [crate::items::MessagePart::Text { text }] if text.trim().eq_ignore_ascii_case("/compact")))
}

fn failed(state: &State, turn: &Turn, error: Option<ExecutionError>, context: Context) -> Step {
    let error = error.or_else(|| turn.error.clone()).unwrap_or_default();
    const DELAYS: [i64; 5] = [2_000, 4_000, 8_000, 16_000, 30_000];
    if matches!(turn.state, TurnState::Running { interrupt: Some(_) }) {
        return finish(state, turn, TurnState::Interrupted, context.now);
    }
    if turn.provider == ProviderKind::Codex
        && !state
            .items
            .values()
            .any(|item| item.turn == turn.id && is_compaction(&item.body))
        && error.provider_code.as_deref() == Some("serverOverloaded")
        && !error.retry.as_ref().is_some_and(|retry| retry.retrying)
        && let Some(delay) = DELAYS.get(usize::from(turn.retry_count))
    {
        let mut error = error;
        error.retry = Some(crate::execution::RetryEvidence {
            retrying: true,
            attempt: Some(u32::from(turn.retry_count) + 1),
            max_attempts: Some(5),
        });
        return retry(state, turn, context.now.saturating_add(*delay), Some(error));
    }
    let mut error = error;
    error.retry = None;
    finish(state, turn, TurnState::Failed(error), context.now)
}

fn observed_start(
    state: &State,
    execution: Uuid,
    provider: ProviderKind,
    source: String,
    context: Context,
) -> Step {
    if provider != state.thread.provider
        || state
            .attempts
            .values()
            .any(|attempt| attempt.id == execution)
        || !matches!(state.bindings.get(&provider), Some(Binding::Ready { source: current, .. }) if current == &source)
        || state
            .active_turn()
            .is_some_and(|turn| !matches!(turn.state, TurnState::Waiting(Wait::Background { .. })))
    {
        return Step::default();
    }
    let Some(ordinal) = state.counters.turn.checked_add(1) else {
        return Step::default();
    };
    let turn: TurnId = allocate(context.seed, "turn").to_string().into();
    Step {
        changes: vec![
            Change::Turn(Turn {
                id: turn.clone(),
                ordinal,
                provider,
                queued: false,
                observed: true,
                started_at: Some(context.now),
                completed_at: None,
                duration_ms: None,
                retry_count: 0,
                error: None,
                state: TurnState::Running { interrupt: None },
            }),
            Change::Binding {
                provider,
                value: Some(Binding::Ready {
                    source: source.clone(),
                    covered: ordinal,
                }),
            },
            Change::Attempt {
                turn: turn.clone(),
                value: Some(Attempt {
                    id: execution,
                    turn,
                    provider,
                    source,
                }),
            },
            Change::Counters(Counters {
                turn: ordinal,
                ..state.counters.clone()
            }),
        ],
        outcome: Outcome::Accepted,
        ..Default::default()
    }
}
