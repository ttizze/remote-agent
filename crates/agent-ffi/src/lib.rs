//! Native binding boundary. Core owns all conversation state and effects.
mod data;
mod intent;
mod snapshot;

use agent_core::transport::{Endpoint, Identity, Relays, Ticket};
pub use data::*;
pub use intent::*;
pub use snapshot::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("{reason}")]
    Failed { reason: String },
}
fn error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Failed {
        reason: error.to_string(),
    }
}

#[derive(uniffi::Record)]
pub struct Connection {
    pub ticket: String,
    pub identity: Vec<u8>,
    pub invitation: Option<String>,
    pub use_relays: bool,
}

#[uniffi::export]
pub fn generate_identity() -> Vec<u8> {
    Identity::generate().to_bytes().to_vec()
}

#[uniffi::export]
pub fn ticket_identity(ticket: String) -> Result<String, AgentError> {
    Ok(ticket
        .parse::<Ticket>()
        .map_err(error)?
        .node_id()
        .to_string())
}

#[derive(uniffi::Record)]
pub struct Invitation {
    pub ticket: String,
    pub token: String,
    pub expires_at: u64,
}

#[uniffi::export]
pub fn parse_invitation(contents: String, now: u64) -> Result<Invitation, AgentError> {
    let invitation: agent_core::models::Invitation =
        serde_json::from_str(&contents).map_err(error)?;
    invitation.endpoint.parse::<Ticket>().map_err(error)?;
    if now >= invitation.expires_at {
        return Err(error("invitation expired"));
    }
    Ok(Invitation {
        ticket: invitation.endpoint,
        token: invitation.invitation.to_string(),
        expires_at: invitation.expires_at,
    })
}

#[derive(uniffi::Object)]
pub struct AgentStore {
    store: agent_core::store::Store,
}

#[uniffi::export(async_runtime = "tokio")]
impl AgentStore {
    #[uniffi::constructor]
    pub async fn offline(persisted: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        let snapshot = if persisted.is_empty() {
            agent_core::state::Snapshot::default()
        } else {
            serde_json::from_slice(&persisted).map_err(error)?
        };
        Ok(Arc::new(Self {
            store: agent_core::store::Store::offline(snapshot),
        }))
    }

    #[uniffi::constructor]
    pub async fn connect(
        connection: Connection,
        persisted: Vec<u8>,
    ) -> Result<Arc<Self>, AgentError> {
        let store = Self::offline(persisted).await?;
        store.reconnect(connection).await?;
        Ok(store)
    }

    pub async fn reconnect(&self, connection: Connection) -> Result<(), AgentError> {
        let secret = Zeroizing::new(connection.identity);
        let bytes = Zeroizing::new(
            <[u8; 32]>::try_from(secret.as_slice())
                .map_err(|_| error("identity must contain 32 bytes"))?,
        );
        let identity = Identity::from_bytes(*bytes);
        let ticket = connection.ticket.parse::<Ticket>().map_err(error)?;
        let invitation = connection
            .invitation
            .map(|value| value.parse())
            .transpose()
            .map_err(error)?;
        let endpoint = Endpoint::bind(
            identity,
            if connection.use_relays {
                Relays::Default
            } else {
                Relays::Disabled
            },
        )
        .await
        .map_err(error)?;
        self.store
            .reconnect(endpoint, &ticket, invitation)
            .await
            .map_err(error)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::new(Snapshot(self.store.snapshot()))
    }

    /// Subscribe before comparing to avoid losing an update between read and wait.
    pub async fn next_snapshot(
        &self,
        previous: Arc<Snapshot>,
    ) -> Result<Arc<Snapshot>, AgentError> {
        let mut updates = self.store.subscribe();
        loop {
            {
                let current = updates.borrow_and_update();
                if !Arc::ptr_eq(&current, &previous.0) {
                    return Ok(Arc::new(Snapshot(current.clone())));
                }
            }
            updates.changed().await.map_err(error)?;
        }
    }

    /// Enqueue synchronously; native task scheduling cannot reorder UI intents.
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let receipt = self.store.dispatch(intent.try_into()?);
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(async move {
                receipt.await.map(Into::into).map_err(error)
            }))),
        }))
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.store.close().await.map_err(error)
    }
}

type Completion = Pin<Box<dyn Future<Output = Result<Outcome, AgentError>> + Send>>;
#[derive(uniffi::Object)]
pub struct Receipt {
    result: Mutex<Option<Completion>>,
}
#[uniffi::export(async_runtime = "tokio")]
impl Receipt {
    pub async fn wait(&self) -> Result<Outcome, AgentError> {
        let result = self
            .result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| error("receipt was already awaited"))?;
        result.await
    }
}
