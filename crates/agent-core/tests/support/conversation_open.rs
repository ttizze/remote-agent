//! Inputs for applying an opened canonical conversation in client tests.
use agent_core::state::{
    Snapshot,
    operations::{Operation, ReadThread},
};
use agent_protocol::models::ThreadResponse;
use std::sync::Arc;

pub type Opened = <ReadThread as Operation>::Output;

pub fn opened(response: ThreadResponse, snapshot: &mut Snapshot) -> Opened {
    let id = response.thread.id.clone().expect("fixture thread identity");
    let provider = response
        .thread
        .active_provider
        .or(id.provider)
        .unwrap_or(agent_protocol::session::ProviderKind::Codex);
    let capabilities = response.thread.capabilities.unwrap_or_default();
    let generation = snapshot
        .sessions
        .subscription_generations
        .get(&id)
        .copied()
        .unwrap_or(1);
    Arc::make_mut(&mut snapshot.sessions.subscription_generations).insert(id.clone(), generation);
    Opened {
        session: id,
        projection: super::rows::orchestration::snapshot(response, 1),
        generation,
        provider,
        capabilities,
        project_root: None,
        replaced: true,
        subscription: uuid::Uuid::new_v4(),
    }
}
