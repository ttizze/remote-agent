use super::*;

const FRESH: &str = "00000000-0000-0000-0000-000000000014";

fn fresh(state: &State, seed: u128) -> Step {
    command(
        state,
        seed,
        Command::Submit {
            id: FRESH.into(),
            content: InputContent {
                fresh_context: true,
                model: Some(ModelRef {
                    provider: ProviderKind::Codex,
                    id: "fresh-model".into(),
                }),
                ..content(FollowUp::Steer)
            },
        },
    )
}

fn history() -> State {
    let (state, attempt) = running();
    state.apply(&observe(&state, &attempt, Observation::Completed, 10).changes)
}

fn prepare(state: &State, result: &Step) -> (State, Uuid) {
    let [
        Action::Prepare {
            id,
            action,
            stage: PreparationStage::Provider,
            ..
        },
    ] = &result.actions[..]
    else {
        panic!(
            "fresh input must create a provider source: {:?}",
            result.actions
        );
    };
    assert_eq!(id.as_str(), FRESH);
    (state.apply(&result.changes), *action)
}

fn delivery(state: &State, action: Uuid) -> (State, Uuid, Attempt) {
    let result = step(
        state,
        Event::Prepared {
            id: FRESH.into(),
            action,
            cwd: Some("/project".into()),
            source: Some("new-native".into()),
        },
        context(21),
    );
    let [
        Action::Deliver {
            action,
            attempt,
            steer,
            ..
        },
    ] = &result.actions[..]
    else {
        panic!("delivery missing");
    };
    assert!(!steer);
    (state.apply(&result.changes), *action, attempt.clone())
}

#[test]
fn fresh_context_preserves_history_and_invalidates_every_provider_binding() {
    let mut original = history();
    original.bindings.insert(
        ProviderKind::Codex,
        Binding::Ready {
            source: "other-old-context".into(),
            covered: 1,
        },
    );
    let result = fresh(&original, 20);
    let (prepared, action) = prepare(&original, &result);
    assert!(prepared.bindings.is_empty());
    assert_eq!(prepared.thread.context_start, 1);
    assert_eq!(prepared.turns, original.turns);
    let (sending, action, attempt) = delivery(&prepared, action);
    let accepted = step(
        &sending,
        Event::Delivered {
            id: FRESH.into(),
            action,
            native_turn: None,
        },
        context(22),
    );
    let accepted = sending.apply(&accepted.changes);
    assert_eq!(accepted.turns.len(), 2);
    assert!(accepted.turns[&attempt.turn].fresh_context);
    assert_eq!(accepted.bindings.len(), 1);
    assert!(
        matches!(&accepted.bindings[&ProviderKind::Codex], Binding::Ready { source, .. } if source == "new-native")
    );
    assert!(!accepted.inputs.contains_key(FRESH));
}

#[test]
fn fresh_context_waits_for_running_and_background_work_and_cannot_be_steered() {
    for background in [false, true] {
        let (mut original, attempt) = running();
        if background {
            original = original.apply(
                &observe(
                    &original,
                    &attempt,
                    Observation::Background {
                        tasks: vec![BackgroundTask {
                            id: "background".into(),
                            kind: crate::execution::BackgroundTaskKind::Command,
                            description: None,
                        }],
                    },
                    11,
                )
                .changes,
            );
        }
        let result = fresh(&original, 20);
        assert!(result.actions.is_empty());
        let mut queued = original.apply(&result.changes);
        assert_eq!(queued.bindings, original.bindings);
        assert_eq!(queued.thread.context_start, 0);
        assert!(matches!(
            queued.inputs[FRESH].state,
            InputState::Queued { steer: None }
        ));
        let steer = command(&queued, 21, Command::SteerInput(FRESH.into()));
        assert!(matches!(steer.outcome, Outcome::Rejected(_)));
        assert!(steer.actions.is_empty());
        queued = queued.apply(
            &command(
                &queued,
                22,
                Command::EditInput {
                    id: FRESH.into(),
                    parts: vec![InputPart::Text {
                        text: "edited".into(),
                    }],
                },
            )
            .changes,
        );
        assert!(queued.inputs[FRESH].content.fresh_context);
        let finished = observe(&queued, &attempt, Observation::Completed, 23);
        prepare(&queued, &finished);
    }
}

#[test]
fn a_failed_or_stopped_reset_restores_context_and_retry_starts_fresh_again() {
    for failure in ["prepare", "delivery", "stop"] {
        let original = history();
        let (preparing, action) = prepare(&original, &fresh(&original, 20));
        let (state, event) = if failure == "delivery" {
            let (sending, action, _) = delivery(&preparing, action);
            (
                sending,
                Event::DeliveryFailed {
                    id: FRESH.into(),
                    action,
                    unknown: false,
                    error: ExecutionError::default(),
                },
            )
        } else if failure == "stop" {
            (
                preparing,
                Event::Command {
                    id: OperationId::Client(Uuid::from_u128(30)),
                    command: Command::Interrupt { turn: None },
                    receipt: None,
                },
            )
        } else {
            (
                preparing,
                Event::PreparationFailed {
                    id: FRESH.into(),
                    action,
                    error: ExecutionError::default(),
                },
            )
        };
        let failed = state.apply(&step(&state, event, context(30)).changes);
        assert_eq!(
            failed.thread.context_start, original.thread.context_start,
            "{failure}"
        );
        assert_eq!(failed.bindings, original.bindings, "{failure}");
        assert_eq!(
            failed.thread.provider, original.thread.provider,
            "{failure}"
        );
        assert!(failed.inputs[FRESH].context_reset.is_none());
        let retry = command(
            &failed,
            31,
            Command::RetryInput {
                id: FRESH.into(),
                work_locally: false,
            },
        );
        let (retry, _) = prepare(&failed, &retry);
        assert!(retry.bindings.is_empty());
        assert!(retry.thread.context_start >= 1);
    }
}

#[test]
fn ambiguous_reset_keeps_its_boundary_and_is_not_automatically_replayed_after_restart() {
    let original = history();
    let (preparing, action) = prepare(&original, &fresh(&original, 20));
    let (sending, action, _) = delivery(&preparing, action);
    let unknown = sending.apply(
        &step(
            &sending,
            Event::DeliveryFailed {
                id: FRESH.into(),
                action,
                unknown: true,
                error: ExecutionError::default(),
            },
            context(22),
        )
        .changes,
    );
    assert_eq!(unknown.thread.context_start, 1);
    assert!(unknown.inputs[FRESH].context_reset.is_some());
    let recovered = step(
        &unknown,
        Event::Restart {
            updating: false,
            continue_after_restart: false,
        },
        context(23),
    );
    assert!(recovered.actions.is_empty());
    let recovered = unknown.apply(&recovered.changes);
    assert_eq!(recovered.thread.context_start, 1);
    assert!(matches!(
        recovered.inputs[FRESH].state,
        InputState::Unknown { .. }
    ));
}

#[test]
fn rewinding_a_reset_restores_the_previous_boundary_with_ordinal_gaps() {
    let mut state = history();
    let first = state.turns.values().next().unwrap().clone();
    for (ordinal, reset) in [(4, true), (7, false), (10, true), (12, false)] {
        let turn = Turn {
            id: ordinal.to_string().into(),
            ordinal,
            fresh_context: reset,
            ..first.clone()
        };
        state.turns.insert(turn.id.clone(), turn);
    }
    state.counters.turn = 12;
    state.thread.context_start = 9;
    for (turn, boundary) in [("12", 9), ("10", 3), ("4", 0)] {
        let result = command(&state, 30, Command::Rewind(turn.into()));
        let after = state.apply(&result.changes);
        assert_eq!(after.thread.context_start, boundary, "{turn}");
    }
}
