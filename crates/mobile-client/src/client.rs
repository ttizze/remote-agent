use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use host_protocol::{DEFAULT_MAX_MESSAGE_BYTES, Ed25519PublicKey, PairingToken, RelayEndpoint};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::{rpc::RpcPeer, transport};

/// Connection parameters obtained from a trusted pairing payload.
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

/// Relay connection metadata exposed to platform wrappers. The token is
/// intentionally omitted so callers cannot accidentally display or log it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedHost {
    pub relay_url: String,
    pub runner_id: String,
    pub host_identity: Ed25519PublicKey,
}

pub struct MobileClient {
    // Keeping the relay task alive keeps the WebSocket session alive after the
    // reader/writer tasks are spawned. Dropping it closes only this mobile
    // connection; the Host-owned Codex process is unaffected.
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
        let peer = RpcPeer::open(stream, DEFAULT_MAX_MESSAGE_BYTES, config.request_timeout)?;
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

    pub fn agent(&self) -> agent_client::operations::AgentClient {
        self.peer.agent()
    }

    pub fn host(&self) -> &ConnectedHost {
        &self.host
    }

    /// Ordered raw notifications and Host requests. A request retains its original
    /// JSON id for the response. Lagged receivers must resynchronize the session.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.peer.subscribe()
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
    #[error("RPC request {method} timed out")]
    RequestTimeout { method: String },
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

#[cfg(test)]
mod native_handle_tests {
    use super::*;
    use crate::ffi;
    use host_protocol::{JsonlReader, JsonlWriter};
    use std::ffi::{CStr, CString};

    fn connected_peer(runtime: &'static tokio::runtime::Runtime) -> (u64, tokio::io::DuplexStream) {
        runtime.block_on(async {
            let (client, server) = tokio::io::duplex(4096);
            let peer = RpcPeer::open(client, 4096, Duration::from_secs(5)).unwrap();
            let client = MobileClient {
                session: StdMutex::new(None),
                peer,
                host: ConnectedHost {
                    relay_url: "fixture".into(),
                    runner_id: "fixture".into(),
                    host_identity: Ed25519PublicKey::from_bytes([1; 32]),
                },
            };
            (ffi::register_client(runtime, client).unwrap(), server)
        })
    }

    #[test]
    fn closing_native_handle_rejects_new_calls_but_keeps_an_inflight_call_alive() {
        let runtime = host_protocol::rpc_runtime().unwrap();
        let (id, server) = connected_peer(runtime);
        let (started, received) = std::sync::mpsc::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let (closed, transport_closed) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let (reader, writer) = tokio::io::split(server);
            let mut reader = JsonlReader::new(reader);
            let mut writer = JsonlWriter::new(writer);
            let request: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["method"], "model/list");
            started.send(()).unwrap();
            wait.await.unwrap();
            writer
                .write_line(
                    &serde_json::json!({"id":request["id"],"result":{"data":[],"nextCursor":null}})
                        .to_string(),
                )
                .await
                .unwrap();
            closed
                .send(reader.read_line().await.unwrap().is_none())
                .unwrap();
        });
        let pending = std::thread::spawn(move || {
            let command = CString::new(r#"{"type":"models"}"#).unwrap();
            let mut error = std::ptr::null_mut();
            // SAFETY: C strings and output pointers are valid for the call;
            // each returned string is released exactly once.
            unsafe {
                let result = ffi::mobile_client_agent_command(id, command.as_ptr(), &mut error);
                assert!(error.is_null());
                assert!(!result.is_null());
                let text = CStr::from_ptr(result).to_str().unwrap().to_owned();
                ffi::mobile_client_string_free(result);
                text
            }
        });
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        ffi::mobile_client_close(id);
        ffi::mobile_client_close(id);
        let (replacement, _server) = connected_peer(runtime);
        assert_ne!(
            replacement, id,
            "retired IDs must never identify another connection"
        );
        let mut error = std::ptr::null_mut();
        // SAFETY: stale IDs are values, and the output pointer remains writable.
        unsafe {
            assert!(ffi::mobile_client_next_event(id, &mut error).is_null());
            assert!(!error.is_null());
            assert_eq!(
                CStr::from_ptr(error).to_str().unwrap(),
                "mobile client handle is closed"
            );
            ffi::mobile_client_string_free(error);
        }
        release.send(()).unwrap();
        assert_eq!(pending.join().unwrap(), "[]");
        assert!(
            transport_closed
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        );
        ffi::mobile_client_close(replacement);
    }
}
