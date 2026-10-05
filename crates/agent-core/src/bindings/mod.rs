//! Native binding boundary. Core owns all conversation state and effects.
mod json;
mod protocol;
mod snapshot;

use crate::{
    diagnostics::{
        ConnectionPhase,
        connection::{Trace, identifier},
    },
    models::Invitation,
    state::{Intent, Snapshot},
    store::Outcome,
    transport::{Endpoint, Identity, Relays, Ticket},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

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
pub fn account_error_message(message: String) -> String {
    crate::presentation::error::error_message(&message)
}

#[uniffi::export]
pub fn apply_model_preferences(
    persisted: Vec<u8>,
    defaults: Vec<u8>,
) -> Result<Vec<u8>, AgentError> {
    crate::persistence::apply_model_preferences(&persisted, &defaults).map_err(error)
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

#[uniffi::export]
pub fn parse_invitation(contents: String, now: u64) -> Result<Invitation, AgentError> {
    let invitation: crate::models::Invitation = serde_json::from_str(&contents).map_err(|_| {
        error("接続情報を読み取れませんでした。PCで新しいQRコードを表示してください。")
    })?;
    validate_invitation(invitation.clone(), now)?;
    Ok(invitation)
}

#[uniffi::export]
pub fn validate_invitation(invitation: Invitation, now: u64) -> Result<String, AgentError> {
    let ticket = invitation
        .endpoint
        .parse::<Ticket>()
        .map_err(|_| error("接続情報が無効です。PCで新しいQRコードを表示してください。"))?;
    if now >= invitation.expires_at {
        return Err(error(
            "接続情報の有効期限が切れています。PCで新しいQRコードを表示してください。",
        ));
    }
    if invitation.host_name.trim().is_empty()
        || invitation
            .ai_recipients
            .iter()
            .any(|name| name.trim().is_empty())
        || invitation
            .transcription_recipient
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
    {
        return Err(error(
            "送信先の情報を確認できませんでした。PCで新しいQRコードを表示してください。",
        ));
    }
    Ok(ticket.node_id().to_string())
}

#[derive(uniffi::Object)]
pub struct AgentStore {
    store: crate::store::Store,
    endpoint: tokio::sync::Mutex<Option<NativeEndpoint>>,
    trace: Arc<Trace>,
}
struct NativeEndpoint {
    endpoint: Endpoint,
    use_relays: bool,
}

impl AgentStore {
    async fn connect_recording(
        &self,
        connection: Connection,
        reuse: bool,
    ) -> Result<(), AgentError> {
        let started = std::time::Instant::now();
        self.resume_connection(started, async {
            let (endpoint, ticket, invitation) = self.connection_endpoint(connection).await?;
            let endpoint_ms = started.elapsed().as_millis() as u64;
            let mut performance = if reuse && invitation.is_none() {
                self.store.resume(&endpoint, &ticket).await.map_err(error)
            } else {
                self.store
                    .reconnect(&endpoint, &ticket, invitation)
                    .await
                    .map_err(error)
            }?;
            performance.endpoint_ms = endpoint_ms;
            performance.total_ms = started.elapsed().as_millis() as u64;
            performance.client_revision = option_env!("BEX_BUILD_REVISION")
                .unwrap_or("development")
                .into();
            performance.platform = crate::diagnostics::ClientPlatform::current();
            Ok::<_, AgentError>(performance)
        })
        .await
    }

    async fn resume_connection(
        &self,
        started: std::time::Instant,
        connection: impl Future<Output = Result<crate::diagnostics::ConnectionPerformance, AgentError>>,
    ) -> Result<(), AgentError> {
        let attempt = identifier();
        self.trace.activate();
        self.trace
            .record(ConnectionPhase::ResumeStart, attempt, 0, 0);
        let cancelled = scopeguard::guard((), |_| {
            self.trace
                .record(ConnectionPhase::ResumeCancelled, attempt, 0, 0)
        });
        let result = connection.await;
        scopeguard::ScopeGuard::into_inner(cancelled);
        self.trace.record(
            if result.is_ok() {
                ConnectionPhase::ResumeReady
            } else {
                ConnectionPhase::ResumeFailed
            },
            attempt,
            0,
            started.elapsed().as_micros() as u64,
        );
        let mut performance = result?;
        performance.attempt_id = attempt;
        self.trace.record(
            ConnectionPhase::ResumeConnection,
            attempt,
            performance.connection_id,
            u64::from(performance.reused),
        );
        self.store.record_connection_performance(performance);
        self.trace.activate();
        Ok(())
    }

    async fn connection_endpoint(
        &self,
        connection: Connection,
    ) -> Result<(Endpoint, Ticket, Option<uuid::Uuid>), AgentError> {
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
        let mut cached = self.endpoint.lock().await;
        if let Some(cached) = cached.as_ref()
            && cached.endpoint.node_id() == identity.node_id()
            && cached.use_relays == connection.use_relays
        {
            return Ok((cached.endpoint.clone(), ticket, invitation));
        }
        let endpoint = Endpoint::bind_recording(
            identity,
            if connection.use_relays {
                Relays::Default
            } else {
                Relays::Disabled
            },
            self.trace.clone(),
        )
        .await
        .map_err(error)?;
        *cached = Some(NativeEndpoint {
            endpoint: endpoint.clone(),
            use_relays: connection.use_relays,
        });
        Ok((endpoint, ticket, invitation))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AgentStore {
    #[uniffi::constructor]
    pub async fn offline(
        persisted: Vec<u8>,
        diagnostics_directory: Option<String>,
    ) -> Result<Arc<Self>, AgentError> {
        let started = std::time::Instant::now();
        let trace = Trace::new();
        if let Some(directory) = diagnostics_directory {
            trace.persist(directory.into()).map_err(error)?;
        }
        trace.activate();
        let snapshot = crate::persistence::decode(&persisted).map_err(error)?;
        let store = crate::store::Store::offline(snapshot);
        trace.record(
            ConnectionPhase::StoreRestored,
            0,
            0,
            started.elapsed().as_micros() as u64,
        );
        Ok(Arc::new(Self {
            store,
            endpoint: Default::default(),
            trace,
        }))
    }

    #[uniffi::constructor]
    pub async fn connect(
        connection: Connection,
        persisted: Vec<u8>,
        diagnostics_directory: Option<String>,
    ) -> Result<Arc<Self>, AgentError> {
        let store = Self::offline(persisted, diagnostics_directory).await?;
        store.reconnect(connection).await?;
        Ok(store)
    }

    pub async fn reconnect(&self, connection: Connection) -> Result<(), AgentError> {
        self.connect_recording(connection, false).await
    }

    /// Foreground recovery reuses a responsive session and the endpoint identity.
    pub async fn resume(&self, connection: Connection) -> Result<(), AgentError> {
        self.connect_recording(connection, true).await
    }
    /// Platform lifecycle boundaries only; shared transport measurements stay in Core.
    pub fn record_connection_event(&self, phase: ConnectionPhase, value: u64) {
        if matches!(
            phase,
            ConnectionPhase::AppPreparation
                | ConnectionPhase::SnapshotRead
                | ConnectionPhase::ClientBuild
                | ConnectionPhase::IdentityRead
                | ConnectionPhase::UiConnectStart
                | ConnectionPhase::UiConnectReady
                | ConnectionPhase::UiConnectFailed
                | ConnectionPhase::UiConnectCancelled
                | ConnectionPhase::ListPublished
                | ConnectionPhase::ListViewUpdated
                | ConnectionPhase::AppScene
        ) {
            if matches!(phase, ConnectionPhase::UiConnectStart) {
                self.trace.activate();
            }
            self.trace.record(phase, 0, 0, value);
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.store.snapshot()
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
                if !Arc::ptr_eq(&current, &previous) {
                    return Ok(current.clone());
                }
            }
            updates.changed().await.map_err(error)?;
        }
    }

    pub async fn browser(
        &self,
        request: crate::browser::BrowserRequest,
    ) -> Result<crate::browser::BrowserFrame, AgentError> {
        self.store
            .browser(request)
            .await
            .map_err(|reason| match reason {
                crate::peer::PeerError::InvalidMessage(reason) => error(reason),
                reason => error(reason),
            })
    }

    /// Prepare network access without capturing or sending microphone audio.
    pub fn prepare_dictation(&self) -> Arc<crate::client::DictationPreparation> {
        Arc::new(self.store.prepare_dictation())
    }

    /// Enqueue synchronously; native task scheduling cannot reorder UI intents.
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let receipt = self.store.dispatch(intent);
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(async move {
                receipt.await.map_err(error)?.map_err(error)
            }))),
        }))
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        let result = self.store.close().await.map_err(error);
        if let Some(cached) = self.endpoint.lock().await.take() {
            cached.endpoint.close().await;
        }
        result
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
