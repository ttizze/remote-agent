use super::*;

pub(super) fn event(state: &State, event: Event, context: Context) -> Step {
    match event {
        Event::Wake => wake(state, context),
        Event::Prepared {
            id,
            action,
            cwd,
            source,
        } => prepared(state, &id, action, cwd, source, context),
        Event::PreparationFailed { id, action, error } => {
            preparation_failed(state, &id, action, error)
        }
        Event::Delivered { id, action } => {
            let result = delivered(state, &id, action, context);
            if result.outcome == Outcome::Accepted {
                return commands::append_advance(state, result, context);
            }
            result
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
            Step {
                changes: vec![
                    input.fenced().expect("selected live delivery"),
                    Change::Thread(Thread {
                        queue_held: state.has_queued_inputs(),
                        ..state.thread.clone()
                    }),
                ],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        Event::Provider {
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
        Event::Command { .. } | Event::Restart { .. } => unreachable!("handled by step"),
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
    let Workspace::Ready(cwd) = &state.thread.workspace else {
        return Step::default();
    };
    let Some(Binding::Ready { source }) = state.bindings.get(&state.thread.provider) else {
        return Step::default();
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
        PreparationStage::Workspace => Some(PreparationStage::Worktree),
        PreparationStage::Worktree => Some(PreparationStage::Provider),
        PreparationStage::Provider => None,
    };
    let workspace = match stage {
        PreparationStage::Workspace => Workspace::Folder(cwd.clone()),
        PreparationStage::Worktree => Workspace::Ready(cwd.clone()),
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
            started_at: context.now,
            state: TurnState::Running {
                interrupt: delivery.interrupt,
            },
        }));
        changes.push(Change::Attempt {
            turn: turn.clone(),
            value: Some(attempt.clone()),
        });
    }
    Step {
        changes,
        actions: delivery
            .interrupt
            .map(|action| Action::Interrupt {
                attempt: attempt.clone(),
                action,
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
    ) && !(turn.state == TurnState::Completed && matches!(observation, Observation::Item { .. }))
    {
        return Step::default();
    }
    let completed = matches!(observation, Observation::Completed);
    let stopping = matches!(turn.state, TurnState::Running { interrupt: Some(_) });
    let result = match observation {
        Observation::Retry { .. } if stopping => finish(state, turn, TurnState::Interrupted),
        Observation::Background { tasks }
            if stopping && crate::execution::background_holds_completion(&tasks) =>
        {
            Step::default()
        }
        Observation::Completed => finish(state, turn, TurnState::Completed),
        Observation::Failed(error) => finish(state, turn, TurnState::Failed(error)),
        Observation::Interrupted => finish(state, turn, TurnState::Interrupted),
        Observation::Retry { at } => {
            let mut changes = vec![
                Change::Turn(Turn {
                    state: TurnState::Waiting(Wait::Retry { at }),
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
        Observation::Background { tasks } => {
            if !crate::execution::background_holds_completion(&tasks) {
                return commands::append_advance(
                    state,
                    finish(state, turn, TurnState::Completed),
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
        Observation::Item { id, status, body } => item(state, turn, id, status, body),
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
    };
    if completed {
        return commands::append_advance(state, result, context);
    }
    result
}

fn item(state: &State, turn: &Turn, id: ItemId, status: ItemStatus, body: ItemBody) -> Step {
    if state.items.get(&id).is_some_and(|item| {
        item.turn != turn.id || matches!(item.body, ItemBody::UserMessage { .. })
    }) {
        return Step::default();
    }
    if matches!(body, ItemBody::UserMessage { .. }) {
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

pub(super) fn finish(state: &State, turn: &Turn, terminal: TurnState) -> Step {
    let held = !matches!(terminal, TurnState::Completed);
    let item_status = match &terminal {
        TurnState::Completed => ItemStatus::Completed,
        TurnState::Failed(_) => ItemStatus::Failed,
        _ => ItemStatus::Interrupted,
    };
    let mut changes = vec![Change::Turn(Turn {
        state: terminal,
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
