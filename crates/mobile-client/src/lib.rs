//! QUIC client core for the Bex Mobile Client.
//!
//! The caller owns the device PKCS#8 document (normally in platform secure
//! storage).  It is supplied only while connecting, used to sign the pairing
//! or authentication proof, and is never retained or logged by this crate.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use host_protocol::{
    AuthenticationProof, ClientHello, ConnectionLimits, ConnectionNonce, DEFAULT_MAX_FRAME_BYTES,
    DeviceAuthenticationReply, DeviceAuthenticationStart, Ed25519PublicKey, Ed25519Signature,
    PairingRequest, PairingToken, ProtocolRange, RpcError, RpcId, RpcMessage, RpcNotification,
    RpcOutcome, RpcRequest, RpcResponse, ServerHello, TransportCertificateHash,
    authentication_proof_message, pairing_proof_message, read_frame, server_hello_proof_message,
    write_frame,
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use rustls::{
    ClientConfig as TlsClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::{
    sync::{Mutex, Semaphore, broadcast, mpsc, oneshot},
    time::timeout,
};
use zeroize::Zeroizing;

const HANDSHAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;

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

        let bind = match config.address.ip() {
            IpAddr::V4(_) => "0.0.0.0:0".parse().expect("valid IPv4 socket address"),
            IpAddr::V6(_) => "[::]:0".parse().expect("valid IPv6 socket address"),
        };
        let mut endpoint = Endpoint::client(bind).map_err(MobileClientError::Endpoint)?;
        let (tls, observed_certificate) = tls_client_config()?;
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(tls)));
        let connection = endpoint
            .connect(config.address, &config.server_name)
            .map_err(MobileClientError::Connect)?
            .await
            .map_err(MobileClientError::Connection)?;

        let nonce = random_nonce()?;
        let hello = ClientHello {
            versions: ProtocolRange::CURRENT,
            max_frame_bytes: config.max_frame_bytes,
            nonce,
        };
        let (mut send, mut receive) = connection
            .open_bi()
            .await
            .map_err(MobileClientError::Connection)?;
        write_frame(&mut send, &hello, HANDSHAKE_MAX_FRAME_BYTES).await?;
        let server_hello: ServerHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES).await?;
        verify_server_hello(
            &hello,
            &server_hello,
            config.host_identity,
            observed_certificate.get()?,
        )?;

        authenticate(
            &mut send,
            &mut receive,
            server_hello.limits.max_frame_bytes,
            config.host_identity,
            &device_key,
            device_identity,
            &config,
        )
        .await?;
        let host = ConnectedHost {
            version: server_hello.version,
            limits: server_hello.limits.clone(),
            supported_methods: server_hello.supported_methods,
        };
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
        self.peer.notifications.subscribe()
    }

    /// Subscribes to requests initiated by the Host. The request's ID may be
    /// either an integer or a string, matching the wire protocol.
    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        self.peer.server_requests.subscribe()
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

type PendingResponse = oneshot::Sender<Result<Value, MobileClientError>>;
type PendingRequests = Arc<Mutex<HashMap<RpcId, PendingResponse>>>;

struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<RpcMessage>,
    pending: PendingRequests,
    notifications: broadcast::Sender<Notification>,
    server_requests: broadcast::Sender<ServerRequest>,
    permits: Arc<Semaphore>,
    request_timeout: Duration,
}

impl RpcPeer {
    fn open(
        send: SendStream,
        receive: RecvStream,
        max_frame_bytes: u32,
        queue_messages: u32,
        max_in_flight: u32,
        request_timeout: Duration,
    ) -> Result<Self, MobileClientError> {
        let queue_messages = usize::try_from(queue_messages)
            .map_err(|_| MobileClientError::InvalidHandshake("outbound queue is too large"))?;
        let max_in_flight = usize::try_from(max_in_flight)
            .map_err(|_| MobileClientError::InvalidHandshake("in-flight limit is too large"))?;
        if queue_messages == 0 || max_in_flight == 0 {
            return Err(MobileClientError::InvalidHandshake(
                "Host advertised a zero queue or concurrency limit",
            ));
        }
        let (outbound, outbound_rx) = mpsc::channel(queue_messages);
        let (notifications, _) = broadcast::channel(queue_messages);
        let (server_requests, _) = broadcast::channel(queue_messages);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(write_loop(
            send,
            outbound_rx,
            pending.clone(),
            max_frame_bytes,
        ));
        tokio::spawn(read_loop(
            receive,
            pending.clone(),
            notifications.clone(),
            server_requests.clone(),
            max_frame_bytes,
        ));
        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            pending,
            notifications,
            server_requests,
            permits: Arc::new(Semaphore::new(max_in_flight)),
            request_timeout,
        })
    }

    async fn request(&self, method: String, params: Value) -> Result<Value, MobileClientError> {
        let numeric_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if numeric_id == u64::MAX {
            return Err(MobileClientError::RequestIdExhausted);
        }
        let id = RpcId::Integer(numeric_id);
        let pending = self.pending.clone();
        let outbound = self.outbound.clone();
        let permits = self.permits.clone();
        let deadline = self.request_timeout;
        let operation = async move {
            let _permit = permits.acquire_owned().await.map_err(|_| {
                MobileClientError::Disconnected("request limiter closed".to_owned())
            })?;
            let (tx, rx) = oneshot::channel();
            pending.lock().await.insert(id.clone(), tx);
            let message = RpcMessage::Request(RpcRequest {
                id: id.clone(),
                method,
                params,
                extensions: Default::default(),
            });
            let result = match outbound.send(message).await {
                Ok(()) => match rx.await {
                    Ok(response) => response,
                    Err(_) => Err(MobileClientError::Disconnected(
                        "RPC reader stopped".to_owned(),
                    )),
                },
                Err(_) => Err(MobileClientError::Disconnected(
                    "RPC writer stopped".to_owned(),
                )),
            };
            pending.lock().await.remove(&id);
            result
        };
        timeout(deadline, operation)
            .await
            .map_err(|_| MobileClientError::RequestTimeout { id: numeric_id })?
    }

    async fn respond(&self, id: RpcId, outcome: RpcOutcome) -> Result<(), MobileClientError> {
        self.outbound
            .send(RpcMessage::Response(RpcResponse {
                id,
                outcome,
                extensions: Default::default(),
            }))
            .await
            .map_err(|_| MobileClientError::Disconnected("RPC writer stopped".to_owned()))
    }
}

async fn write_loop(
    mut send: SendStream,
    mut outbound: mpsc::Receiver<RpcMessage>,
    pending: PendingRequests,
    max_frame_bytes: u32,
) {
    while let Some(message) = outbound.recv().await {
        if let Err(error) = write_frame(&mut send, &message, max_frame_bytes).await {
            fail_pending(&pending, error.to_string()).await;
            return;
        }
    }
}

async fn read_loop(
    mut receive: RecvStream,
    pending: PendingRequests,
    notifications: broadcast::Sender<Notification>,
    server_requests: broadcast::Sender<ServerRequest>,
    max_frame_bytes: u32,
) {
    loop {
        let message: RpcMessage = match read_frame(&mut receive, max_frame_bytes).await {
            Ok(message) => message,
            Err(error) => {
                fail_pending(&pending, error.to_string()).await;
                return;
            }
        };
        match message {
            RpcMessage::Response(RpcResponse { id, outcome, .. }) => {
                let Some(tx) = pending.lock().await.remove(&id) else {
                    continue; // Late or duplicate response after timeout.
                };
                let result = match outcome {
                    RpcOutcome::Success { result } => Ok(result),
                    RpcOutcome::Failure { error } => Err(error.into()),
                };
                let _ = tx.send(result);
            }
            RpcMessage::Notification(notification) => {
                let _ = notifications.send(notification);
            }
            RpcMessage::Request(request) => {
                // Host-initiated requests are part of the normal
                // bidirectional protocol. Dropping a request when there is
                // no subscriber must not tear down unrelated in-flight work.
                let _ = server_requests.send(request);
            }
        }
    }
}

async fn fail_pending(pending: &PendingRequests, reason: String) {
    for (_, tx) in pending.lock().await.drain() {
        let _ = tx.send(Err(MobileClientError::Disconnected(reason.clone())));
    }
}

async fn authenticate(
    send: &mut SendStream,
    receive: &mut RecvStream,
    max_frame_bytes: u32,
    host_identity: Ed25519PublicKey,
    device_key: &Ed25519KeyPair,
    device_identity: Ed25519PublicKey,
    config: &MobileClientConfig,
) -> Result<(), MobileClientError> {
    if let Some(ticket) = config.pairing_ticket {
        let signature = sign(
            device_key,
            &pairing_proof_message(host_identity, ticket, device_identity),
        );
        let start = DeviceAuthenticationStart::Pair {
            request: PairingRequest {
                ticket,
                device_identity,
                device_name: config.device_name.clone(),
                signature,
            },
        };
        write_frame(send, &start, max_frame_bytes).await?;
        match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Accepted {
                device_identity: accepted,
            } if accepted == device_identity => Ok(()),
            _ => Err(MobileClientError::AuthenticationRejected),
        }
    } else {
        write_frame(
            send,
            &DeviceAuthenticationStart::Authenticate { device_identity },
            max_frame_bytes,
        )
        .await?;
        let challenge = match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Challenge { challenge }
                if challenge.host_identity == host_identity =>
            {
                challenge
            }
            _ => return Err(MobileClientError::AuthenticationRejected),
        };
        let proof = AuthenticationProof {
            token: challenge.token,
            signature: sign(
                device_key,
                &authentication_proof_message(host_identity, challenge.token, device_identity),
            ),
        };
        write_frame(send, &proof, max_frame_bytes).await?;
        match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Accepted {
                device_identity: accepted,
            } if accepted == device_identity => Ok(()),
            _ => Err(MobileClientError::AuthenticationRejected),
        }
    }
}

fn verify_server_hello(
    hello: &ClientHello,
    server: &ServerHello,
    expected_identity: Ed25519PublicKey,
    observed_certificate_hash: TransportCertificateHash,
) -> Result<(), MobileClientError> {
    validate_limits(&server.limits)?;
    if server.version < hello.versions.min || server.version > hello.versions.max {
        return Err(MobileClientError::InvalidHandshake(
            "Host selected an unsupported protocol version",
        ));
    }
    if server.limits.max_frame_bytes > hello.max_frame_bytes {
        return Err(MobileClientError::InvalidHandshake(
            "Host exceeded the offered maximum frame size",
        ));
    }
    if server.host_identity != expected_identity {
        return Err(MobileClientError::HostIdentityMismatch);
    }
    if server.transport_certificate_hash != observed_certificate_hash {
        return Err(MobileClientError::TransportCertificateMismatch);
    }
    let message = server_hello_proof_message(
        hello.nonce,
        server.version,
        server.limits.max_frame_bytes,
        server.host_identity,
        server.transport_certificate_hash,
    );
    UnparsedPublicKey::new(&ED25519, server.host_identity.as_bytes())
        .verify(&message, server.host_signature.as_bytes())
        .map_err(|_| MobileClientError::InvalidHostProof)
}

fn validate_limits(limits: &ConnectionLimits) -> Result<(), MobileClientError> {
    if limits.max_frame_bytes == 0
        || limits.max_in_flight_requests == 0
        || limits.outbound_queue_messages == 0
        || limits.request_timeout_ms == 0
    {
        return Err(MobileClientError::InvalidHandshake(
            "Host advertised a zero limit",
        ));
    }
    Ok(())
}

fn tls_client_config()
-> Result<(quinn::crypto::rustls::QuicClientConfig, ObservedCertificate), MobileClientError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let observed_certificate = ObservedCertificate::default();
    let verifier = ProofOnlyServerCertVerifier {
        algorithms: provider.signature_verification_algorithms,
        observed_certificate: observed_certificate.clone(),
    };
    let tls = TlsClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| MobileClientError::Tls(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|error| MobileClientError::Tls(error.to_string()))?;
    Ok((quic, observed_certificate))
}

/// A rotating transport certificate is accepted only to form encrypted QUIC.
/// Its TLS handshake signature is still verified. Before a device proof or any
/// RPC is sent, the ServerHello signature is verified with the pinned Ed25519
/// Host identity from pairing; that application proof authenticates the Host.
#[derive(Debug)]
struct ProofOnlyServerCertVerifier {
    algorithms: WebPkiSupportedAlgorithms,
    observed_certificate: ObservedCertificate,
}

impl ServerCertVerifier for ProofOnlyServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        self.observed_certificate.record(end_entity);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[derive(Debug, Clone, Default)]
struct ObservedCertificate(Arc<StdMutex<Option<TransportCertificateHash>>>);

impl ObservedCertificate {
    fn record(&self, certificate: &CertificateDer<'_>) {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate.as_ref());
        let hash = TransportCertificateHash::from_bytes(
            digest
                .as_ref()
                .try_into()
                .expect("SHA-256 digests have 32 bytes"),
        );
        *self.0.lock().expect("certificate observer mutex poisoned") = Some(hash);
    }

    fn get(&self) -> Result<TransportCertificateHash, MobileClientError> {
        self.0
            .lock()
            .map_err(|_| MobileClientError::Tls("certificate observer mutex poisoned".to_owned()))?
            .ok_or(MobileClientError::MissingTransportCertificate)
    }
}

fn random_nonce() -> Result<ConnectionNonce, MobileClientError> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| MobileClientError::Random)?;
    Ok(ConnectionNonce::from_bytes(bytes))
}

fn sign(key: &Ed25519KeyPair, message: &[u8]) -> Ed25519Signature {
    Ed25519Signature::from_bytes(
        key.sign(message)
            .as_ref()
            .try_into()
            .expect("ring Ed25519 signatures have 64 bytes"),
    )
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CConfig {
    address: SocketAddr,
    server_name: String,
    host_identity: Ed25519PublicKey,
    device_name: String,
    #[serde(default)]
    pairing_ticket: Option<PairingToken>,
    max_frame_bytes: u32,
    request_timeout_ms: u64,
}

impl TryFrom<CConfig> for MobileClientConfig {
    type Error = MobileClientError;

    fn try_from(value: CConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            address: value.address,
            server_name: value.server_name,
            host_identity: value.host_identity,
            device_name: value.device_name,
            pairing_ticket: value.pairing_ticket,
            max_frame_bytes: value.max_frame_bytes,
            request_timeout: Duration::from_millis(value.request_timeout_ms),
        })
    }
}

/// Minimal C ABI for Android JNI/Swift wrappers. The command/event model above
/// remains the primary API; this only owns a Tokio runtime and never persists
/// or logs the caller-supplied PKCS#8 key.
pub mod ffi {
    use std::{
        ffi::{CStr, CString, c_char},
        panic::AssertUnwindSafe,
        ptr,
        sync::Mutex,
    };

    use super::*;

    /// Generates an Ed25519 PKCS#8 document and returns it as base64url without
    /// padding. Platform code must decode and store the bytes in Keychain or
    /// Keystore, then pass the recovered bytes to `mobile_client_connect`.
    /// Generates a PKCS#8 device key for caller-managed secure storage.
    ///
    /// # Safety
    /// If non-null, `error_out` must be writable for one `char *`; returned
    /// strings must be released with `mobile_client_string_free`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_generate_device_key(
        error_out: *mut *mut c_char,
    ) -> *mut c_char {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        let result = std::panic::catch_unwind(|| {
            use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
            let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| "secure random generation failed")?;
            CString::new(URL_SAFE_NO_PAD.encode(key.as_ref()))
                .map_err(|_| "failed to encode device key")
        });
        match result {
            Ok(Ok(value)) => value.into_raw(),
            Ok(Err(error)) => {
                set_error(error_out, error);
                ptr::null_mut()
            }
            Err(_) => {
                set_error(error_out, "mobile client panicked");
                ptr::null_mut()
            }
        }
    }

    pub struct Handle {
        // A multi-thread Tokio runtime supports concurrent `block_on` calls.
        // Requests must not exclude polling/responding: an outbound Codex
        // request can pause until the mobile answers a server request.
        runtime: tokio::runtime::Runtime,
        client: MobileClient,
        notifications: Mutex<broadcast::Receiver<Notification>>,
        server_requests: Mutex<broadcast::Receiver<ServerRequest>>,
    }

    fn set_error(out: *mut *mut c_char, error: impl ToString) {
        if !out.is_null() {
            let text =
                CString::new(error.to_string()).unwrap_or_else(|_| CString::new("error").unwrap());
            // SAFETY: caller supplies a valid writable error-output pointer.
            unsafe { *out = text.into_raw() };
        }
    }

    fn input_string<'a>(input: *const c_char) -> Result<&'a str, String> {
        if input.is_null() {
            return Err("null string input".to_owned());
        }
        // SAFETY: C ABI requires a NUL-terminated string valid for this call.
        unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|_| "input is not UTF-8".to_owned())
    }

    pub(crate) fn connect_handle(config: CConfig, key: &[u8]) -> Result<*mut Handle, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| "failed to create Tokio runtime")?;
        let client = runtime
            .block_on(MobileClient::connect(
                config
                    .try_into()
                    .map_err(|error: MobileClientError| error.to_string())?,
                key,
            ))
            .map_err(|error| error.to_string())?;
        let notifications = client.subscribe();
        let server_requests = client.subscribe_server_requests();
        Ok(Box::into_raw(Box::new(Handle {
            runtime,
            client,
            notifications: Mutex::new(notifications),
            server_requests: Mutex::new(server_requests),
        })))
    }

    pub(crate) fn request_json(
        handle: &Handle,
        method: String,
        params: Value,
    ) -> Result<String, String> {
        let result = handle
            .runtime
            .block_on(handle.client.request(method, params))
            .map_err(encode_mobile_error)?;
        serde_json::to_string(&result).map_err(|_| "failed to encode response".to_owned())
    }

    fn encode_mobile_error(error: MobileClientError) -> String {
        match error {
            MobileClientError::Remote {
                code,
                message,
                data,
                extensions,
            } => serde_json::to_string(&RpcError {
                code,
                message,
                data,
                extensions,
            })
            .unwrap_or_else(|serialization| serialization.to_string()),
            other => other.to_string(),
        }
    }

    pub(crate) fn respond_result_json(
        handle: &Handle,
        request_id_json: &str,
        result_json: &str,
    ) -> Result<(), String> {
        let request_id = serde_json::from_str::<RpcId>(request_id_json)
            .map_err(|_| "invalid request ID JSON".to_owned())?;
        let result = serde_json::from_str::<Value>(result_json)
            .map_err(|_| "invalid result JSON".to_owned())?;
        handle
            .runtime
            .block_on(handle.client.respond_result(request_id, result))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn respond_error_json(
        handle: &Handle,
        request_id_json: &str,
        error_json: &str,
    ) -> Result<(), String> {
        let request_id = serde_json::from_str::<RpcId>(request_id_json)
            .map_err(|_| "invalid request ID JSON".to_owned())?;
        let error = serde_json::from_str::<RpcError>(error_json)
            .map_err(|_| "invalid RPC error JSON".to_owned())?;
        handle
            .runtime
            .block_on(handle.client.respond_error(request_id, error))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn next_notification_json(handle: &Handle) -> Result<Option<String>, String> {
        let mut notifications = handle
            .notifications
            .lock()
            .map_err(|_| "notification lock poisoned")?;
        match notifications.try_recv() {
            Ok(notification) => serde_json::to_string(&notification)
                .map(Some)
                .map_err(|_| "failed to encode notification".to_owned()),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn next_server_request_json(handle: &Handle) -> Result<Option<String>, String> {
        let mut requests = handle
            .server_requests
            .lock()
            .map_err(|_| "server-request lock poisoned")?;
        match requests.try_recv() {
            Ok(request) => serde_json::to_string(&request)
                .map(Some)
                .map_err(|_| "failed to encode server request".to_owned()),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn close_handle(handle: *mut Handle) {
        if !handle.is_null() {
            // SAFETY: caller transfers ownership exactly once to close.
            let handle = unsafe { Box::from_raw(handle) };
            handle.client.close();
        }
    }

    /// Connects and returns an opaque mobile-client handle.
    ///
    /// # Safety
    /// `config_json` must be a valid NUL-terminated UTF-8 string and
    /// `device_pkcs8` must designate `device_pkcs8_len` readable bytes. If
    /// non-null, `error_out` must be writable for one `char *`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_connect(
        config_json: *const c_char,
        device_pkcs8: *const u8,
        device_pkcs8_len: usize,
        error_out: *mut *mut c_char,
    ) -> *mut Handle {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        let result = std::panic::catch_unwind(|| -> Result<*mut Handle, String> {
            let config: CConfig = serde_json::from_str(input_string(config_json)?)
                .map_err(|_| "invalid config JSON".to_owned())?;
            if device_pkcs8.is_null() || device_pkcs8_len == 0 {
                return Err("missing device PKCS#8 bytes".to_owned());
            }
            // SAFETY: caller promises a readable byte range for this call.
            let key = unsafe { std::slice::from_raw_parts(device_pkcs8, device_pkcs8_len) };
            let key = Zeroizing::new(key.to_vec());
            connect_handle(config, &key)
        });
        match result {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                set_error(error_out, error);
                ptr::null_mut()
            }
            Err(_) => {
                set_error(error_out, "mobile client panicked");
                ptr::null_mut()
            }
        }
    }

    /// Sends one request through a live opaque handle.
    ///
    /// # Safety
    /// `handle` must be live and exclusively retained by the caller; all
    /// string inputs must be valid NUL-terminated UTF-8. If non-null,
    /// `error_out` must be writable for one `char *`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_request(
        handle: *mut Handle,
        method: *const c_char,
        params_json: *const c_char,
        error_out: *mut *mut c_char,
    ) -> *mut c_char {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        if handle.is_null() {
            set_error(error_out, "null mobile client handle");
            return ptr::null_mut();
        }
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<CString, String> {
            let method = input_string(method)?.to_owned();
            let params = serde_json::from_str(input_string(params_json)?)
                .map_err(|_| "invalid params JSON".to_owned())?;
            // SAFETY: checked non-null and the handle remains owned by caller.
            let handle = unsafe { &*handle };
            CString::new(request_json(handle, method, params)?)
                .map_err(|_| "response contains NUL".to_owned())
        }));
        match result {
            Ok(Ok(value)) => value.into_raw(),
            Ok(Err(error)) => {
                set_error(error_out, error);
                ptr::null_mut()
            }
            Err(_) => {
                set_error(error_out, "mobile client panicked");
                ptr::null_mut()
            }
        }
    }

    /// Returns one queued notification, if available.
    ///
    /// # Safety
    /// `handle` must be live. If non-null, `error_out` must be writable for
    /// one `char *`; non-null returned strings use `mobile_client_string_free`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_next_notification(
        handle: *mut Handle,
        error_out: *mut *mut c_char,
    ) -> *mut c_char {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        if handle.is_null() {
            set_error(error_out, "null mobile client handle");
            return ptr::null_mut();
        }
        // SAFETY: checked non-null and the handle remains owned by caller.
        let handle = unsafe { &*handle };
        match next_notification_json(handle) {
            Ok(Some(notification)) => CString::new(notification).unwrap().into_raw(),
            Ok(None) => ptr::null_mut(),
            Err(error) => {
                set_error(error_out, error);
                ptr::null_mut()
            }
        }
    }

    /// Returns one queued Host-initiated request, if available. The returned
    /// JSON contains the raw request, including its numeric or string `id`.
    /// Call `mobile_client_respond_result` or `mobile_client_respond_error`
    /// with that ID to complete it.
    ///
    /// # Safety
    /// `handle` must remain live for the call. If non-null, `error_out` must
    /// be writable for one `char *`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_next_server_request(
        handle: *mut Handle,
        error_out: *mut *mut c_char,
    ) -> *mut c_char {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        if handle.is_null() {
            set_error(error_out, "null mobile client handle");
            return ptr::null_mut();
        }
        // SAFETY: checked non-null and the handle remains owned by caller.
        let handle = unsafe { &*handle };
        match next_server_request_json(handle) {
            Ok(Some(request)) => CString::new(request).unwrap().into_raw(),
            Ok(None) => ptr::null_mut(),
            Err(error) => {
                set_error(error_out, error);
                ptr::null_mut()
            }
        }
    }

    /// Responds successfully to a Host-initiated request. `request_id_json`
    /// must be a JSON number or string, and `result_json` may be any JSON
    /// value. Returns 1 on success and 0 on failure.
    ///
    /// # Safety
    /// `handle` must remain live and every string pointer must designate a
    /// NUL-terminated UTF-8 string. If non-null, `error_out` must be writable.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_respond_result(
        handle: *mut Handle,
        request_id_json: *const c_char,
        result_json: *const c_char,
        error_out: *mut *mut c_char,
    ) -> i32 {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        if handle.is_null() {
            set_error(error_out, "null mobile client handle");
            return 0;
        }
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
            let request_id = input_string(request_id_json)?;
            let result = input_string(result_json)?;
            // SAFETY: checked non-null and the handle remains owned by caller.
            let handle = unsafe { &*handle };
            respond_result_json(handle, request_id, result)
        }));
        match result {
            Ok(Ok(())) => 1,
            Ok(Err(error)) => {
                set_error(error_out, error);
                0
            }
            Err(_) => {
                set_error(error_out, "mobile client panicked");
                0
            }
        }
    }

    /// Responds with a structured RPC error to a Host-initiated request.
    /// `error_json` must contain the raw error object, including `code`,
    /// `message`, and optional `data`. Returns 1 on success and 0 on failure.
    ///
    /// # Safety
    /// `handle` must remain live and every string pointer must designate a
    /// NUL-terminated UTF-8 string. If non-null, `error_out` must be writable.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_respond_error(
        handle: *mut Handle,
        request_id_json: *const c_char,
        error_json: *const c_char,
        error_out: *mut *mut c_char,
    ) -> i32 {
        if !error_out.is_null() {
            // SAFETY: checked non-null and owned by the caller.
            unsafe { *error_out = ptr::null_mut() };
        }
        if handle.is_null() {
            set_error(error_out, "null mobile client handle");
            return 0;
        }
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
            let request_id = input_string(request_id_json)?;
            let error = input_string(error_json)?;
            // SAFETY: checked non-null and the handle remains owned by caller.
            let handle = unsafe { &*handle };
            respond_error_json(handle, request_id, error)
        }));
        match result {
            Ok(Ok(())) => 1,
            Ok(Err(error)) => {
                set_error(error_out, error);
                0
            }
            Err(_) => {
                set_error(error_out, "mobile client panicked");
                0
            }
        }
    }

    /// Closes and destroys an opaque handle.
    ///
    /// # Safety
    /// `handle` must originate from `mobile_client_connect` and be passed
    /// exactly once, after all concurrent calls that borrow it have returned.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_close(handle: *mut Handle) {
        close_handle(handle);
    }

    /// Releases a string returned by this C ABI.
    ///
    /// # Safety
    /// `value` must be null or an unmodified pointer returned by this crate's
    /// string-returning C ABI functions, and must be freed exactly once.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mobile_client_string_free(value: *mut c_char) {
        if !value.is_null() {
            // SAFETY: returned strings are allocated with CString::into_raw.
            drop(unsafe { CString::from_raw(value) });
        }
    }
}

/// Android's Kotlin/JVM layer calls these exports directly. iOS uses the C ABI
/// above from its native wrapper; `jni` is never linked for non-Android targets.
#[cfg(target_os = "android")]
mod android_jni {
    use std::ptr;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jni::{
        JNIEnv,
        objects::{JClass, JString},
        sys::{JNI_FALSE, JNI_TRUE, jboolean, jlong, jstring},
    };
    use zeroize::Zeroizing;

    use super::*;

    fn exception(env: &mut JNIEnv<'_>, error: impl AsRef<str>) {
        let _ = env.throw_new("java/lang/RuntimeException", error.as_ref());
    }

    fn java_string(env: &mut JNIEnv<'_>, value: impl AsRef<str>) -> jstring {
        match env.new_string(value.as_ref()) {
            Ok(value) => value.into_raw(),
            Err(error) => {
                exception(env, error.to_string());
                ptr::null_mut()
            }
        }
    }

    fn string(env: &mut JNIEnv<'_>, value: JString<'_>) -> Result<String, String> {
        env.get_string(&value)
            .map_err(|error| error.to_string())?
            .to_str()
            .map(str::to_owned)
            .map_err(|error| error.to_string())
    }

    fn borrowed_handle(handle: jlong) -> Result<&'static ffi::Handle, String> {
        if handle == 0 {
            return Err("null mobile client handle".to_owned());
        }
        // SAFETY: Kotlin receives the pointer solely from connect and must not
        // use it after close; methods borrow it only for this native call.
        Ok(unsafe { &*(handle as *mut ffi::Handle) })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_generateDeviceKey(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
    ) -> jstring {
        match Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()) {
            Ok(key) => java_string(&mut env, URL_SAFE_NO_PAD.encode(key.as_ref())),
            Err(_) => {
                exception(&mut env, "secure random generation failed");
                ptr::null_mut()
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_connect(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        config_json: JString<'_>,
        key_base64: JString<'_>,
    ) -> jlong {
        let result = (|| {
            let config: CConfig = serde_json::from_str(&string(&mut env, config_json)?)
                .map_err(|_| "invalid config JSON".to_owned())?;
            let key = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(string(&mut env, key_base64)?)
                    .map_err(|_| "invalid device key base64".to_owned())?,
            );
            ffi::connect_handle(config, &key).map(|handle| handle as jlong)
        })();
        match result {
            Ok(handle) => handle,
            Err(error) => {
                exception(&mut env, error);
                0
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_request(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
        method: JString<'_>,
        params_json: JString<'_>,
    ) -> jstring {
        let result = (|| {
            let params = serde_json::from_str(&string(&mut env, params_json)?)
                .map_err(|_| "invalid params JSON".to_owned())?;
            ffi::request_json(borrowed_handle(handle)?, string(&mut env, method)?, params)
        })();
        match result {
            Ok(response) => java_string(&mut env, response),
            Err(error) => {
                exception(&mut env, error);
                ptr::null_mut()
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_nextServerRequest(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
    ) -> jstring {
        match borrowed_handle(handle).and_then(ffi::next_server_request_json) {
            Ok(Some(request)) => java_string(&mut env, request),
            Ok(None) => ptr::null_mut(),
            Err(error) => {
                exception(&mut env, error);
                ptr::null_mut()
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_respondResult(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
        request_id_json: JString<'_>,
        result_json: JString<'_>,
    ) -> jboolean {
        match (
            borrowed_handle(handle),
            string(&mut env, request_id_json),
            string(&mut env, result_json),
        ) {
            (Ok(handle), Ok(request_id), Ok(result)) => {
                match ffi::respond_result_json(handle, &request_id, &result) {
                    Ok(()) => JNI_TRUE,
                    Err(error) => {
                        exception(&mut env, error);
                        JNI_FALSE
                    }
                }
            }
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                exception(&mut env, error);
                JNI_FALSE
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_respondError(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
        request_id_json: JString<'_>,
        error_json: JString<'_>,
    ) -> jboolean {
        match (
            borrowed_handle(handle),
            string(&mut env, request_id_json),
            string(&mut env, error_json),
        ) {
            (Ok(handle), Ok(request_id), Ok(error_json)) => {
                match ffi::respond_error_json(handle, &request_id, &error_json) {
                    Ok(()) => JNI_TRUE,
                    Err(error) => {
                        exception(&mut env, error);
                        JNI_FALSE
                    }
                }
            }
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                exception(&mut env, error);
                JNI_FALSE
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_nextNotification(
        mut env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
    ) -> jstring {
        match borrowed_handle(handle).and_then(ffi::next_notification_json) {
            Ok(Some(notification)) => java_string(&mut env, notification),
            Ok(None) => ptr::null_mut(),
            Err(error) => {
                exception(&mut env, error);
                ptr::null_mut()
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_close(
        _env: JNIEnv<'_>,
        _class: JClass<'_>,
        handle: jlong,
    ) {
        if handle != 0 {
            // SAFETY: Kotlin calls close exactly once for the returned handle.
            ffi::close_handle(handle as *mut ffi::Handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use host_protocol::{
        ConnectionLimits, DeviceAuthenticationReply, DeviceAuthenticationStart, PairingToken,
        RpcId, RpcNotification, RpcOutcome, RpcRequest, RpcResponse, ServerHello, read_frame,
        server_hello_proof_message, write_frame,
    };
    use quinn::{Endpoint, ServerConfig};
    use rcgen::generate_simple_self_signed;
    use ring::{
        rand::SystemRandom,
        signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
    };
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use serde_json::json;

    use super::*;

    fn valid_config(max_frame_bytes: u32) -> MobileClientConfig {
        MobileClientConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            server_name: "host.local".to_owned(),
            host_identity: Ed25519PublicKey::from_bytes([1; 32]),
            device_name: "test phone".to_owned(),
            pairing_ticket: None,
            max_frame_bytes,
            request_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn handshake_and_data_frame_limits_are_distinct() {
        assert_eq!(HANDSHAKE_MAX_FRAME_BYTES, 64 * 1024);
        assert_eq!(DEFAULT_MAX_FRAME_BYTES, 4 * 1024 * 1024);
        assert!(HANDSHAKE_MAX_FRAME_BYTES < DEFAULT_MAX_FRAME_BYTES);
    }

    #[test]
    fn accepts_host_data_frame_limit_even_though_it_exceeds_handshake_limit() {
        assert!(valid_config(DEFAULT_MAX_FRAME_BYTES).validate().is_ok());
    }

    #[test]
    fn rejects_data_frame_limit_above_host_protocol_maximum() {
        assert!(matches!(
            valid_config(DEFAULT_MAX_FRAME_BYTES + 1).validate(),
            Err(MobileClientError::InvalidConfig(
                "max_frame_bytes exceeds data-frame maximum"
            ))
        ));
    }

    #[tokio::test]
    async fn pairs_then_correlates_concurrent_requests_and_receives_notifications() {
        let (endpoint, address, certificate_hash) = server_endpoint();
        let (host_key, _) = key_pair();
        let host_identity = public_key(&host_key);
        let ticket = PairingToken::from_bytes([7; 32]);
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let (mut send, mut receive) = connection.accept_bi().await.unwrap();
            let hello: ClientHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES)
                .await
                .unwrap();
            let limits = ConnectionLimits {
                max_frame_bytes: 4096,
                max_in_flight_requests: 4,
                outbound_queue_messages: 8,
                request_timeout_ms: 1_000,
            };
            let server_hello = ServerHello {
                version: 2,
                limits: limits.clone(),
                supported_methods: vec!["thread/list".to_owned(), "thread/read".to_owned()],
                host_identity,
                transport_certificate_hash: certificate_hash,
                host_signature: sign(
                    &host_key,
                    &server_hello_proof_message(
                        hello.nonce,
                        2,
                        limits.max_frame_bytes,
                        host_identity,
                        certificate_hash,
                    ),
                ),
            };
            write_frame(&mut send, &server_hello, HANDSHAKE_MAX_FRAME_BYTES)
                .await
                .unwrap();
            let pairing: DeviceAuthenticationStart =
                read_frame(&mut receive, limits.max_frame_bytes)
                    .await
                    .unwrap();
            let DeviceAuthenticationStart::Pair { request } = pairing else {
                panic!("expected pairing")
            };
            assert_eq!(request.ticket, ticket);
            UnparsedPublicKey::new(&ED25519, request.device_identity.as_bytes())
                .verify(
                    &pairing_proof_message(host_identity, ticket, request.device_identity),
                    request.signature.as_bytes(),
                )
                .unwrap();
            write_frame(
                &mut send,
                &DeviceAuthenticationReply::Accepted {
                    device_identity: request.device_identity,
                },
                limits.max_frame_bytes,
            )
            .await
            .unwrap();

            let first: RpcMessage = read_frame(&mut receive, limits.max_frame_bytes)
                .await
                .unwrap();
            let second: RpcMessage = read_frame(&mut receive, limits.max_frame_bytes)
                .await
                .unwrap();
            let RpcMessage::Request(first) = first else {
                panic!("expected first request")
            };
            let RpcMessage::Request(second) = second else {
                panic!("expected second request")
            };
            write_frame(&mut send, &response(&second), limits.max_frame_bytes)
                .await
                .unwrap();
            write_frame(
                &mut send,
                &RpcMessage::Notification(RpcNotification {
                    method: "turn/started".to_owned(),
                    params: json!({
                        "type":"turnStarted",
                        "threadId":"t1",
                        "turnId":"turn-1",
                        "status":"inProgress"
                    }),
                    extensions: Default::default(),
                }),
                limits.max_frame_bytes,
            )
            .await
            .unwrap();
            write_frame(
                &mut send,
                &RpcMessage::Notification(RpcNotification {
                    method: "codex/futureEvent".to_owned(),
                    params: json!({"futureField": {"preserve": true}}),
                    extensions: Default::default(),
                }),
                limits.max_frame_bytes,
            )
            .await
            .unwrap();
            write_frame(
                &mut send,
                &RpcMessage::Request(RpcRequest {
                    id: RpcId::String("approval-7".to_owned()),
                    method: "item/commandExecution/requestApproval".to_owned(),
                    params: json!({"command": "cargo test"}),
                    extensions: Default::default(),
                }),
                limits.max_frame_bytes,
            )
            .await
            .unwrap();
            write_frame(&mut send, &response(&first), limits.max_frame_bytes)
                .await
                .unwrap();
            let response: RpcMessage = timeout(
                Duration::from_secs(1),
                read_frame(&mut receive, limits.max_frame_bytes),
            )
            .await
            .expect("Host response timed out")
            .unwrap();
            let RpcMessage::Response(response) = response else {
                panic!("expected response to Host request")
            };
            assert_eq!(response.id, RpcId::String("approval-7".to_owned()));
            let RpcOutcome::Success { result } = response.outcome else {
                panic!("expected successful Host request response")
            };
            assert_eq!(result, json!({"approved": true}));
            write_frame(
                &mut send,
                &RpcMessage::Notification(RpcNotification {
                    method: "test/responseReceived".to_owned(),
                    params: json!({}),
                    extensions: Default::default(),
                }),
                limits.max_frame_bytes,
            )
            .await
            .unwrap();
            connection.closed().await;
        });

        let (_device, pkcs8) = key_pair();
        let client = MobileClient::connect(
            MobileClientConfig {
                address,
                server_name: "ignored-by-proof-verifier".to_owned(),
                host_identity,
                device_name: "test phone".to_owned(),
                pairing_ticket: Some(ticket),
                max_frame_bytes: 4096,
                request_timeout: Duration::from_secs(1),
            },
            &pkcs8,
        )
        .await
        .unwrap();
        let mut notifications = client.subscribe();
        let mut server_requests = client.subscribe_server_requests();
        let (list, read) = tokio::join!(
            client.request("codex/customMethod", json!({"arbitrary": true})),
            client.request("thread/experimental", json!({"id":"t1"}))
        );
        assert_eq!(list.unwrap()["method"], "codex/customMethod");
        assert_eq!(read.unwrap()["method"], "thread/experimental");
        assert_eq!(
            notifications.recv().await.unwrap(),
            Notification {
                method: "turn/started".to_owned(),
                params: json!({
                    "type":"turnStarted",
                    "threadId":"t1",
                    "turnId":"turn-1",
                    "status":"inProgress"
                }),
                extensions: Default::default(),
            }
        );
        assert_eq!(
            notifications.recv().await.unwrap(),
            Notification {
                method: "codex/futureEvent".to_owned(),
                params: json!({"futureField": {"preserve": true}}),
                extensions: Default::default(),
            }
        );
        let server_request = server_requests.recv().await.unwrap();
        assert_eq!(server_request.id, RpcId::String("approval-7".to_owned()));
        assert_eq!(
            server_request.method,
            "item/commandExecution/requestApproval"
        );
        assert_eq!(server_request.params, json!({"command": "cargo test"}));
        client
            .respond_result(server_request.id, json!({"approved": true}))
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), notifications.recv())
                .await
                .expect("response acknowledgement timed out")
                .unwrap()
                .method,
            "test/responseReceived"
        );
        client.close();
        server.await.unwrap();
    }

    fn response(request: &RpcRequest) -> RpcMessage {
        RpcMessage::Response(RpcResponse {
            id: request.id.clone(),
            outcome: RpcOutcome::Success {
                result: json!({"method": request.method}),
            },
            extensions: Default::default(),
        })
    }

    fn server_endpoint() -> (Endpoint, SocketAddr, TransportCertificateHash) {
        let certificate = generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let certificate_der = certificate.cert.der().clone();
        let certificate_hash = TransportCertificateHash::from_bytes(
            ring::digest::digest(&ring::digest::SHA256, certificate_der.as_ref())
                .as_ref()
                .try_into()
                .unwrap(),
        );
        let config = ServerConfig::with_single_cert(vec![certificate_der], key.into()).unwrap();
        let endpoint = Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        (endpoint, address, certificate_hash)
    }

    fn key_pair() -> (Ed25519KeyPair, Vec<u8>) {
        let bytes = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let bytes = bytes.as_ref().to_vec();
        (Ed25519KeyPair::from_pkcs8(&bytes).unwrap(), bytes)
    }

    fn public_key(key: &Ed25519KeyPair) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(key.public_key().as_ref().try_into().unwrap())
    }
}
