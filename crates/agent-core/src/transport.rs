//! iroh is confined to this module. A session is one bidirectional JSONL stream.
use crate::peer::{PeerError, RpcPeer};
use iroh::{EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use iroh_tickets::endpoint::EndpointTicket;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    io,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use uuid::Uuid;

const ALPN: &[u8] = b"remote-agent";
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
pub struct Endpoint(iroh::Endpoint);
impl Endpoint {
    pub async fn bind(identity: Identity, relays: Relays) -> Result<Self, TransportError> {
        let mut builder = iroh::Endpoint::builder(presets::N0)
            .secret_key(identity.0)
            .alpns(vec![ALPN.to_vec()]);
        builder = match relays {
            Relays::Default => builder,
            Relays::Disabled => builder
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
        Ok(Self(builder.bind().await.map_err(connection)?))
    }
    pub fn node_id(&self) -> NodeId {
        NodeId(self.0.id())
    }
    pub fn ticket(&self) -> Ticket {
        Ticket(EndpointTicket::new(self.0.addr()))
    }
    pub async fn online(&self) {
        self.0.online().await;
    }
    pub async fn connect(&self, ticket: &Ticket) -> Result<Session, TransportError> {
        let address: EndpointAddr = ticket.0.endpoint_addr().clone();
        let connection = self.0.connect(address, ALPN).await.map_err(connection)?;
        Ok(Session {
            connection,
            _endpoint: self.clone(),
        })
    }
    /// TLS authenticates the node ID. Call `authorize` before routing any RPC or blob.
    pub async fn accept(&self) -> Result<Session, TransportError> {
        let incoming = self
            .0
            .accept()
            .await
            .ok_or_else(|| connection("endpoint closed"))?;
        let connection = incoming.await.map_err(connection)?;
        Ok(Session {
            connection,
            _endpoint: self.clone(),
        })
    }
    pub async fn close(&self) {
        self.0.close().await;
    }
}
#[derive(Clone)]
pub struct Session {
    connection: iroh::endpoint::Connection,
    _endpoint: Endpoint,
}
impl Session {
    pub fn node_id(&self) -> NodeId {
        NodeId(self.connection.remote_id())
    }
    pub async fn open_stream(
        &self,
    ) -> Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static, TransportError> {
        let (send, recv) = self.connection.open_bi().await.map_err(connection)?;
        Ok(Stream {
            send,
            recv,
            _session: self.clone(),
            shutdown: None,
        })
    }
    pub async fn accept_stream(
        &self,
    ) -> Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static, TransportError> {
        let (send, recv) = self.connection.accept_bi().await.map_err(connection)?;
        Ok(Stream {
            send,
            recv,
            _session: self.clone(),
            shutdown: None,
        })
    }
    pub async fn open_peer(
        &self,
        timeout: Duration,
        max_requests: usize,
    ) -> Result<RpcPeer, TransportError> {
        let stream = self.open_stream().await?;
        let (read, write) = tokio::io::split(stream);
        Ok(RpcPeer::open(
            host_protocol::JsonlReader::new(read),
            write,
            timeout,
            max_requests,
        )?)
    }
    pub fn close(&self) {
        self.connection.close(0u8.into(), b"session closed");
    }
}
struct Stream {
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
