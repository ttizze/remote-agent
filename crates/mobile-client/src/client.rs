use std::{net::SocketAddr, time::Duration};

use host_protocol::{
    ConnectionLimits, DEFAULT_MAX_FRAME_BYTES, Ed25519PublicKey, PairingToken, RpcError, RpcId,
    RpcNotification, RpcOutcome, RpcRequest,
};
use quinn::{Connection, Endpoint};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::{rpc::RpcPeer, transport};

/// Connection parameters obtained from a trusted pairing payload. `server_name`
/// is a QUIC/TLS routing name only: the Host authentication boundary is the
/// pinned Ed25519 identity and ServerHello proof, not the rotating TLS cert.
#[derive(Debug, Clone)]
pub struct MobileClientConfig {
    pub address: SocketAddr,
    pub server_name: String,
    pub host_identity: Ed25519PublicKey,
    pub device_name: String,
    pub pairing_ticket: Option<PairingToken>,
    pub max_frame_bytes: u32,
    pub request_timeout: Duration,
}

impl MobileClientConfig {
    pub fn validate(&self) -> Result<(), MobileClientError> {
        if self.server_name.is_empty() {
            return Err(MobileClientError::InvalidConfig("server_name is empty"));
        }
        if self.device_name.is_empty() {
            return Err(MobileClientError::InvalidConfig("device_name is empty"));
        }
        if self.max_frame_bytes == 0 {
            return Err(MobileClientError::InvalidConfig(
                "max_frame_bytes must be positive",
            ));
        }
        if self.max_frame_bytes > DEFAULT_MAX_FRAME_BYTES {
            return Err(MobileClientError::InvalidConfig(
                "max_frame_bytes exceeds data-frame maximum",
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

/// A Host notification preserved exactly as it appeared on the RPC wire.
pub type Notification = RpcNotification;

/// A request initiated by the Host (for example an approval request).
///
/// This is intentionally the raw host-protocol request rather than a fixed
/// allow-list of Codex operations. Callers must answer it with
/// [`MobileClient::respond_result`] or [`MobileClient::respond_error`].
pub type ServerRequest = RpcRequest;

#[derive(Debug, Clone)]
pub struct ConnectedHost {
    pub version: u16,
    pub limits: ConnectionLimits,
    pub supported_methods: Vec<String>,
}

pub struct MobileClient {
    _endpoint: Endpoint,
    connection: Connection,
    peer: RpcPeer,
    host: ConnectedHost,
}

impl MobileClient {
    /// Establishes QUIC, verifies the pinned Host identity proof, then pairs
    /// or authenticates the supplied device identity before exposing RPC.
    pub async fn connect(
        config: MobileClientConfig,
        device_pkcs8: &[u8],
    ) -> Result<Self, MobileClientError> {
        config.validate()?;
        let device_key = Ed25519KeyPair::from_pkcs8(device_pkcs8)
            .map_err(|_| MobileClientError::InvalidDeviceKey)?;
        let device_identity = Ed25519PublicKey::from_bytes(
            device_key
                .public_key()
                .as_ref()
                .try_into()
                .expect("ring Ed25519 public keys have 32 bytes"),
        );

        let transport::AuthenticatedChannel {
            endpoint,
            connection,
            send,
            receive,
            host,
        } = transport::establish(&config, &device_key, device_identity).await?;
        let peer = RpcPeer::open(
            send,
            receive,
            host.limits.max_frame_bytes,
            host.limits.outbound_queue_messages,
            host.limits.max_in_flight_requests,
            config.request_timeout,
        )?;
        Ok(Self {
            _endpoint: endpoint,
            connection,
            peer,
            host,
        })
    }

    pub fn host(&self) -> &ConnectedHost {
        &self.host
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.peer.subscribe_notifications()
    }

    /// Subscribes to requests initiated by the Host. The request's ID may be
    /// either an integer or a string, matching the wire protocol.
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

    /// Sends a successful response to a Host-initiated request.
    pub async fn respond_result(&self, id: RpcId, result: Value) -> Result<(), MobileClientError> {
        self.peer.respond(id, RpcOutcome::Success { result }).await
    }

    /// Sends a structured error response to a Host-initiated request.
    pub async fn respond_error(&self, id: RpcId, error: RpcError) -> Result<(), MobileClientError> {
        self.peer.respond(id, RpcOutcome::Failure { error }).await
    }

    pub fn close(&self) {
        self.connection.close(0_u32.into(), b"mobile client closed");
    }
}

#[derive(Debug, Error)]
pub enum MobileClientError {
    #[error("invalid mobile client configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("device secure-storage key is not an Ed25519 PKCS#8 document")]
    InvalidDeviceKey,
    #[error("failed to create QUIC endpoint: {0}")]
    Endpoint(#[source] std::io::Error),
    #[error("failed to start QUIC connection: {0}")]
    Connect(#[source] quinn::ConnectError),
    #[error("QUIC connection failed: {0}")]
    Connection(#[source] quinn::ConnectionError),
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("secure random generation failed")]
    Random,
    #[error("RPC framing failed: {0}")]
    Frame(#[from] host_protocol::FrameError),
    #[error("invalid Host handshake: {0}")]
    InvalidHandshake(&'static str),
    #[error("pinned Host identity does not match ServerHello")]
    HostIdentityMismatch,
    #[error("ServerHello does not bind the certificate observed in the QUIC TLS handshake")]
    TransportCertificateMismatch,
    #[error("QUIC TLS handshake did not expose a transport certificate")]
    MissingTransportCertificate,
    #[error("Host ServerHello signature is invalid")]
    InvalidHostProof,
    #[error("Host rejected device pairing or authentication")]
    AuthenticationRejected,
    #[error("Host disconnected: {0}")]
    Disconnected(String),
    #[error("RPC protocol violation: {0}")]
    Protocol(String),
    #[error("request ID space exhausted")]
    RequestIdExhausted,
    #[error("RPC request {id} timed out")]
    RequestTimeout { id: u64 },
    #[error("Host rejected RPC request ({code}): {message}")]
    Remote {
        code: Value,
        message: String,
        data: Option<Value>,
        extensions: serde_json::Map<String, Value>,
    },
}

impl From<RpcError> for MobileClientError {
    fn from(error: RpcError) -> Self {
        Self::Remote {
            code: error.code,
            message: error.message,
            data: error.data,
            extensions: error.extensions,
        }
    }
}
