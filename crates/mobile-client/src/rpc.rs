use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use host_protocol::{
    RpcId, RpcMessage, RpcOutcome, RpcRequest, RpcResponse, read_frame, write_frame,
};
use quinn::{RecvStream, SendStream};
use serde_json::Value;
use tokio::{
    sync::{Semaphore, broadcast, mpsc, oneshot},
    time::timeout,
};

use crate::{MobileClientError, Notification, ServerRequest};

type PendingResponse = oneshot::Sender<Result<Value, MobileClientError>>;

type SharedState = Arc<StdMutex<PeerState>>;

struct PeerState {
    pending: HashMap<RpcId, PendingResponse>,
    terminal: Option<String>,
    notifications: Option<broadcast::Sender<Notification>>,
    server_requests: Option<broadcast::Sender<ServerRequest>>,
}

/// Correlates bidirectional RPC traffic on one authenticated QUIC stream.
///
/// Requests are bounded by both the Host-advertised in-flight limit and the
/// outbound queue. The request timeout covers waiting for both limits, sending
/// the frame, and receiving its response. A dropped request future removes its
/// correlation entry, so a cancelled request cannot retain state indefinitely.
pub(crate) struct RpcPeer {
    next_id: AtomicU64,
    outbound: mpsc::Sender<RpcMessage>,
    state: SharedState,
    permits: Arc<Semaphore>,
    request_timeout: Duration,
}

impl RpcPeer {
    pub(crate) fn open(
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
        let state = Arc::new(StdMutex::new(PeerState {
            pending: HashMap::new(),
            terminal: None,
            notifications: Some(notifications),
            server_requests: Some(server_requests),
        }));
        let permits = Arc::new(Semaphore::new(max_in_flight));
        tokio::spawn(write_loop(
            send,
            outbound_rx,
            state.clone(),
            permits.clone(),
            max_frame_bytes,
        ));
        tokio::spawn(read_loop(
            receive,
            state.clone(),
            permits.clone(),
            max_frame_bytes,
        ));

        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            state,
            permits,
            request_timeout,
        })
    }

    pub(crate) fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        notification_receiver(&self.state)
    }

    pub(crate) fn subscribe_server_requests(&self) -> broadcast::Receiver<ServerRequest> {
        server_request_receiver(&self.state)
    }

    pub(crate) async fn request(
        &self,
        method: String,
        params: Value,
    ) -> Result<Value, MobileClientError> {
        if let Some(reason) = terminal_reason(&self.state) {
            return Err(MobileClientError::Disconnected(reason));
        }
        let numeric_id = allocate_request_id(&self.next_id)?;
        let id = RpcId::Integer(numeric_id);
        let state = self.state.clone();
        let outbound = self.outbound.clone();
        let permits = self.permits.clone();
        let deadline = self.request_timeout;
        let operation = async move {
            let _permit = permits
                .acquire_owned()
                .await
                .map_err(|_| disconnected_or(&state, "request limiter closed"))?;

            let (tx, rx) = oneshot::channel();
            register_pending(&state, id.clone(), tx)?;
            let _registration = PendingRegistration {
                state: state.clone(),
                id: id.clone(),
            };

            ensure_active(&state)?;
            let message = RpcMessage::Request(RpcRequest {
                id,
                method,
                params,
                extensions: Default::default(),
            });
            outbound
                .send(message)
                .await
                .map_err(|_| disconnected_or(&state, "RPC writer stopped"))?;
            rx.await
                .map_err(|_| disconnected_or(&state, "RPC reader stopped"))?
        };

        timeout(deadline, operation)
            .await
            .map_err(|_| MobileClientError::RequestTimeout { id: numeric_id })?
    }

    pub(crate) async fn respond(
        &self,
        id: RpcId,
        outcome: RpcOutcome,
    ) -> Result<(), MobileClientError> {
        ensure_active(&self.state)?;
        self.outbound
            .send(RpcMessage::Response(RpcResponse {
                id,
                outcome,
                extensions: Default::default(),
            }))
            .await
            .map_err(|_| disconnected_or(&self.state, "RPC writer stopped"))
    }
}

/// Removes a pending request when the request future is cancelled or times
/// out. The reader may already have removed the same ID; in that case removal
/// is intentionally a no-op and the response has already won the race.
struct PendingRegistration {
    state: SharedState,
    id: RpcId,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        lock_state(&self.state).pending.remove(&self.id);
    }
}

fn lock_state(state: &SharedState) -> MutexGuard<'_, PeerState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn notification_receiver(state: &SharedState) -> broadcast::Receiver<Notification> {
    let state = lock_state(state);
    state
        .notifications
        .as_ref()
        .map(broadcast::Sender::subscribe)
        .unwrap_or_else(closed_receiver)
}

fn server_request_receiver(state: &SharedState) -> broadcast::Receiver<ServerRequest> {
    let state = lock_state(state);
    state
        .server_requests
        .as_ref()
        .map(broadcast::Sender::subscribe)
        .unwrap_or_else(closed_receiver)
}

fn closed_receiver<T: Clone>() -> broadcast::Receiver<T> {
    let (sender, receiver) = broadcast::channel(1);
    drop(sender);
    receiver
}

fn terminal_reason(state: &SharedState) -> Option<String> {
    lock_state(state).terminal.clone()
}

fn ensure_active(state: &SharedState) -> Result<(), MobileClientError> {
    terminal_reason(state).map_or(Ok(()), |reason| {
        Err(MobileClientError::Disconnected(reason))
    })
}

fn disconnected_or(state: &SharedState, fallback: &str) -> MobileClientError {
    terminal_reason(state).map_or_else(
        || MobileClientError::Disconnected(fallback.to_owned()),
        MobileClientError::Disconnected,
    )
}

fn register_pending(
    state: &SharedState,
    id: RpcId,
    response: PendingResponse,
) -> Result<(), MobileClientError> {
    let mut state = lock_state(state);
    if let Some(reason) = state.terminal.as_ref() {
        return Err(MobileClientError::Disconnected(reason.clone()));
    }
    state.pending.insert(id, response);
    Ok(())
}

fn allocate_request_id(next_id: &AtomicU64) -> Result<u64, MobileClientError> {
    let mut current = next_id.load(Ordering::Relaxed);
    loop {
        // `u64::MAX` is a permanent exhausted sentinel. In particular, do
        // not use fetch_add here: it would wrap to zero after exhaustion and
        // could eventually reuse an ID.
        if current == u64::MAX {
            return Err(MobileClientError::RequestIdExhausted);
        }
        match next_id.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}

async fn write_loop(
    mut send: SendStream,
    mut outbound: mpsc::Receiver<RpcMessage>,
    state: SharedState,
    permits: Arc<Semaphore>,
    max_frame_bytes: u32,
) {
    while let Some(message) = outbound.recv().await {
        if let Err(error) = write_frame(&mut send, &message, max_frame_bytes).await {
            terminate(&state, &permits, error.to_string());
            return;
        }
    }
    terminate(&state, &permits, "RPC writer stopped".to_owned());
}

async fn read_loop(
    mut receive: RecvStream,
    state: SharedState,
    permits: Arc<Semaphore>,
    max_frame_bytes: u32,
) {
    loop {
        let message: RpcMessage = match read_frame(&mut receive, max_frame_bytes).await {
            Ok(message) => message,
            Err(error) => {
                terminate(&state, &permits, error.to_string());
                return;
            }
        };
        match message {
            RpcMessage::Response(RpcResponse { id, outcome, .. }) => {
                let Some(tx) = lock_state(&state).pending.remove(&id) else {
                    continue; // Late or duplicate response after timeout.
                };
                let result = match outcome {
                    RpcOutcome::Success { result } => Ok(result),
                    RpcOutcome::Failure { error } => Err(error.into()),
                };
                let _ = tx.send(result);
            }
            RpcMessage::Notification(notification) => {
                let state = lock_state(&state);
                if let Some(sender) = state.notifications.as_ref() {
                    let _ = sender.send(notification);
                }
            }
            RpcMessage::Request(request) => {
                // Host-initiated requests are part of the normal
                // bidirectional protocol. Dropping a request when there is
                // no subscriber must not tear down unrelated in-flight work.
                let state = lock_state(&state);
                if let Some(sender) = state.server_requests.as_ref() {
                    let _ = sender.send(request);
                }
            }
        }
    }
}

fn terminate(state: &SharedState, permits: &Arc<Semaphore>, reason: String) {
    let (pending, notifications, server_requests) = {
        let mut state = lock_state(state);
        if state.terminal.is_some() {
            return;
        }
        state.terminal = Some(reason.clone());
        (
            state.pending.drain().map(|(_, tx)| tx).collect::<Vec<_>>(),
            state.notifications.take(),
            state.server_requests.take(),
        )
    };

    // Closing the semaphore wakes requests waiting for an in-flight permit;
    // they then observe the first terminal reason instead of timing out.
    permits.close();
    drop(notifications);
    drop(server_requests);
    for tx in pending {
        let _ = tx.send(Err(MobileClientError::Disconnected(reason.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_request_registration_is_removed() {
        let state = test_state();
        let id = RpcId::Integer(7);
        let (tx, _rx) = oneshot::channel();
        register_pending(&state, id.clone(), tx).unwrap();
        {
            let _registration = PendingRegistration {
                state: state.clone(),
                id,
            };
        }
        assert!(lock_state(&state).pending.is_empty());
    }

    #[test]
    fn registration_after_terminal_is_rejected_atomically() {
        let state = test_state();
        let permits = Arc::new(Semaphore::new(1));
        terminate(&state, &permits, "read side closed".to_owned());

        let (tx, _rx) = oneshot::channel();
        let result = register_pending(&state, RpcId::Integer(9), tx);
        assert!(matches!(
            result,
            Err(MobileClientError::Disconnected(reason)) if reason == "read side closed"
        ));
        assert!(lock_state(&state).pending.is_empty());
    }

    #[test]
    fn terminal_failure_closes_existing_and_new_event_receivers() {
        let state = test_state();
        let mut notifications = notification_receiver(&state);
        let mut server_requests = server_request_receiver(&state);
        let permits = Arc::new(Semaphore::new(1));

        terminate(&state, &permits, "writer failed".to_owned());

        assert!(matches!(
            notifications.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            server_requests.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            notification_receiver(&state).try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            server_request_receiver(&state).try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn request_ids_stop_at_exhaustion_without_wrapping_or_reusing() {
        let next_id = AtomicU64::new(u64::MAX - 1);
        assert!(matches!(
            allocate_request_id(&next_id),
            Ok(id) if id == u64::MAX - 1
        ));
        assert!(matches!(
            allocate_request_id(&next_id),
            Err(MobileClientError::RequestIdExhausted)
        ));
        assert_eq!(next_id.load(Ordering::Relaxed), u64::MAX);
        assert!(matches!(
            allocate_request_id(&next_id),
            Err(MobileClientError::RequestIdExhausted)
        ));
    }

    fn test_state() -> SharedState {
        let (notifications, _) = broadcast::channel(1);
        let (server_requests, _) = broadcast::channel(1);
        Arc::new(StdMutex::new(PeerState {
            pending: HashMap::new(),
            terminal: None,
            notifications: Some(notifications),
            server_requests: Some(server_requests),
        }))
    }
}
