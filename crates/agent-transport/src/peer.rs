//! RPC correlation and ordering for external provider JSONL processes.
//!
//! Owns shared peer state, connection lifecycle, and outbound write acknowledgement.
//! Request exchange, stream pumps, and envelope encoding have private modules;
//! the public API remains available through this module.
mod envelope;
mod io;
mod jsonl;
mod request;
#[cfg(test)]
mod tests;

pub use envelope::{request_line, response_line};
pub use request::Request;

pub use agent_protocol::message::{RpcMessage, RpcMessageError, RpcMessageKind, RpcResponse};
use io::{read_loop, write_loop};
pub use jsonl::{DEFAULT_MAX_MESSAGE_BYTES, JsonlError, JsonlReader, JsonlWriter};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, broadcast, mpsc, oneshot, watch},
};
use tokio_util::sync::{CancellationToken, DropGuard};

const QUEUE_CAPACITY: usize = 4096;

pub use agent_protocol::error::{Delivery, PeerError};
/// Position in the external provider’s JSONL stream.
#[derive(Clone, Debug)]
pub struct Reply<T> {
    pub sequence: u64,
    pub value: T,
}
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// Notifications and server requests retain their complete envelopes.
    Message(Reply<Arc<str>>),
    /// Provider adapters can wait until preceding stdio events have been processed.
    Response {
        sequence: u64,
        request_id: Option<u64>,
        method: Option<Arc<str>>,
    },
    Closed(String),
}
struct Outbound {
    line: String,
    written: oneshot::Sender<()>,
}
struct Pending {
    original_id: Option<String>,
    method: Arc<str>,
    complete: oneshot::Sender<Result<Reply<String>, PeerError>>,
}
struct State {
    sequence: u64,
    pending: HashMap<u64, Pending>,
    closed: Option<String>,
    events: Option<broadcast::Sender<PeerEvent>>,
    initial: Option<broadcast::Receiver<PeerEvent>>,
}
type WriterStatus = Option<Result<(), String>>;

pub struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<Outbound>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    request_timeout: Option<Duration>,
    writer_done: watch::Receiver<WriterStatus>,
}

impl RpcPeer {
    fn new(
        timeout: Option<Duration>,
        max_requests: usize,
        outbound: mpsc::Sender<Outbound>,
    ) -> Result<(Self, watch::Sender<WriterStatus>), PeerError> {
        if max_requests == 0 {
            return Err(invalid("max_requests must be positive"));
        }
        let (events, initial) = broadcast::channel(QUEUE_CAPACITY);
        let (finished, writer_done) = watch::channel(None);
        let stop = CancellationToken::new();
        Ok((
            Self {
                next_id: AtomicU64::new(1),
                outbound,
                state: Arc::new(Mutex::new(State {
                    sequence: 0,
                    pending: HashMap::new(),
                    closed: None,
                    events: Some(events),
                    initial: Some(initial),
                })),
                permits: Arc::new(Semaphore::new(max_requests)),
                _close_on_drop: stop.clone().drop_guard(),
                stop,
                request_timeout: timeout,
                writer_done,
            },
            finished,
        ))
    }

    pub fn open<R, W>(
        reader: JsonlReader<R>,
        writer: W,
        request_timeout: Option<Duration>,
        max_requests: usize,
    ) -> Result<Self, PeerError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let maximum = reader.max_message_bytes();
        if maximum == 0 {
            return Err(PeerError::InvalidMessage(
                "max_message_bytes must be positive".into(),
            ));
        }
        let (outbound, outgoing) = mpsc::channel(QUEUE_CAPACITY);
        let (peer, writer_finished) = Self::new(request_timeout, max_requests, outbound)?;
        tokio::spawn(read_loop(
            reader,
            peer.state.clone(),
            peer.permits.clone(),
            peer.stop.clone(),
        ));
        tokio::spawn(write_loop(
            writer,
            outgoing,
            peer.state.clone(),
            peer.permits.clone(),
            peer.stop.clone(),
            maximum,
            writer_finished,
        ));
        Ok(peer)
    }
    /// Lag is explicit through `RecvError::Lagged`; consumers must refresh state.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        let mut state = self.state.lock().unwrap();
        if let Some(initial) = state.initial.take() {
            return initial;
        }
        if let Some(events) = &state.events {
            return events.subscribe();
        }
        let (sender, receiver) = broadcast::channel(1);
        drop(sender);
        receiver
    }
    /// Stops new work and waits for the writer's transport shutdown.
    pub async fn close(&self) -> Result<(), PeerError> {
        self.stop.cancel();
        let mut done = self.writer_done.clone();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result.map_err(PeerError::ConnectionClosed);
            }
            done.changed()
                .await
                .map_err(|_| PeerError::ConnectionClosed("writer task stopped".into()))?;
        }
    }

    pub async fn send_raw(&self, line: impl Into<String>) -> Result<(), PeerError> {
        let line = line.into();
        if RpcMessage::parse(&line).map_err(invalid)?.kind() == RpcMessageKind::Request {
            return Err(invalid("raw requests must go through request_raw"));
        }
        self.enqueue(line).await
    }
    pub async fn respond_raw(
        &self,
        id: &str,
        field: &'static str,
        payload: &str,
    ) -> Result<(), PeerError> {
        self.send_raw(&response_line(id, field, payload)?).await
    }
    async fn enqueue(&self, line: String) -> Result<(), PeerError> {
        if self.stop.is_cancelled() {
            return Err(self.closed_error());
        }
        let (written, completed) = oneshot::channel();
        self.outbound
            .send(Outbound { line, written })
            .await
            .map_err(|_| self.closed_error())?;
        tokio::select! {
            result = completed => result.map_err(|_| self.closed_error()),
            _ = self.stop.cancelled() => Err(self.closed_error()),
        }
    }
    fn closed_error(&self) -> PeerError {
        PeerError::ConnectionClosed(
            self.state
                .lock()
                .unwrap()
                .closed
                .clone()
                .unwrap_or_else(|| "peer is closed".into()),
        )
    }
}

fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}
