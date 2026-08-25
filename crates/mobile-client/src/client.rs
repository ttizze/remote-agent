use std::{net::SocketAddr, sync::Mutex as StdMutex, time::Duration};

use host_protocol::{DEFAULT_MAX_MESSAGE_BYTES, Ed25519PublicKey, PairingToken, SSH_SUBSYSTEM};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::{rpc::RpcPeer, transport};

/// Connection parameters obtained from a trusted pairing payload.
#[derive(Debug, Clone)]
pub struct MobileClientConfig {
    pub address: SocketAddr,
    pub host_identity: Ed25519PublicKey,
    pub device_name: String,
    pub pairing_ticket: Option<PairingToken>,
    pub request_timeout: Duration,
}

impl MobileClientConfig {
    pub fn validate(&self) -> Result<(), MobileClientError> {
        if self.device_name.is_empty() {
            return Err(MobileClientError::InvalidConfig("device_name is empty"));
        }
        if self.request_timeout.is_zero() {
            return Err(MobileClientError::InvalidConfig(
                "request_timeout must be positive",
            ));
        }
        if transport::pairing_username(self)?.len() > 255 {
            return Err(MobileClientError::InvalidConfig(
                "device_name is too long for SSH authentication",
            ));
        }
        Ok(())
    }
}

/// A notification preserved exactly as it appeared on the Codex JSONL wire.
pub type Notification = String;

/// A request initiated by the Host, preserved as its raw JSON object.
pub type ServerRequest = String;

/// The SSH subsystem is intentionally the only connection metadata exposed.
/// Codex's initialize response is not a transport handshake and is therefore
/// not decoded into a transport DTO here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedHost {
    pub version: u16,
    pub subsystem: String,
}

pub struct MobileClient {
    // Keeping the russh handle alive keeps the SSH session alive after the
    // channel reader/writer tasks are spawned. Dropping it closes only this
    // mobile connection; the Host-owned Codex process is unaffected.
    session: StdMutex<Option<transport::SshSession>>,
    peer: RpcPeer,
    host: ConnectedHost,
}

impl MobileClient {
    /// Establishes SSH, verifies the pinned Host public key, authenticates the
    /// device key, and opens the `remote-agent-v3` subsystem carrying raw
    /// Codex JSONL.
    pub async fn connect(
        config: MobileClientConfig,
        device_pkcs8: &[u8],
    ) -> Result<Self, MobileClientError> {
        config.validate()?;
        let device_key = transport::decode_device_key(device_pkcs8)?;
        let transport::AuthenticatedChannel { session, stream } = timeout(
            config.request_timeout,
            transport::establish(&config, device_key),
        )
        .await
        .map_err(|_| MobileClientError::ConnectionTimeout)??;
        let peer = RpcPeer::open(stream, DEFAULT_MAX_MESSAGE_BYTES, config.request_timeout)?;
        Ok(Self {
            session: StdMutex::new(Some(session)),
            peer,
            host: ConnectedHost {
                version: host_protocol::CURRENT_PROTOCOL_VERSION,
                subsystem: SSH_SUBSYSTEM.to_owned(),
            },
        })
    }

    pub fn host(&self) -> &ConnectedHost {
        &self.host
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.peer.subscribe_notifications()
    }

    /// Subscribes to raw requests initiated by the Host. The request includes
    /// its original JSON `id`, which must be supplied unchanged when replying.
    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        self.peer.subscribe_server_requests()
    }

    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, MobileClientError> {
        self.peer.request(method.into(), params).await
    }

    /// Sends a request while retaining the caller's raw JSON params text.
    /// This is useful to wrappers that already have Codex JSONL and avoids a
    /// needless params deserialize/re-serialize cycle at the mobile boundary.
    pub async fn request_raw(
        &self,
        method: impl Into<String>,
        params: impl Into<String>,
    ) -> Result<Value, MobileClientError> {
        self.peer.request_raw(method.into(), params.into()).await
    }

    /// Sends a successful response to a Host-initiated request.
    pub async fn respond_result(
        &self,
        id: impl Into<String>,
        result: Value,
    ) -> Result<(), MobileClientError> {
        self.peer.respond_result(id.into(), result).await
    }

    /// Sends a response whose `error` member is already represented as JSON.
    /// The error object is not decoded into a fixed DTO.
    pub async fn respond_error(
        &self,
        id: impl Into<String>,
        error: Value,
    ) -> Result<(), MobileClientError> {
        self.peer.respond_error(id.into(), error).await
    }

    /// Sends a response while retaining the caller's raw JSON result/error.
    /// This is the preferred seam for mobile wrappers that receive Codex JSON
    /// as text and should not deserialize/re-serialize it.
    pub async fn respond_raw(
        &self,
        id: impl Into<String>,
        field: &'static str,
        payload: impl Into<String>,
    ) -> Result<(), MobileClientError> {
        self.peer
            .respond_raw(id.into(), field, payload.into())
            .await
    }

    pub fn close(&self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        drop(session);
    }
}

#[derive(Debug, Error)]
pub enum MobileClientError {
    #[error("invalid mobile client configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("device secure-storage key is not an Ed25519 PKCS#8 document")]
    InvalidDeviceKey,
    #[error("SSH connection failed: {0}")]
    Ssh(#[source] russh::Error),
    #[error("SSH connection did not complete before the deadline")]
    ConnectionTimeout,
    #[error("SSH authentication was rejected by the Host")]
    AuthenticationRejected,
    #[error("Host rejected the remote-agent SSH subsystem")]
    SubsystemRejected,
    #[error("Host did not confirm the remote-agent SSH subsystem before the deadline")]
    SubsystemTimeout,
    #[error("pinned Host SSH public key does not match")]
    HostKeyMismatch,
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

impl From<russh::Error> for MobileClientError {
    fn from(error: russh::Error) -> Self {
        Self::Ssh(error)
    }
}
