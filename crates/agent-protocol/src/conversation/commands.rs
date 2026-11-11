use super::*;

fn reject(reason: &str) -> Step {
    Step {
        outcome: Outcome::Rejected(reason.into()),
        ..Step::default()
    }
}

fn accepted(changes: Vec<Change>) -> Step {
    Step {
        changes,
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

pub(super) fn command(
    state: &State,
    operation: OperationId,
    command: Command,
    context: Context,
) -> Step {
    match command {
        Command::Submit { id, content } => submit(state, operation, id, content, context),
        Command::Rename(name) => {
            if name.trim().is_empty() {
                return reject("name is empty");
            }
            accepted(vec![Change::Thread(Thread {
                name,
                ..state.thread.clone()
            })])
        }
        Command::ResumeQueue => {
            if state
                .inputs
                .values()
                .any(|input| matches!(input.state, InputState::Unknown { .. }))
            {
                return reject("resolve unknown delivery before resuming the queue");
            }
            let result = accepted(vec![Change::Thread(Thread {
                queue_held: false,
                ..state.thread.clone()
            })]);
            append_advance(state, result, context)
        }
        Command::HoldQueue => accepted(vec![Change::Thread(Thread {
            queue_held: state.has_queued_inputs(),
            ..state.thread.clone()
        })]),
        Command::RemoveInput(id) => match state.inputs.get(&id) {
            None => reject("input does not exist"),
            Some(input)
                if matches!(input.state, InputState::Preparing { .. })
                    || input.live_delivery().is_some() =>
            {
                reject("input is in flight")
            }
            Some(_) => append_advance(state, accepted(vec![Change::RemoveInput(id)]), context),
        },
        Command::ResolveUnknown { id, retry } => resolve_unknown(state, &id, retry),
        Command::RetryInput(id) => retry_input(state, &id, context),
        Command::ContinueAfterRestart {
            source,
            enabled,
            content,
        } => continue_after_restart(state, operation, &source, enabled, content, context),
        Command::Interrupt => interrupt(state, context),
        Command::Answer { id, answer } => answer_request(state, id, answer, context),
    }
}

fn resolve_unknown(state: &State, id: &ClientInputId, retry: bool) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return reject("input does not exist");
    };
    let InputState::Unknown { delivery, status } = &input.state else {
        return reject("input delivery is not unknown");
    };
    if matches!(status, UnknownState::Resolving { .. }) {
        return reject("input resolution is in progress");
    }
    let mut result = if *status == UnknownState::Live {
        Step {
            changes: vec![Change::Input(PendingInput {
                state: InputState::Unknown {
                    delivery: delivery.clone(),
                    status: UnknownState::Resolving { retry },
                },
                ..input.clone()
            })],
            actions: vec![Action::FenceDelivery {
                id: id.clone(),
                action: delivery.action,
                attempt: delivery.attempt.clone(),
            }],
            outcome: Outcome::Accepted,
            ..Step::default()
        }
    } else {
        accepted(vec![input.resolved(retry)])
    };
    // Resolution is not permission to drain the other held inputs.
    if retry {
        result.changes.push(Change::Thread(Thread {
            queue_held: true,
            ..state.thread.clone()
        }));
    }
    result
}

fn continue_after_restart(
    state: &State,
    operation: OperationId,
    source: &TurnId,
    enabled: bool,
    content: InputContent,
    context: Context,
) -> Step {
    let OperationId::Job(id) = operation else {
        return reject("restart continuation is internal");
    };
    if !enabled
        || state.active_turn().is_some()
        || (!state.thread.queue_held && state.has_queued_inputs())
        || state
            .turns
            .values()
            .max_by_key(|turn| turn.ordinal)
            .is_none_or(|turn| &turn.id != source || turn.state != TurnState::Interrupted)
        || state.inputs.values().any(|input| match &input.state {
            InputState::Delivering(_) | InputState::Preparing { .. } => true,
            // Restart fenced steers; their inputs remain for user disposition
            // and are never resent by this separate continuation.
            InputState::Unknown { delivery, .. } => !delivery.steer,
            _ => false,
        })
    {
        return Step::default();
    }
    submit(state, operation, id.to_string().into(), content, context)
}

fn retry_input(state: &State, id: &ClientInputId, context: Context) -> Step {
    let Some(input) = state.inputs.get(id) else {
        return reject("input does not exist");
    };
    let InputState::Failed(_) = input.state else {
        return reject("input has not failed");
    };
    let mut result = accepted(vec![Change::Input(PendingInput {
        state: InputState::Queued { steer: None },
        ..input.clone()
    })]);
    let next = state.apply(&result.changes);
    let delivery = dispatch(&next, id, context, DeliveryMode::Direct);
    result.changes.extend(delivery.changes);
    result.actions.extend(delivery.actions);
    result
}

fn submit(
    state: &State,
    operation: OperationId,
    id: ClientInputId,
    content: InputContent,
    context: Context,
) -> Step {
    let operation_uuid = match operation {
        OperationId::Client(id) | OperationId::Job(id) => id,
    };
    if Uuid::parse_str(id.as_str()) != Ok(operation_uuid) {
        return reject("input ID must be the operation UUID");
    }
    if state
        .inputs
        .keys()
        .map(|id| id.as_str())
        .chain(state.items.keys().map(|id| id.as_str()))
        .any(|id| Uuid::parse_str(id) == Ok(operation_uuid))
    {
        return reject("input ID is already in use");
    }
    if content.parts.is_empty() {
        return reject("input is empty");
    }
    if content
        .model
        .as_ref()
        .is_some_and(|model| model.provider != state.thread.provider)
    {
        return reject("model belongs to another provider");
    }
    let Some(position) = state.counters.input.checked_add(1) else {
        return reject("input positions exhausted");
    };
    let steer = if context.steers && content.follow_up == FollowUp::Steer {
        state.steerable_attempt().map(|attempt| attempt.id)
    } else {
        None
    };
    let input = PendingInput {
        id: id.clone(),
        position,
        content,
        state: InputState::Queued { steer },
    };
    let mut result = accepted(vec![
        Change::Input(input),
        Change::Counters(Counters {
            input: position,
            ..state.counters.clone()
        }),
    ]);
    if state.thread.queue_held && !state.has_queued_inputs() {
        result.changes.push(Change::Thread(Thread {
            queue_held: false,
            ..state.thread.clone()
        }));
    }
    let next = state.apply(&result.changes);
    let delivery = dispatch(&next, &id, context, DeliveryMode::Direct);
    result.changes.extend(delivery.changes);
    result.actions.extend(delivery.actions);
    result
}

fn interrupt(state: &State, context: Context) -> Step {
    let unstarted = state.inputs.values().find(|input| match &input.state {
        InputState::Preparing { .. } => true,
        _ => input
            .live_delivery()
            .is_some_and(|delivery| !delivery.steer),
    });
    if let Some(input) = unstarted {
        return interrupt_input(state, input, context);
    }
    let Some(turn) = state.active_turn() else {
        return reject("no active turn");
    };
    match &turn.state {
        TurnState::Running { .. } | TurnState::Waiting(Wait::Background { .. }) => {
            let Some(attempt) = state.attempts.get(&turn.id) else {
                return reject("active attempt is missing");
            };
            let action = allocate(context.seed, "interrupt");
            Step {
                changes: vec![
                    Change::Turn(Turn {
                        state: TurnState::Running {
                            interrupt: Some(action),
                        },
                        ..turn.clone()
                    }),
                    Change::Thread(Thread {
                        queue_held: state.has_queued_inputs(),
                        ..state.thread.clone()
                    }),
                ],
                actions: vec![Action::Interrupt {
                    attempt: attempt.clone(),
                    action,
                }],
                outcome: Outcome::Accepted,
                ..Step::default()
            }
        }
        TurnState::Waiting(Wait::Retry { .. }) => {
            execution::finish(state, turn, TurnState::Interrupted)
        }
        _ => reject("no active turn"),
    }
}

fn interrupt_input(state: &State, input: &PendingInput, context: Context) -> Step {
    let (next, actions) = match &input.state {
        InputState::Preparing { action, .. } => (
            InputState::Failed(ExecutionError {
                message: "preparation interrupted".into(),
                ..ExecutionError::default()
            }),
            vec![Action::CancelPreparation { action: *action }],
        ),
        _ => {
            let delivery = input.live_delivery().expect("selected live delivery");
            let delivery = PendingDelivery {
                interrupt: Some(
                    delivery
                        .interrupt
                        .unwrap_or_else(|| allocate(context.seed, "interrupt")),
                ),
                ..delivery.clone()
            };
            let next = if let InputState::Unknown { status, .. } = input.state {
                InputState::Unknown { delivery, status }
            } else {
                InputState::Delivering(delivery)
            };
            (next, Vec::new())
        }
    };
    Step {
        changes: vec![
            Change::Input(PendingInput {
                state: next,
                ..input.clone()
            }),
            Change::Thread(Thread {
                queue_held: state.has_queued_inputs(),
                ..state.thread.clone()
            }),
        ],
        actions,
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn answer_request(state: &State, id: RequestId, answer: Answer, context: Context) -> Step {
    let Some(request) = state
        .requests
        .get(&id)
        .filter(|request| request.state == RequestState::Awaiting)
    else {
        return reject("request is not awaiting an answer");
    };
    if let Err(reason) = crate::requests::validate_answer(&request.body, &answer) {
        return reject(&reason);
    }
    let Some(attempt) = state.attempts.get(&request.turn) else {
        return reject("request attempt has ended");
    };
    let action = allocate(context.seed, "answer");
    Step {
        changes: vec![Change::Request(Request {
            state: RequestState::Sending {
                action,
                answer: answer.clone(),
            },
            ..request.clone()
        })],
        actions: vec![Action::Answer {
            attempt: attempt.clone(),
            id,
            action,
            answer,
        }],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

pub(super) fn append_advance(state: &State, mut result: Step, context: Context) -> Step {
    let next = state.apply(&result.changes);
    let advanced = advance(&next, context);
    result.changes.extend(advanced.changes);
    result.actions.extend(advanced.actions);
    result
}

pub(super) fn advance(state: &State, context: Context) -> Step {
    let active = state.steerable_attempt();
    let candidate = state
        .inputs
        .values()
        .filter(|input| matches!(input.state, InputState::Queued { .. }))
        .min_by_key(|input| {
            let steering = context.steers
                && matches!(input.state, InputState::Queued { steer: Some(id) }
                if active.is_some_and(|attempt| attempt.id == id));
            (!steering, input.position)
        });
    candidate.map_or_else(Step::default, |input| {
        dispatch(state, &input.id, context, DeliveryMode::Queued)
    })
}

pub(super) fn dispatch(
    state: &State,
    id: &ClientInputId,
    context: Context,
    mode: DeliveryMode,
) -> Step {
    if state.inputs.values().any(|input| {
        matches!(
            input.state,
            InputState::Preparing { .. } | InputState::Delivering(_)
        ) || input
            .live_delivery()
            .is_some_and(|delivery| !delivery.steer)
    }) {
        return Step::default();
    }
    let Some(input) = state
        .inputs
        .get(id)
        .filter(|input| matches!(input.state, InputState::Queued { .. }))
    else {
        return Step::default();
    };
    let active = state
        .active_turn()
        .filter(|turn| !matches!(turn.state, TurnState::Waiting(Wait::Background { .. })));
    let steer = context.steers
        && matches!(input.state, InputState::Queued { steer: Some(id) }
            if state.steerable_attempt().is_some_and(|attempt| attempt.id == id));
    if active.is_some() && !steer {
        return Step::default();
    }
    let ahead = state.inputs.values().any(|ahead| {
        ahead.position < input.position && matches!(ahead.state, InputState::Queued { .. })
    });
    let blocked = match mode {
        DeliveryMode::Queued => state.thread.queue_held || ahead,
        DeliveryMode::Direct => !state.thread.queue_held && ahead,
    };
    if !steer && blocked {
        return Step::default();
    }
    let cwd = match &state.thread.workspace {
        Workspace::Missing => {
            return prepare(
                input,
                PreparationStage::Workspace,
                None,
                state.thread.provider,
                context.seed,
                mode,
            );
        }
        Workspace::Folder(cwd) => {
            return prepare(
                input,
                PreparationStage::Worktree,
                Some(cwd),
                state.thread.provider,
                context.seed,
                mode,
            );
        }
        Workspace::Ready(cwd) => cwd,
    };
    let Some(Binding::Ready { source }) = state
        .bindings
        .get(&state.thread.provider)
        .filter(|binding| matches!(binding, Binding::Ready { source } if !source.is_empty()))
    else {
        return prepare(
            input,
            PreparationStage::Provider,
            Some(cwd),
            state.thread.provider,
            context.seed,
            mode,
        );
    };
    let (attempt, ordinal) = if steer {
        let turn = active.expect("steer needs an active turn");
        let Some(attempt) = state.attempts.get(&turn.id) else {
            return Step::default();
        };
        (attempt.clone(), turn.ordinal)
    } else {
        let Some(ordinal) = state.counters.turn.checked_add(1) else {
            return Step::default();
        };
        (
            Attempt {
                id: allocate(context.seed, "attempt"),
                turn: allocate(context.seed, "turn").to_string().into(),
                provider: state.thread.provider,
                source: source.clone(),
            },
            ordinal,
        )
    };
    let action = allocate(context.seed, "delivery");
    let mut changes = vec![Change::Input(PendingInput {
        state: InputState::Delivering(PendingDelivery {
            action,
            attempt: attempt.clone(),
            ordinal,
            interrupt: None,
            steer,
        }),
        ..input.clone()
    })];
    if !steer {
        changes.push(Change::Counters(Counters {
            turn: ordinal,
            ..state.counters.clone()
        }));
    }
    Step {
        changes,
        actions: vec![Action::Deliver {
            id: id.clone(),
            action,
            attempt,
            cwd: cwd.clone(),
            content: input.content.clone(),
            steer,
        }],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}

fn prepare(
    input: &PendingInput,
    stage: PreparationStage,
    cwd: Option<&str>,
    provider: ProviderKind,
    seed: Uuid,
    mode: DeliveryMode,
) -> Step {
    let action = allocate(seed, "prepare");
    Step {
        changes: vec![Change::Input(PendingInput {
            state: InputState::Preparing {
                stage,
                action,
                mode,
            },
            ..input.clone()
        })],
        actions: vec![Action::Prepare {
            id: input.id.clone(),
            action,
            stage,
            cwd: cwd.map(str::to_owned),
            provider,
        }],
        outcome: Outcome::Accepted,
        ..Step::default()
    }
}
