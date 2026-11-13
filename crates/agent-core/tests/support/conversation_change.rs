//! Convert native test facts into the complete rows a Host publishes.
use super::rows::{replica, replica_at};
use agent_core::state::Snapshot;
use agent_protocol::{conversation::records, models::ThreadResponse};
use std::sync::Arc;

/// Provider fixtures still speak their native changes. Their Host boundary
/// publishes complete canonical rows; core only receives that public contract.
pub fn changed(
    previous: &Snapshot,
    subscription: uuid::Uuid,
    change: agent_protocol::session::SessionChange,
) -> Option<(Snapshot, records::Message)> {
    let id = previous
        .sessions
        .subscriptions
        .iter()
        .find_map(|(id, current)| (*current == subscription).then_some(id.clone()))?;
    let current = previous.sessions.conversations[&id].clone();
    let mut snapshot = previous.clone();
    let generation = snapshot
        .sessions
        .replicas
        .get(&id)
        .map_or(1, |replica| replica.subscription());
    Arc::make_mut(&mut snapshot.sessions.replicas)
        .entry(id.clone())
        .or_insert_with(|| {
            Arc::new(replica(
                ThreadResponse {
                    thread: current.as_ref().clone(),
                    ..Default::default()
                },
                generation,
            ))
        });
    let sequence = snapshot.sessions.replicas[&id].cursor().unwrap().sequence + 1;
    let thread = change.apply(&current).expect("valid native fixture");
    let rows = replica_at(
        ThreadResponse {
            thread,
            ..Default::default()
        },
        generation,
        sequence,
    )
    .snapshot()
    .unwrap();
    Some((snapshot, rows))
}
