//! Authenticated connection ownership, independent of conversation state.
use agent_protocol::protocol::{self, Notification};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
pub type SessionId = u64;
const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
fn frame_cost(capacity: usize) -> usize {
    capacity + std::mem::size_of::<Vec<u8>>()
}
fn lock_state<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}
pub struct HostSession {
    id: SessionId,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued_bytes: Arc<AtomicUsize>,
    state: Weak<Mutex<State>>,
}
impl HostSession {
    pub fn id(&self) -> SessionId {
        self.id
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let frame = self.receiver.recv().await?;
        self.queued_bytes
            .fetch_sub(frame_cost(frame.capacity()), Relaxed);
        Some(frame)
    }
}
impl fmt::Debug for HostSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Drop for HostSession {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            Connections { state }.close_session(self.id);
        }
    }
}

#[derive(Clone)]
struct Outbound {
    principal: String,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    bytes: Arc<AtomicUsize>,
    stopped: CancellationToken,
}
impl Outbound {
    fn try_send(
        &self,
        session: SessionId,
        frame: Vec<u8>,
    ) -> Result<(), mpsc::error::TrySendError<Vec<u8>>> {
        if self.stopped.is_cancelled() {
            return Err(mpsc::error::TrySendError::Closed(frame));
        }
        let cost = frame_cost(frame.capacity());
        if self
            .bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=byte_limit queued_bytes={} frame_bytes={} frame_cost={cost} limit_bytes={MAX_QUEUED_BYTES}", self.bytes.load(Relaxed), frame.len()));
            return Err(mpsc::error::TrySendError::Full(frame));
        }
        self.sender.send(frame).map_err(|error| {
            self.bytes.fetch_sub(cost, Relaxed);
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=receiver_closed frame_bytes={}", error.0.len()));
            mpsc::error::TrySendError::Closed(error.0)
        })
    }
}

#[derive(Default)]
struct State {
    next_session_id: SessionId,
    sessions: HashMap<SessionId, Outbound>,
}
#[derive(Clone)]
pub(crate) struct Connections {
    state: Arc<Mutex<State>>,
}
impl Connections {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                next_session_id: 1,
                ..Default::default()
            })),
        }
    }
    pub(crate) fn open_session(&self) -> HostSession {
        self.open_authenticated_session(None)
    }
    pub(crate) fn open_authenticated_session(&self, principal: Option<String>) -> HostSession {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = lock_state(&self.state);
        let id = loop {
            let id = state.next_session_id;
            state.next_session_id = state.next_session_id.checked_add(1).unwrap_or(1);
            if !state.sessions.contains_key(&id) {
                break id;
            }
        };
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        state.sessions.insert(
            id,
            Outbound {
                principal: principal.unwrap_or_else(|| format!("session:{id}")),
                sender,
                bytes: queued_bytes.clone(),
                stopped: CancellationToken::new(),
            },
        );
        HostSession {
            id,
            receiver,
            queued_bytes,
            state: Arc::downgrade(&self.state),
        }
    }
    pub(crate) fn principal(&self, session: SessionId) -> Result<String, String> {
        self.connection(session)
            .map(|output| output.principal)
            .ok_or_else(|| "connection is closed".into())
    }
    fn connection(&self, session: SessionId) -> Option<Outbound> {
        lock_state(&self.state)
            .sessions
            .get(&session)
            .filter(|output| !output.stopped.is_cancelled())
            .cloned()
    }
    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), String> {
        self.connection(session)
            .map(|_| ())
            .ok_or_else(|| "connection is closed".into())
    }

    pub(crate) fn close_session(&self, id: SessionId) {
        if let Some(connection) = lock_state(&self.state).sessions.remove(&id) {
            connection.stopped.cancel();
        }
    }
    pub(crate) fn cancellation(&self, id: SessionId) -> Result<CancellationToken, String> {
        self.connection(id)
            .map(|connection| connection.stopped)
            .ok_or_else(|| "connection is closed".into())
    }
    pub(crate) fn send(&self, id: SessionId, notification: Notification) -> Result<(), String> {
        let output = self
            .connection(id)
            .ok_or_else(|| "connection is closed".to_string())?;
        let frame = protocol::encode(notification).map_err(|error| error.to_string())?;
        output.try_send(id, frame).map_err(|_| {
            self.close_session(id);
            "connection outbound queue failed".into()
        })
    }
}
/// A response stream owns and cancels its delivery task.
pub struct HostSubscription {
    pub(crate) receiver: mpsc::Receiver<Vec<u8>>,
    pub(crate) _task: tokio_util::task::AbortOnDropHandle<()>,
}
impl HostSubscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.receiver.recv().await
    }
}
pub struct HostReply {
    pub initial: Vec<u8>,
    pub updates: Option<HostSubscription>,
}
impl From<protocol::Response> for HostReply {
    fn from(response: protocol::Response) -> Self {
        Self {
            initial: protocol::response_frame(response).expect("response encodes"),
            updates: None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn connection_identity_delivery_and_close_have_no_conversation_owner() {
        let connections = Connections::new();
        let mut connection = connections.open_authenticated_session(Some("device".into()));
        assert_eq!(connections.principal(connection.id()).unwrap(), "device");
        let stopped = connections.cancellation(connection.id()).unwrap();
        connections
            .send(
                connection.id(),
                Notification::Exited {
                    handle: "terminal".into(),
                    code: 0,
                },
            )
            .unwrap();
        assert!(matches!(
            protocol::decode::<Notification>(&connection.recv().await.unwrap()).unwrap(),
            Notification::Exited { code: 0, .. }
        ));
        connections.close_session(connection.id());
        assert!(stopped.is_cancelled());
        assert!(connection.recv().await.is_none());
    }
    #[test]
    fn oversized_frame_cannot_increase_queued_bytes() {
        let connections = Connections::new();
        let connection = connections.open_session();
        let output = connections.connection(connection.id()).unwrap();
        assert!(
            output
                .try_send(connection.id(), vec![0; MAX_QUEUED_BYTES])
                .is_err()
        );
        assert_eq!(output.bytes.load(Relaxed), 0);
    }
}
