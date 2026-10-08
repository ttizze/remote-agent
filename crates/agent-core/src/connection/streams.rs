//! Subscription tasks: each forwards the response and later stream items to the
//! owner, then reports how the stream ended.
use super::{
    Peer,
    owner::{Event, StreamKey},
};
use crate::{peer::PeerError, protocol::Call};
use agent_domain::WorktreeSetupSnapshot;
use agent_protocol::conversation::{ShellUpdate, ThreadUpdate};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

pub(super) enum Payload {
    Shell(ShellUpdate),
    Thread(ThreadUpdate),
    Setup(Option<WorktreeSetupSnapshot>),
    TerminalMetadata(agent_protocol::operations::TerminalMetadataEvent),
    Keybindings(agent_protocol::keybindings::KeybindingsConfig),
    Awareness(agent_protocol::models::AwarenessSnapshot),
    /// The stream closed; `None` when it ended without an error.
    Ended(Option<PeerError>),
}

pub(super) struct Target {
    pub peer: Peer,
    pub epoch: u64,
    pub key: StreamKey,
    pub generation: u64,
    pub sender: mpsc::Sender<Event>,
}
impl Target {
    async fn send(&self, payload: Payload) -> bool {
        self.sender
            .send(Event::Stream {
                epoch: self.epoch,
                key: self.key.clone(),
                generation: self.generation,
                payload: Box::new(payload),
            })
            .await
            .is_ok()
    }
}

pub(super) async fn follow<T: DeserializeOwned>(
    target: Target,
    call: Call,
    wrap: fn(T) -> Payload,
) {
    let ended = match target.peer.request_stream::<T>(&call).await {
        Err(error) => Some(error),
        Ok((first, mut updates)) => {
            if !target.send(wrap(first)).await {
                return;
            }
            loop {
                match updates.read::<T>().await {
                    Ok(Some(item)) => {
                        if !target.send(wrap(item)).await {
                            return;
                        }
                    }
                    Ok(None) => break None,
                    Err(error) => break Some(PeerError::ConnectionClosed(error.to_string())),
                }
            }
        }
    };
    target.send(Payload::Ended(ended)).await;
}

/// The Host's typed refusal carried by a failed request.
pub(super) fn rpc_failure(error: &PeerError) -> Option<agent_protocol::error::RpcFailure> {
    match error {
        PeerError::Remote { error, .. } => serde_json::from_str(error).ok(),
        _ => None,
    }
}
