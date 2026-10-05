//! T3 orchestration-v2: shared contracts and pure decisions/projections, with
//! transactional persistence and an independently owned provider effect worker.
#[cfg(feature = "adapter")]
pub mod adapter;
pub mod attachments;
pub mod checkpoint;
pub mod contracts;
pub mod decider;
pub mod delegation;
pub mod projector;
#[cfg(feature = "runtime")]
pub mod store;
#[cfg(feature = "runtime")]
pub mod worker;
#[cfg(feature = "adapter")]
pub use adapter::{AdapterError, ProviderAdapter};
pub mod capabilities;
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

/// Provider maintenance has its own turn and does not capture workspace changes.
pub fn native_maintenance(text: &str, has_attachments: bool) -> bool {
    !has_attachments
        && matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "/compact" | "/logout"
        )
}

mod shared;
pub use shared::Shared;
