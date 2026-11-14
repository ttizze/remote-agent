//! Native test observations become canonical snapshots at the fixture Host.
use agent_core::state::Snapshot;
use agent_protocol::{models::ThreadResponse, orchestration::projection::ThreadStreamItem};
use std::sync::Arc;

pub fn changed(
    previous: &Snapshot,
    subscription: uuid::Uuid,
    change: agent_protocol::session::SessionChange,
) -> Option<(Snapshot, ThreadStreamItem)> {
    let id = previous
        .sessions
        .subscriptions
        .iter()
        .find_map(|(id, current)| (*current == subscription).then_some(id.clone()))?;
    let current = previous.sessions.conversations.get(&id)?;
    let mut snapshot = previous.clone();
    Arc::make_mut(&mut snapshot.sessions.subscription_generations)
        .entry(id.clone())
        .or_insert(1);
    let sequence = snapshot
        .sessions
        .projections
        .get(&id)
        .map_or(1, |snapshot| snapshot.snapshot_sequence + 1);
    let thread = change.apply(current).expect("valid native fixture");
    let projection = super::rows::orchestration::snapshot(
        ThreadResponse {
            thread,
            ..Default::default()
        },
        sequence,
    );
    Some((snapshot, ThreadStreamItem::Snapshot(projection)))
}
