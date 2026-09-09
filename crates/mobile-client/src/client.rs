use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use host_protocol::{DEFAULT_MAX_MESSAGE_BYTES, Ed25519PublicKey, PairingToken, RelayEndpoint};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::transport;
use agent_core::peer::{EventDelivery, PeerError, RpcPeer};

#[derive(Debug, Clone)]
pub struct MobileClientConfig {
    pub relay: RelayEndpoint,
    pub host_identity: Ed25519PublicKey,
    pub device_name: String,
    pub pairing_ticket: Option<PairingToken>,
    pub request_timeout: Duration,
}

impl MobileClientConfig {
    pub fn validate(&self) -> Result<(), MobileClientError> {
        self.relay
            .validate()
            .map_err(|error| MobileClientError::InvalidRelayUrl(error.to_string()))?;
        if self.device_name.is_empty()
            || self.device_name.len() > 128
            || self.device_name.chars().any(char::is_control)
        {
            return Err(MobileClientError::InvalidConfig(
                "device name must contain 1 to 128 non-control bytes",
            ));
        }
        if self.request_timeout.is_zero() {
            return Err(MobileClientError::InvalidConfig(
                "request_timeout must be positive",
            ));
        }
        Ok(())
    }
}

/// Public connection metadata; pairing tokens remain private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedHost {
    pub relay_url: String,
    pub runner_id: String,
    pub host_identity: Ed25519PublicKey,
}

pub struct MobileClient {
    // Owns the relay task for the lifetime of this connection.
    session: StdMutex<Option<Arc<transport::Connection>>>,
    peer: RpcPeer,
    host: ConnectedHost,
}

impl MobileClient {
    /// Establishes SSH through the relay, verifies the pinned Host key, then
    /// proves possession of the caller's secure-storage device key.
    pub async fn connect(
        config: MobileClientConfig,
        device_pkcs8: &[u8],
    ) -> Result<Self, MobileClientError> {
        config.validate()?;
        let key = transport::decode_device_key(device_pkcs8)?;
        let transport::AuthenticatedChannel { session, stream } =
            timeout(config.request_timeout, transport::establish(&config, key))
                .await
                .map_err(|_| MobileClientError::ConnectionTimeout)??;
        let (reader, writer) = tokio::io::split(stream);
        let peer = RpcPeer::open(
            host_protocol::JsonlReader::with_max_message_bytes(reader, DEFAULT_MAX_MESSAGE_BYTES),
            writer,
            config.request_timeout,
            1024,
            EventDelivery::SplitRequests,
        )?;
        Ok(Self {
            session: StdMutex::new(Some(Arc::new(session))),
            peer,
            host: ConnectedHost {
                relay_url: config.relay.relay_url,
                runner_id: config.relay.runner_id,
                host_identity: config.host_identity,
            },
        })
    }

    pub fn host(&self) -> &ConnectedHost {
        &self.host
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.peer.subscribe()
    }

    /// Subscribes to raw requests initiated by the Host. The request includes
    /// its original JSON `id`, which must be supplied unchanged when replying.
    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<String> {
        self.peer.subscribe_server_requests()
    }

    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, MobileClientError> {
        Ok(self.peer.request(&method.into(), &params).await?)
    }

    /// Retains the caller's raw JSON params without re-encoding them.
    pub async fn request_raw(
        &self,
        method: impl Into<String>,
        params: impl Into<String>,
    ) -> Result<Value, MobileClientError> {
        Ok(self
            .peer
            .request_params_raw(&method.into(), &params.into())
            .await?)
    }

    pub async fn respond_result(
        &self,
        id: impl Into<String>,
        result: Value,
    ) -> Result<(), MobileClientError> {
        Ok(self
            .peer
            .respond_raw(&id.into(), "result", &serde_json::to_string(&result)?)
            .await?)
    }

    pub async fn respond_error(
        &self,
        id: impl Into<String>,
        error: Value,
    ) -> Result<(), MobileClientError> {
        Ok(self
            .peer
            .respond_raw(&id.into(), "error", &serde_json::to_string(&error)?)
            .await?)
    }

    /// Retains the caller's raw JSON result/error without re-encoding it.
    pub async fn respond_raw(
        &self,
        id: impl Into<String>,
        field: &'static str,
        payload: impl Into<String>,
    ) -> Result<(), MobileClientError> {
        Ok(self
            .peer
            .respond_raw(&id.into(), field, &payload.into())
            .await?)
    }

    pub(crate) fn connection(&self) -> Result<Arc<transport::Connection>, MobileClientError> {
        self.session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| MobileClientError::Disconnected("connection is closed".into()))
    }

    pub fn close(&self) {
        self.peer.close();
        let session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(session) = session {
            session.relay.abort();
        }
    }
}

#[derive(Debug, Error)]
pub enum MobileClientError {
    #[error(transparent)]
    Transfer(#[from] agent_core::transfers::TransferError),
    #[error("encrypted connection failed: {0}")]
    Ssh(#[from] russh::Error),
    #[error("device identity is not a valid Ed25519 private key")]
    InvalidDeviceKey,
    #[error("PC rejected this device or invitation")]
    AuthenticationRejected,
    #[error("PC rejected the application channel")]
    SubsystemRejected,
    #[error("invalid mobile client configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("relay URL is invalid: {0}")]
    InvalidRelayUrl(String),
    #[error("relay WebSocket connection failed: {0}")]
    Relay(#[from] relay_transport::RelayError),
    #[error("relay stream failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("relay connection did not complete before the deadline")]
    ConnectionTimeout,
    #[error("JSONL transport failed: {0}")]
    Jsonl(#[from] host_protocol::JsonlError),
    #[error("invalid Codex JSONL message: {0}")]
    Message(#[from] host_protocol::RpcMessageError),
    #[error("Host disconnected: {0}")]
    Disconnected(String),
    #[error("RPC protocol violation: {0}")]
    Protocol(String),
    #[error("request ID space exhausted")]
    RequestIdExhausted,
    #[error("RPC request {id} timed out")]
    RequestTimeout { id: u64 },
    #[error("Host returned an RPC error: {error}")]
    Remote { error: String },
    #[error("failed to encode JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Drop for MobileClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// Forwards RPC over the pinned transport without changing JSON fields or IDs.
pub async fn forward_rpc<S>(
    config: MobileClientConfig,
    device_pkcs8: &[u8],
    mut local: S,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), MobileClientError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    config.validate()?;
    let key = transport::decode_device_key(device_pkcs8)?;
    let mut channel = tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        result = tokio::time::timeout(config.request_timeout, transport::establish(&config, key)) => result.map_err(|_| MobileClientError::ConnectionTimeout)??,
    };
    tokio::select! {
        _ = shutdown.cancelled() => {},
        result = tokio::io::copy_bidirectional(&mut local, &mut channel.stream) => { result?; },
    }
    drop(channel);
    Ok(())
}

impl From<PeerError> for MobileClientError {
    fn from(error: PeerError) -> Self {
        match error {
            PeerError::ConnectionClosed(reason) => Self::Disconnected(reason),
            PeerError::RequestTimeout { id, .. } => Self::RequestTimeout { id },
            PeerError::RequestIdExhausted => Self::RequestIdExhausted,
            PeerError::Remote { error } => Self::Remote { error },
            PeerError::InvalidMessage(reason) => Self::Protocol(reason),
        }
    }
}

impl MobileClient {
    pub async fn upload_file(
        &self,
        source: &std::path::Path,
        directory: &std::path::Path,
        file_name: &str,
    ) -> Result<Value, MobileClientError> {
        let connection = self.connection()?;
        tokio::time::timeout(
            Duration::from_secs(120),
            agent_core::transfers::upload_file(
                &self.peer,
                || async {
                    transport::open_subsystem(&connection.ssh, host_protocol::BLOB_SUBSYSTEM)
                        .await
                        .map_err(std::io::Error::other)
                },
                source,
                directory,
                file_name,
            ),
        )
        .await
        .map_err(|_| MobileClientError::Protocol("upload timed out".into()))?
        .map_err(Into::into)
    }
    pub async fn download_file(
        &self,
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), MobileClientError> {
        let connection = self.connection()?;
        tokio::time::timeout(
            Duration::from_secs(120),
            agent_core::transfers::download_file(
                &self.peer,
                || async {
                    transport::open_subsystem(&connection.ssh, host_protocol::BLOB_SUBSYSTEM)
                        .await
                        .map_err(std::io::Error::other)
                },
                source,
                destination,
            ),
        )
        .await
        .map_err(|_| MobileClientError::Protocol("download timed out".into()))?
        .map_err(Into::into)
    }
}
