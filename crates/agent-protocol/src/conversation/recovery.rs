use super::*;

pub(super) fn restart(state: &State, continue_running: bool, context: Context) -> Step {
    let mut result = Step {
        changes: (state.thread.queue_held != state.has_queued_inputs())
            .then(|| {
                Change::Thread(Thread {
                    queue_held: state.has_queued_inputs(),
                    ..state.thread.clone()
                })
            })
            .into_iter()
            .collect(),
        outcome: Outcome::Accepted,
        ..Step::default()
    };
    if context.preparation.is_some_and(|saved| {
        saved
            .setup
            .as_ref()
            .is_some_and(|setup| setup.phase == crate::workspace::SetupPhase::Cancelled)
    }) && let Some((generation, _, checkout)) = workspace::cancelled(context.preparation)
    {
        result.actions.push(Action::CancelPreparation {
            action: generation,
            discard: Some((generation, checkout)),
        });
    }
    for (provider, binding) in &state.bindings {
        if matches!(binding, Binding::Aligning { .. }) {
            result.changes.push(Change::Binding {
                provider: *provider,
                value: None,
            });
        }
    }
    for input in state.inputs.values() {
        match &input.state {
            InputState::Preparing { stage, mode, .. } => {
                let action = allocate(context.seed, &format!("prepare:{}", input.id));
                result.changes.push(Change::Input(PendingInput {
                    state: InputState::Preparing {
                        stage: *stage,
                        action,
                        mode: *mode,
                    },
                    ..input.clone()
                }));
                result.actions.push(Action::Prepare {
                    id: input.id.clone(),
                    action,
                    stage: *stage,
                    cwd: state.thread.workspace.cwd().map(str::to_owned),
                    provider: state.thread.provider,
                });
            }
            InputState::Delivering(_)
            | InputState::Unknown {
                status: UnknownState::Live | UnknownState::Resolving { .. },
                ..
            } => {
                result.changes.push(input.fenced().expect("live delivery"));
            }
            InputState::Queued { .. }
            | InputState::Unknown {
                status: UnknownState::Fenced,
                ..
            }
            | InputState::Failed(_) => {}
        }
    }
    for input in state.inputs.values() {
        if let InputState::Delivering(delivery) | InputState::Unknown { delivery, .. } =
            &input.state
            && matches!(state.bindings.get(&delivery.attempt.provider), Some(Binding::Ready { source, .. }) if source == &delivery.attempt.source)
        {
            result.changes.push(Change::Binding {
                provider: delivery.attempt.provider,
                value: None,
            });
        }
    }
    for turn in state.turns.values() {
        if context.imported_turns.contains(&turn.id) {
            continue;
        }
        match &turn.state {
            TurnState::Running { interrupt } => {
                let finished = execution::finish(state, turn, TurnState::Interrupted, context.now);
                result.changes.extend(finished.changes);
                if continue_running
                    && interrupt.is_none()
                    && !turn.compaction
                    && !state
                        .requests
                        .values()
                        .any(|request| request.turn == turn.id)
                {
                    result.continuations.push(Continuation {
                        id: allocate(context.seed, &format!("continue:{}", turn.id)),
                        source: turn.id.clone(),
                    });
                }
            }
            TurnState::Waiting(Wait::Background { .. }) => result.changes.extend(
                execution::finish(state, turn, TurnState::Interrupted, context.now).changes,
            ),
            TurnState::Waiting(Wait::Retry { at }) => {
                result
                    .actions
                    .push(Action::WaitUntil((*at).max(context.now)));
            }
            TurnState::Completed
            | TurnState::Interrupted
            | TurnState::Failed(_)
            | TurnState::RolledBack => {}
        }
    }
    // No callback from a connection owned by the previous process is current.
    for attempt in state.attempts.values() {
        result.changes.push(Change::Attempt {
            turn: attempt.turn.clone(),
            value: None,
        });
    }
    for request in state.requests.values() {
        let turn_ended = state
            .turns
            .get(&request.turn)
            .is_none_or(|turn| !matches!(turn.state, TurnState::Waiting(Wait::Retry { .. })));
        if turn_ended {
            result
                .changes
                .push(Change::RemoveRequest(request.id.clone()));
        } else {
            match request.state {
                RequestState::Sending { .. } => result.changes.push(Change::Request(Request {
                    state: RequestState::Unknown,
                    ..request.clone()
                })),
                RequestState::Awaiting => result
                    .changes
                    .push(Change::RemoveRequest(request.id.clone())),
                RequestState::Unknown => {}
            }
        }
    }
    result
}
