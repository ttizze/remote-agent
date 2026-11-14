//! Inputs for applying an opened canonical conversation in client tests.
use super::rows::replica;
use agent_core::state::{
    Snapshot,
    operations::{Operation, ReadThread},
};
use agent_protocol::{conversation::records, models::ThreadResponse};
use std::sync::Arc;

pub type Opened = <ReadThread as Operation>::Output;

pub fn opened(response: ThreadResponse, snapshot: &mut Snapshot) -> Opened {
    let id = response.thread.id.clone().unwrap();
    let generation = snapshot
        .sessions
        .replicas
        .get(&id)
        .map_or(1, |replica| replica.subscription());
    let replica = replica(response, generation);
    Arc::make_mut(&mut snapshot.sessions.replicas)
        .entry(id)
        .or_insert_with(|| Arc::new(records::Replica::new(replica.scope().clone(), generation)));
    Opened {
        replica,
        subscription: uuid::Uuid::new_v4(),
    }
}
