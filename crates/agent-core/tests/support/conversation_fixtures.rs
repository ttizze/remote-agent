//! Canonical conversation fixtures for core's colocated tests.
use crate::test_host::rows;
pub use rows::{catalog, replica};
#[path = "conversation_open.rs"]
mod opening;
pub use opening::{Opened, opened};
#[path = "conversation_change.rs"]
mod changes;
pub use changes::changed;

pub fn open(
    snapshot: &mut agent_core::state::Snapshot,
    response: agent_protocol::models::ThreadResponse,
) {
    use agent_core::state::operations::{Operation, ReadThread};
    let id = response.thread.id.clone().unwrap();
    let output = opened(response, snapshot);
    ReadThread::open(id).apply(snapshot, "", output);
}
