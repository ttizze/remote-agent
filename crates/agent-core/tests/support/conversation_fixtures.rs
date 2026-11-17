//! Canonical conversation fixtures for core's colocated tests.
use crate::test_host::rows;
pub(crate) use rows::orchestration::snapshot as projection;
pub(crate) use rows::{shell, shell_thread};
#[path = "conversation_open.rs"]
mod opening;
pub(crate) use opening::opened;
#[path = "conversation_change.rs"]
mod changes;
pub(crate) use changes::changed;

pub(crate) fn open(
    snapshot: &mut agent_core::state::Snapshot,
    response: agent_protocol::models::ThreadResponse,
) {
    use agent_core::state::operations::{Operation, ReadThread};
    let id = response.thread.id.clone().unwrap();
    let output = opened(response, snapshot);
    ReadThread::open(id).apply(snapshot, "", output);
}
