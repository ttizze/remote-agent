//! Client connections: outbound byte budgets, delivery and connection-wide notifications.
use super::*;

pub(in crate::host_rpc) const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
pub(in crate::host_rpc) fn frame_cost(capacity: usize) -> usize {
    capacity + std::mem::size_of::<Vec<u8>>()
}
pub struct HostSession {
    id: SessionId,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    pub(super) queued_bytes: Arc<AtomicUsize>,
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
            close_session(&state, self.id);
        }
    }
}
#[derive(Clone)]
pub(in crate::host_rpc) struct Outbound {
    pub(super) principal: String,
    pub(super) sender: mpsc::UnboundedSender<Vec<u8>>,
    pub(super) bytes: Arc<AtomicUsize>,
    pub(super) alive: Arc<AtomicBool>,
}
impl Outbound {
    pub(super) fn try_send(
        &self,
        session: SessionId,
        frame: Vec<u8>,
    ) -> Result<(), mpsc::error::TrySendError<Vec<u8>>> {
        if !self.alive.load(Relaxed) {
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
            tracing::warn!(target: "agent", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=byte_limit queued_bytes={} frame_bytes={} frame_cost={cost} limit_bytes={MAX_QUEUED_BYTES}", self.bytes.load(Relaxed), frame.len()));
            return Err(mpsc::error::TrySendError::Full(frame));
        }
        self.sender.send(frame).map_err(|error| {
            self.bytes.fetch_sub(cost, Relaxed);
            tracing::warn!(target: "agent", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=receiver_closed frame_bytes={}", error.0.len()));
            mpsc::error::TrySendError::Closed(error.0)
        })
    }
}

fn close_session(state: &Mutex<State>, session: SessionId) {
    if let Some(connection) = lock_state(state).sessions.remove(&session) {
        connection.alive.store(false, Relaxed);
    }
}

impl SessionRouter {
    pub(in crate::host_rpc) fn revoke_device(&self, principal: &str) {
        let activity = lock_state(&self.activity);
        let state = lock_state(&self.state);
        for output in state
            .sessions
            .values()
            .filter(|output| output.principal == principal)
        {
            output.alive.store(false, Relaxed);
        }
        if let Some(apns) = activity.as_ref() {
            apns.revoke(principal);
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
                alive: Arc::new(AtomicBool::new(true)),
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
    pub(in crate::host_rpc) fn queue_budget(
        &self,
        session: SessionId,
    ) -> Result<Arc<AtomicUsize>, String> {
        self.connection(session)
            .map(|outbound| outbound.bytes)
            .ok_or_else(|| "connection is closed".into())
    }
    pub(super) fn connection(&self, session: SessionId) -> Option<Outbound> {
        lock_state(&self.state)
            .sessions
            .get(&session)
            .filter(|output| output.alive.load(Relaxed))
            .cloned()
    }
    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), String> {
        self.connection(session)
            .map(|_| ())
            .ok_or_else(|| "connection is closed".into())
    }
    pub(crate) fn close_session(&self, session: SessionId) {
        close_session(&self.state, session);
    }
    pub(super) fn close_failed(&self, failed: Vec<SessionId>) {
        for session in failed {
            self.close_session(session);
        }
    }
    pub(crate) fn send(
        &self,
        session: SessionId,
        notification: Notification,
    ) -> Result<(), String> {
        self.send_frame(
            session,
            protocol::encode(notification).map_err(|e| e.to_string())?,
        )
    }
    pub(super) fn send_frame(&self, session: SessionId, frame: Vec<u8>) -> Result<(), String> {
        let output = self
            .connection(session)
            .ok_or_else(|| format!("RPC session {session} is not open"))?;
        output.try_send(session, frame).map_err(|_| {
            self.close_session(session);
            format!("RPC session {session} outbound queue failed")
        })
    }
    pub(super) fn broadcast_frames(&self, frame: Vec<u8>) -> Vec<SessionId> {
        let outputs: Vec<_> = lock_state(&self.state)
            .sessions
            .iter()
            .map(|(id, output)| (*id, output.clone()))
            .collect();
        outputs
            .into_iter()
            .filter_map(|(id, output)| output.try_send(id, frame.clone()).is_err().then_some(id))
            .collect()
    }
    pub(crate) fn broadcast(&self, notification: Notification) {
        let failed =
            self.broadcast_frames(protocol::encode(notification).expect("notification encodes"));
        self.close_failed(failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn byte_budget_disconnects_only_the_slow_connection() {
        let router = SessionRouter::new();
        let slow = router.open_session();
        let mut healthy = router.open_session();
        let capacity = MAX_QUEUED_BYTES - std::mem::size_of::<Vec<u8>>();
        router
            .send_frame(slow.id(), Vec::with_capacity(capacity))
            .unwrap();
        assert!(router.send_frame(slow.id(), vec![1]).is_err());
        assert!(router.ensure_session(slow.id()).is_err());
        router.send_frame(healthy.id(), vec![2]).unwrap();
        assert_eq!(healthy.recv().await, Some(vec![2]));
        assert!(router.ensure_session(healthy.id()).is_ok());
    }
}
