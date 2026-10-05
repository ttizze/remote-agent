//! What clients receive. Transfer transcripts and the pinned fork command's history
//! are Host-only provider context: clients fold the same facts without them, like
//! T3's wire projection.
use crate::StoredFact;
use agent_domain::{Command, FactBody, HistoricalContext, State};
use std::sync::Arc;

fn empty_history() -> HistoricalContext {
    HistoricalContext {
        messages: vec![],
        context: String::new(),
        omitted_items: 0,
        omitted_item_ids: vec![],
    }
}
fn carries_history(history: &HistoricalContext) -> bool {
    history != &empty_history()
}

/// The child-creating command a native fork pins until the provider answers.
fn carries_fork_history(command: &Command) -> bool {
    matches!(
        command,
        Command::AcceptFork { history, messages, context, .. }
            if !history.is_empty() || !messages.is_empty() || carries_history(context)
    )
}
fn strip_fork_history(command: &mut Command) {
    if let Command::AcceptFork {
        history,
        messages,
        context,
        ..
    } = command
    {
        history.clear();
        messages.clear();
        *context = empty_history();
    }
}

fn carries_host_only(state: &State) -> bool {
    state.transfers.iter().any(|t| carries_history(&t.history))
        || state
            .pending_forks
            .values()
            .any(|fork| carries_fork_history(&fork.child_command))
}

/// The projection a client folds: Host-only context removed, everything else intact.
pub fn client_state(state: &Arc<State>) -> Arc<State> {
    if !carries_host_only(state) {
        return state.clone();
    }
    let mut projected = State::clone(state);
    strip_host_only(&mut projected);
    Arc::new(projected)
}

pub(crate) fn strip_host_only(state: &mut State) {
    for transfer in &mut state.transfers {
        transfer.history = empty_history();
    }
    for fork in state.pending_forks.values_mut() {
        strip_fork_history(&mut fork.child_command);
    }
}

fn host_only_fact(body: &FactBody) -> bool {
    match body {
        FactBody::TransferOpened { history, .. } => carries_history(history),
        FactBody::ForkPrepared { child_command, .. } => carries_fork_history(child_command),
        _ => false,
    }
}

/// Facts as clients receive them; only `TransferOpened` and `ForkPrepared` change.
pub fn client_facts(facts: &Arc<[StoredFact]>) -> Arc<[StoredFact]> {
    if !facts.iter().any(|stored| host_only_fact(&stored.fact.body)) {
        return facts.clone();
    }
    facts
        .iter()
        .map(|stored| {
            let mut stored = stored.clone();
            match &mut stored.fact.body {
                FactBody::TransferOpened { history, .. } => *history = empty_history(),
                FactBody::ForkPrepared { child_command, .. } => strip_fork_history(child_command),
                _ => {}
            }
            stored
        })
        .collect()
}
