//! T3 orchestration-v2: shared contracts and pure decisions/projections, with
//! transactional persistence and an independently owned provider effect worker.
pub mod checkpoint;
pub mod contracts;
pub mod decider;
pub mod projector;
pub mod store;
pub mod worker;
pub use contracts::*;
#[cfg(test)]
mod test_support;

pub mod context;
pub mod rollback;

/// Deterministic event identities for a committed command or guarded effect.
pub fn events(
    thread: &ThreadId,
    key: &str,
    payloads: Vec<EventPayload>,
    now: &Timestamp,
) -> Vec<DomainEvent> {
    payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| DomainEvent {
            id: EventId::new(format!("event:{key}:{i}")).expect("derived id"),
            thread_id: thread.clone(),
            occurred_at: now.clone(),
            payload,
        })
        .collect()
}
