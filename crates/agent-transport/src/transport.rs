//! Authenticated iroh sessions with independent request, subscription, and blob streams.
use crate::client::Client;
pub use crate::client::{HostPeer, HostRequest};
use crate::{
    diagnostics::{
        ConnectionPhase as Phase,
        connection::{Trace, identifier},
    },
    peer::PeerError,
};
use iroh::{EndpointAddr, RelayMode, SecretKey, TransportAddr, endpoint::presets};
use iroh_tickets::endpoint::EndpointTicket;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    io,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::Instrument;
use uuid::Uuid;

// Bump when shared Postcard types change incompatibly; enum indices and field
// positions are part of the wire format, even when decoding still succeeds.
const ALPN: &[u8] = b"remote-agent/streams/6";
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("iroh transport failed: {0}")]
    Connection(String),
    #[error("invalid endpoint ticket: {0}")]
    Ticket(String),
    #[error("peer is not authorized")]
    Unauthorized,
    #[error(transparent)]
    Peer(#[from] PeerError),
}
fn connection(error: impl fmt::Display) -> TransportError {
    TransportError::Connection(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(iroh::EndpointId);
impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for NodeId {
    type Err = TransportError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self).map_err(connection)
    }
}
pub struct Identity(SecretKey);
impl Identity {
    pub fn generate() -> Self {
        Self(SecretKey::generate())
    }
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(SecretKey::from_bytes(&bytes))
    }
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
    pub fn node_id(&self) -> NodeId {
        NodeId(self.0.public())
    }
}
#[derive(Debug, Clone, Default)]
pub enum Relays {
    #[default]
    Default,
    Disabled,
    /// Same-machine connections, without relay, lookup or interface addresses.
    Loopback,
    Custom(Vec<String>),
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ticket(EndpointTicket);
impl Ticket {
    pub fn node_id(&self) -> NodeId {
        NodeId(self.0.endpoint_addr().id)
    }
}
impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Ticket {
    type Err = TransportError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self)
            .map_err(|error: iroh_tickets::ParseError| TransportError::Ticket(error.to_string()))
    }
}
/// Pure authorization data. The daemon commits updates under its state owner's lock.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    pub allowed: BTreeSet<NodeId>,
    pub invitations: BTreeMap<Uuid, u64>,
}
/// Returns `Some(updated)` when pairing consumed a token, `None` for an existing peer.
/// The owner must commit the update before accepting another pairing attempt.
pub fn authorize(
    trust: &Trust,
    node: NodeId,
    invitation: Option<Uuid>,
    now: u64,
) -> Result<Option<Trust>, TransportError> {
    if trust.allowed.contains(&node) {
        return Ok(None);
    }
    let invitation = invitation
        .filter(|token| {
            trust
                .invitations
                .get(token)
                .is_some_and(|expires_at| now < *expires_at)
        })
        .ok_or(TransportError::Unauthorized)?;
    let mut next = trust.clone();
    next.invitations.remove(&invitation);
    next.allowed.insert(node);
    Ok(Some(next))
}

#[derive(Clone)]
pub struct Endpoint(Arc<iroh::Endpoint>, Arc<Trace>);
impl Endpoint {
    pub async fn bind(identity: Identity, relays: Relays) -> Result<Self, TransportError> {
        Self::bind_recording(identity, relays, Trace::new()).await
    }
    pub async fn bind_recording(
        identity: Identity,
        relays: Relays,
        trace: Arc<Trace>,
    ) -> Result<Self, TransportError> {
        crate::diagnostics::connection::initialize_mobile();
        trace.activate();
        trace.record(Phase::EndpointStart, 0, 0, 0);
        let mut builder = iroh::Endpoint::builder(presets::N0)
            .secret_key(identity.0)
            .alpns(vec![ALPN.to_vec()]);
        if matches!(relays, Relays::Loopback) {
            builder = builder
                .clear_ip_transports()
                .bind_addr("127.0.0.1:0")
                .map_err(connection)?;
        }
        builder = match relays {
            Relays::Default => builder,
            Relays::Disabled | Relays::Loopback => builder
                .relay_mode(RelayMode::Disabled)
                .clear_address_lookup(),
            Relays::Custom(urls) => {
                let urls: Vec<iroh::RelayUrl> = urls
                    .into_iter()
                    .map(|url| url.parse().map_err(connection))
                    .collect::<Result<_, _>>()?;
                builder.relay_mode(RelayMode::Custom(urls.into_iter().collect()))
            }
        };
        let span = tracing::info_span!(target: "bex.net", "network", trace_id = trace.id);
        let endpoint = builder.bind().instrument(span).await.map_err(connection)?;
        trace.record(Phase::EndpointReady, 0, 0, 0);
        Ok(Self(Arc::new(endpoint), trace))
    }
    pub fn node_id(&self) -> NodeId {
        NodeId(self.0.id())
    }
    pub fn ticket(&self) -> Ticket {
        Ticket(EndpointTicket::new(self.0.addr()))
    }
    /// The wildcard Host socket also accepts loopback traffic; local discovery
    /// supplies its port with a loopback IP instead of the advertised LAN IPs.
    pub fn local_ticket(&self) -> Ticket {
        Ticket(EndpointTicket::new(EndpointAddr::from_parts(
            self.0.id(),
            self.0
                .bound_sockets()
                .into_iter()
                .filter(|addr| addr.is_ipv4())
                .map(|addr| TransportAddr::Ip((std::net::Ipv4Addr::LOCALHOST, addr.port()).into())),
        )))
    }
    pub async fn connect(&self, ticket: &Ticket) -> Result<Session, TransportError> {
        self.1.activate();
        let group = identifier();
        self.1.record(Phase::ResolveStart, group, 0, 0);
        let address: EndpointAddr = ticket.0.endpoint_addr().clone();
        let started = std::time::Instant::now();
        let connecting = self
            .0
            .connect_with_opts(address, ALPN, Default::default())
            .await
            .inspect_err(|_| self.1.record(Phase::ResolveFailed, group, 0, 0))
            .map_err(connection)?;
        let resolution_ms = started.elapsed().as_millis() as u64;
        self.1.record(Phase::ResolveReady, group, 0, 0);
        self.1.record(Phase::QuicStart, group, 0, 0);
        let connection = connecting
            .await
            .inspect_err(|_| self.1.record(Phase::QuicFailed, group, 0, 0))
            .map_err(connection)?;
        self.1.record(Phase::QuicReady, group, 0, 0);
        Ok(Session {
            connection,
            _endpoint: self.clone(),
            resolution_ms,
            diagnostic_id: group,
        })
    }
    /// TLS identifies the peer; RPC and blob streams remain inaccessible until
    /// the caller supplies the committed authorization state.
    /// `None` means endpoint shutdown. Handshake errors belong to one connection.
    pub async fn accept(&self) -> Option<Result<IncomingSession, TransportError>> {
        let incoming = self.0.accept().await?;
        Some(
            incoming
                .await
                .map(|connection| {
                    IncomingSession(Some(Session {
                        connection,
                        _endpoint: self.clone(),
                        resolution_ms: 0,
                        diagnostic_id: identifier(),
                    }))
                })
                .map_err(connection),
        )
    }
    pub fn connection_diagnostics_active(&self) -> bool {
        self.1.active()
    }
    pub fn connection_time(&self, at: std::time::Instant) -> (u64, u64) {
        (self.1.id, self.1.elapsed_at(at))
    }
    pub fn log_connection_diagnostics(&self) {
        let mut timeline = self.1.snapshot();
        let cutoff = self
            .1
            .elapsed_at(std::time::Instant::now())
            .saturating_sub(45_000_000);
        timeline.events.retain(|event| event.at_us >= cutoff);
        tracing::info!(target: "bex", operation = "host.connection.timeline", message = %format_args!("trace={} dropped={}", timeline.id, timeline.dropped));
        crate::diagnostics::connection_events("host.connection.event", &timeline);
    }
    pub async fn close(&self) {
        self.0.close().await;
    }
}
/// An authenticated node identity without permission to exchange application data.
/// Dropping or rejecting it closes the connection.
pub struct IncomingSession(Option<Session>);
impl IncomingSession {
    pub fn node_id(&self) -> NodeId {
        self.0.as_ref().unwrap().node_id()
    }
    /// Read only the typed pairing request. No RPC peer or stream is exposed
    /// until the owner commits trust and authorizes the request.
    pub async fn pairing(self) -> Result<PairingRequest, TransportError> {
        let session = self.0.as_ref().unwrap();
        let peer = session.accept_peer().await?;
        let IncomingRequest::Call(call) = session.accept_request().await? else {
            return Err(TransportError::Unauthorized);
        };
        let crate::protocol::Call::Pair(params) = &call.call else {
            return Err(connection("expected pairing request"));
        };
        Ok(PairingRequest {
            incoming: self,
            peer,
            reply: call.send,
            invitation: params.invitation,
        })
    }
    /// Only an allowlisted identity can become an application session. Pairing
    /// must persist the updated trust before passing it to this gate.
    pub fn authorize(mut self, trust: &Trust) -> Result<Session, TransportError> {
        authorize(trust, self.node_id(), None, 0)?;
        Ok(self.0.take().unwrap())
    }
}
impl Drop for IncomingSession {
    fn drop(&mut self) {
        if let Some(session) = &self.0 {
            session.close();
        }
    }
}
/// A pairing request whose underlying connection remains inaccessible.
pub struct PairingRequest {
    incoming: IncomingSession,
    peer: HostPeer,
    reply: iroh::endpoint::SendStream,
    pub invitation: Uuid,
}
impl PairingRequest {
    /// Supply the persisted allowlist after consuming `invitation` atomically.
    pub async fn authorize(mut self, trust: &Trust) -> Result<(Session, HostPeer), TransportError> {
        let session = scopeguard::guard(self.incoming.authorize(trust)?, |session| session.close());
        crate::framing::write(
            &mut self.reply,
            crate::protocol::Response::Success {
                result: crate::models::Empty {},
            },
        )
        .await
        .map_err(connection)?;
        self.reply.finish().map_err(connection)?;
        Ok((scopeguard::ScopeGuard::into_inner(session), self.peer))
    }
}
#[derive(Clone)]
pub struct Session {
    connection: iroh::endpoint::Connection,
    _endpoint: Endpoint,
    resolution_ms: u64,
    diagnostic_id: u64,
}
impl Session {
    pub fn resolution_ms(&self) -> u64 {
        self.resolution_ms
    }
    pub fn uses_endpoint(&self, endpoint: &Endpoint) -> bool {
        Arc::ptr_eq(&self._endpoint.0, &endpoint.0)
    }

    /// Open another destination through this client's existing endpoint identity.
    pub async fn connect(&self, ticket: &Ticket) -> Result<Session, TransportError> {
        self._endpoint.connect(ticket).await
    }
    pub fn node_id(&self) -> NodeId {
        NodeId(self.connection.remote_id())
    }
    pub async fn open_stream(
        &self,
    ) -> Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static + use<>, TransportError> {
        let (mut send, recv) = self.connection.open_bi().await.map_err(connection)?;
        send.write_all(&[crate::client::BLOB])
            .await
            .map_err(connection)?;
        Ok(Stream {
            send,
            recv,
            _session: self.clone(),
            shutdown: None,
        })
    }
    /// Accept the session's ordered notification feed before dispatching requests.
    pub async fn accept_peer(&self) -> Result<HostPeer, TransportError> {
        let (send, mut recv) = self.connection.accept_bi().await.map_err(connection)?;
        let mut kind = [0u8; 1];
        recv.read_exact(&mut kind).await.map_err(connection)?;
        if kind[0] != crate::client::EVENTS {
            return Err(connection("expected event subscription"));
        }
        Ok(send)
    }
    /// Hand each accepted stream directly to the resource owner.
    pub async fn accept_request(&self) -> Result<IncomingRequest, TransportError> {
        use crate::client::{BLOB, CALL, CLOSE};
        let (mut send, mut recv) = self.connection.accept_bi().await.map_err(connection)?;
        let accepted_at = std::time::Instant::now();
        let mut kind = [0u8; 1];
        recv.read_exact(&mut kind).await.map_err(connection)?;
        match kind[0] {
            BLOB => Ok(IncomingRequest::Blob(Stream {
                send,
                recv,
                _session: self.clone(),
                shutdown: None,
            })),
            CALL => {
                let call = crate::framing::Reader::new(recv)
                    .read::<crate::protocol::Call>()
                    .await
                    .map_err(connection)?
                    .ok_or_else(|| connection("request stream ended before its request"))?;
                if matches!(call, crate::protocol::Call::SubscribeShell(_)) {
                    self._endpoint.1.activate();
                }
                Ok(IncomingRequest::Call(HostRequest {
                    call,
                    send,
                    accepted_at,
                    decoded_at: std::time::Instant::now(),
                }))
            }
            CLOSE => {
                send.write_all(&[0]).await.map_err(connection)?;
                send.finish().map_err(connection)?;
                Ok(IncomingRequest::Close)
            }
            _ => Err(connection("unknown stream kind")),
        }
    }
    pub async fn open_peer(
        &self,
        timeout: Duration,
        max_requests: usize,
    ) -> Result<(Client, crate::framing::Reader), TransportError> {
        Ok(Client::connect(
            self.connection.clone(),
            timeout,
            max_requests,
            self._endpoint.1.clone(),
            self.diagnostic_id,
        )
        .await?)
    }
    pub fn close(&self) {
        self.connection.close(0u8.into(), b"session closed");
    }
}
pub enum IncomingRequest {
    Call(HostRequest),
    Blob(Stream),
    Close,
}
pub struct Stream {
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    _session: Session,
    shutdown: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().recv).poll_read(cx, buffer)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().send), cx, buffer)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().send), cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        if stream.shutdown.is_none() {
            if let Err(error) = stream.send.finish() {
                return Poll::Ready(Err(io::Error::other(error)));
            }
            let stopped = stream.send.stopped();
            // Keep the SDK's acknowledgment future alive across polls. It is not re-exported
            // by iroh, so this single allocation is confined to stream shutdown.
            stream.shutdown = Some(Box::pin(async move {
                match stopped.await.map_err(io::Error::other)? {
                    None => Ok(()),
                    Some(code) => Err(io::Error::other(format!(
                        "peer stopped stream with code {code}"
                    ))),
                }
            }));
        }
        stream.shutdown.as_mut().unwrap().as_mut().poll(cx)
    }
}
