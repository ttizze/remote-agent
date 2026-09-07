//! Bounded Phoenix byte tunnels. Both roles use this one framing and lifecycle
//! implementation; authentication and encryption belong to the SSH endpoints.

use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use host_protocol::RelayEndpoint;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    sync::{mpsc, Semaphore},
    task::JoinHandle,
    time::{self, Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const JOIN_REF: &str = "1";
const CHUNK_BYTES: usize = 32 * 1024;
const MAX_ENCODED_BYTES: usize = 65_536;
const PEER_BUFFER_BYTES: usize = 128 * 1024;
const PEER_QUEUE: usize = 16;
const CONNECTION_QUEUE: usize = 64;
const IO_DEADLINE: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("invalid relay endpoint: {0}")]
    Endpoint(#[from] host_protocol::RelayEndpointError),
    #[error("relay WebSocket failed")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("relay protocol violation: {0}")]
    Protocol(&'static str),
    #[error("relay rejected the operation: {0}")]
    Rejected(String),
    #[error("relay closed the connection")]
    Closed,
    #[error("relay did not respond before the deadline")]
    Timeout,
    #[error("relay task stopped unexpectedly")]
    Task,
}

/// Drop cancels the socket and every bridge it owns. No task is detached when a
/// caller abandons a connection or an SSH handshake times out.
pub struct RelayTask(Option<JoinHandle<Result<(), RelayError>>>);

impl RelayTask {
    pub fn abort(&self) {
        if let Some(task) = &self.0 { task.abort(); }
    }

    pub async fn wait(&mut self) -> Result<(), RelayError> {
        let result = self.0.as_mut().ok_or(RelayError::Closed)?.await.map_err(|_| RelayError::Task)?;
        self.0.take();
        result
    }
}

impl Drop for RelayTask {
    fn drop(&mut self) { self.abort(); }
}

pub struct IncomingConnection {
    pub id: Arc<str>,
    pub stream: DuplexStream,
    pub disconnect: CancellationToken,
}

pub async fn connect_runner(endpoint: &RelayEndpoint) -> Result<(mpsc::Receiver<IncomingConnection>, RelayTask), RelayError> {
    let (socket, _) = connect(endpoint, "runner").await?;
    let (incoming, receiver) = mpsc::channel(CONNECTION_QUEUE);
    let topic = format!("runner:{}", endpoint.runner_id);
    let task = tokio::spawn(run(socket, topic, Some(incoming), None));
    Ok((receiver, RelayTask(Some(task))))
}

pub async fn connect_client(endpoint: &RelayEndpoint) -> Result<(DuplexStream, RelayTask), RelayError> {
    let (socket, client_id) = connect(endpoint, "mobile").await?;
    let id: Arc<str> = client_id.ok_or(RelayError::Protocol("join response has no client ID"))?.into();
    let (stream, bridge) = tokio::io::duplex(PEER_BUFFER_BYTES);
    let topic = format!("runner:{}", endpoint.runner_id);
    let task = tokio::spawn(run(socket, topic, None, Some((id, bridge))));
    Ok((stream, RelayTask(Some(task))))
}

async fn connect(endpoint: &RelayEndpoint, role: &str) -> Result<(Socket, Option<String>), RelayError> {
    let url = endpoint.socket_url(role)?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(100_000))
        .max_frame_size(Some(100_000));
    let (mut socket, _) = time::timeout(IO_DEADLINE, connect_async_with_config(url.as_str(), Some(config), true))
        .await.map_err(|_| RelayError::Timeout)??;
    let topic = format!("runner:{}", endpoint.runner_id);
    send(&mut socket, &(JOIN_REF, JOIN_REF, &topic, "phx_join", serde_json::json!({}))).await?;
    let id = time::timeout(IO_DEADLINE, async {
        loop {
            match socket.next().await.ok_or(RelayError::Closed)?? {
                Message::Text(text) => {
                    let frame = Frame::parse(&text)?;
                    if frame.0 != Some(JOIN_REF) || frame.1 != Some(JOIN_REF) || frame.2 != topic {
                        continue;
                    }
                    if frame.3 != "phx_reply" { return Err(RelayError::Protocol("unexpected join response")); }
                    let payload = Payload::parse(frame.4)?;
                    if payload.status != Some("ok") { return Err(rejected(&payload)); }
                    let response = payload.response.ok_or(RelayError::Protocol("join reply has no response"))?;
                    return Ok(Payload::parse(response)?.client_id.map(str::to_owned));
                }
                Message::Ping(bytes) => send_message(&mut socket, Message::Pong(bytes)).await?,
                Message::Pong(_) => {},
                _ => return Err(RelayError::Closed),
            }
        }
    }).await.map_err(|_| RelayError::Timeout)??;
    Ok((socket, id))
}

#[derive(Deserialize)]
struct Frame<'a>(
    #[serde(borrow)] Option<&'a str>,
    #[serde(borrow)] Option<&'a str>,
    &'a str,
    &'a str,
    #[serde(borrow)] &'a RawValue,
);

impl<'a> Frame<'a> {
    fn parse(text: &'a str) -> Result<Self, RelayError> {
        serde_json::from_str(text).map_err(|_| RelayError::Protocol("invalid Phoenix frame"))
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    #[serde(borrow)]
    client_id: Option<&'a str>,
    data: Option<&'a str>,
    status: Option<&'a str>,
    reason: Option<&'a str>,
    #[serde(borrow)]
    response: Option<&'a RawValue>,
}

impl<'a> Payload<'a> {
    fn parse(raw: &'a RawValue) -> Result<Self, RelayError> {
        serde_json::from_str(raw.get()).map_err(|_| RelayError::Protocol("invalid Phoenix payload"))
    }
}

fn rejected(payload: &Payload<'_>) -> RelayError {
    let reason = payload.response.and_then(|raw| Payload::parse(raw).ok())
        .and_then(|response| response.reason).unwrap_or("operation rejected");
    // Relay-controlled strings are bounded before they enter diagnostics.
    RelayError::Rejected(reason.chars().filter(|c| !c.is_control()).take(128).collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Data<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<&'a str>,
    data: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientId<'a> { client_id: &'a str }

enum Outbound {
    Data(Arc<str>, String),
    Ack(Arc<str>),
    Closed(Arc<str>),
}

struct Bridge {
    incoming: mpsc::Sender<Vec<u8>>,
    credits: Arc<Semaphore>,
    in_flight: usize,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    disconnect: CancellationToken,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.disconnect.cancel();
        self.reader.abort();
        self.writer.abort();
    }
}

fn bridge(id: Arc<str>, stream: DuplexStream, outgoing: mpsc::Sender<Outbound>) -> Bridge {
    let disconnect = CancellationToken::new();
    let (incoming, mut receiver) = mpsc::channel::<Vec<u8>>(PEER_QUEUE);
    let credits = Arc::new(Semaphore::new(PEER_QUEUE));
    let read_credits = credits.clone();
    let (mut input, mut output) = tokio::io::split(stream);
    let read_id = id.clone();
    let read_outgoing = outgoing.clone();
    let read_disconnect = disconnect.clone();
    let reader = tokio::spawn(async move {
        let mut buffer = [0; CHUNK_BYTES];
        let transfer = async { loop {
            let Ok(permit) = read_credits.clone().acquire_owned().await else { break };
            let Ok(count) = input.read(&mut buffer).await else { break };
            if count == 0 { break; }
            let data = STANDARD.encode(&buffer[..count]);
            if read_outgoing.send(Outbound::Data(read_id.clone(), data)).await.is_err() { return; }
            // Each chunk consumes one credit until the opposite endpoint has
            // written it to its own bounded SSH stream. Reading and writing run
            // independently so two full-duplex peers cannot deadlock.
            permit.forget();
        }};
        tokio::select! { _ = transfer => {}, _ = read_disconnect.cancelled() => {} }
        let _ = read_outgoing.send(Outbound::Closed(read_id)).await;
    });
    let write_disconnect = disconnect.clone();
    let writer_id = id.clone();
    let writer_outgoing = outgoing.clone();
    let writer = tokio::spawn(async move {
        let transfer = async { while let Some(data) = receiver.recv().await {
            if output.write_all(&data).await.is_err() {
                let _ = outgoing.send(Outbound::Closed(id)).await;
                return;
            }
            if outgoing.send(Outbound::Ack(id.clone())).await.is_err() { return; }
        }};
        tokio::select! { _ = transfer => {}, _ = write_disconnect.cancelled() => {} }
        let _ = writer_outgoing.send(Outbound::Closed(writer_id)).await;
    });
    Bridge { incoming, credits, in_flight: 0, reader, writer, disconnect }
}

async fn run(
    mut socket: Socket,
    topic: String,
    incoming: Option<mpsc::Sender<IncomingConnection>>,
    client: Option<(Arc<str>, DuplexStream)>,
) -> Result<(), RelayError> {
    let is_runner = incoming.is_some();
    let (outgoing, mut outbound) = mpsc::channel(CONNECTION_QUEUE);
    let mut peers = HashMap::<Arc<str>, Bridge>::new();
    let client_id = client.as_ref().map(|(id, _)| id.clone());
    if let Some((id, stream)) = client {
        peers.insert(id.clone(), bridge(id, stream, outgoing.clone()));
    }
    let mut reference = 2_u64;
    let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut heartbeat_pending = false;
    let mut last_received = Instant::now();

    loop {
        tokio::select! {
            incoming_frame = socket.next() => {
                match incoming_frame.ok_or(RelayError::Closed)?? {
                    Message::Text(text) => {
                        let frame = Frame::parse(&text)?;
                        last_received = Instant::now();
                        if frame.2 == "phoenix" && frame.1 == Some("heartbeat") && frame.3 == "phx_reply" {
                            if Payload::parse(frame.4)?.status != Some("ok") { return Err(RelayError::Protocol("heartbeat rejected")); }
                            heartbeat_pending = false;
                            continue;
                        }
                        if frame.2 != topic || frame.0.is_some_and(|reference| reference != JOIN_REF) { continue; }
                        let payload = Payload::parse(frame.4)?;
                        match frame.3 {
                            "open" if is_runner => {
                                let id = checked_client_id(payload.client_id)?;
                                if peers.contains_key(id) { return Err(RelayError::Protocol("duplicate client route")); }
                                if peers.len() >= CONNECTION_QUEUE { return Err(RelayError::Protocol("too many client routes")); }
                                let id: Arc<str> = id.into();
                                let (stream, local_bridge) = tokio::io::duplex(PEER_BUFFER_BYTES);
                                let peer = bridge(id.clone(), local_bridge, outgoing.clone());
                                if incoming.as_ref().expect("runner has receiver").try_send(IncomingConnection { id: id.clone(), stream, disconnect: peer.disconnect.clone() }).is_err() {
                                    return Err(RelayError::Protocol("runner stopped accepting clients"));
                                }
                                peers.insert(id, peer);
                            }
                            "data" => {
                                let id = if is_runner { checked_client_id(payload.client_id)? } else { client_id.as_deref().expect("client ID assigned") };
                                let encoded = payload.data.ok_or(RelayError::Protocol("data field missing"))?;
                                if encoded.is_empty() || encoded.len() > MAX_ENCODED_BYTES { return Err(RelayError::Protocol("data frame size invalid")); }
                                let bytes = STANDARD.decode(encoded).map_err(|_| RelayError::Protocol("invalid base64 data"))?;
                                if let Some(peer) = peers.get(id) {
                                    if peer.incoming.try_send(bytes).is_err() {
                                        peers.remove(id);
                                        if !is_runner { return Err(RelayError::Protocol("client buffer full")); }
                                        event(&mut socket, &topic, &mut reference, "close", &ClientId { client_id: id }).await?;
                                    }
                                }
                                // An already-closed route may have in-flight
                                // frames. Never deliver them to a replacement.
                            }
                            "ack" => {
                                let id = if is_runner { checked_client_id(payload.client_id)? } else { client_id.as_deref().expect("client ID assigned") };
                                if let Some(peer) = peers.get_mut(id) {
                                    if peer.in_flight == 0 { return Err(RelayError::Protocol("unexpected credit acknowledgement")); }
                                    peer.in_flight -= 1;
                                    peer.credits.add_permits(1);
                                }
                            }
                            "close" if is_runner => { peers.remove(checked_client_id(payload.client_id)?); }
                            "closed" | "phx_close" | "phx_error" => return Err(RelayError::Closed),
                            "phx_reply" => {
                                if payload.status != Some("ok") {
                                    let reason = payload.response.and_then(|raw| Payload::parse(raw).ok()).and_then(|p| p.reason);
                                    if !(is_runner && reason == Some("client_closed")) { return Err(rejected(&payload)); }
                                }
                            }
                            _ => return Err(RelayError::Protocol("unexpected relay event")),
                        }
                    }
                    Message::Ping(bytes) => { send_message(&mut socket, Message::Pong(bytes)).await?; }
                    Message::Pong(_) => { last_received = Instant::now(); }
                    _ => return Err(RelayError::Closed),
                }
            }
            outgoing_frame = outbound.recv() => {
                match outgoing_frame.ok_or(RelayError::Closed)? {
                    Outbound::Data(id, data) => {
                        if let Some(peer) = peers.get_mut(&id) {
                            peer.in_flight += 1;
                            event(&mut socket, &topic, &mut reference, "data", &Data { client_id: is_runner.then_some(id.as_ref()), data: &data }).await?;
                        }
                    }
                    Outbound::Ack(id) => {
                        if peers.contains_key(&id) {
                            if is_runner {
                                event(&mut socket, &topic, &mut reference, "ack", &ClientId { client_id: &id }).await?;
                            } else {
                                event(&mut socket, &topic, &mut reference, "ack", &serde_json::json!({})).await?;
                            }
                        }
                    }
                    Outbound::Closed(id) => {
                        if peers.remove(&id).is_some() {
                            if !is_runner { return Ok(()); }
                            event(&mut socket, &topic, &mut reference, "close", &ClientId { client_id: &id }).await?;
                        }
                    }
                }
            }
            _ = heartbeat.tick() => {
                if heartbeat_pending || last_received.elapsed() > IO_DEADLINE { return Err(RelayError::Timeout); }
                send(&mut socket, &(Option::<&str>::None, "heartbeat", "phoenix", "heartbeat", serde_json::json!({}))).await?;
                heartbeat_pending = true;
            }
        }
    }
}

fn checked_client_id(id: Option<&str>) -> Result<&str, RelayError> {
    id.filter(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .ok_or(RelayError::Protocol("invalid client ID"))
}

async fn event(socket: &mut Socket, topic: &str, reference: &mut u64, event: &str, payload: &impl Serialize) -> Result<(), RelayError> {
    let current = reference.to_string();
    *reference = reference.checked_add(1).ok_or(RelayError::Protocol("frame reference exhausted"))?;
    send(socket, &(JOIN_REF, current, topic, event, payload)).await
}

async fn send(socket: &mut Socket, value: &impl Serialize) -> Result<(), RelayError> {
    let text = serde_json::to_string(value).map_err(|_| RelayError::Protocol("failed to encode frame"))?;
    send_message(socket, Message::text(text)).await
}

async fn send_message(socket: &mut Socket, message: Message) -> Result<(), RelayError> {
    time::timeout(IO_DEADLINE, socket.send(message)).await.map_err(|_| RelayError::Timeout)??;
    Ok(())
}
