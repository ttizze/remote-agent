//! Execution history is recorded with the turn, independently of the current
//! provider binding. Cooperative steering does not end an execution attempt.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAttempt {
    pub id: Uuid,
    pub execution: Uuid,
    pub native_turn: Option<TurnId>,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub state: AttemptState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptState {
    Running,
    Completed,
    Interrupted,
    Failed,
    Superseded { by_steer: bool },
}

pub struct NativeTurnObservation {
    pub execution: Uuid,
    pub native: TurnId,
    pub steering: bool,
}

pub fn observed_native_turn(
    inputs: &BTreeMap<ClientInputId, PendingInput>,
    event: &Event,
) -> Option<NativeTurnObservation> {
    let (execution, native, steering) = match event {
        Event::Delivered {
            id,
            action,
            native_turn: Some(native),
        } => {
            let delivery = inputs.get(id)?.live_delivery()?;
            if delivery.action != *action {
                return None;
            }
            (delivery.attempt.id, native, delivery.steer)
        }
        Event::Provider {
            attempt,
            native_turn: Some(native),
            ..
        } => {
            let steering = inputs
                .values()
                .filter_map(PendingInput::live_delivery)
                .any(|delivery| delivery.steer && delivery.attempt.id == *attempt);
            (*attempt, native, steering)
        }
        _ => return None,
    };
    Some(NativeTurnObservation {
        execution,
        native: native.clone(),
        steering,
    })
}

pub fn current_native_turn(history: &[ExecutionAttempt], execution: Uuid, native: &TurnId) -> bool {
    !history.iter().rev().skip(1).any(|attempt| {
        attempt.execution == execution && attempt.native_turn.as_ref() == Some(native)
    })
}

fn bind_native(history: &mut Vec<ExecutionAttempt>, observed: &NativeTurnObservation, now: i64) {
    if !current_native_turn(history, observed.execution, &observed.native) {
        return;
    }
    let Some(last) = history
        .last_mut()
        .filter(|last| last.execution == observed.execution)
    else {
        return;
    };
    if last.native_turn.is_none() {
        last.native_turn = Some(observed.native.clone());
    } else if last.native_turn.as_ref() != Some(&observed.native)
        && observed.steering
        && last.state == AttemptState::Running
    {
        last.state = AttemptState::Superseded { by_steer: true };
        last.completed_at_ms = Some(now.max(last.started_at_ms));
        history.push(ExecutionAttempt {
            id: allocate(
                observed.execution,
                &format!("native-turn:{}", observed.native),
            ),
            execution: observed.execution,
            native_turn: Some(observed.native.clone()),
            started_at_ms: now,
            completed_at_ms: None,
            state: AttemptState::Running,
        });
    }
}

fn terminal(state: &TurnState, cut_off: bool) -> Option<AttemptState> {
    Some(match state {
        TurnState::Waiting(Wait::Retry { .. }) => AttemptState::Superseded { by_steer: false },
        TurnState::Interrupted if cut_off => AttemptState::Superseded { by_steer: true },
        TurnState::Interrupted => AttemptState::Interrupted,
        TurnState::Failed(_) => AttemptState::Failed,
        TurnState::Completed | TurnState::Waiting(Wait::Background { .. }) => {
            AttemptState::Completed
        }
        TurnState::RolledBack | TurnState::Running { .. } => return None,
    })
}

fn updated(
    history: &[ExecutionAttempt],
    current: Option<Uuid>,
    state: &TurnState,
    cut_off: bool,
    native: Option<&NativeTurnObservation>,
    now: i64,
) -> Vec<ExecutionAttempt> {
    let mut history = history.to_vec();
    if let Some(id) = current
        && !history.iter().any(|record| record.execution == id)
    {
        if let Some(last) = history
            .last_mut()
            .filter(|record| record.state == AttemptState::Running)
        {
            last.state = AttemptState::Superseded { by_steer: false };
            last.completed_at_ms = Some(now.max(last.started_at_ms));
        }
        history.push(ExecutionAttempt {
            id,
            execution: id,
            native_turn: None,
            started_at_ms: now,
            completed_at_ms: None,
            state: AttemptState::Running,
        });
    }
    if let Some(native) = native {
        bind_native(&mut history, native, now);
    }
    if let Some(state) = terminal(state, cut_off)
        && let Some(last) = history
            .last_mut()
            .filter(|record| record.state == AttemptState::Running)
    {
        last.state = state;
        last.completed_at_ms = Some(now.max(last.started_at_ms));
    }
    history
}

pub(super) fn record(
    turns: &BTreeMap<TurnId, Turn>,
    active: &BTreeMap<TurnId, Attempt>,
    items: &BTreeMap<ItemId, Item>,
    mut changes: Vec<Change>,
    native: Option<&NativeTurnObservation>,
    now: i64,
) -> Vec<Change> {
    let mut running = active
        .iter()
        .map(|(turn, attempt)| (turn.clone(), attempt.id))
        .collect::<BTreeMap<_, _>>();
    for change in &changes {
        if let Change::Attempt { turn, value } = change {
            match value {
                Some(attempt) => {
                    running.insert(turn.clone(), attempt.id);
                }
                None => {
                    running.remove(turn);
                }
            }
        }
    }
    let mut touched = changes
        .iter()
        .filter_map(|change| match change {
            Change::Turn(turn) => Some(turn.id.clone()),
            Change::Attempt { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(native) = native
        && let Some((id, _)) = running
            .iter()
            .find(|(_, execution)| **execution == native.execution)
    {
        touched.insert(id.clone());
    }
    let mut updates = Vec::new();
    for id in touched {
        let turn = changes
            .iter()
            .rev()
            .find_map(|change| match change {
                Change::Turn(turn) if turn.id == id => Some(turn),
                _ => None,
            })
            .or_else(|| turns.get(&id));
        let Some(turn) = turn else {
            continue;
        };
        let cut_off = turns.get(&id).is_some_and(|old|
            matches!(&old.state, TurnState::Running { interrupt: Some(value) } if value.next_input.is_some()));
        let history = updated(
            &turn.attempts,
            running.get(&id).copied(),
            &turn.state,
            cut_off,
            native.filter(|native| running.get(&id) == Some(&native.execution)),
            now,
        );
        if history != turn.attempts {
            updates.push(Change::Turn(Turn {
                attempts: history,
                ..turn.clone()
            }));
        }
    }
    for change in &updates {
        if let Change::Turn(turn) = change
            && let Some(last) = turn.attempts.last()
        {
            running.insert(turn.id.clone(), last.id);
        }
    }
    for (id, turn) in turns {
        if let Some(last) = turn.attempts.last()
            && running.get(id) == Some(&last.execution)
        {
            running.insert(id.clone(), last.id);
        }
    }
    for change in &mut changes {
        if let Change::Item(item) = change
            && item.attempt.is_none()
        {
            item.attempt = items
                .get(&item.id)
                .and_then(|item| item.attempt)
                .or_else(|| {
                    native.and_then(|native| {
                        turns
                            .get(&item.turn)?
                            .attempts
                            .iter()
                            .find(|attempt| {
                                attempt.execution == native.execution
                                    && attempt.native_turn.as_ref() == Some(&native.native)
                            })
                            .map(|attempt| attempt.id)
                    })
                })
                .or_else(|| running.get(&item.turn).copied())
                .or_else(|| {
                    turns
                        .get(&item.turn)?
                        .attempts
                        .last()
                        .map(|attempt| attempt.id)
                });
        }
    }
    changes.extend(updates);
    changes
}
