use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::provider_events::notification_change;
use super::session_runtime::{ReadToken, SessionRuntime};
use agent_core::peer::{RpcMessage, RpcMessageKind};
use agent_core::{
    models::ThreadResponse,
    session::{OpenSession, ProviderKind, SessionRef},
};
use serde_json::Value;
use tokio::sync::mpsc;

pub(super) struct SessionRead {
    router: SessionRouter,
    token: Option<ReadToken>,
    pub(super) cached: Option<ThreadResponse>,
    pub(super) limit: usize,
}
impl Drop for SessionRead {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            lock_state(&self.router.state)
                .conversations
                .cancel_read(token);
        }
    }
}

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is never put on the wire.
pub type SessionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    Forward { provider: ProviderKind, id: String },
    Unknown,
}

/// A live authenticated session's bounded outbound queue.
pub struct HostSession {
    id: SessionId,
    receiver: mpsc::Receiver<String>,
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    state: Weak<Mutex<State>>,
}

impl HostSession {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<String> {
        let line = self.receiver.recv().await?;
        self.queued_bytes
            .fetch_sub(line.len(), std::sync::atomic::Ordering::Relaxed);
        Some(line)
    }
}

impl fmt::Debug for HostSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        close_session_state(&self.state, self.id);
    }
}

/// Owns bounded connection queues and the shared current session state.
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
}

const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone)]
struct Outbound {
    sender: mpsc::Sender<String>,
    bytes: Arc<std::sync::atomic::AtomicUsize>,
}
impl Outbound {
    fn try_send(&self, line: String) -> Result<(), mpsc::error::TrySendError<String>> {
        use std::sync::atomic::Ordering::Relaxed;
        let length = line.len();
        if self
            .bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(length)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            return Err(mpsc::error::TrySendError::Full(line));
        }
        self.sender.try_send(line).inspect_err(|_| {
            self.bytes.fetch_sub(length, Relaxed);
        })
    }
}

struct State {
    next_session_id: SessionId,
    sessions: HashMap<SessionId, Outbound>,
    conversations: SessionRuntime,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            sessions: HashMap::new(),
            conversations: SessionRuntime::default(),
        }
    }
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum Delivery {
    Send(SessionId, String),
}

impl SessionRouter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub(crate) fn open_session(&self, capacity: usize) -> HostSession {
        assert!(capacity > 0, "a Host session queue must have capacity");
        let (sender, receiver) = mpsc::channel(capacity);
        let mut state = lock_state(&self.state);
        let id = allocate_session_id(&mut state);
        let queued_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        state.sessions.insert(
            id,
            Outbound {
                sender,
                bytes: queued_bytes.clone(),
            },
        );

        HostSession {
            id,
            receiver,
            queued_bytes,
            state: Arc::downgrade(&self.state),
        }
    }

    pub(super) fn submission(
        &self,
        target: &SessionRef,
        id: &str,
        pending: bool,
    ) -> Result<(), String> {
        lock_state(&self.state)
            .conversations
            .submission(target, id, pending)
            .map_err(str::to_owned)
    }

    pub(crate) fn current_session(&self, id: &str) -> Option<ThreadResponse> {
        lock_state(&self.state)
            .conversations
            .current(&SessionRef::from_thread_id(id).ok()?)
    }

    pub(crate) fn created_session(&self, response: ThreadResponse) {
        // Preserve the native ID in the creation response even when capacity
        // is exhausted. Never turn an accepted creation into a retryable send.
        let _ = lock_state(&self.state).conversations.created(response);
    }

    pub(super) fn begin_session_read(&self, params: OpenSession) -> Result<SessionRead, String> {
        let mut state = lock_state(&self.state);
        let token = state
            .conversations
            .begin_read(params.session, params.limit)?;
        let cached = state.conversations.cached(&token);
        Ok(SessionRead {
            router: self.clone(),
            limit: token.limit,
            token: Some(token),
            cached,
        })
    }

    pub(super) fn finish_session_read(
        &self,
        mut read: SessionRead,
        session: SessionId,
        request: &RpcMessage<'_>,
        response: ThreadResponse,
    ) -> Result<(), String> {
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) {
            return Err("connection closed during session open".into());
        }
        let result = state.conversations.finish_read(
            read.token.take().expect("open read"),
            session,
            response,
        );
        let line = match result {
            Ok(opened) => request.response::<_, ()>(Ok(opened)),
            Err(error) => request.error("session_open_failed", &error),
        }
        .map_err(|error| error.to_string())?;
        deliver_locked(&mut state, vec![Delivery::Send(session, line)]);
        Ok(())
    }

    pub(super) fn close_subscription(&self, session: SessionId, subscription: uuid::Uuid) {
        lock_state(&self.state)
            .conversations
            .close(session, subscription);
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        remove_session_locked(&mut lock_state(&self.state), session);
    }

    /// Resolve only requests belonging to the stopped Codex backend. Claude
    /// approvals and authenticated client connections remain live.
    pub(crate) fn resolve_codex_requests(&self) {
        let mut state = lock_state(&self.state);
        let ids = state.conversations.pending_ids();
        for id in ids {
            if let Some((target, _)) = state.conversations.pending_request(&id)
                && target.provider == agent_core::session::ProviderKind::Codex
            {
                change_locked(
                    &mut state,
                    &target,
                    &agent_core::session::SessionChange::ResolveRequest { request_id: id },
                );
            }
        }
    }

    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), String> {
        if lock_state(&self.state).sessions.contains_key(&session) {
            Ok(())
        } else {
            Err(format!("RPC session {session} is not open"))
        }
    }

    pub(crate) fn send_line(&self, session: SessionId, line: String) -> Result<(), String> {
        let mut state = lock_state(&self.state);
        let sender = state
            .sessions
            .get(&session)
            .cloned()
            .ok_or_else(|| format!("RPC session {session} is not open"))?;
        match sender.try_send(line) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                remove_session_locked(&mut state, session);
                Err(format!("RPC session {session} outbound queue is full"))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                remove_session_locked(&mut state, session);
                Err(format!("RPC session {session} is not open"))
            }
        }
    }

    /// Normalize provider output once; every client receives the same request
    /// identity and reads unresolved requests from its current session snapshot.
    pub(crate) fn request_admission(&self, message: &RpcMessage<'_>) -> Result<(), String> {
        if message.line().len() > 64 * 1024 {
            return Err("provider request exceeds 64 KiB; execution cannot be approved".into());
        }
        let params: Value = message.params().map_err(|error| error.to_string())?;
        let target = SessionRef::from_thread_id(
            params["threadId"]
                .as_str()
                .ok_or("request session ID is missing")?,
        )
        .map_err(str::to_owned)?;
        let state = lock_state(&self.state);
        let current = state
            .conversations
            .current(&target)
            .ok_or("request has no owned current session")?;
        if current.thread.requests.len() >= 32 {
            return Err("session pending request capacity reached".into());
        }
        Ok(())
    }

    pub(crate) fn session_change(&self, id: &str, change: agent_core::session::SessionChange) {
        if let Ok(target) = SessionRef::from_thread_id(id) {
            change_locked(&mut lock_state(&self.state), &target, &change);
        }
    }

    pub(crate) fn resolve_native_request(&self, provider: ProviderKind, id: &Value) {
        let mut state = lock_state(&self.state);
        if let Some((target, request)) = state.conversations.pending_native(provider, id) {
            change_locked(
                &mut state,
                &target,
                &agent_core::session::SessionChange::ResolveRequest {
                    request_id: request.id.to_string(),
                },
            );
        }
    }

    pub(crate) fn handle_server_message(&self, provider: ProviderKind, message: &RpcMessage<'_>) {
        let params = message.params::<Value>().unwrap_or(Value::Null);
        if message.kind() == RpcMessageKind::Notification
            && message.method() == Some("serverRequest/resolved")
        {
            self.resolve_native_request(provider, &params["requestId"]);
            return;
        }
        let mut state = lock_state(&self.state);
        if message.kind() == RpcMessageKind::Request {
            let Some(target) = params["threadId"]
                .as_str()
                .and_then(|id| SessionRef::from_thread_id(id).ok())
            else {
                return;
            };
            if target.provider != provider {
                return;
            }
            let Ok(mut request) =
                serde_json::from_str::<agent_core::client::ServerRequest>(message.line())
            else {
                return;
            };
            if state
                .conversations
                .pending_native(provider, &request.id)
                .is_some()
            {
                return;
            }
            request
                .extra
                .insert("nativeRequestId".into(), request.id.clone());
            request.id = Value::String(format!("request:{}", uuid::Uuid::new_v4()));
            change_locked(
                &mut state,
                &target,
                &agent_core::session::SessionChange::Request { request },
            );
            return;
        }
        if message.kind() == RpcMessageKind::Notification {
            let target = params["threadId"]
                .as_str()
                .and_then(|id| SessionRef::from_thread_id(id).ok());
            match notification_change(message.method().unwrap_or_default(), params) {
                Ok(Some((id, change))) => {
                    if let Ok(target) = SessionRef::from_thread_id(&id)
                        && target.provider == provider
                    {
                        change_locked(&mut state, &target, &change);
                    }
                    return;
                }
                Err(_) => {
                    for connection in state.conversations.invalidate(target.as_ref(), true) {
                        remove_session_locked(&mut state, connection);
                    }
                    return;
                }
                Ok(None) => {}
            }
        }
        let deliveries = state
            .sessions
            .keys()
            .map(|id| Delivery::Send(*id, message.line().to_owned()))
            .collect();
        deliver_locked(&mut state, deliveries);
    }

    pub(super) fn request_session(&self, id: &str) -> Option<SessionRef> {
        lock_state(&self.state)
            .conversations
            .pending_request(id)
            .map(|(session, _)| session)
    }

    pub(crate) fn claim_response(
        &self,
        session: SessionId,
        id: &str,
        result: &Value,
    ) -> Result<ResponseRoute, String> {
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) {
            return Err("connection is closed".into());
        }
        let Some((target, request)) = state.conversations.pending_request(id) else {
            return Ok(ResponseRoute::Unknown);
        };
        if request
            .extra
            .get("deliveryState")
            .is_some_and(|state| state != "awaiting")
        {
            return Ok(ResponseRoute::Unknown);
        }
        let current = state
            .conversations
            .current(&target)
            .ok_or("request execution is unavailable")?;
        let live_turn = current.thread.turns.iter().flatten().any(|turn| {
            turn.status.as_deref() == Some("inProgress")
                && request
                    .params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .is_none_or(|id| id == turn.id)
        });
        if !live_turn {
            return Err("request execution has ended".into());
        }
        agent_core::client::validate_answer(&request, result)?;
        let native = request
            .extra
            .get("nativeRequestId")
            .ok_or("request execution is unavailable")?
            .to_string();
        change_locked(
            &mut state,
            &target,
            &agent_core::session::SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_core::session::RequestDelivery::Sending,
            },
        );
        Ok(ResponseRoute::Forward {
            provider: target.provider,
            id: native,
        })
    }

    pub(crate) fn response_unknown(&self, id: &str) {
        let mut state = lock_state(&self.state);
        if let Some((target, _)) = state.conversations.pending_request(id) {
            change_locked(
                &mut state,
                &target,
                &agent_core::session::SessionChange::RequestDelivery {
                    request_id: id.into(),
                    state: agent_core::session::RequestDelivery::Unknown,
                },
            );
        }
    }
}

fn change_locked(
    state: &mut State,
    target: &SessionRef,
    change: &agent_core::session::SessionChange,
) {
    match state.conversations.update(target, change) {
        Ok(updates) => {
            let deliveries = updates
                .into_iter()
                .map(|(session, update)| {
                    Delivery::Send(
                        session,
                        serde_json::json!({"method":"host/session/update", "params":update})
                            .to_string(),
                    )
                })
                .collect();
            deliver_locked(state, deliveries);
        }
        Err(_) => {
            for connection in state.conversations.invalidate(Some(target), false) {
                remove_session_locked(state, connection);
            }
        }
    }
    // Background navigation needs activity, not copies of
    // provider turn/item payloads outside a subscription.
    let active = match change {
        agent_core::session::SessionChange::Status { status } => Some(status.kind == "active"),
        agent_core::session::SessionChange::Turn { .. } => {
            state.conversations.current(target).map(|response| {
                response
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.status.as_deref() == Some("inProgress"))
            })
        }
        _ => None,
    };
    if let Some(active) = active {
        let line = serde_json::json!({"method":"host/session/activity", "params":{"session":target,"active":active,"finished":!active && matches!(change, agent_core::session::SessionChange::Turn {completed:true, turn} if turn.status.as_deref() == Some("completed"))}}).to_string();
        let deliveries = state
            .sessions
            .keys()
            .map(|id| Delivery::Send(*id, line.clone()))
            .collect();
        deliver_locked(state, deliveries);
    }
}

fn deliver_locked(state: &mut State, deliveries: Vec<Delivery>) {
    // Keep transitions, delivery, and failure cleanup under the same lock.
    // A concurrent response cannot observe aliases for a failed delivery.
    for delivery in deliveries {
        let failed = match delivery {
            Delivery::Send(session, line) => state
                .sessions
                .get(&session)
                .map(|sender| sender.try_send(line).is_err())
                .unwrap_or(true)
                .then_some(session),
        };
        if let Some(session) = failed {
            remove_session_locked(state, session);
        }
    }
}

fn allocate_session_id(state: &mut State) -> SessionId {
    loop {
        let id = state.next_session_id;
        state.next_session_id = state.next_session_id.checked_add(1).unwrap_or(1);
        if !state.sessions.contains_key(&id) {
            return id;
        }
    }
}

fn lock_state(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn close_session_state(state: &Weak<Mutex<State>>, session: SessionId) {
    if let Some(state) = state.upgrade() {
        remove_session_locked(&mut lock_state(&state), session);
    }
}

fn remove_session_locked(state: &mut State, session: SessionId) {
    state.sessions.remove(&session);
    state.conversations.disconnect(session);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn byte_budget_disconnects_only_the_slow_connection() {
        let router = SessionRouter::new();
        let slow = router.open_session(100);
        let mut healthy = router.open_session(100);
        router
            .send_line(slow.id(), "x".repeat(MAX_QUEUED_BYTES))
            .unwrap();
        assert!(router.send_line(slow.id(), "overflow".into()).is_err());
        router.send_line(healthy.id(), "current".into()).unwrap();
        assert_eq!(healthy.recv().await.as_deref(), Some("current"));
    }

    #[test]
    fn slow_session_subscribers_do_not_block_other_devices_or_destroy_current_state() {
        let router = SessionRouter::new();
        let mut slow = router.open_session(1);
        let mut healthy = router.open_session(8);
        let response: ThreadResponse =
            serde_json::from_value(serde_json::json!({"thread":{"id":"native","turns":[]}}))
                .unwrap();
        let request =
            RpcMessage::parse(r#"{"id":1,"method":"host/session/open","params":{}}"#).unwrap();
        for connection in [slow.id(), healthy.id()] {
            let read = router
                .begin_session_read(OpenSession {
                    session: SessionRef::from_thread_id("native").unwrap(),
                    limit: 5,
                })
                .unwrap();
            router
                .finish_session_read(read, connection, &request, response.clone())
                .unwrap();
        }
        let notification = RpcMessage::parse(r#"{"method":"thread/status/changed","params":{"threadId":"native","status":{"type":"active"}}}"#).unwrap();
        router.handle_server_message(ProviderKind::Codex, &notification);
        assert!(router.ensure_session(slow.id()).is_err());
        assert!(router.ensure_session(healthy.id()).is_ok());
        let initial: Value = serde_json::from_str(&healthy.receiver.try_recv().unwrap()).unwrap();
        let update: Value = serde_json::from_str(&healthy.receiver.try_recv().unwrap()).unwrap();
        assert_eq!(initial["id"], 1);
        assert_eq!(update["method"], "host/session/update");
        assert_eq!(
            update["params"]["subscriptionId"],
            initial["result"]["subscriptionId"]
        );
        assert!(slow.receiver.try_recv().is_ok());
        assert!(matches!(
            slow.receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
        let mut read = router
            .begin_session_read(OpenSession {
                session: SessionRef::from_thread_id("native").unwrap(),
                limit: 5,
            })
            .unwrap();
        assert_eq!(
            read.cached.take().unwrap().thread.status.unwrap().kind,
            "active"
        );
    }

    #[test]
    fn cancellation_and_disconnect_release_an_unfinished_open() {
        let router = SessionRouter::new();
        let connection = router.open_session(4);
        let read = router
            .begin_session_read(OpenSession {
                session: SessionRef::from_thread_id("native").unwrap(),
                limit: 5,
            })
            .unwrap();
        drop(connection);
        drop(read);
        // Fill and evict idle entries: leaked readers would eventually prevent admission.
        for id in 0..200 {
            let read = router
                .begin_session_read(OpenSession {
                    session: SessionRef::from_thread_id(&id.to_string()).unwrap(),
                    limit: 5,
                })
                .unwrap();
            drop(read);
        }
    }
}
#[test]
fn identical_native_request_ids_keep_their_provider_owner() {
    let router = SessionRouter::new();
    let connection = router.open_session(32);
    let open = RpcMessage::parse(r#"{"id":1,"method":"host/session/open","params":{}}"#).unwrap();
    let native = serde_json::json!("claude-permission:shared-native-id");
    let mut ids = Vec::new();
    for thread in ["codex-native", "claude:claude-native"] {
        let target = SessionRef::from_thread_id(thread).unwrap();
        let read = router
            .begin_session_read(OpenSession {
                session: target.clone(),
                limit: 5,
            })
            .unwrap();
        let response = serde_json::from_value(serde_json::json!({"thread":{"id":thread,
                "turns":[{"id":"turn","status":"inProgress","items":[]}]}}))
        .unwrap();
        router
            .finish_session_read(read, connection.id(), &open, response)
            .unwrap();
        let line = serde_json::json!({"id":native,"method":"item/commandExecution/requestApproval",
                "params":{"threadId":thread,"turnId":"turn","availableDecisions":["accept","decline"]}}).to_string();
        router.handle_server_message(target.provider, &RpcMessage::parse(&line).unwrap());
        let (_, request) = lock_state(&router.state)
            .conversations
            .pending_native(target.provider, &native)
            .unwrap();
        ids.push(request.id.to_string());
        assert_eq!(
            router
                .claim_response(
                    connection.id(),
                    ids.last().unwrap(),
                    &serde_json::json!({"decision":"accept"})
                )
                .unwrap(),
            ResponseRoute::Forward {
                provider: target.provider,
                id: native.to_string()
            }
        );
    }
    let not_a_notification =
        serde_json::json!({"id":1,"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    router.handle_server_message(
        ProviderKind::Codex,
        &RpcMessage::parse(&not_a_notification).unwrap(),
    );
    assert!(router.request_session(&ids[0]).is_some());
    let resolved =
        serde_json::json!({"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    router.handle_server_message(ProviderKind::Codex, &RpcMessage::parse(&resolved).unwrap());
    assert!(router.request_session(&ids[0]).is_none());
    assert_eq!(
        router.request_session(&ids[1]).unwrap().provider,
        ProviderKind::Claude
    );
}
