use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use agent_client::operations::{AgentClient, AgentError};
use host_protocol::{
    DEFAULT_MAX_MESSAGE_BYTES, Ed25519PublicKey, HOST_REQUEST_LIMIT, PairingToken, RelayEndpoint,
    RpcEventQueue, RpcPeer,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::transport;

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

pub struct MobileClient {
    // Keeping the relay task alive keeps the WebSocket session alive after the
    // reader/writer tasks are spawned. Dropping it closes only this mobile
    // connection; the Host-owned Codex process is unaffected.
    session: StdMutex<Option<Arc<transport::Connection>>>,
    peer: Arc<RpcPeer>,
    events: RpcEventQueue,
    request_timeout: Duration,
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
        Ok(Self::from_channel(
            Some(session),
            stream,
            config.request_timeout,
        ))
    }

    pub(super) fn from_channel<S>(
        session: Option<transport::Connection>,
        stream: S,
        request_timeout: Duration,
    ) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let events = RpcEventQueue::new(4096);
        let sink = events.clone();
        let (reader, writer) = tokio::io::split(stream);
        let peer = RpcPeer::open(
            reader,
            writer,
            DEFAULT_MAX_MESSAGE_BYTES,
            HOST_REQUEST_LIMIT,
            move |event| sink.deliver(event),
        );
        Self {
            session: StdMutex::new(session.map(Arc::new)),
            peer: Arc::new(peer),
            events,
            request_timeout,
        }
    }

    pub fn agent(&self) -> AgentClient {
        AgentClient::new(self.peer.clone(), self.request_timeout)
    }

    /// Ordered raw notifications and Host requests. A request retains its original
    /// JSON id for the response. Lagged receivers must resynchronize the session.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
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
    #[error("Host disconnected: {0}")]
    Disconnected(String),
    #[error("RPC protocol violation: {0}")]
    Protocol(String),
    #[error(transparent)]
    Agent(#[from] AgentError),
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
    use serde_json::Value;
    use std::ffi::{CStr, CString};

    fn connected_peer(runtime: &'static tokio::runtime::Runtime) -> (u64, tokio::io::DuplexStream) {
        runtime.block_on(async {
            let (client, server) = tokio::io::duplex(4096);
            let client = MobileClient::from_channel(None, client, Duration::from_secs(5));
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
