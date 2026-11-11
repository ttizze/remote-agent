use super::*;
use crate::items::AssistantPhase;

fn context(seed: u128) -> Context {
    Context {
        now: 100,
        seed: Uuid::from_u128(seed),
        steers: true,
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

fn running() -> (State, Attempt) {
    let (state, id, action, attempt) = starting(&state(), 1);
    let delivered = step(&state, Event::Delivered { id, action }, context(2));
    (state.apply(&delivered.changes), attempt)
}

fn observe(state: &State, attempt: &Attempt, observation: Observation, seed: u128) -> Step {
    let result = step(
        state,
        Event::Provider {
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
    let duplicate = step(&state, Event::Delivered { id, action }, context(3));
    assert_eq!(duplicate, Step::default());
    assert_eq!(initial, self::state());
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
        step(&state, Event::Delivered { id, action }, context(3))
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
    let ack = step(&state, Event::Delivered { id, action }, context(5));
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
    assert!(
        matches!(&resumed.actions[..], [Action::Deliver { attempt: next, .. }] if next.id != attempt.id)
    );
}

#[test]
fn interrupted_and_failed_turns_hold_the_queue_and_fence_late_events() {
    for terminal in [
        Observation::Interrupted,
        Observation::Failed(ExecutionError::default()),
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
    let interrupted = command(&state, 5, Command::Interrupt);
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
    let (state, attempt) = running();
    let waiting = observe(&state, &attempt, Observation::Retry { at: 200 }, 3);
    assert_eq!(waiting.actions, vec![Action::WaitUntil(200)]);
    let state = state.apply(&waiting.changes);
    let recovered = step(
        &state,
        Event::Restart {
            updating: true,
            continue_after_restart: true,
        },
        context(4),
    );
    assert_eq!(recovered.actions, vec![Action::WaitUntil(200)]);
    assert!(recovered.continuations.is_empty());
    let state = state.apply(&recovered.changes);
    let early = step(
        &state,
        Event::Wake,
        Context {
            now: 199,
            ..context(5)
        },
    );
    assert_eq!(early.actions, vec![Action::WaitUntil(200)]);
    assert!(early.changes.is_empty());
    let due = step(
        &state,
        Event::Wake,
        Context {
            now: 200,
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
    let worktree = step(
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
        &worktree.actions[..],
        [Action::Prepare {
            stage: PreparationStage::Worktree,
            ..
        }]
    ));
    let state = state.apply(&worktree.changes);
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
            stage: PreparationStage::Worktree,
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
    let retried = command(&state, 3, Command::RetryInput(id.clone()));
    assert!(
        matches!(&retried.actions[..], [Action::Prepare { action: next, stage: PreparationStage::Workspace, .. }] if next != action)
    );
}

#[test]
fn completed_item_updates_keep_their_position_without_reopening_the_turn() {
    let (state, attempt) = running();
    let item = Observation::Item {
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
fn recovery_discards_partial_bindings_and_serialization_preserves_state() {
    let mut state = state();
    state.bindings.insert(
        ProviderKind::Codex,
        Binding::Ready {
            source: "ready".into(),
        },
    );
    state.bindings.insert(
        ProviderKind::Claude,
        Binding::Injecting {
            source: "partial".into(),
            action: Uuid::nil(),
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
    assert!(!state.bindings.contains_key(&ProviderKind::Claude));
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
    let stop = command(&state, 2, Command::Interrupt);
    let state = state.apply(&stop.changes);
    assert_eq!(stop.outcome, Outcome::Accepted);
    assert!(stop.actions.is_empty());
    let delivered = step(
        &state,
        Event::Delivered {
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
        step(&state, Event::Delivered { id, action }, context(4)),
        Step::default()
    );
    let replay = step(
        &state,
        Event::Command {
            id: OperationId::Client(Uuid::from_u128(2)),
            command: Command::Interrupt,
            receipt: Some(Outcome::Accepted),
        },
        context(5),
    );
    assert!(replay.actions.is_empty());
    assert!(matches!(
        &command(&state, 6, Command::Interrupt).actions[..],
        [Action::Interrupt { .. }]
    ));
}

#[test]
fn completion_before_the_start_ack_does_not_send_an_obsolete_interrupt() {
    let (state, _, _, attempt) = starting(&state(), 1);
    let stop = command(&state, 2, Command::Interrupt);
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
    let cancelled = command(&state, 2, Command::Interrupt);
    assert_eq!(
        cancelled.actions,
        vec![Action::CancelPreparation { action: *action }]
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
    let retry = command(&state, 4, Command::RetryInput(id.clone()));
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
            source: "prepared-native".into()
        }
    );
    assert!(state.turns.is_empty());
    assert!(state.attempts.is_empty());
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
        Observation::Failed(ExecutionError::default()),
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
    let stop = command(&state, 5, Command::Interrupt);
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
                answer: Answer::Approval {
                    choice_id: "allow".into(),
                },
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
    let cancelled = command(&state, 3, Command::Interrupt);
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
    let (state, attempt) = running();
    let steer = submit(&state, 3, FollowUp::Steer);
    let state = state.apply(&steer.changes);
    let Action::Deliver { id, action, .. } = &steer.actions[0] else {
        unreachable!()
    };
    let in_flight = state.inputs[id].clone();
    let retry = observe(&state, &attempt, Observation::Retry { at: 200 }, 4);
    let state = state.apply(&retry.changes);
    assert!(matches!(&state.inputs[id].state,
        InputState::Unknown { delivery, status: UnknownState::Fenced, .. } if delivery.attempt.turn == attempt.turn));
    assert_eq!(
        step(
            &state,
            Event::Delivered {
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
            now: 200,
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
        (4, Some("/base"), None),
        (5, Some("/worktree"), None),
        (6, None, Some("native-second")),
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
            id: second_id,
            action: token,
        },
        context(7),
    );
    state = state.apply(&delivered.changes);
    let completed = observe(&state, &attempt, Observation::Completed, 8);
    state = state.apply(&completed.changes);
    let retried = command(&state, 9, Command::RetryInput(id.clone()));
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
    let queued = submit(&state, 5, FollowUp::Queue);
    let state = state.apply(&queued.changes);
    let retry = observe(&state, &attempt, Observation::Retry { at: 200 }, 6);
    assert_eq!(retry.actions, vec![Action::WaitUntil(200)]);
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
fn preparation_completion_drains_the_head_after_retry_or_resume_changes_queue_order() {
    for resume in [false, true] {
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
        let retried = command(&state, 4, Command::RetryInput(first_id.clone()));
        assert!(retried.actions.is_empty());
        state = state.apply(&retried.changes);
        if resume {
            let held = command(&state, 5, Command::HoldQueue);
            state = state.apply(&held.changes);
            let resumed = command(&state, 6, Command::ResumeQueue);
            assert!(resumed.actions.is_empty());
            state = state.apply(&resumed.changes);
        }
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
            let stop = command(&state, 3, Command::Interrupt);
            let state = state.apply(&stop.changes);
            let observation = if retry {
                Observation::Retry { at: 200 }
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
    assert!(matches!(&resumed.actions[..], [Action::Deliver { id, .. }]
        if id.as_str() == job.id.to_string()));
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
    let stopped = command(&state, 3, Command::Interrupt);
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
    let stopped = command(&state, 5, Command::Interrupt);
    assert!(stopped.actions.is_empty());
    let state = state.apply(&stopped.changes);
    let delivered = step(
        &state,
        Event::Delivered {
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
        let stopped = command(&state, 4, Command::Interrupt);
        assert!(
            matches!(&stopped.actions[..], [Action::Interrupt { attempt: target, .. }]
            if target == &attempt)
        );
        assert_eq!(
            step(&state, Event::Delivered { id, action }, context(5)),
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
    let stopped = command(&state, 3, Command::Interrupt);
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
    let stopped = command(&state, 7, Command::Interrupt);
    let state = state.apply(&stopped.changes);
    let delivered = step(&state, Event::Delivered { id, action }, context(8));
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
                assert!(
                    matches!(&resumed.actions[..], [Action::Deliver { attempt: next, .. }]
                    if next.id != attempt.id)
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
        Observation::Failed(ExecutionError::default()),
    ] {
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
        let (state, id, action, foreground) = starting(&state, 4);
        let delivered = step(&state, Event::Delivered { id, action }, context(5));
        let state = state.apply(&delivered.changes);
        let waiting = observe(&state, &old, Observation::Retry { at: 200 }, 6);
        let state = state.apply(&waiting.changes);
        let wake = step(
            &state,
            Event::Wake,
            Context {
                now: 200,
                ..context(7)
            },
        );
        assert!(wake.actions.is_empty());
        let ended = observe(&state, &foreground, terminal, 8);
        assert_eq!(ended.actions, vec![Action::WaitUntil(200)]);
        let state = state.apply(&ended.changes);
        let resumed = step(
            &state,
            Event::Wake,
            Context {
                now: 201,
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
    let (state, attempt) = running();
    let waiting = observe(&state, &attempt, Observation::Retry { at: 200 }, 3);
    let state = state.apply(&waiting.changes);
    let stopped = command(&state, 4, Command::Interrupt);
    let state = state.apply(&stopped.changes);
    assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
    assert!(
        step(
            &state,
            Event::Wake,
            Context {
                now: 201,
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
    let held = command(&state, 4, Command::HoldQueue);
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
    let (state, attempt) = running();
    let waiting = observe(&state, &attempt, Observation::Retry { at: 100 }, 3);
    let state = state.apply(&waiting.changes);
    let resumed = step(&state, Event::Wake, context(4));
    let state = state.apply(&resumed.changes);
    let action = &resumed.actions[0];
    let Action::Resume {
        attempt: current, ..
    } = action
    else {
        panic!("resume")
    };
    assert!(state.permits(action));
    let stopped = command(&state, 5, Command::Interrupt);
    assert!(!state.apply(&stopped.changes).permits(action));
    let completed = observe(&state, current, Observation::Completed, 6);
    assert!(!state.apply(&completed.changes).permits(action));
    let retry = observe(&state, current, Observation::Retry { at: 100 }, 7);
    let state = state.apply(&retry.changes);
    let next = step(&state, Event::Wake, context(8));
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
    let delivered = step(&state, Event::Delivered { id, action }, context(5));
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
    let (state, id, action, source) = starting(&state, 4);
    let delivered = step(&state, Event::Delivered { id, action }, context(5));
    let state = state.apply(&delivered.changes);
    let retry = observe(&state, &old, Observation::Retry { at: 200 }, 6);
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
    let stopped = command(&state, 9, Command::Interrupt);
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
    let delivered = step(&state, Event::Delivered { id: input, action }, context(7));
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
    assert!(matches!(&resumed.actions[..], [Action::Deliver { id: target, .. }] if target != id));
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
            Observation::Failed(ExecutionError::default()),
            ItemStatus::Failed,
        ),
    ] {
        let (state, old) = running();
        let item = observe(
            &state,
            &old,
            Observation::Item {
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
