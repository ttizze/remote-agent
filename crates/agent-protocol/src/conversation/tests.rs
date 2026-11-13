use super::*;
use crate::items::AssistantPhase;

fn context(seed: u128) -> Context<'static> {
    Context {
        preparation: None,
        rewind_binding: None,
        external_busy: false,
        now: 100,
        seed: Uuid::from_u128(seed),
        steers: true,
        steer_ready: true,
    }
}

fn state() -> State {
    let mut state = State::new(Thread {
        name: "Conversation".into(),
        workspace: Workspace::Ready("/project".into()),
        provider: ProviderKind::Claude,
        queue_held: false,
    });
    state.bindings.insert(
        ProviderKind::Claude,
        Binding::Ready {
            covered: 0,
            source: "native-session".into(),
        },
    );
    state
}

fn content(follow_up: FollowUp) -> InputContent {
    InputContent {
        parts: vec![InputPart::Text {
            text: "hello".into(),
        }],
        model: None,
        effort: None,
        service_tier: None,
        follow_up,
    }
}

fn command(state: &State, seed: u128, command: Command) -> Step {
    let result = step(
        state,
        Event::Command {
            id: OperationId::Client(Uuid::from_u128(seed)),
            command,
            receipt: None,
        },
        context(seed),
    );
    if !result.changes.is_empty() {
        assert_eq!(result.outcome, Outcome::Accepted);
    }
    result
}

fn submit(state: &State, seed: u128, follow_up: FollowUp) -> Step {
    command(
        state,
        seed,
        Command::Submit {
            id: Uuid::from_u128(seed).to_string().into(),
            content: content(follow_up),
        },
    )
}

fn starting(state: &State, seed: u128) -> (State, ClientInputId, Uuid, Attempt) {
    let result = submit(state, seed, FollowUp::Queue);
    let Action::Deliver {
        id,
        action,
        attempt,
        ..
    } = &result.actions[0]
    else {
        panic!("expected delivery")
    };
    (
        state.apply(&result.changes),
        id.clone(),
        *action,
        attempt.clone(),
    )
}

// An ambiguous native write can have changed provider context. Explicit retry
// must prepare a fresh source before sending, while preserving the input ID.
fn deliver_after_rebuilding(state: &State, resumed: &Step, seed: u128) -> Step {
    let [
        Action::Prepare {
            id,
            action,
            stage: PreparationStage::Provider,
            ..
        },
    ] = &resumed.actions[..]
    else {
        panic!("ambiguous delivery must rebuild its provider source");
    };
    let pending = state.apply(&resumed.changes);
    step(
        &pending,
        Event::Prepared {
            id: id.clone(),
            action: *action,
            cwd: Some("/project".into()),
            source: Some("rebuilt-native-session".into()),
        },
        context(seed),
    )
}

fn running() -> (State, Attempt) {
    let (state, id, action, attempt) = starting(&state(), 1);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(2),
    );
    (state.apply(&delivered.changes), attempt)
}

#[test]
fn failed_interrupts_allow_retry_without_replacing_a_newer_interrupt_or_losing_background_work() {
    for background in [
        Vec::new(),
        vec![BackgroundTask {
            id: "task".into(),
            kind: crate::execution::BackgroundTaskKind::Command,
            description: None,
        }],
    ] {
        let (mut state, attempt) = running();
        if !background.is_empty() {
            state = state.apply(
                &observe(
                    &state,
                    &attempt,
                    Observation::Background {
                        tasks: background.clone(),
                    },
                    5,
                )
                .changes,
            );
        }
        let stopping = command(&state, 6, Command::Interrupt { turn: None });
        let Action::Interrupt { action, .. } = stopping.actions[0] else {
            panic!("interrupt missing")
        };
        state = state.apply(&stopping.changes);
        let failed = step(
            &state,
            Event::InterruptResult {
                attempt: attempt.id,
                action,
                error: Some(ExecutionError {
                    message: "interrupt not sent".into(),
                    ..Default::default()
                }),
                background: background.clone(),
            },
            context(7),
        );
        state = state.apply(&failed.changes);
        assert_eq!(
            state.turns[&attempt.turn].error.as_ref().unwrap().message,
            "interrupt not sent"
        );
        if background.is_empty() {
            assert_eq!(
                state.turns[&attempt.turn].state,
                TurnState::Running { interrupt: None }
            );
        } else {
            assert_eq!(
                state.turns[&attempt.turn].state,
                TurnState::Waiting(Wait::Background {
                    tasks: background.clone()
                })
            );
        }
        let retry = command(&state, 8, Command::Interrupt { turn: None });
        assert_eq!(retry.actions.len(), 1);
        state = state.apply(&retry.changes);
        let stale = step(
            &state,
            Event::InterruptResult {
                attempt: attempt.id,
                action,
                error: Some(ExecutionError::default()),
                background,
            },
            context(9),
        );
        assert!(stale.changes.is_empty());
    }
}

#[test]
fn stopping_background_work_retains_the_answer_and_the_work_that_keeps_running() {
    let task = BackgroundTask {
        id: "agent".into(),
        kind: crate::execution::BackgroundTaskKind::Subagent,
        description: Some("Review".into()),
    };
    for remaining in [vec![task.clone()], vec![]] {
        let (state, attempt) = running();
        let waiting = observe(
            &state,
            &attempt,
            Observation::Background {
                tasks: vec![task.clone()],
            },
            3,
        );
        let state = state.apply(&waiting.changes);
        let stop = command(&state, 4, Command::Interrupt { turn: None });
        let Action::Interrupt {
            action,
            background: true,
            ..
        } = stop.actions[0]
        else {
            panic!("background stop")
        };
        let state = state.apply(&stop.changes);
        let result = step(
            &state,
            Event::InterruptResult {
                attempt: attempt.id,
                action,
                error: None,
                background: remaining.clone(),
            },
            context(5),
        );
        let state = state.apply(&result.changes);
        let turn = &state.turns[&attempt.turn];
        assert!(turn.error.is_none());
        assert_eq!(
            turn.state,
            if remaining.is_empty() {
                TurnState::Completed
            } else {
                TurnState::Waiting(Wait::Background { tasks: remaining })
            }
        );
        let stale = step(
            &state,
            Event::InterruptResult {
                attempt: attempt.id,
                action,
                error: None,
                background: vec![],
            },
            context(6),
        );
        assert!(stale.changes.is_empty());
    }
}

#[test]
fn delayed_stop_cannot_cancel_a_different_turn_or_a_new_preparation() {
    let (state, attempt) = running();
    let rejected = command(
        &state,
        20,
        Command::Interrupt {
            turn: Some("older-turn".into()),
        },
    );
    assert!(matches!(rejected.outcome, Outcome::Rejected(_)));
    assert!(rejected.changes.is_empty());
    assert!(rejected.actions.is_empty());
    let stopped = command(
        &state,
        21,
        Command::Interrupt {
            turn: Some(attempt.turn),
        },
    );
    assert!(matches!(&stopped.actions[..], [Action::Interrupt { .. }]));

    let state = State::new(Thread {
        workspace: Workspace::Missing,
        ..state.thread
    });
    let submitted = submit(&state, 22, FollowUp::Queue);
    let state = state.apply(&submitted.changes);
    let rejected = command(
        &state,
        23,
        Command::Interrupt {
            turn: Some("older-input".into()),
        },
    );
    assert!(matches!(rejected.outcome, Outcome::Rejected(_)));
    assert!(rejected.actions.is_empty());
    let input = state.inputs.keys().next().unwrap();
    let stopped = command(
        &state,
        24,
        Command::Interrupt {
            turn: Some(input.to_string().into()),
        },
    );
    assert!(matches!(
        &stopped.actions[..],
        [Action::CancelPreparation { .. }]
    ));
}

fn observe(state: &State, attempt: &Attempt, observation: Observation, seed: u128) -> Step {
    let result = step(
        state,
        Event::Provider {
            native_items: Vec::new(),
            native_turn: None,
            attempt: attempt.id,
            observation,
        },
        context(seed),
    );
    if !result.changes.is_empty() {
        assert_eq!(result.outcome, Outcome::Accepted);
    }
    result
}

#[test]
fn admission_stores_input_before_delivery_and_ack_moves_the_same_id_into_history() {
    let initial = state();
    let (pending, id, action, attempt) = starting(&initial, 1);
    assert!(pending.turns.is_empty());
    assert!(pending.items.is_empty());
    assert!(matches!(
        pending.inputs[&id].state,
        InputState::Delivering(_)
    ));
    let delivered = step(
        &pending,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action,
        },
        context(2),
    );
    let state = pending.apply(&delivered.changes);
    assert!(state.inputs.is_empty());
    assert_eq!(state.items[&ItemId(id.0.clone())].turn, attempt.turn);
    assert_eq!(
        state.turns[&attempt.turn].state,
        TurnState::Running { interrupt: None }
    );
    let duplicate = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(3),
    );
    assert_eq!(duplicate, Step::default());
    assert_eq!(initial, self::state());
}

#[test]
fn queued_edits_and_moves_keep_identity_and_never_modify_in_flight_input() {
    let (mut state, _) = running();
    for seed in [3, 4, 5] {
        state = state.apply(&submit(&state, seed, FollowUp::Queue).changes);
    }
    let id = |seed| ClientInputId(Uuid::from_u128(seed).to_string());
    let moved = command(
        &state,
        6,
        Command::MoveInput {
            id: id(5),
            before: Some(id(3)),
        },
    );
    state = state.apply(&moved.changes);
    let ordered = |state: &State| {
        let mut rows: Vec<_> = state.inputs.values().collect();
        rows.sort_by_key(|input| input.position);
        rows.into_iter()
            .map(|input| input.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ordered(&state), [id(5), id(3), id(4)]);
    let edited = command(
        &state,
        7,
        Command::EditInput {
            id: id(3),
            parts: vec![InputPart::Text {
                text: "changed".into(),
            }],
        },
    );
    state = state.apply(&edited.changes);
    assert_eq!(ordered(&state), [id(5), id(3), id(4)]);
    assert_eq!(
        state.inputs[&id(3)].content.parts,
        [InputPart::Text {
            text: "changed".into()
        }]
    );
    assert!(matches!(
        command(
            &state,
            8,
            Command::EditInput {
                id: id(3),
                parts: vec![]
            }
        )
        .outcome,
        Outcome::Rejected(_)
    ));
    let (pending, input, _, _) = starting(&self::state(), 11);
    assert!(matches!(
        command(
            &pending,
            12,
            Command::MoveInput {
                id: input.clone(),
                before: None
            }
        )
        .outcome,
        Outcome::Rejected(_)
    ));
    assert!(matches!(
        command(
            &pending,
            13,
            Command::EditInput {
                id: input,
                parts: content(FollowUp::Queue).parts
            }
        )
        .outcome,
        Outcome::Rejected(_)
    ));
}

#[test]
fn steering_a_selected_queued_input_does_not_drain_the_inputs_before_it() {
    let (mut state, attempt) = running();
    for seed in [3, 4] {
        state = state.apply(&submit(&state, seed, FollowUp::Queue).changes);
    }
    let selected: ClientInputId = Uuid::from_u128(4).to_string().into();
    let steer = command(&state, 5, Command::SteerInput(selected.clone()));
    assert!(
        matches!(&steer.actions[..], [Action::Deliver { id, attempt: current, steer: true, .. }] if id == &selected && current == &attempt)
    );
    let next = state.apply(&steer.changes);
    assert!(matches!(
        next.inputs[Uuid::from_u128(3).to_string().as_str()].state,
        InputState::Queued { .. }
    ));
    assert_eq!(state.inputs.len(), 2);
}

#[test]
fn rewind_rejects_work_running_outside_the_saved_history() {
    let (state, attempt) = running();
    let state = state.apply(&observe(&state, &attempt, Observation::Completed, 4).changes);
    let result = step(
        &state,
        Event::Command {
            id: OperationId::Client(Uuid::from_u128(u128::MAX - 1)),
            receipt: None,
            command: Command::Rewind(attempt.turn),
        },
        Context {
            external_busy: true,
            ..context(5)
        },
    );
    assert_eq!(
        result.outcome,
        Outcome::Rejected("conversation is busy".into())
    );
    assert!(result.changes.is_empty());
    assert!(result.actions.is_empty());
}

#[test]
fn rewind_retains_rows_and_counters_and_invalidates_native_bindings() {
    let (state, attempt) = running();
    assert!(matches!(
        command(&state, 3, Command::Rewind(attempt.turn.clone())).outcome,
        Outcome::Rejected(_)
    ));
    let completed = observe(&state, &attempt, Observation::Completed, 4);
    let mut state = state.apply(&completed.changes);
    state.thread.queue_held = true;
    let before = state.clone();
    let rewound = command(&state, 5, Command::Rewind(attempt.turn.clone()));
    let state = state.apply(&rewound.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::RolledBack);
    assert_eq!(state.items, before.items);
    assert_eq!(state.counters, before.counters);
    assert!(state.bindings.is_empty());
    assert!(state.attempts.is_empty());
    assert!(
        !state.thread.queue_held,
        "rewinding without queued input releases the old hold"
    );
    assert!(
        observe(&state, &attempt, Observation::Completed, 6)
            .changes
            .is_empty()
    );
    let next = submit(&state, 7, FollowUp::Queue);
    assert!(matches!(
        &next.actions[..],
        [Action::Prepare {
            stage: PreparationStage::Provider,
            ..
        }]
    ));
}

#[test]
fn switching_provider_keeps_the_root_and_rebuilds_the_binding_from_confirmed_history() {
    let state = state();
    let mut input = content(FollowUp::Queue);
    input.model = Some(ModelRef {
        provider: ProviderKind::Codex,
        id: "selected".into(),
    });
    let switched = command(
        &state,
        3,
        Command::Submit {
            id: Uuid::from_u128(3).to_string().into(),
            content: input.clone(),
        },
    );
    let next = state.apply(&switched.changes);
    assert_eq!(next.thread.provider, ProviderKind::Codex);
    assert!(matches!(
        &switched.actions[..],
        [Action::Prepare {
            provider: ProviderKind::Codex,
            stage: PreparationStage::Provider,
            ..
        }]
    ));
    let (busy, _) = running();
    assert!(matches!(
        command(
            &busy,
            3,
            Command::Submit {
                id: Uuid::from_u128(3).to_string().into(),
                content: input
            }
        )
        .outcome,
        Outcome::Rejected(_)
    ));
}

#[test]
fn receipts_replay_acceptance_and_rejection_without_new_effects() {
    let initial = state();
    for follow_up in [FollowUp::Queue, FollowUp::Steer] {
        let command = Command::Submit {
            id: Uuid::from_u128(1).to_string().into(),
            content: content(follow_up),
        };
        for receipt in [Outcome::Accepted, Outcome::Rejected("not permitted".into())] {
            let result = step(
                &initial,
                Event::Command {
                    id: OperationId::Client(Uuid::from_u128(1)),
                    command: command.clone(),
                    receipt: Some(receipt.clone()),
                },
                context(99),
            );
            assert_eq!(result.outcome, receipt);
            assert!(result.changes.is_empty());
            assert!(result.actions.is_empty());
            assert!(result.receipt.is_none());
        }
    }
    assert_ne!(
        OperationId::Client(Uuid::nil()).receipt_key(),
        OperationId::Job(Uuid::nil()).receipt_key()
    );
}

#[test]
fn events_before_the_start_ack_prove_delivery_and_the_late_ack_is_harmless() {
    let (pending, id, action, attempt) = starting(&state(), 1);
    let completed = observe(&pending, &attempt, Observation::Completed, 2);
    let state = pending.apply(&completed.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Completed);
    assert!(state.inputs.is_empty());
    assert_eq!(state.items.len(), 1);
    assert!(
        step(
            &state,
            Event::Delivered {
                native_turn: None,
                id,
                action
            },
            context(3)
        )
        .changes
        .is_empty()
    );
}

#[test]
fn normal_input_waits_and_only_the_queue_head_starts_after_completion() {
    let (state, attempt) = running();
    let second = submit(&state, 3, FollowUp::Queue);
    assert!(second.actions.is_empty());
    let state = state.apply(&second.changes);
    let third = submit(&state, 4, FollowUp::Queue);
    let state = state.apply(&third.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 5);
    assert!(
        matches!(&completed.actions[..], [Action::Deliver { id, steer: false, .. }]
        if id.as_str() == Uuid::from_u128(3).to_string())
    );
    let state = state.apply(&completed.changes);
    assert!(step(&state, Event::Wake, context(6)).actions.is_empty());
}

#[test]
fn steering_joins_the_current_attempt_without_overtaking_queued_turns() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let steer = submit(&state, 4, FollowUp::Steer);
    assert!(
        matches!(&steer.actions[..], [Action::Deliver { attempt: target, steer: true, .. }] if target == &attempt)
    );
    let state = state.apply(&steer.changes);
    let (id, action) = match &steer.actions[0] {
        Action::Deliver { id, action, .. } => (id.clone(), *action),
        _ => unreachable!(),
    };
    let ack = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(5),
    );
    let state = state.apply(&ack.changes);
    assert_eq!(state.turns.len(), 1);
    assert_eq!(state.items.len(), 2);
    assert_eq!(state.inputs.len(), 1);
}

#[test]
fn an_unsupported_steer_queues_and_failed_steering_keeps_the_live_attempt() {
    let (state, attempt) = running();
    let event = Event::Command {
        id: OperationId::Client(Uuid::from_u128(3)),
        command: Command::Submit {
            id: Uuid::from_u128(3).to_string().into(),
            content: content(FollowUp::Steer),
        },
        receipt: None,
    };
    let queued = step(
        &state,
        event.clone(),
        Context {
            external_busy: false,
            steers: false,
            ..context(3)
        },
    );
    assert!(queued.actions.is_empty());
    let steer = step(&state, event, context(3));
    let state = state.apply(&steer.changes);
    let Action::Deliver { id, action, .. } = &steer.actions[0] else {
        unreachable!()
    };
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(4),
    );
    assert_eq!(failed.outcome, Outcome::Accepted);
    let state = state.apply(&failed.changes);
    assert_eq!(state.attempts[&attempt.turn], attempt);
    assert!(!state.thread.queue_held);
    assert!(matches!(
        command(&state, 5, Command::ResumeQueue).outcome,
        Outcome::Rejected(_)
    ));
}

#[test]
fn restart_never_resends_an_ambiguous_start_and_rejects_its_late_evidence() {
    let (state, id, action, attempt) = starting(&state(), 1);
    let recovery = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(2),
    );
    assert!(recovery.actions.is_empty());
    assert!(recovery.continuations.is_empty());
    let state = state.apply(&recovery.changes);
    assert!(matches!(&state.inputs[&id].state,
        InputState::Unknown { delivery, status: UnknownState::Fenced, .. } if delivery.attempt.turn == attempt.turn));
    assert!(state.attempts.is_empty());
    assert_eq!(
        step(
            &state,
            Event::Delivered {
                native_turn: None,
                id: id.clone(),
                action
            },
            context(3)
        ),
        Step::default()
    );
    assert_eq!(
        observe(&state, &attempt, Observation::Completed, 4),
        Step::default()
    );
    let resolved = command(&state, 5, Command::ResolveUnknown { id, retry: true });
    let state = state.apply(&resolved.changes);
    assert!(step(&state, Event::Wake, context(6)).actions.is_empty());
    let resumed = command(&state, 7, Command::ResumeQueue);
    let delivered = deliver_after_rebuilding(&state, &resumed, 8);
    assert!(
        matches!(&delivered.actions[..], [Action::Deliver { attempt: next, .. }]
            if next.id != attempt.id && next.source != attempt.source)
    );
}

#[test]
fn interrupted_and_failed_turns_hold_the_queue_and_fence_late_events() {
    for terminal in [
        Observation::Interrupted,
        Observation::Failed(Some(ExecutionError::default())),
    ] {
        let (state, attempt) = running();
        let queued = submit(&state, 3, FollowUp::Queue);
        let state = state.apply(&queued.changes);
        let end = observe(&state, &attempt, terminal, 4);
        let state = state.apply(&end.changes);
        assert!(state.thread.queue_held);
        assert!(end.actions.is_empty());
        assert_eq!(
            observe(&state, &attempt, Observation::Completed, 5),
            Step::default()
        );
        assert!(matches!(
            command(&state, 6, Command::ResumeQueue).actions.as_slice(),
            [Action::Deliver { .. }]
        ));
    }
}

#[test]
fn recovery_continuation_is_once_and_respects_settings_and_explicit_interrupts() {
    let (state, attempt) = running();
    for (updating, enabled, expected) in [(false, true, 0), (true, false, 0), (true, true, 1)] {
        let recovered = step(
            &state,
            Event::Restart {
                updating,
                continue_after_restart: enabled,
            },
            context(3),
        );
        assert_eq!(recovered.continuations.len(), expected);
        let next = state.apply(&recovered.changes);
        assert_eq!(next.turns[&attempt.turn].state, TurnState::Interrupted);
        let again = step(
            &next,
            Event::Restart {
                updating,
                continue_after_restart: enabled,
            },
            context(4),
        );
        assert!(again.continuations.is_empty());
    }
    let interrupted = command(&state, 5, Command::Interrupt { turn: None });
    let state = state.apply(&interrupted.changes);
    let recovered = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(6),
    );
    assert!(recovered.continuations.is_empty());
}

#[test]
fn update_recovery_does_not_continue_a_turn_waiting_for_user_input() {
    for body in [approval(), RequestBody::Question { questions: vec![] }] {
        let (state, attempt) = running();
        let requested = observe(
            &state,
            &attempt,
            Observation::Request {
                id: "waiting".into(),
                body,
            },
            3,
        );
        let state = state.apply(&requested.changes);
        let recovered = step(
            &state,
            Event::Restart {
                updating: true,
                continue_after_restart: true,
            },
            context(4),
        );
        assert!(recovered.continuations.is_empty());
        assert!(recovered.actions.is_empty());
        let state = state.apply(&recovered.changes);
        assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
        assert!(state.requests.is_empty());
    }
}

#[test]
fn continuation_bypasses_the_held_queue_without_releasing_other_inputs() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let recovered = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(4),
    );
    let state = state.apply(&recovered.changes);
    let continuation = &recovered.continuations[0];
    for enabled in [false, true] {
        let resumed = step(
            &state,
            Event::Command {
                id: OperationId::Job(continuation.id),
                receipt: None,
                command: Command::ContinueAfterRestart {
                    source: attempt.turn.clone(),
                    enabled,
                    content: content(FollowUp::Queue),
                },
            },
            context(5),
        );
        assert_eq!(resumed.actions.len(), usize::from(enabled));
        assert!(state.apply(&resumed.changes).thread.queue_held);
    }
}

#[test]
fn retry_wait_survives_restart_and_only_the_due_wake_creates_a_new_attempt() {
    let (state, attempt) = codex_running();
    let waiting = observe(&state, &attempt, Observation::Failed(Some(overload())), 3);
    assert_eq!(waiting.actions, vec![Action::WaitUntil(2100)]);
    let state = state.apply(&waiting.changes);
    let recovered = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(4),
    );
    assert_eq!(recovered.actions, vec![Action::WaitUntil(2100)]);
    assert!(recovered.continuations.is_empty());
    let state = state.apply(&recovered.changes);
    let early = step(
        &state,
        Event::Wake,
        Context {
            external_busy: false,
            now: 2099,
            ..context(5)
        },
    );
    assert_eq!(early.actions, vec![Action::WaitUntil(2100)]);
    assert!(early.changes.is_empty());
    let due = step(
        &state,
        Event::Wake,
        Context {
            external_busy: false,
            now: 2100,
            ..context(6)
        },
    );
    assert_eq!(due.outcome, Outcome::Accepted);
    assert!(
        matches!(&due.actions[..], [Action::Resume { attempt: next, .. }] if next.id != attempt.id && next.turn == attempt.turn)
    );
    let state = state.apply(&due.changes);
    assert!(step(&state, Event::Wake, context(7)).actions.is_empty());
    assert_eq!(
        observe(&state, &attempt, Observation::Completed, 8),
        Step::default()
    );
}

#[test]
fn due_retry_without_a_connection_or_workspace_finishes_and_holds_the_queue() {
    for missing_workspace in [false, true] {
        let (state, attempt) = codex_running();
        let state = state.apply(&submit(&state, 2, FollowUp::Queue).changes);
        let mut state = state
            .apply(&observe(&state, &attempt, Observation::Failed(Some(overload())), 3).changes);
        if missing_workspace {
            state.thread.workspace = Workspace::Missing;
        } else {
            state.bindings.clear();
        }
        let recovered = step(
            &state,
            Event::Restart {
                updating: false,
                continue_after_restart: false,
            },
            context(4),
        );
        let state = state.apply(&recovered.changes);
        let due = step(
            &state,
            Event::Wake,
            Context {
                now: 2100,
                ..context(5)
            },
        );
        let settled = state.apply(&due.changes);
        assert!(settled.active_turn().is_none());
        assert!(matches!(
            settled.turns[&attempt.turn].state,
            TurnState::Failed(_)
        ));
        assert!(due.actions.is_empty());
        assert!(settled.thread.queue_held);
        assert!(settled.has_queued_inputs());
    }
}

#[test]
fn preparation_resumes_the_saved_stage_and_discards_old_action_results() {
    let initial = State::new(Thread {
        workspace: Workspace::Missing,
        ..state().thread
    });
    let pending = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&pending.changes);
    let Action::Prepare {
        id,
        action,
        stage: PreparationStage::Workspace,
        ..
    } = &pending.actions[0]
    else {
        panic!("workspace")
    };
    let provider = step(
        &state,
        Event::Prepared {
            id: id.clone(),
            action: *action,
            cwd: Some("/prepared".into()),
            source: None,
        },
        context(2),
    );
    assert!(matches!(
        &provider.actions[..],
        [Action::Prepare {
            stage: PreparationStage::Provider,
            ..
        }]
    ));
    let state = state.apply(&provider.changes);
    assert_eq!(state.thread.workspace, Workspace::Ready("/prepared".into()));
    let recovered = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(3),
    );
    assert!(matches!(
        &recovered.actions[..],
        [Action::Prepare {
            stage: PreparationStage::Provider,
            ..
        }]
    ));
    let next = state.apply(&recovered.changes);
    assert_eq!(
        step(
            &next,
            Event::Prepared {
                id: id.clone(),
                action: *action,
                cwd: Some("/wrong".into()),
                source: None
            },
            context(4)
        ),
        Step::default()
    );
    assert_eq!(next.thread.workspace.cwd(), Some("/prepared"));
}

#[test]
fn failed_preparation_retries_the_same_stage_with_a_new_action() {
    let initial = State::new(Thread {
        workspace: Workspace::Missing,
        ..state().thread
    });
    let submitted = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&submitted.changes);
    let Action::Prepare { id, action, .. } = &submitted.actions[0] else {
        unreachable!()
    };
    let failed = step(
        &state,
        Event::PreparationFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
        },
        context(2),
    );
    assert_eq!(failed.outcome, Outcome::Accepted);
    let state = state.apply(&failed.changes);
    let retried = command(
        &state,
        3,
        Command::RetryInput {
            id: id.clone(),
            work_locally: false,
        },
    );
    assert!(
        matches!(&retried.actions[..], [Action::Prepare { action: next, stage: PreparationStage::Workspace, .. }] if next != action)
    );
}

#[test]
fn completed_item_updates_keep_their_position_without_reopening_the_turn() {
    let (state, attempt) = running();
    let item = Observation::Item {
        deferred: false,
        id: "assistant".into(),
        status: ItemStatus::Running,
        body: ItemBody::AssistantText {
            text: "partial".into(),
            phase: AssistantPhase::Final,
        },
    };
    let partial = observe(&state, &attempt, item, 3);
    let state = state.apply(&partial.changes);
    let position = state.items["assistant"].position;
    let complete = observe(&state, &attempt, Observation::Completed, 4);
    let state = state.apply(&complete.changes);
    let updated = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: "assistant".into(),
            status: ItemStatus::Completed,
            body: ItemBody::AssistantText {
                text: "complete".into(),
                phase: AssistantPhase::Final,
            },
        },
        5,
    );
    let state = state.apply(&updated.changes);
    assert_eq!(state.items["assistant"].position, position);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Completed);
}

#[test]
fn recovery_preserves_ready_bindings_and_fork_hints_with_serialization() {
    let mut state = state();
    state.bindings.insert(
        ProviderKind::Codex,
        Binding::Ready {
            covered: 0,
            source: "ready".into(),
        },
    );
    state.bindings.insert(
        ProviderKind::Claude,
        Binding::Fork {
            source: "original".into(),
            through: "prefix".into(),
        },
    );
    let recovered = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(2),
    );
    let state = state.apply(&recovered.changes);
    assert!(state.bindings.contains_key(&ProviderKind::Codex));
    assert_eq!(
        state.bindings.get(&ProviderKind::Claude),
        Some(&Binding::Fork {
            source: "original".into(),
            through: "prefix".into()
        })
    );
    let bytes = crate::protocol::encode(&state).unwrap();
    let decoded: State = crate::protocol::decode(&bytes).unwrap();
    assert_eq!(decoded, state);
}

proptest::proptest! {
    #[test]
    fn repeating_receipted_commands_cannot_change_the_conversation(seeds in proptest::collection::vec(1u128..10000, 1..30)) {
        let mut state = state();
        let mut receipts = BTreeMap::new();
        for seed in seeds {
            let operation = OperationId::Client(Uuid::from_u128(seed));
            let event = Event::Command { id: operation,
                command: Command::Submit { id: Uuid::from_u128(seed).to_string().into(), content: content(FollowUp::Queue) },
                receipt: receipts.get(&operation.receipt_key()).cloned() };
            let result = step(&state, event.clone(), context(seed));
            proptest::prop_assert_eq!(&result, &step(&state, event, context(seed)));
            if let Some((id, outcome)) = result.receipt.clone() { receipts.insert(id.receipt_key(), outcome); }
            state = state.apply(&result.changes);
            let repeated = step(&state, Event::Command { id: operation, command: Command::Rename("must not apply".into()),
                receipt: receipts.get(&operation.receipt_key()).cloned() }, context(seed + 10000));
            proptest::prop_assert!(repeated.actions.is_empty());
            proptest::prop_assert_eq!(state.apply(&repeated.changes), state.clone());
        }
    }
}

fn approval() -> RequestBody {
    use crate::requests::{ApprovalKind, Choice, ChoiceEffect};
    RequestBody::Approval {
        kind: ApprovalKind::Command,
        description: "run".into(),
        details: "command".into(),
        choices: vec![Choice {
            id: "allow".into(),
            label: "Allow".into(),
            description: "once".into(),
            effect: ChoiceEffect::Allow,
        }],
    }
}

#[test]
fn answers_are_validated_and_an_ended_request_cannot_be_answered_again() {
    let (state, attempt) = running();
    let requested = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "request".into(),
            body: approval(),
        },
        3,
    );
    let state = state.apply(&requested.changes);
    let invalid = command(
        &state,
        4,
        Command::Answer {
            id: "request".into(),
            answer: Answer::Approval {
                choice_id: "invalid".into(),
            },
        },
    );
    assert!(matches!(invalid.outcome, Outcome::Rejected(_)));
    assert!(invalid.changes.is_empty());
    let answer = command(
        &state,
        5,
        Command::Answer {
            id: "request".into(),
            answer: Answer::Approval {
                choice_id: "allow".into(),
            },
        },
    );
    let state = state.apply(&answer.changes);
    let Action::Answer { id, action, .. } = &answer.actions[0] else {
        panic!("answer")
    };
    assert!(state.permits(&answer.actions[0]));
    let sent = step(
        &state,
        Event::Answered {
            id: id.clone(),
            action: *action,
            result: Ok(()),
        },
        context(6),
    );
    assert_eq!(sent.outcome, Outcome::Accepted);
    let state = state.apply(&sent.changes);
    assert!(state.requests.is_empty());
    assert!(!state.permits(&answer.actions[0]));
    assert_eq!(
        step(
            &state,
            Event::Answered {
                id: id.clone(),
                action: *action,
                result: Ok(())
            },
            context(7)
        ),
        Step::default()
    );
}

#[test]
fn ambiguous_answers_require_confirmation_and_restart_closes_interrupted_requests() {
    let (state, attempt) = running();
    let requested = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "request".into(),
            body: approval(),
        },
        3,
    );
    let state = state.apply(&requested.changes);
    let answered = command(
        &state,
        4,
        Command::Answer {
            id: "request".into(),
            answer: Answer::Approval {
                choice_id: "allow".into(),
            },
        },
    );
    let state = state.apply(&answered.changes);
    let Action::Answer { id, action, .. } = &answered.actions[0] else {
        unreachable!()
    };
    let unknown = step(
        &state,
        Event::Answered {
            id: id.clone(),
            action: *action,
            result: Err(crate::error::Delivery::Unknown),
        },
        context(5),
    );
    let unknown_state = state.apply(&unknown.changes);
    assert_eq!(unknown_state.requests[id].state, RequestState::Unknown);
    assert!(matches!(
        command(
            &unknown_state,
            6,
            Command::Answer {
                id: id.clone(),
                answer: Answer::Approval {
                    choice_id: "allow".into()
                }
            }
        )
        .outcome,
        Outcome::Rejected(_)
    ));
    let restarted = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(7),
    );
    let state = state.apply(&restarted.changes);
    assert!(state.requests.is_empty());
    assert_eq!(
        step(
            &state,
            Event::Answered {
                id: id.clone(),
                action: *action,
                result: Ok(())
            },
            context(8)
        ),
        Step::default()
    );
}

#[test]
fn an_interrupt_before_the_start_ack_is_preserved_and_sent_once() {
    let (state, id, action, attempt) = starting(&state(), 1);
    assert!(
        state.attempts.is_empty(),
        "unstarted turns have no execution rows"
    );
    let stop = command(&state, 2, Command::Interrupt { turn: None });
    let state = state.apply(&stop.changes);
    assert_eq!(stop.outcome, Outcome::Accepted);
    assert!(stop.actions.is_empty());
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action,
        },
        context(3),
    );
    assert!(
        matches!(&delivered.actions[..], [Action::Interrupt { attempt: target, .. }] if target == &attempt)
    );
    let state = state.apply(&delivered.changes);
    assert!(!state.thread.queue_held);
    assert!(state.permits(&delivered.actions[0]));
    assert_eq!(
        step(
            &state,
            Event::Delivered {
                native_turn: None,
                id,
                action
            },
            context(4)
        ),
        Step::default()
    );
    let replay = step(
        &state,
        Event::Command {
            id: OperationId::Client(Uuid::from_u128(2)),
            command: Command::Interrupt { turn: None },
            receipt: Some(Outcome::Accepted),
        },
        context(5),
    );
    assert!(replay.actions.is_empty());
    assert!(matches!(
        &command(&state, 6, Command::Interrupt { turn: None }).actions[..],
        [Action::Interrupt { .. }]
    ));
}

#[test]
fn completion_before_the_start_ack_does_not_send_an_obsolete_interrupt() {
    let (state, _, _, attempt) = starting(&state(), 1);
    let stop = command(&state, 2, Command::Interrupt { turn: None });
    let state = state.apply(&stop.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 3);
    assert!(completed.actions.is_empty());
    assert_eq!(
        state.apply(&completed.changes).turns[&attempt.turn].state,
        TurnState::Completed
    );
}

#[test]
fn ending_a_turn_with_a_steer_in_flight_leaves_no_stuck_delivery() {
    let (state, attempt) = running();
    let steer = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steer.changes);
    let Action::Deliver { id, action, .. } = &steer.actions[0] else {
        unreachable!()
    };
    let interrupted = observe(&state, &attempt, Observation::Interrupted, 4);
    let state = state.apply(&interrupted.changes);
    assert!(matches!(&state.inputs[id].state,
        InputState::Unknown { delivery, status: UnknownState::Fenced, .. } if delivery.attempt.turn == attempt.turn));
    assert_eq!(
        step(
            &state,
            Event::Delivered {
                native_turn: None,
                id: id.clone(),
                action: *action
            },
            context(5)
        ),
        Step::default()
    );
    assert!(!state.permits(&steer.actions[0]));
}

#[test]
fn steering_waits_for_in_flight_delivery_but_keeps_its_original_attempt() {
    let (state, attempt) = running();
    let queue = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queue.changes);
    let first = submit(&state, 4, FollowUp::Steer);
    let state = state.apply(&first.changes);
    let next = submit(&state, 5, FollowUp::Steer);
    assert!(next.actions.is_empty());
    let state = state.apply(&next.changes);
    let Action::Deliver { id, action, .. } = &first.actions[0] else {
        unreachable!()
    };
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action: *action,
        },
        context(6),
    );
    assert!(
        matches!(&delivered.actions[..], [Action::Deliver { id, attempt: target, steer: true, .. }]
        if id.as_str() == Uuid::from_u128(5).to_string() && target == &attempt)
    );
}

#[test]
fn cancelling_preparation_keeps_the_saved_stage_and_fences_its_late_completion() {
    let initial = State::new(Thread {
        workspace: Workspace::Missing,
        ..state().thread
    });
    let submitted = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&submitted.changes);
    let Action::Prepare { id, action, .. } = &submitted.actions[0] else {
        unreachable!()
    };
    let cancelled = command(&state, 2, Command::Interrupt { turn: None });
    assert_eq!(
        cancelled.actions,
        vec![Action::CancelPreparation {
            action: *action,
            discard: None
        }]
    );
    let state = state.apply(&cancelled.changes);
    assert!(matches!(&state.inputs[id].state, InputState::Failed(error)
        if !error.message.trim().is_empty()));
    assert_eq!(
        step(
            &state,
            Event::Prepared {
                id: id.clone(),
                action: *action,
                cwd: Some("/late".into()),
                source: None
            },
            context(3)
        ),
        Step::default()
    );
    let retry = command(
        &state,
        4,
        Command::RetryInput {
            id: id.clone(),
            work_locally: false,
        },
    );
    assert!(
        matches!(&retry.actions[..], [Action::Prepare { action: next, stage: PreparationStage::Workspace, .. }] if next != action)
    );
}

#[test]
fn provider_preparation_persists_the_binding_before_starting_the_turn() {
    let mut state = state();
    state.bindings.clear();
    let submitted = submit(&state, 1, FollowUp::Queue);
    assert!(matches!(
        &submitted.actions[..],
        [Action::Prepare {
            stage: PreparationStage::Provider,
            ..
        }]
    ));
    let state = state.apply(&submitted.changes);
    let Action::Prepare { id, action, .. } = &submitted.actions[0] else {
        unreachable!()
    };
    let prepared = step(
        &state,
        Event::Prepared {
            id: id.clone(),
            action: *action,
            cwd: None,
            source: Some("prepared-native".into()),
        },
        context(2),
    );
    assert_eq!(prepared.outcome, Outcome::Accepted);
    assert!(
        matches!(&prepared.actions[..], [Action::Deliver { attempt, .. }] if attempt.source == "prepared-native")
    );
    let state = state.apply(&prepared.changes);
    assert_eq!(
        state.bindings[&ProviderKind::Claude],
        Binding::Ready {
            covered: 0,
            source: "prepared-native".into()
        }
    );
    assert!(state.turns.is_empty());
    assert!(state.attempts.is_empty());
}

#[test]
fn restarting_during_history_injection_rebuilds_the_source_and_rejects_its_old_reply() {
    let mut state = state();
    state.bindings.clear();
    let preparing = submit(&state, 1, FollowUp::Queue);
    let Action::Prepare { id, action, .. } = &preparing.actions[0] else {
        panic!("provider preparation")
    };
    let state = state.apply(&preparing.changes);
    let injecting = step(
        &state,
        Event::AligningProvider {
            id: id.clone(),
            action: *action,
            source: "uncertain-source".into(),
            provider: ProviderKind::Claude,
            cwd: "/project".into(),
        },
        context(2),
    );
    assert_eq!(injecting.outcome, Outcome::Accepted);
    let state = state.apply(&injecting.changes);
    assert_eq!(
        state.bindings[&ProviderKind::Claude],
        Binding::Aligning {
            source: "uncertain-source".into(),
            action: *action,
        }
    );
    let state: State = crate::protocol::decode(&crate::protocol::encode(state).unwrap()).unwrap();
    let restarted = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(3),
    );
    let state = state.apply(&restarted.changes);
    assert!(!state.bindings.contains_key(&ProviderKind::Claude));
    assert!(
        matches!(&restarted.actions[..],[Action::Prepare {action:next,stage:PreparationStage::Provider,..}] if next!=action)
    );
    assert_eq!(
        step(
            &state,
            Event::Prepared {
                id: id.clone(),
                action: *action,
                cwd: None,
                source: Some("uncertain-source".into())
            },
            context(4)
        ),
        Step::default()
    );
}

#[test]
fn an_empty_background_roster_completes_and_failure_marks_running_items_failed() {
    let (state, attempt) = running();
    let done = observe(
        &state,
        &attempt,
        Observation::Background { tasks: Vec::new() },
        3,
    );
    assert_eq!(
        state.apply(&done.changes).turns[&attempt.turn].state,
        TurnState::Completed
    );
    let item = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: "tool".into(),
            status: ItemStatus::Running,
            body: ItemBody::Plan {
                text: "work".into(),
            },
        },
        4,
    );
    let state = state.apply(&item.changes);
    let failed = observe(
        &state,
        &attempt,
        Observation::Failed(Some(ExecutionError::default())),
        5,
    );
    assert_eq!(
        state.apply(&failed.changes).items["tool"].status,
        ItemStatus::Failed
    );
}

#[test]
fn new_input_can_use_the_provider_process_while_its_previous_turn_waits_on_background_work() {
    let (state, attempt) = running();
    let background = observe(
        &state,
        &attempt,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        3,
    );
    let state = state.apply(&background.changes);
    let next = submit(&state, 4, FollowUp::Queue);
    assert!(
        matches!(&next.actions[..], [Action::Deliver { attempt: next, steer: false, .. }] if next.turn != attempt.turn)
    );
    let stop = command(&state, 5, Command::Interrupt { turn: None });
    assert!(
        matches!(&stop.actions[..], [Action::Interrupt { attempt: target, .. }] if target == &attempt)
    );
}

#[test]
fn counters_and_positions_survive_removal_and_each_step_contains_one_change_per_entity() {
    let (state, attempt) = running();
    let complete = observe(&state, &attempt, Observation::Completed, 3);
    let mut state = state.apply(&complete.changes);
    let last_position = state.counters.item;
    state.items.clear();
    let submitted = submit(&state, 4, FollowUp::Queue);
    let state = state.apply(&submitted.changes);
    let Action::Deliver { id, action, .. } = &submitted.actions[0] else {
        unreachable!()
    };
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action: *action,
        },
        context(5),
    );
    let state = state.apply(&delivered.changes);
    assert!(state.items[&ItemId(id.0.clone())].position > last_position);
    assert!(state.counters.turn > 1);
    let keys: std::collections::BTreeSet<_> = submitted.changes.iter().map(Change::key).collect();
    assert_eq!(keys.len(), submitted.changes.len());
}

#[test]
fn sending_answers_on_a_retrying_turn_recover_as_unknown_instead_of_being_resent() {
    let (mut state, attempt) = running();
    state.turns.get_mut(&attempt.turn).unwrap().state = TurnState::Waiting(Wait::Retry { at: 200 });
    state.requests.insert(
        "request".into(),
        Request {
            id: "request".into(),
            turn: attempt.turn,
            body: approval(),
            state: RequestState::Sending {
                action: Uuid::nil(),
            },
        },
    );
    let recovered = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(3),
    );
    let state = state.apply(&recovered.changes);
    assert_eq!(state.requests["request"].state, RequestState::Unknown);
    assert_eq!(recovered.actions, vec![Action::WaitUntil(200)]);
    assert!(state.attempts.is_empty());
}

#[test]
fn recovery_keeps_an_empty_queue_unheld_and_drops_old_callbacks() {
    let (state, attempt) = running();
    let completed = observe(&state, &attempt, Observation::Completed, 3);
    let state = state.apply(&completed.changes);
    let recovered = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(4),
    );
    assert_eq!(recovered.outcome, Outcome::Accepted);
    let state = state.apply(&recovered.changes);
    assert!(!state.thread.queue_held);
    assert!(state.attempts.is_empty());
    let late = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: "late".into(),
            status: ItemStatus::Completed,
            body: ItemBody::Plan {
                text: "old process".into(),
            },
        },
        5,
    );
    assert!(late.changes.is_empty());
}

#[test]
fn inputs_during_workspace_preparation_queue_without_launching_a_second_preparation() {
    let initial = State::new(Thread {
        workspace: Workspace::Missing,
        ..state().thread
    });
    let submitted = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&submitted.changes);
    let second = submit(&state, 2, FollowUp::Queue);
    assert_eq!(second.outcome, Outcome::Accepted);
    assert!(second.actions.is_empty());
    let state = state.apply(&second.changes);
    assert_eq!(state.inputs.len(), 2);
    let cancelled = command(&state, 3, Command::Interrupt { turn: None });
    let state = state.apply(&cancelled.changes);
    let removed = command(
        &state,
        4,
        Command::RemoveInput(Uuid::from_u128(1).to_string().into()),
    );
    let state = state.apply(&removed.changes);
    let resumed = command(&state, 5, Command::ResumeQueue);
    assert!(
        matches!(&resumed.actions[..], [Action::Prepare { id, stage: PreparationStage::Workspace, .. }]
        if id.as_str() == Uuid::from_u128(2).to_string())
    );
}

#[test]
fn a_new_message_starts_past_a_held_queue_without_releasing_old_inputs() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let stopped = observe(&state, &attempt, Observation::Interrupted, 4);
    let state = state.apply(&stopped.changes);
    assert!(state.thread.queue_held);
    let direct = submit(&state, 5, FollowUp::Queue);
    assert!(matches!(&direct.actions[..], [Action::Deliver { id, .. }]
        if id.as_str() == Uuid::from_u128(5).to_string()));
    let state = state.apply(&direct.changes);
    assert!(state.thread.queue_held);
    assert!(matches!(
        state.inputs[&ClientInputId::from(Uuid::from_u128(3).to_string())].state,
        InputState::Queued { .. }
    ));
}

#[test]
fn retry_fences_an_in_flight_steer_and_cannot_be_ended_by_old_callbacks() {
    let (state, attempt) = codex_running();
    let steer = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steer.changes);
    let Action::Deliver { id, action, .. } = &steer.actions[0] else {
        unreachable!()
    };
    let in_flight = state.inputs[id].clone();
    let retry = observe(&state, &attempt, Observation::Failed(Some(overload())), 4);
    let state = state.apply(&retry.changes);
    assert!(matches!(&state.inputs[id].state,
        InputState::Unknown { delivery, status: UnknownState::Fenced, .. } if delivery.attempt.turn == attempt.turn));
    assert_eq!(
        step(
            &state,
            Event::Delivered {
                native_turn: None,
                id: id.clone(),
                action: *action
            },
            context(5)
        ),
        Step::default()
    );
    let resumed = step(
        &state,
        Event::Wake,
        Context {
            external_busy: false,
            now: 2100,
            ..context(6)
        },
    );
    let state = state.apply(&resumed.changes);
    let Action::Resume {
        attempt: current, ..
    } = &resumed.actions[0]
    else {
        panic!("resume")
    };
    assert_eq!(
        observe(&state, &attempt, Observation::Completed, 7),
        Step::default()
    );
    // A leftover pre-restart delivery row must not bypass the attempt fence.
    let mut stale_view = state.clone();
    stale_view.inputs.insert(id.clone(), in_flight);
    assert_eq!(
        step(
            &stale_view,
            Event::Delivered {
                native_turn: None,
                id: id.clone(),
                action: *action
            },
            context(8)
        ),
        Step::default()
    );
    assert_eq!(
        observe(&stale_view, &attempt, Observation::Completed, 8),
        Step::default()
    );
    let completed = observe(&state, current, Observation::Completed, 9);
    assert_eq!(
        state.apply(&completed.changes).turns[&current.turn].state,
        TurnState::Completed
    );
    assert!(
        !state
            .inputs
            .values()
            .any(|input| matches!(input.state, InputState::Delivering(_)))
    );
}

#[test]
fn a_definitely_unsent_answer_can_be_answered_again_with_a_new_action() {
    let (state, attempt) = running();
    let requested = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "request".into(),
            body: approval(),
        },
        3,
    );
    let state = state.apply(&requested.changes);
    let answer = command(
        &state,
        4,
        Command::Answer {
            id: "request".into(),
            answer: Answer::Approval {
                choice_id: "allow".into(),
            },
        },
    );
    let state = state.apply(&answer.changes);
    let Action::Answer { id, action, .. } = &answer.actions[0] else {
        unreachable!()
    };
    let unsent = step(
        &state,
        Event::Answered {
            id: id.clone(),
            action: *action,
            result: Err(crate::error::Delivery::NotSent),
        },
        context(5),
    );
    let state = state.apply(&unsent.changes);
    assert_eq!(state.requests[id].state, RequestState::Awaiting);
    let retry = command(
        &state,
        6,
        Command::Answer {
            id: id.clone(),
            answer: Answer::Approval {
                choice_id: "allow".into(),
            },
        },
    );
    assert!(matches!(&retry.actions[..], [Action::Answer { action: next, .. }] if next != action));
}

#[test]
fn uppercase_client_uuids_are_preserved_and_case_aliases_cannot_duplicate_input() {
    let uuid = Uuid::from_u128(0xabcdef0123456789abcdef0123456789);
    let initial = state();
    let submitted = step(
        &initial,
        Event::Command {
            id: OperationId::Client(uuid),
            receipt: None,
            command: Command::Submit {
                id: uuid.to_string().to_uppercase().into(),
                content: content(FollowUp::Queue),
            },
        },
        context(1),
    );
    assert_eq!(submitted.outcome, Outcome::Accepted);
    let state = initial.apply(&submitted.changes);
    assert!(
        state
            .inputs
            .contains_key(uuid.to_string().to_uppercase().as_str())
    );
    let alias = step(
        &state,
        Event::Command {
            id: OperationId::Client(uuid),
            receipt: None,
            command: Command::Submit {
                id: uuid.to_string().into(),
                content: content(FollowUp::Queue),
            },
        },
        context(2),
    );
    assert!(matches!(alias.outcome, Outcome::Rejected(_)));
    assert!(alias.changes.is_empty());
}

#[test]
fn provider_item_ids_are_scoped_to_attempts_and_existing_mappings_keep_legacy_ids() {
    let native = "native-item";
    let (state, attempt) = running();
    let first = provider_item_id(attempt.id, native);
    assert_eq!(first, provider_item_id(attempt.id, native));
    assert_ne!(first, provider_item_id(Uuid::nil(), native));
    assert_eq!(
        provider_request_id(attempt.id, native),
        provider_request_id(attempt.id, native)
    );
    assert_ne!(
        provider_request_id(attempt.id, native),
        provider_request_id(Uuid::nil(), native)
    );
    let observed = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: "legacy-item".into(),
            status: ItemStatus::Running,
            body: ItemBody::Plan {
                text: "original".into(),
            },
        },
        3,
    );
    let state = state.apply(&observed.changes);
    let updated = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: "legacy-item".into(),
            status: ItemStatus::Completed,
            body: ItemBody::Plan {
                text: "updated".into(),
            },
        },
        4,
    );
    let state = state.apply(&updated.changes);
    assert!(state.items.contains_key("legacy-item"));
    assert!(!state.items.contains_key(&first));
}

#[test]
fn retrying_a_failed_input_uses_the_workspace_and_binding_established_by_later_input() {
    let initial = State::new(Thread {
        workspace: Workspace::Missing,
        ..state().thread
    });
    let first = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&first.changes);
    let Action::Prepare { id, action, .. } = &first.actions[0] else {
        unreachable!()
    };
    let failed = step(
        &state,
        Event::PreparationFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
        },
        context(2),
    );
    let state = state.apply(&failed.changes);
    let second = submit(&state, 3, FollowUp::Queue);
    let mut state = state.apply(&second.changes);
    let mut action = second.actions[0].clone();
    for (seed, cwd, source) in [
        (4, Some("/worktree"), None),
        (5, None, Some("native-second")),
    ] {
        let Action::Prepare {
            id, action: token, ..
        } = action
        else {
            panic!("prepare")
        };
        let prepared = step(
            &state,
            Event::Prepared {
                id,
                action: token,
                cwd: cwd.map(str::to_owned),
                source: source.map(str::to_owned),
            },
            context(seed),
        );
        state = state.apply(&prepared.changes);
        action = prepared.actions[0].clone();
    }
    let Action::Deliver {
        id: second_id,
        action: token,
        attempt,
        ..
    } = action
    else {
        panic!("deliver")
    };
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: second_id,
            action: token,
        },
        context(7),
    );
    state = state.apply(&delivered.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 8);
    state = state.apply(&completed.changes);
    let retried = command(
        &state,
        9,
        Command::RetryInput {
            id: id.clone(),
            work_locally: false,
        },
    );
    assert!(
        matches!(&retried.actions[..], [Action::Deliver { cwd, attempt, .. }]
        if cwd == "/worktree" && attempt.source == "native-second")
    );
    assert_eq!(
        state.apply(&retried.changes).thread.workspace.cwd(),
        Some("/worktree")
    );
}

#[test]
fn retry_closes_the_old_answer_channel_and_holds_existing_queued_input() {
    let (state, attempt) = codex_running();
    let requested = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "request".into(),
            body: approval(),
        },
        3,
    );
    let state = state.apply(&requested.changes);
    let answered = command(
        &state,
        4,
        Command::Answer {
            id: "request".into(),
            answer: Answer::Approval {
                choice_id: "allow".into(),
            },
        },
    );
    let state = state.apply(&answered.changes);
    let queued = submit(&state, 5, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let retry = observe(&state, &attempt, Observation::Failed(Some(overload())), 6);
    assert_eq!(retry.actions, vec![Action::WaitUntil(2100)]);
    let state = state.apply(&retry.changes);
    assert_eq!(state.requests["request"].state, RequestState::Unknown);
    assert!(state.thread.queue_held);
    assert!(!state.permits(&answered.actions[0]));
}

#[test]
fn invalid_preparation_results_leave_a_readable_error_and_cannot_start_a_turn() {
    for (workspace, cwd, source) in [
        (Workspace::Missing, None, None),
        (Workspace::Ready("/project".into()), None, None),
        (
            Workspace::Ready("/project".into()),
            Some("/different"),
            Some("native"),
        ),
    ] {
        let initial = State::new(Thread {
            workspace: workspace.clone(),
            ..state().thread
        });
        let submitted = submit(&initial, 1, FollowUp::Queue);
        let state = initial.apply(&submitted.changes);
        let Action::Prepare { id, action, .. } = &submitted.actions[0] else {
            panic!("prepare")
        };
        let rejected = step(
            &state,
            Event::Prepared {
                id: id.clone(),
                action: *action,
                cwd: cwd.map(str::to_owned),
                source: source.map(str::to_owned),
            },
            context(2),
        );
        assert_eq!(rejected.outcome, Outcome::Accepted);
        assert!(rejected.actions.is_empty());
        let state = state.apply(&rejected.changes);
        let InputState::Failed(error) = &state.inputs[id].state else {
            panic!("failure")
        };
        assert!(!error.message.trim().is_empty());
        assert_eq!(state.thread.workspace, workspace);
        assert!(state.turns.is_empty());
    }
}

#[test]
fn an_idle_persisted_queue_is_held_on_restart() {
    let mut state = state();
    let id: ClientInputId = Uuid::from_u128(1).to_string().into();
    state.inputs.insert(
        id.clone(),
        PendingInput {
            id,
            position: 1,
            content: content(FollowUp::Queue),
            state: InputState::Queued { steer: None },
        },
    );
    state.counters.input = 1;
    let restarted = step(
        &state,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(2),
    );
    assert_eq!(restarted.outcome, Outcome::Accepted);
    assert!(state.apply(&restarted.changes).thread.queue_held);
    assert!(restarted.actions.is_empty());
}

#[test]
fn preparation_completion_drains_the_head_after_retry_changes_queue_order() {
    let mut initial = state();
    initial.bindings.clear();
    let first = submit(&initial, 1, FollowUp::Queue);
    let state = initial.apply(&first.changes);
    let Action::Prepare {
        id: first_id,
        action,
        ..
    } = &first.actions[0]
    else {
        unreachable!()
    };
    let failed = step(
        &state,
        Event::PreparationFailed {
            id: first_id.clone(),
            action: *action,
            error: ExecutionError::default(),
        },
        context(2),
    );
    let state = state.apply(&failed.changes);
    let second = submit(&state, 3, FollowUp::Queue);
    let mut state = state.apply(&second.changes);
    let retried = command(
        &state,
        4,
        Command::RetryInput {
            id: first_id.clone(),
            work_locally: false,
        },
    );
    assert!(retried.actions.is_empty());
    state = state.apply(&retried.changes);
    let Action::Prepare { id, action, .. } = &second.actions[0] else {
        unreachable!()
    };
    let prepared = step(
        &state,
        Event::Prepared {
            id: id.clone(),
            action: *action,
            cwd: None,
            source: Some("ready".into()),
        },
        context(7),
    );
    assert!(matches!(&prepared.actions[..], [Action::Deliver { id, .. }] if id == first_id));
}

#[test]
fn a_pending_interrupt_survives_retry_and_background_events_before_and_after_the_ack() {
    for before_ack in [false, true] {
        for retry in [false, true] {
            let (state, attempt) = if before_ack {
                let (state, _, _, attempt) = starting(&state(), 1);
                (state, attempt)
            } else {
                running()
            };
            let stop = command(&state, 3, Command::Interrupt { turn: None });
            let state = state.apply(&stop.changes);
            let observation = if retry {
                Observation::Failed(Some(overload()))
            } else {
                Observation::Background {
                    tasks: vec![BackgroundTask {
                        id: "agent".into(),
                        kind: crate::execution::BackgroundTaskKind::Subagent,
                        description: None,
                    }],
                }
            };
            let observed = observe(&state, &attempt, observation, 4);
            let state = state.apply(&observed.changes);
            if retry {
                assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
                assert!(observed.actions.is_empty());
                assert!(
                    step(
                        &state,
                        Event::Wake,
                        Context {
                            external_busy: false,
                            now: 200,
                            ..context(5)
                        }
                    )
                    .actions
                    .is_empty()
                );
            } else {
                assert!(matches!(
                    state.turns[&attempt.turn].state,
                    TurnState::Running { interrupt: Some(_) }
                ));
                let actions = if before_ack {
                    &observed.actions
                } else {
                    &stop.actions
                };
                assert!(matches!(&actions[..], [Action::Interrupt { .. }]));
                assert!(state.permits(&actions[0]));
            }
        }
    }
}

#[test]
fn update_continues_a_turn_with_an_unknown_steer_but_not_a_later_unknown_start() {
    let (state, attempt) = running();
    let steer = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steer.changes);
    let restarted = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(4),
    );
    let state = state.apply(&restarted.changes);
    let job = &restarted.continuations[0];
    let resume = |state: &State, seed| {
        step(
            state,
            Event::Command {
                id: OperationId::Job(job.id),
                receipt: None,
                command: Command::ContinueAfterRestart {
                    source: attempt.turn.clone(),
                    enabled: true,
                    content: content(FollowUp::Queue),
                },
            },
            context(seed),
        )
    };
    let resumed = resume(&state, 5);
    let delivered = deliver_after_rebuilding(&state, &resumed, 50);
    assert!(
        matches!(&delivered.actions[..], [Action::Deliver { id, .. }]
        if id.as_str() == job.id.to_string())
    );
    let later = submit(&state, 6, FollowUp::Queue);
    let state = state.apply(&later.changes);
    assert!(resume(&state, 7).changes.is_empty());
    let crashed = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(7),
    );
    let state = state.apply(&crashed.changes);
    assert!(resume(&state, 8).actions.is_empty());
}

#[test]
fn the_first_native_user_message_proves_delivery_without_duplicating_the_input() {
    let (state, id, _, attempt) = starting(&state(), 1);
    let proven = observe(
        &state,
        &attempt,
        Observation::Item {
            deferred: false,
            id: provider_item_id(attempt.id, "native-user"),
            status: ItemStatus::Completed,
            body: ItemBody::UserMessage {
                text: Some("hello".into()),
                content: Vec::new(),
            },
        },
        2,
    );
    assert_eq!(proven.outcome, Outcome::Accepted);
    let state = state.apply(&proven.changes);
    assert!(state.inputs.is_empty());
    assert_eq!(state.items.len(), 1);
    assert!(state.items.contains_key(&ItemId(id.0)));
}

#[test]
fn an_empty_background_roster_finishes_a_turn_even_when_an_interrupt_is_pending() {
    let (state, attempt) = running();
    let stopped = command(&state, 3, Command::Interrupt { turn: None });
    let state = state.apply(&stopped.changes);
    let finished = observe(
        &state,
        &attempt,
        Observation::Background { tasks: Vec::new() },
        4,
    );
    let state = state.apply(&finished.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Completed);
    assert!(!state.permits(&stopped.actions[0]));
}

#[test]
fn stopping_an_unacknowledged_foreground_start_does_not_target_an_older_background_turn() {
    let (state, old) = running();
    let background = observe(
        &state,
        &old,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        3,
    );
    let state = state.apply(&background.changes);
    let next = submit(&state, 4, FollowUp::Queue);
    let state = state.apply(&next.changes);
    let Action::Deliver {
        id,
        action,
        attempt,
        ..
    } = &next.actions[0]
    else {
        panic!("start")
    };
    let unknown = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action: *action,
            unknown: true,
            error: Default::default(),
        },
        context(55),
    );
    let unknown = state.apply(&unknown.changes);
    let background_stop = command(
        &unknown,
        56,
        Command::Interrupt {
            turn: Some(old.turn.clone()),
        },
    );
    assert_eq!(background_stop.outcome, Outcome::Accepted);
    assert!(
        matches!(&background_stop.actions[..],[Action::Interrupt {attempt,background:true,..}] if attempt==&old)
    );
    let stopped = command(
        &state,
        5,
        Command::Interrupt {
            turn: Some(id.to_string().into()),
        },
    );
    assert!(stopped.actions.is_empty());
    let state = state.apply(&stopped.changes);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action: *action,
        },
        context(6),
    );
    assert!(
        matches!(&delivered.actions[..], [Action::Interrupt { attempt: target, .. }] if target == attempt)
    );
    assert!(matches!(
        state.apply(&delivered.changes).turns[&old.turn].state,
        TurnState::Waiting(Wait::Background { .. })
    ));
}

#[test]
fn a_live_unknown_start_accepts_late_delivery_proof_and_can_be_stopped() {
    for observation in [
        Observation::Item {
            deferred: false,
            id: "reply".into(),
            status: ItemStatus::Running,
            body: ItemBody::AssistantText {
                text: "working".into(),
                phase: AssistantPhase::Final,
            },
        },
        Observation::Request {
            id: "approval".into(),
            body: approval(),
        },
    ] {
        let (state, id, action, attempt) = starting(&state(), 1);
        let failed = step(
            &state,
            Event::DeliveryFailed {
                id: id.clone(),
                action,
                error: ExecutionError::default(),
                unknown: true,
            },
            context(2),
        );
        let state = state.apply(&failed.changes);
        let proved = observe(&state, &attempt, observation.clone(), 3);
        let state = state.apply(&proved.changes);
        assert!(state.inputs.is_empty());
        assert_eq!(state.attempts[&attempt.turn], attempt);
        assert_eq!(state.items[&ItemId(id.0.clone())].turn, attempt.turn);
        match observation {
            Observation::Item { id, body, .. } => assert_eq!(state.items[&id].body, body),
            Observation::Request { id, body } => assert_eq!(state.requests[&id].body, body),
            _ => unreachable!(),
        }
        let stopped = command(&state, 4, Command::Interrupt { turn: None });
        assert!(
            matches!(&stopped.actions[..], [Action::Interrupt { attempt: target, .. }]
            if target == &attempt)
        );
        assert_eq!(
            step(
                &state,
                Event::Delivered {
                    native_turn: None,
                    id,
                    action
                },
                context(5)
            ),
            Step::default()
        );
    }
}

#[test]
fn stopping_a_live_unknown_start_is_remembered_until_delivery_is_proved() {
    let (state, id, action, attempt) = starting(&state(), 1);
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id,
            action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(2),
    );
    let state = state.apply(&failed.changes);
    let stopped = command(&state, 3, Command::Interrupt { turn: None });
    assert_eq!(stopped.outcome, Outcome::Accepted);
    assert!(stopped.actions.is_empty());
    let state = state.apply(&stopped.changes);
    let proved = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "approval".into(),
            body: approval(),
        },
        4,
    );
    assert!(
        matches!(&proved.actions[..], [Action::Interrupt { attempt: target, .. }]
        if target == &attempt)
    );
    let state = state.apply(&proved.changes);
    assert!(state.permits(&proved.actions[0]));
    assert!(matches!(
        state.turns[&attempt.turn].state,
        TurnState::Running { interrupt: Some(_) }
    ));
}

#[test]
fn an_unknown_delivery_must_be_fenced_before_the_user_can_remove_or_retry_it() {
    let (state, id, action, attempt) = starting(&state(), 1);
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(2),
    );
    let state = state.apply(&failed.changes);
    let denied = command(&state, 3, Command::RemoveInput(id.clone()));
    assert!(matches!(denied.outcome, Outcome::Rejected(_)));
    assert!(denied.changes.is_empty());
    assert_eq!(
        step(
            &state,
            Event::DeliveryFenced {
                id: id.clone(),
                action: Uuid::nil(),
            },
            context(4)
        ),
        Step::default()
    );
    for boundary in [
        Event::DeliveryFenced {
            id: id.clone(),
            action,
        },
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
    ] {
        let fenced = step(&state, boundary, context(5));
        let state = state.apply(&fenced.changes);
        assert!(matches!(
            state.inputs[&id].state,
            InputState::Unknown {
                status: UnknownState::Fenced,
                ..
            }
        ));
        assert_eq!(
            observe(&state, &attempt, Observation::Completed, 6),
            Step::default()
        );
        assert_eq!(
            step(
                &state,
                Event::Delivered {
                    native_turn: None,
                    id: id.clone(),
                    action
                },
                context(7)
            ),
            Step::default()
        );
        for request in [
            Command::RemoveInput(id.clone()),
            Command::ResolveUnknown {
                id: id.clone(),
                retry: false,
            },
            Command::ResolveUnknown {
                id: id.clone(),
                retry: true,
            },
        ] {
            assert_eq!(command(&state, 8, request).outcome, Outcome::Accepted);
        }
    }
}

#[test]
fn moving_into_background_work_immediately_sends_the_oldest_queued_input() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let background = observe(
        &state,
        &attempt,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        4,
    );
    assert!(
        matches!(&background.actions[..], [Action::Deliver { id, steer: false, .. }]
        if id.as_str() == Uuid::from_u128(3).to_string())
    );
    let state = state.apply(&background.changes);
    assert!(matches!(
        state.turns[&attempt.turn].state,
        TurnState::Waiting(Wait::Background { .. })
    ));
}

#[test]
fn a_queued_steer_cannot_block_older_input_after_its_target_enters_background_work() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let steering = submit(&state, 4, FollowUp::Steer);
    let state = state.apply(&steering.changes);
    let queued_steer = submit(&state, 5, FollowUp::Steer);
    let state = state.apply(&queued_steer.changes);
    let background = observe(
        &state,
        &attempt,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        6,
    );
    assert!(background.actions.is_empty());
    let state = state.apply(&background.changes);
    let Action::Deliver { id, action, .. } = &steering.actions[0] else {
        panic!("steer")
    };
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action: *action,
        },
        context(7),
    );
    assert!(
        matches!(&delivered.actions[..], [Action::Deliver { id, steer: false, .. }]
        if id.as_str() == Uuid::from_u128(3).to_string())
    );
}

#[test]
fn an_unknown_steer_does_not_hide_or_block_a_later_foreground_start() {
    let (state, attempt) = running();
    let steering = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steering.changes);
    let Action::Deliver { id, action, .. } = &steering.actions[0] else {
        panic!("steer")
    };
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(4),
    );
    let state = state.apply(&failed.changes);
    let background = observe(
        &state,
        &attempt,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        5,
    );
    let state = state.apply(&background.changes);
    let (state, id, action, foreground) = starting(&state, 6);
    let stopped = command(&state, 7, Command::Interrupt { turn: None });
    let state = state.apply(&stopped.changes);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(8),
    );
    assert!(
        matches!(&delivered.actions[..], [Action::Interrupt { attempt: target, .. }]
        if target == &foreground)
    );
    assert!(matches!(
        state.turns[&attempt.turn].state,
        TurnState::Waiting(Wait::Background { .. })
    ));
}

#[test]
fn fencing_an_unknown_steer_ends_only_its_execution_and_keeps_a_newer_source_user() {
    for foreground in [0, 1, 2] {
        let (state, old) = running();
        let request = observe(
            &state,
            &old,
            Observation::Request {
                id: "old-request".into(),
                body: approval(),
            },
            3,
        );
        let state = state.apply(&request.changes);
        let steering = submit(&state, 4, FollowUp::Steer);
        let Action::Deliver { id, action, .. } = &steering.actions[0] else {
            panic!("steer")
        };
        let state = state.apply(&steering.changes);
        let unknown = step(
            &state,
            Event::DeliveryFailed {
                id: id.clone(),
                action: *action,
                error: ExecutionError::default(),
                unknown: true,
            },
            context(5),
        );
        let mut state = state.apply(&unknown.changes);
        let mut newer = None;
        if foreground > 0 {
            let waiting = observe(
                &state,
                &old,
                Observation::Background {
                    tasks: vec![BackgroundTask {
                        id: "old-command".into(),
                        kind: crate::execution::BackgroundTaskKind::Command,
                        description: None,
                    }],
                },
                6,
            );
            state = state.apply(&waiting.changes);
            let (pending, input, token, attempt) = starting(&state, 7);
            state = pending;
            if foreground == 2 {
                let delivered = step(
                    &state,
                    Event::Delivered {
                        id: input,
                        action: token,
                        native_turn: None,
                    },
                    context(8),
                );
                state = state.apply(&delivered.changes);
            }
            newer = Some(attempt);
        }
        assert_eq!(
            state.source_in_use(old.provider, &old.source, old.id),
            foreground > 0
        );
        let retained_binding = state.bindings.get(&old.provider).cloned();
        let resolved = command(
            &state,
            9,
            Command::ResolveUnknown {
                id: id.clone(),
                retry: false,
            },
        );
        let state = state.apply(&resolved.changes);
        if foreground == 0 {
            assert!(!state.bindings.contains_key(&old.provider));
            assert!(
                submit(&state, 10, FollowUp::Steer).actions.is_empty(),
                "fencing cannot admit another steer to the same execution"
            );
        }
        let fenced = step(
            &state,
            Event::DeliveryFenced {
                id: id.clone(),
                action: *action,
            },
            context(11),
        );
        let state = state.apply(&fenced.changes);
        assert!(!state.inputs.contains_key(id));
        assert_eq!(state.turns[&old.turn].state, TurnState::Interrupted);
        assert!(!state.requests.contains_key(&RequestId::from("old-request")));
        assert!(!state.attempts.contains_key(&old.turn));
        if let Some(newer) = newer {
            assert_eq!(state.bindings.get(&old.provider), retained_binding.as_ref());
            if foreground == 2 {
                assert_eq!(
                    state.turns[&newer.turn].state,
                    TurnState::Running { interrupt: None }
                );
            } else {
                assert!(state.inputs.values().any(|input| {
                    input
                        .live_delivery()
                        .is_some_and(|delivery| delivery.attempt == newer)
                }));
            }
        }
    }
}

#[test]
fn user_resolution_of_a_live_unknown_is_saved_then_applied_after_fencing_or_restart() {
    for retry in [false, true] {
        let (state, id, action, attempt) = starting(&state(), 1);
        let failed = step(
            &state,
            Event::DeliveryFailed {
                id: id.clone(),
                action,
                error: ExecutionError::default(),
                unknown: true,
            },
            context(2),
        );
        let state = state.apply(&failed.changes);
        let resolved = command(
            &state,
            3,
            Command::ResolveUnknown {
                id: id.clone(),
                retry,
            },
        );
        assert!(matches!(&resolved.actions[..], [Action::FenceDelivery {
            id: target, action: token, attempt: generation,
        }] if target == &id && *token == action && generation == &attempt));
        let state = state.apply(&resolved.changes);
        assert!(state.permits(&resolved.actions[0]));
        let retried = command(
            &state,
            44,
            Command::ResolveUnknown {
                id: id.clone(),
                retry,
            },
        );
        assert_eq!(retried.outcome, Outcome::Accepted);
        assert_eq!(
            retried.actions, resolved.actions,
            "a failed fence may be requested again with the saved disposition"
        );
        assert!(retried.changes.is_empty());
        let duplicate = command(
            &state,
            4,
            Command::ResolveUnknown {
                id: id.clone(),
                retry: !retry,
            },
        );
        assert!(matches!(duplicate.outcome, Outcome::Rejected(_)));
        assert!(duplicate.actions.is_empty());
        for boundary in [
            Event::DeliveryFenced {
                id: id.clone(),
                action,
            },
            Event::Restart {
                updating: false,
                continue_after_restart: false,
            },
        ] {
            let fenced = step(&state, boundary, context(5));
            assert_eq!(fenced.outcome, Outcome::Accepted);
            assert!(fenced.actions.is_empty());
            let state = state.apply(&fenced.changes);
            assert!(!state.permits(&resolved.actions[0]));
            assert!(state.turns.is_empty());
            assert_eq!(
                observe(&state, &attempt, Observation::Completed, 6),
                Step::default()
            );
            if retry {
                assert!(matches!(state.inputs[&id].state, InputState::Queued { .. }));
                assert!(state.thread.queue_held);
                assert!(step(&state, Event::Wake, context(7)).actions.is_empty());
                let resumed = command(&state, 8, Command::ResumeQueue);
                let delivery = match &resumed.actions[..] {
                    [
                        Action::Prepare {
                            id,
                            action,
                            stage: PreparationStage::Provider,
                            ..
                        },
                    ] => step(
                        &state.apply(&resumed.changes),
                        Event::Prepared {
                            id: id.clone(),
                            action: *action,
                            cwd: None,
                            source: Some("replacement-native".into()),
                        },
                        context(9),
                    ),
                    _ => resumed,
                };
                assert!(
                    matches!(&delivery.actions[..], [Action::Deliver { attempt: next, .. }] if next.id != attempt.id)
                );
            } else {
                assert!(state.inputs.is_empty());
            }
        }
    }
}

#[test]
fn delivery_proof_winning_a_user_resolution_never_retries_the_accepted_input() {
    let (state, id, action, attempt) = starting(&state(), 1);
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(2),
    );
    let state = state.apply(&failed.changes);
    let resolved = command(
        &state,
        3,
        Command::ResolveUnknown {
            id: id.clone(),
            retry: true,
        },
    );
    let state = state.apply(&resolved.changes);
    let proved = observe(
        &state,
        &attempt,
        Observation::Request {
            id: "approval".into(),
            body: approval(),
        },
        4,
    );
    let state = state.apply(&proved.changes);
    assert!(!state.permits(&resolved.actions[0]));
    assert!(state.inputs.is_empty());
    assert_eq!(state.turns.len(), 1);
    assert_eq!(
        step(&state, Event::DeliveryFenced { id, action }, context(5)),
        Step::default()
    );
    assert!(command(&state, 6, Command::ResumeQueue).actions.is_empty());
}

#[test]
fn completing_a_turn_fences_its_unknown_steer_and_allows_queue_resume_after_disposition() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let steering = submit(&state, 4, FollowUp::Steer);
    let state = state.apply(&steering.changes);
    let Action::Deliver { id, action, .. } = &steering.actions[0] else {
        panic!("steer")
    };
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(5),
    );
    let state = state.apply(&failed.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 6);
    let state = state.apply(&completed.changes);
    assert!(matches!(
        state.inputs[id].state,
        InputState::Unknown {
            status: UnknownState::Fenced,
            ..
        }
    ));
    let resolved = command(
        &state,
        7,
        Command::ResolveUnknown {
            id: id.clone(),
            retry: false,
        },
    );
    let state = state.apply(&resolved.changes);
    let resumed = command(&state, 8, Command::ResumeQueue);
    assert!(
        matches!(&resumed.actions[..], [Action::Deliver { id, steer: false, .. }]
        if id.as_str() == Uuid::from_u128(3).to_string())
    );
}

#[test]
fn a_steer_still_being_delivered_can_be_acknowledged_after_its_turn_completes() {
    let (state, attempt) = running();
    let steering = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steering.changes);
    let Action::Deliver { id, action, .. } = &steering.actions[0] else {
        panic!("steer")
    };
    let completed = observe(&state, &attempt, Observation::Completed, 4);
    let state = state.apply(&completed.changes);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: id.clone(),
            action: *action,
        },
        context(5),
    );
    assert_eq!(delivered.outcome, Outcome::Accepted);
    let state = state.apply(&delivered.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Completed);
    assert_eq!(state.items[&ItemId(id.0.clone())].turn, attempt.turn);
    assert!(state.inputs.is_empty());
}

#[test]
fn ending_a_newer_turn_rearms_an_older_retry_whose_timer_fired_while_it_was_blocked() {
    for terminal in [
        Observation::Completed,
        Observation::Interrupted,
        Observation::Failed(Some(ExecutionError::default())),
    ] {
        let (state, old) = codex_running();
        let background = observe(
            &state,
            &old,
            Observation::Background {
                tasks: vec![BackgroundTask {
                    id: "agent".into(),
                    kind: crate::execution::BackgroundTaskKind::Subagent,
                    description: None,
                }],
            },
            3,
        );
        let state = state.apply(&background.changes);
        let (state, id, action, foreground) = starting(&state, 4);
        let delivered = step(
            &state,
            Event::Delivered {
                native_turn: None,
                id,
                action,
            },
            context(5),
        );
        let state = state.apply(&delivered.changes);
        let waiting = observe(&state, &old, Observation::Failed(Some(overload())), 6);
        let state = state.apply(&waiting.changes);
        let wake = step(
            &state,
            Event::Wake,
            Context {
                external_busy: false,
                now: 2100,
                ..context(7)
            },
        );
        assert!(wake.actions.is_empty());
        let ended = observe(&state, &foreground, terminal, 8);
        assert_eq!(ended.actions, vec![Action::WaitUntil(2100)]);
        let state = state.apply(&ended.changes);
        let resumed = step(
            &state,
            Event::Wake,
            Context {
                external_busy: false,
                now: 2101,
                ..context(9)
            },
        );
        assert!(
            matches!(&resumed.actions[..], [Action::Resume { attempt, .. }]
            if attempt.turn == old.turn && attempt.id != old.id)
        );
    }
}

#[test]
fn stopping_a_retry_wait_prevents_the_due_timer_from_starting_a_new_attempt() {
    let (state, attempt) = codex_running();
    let waiting = observe(&state, &attempt, Observation::Failed(Some(overload())), 3);
    let state = state.apply(&waiting.changes);
    let stopped = command(&state, 4, Command::Interrupt { turn: None });
    let state = state.apply(&stopped.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
    assert!(
        step(
            &state,
            Event::Wake,
            Context {
                external_busy: false,
                now: 2101,
                ..context(5)
            }
        )
        .actions
        .is_empty()
    );
}

#[test]
fn removing_all_held_input_does_not_hold_the_next_new_message() {
    let (state, attempt) = running();
    let queued = submit(&state, 3, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let held = command(&state, 4, Command::Interrupt { turn: None });
    let state = state.apply(&held.changes);
    let removed = command(
        &state,
        5,
        Command::RemoveInput(Uuid::from_u128(3).to_string().into()),
    );
    let state = state.apply(&removed.changes);
    let fresh = submit(&state, 6, FollowUp::Queue);
    let state = state.apply(&fresh.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 7);
    assert!(
        matches!(&completed.actions[..], [Action::Deliver { id, .. }]
        if id.as_str() == Uuid::from_u128(6).to_string())
    );
}

#[test]
fn waking_an_idle_owner_acknowledges_delivery_of_its_persisted_queue() {
    for (workspace, needs_preparation) in [
        (Workspace::Missing, true),
        (Workspace::Ready("/project".into()), false),
    ] {
        let mut state = state();
        state.thread.workspace = workspace;
        let id: ClientInputId = Uuid::from_u128(1).to_string().into();
        state.inputs.insert(
            id.clone(),
            PendingInput {
                id,
                position: 1,
                content: content(FollowUp::Queue),
                state: InputState::Queued { steer: None },
            },
        );
        state.counters.input = 1;
        let woke = step(&state, Event::Wake, context(2));
        assert_eq!(woke.outcome, Outcome::Accepted);
        if needs_preparation {
            assert!(matches!(
                &woke.actions[..],
                [Action::Prepare {
                    stage: PreparationStage::Workspace,
                    ..
                }]
            ));
        } else {
            assert!(matches!(
                &woke.actions[..],
                [Action::Deliver { steer: false, .. }]
            ));
        }
    }
}

#[test]
fn a_resume_action_expires_when_stopped_completed_or_superseded_by_another_attempt() {
    let (state, attempt) = codex_running();
    let waiting = observe(&state, &attempt, Observation::Failed(Some(overload())), 3);
    let state = state.apply(&waiting.changes);
    let resumed = step(
        &state,
        Event::Wake,
        Context {
            now: 2100,
            ..context(4)
        },
    );
    let state = state.apply(&resumed.changes);
    let action = &resumed.actions[0];
    let Action::Resume {
        attempt: current, ..
    } = action
    else {
        panic!("resume")
    };
    assert!(state.permits(action));
    let stopped = command(&state, 5, Command::Interrupt { turn: None });
    assert!(!state.apply(&stopped.changes).permits(action));
    let completed = observe(&state, current, Observation::Completed, 6);
    assert!(!state.apply(&completed.changes).permits(action));
    let retry = observe(&state, current, Observation::Failed(Some(overload())), 7);
    let state = state.apply(&retry.changes);
    let next = step(
        &state,
        Event::Wake,
        Context {
            now: 4100,
            ..context(8)
        },
    );
    let state = state.apply(&next.changes);
    assert!(state.permits(&next.actions[0]));
    assert!(!state.permits(action));
}

#[test]
fn a_restart_continuation_cannot_be_admitted_after_a_later_message_finishes() {
    let (state, source) = running();
    let restarted = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(3),
    );
    let state = state.apply(&restarted.changes);
    let job = &restarted.continuations[0];
    let (state, id, action, later) = starting(&state, 4);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(5),
    );
    let state = state.apply(&delivered.changes);
    for terminal in [Observation::Completed, Observation::Interrupted] {
        let ended = observe(&state, &later, terminal, 6);
        let state = state.apply(&ended.changes);
        let resumed = step(
            &state,
            Event::Command {
                id: OperationId::Job(job.id),
                receipt: None,
                command: Command::ContinueAfterRestart {
                    source: source.turn.clone(),
                    enabled: true,
                    content: content(FollowUp::Queue),
                },
            },
            context(7),
        );
        assert!(resumed.changes.is_empty());
        assert!(resumed.actions.is_empty());
        assert_eq!(resumed.outcome, Outcome::Ignored);
    }
}

#[test]
fn a_restart_continuation_yields_to_a_surviving_retry_and_unheld_queued_input() {
    let (state, old) = codex_running();
    let background = observe(
        &state,
        &old,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        3,
    );
    let state = state.apply(&background.changes);
    let (state, id, action, source) = starting(&state, 4);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id,
            action,
        },
        context(5),
    );
    let state = state.apply(&delivered.changes);
    let retry = observe(&state, &old, Observation::Failed(Some(overload())), 6);
    let state = state.apply(&retry.changes);
    let restarted = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(7),
    );
    let state = state.apply(&restarted.changes);
    let job = &restarted.continuations[0];
    let continue_step = |state: &State, seed| {
        step(
            state,
            Event::Command {
                id: OperationId::Job(job.id),
                receipt: None,
                command: Command::ContinueAfterRestart {
                    source: source.turn.clone(),
                    enabled: true,
                    content: content(FollowUp::Queue),
                },
            },
            context(seed),
        )
    };
    assert!(continue_step(&state, 8).changes.is_empty());
    let stopped = command(&state, 9, Command::Interrupt { turn: None });
    let mut state = state.apply(&stopped.changes);
    // An idle owner can load queued rows before receiving its next Wake.
    let id: ClientInputId = Uuid::from_u128(10).to_string().into();
    state.inputs.insert(
        id.clone(),
        PendingInput {
            id,
            position: 3,
            content: content(FollowUp::Queue),
            state: InputState::Queued { steer: None },
        },
    );
    state.counters.input = 3;
    assert!(!state.thread.queue_held);
    assert!(continue_step(&state, 11).changes.is_empty());
}

#[test]
fn an_older_unknown_steer_does_not_prevent_continuing_the_newest_interrupted_turn() {
    let (state, old) = running();
    let steering = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steering.changes);
    let Action::Deliver { id, action, .. } = &steering.actions[0] else {
        panic!("steer")
    };
    let failed = step(
        &state,
        Event::DeliveryFailed {
            id: id.clone(),
            action: *action,
            error: ExecutionError::default(),
            unknown: true,
        },
        context(4),
    );
    let state = state.apply(&failed.changes);
    let background = observe(
        &state,
        &old,
        Observation::Background {
            tasks: vec![BackgroundTask {
                id: "agent".into(),
                kind: crate::execution::BackgroundTaskKind::Subagent,
                description: None,
            }],
        },
        5,
    );
    let state = state.apply(&background.changes);
    let (state, input, action, source) = starting(&state, 6);
    let delivered = step(
        &state,
        Event::Delivered {
            native_turn: None,
            id: input,
            action,
        },
        context(7),
    );
    let state = state.apply(&delivered.changes);
    let restarted = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(8),
    );
    let state = state.apply(&restarted.changes);
    assert_eq!(restarted.continuations.len(), 1);
    let job = &restarted.continuations[0];
    assert_eq!(job.source, source.turn);
    let resumed = step(
        &state,
        Event::Command {
            id: OperationId::Job(job.id),
            receipt: None,
            command: Command::ContinueAfterRestart {
                source: source.turn,
                enabled: true,
                content: content(FollowUp::Queue),
            },
        },
        context(9),
    );
    let delivered = deliver_after_rebuilding(&state, &resumed, 10);
    assert!(matches!(&delivered.actions[..], [Action::Deliver { id: target, .. }] if target != id));
    assert!(state.apply(&resumed.changes).inputs.contains_key(id));
}

#[test]
fn a_foreign_attempt_cannot_prove_delivery_of_the_current_pending_start() {
    let (state, _, _, current) = starting(&state(), 1);
    let stale = Attempt {
        id: Uuid::from_u128(999),
        ..current
    };
    for observation in [
        Observation::Completed,
        Observation::Request {
            id: "stale".into(),
            body: approval(),
        },
    ] {
        assert_eq!(observe(&state, &stale, observation, 2), Step::default());
    }
}

#[test]
fn finishing_a_turn_closes_its_requests_and_only_finishes_its_running_items() {
    for (terminal, expected) in [
        (Observation::Completed, ItemStatus::Completed),
        (Observation::Interrupted, ItemStatus::Interrupted),
        (
            Observation::Failed(Some(ExecutionError::default())),
            ItemStatus::Failed,
        ),
    ] {
        let (state, old) = running();
        let item = observe(
            &state,
            &old,
            Observation::Item {
                deferred: false,
                id: "old-reply".into(),
                status: ItemStatus::Running,
                body: ItemBody::AssistantText {
                    text: "old".into(),
                    phase: AssistantPhase::Commentary,
                },
            },
            3,
        );
        let state = state.apply(&item.changes);
        let request = observe(
            &state,
            &old,
            Observation::Request {
                id: "old-request".into(),
                body: approval(),
            },
            4,
        );
        let state = state.apply(&request.changes);
        let background = observe(
            &state,
            &old,
            Observation::Background {
                tasks: vec![BackgroundTask {
                    id: "agent".into(),
                    kind: crate::execution::BackgroundTaskKind::Subagent,
                    description: None,
                }],
            },
            5,
        );
        let state = state.apply(&background.changes);
        let (state, user, action, foreground) = starting(&state, 6);
        let delivered = step(
            &state,
            Event::Delivered {
                native_turn: None,
                id: user.clone(),
                action,
            },
            context(7),
        );
        let state = state.apply(&delivered.changes);
        let item = observe(
            &state,
            &foreground,
            Observation::Item {
                deferred: false,
                id: "new-reply".into(),
                status: ItemStatus::Running,
                body: ItemBody::AssistantText {
                    text: "new".into(),
                    phase: AssistantPhase::Commentary,
                },
            },
            8,
        );
        let state = state.apply(&item.changes);
        let request = observe(
            &state,
            &foreground,
            Observation::Request {
                id: "new-request".into(),
                body: approval(),
            },
            9,
        );
        let state = state.apply(&request.changes);
        let ended = observe(&state, &foreground, terminal, 10);
        let state = state.apply(&ended.changes);
        assert_eq!(state.items["new-reply"].status, expected);
        assert_eq!(state.items[&ItemId(user.0)].status, ItemStatus::Completed);
        assert_eq!(state.items["old-reply"].status, ItemStatus::Running);
        assert_eq!(state.requests["old-request"].state, RequestState::Awaiting);
        assert!(!state.requests.contains_key("new-request"));
        assert!(matches!(
            state.turns[&old.turn].state,
            TurnState::Waiting(Wait::Background { .. })
        ));
    }
}

fn codex_running() -> (State, Attempt) {
    let (mut state, mut attempt) = running();
    state.thread.provider = ProviderKind::Codex;
    state.turns.get_mut(&attempt.turn).unwrap().provider = ProviderKind::Codex;
    state.bindings.insert(
        ProviderKind::Codex,
        Binding::Ready {
            covered: state.history_end(),
            source: attempt.source.clone(),
        },
    );
    attempt.provider = ProviderKind::Codex;
    state.attempts.insert(attempt.turn.clone(), attempt.clone());
    (state, attempt)
}
fn overload() -> ExecutionError {
    ExecutionError {
        provider_code: Some("serverOverloaded".into()),
        message: "capacity".into(),
        ..Default::default()
    }
}
#[test]
fn capacity_retries_have_five_exact_delays_and_keep_one_canonical_turn() {
    let (mut state, mut attempt) = codex_running();
    for (index, delay) in [2_000, 4_000, 8_000, 16_000, 30_000]
        .into_iter()
        .enumerate()
    {
        let failed = observe(
            &state,
            &attempt,
            Observation::Failed(Some(overload())),
            100 + index as u128,
        );
        assert_eq!(failed.actions, [Action::WaitUntil(100 + delay)]);
        state = state.apply(&failed.changes);
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[&attempt.turn].retry_count, index as u8 + 1);
        let next = step(
            &state,
            Event::Wake,
            Context {
                external_busy: false,
                now: 100 + delay,
                ..context(200 + index as u128)
            },
        );
        let [Action::Resume { attempt: fresh, .. }] = &next.actions[..] else {
            panic!("native continuation")
        };
        let fresh = fresh.clone();
        assert_ne!(fresh.id, attempt.id);
        state = state.apply(&next.changes);
        assert_eq!(
            observe(&state, &attempt, Observation::Completed, 300),
            Step::default()
        );
        attempt = fresh;
    }
    let final_failure = observe(&state, &attempt, Observation::Failed(Some(overload())), 500);
    assert!(final_failure.actions.is_empty());
    assert!(matches!(
        state.apply(&final_failure.changes).turns[&attempt.turn].state,
        TurnState::Failed(_)
    ));
}
#[test]
fn other_errors_and_provider_owned_retries_never_start_a_host_capacity_retry() {
    for (provider, code, retrying) in [
        (ProviderKind::Claude, "serverOverloaded", false),
        (ProviderKind::Codex, "503", false),
        (ProviderKind::Codex, "serverOverloaded", true),
    ] {
        let (mut state, attempt) = codex_running();
        state.turns.get_mut(&attempt.turn).unwrap().provider = provider;
        let error = ExecutionError {
            provider_code: Some(code.into()),
            retry: Some(crate::execution::RetryEvidence {
                retrying,
                attempt: None,
                max_attempts: None,
            }),
            ..Default::default()
        };
        let result = observe(&state, &attempt, Observation::Failed(Some(error)), 3);
        assert!(result.actions.is_empty());
        assert!(matches!(
            state.apply(&result.changes).turns[&attempt.turn].state,
            TurnState::Failed(_)
        ));
    }
}
#[test]
fn manual_input_cancels_a_capacity_wait_and_keeps_queued_input_held() {
    let (state, attempt) = codex_running();
    let queued = submit(&state, 10, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let waiting = observe(&state, &attempt, Observation::Failed(Some(overload())), 11);
    let state = state.apply(&waiting.changes);
    let manual = submit(&state, 12, FollowUp::Queue);
    let next = state.apply(&manual.changes);
    assert_eq!(
        next.turns[&attempt.turn].state,
        TurnState::Failed(ExecutionError::default())
    );
    assert!(matches!(
        next.inputs[Uuid::from_u128(10).to_string().as_str()].state,
        InputState::Queued { .. }
    ));
    assert!(next.thread.queue_held);
    assert!(
        matches!(&manual.actions[..], [Action::Deliver { id, .. }] if id.as_str() == Uuid::from_u128(12).to_string())
    );
    assert!(
        step(
            &next,
            Event::Wake,
            Context {
                external_busy: false,
                now: 50_000,
                ..context(13)
            }
        )
        .actions
        .is_empty()
    );
}
#[test]
fn generated_titles_cannot_replace_a_name_the_user_has_already_chosen() {
    let initial = state();
    let named = command(&initial, 1, Command::Rename("Chosen".into()));
    let named = initial.apply(&named.changes);
    assert_eq!(
        step(&named, Event::Title("Automatic".into()), context(2)),
        Step::default()
    );
    let mut empty = initial;
    empty.thread.name.clear();
    let generated = step(&empty, Event::Title("Automatic".into()), context(3));
    assert_eq!(empty.apply(&generated.changes).thread.name, "Automatic");
}

#[test]
fn native_status_only_accepts_the_current_provider_and_ready_source() {
    let ready = Binding::Ready {
        covered: 0,
        source: "current".into(),
    };
    assert!(current_source(
        ProviderKind::Codex,
        Some(&ready),
        ProviderKind::Codex,
        "current"
    ));
    assert!(!current_source(
        ProviderKind::Claude,
        Some(&ready),
        ProviderKind::Codex,
        "current"
    ));
    assert!(!current_source(
        ProviderKind::Codex,
        Some(&ready),
        ProviderKind::Claude,
        "current"
    ));
    assert!(!current_source(
        ProviderKind::Codex,
        Some(&ready),
        ProviderKind::Codex,
        "previous"
    ));
    assert!(!current_source(
        ProviderKind::Codex,
        None,
        ProviderKind::Codex,
        "current"
    ));
    let fork = Binding::Fork {
        source: "current".into(),
        through: "turn".into(),
    };
    assert!(!current_source(
        ProviderKind::Codex,
        Some(&fork),
        ProviderKind::Codex,
        "current"
    ));
}

#[test]
fn admission_failures_keep_a_receipt_and_replay_the_original_outcome() {
    let state = state();
    let operation = OperationId::Client(Uuid::from_u128(900));
    let rejected = Outcome::Rejected("selected account is unavailable".into());
    let result = step(
        &state,
        Event::RejectedCommand {
            id: operation,
            reason: "selected account is unavailable".into(),
            receipt: None,
        },
        context(900),
    );
    assert_eq!(result.outcome, rejected);
    assert_eq!(result.receipt, Some((operation, rejected.clone())));
    assert!(
        result.changes.is_empty() && result.actions.is_empty() && result.continuations.is_empty()
    );
    for original in [rejected, Outcome::Accepted, Outcome::DeliveryUnknown] {
        let replay = step(
            &state,
            Event::RejectedCommand {
                id: operation,
                reason: "a later account failure".into(),
                receipt: Some(original.clone()),
            },
            context(901),
        );
        assert_eq!(replay.outcome, original);
        assert!(replay.receipt.is_none() && replay.changes.is_empty() && replay.actions.is_empty());
    }
}

#[test]
fn unmaterialized_external_work_blocks_start_and_steering_until_the_current_source_is_idle() {
    for follow_up in [FollowUp::Queue, FollowUp::Steer] {
        let initial = state();
        let id = Uuid::from_u128(902).to_string().into();
        let admitted = step(
            &initial,
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(902)),
                receipt: None,
                command: Command::Submit {
                    id,
                    content: content(follow_up),
                },
            },
            Context {
                external_busy: true,
                ..context(902)
            },
        );
        assert_eq!(admitted.outcome, Outcome::Accepted);
        assert!(admitted.actions.is_empty());
        let queued = initial.apply(&admitted.changes);
        assert!(queued.turns.is_empty());
        assert!(matches!(
            queued.inputs.values().next().unwrap().state,
            InputState::Queued { .. }
        ));
        let other = step(
            &queued,
            Event::NativeStatus {
                provider: ProviderKind::Claude,
                source: "previous-source".into(),
                status: crate::models::SessionStatus::Idle,
            },
            context(903),
        );
        assert!(other.actions.is_empty() && other.changes.is_empty());
        let ready = step(
            &queued,
            Event::NativeStatus {
                provider: ProviderKind::Claude,
                source: "native-session".into(),
                status: crate::models::SessionStatus::Idle,
            },
            context(904),
        );
        assert!(
            matches!(&ready.actions[..],[Action::Deliver {attempt,steer:false,..}] if attempt.source=="native-session")
        );
    }
}

#[test]
fn unwritten_steering_requeues_only_the_matching_delivery_and_waits_for_an_addressable_attempt() {
    let (running, attempt) = running();
    let delivery = submit(&running, 3, FollowUp::Steer);
    let state = running.apply(&delivery.changes);
    let Action::Deliver { id, action, .. } = &delivery.actions[0] else {
        panic!("steering delivery")
    };
    let waiting = Context {
        steer_ready: false,
        ..context(4)
    };
    assert_eq!(
        step(
            &state,
            Event::DeliveryDeferred {
                id: id.clone(),
                action: Uuid::from_u128(u128::MAX - 2)
            },
            waiting
        ),
        Step::default()
    );
    let (starting, input, token, _) = starting(&self::state(), 1);
    assert_eq!(
        step(
            &starting,
            Event::DeliveryDeferred {
                id: input,
                action: token
            },
            waiting
        ),
        Step::default()
    );
    let deferred = step(
        &state,
        Event::DeliveryDeferred {
            id: id.clone(),
            action: *action,
        },
        waiting,
    );
    assert_eq!(deferred.outcome, Outcome::Accepted);
    assert!(deferred.actions.is_empty());
    let queued = state.apply(&deferred.changes);
    assert_eq!(queued.inputs[id].position, state.inputs[id].position);
    assert!(
        matches!(queued.inputs[id].state, InputState::Queued { steer: Some(current) } if current == attempt.id)
    );
    assert!(!queued.permits(&delivery.actions[0]));
    let next = step(&queued, Event::Wake, context(5));
    assert!(
        matches!(next.actions.as_slice(), [Action::Deliver { id: current, action: token, attempt: target, steer: true, .. }] if current == id && token != action && target == &attempt)
    );
    let delivered = queued.apply(&next.changes);
    assert!(step(&delivered, Event::Wake, context(6)).actions.is_empty());
}

#[test]
fn a_terminal_failure_without_details_keeps_the_prior_notice_and_its_retry_owner() {
    for retrying in [false, true] {
        let (state, attempt) = codex_running();
        let notice = ExecutionError {
            retry: Some(crate::execution::RetryEvidence {
                retrying,
                attempt: None,
                max_attempts: None,
            }),
            ..overload()
        };
        let noted = observe(&state, &attempt, Observation::Notice(notice), 10);
        let state = state.apply(&noted.changes);
        let failed = observe(&state, &attempt, Observation::Failed(None), 11);
        let next = state.apply(&failed.changes);
        if retrying {
            assert!(failed.actions.is_empty());
            assert!(
                matches!(&next.turns[&attempt.turn].state, TurnState::Failed(error) if error.message == "capacity" && error.retry.is_none())
            );
        } else {
            assert_eq!(failed.actions, [Action::WaitUntil(2100)]);
            assert_eq!(
                next.turns[&attempt.turn].error.as_ref().unwrap().message,
                "capacity"
            );
        }
    }
}

#[test]
fn failed_compaction_never_resumes_the_preceding_task() {
    for (text, compact) in [
        ("/compact", true),
        ("  /CoMpAcT\n", true),
        ("/compact please", false),
    ] {
        for steer in [false, true] {
            let (state, active) = codex_running();
            let state = if steer {
                state
            } else {
                state.apply(&observe(&state, &active, Observation::Completed, 10).changes)
            };
            let admitted = command(
                &state,
                200,
                Command::Submit {
                    id: Uuid::from_u128(200).to_string().into(),
                    content: InputContent {
                        parts: vec![InputPart::Text { text: text.into() }],
                        ..content(if steer {
                            FollowUp::Steer
                        } else {
                            FollowUp::Queue
                        })
                    },
                },
            );
            if steer && compact {
                assert!(matches!(admitted.outcome, Outcome::Rejected(_)));
                assert!(admitted.changes.is_empty());
                assert!(admitted.actions.is_empty());
                continue;
            }
            let [
                Action::Deliver {
                    id,
                    action,
                    attempt,
                    ..
                },
            ] = &admitted.actions[..]
            else {
                panic!("admitted input")
            };
            let state = state.apply(&admitted.changes);
            let acknowledged = step(
                &state,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: None,
                },
                context(201),
            );
            let mut state = state.apply(&acknowledged.changes);
            // The owner can omit completed transcript bodies from the working set.
            state.items.clear();
            if compact {
                let restart = step(
                    &state,
                    Event::Restart {
                        updating: true,
                        continue_after_restart: true,
                    },
                    context(203),
                );
                assert!(restart.continuations.is_empty());
                let resumed = step(
                    &state.apply(&restart.changes),
                    Event::Command {
                        id: OperationId::Job(Uuid::from_u128(204)),
                        receipt: None,
                        command: Command::ContinueAfterRestart {
                            source: attempt.turn.clone(),
                            enabled: true,
                            content: content(FollowUp::Queue),
                        },
                    },
                    context(204),
                );
                assert!(resumed.changes.is_empty());
                assert!(resumed.actions.is_empty());
                // Ordinary follow-ups cannot be delivered into a compacting turn.
                let queued = submit(&state, 205, FollowUp::Steer);
                assert_eq!(queued.outcome, Outcome::Accepted);
                assert!(queued.actions.is_empty());
                let next = state.apply(&queued.changes);
                let completed = observe(&next, attempt, Observation::Completed, 206);
                assert!(matches!(
                    &completed.actions[..],
                    [Action::Deliver { steer: false, .. }]
                ));
            }
            let failed = observe(&state, attempt, Observation::Failed(Some(overload())), 202);
            assert_eq!(
                failed.actions.is_empty(),
                compact,
                "steer={steer}, text={text:?}"
            );
            if compact {
                assert!(
                    matches!(&state.apply(&failed.changes).turns[&attempt.turn].state, TurnState::Failed(error) if error.message == "capacity")
                );
            } else {
                assert_eq!(failed.actions, [Action::WaitUntil(2100)]);
            }
        }
    }
}

#[test]
fn editing_a_queued_steer_into_compact_does_not_block_later_steering() {
    let (mut state, active) = codex_running();
    for n in [200u128, 201] {
        let mut waiting = context(n);
        waiting.steer_ready = false;
        let queued = step(
            &state,
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(n)),
                receipt: None,
                command: Command::Submit {
                    id: Uuid::from_u128(n).to_string().into(),
                    content: content(FollowUp::Steer),
                },
            },
            waiting,
        );
        assert_eq!(queued.outcome, Outcome::Accepted);
        assert!(queued.actions.is_empty());
        state = state.apply(&queued.changes);
    }
    let compact: ClientInputId = Uuid::from_u128(200).to_string().into();
    let edited = command(
        &state,
        202,
        Command::EditInput {
            id: compact.clone(),
            parts: vec![InputPart::Text {
                text: "/compact".into(),
            }],
        },
    );
    let state = state.apply(&edited.changes);
    let next = step(&state, Event::Wake, context(203));
    assert!(
        matches!(&next.actions[..], [Action::Deliver { id, steer: true, .. }] if id.as_str() == Uuid::from_u128(201).to_string())
    );
    let state = state.apply(&next.changes);
    let Action::Deliver { id, action, .. } = &next.actions[0] else {
        unreachable!()
    };
    let delivered = step(
        &state,
        Event::Delivered {
            id: id.clone(),
            action: *action,
            native_turn: None,
        },
        context(204),
    );
    let state = state.apply(&delivered.changes);
    let completed = observe(&state, &active, Observation::Completed, 205);
    assert!(
        matches!(&completed.actions[..], [Action::Deliver { id, steer: false, .. }] if id == &compact)
    );
}

fn starting_compaction() -> (State, ClientInputId, Uuid, Attempt) {
    let (state, old) = codex_running();
    let state = state.apply(&observe(&state, &old, Observation::Completed, 10).changes);
    let submitted = command(
        &state,
        200,
        Command::Submit {
            id: Uuid::from_u128(200).to_string().into(),
            content: InputContent {
                parts: vec![InputPart::Text {
                    text: "/compact".into(),
                }],
                ..content(FollowUp::Queue)
            },
        },
    );
    let [
        Action::Deliver {
            id,
            action,
            attempt,
            ..
        },
    ] = &submitted.actions[..]
    else {
        panic!("compact delivery")
    };
    (
        state.apply(&submitted.changes),
        id.clone(),
        *action,
        attempt.clone(),
    )
}

#[test]
fn compact_promotion_sends_only_the_selected_input_and_preserves_the_queue_hold() {
    for before_ack in [false, true] {
        for held in [false, true] {
            for terminal in [
                Observation::Completed,
                Observation::Interrupted,
                Observation::Failed(None),
            ] {
                let (mut state, id, action, compact) = starting_compaction();
                if !before_ack {
                    state = state.apply(
                        &step(
                            &state,
                            Event::Delivered {
                                id: id.clone(),
                                action,
                                native_turn: None,
                            },
                            context(201),
                        )
                        .changes,
                    );
                }
                for n in [202, 203] {
                    state = state.apply(&submit(&state, n, FollowUp::Queue).changes);
                }
                state.thread.queue_held = held;
                let selected: ClientInputId = Uuid::from_u128(203).to_string().into();
                let promoted = command(&state, 204, Command::SteerInput(selected.clone()));
                assert_eq!(promoted.outcome, Outcome::Accepted);
                state = state.apply(&promoted.changes);
                if before_ack {
                    let ack = step(
                        &state,
                        Event::Delivered {
                            id,
                            action,
                            native_turn: None,
                        },
                        context(205),
                    );
                    assert!(matches!(&ack.actions[..], [Action::Interrupt { .. }]));
                    state = state.apply(&ack.changes);
                } else {
                    assert!(matches!(&promoted.actions[..], [Action::Interrupt { .. }]));
                }
                let ended = observe(&state, &compact, terminal, 206);
                let [
                    Action::Deliver {
                        id,
                        action,
                        attempt,
                        steer: false,
                        ..
                    },
                ] = &ended.actions[..]
                else {
                    panic!("selected input must start")
                };
                assert_eq!(id, &selected);
                state = state.apply(&ended.changes);
                let ack = step(
                    &state,
                    Event::Delivered {
                        id: id.clone(),
                        action: *action,
                        native_turn: None,
                    },
                    context(207),
                );
                state = state.apply(&ack.changes);
                let completed = observe(&state, attempt, Observation::Completed, 208);
                assert_eq!(
                    completed.actions.is_empty(),
                    held,
                    "other inputs obey the original hold"
                );
                let state = state.apply(&completed.changes);
                assert_eq!(state.thread.queue_held, held);
                if held {
                    assert!(step(&state, Event::Wake, context(209)).actions.is_empty());
                    assert!(matches!(
                        &command(&state, 210, Command::ResumeQueue).actions[..],
                        [Action::Deliver { .. }]
                    ));
                }
            }
        }
    }
}

#[test]
fn editing_the_promoted_queue_position_during_compact_respects_the_original_hold() {
    for before_ack in [false, true] {
        for held in [false, true] {
            for remove in [false, true] {
                for terminal in [
                    Observation::Completed,
                    Observation::Interrupted,
                    Observation::Failed(None),
                ] {
                    let (mut state, compact_input, action, compact) = starting_compaction();
                    if !before_ack {
                        state = state.apply(
                            &step(
                                &state,
                                Event::Delivered {
                                    id: compact_input.clone(),
                                    action,
                                    native_turn: None,
                                },
                                context(201),
                            )
                            .changes,
                        );
                    }
                    for n in [202, 203] {
                        state = state.apply(&submit(&state, n, FollowUp::Queue).changes);
                    }
                    state.thread.queue_held = held;
                    let first: ClientInputId = Uuid::from_u128(202).to_string().into();
                    let selected: ClientInputId = Uuid::from_u128(203).to_string().into();
                    let promoted = command(&state, 204, Command::SteerInput(selected.clone()));
                    state = state.apply(&promoted.changes);
                    let change = if remove {
                        Command::RemoveInput(selected.clone())
                    } else {
                        Command::MoveInput {
                            id: first.clone(),
                            before: Some(selected.clone()),
                        }
                    };
                    let changed = command(&state, 205, change);
                    assert_eq!(changed.outcome, Outcome::Accepted);
                    state = state.apply(&changed.changes);
                    if before_ack {
                        state = state.apply(
                            &step(
                                &state,
                                Event::Delivered {
                                    id: compact_input,
                                    action,
                                    native_turn: None,
                                },
                                context(206),
                            )
                            .changes,
                        );
                    }
                    let ended = observe(&state, &compact, terminal, 207);
                    if held {
                        assert!(ended.actions.is_empty());
                    } else {
                        assert!(
                            matches!(&ended.actions[..], [Action::Deliver { id, .. }] if id == &first)
                        );
                    }
                    assert_eq!(state.apply(&ended.changes).thread.queue_held, held);
                }
            }
        }
    }
}

#[test]
fn another_compact_promotion_updates_the_selection_without_a_second_native_interrupt() {
    let (state, input, action, compact) = starting_compaction();
    let mut state = state.apply(
        &step(
            &state,
            Event::Delivered {
                id: input,
                action,
                native_turn: None,
            },
            context(201),
        )
        .changes,
    );
    state.thread.queue_held = true;
    for n in [202, 203] {
        state = state.apply(&submit(&state, n, FollowUp::Queue).changes);
    }
    let selected = |n| Uuid::from_u128(n).to_string().into();
    let promoted = command(&state, 204, Command::SteerInput(selected(202)));
    assert!(matches!(&promoted.actions[..], [Action::Interrupt { .. }]));
    state = state.apply(&promoted.changes);
    let again = command(&state, 205, Command::SteerInput(selected(203)));
    assert_eq!(again.outcome, Outcome::Accepted);
    assert!(again.actions.is_empty());
    state = state.apply(&again.changes);
    let ended = observe(&state, &compact, Observation::Interrupted, 206);
    assert!(matches!(&ended.actions[..],[Action::Deliver {id,..}] if *id==selected(203)));
    assert!(matches!(
        state.apply(&ended.changes).inputs[&selected(202)].state,
        InputState::Queued { .. }
    ));
}

#[test]
fn stopping_or_restarting_a_compact_promotion_cancels_its_selected_input() {
    for before_ack in [false, true] {
        for restart in [false, true] {
            let (mut state, id, action, compact) = starting_compaction();
            if !before_ack {
                state = state.apply(
                    &step(
                        &state,
                        Event::Delivered {
                            id: id.clone(),
                            action,
                            native_turn: None,
                        },
                        context(201),
                    )
                    .changes,
                );
            }
            state = state.apply(&submit(&state, 202, FollowUp::Queue).changes);
            state = state.apply(
                &command(
                    &state,
                    203,
                    Command::SteerInput(Uuid::from_u128(202).to_string().into()),
                )
                .changes,
            );
            let stopped = if restart {
                step(
                    &state,
                    Event::Restart {
                        updating: true,
                        continue_after_restart: true,
                    },
                    context(204),
                )
            } else {
                command(&state, 204, Command::Interrupt { turn: None })
            };
            assert!(stopped.continuations.is_empty());
            state = state.apply(&stopped.changes);
            if !restart {
                if before_ack {
                    state = state.apply(
                        &step(
                            &state,
                            Event::Delivered {
                                id,
                                action,
                                native_turn: None,
                            },
                            context(205),
                        )
                        .changes,
                    );
                }
                let ended = observe(&state, &compact, Observation::Interrupted, 206);
                assert!(ended.actions.is_empty());
                state = state.apply(&ended.changes);
            }
            assert!(state.thread.queue_held);
            assert!(step(&state, Event::Wake, context(207)).actions.is_empty());
        }
        // A later queued message must not be mistaken for promotion after Stop.
        let (mut state, id, action, compact) = starting_compaction();
        if !before_ack {
            state = state.apply(
                &step(
                    &state,
                    Event::Delivered {
                        id: id.clone(),
                        action,
                        native_turn: None,
                    },
                    context(201),
                )
                .changes,
            );
        }
        state = state.apply(&command(&state, 202, Command::Interrupt { turn: None }).changes);
        state = state.apply(&submit(&state, 203, FollowUp::Queue).changes);
        if before_ack {
            state = state.apply(
                &step(
                    &state,
                    Event::Delivered {
                        id,
                        action,
                        native_turn: None,
                    },
                    context(204),
                )
                .changes,
            );
        }
        let ended = observe(&state, &compact, Observation::Interrupted, 205);
        assert!(ended.actions.is_empty());
        assert!(state.apply(&ended.changes).thread.queue_held);
    }
}

#[test]
fn a_queued_compact_cannot_be_promoted_into_an_ordinary_running_turn() {
    let (state, _) = codex_running();
    let queued = command(
        &state,
        200,
        Command::Submit {
            id: Uuid::from_u128(200).to_string().into(),
            content: InputContent {
                parts: vec![InputPart::Text {
                    text: "/compact".into(),
                }],
                ..content(FollowUp::Queue)
            },
        },
    );
    let state = state.apply(&queued.changes);
    let promoted = command(
        &state,
        201,
        Command::SteerInput(Uuid::from_u128(200).to_string().into()),
    );
    assert!(matches!(promoted.outcome, Outcome::Rejected(_)));
    assert!(promoted.changes.is_empty());
    assert!(promoted.actions.is_empty());
}

#[test]
fn an_unacknowledged_regular_input_does_not_offer_compaction_promotion() {
    let (state, _, _, _) = starting(&state(), 1);
    let queued = submit(&state, 2, FollowUp::Queue);
    assert!(queued.actions.is_empty());
    let state = state.apply(&queued.changes);
    let promotion = command(
        &state,
        3,
        Command::SteerInput(Uuid::from_u128(2).to_string().into()),
    );
    assert!(matches!(promotion.outcome, Outcome::Rejected(_)));
    assert!(promotion.changes.is_empty());
    assert!(promotion.actions.is_empty());
    assert!(state.steer_target(true).is_none());
}
