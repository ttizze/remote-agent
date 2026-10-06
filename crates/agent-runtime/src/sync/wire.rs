//! What clients receive. Transfer transcripts are Host-only provider context:
//! clients fold the same facts without them, like T3's wire projection.
use crate::StoredFact;
use agent_domain::{FactBody, HistoricalContext, State};
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

fn carries_host_only(state: &State) -> bool {
    state.transfers.iter().any(|t| carries_history(&t.history))
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
}

fn host_only_fact(body: &FactBody) -> bool {
    match body {
        FactBody::TransferOpened { history, .. } => carries_history(history),
        _ => false,
    }
}

/// Facts as clients receive them; only `TransferOpened` changes.
pub fn client_facts(facts: &Arc<[StoredFact]>) -> Arc<[StoredFact]> {
    if !facts.iter().any(|stored| host_only_fact(&stored.fact.body)) {
        return facts.clone();
    }
    facts
        .iter()
        .map(|stored| {
            let mut stored = stored.clone();
            if let FactBody::TransferOpened { history, .. } = &mut stored.fact.body {
                *history = empty_history();
            }
            stored
        })
        .collect()
}
