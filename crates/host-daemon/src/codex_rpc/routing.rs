use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use codex_app_server::{RequestId, ServerEvent};
use host_protocol::{RpcId, RpcMessage, RpcNotification, RpcRequest};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is intentionally not exposed on the wire.
pub type SessionId = u64;

/// The result of submitting a response to a Codex-originated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDisposition {
    /// This was the first response for the request and was forwarded to
    /// Codex.
    Accepted,
    /// The id was never routed to this session, or another session already
    /// resolved it and its aliases were removed.
    Unknown,
}

/// The result of resolving a response id against the in-process routing
/// table. The upstream id is returned only for the winner; all aliases are
/// removed before this value is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    Forward(RequestId),
    Unknown,
}

/// Errors produced while routing a message to an authenticated session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RouteError {
    #[error("RPC session {0} is not open")]
    UnknownSession(SessionId),
    #[error("RPC session {session} outbound queue is full")]
    QueueFull { session: SessionId },
}

/// A live authenticated session's bounded outbound queue.
///
/// Dropping the session unregisters it from the router and removes all of its
/// proxy aliases.
pub struct CodexSession {
    id: SessionId,
    receiver: Option<mpsc::Receiver<RpcMessage>>,
    state: Weak<Mutex<State>>,
}

impl CodexSession {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<RpcMessage> {
        self.receiver
            .as_mut()
            .expect("session receiver was already taken")
            .recv()
            .await
    }
}

impl fmt::Debug for CodexSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        close_session_state(&self.state, self.id);
    }
}

/// Owns the bounded session queues and the aliases used to fan out
/// Codex-originated requests.
///
/// The mutex is held only across synchronous state transitions and
/// `try_send` calls. No async operation is performed while it is held.
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
}

struct State {
    next_session_id: SessionId,
    next_proxy_id: u64,
    sessions: HashMap<SessionId, mpsc::Sender<RpcMessage>>,
    pending: HashMap<RequestId, PendingServerRequest>,
    proxy_to_upstream: HashMap<ProxyKey, RequestId>,
}

struct PendingServerRequest {
    method: String,
    params: Value,
    extensions: Map<String, Value>,
    /// One proxy id per session. A session may join after the request was
    /// created, so this is deliberately not a single global proxy id.
    proxies: HashMap<SessionId, RpcId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProxyKey {
    session: SessionId,
    id: RpcId,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            next_proxy_id: 1,
            sessions: HashMap::new(),
            pending: HashMap::new(),
            proxy_to_upstream: HashMap::new(),
        }
    }
}

impl SessionRouter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Open a session with a bounded outbound queue.
    pub(crate) fn open_session(&self, capacity: usize) -> CodexSession {
        assert!(capacity > 0, "a Codex session queue must have capacity");
        let (sender, receiver) = mpsc::channel(capacity);
        let id = {
            let mut state = lock_state(&self.state);
            let id = allocate_session_id(&mut state);
            state.sessions.insert(id, sender.clone());

            // Register each alias and enqueue its request while holding the
            // same short-lived lock. This makes joining a session atomic with
            // respect to a concurrent response or server event.
            let pending = state
                .pending
                .iter()
                .map(|(upstream_id, request)| {
                    (
                        upstream_id.clone(),
                        request.method.clone(),
                        request.params.clone(),
                        request.extensions.clone(),
                    )
                })
                .collect::<Vec<_>>();
            for (upstream_id, method, params, extensions) in pending {
                let proxy_id = allocate_proxy_id(&mut state);
                let message = RpcMessage::Request(RpcRequest {
                    id: proxy_id.clone(),
                    method,
                    params,
                    extensions,
                });
                if sender.try_send(message).is_err() {
                    remove_session_locked(&mut state, id);
                    break;
                }
                let pending_request = state
                    .pending
                    .get_mut(&upstream_id)
                    .expect("pending entry was copied while the state was locked");
                pending_request.proxies.insert(id, proxy_id.clone());
                state.proxy_to_upstream.insert(
                    ProxyKey {
                        session: id,
                        id: proxy_id,
                    },
                    upstream_id,
                );
            }
            id
        };

        CodexSession {
            id,
            receiver: Some(receiver),
            state: Arc::downgrade(&self.state),
        }
    }

    /// Unregister a session explicitly. Dropping [`CodexSession`] has the
    /// same effect while its receiver remains owned by the session.
    pub(crate) fn close_session(&self, session: SessionId) {
        remove_session_locked(&mut lock_state(&self.state), session);
    }

    /// Close all currently registered sessions while retaining unresolved
    /// upstream requests for a later session to replay.
    pub(crate) fn close_all(&self) {
        let mut state = lock_state(&self.state);
        state.sessions.clear();
        state.proxy_to_upstream.clear();
        for pending in state.pending.values_mut() {
            pending.proxies.clear();
        }
    }

    #[cfg(test)]
    fn session_count(&self) -> usize {
        lock_state(&self.state).sessions.len()
    }

    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), RouteError> {
        if lock_state(&self.state).sessions.contains_key(&session) {
            Ok(())
        } else {
            Err(RouteError::UnknownSession(session))
        }
    }

    /// Send one message to a session without ever exceeding its bounded
    /// queue. A full or closed queue unregisters the session and all aliases.
    pub(crate) fn send_message(
        &self,
        session: SessionId,
        message: RpcMessage,
    ) -> Result<(), RouteError> {
        let mut state = lock_state(&self.state);
        let sender = state
            .sessions
            .get(&session)
            .cloned()
            .ok_or(RouteError::UnknownSession(session))?;
        match sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                remove_session_locked(&mut state, session);
                Err(RouteError::QueueFull { session })
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                remove_session_locked(&mut state, session);
                Err(RouteError::UnknownSession(session))
            }
        }
    }

    /// Fan out a raw Codex event. Notifications are forwarded unchanged;
    /// server requests receive one unique string proxy id per session.
    pub(crate) fn handle_server_event(&self, event: ServerEvent) {
        let mut state = lock_state(&self.state);
        match event {
            ServerEvent::Notification {
                method,
                params,
                extensions,
            } => broadcast_message_locked(
                &mut state,
                RpcMessage::Notification(RpcNotification {
                    method,
                    params,
                    extensions,
                }),
            ),
            ServerEvent::Request {
                id,
                method,
                params,
                extensions,
            } => fanout_server_request_locked(&mut state, id, method, params, extensions),
        }
    }

    /// Atomically consume the alias for a session's response. The winning
    /// route carries the original Codex request id; every other alias is
    /// removed before the route is returned.
    pub(crate) fn resolve_response(&self, session: SessionId, id: RpcId) -> ResponseRoute {
        let mut state = lock_state(&self.state);
        let Some(upstream_id) = state
            .proxy_to_upstream
            .get(&ProxyKey { session, id })
            .cloned()
        else {
            return ResponseRoute::Unknown;
        };
        let Some(pending) = state.pending.remove(&upstream_id) else {
            return ResponseRoute::Unknown;
        };
        for (alias_session, proxy_id) in pending.proxies {
            state.proxy_to_upstream.remove(&ProxyKey {
                session: alias_session,
                id: proxy_id,
            });
        }
        ResponseRoute::Forward(upstream_id)
    }
}

fn fanout_server_request_locked(
    state: &mut State,
    upstream_id: RequestId,
    method: String,
    params: Value,
    extensions: Map<String, Value>,
) {
    if state.pending.contains_key(&upstream_id) {
        // Codex request ids are expected to be unique while pending. Keep the
        // first request if a broken server violates that contract.
        return;
    }

    let session_ids = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut pending = PendingServerRequest {
        method: method.clone(),
        params: params.clone(),
        extensions: extensions.clone(),
        proxies: HashMap::new(),
    };
    let mut failed_sessions = Vec::new();
    for session in session_ids {
        let proxy_id = allocate_proxy_id(state);
        let message = RpcMessage::Request(RpcRequest {
            id: proxy_id.clone(),
            method: method.clone(),
            params: params.clone(),
            extensions: extensions.clone(),
        });
        let Some(sender) = state.sessions.get(&session).cloned() else {
            failed_sessions.push(session);
            continue;
        };
        match sender.try_send(message) {
            Ok(()) => {
                pending.proxies.insert(session, proxy_id.clone());
                state.proxy_to_upstream.insert(
                    ProxyKey {
                        session,
                        id: proxy_id,
                    },
                    upstream_id.clone(),
                );
            }
            Err(_) => failed_sessions.push(session),
        }
    }
    for session in failed_sessions {
        remove_session_locked(state, session);
    }
    state.pending.insert(upstream_id, pending);
}

fn broadcast_message_locked(state: &mut State, message: RpcMessage) {
    let sessions = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut failed_sessions = Vec::new();
    for session in sessions {
        let Some(sender) = state.sessions.get(&session).cloned() else {
            continue;
        };
        if sender.try_send(message.clone()).is_err() {
            failed_sessions.push(session);
        }
    }
    for session in failed_sessions {
        remove_session_locked(state, session);
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

fn allocate_proxy_id(state: &mut State) -> RpcId {
    loop {
        let id = state.next_proxy_id;
        state.next_proxy_id = state.next_proxy_id.checked_add(1).unwrap_or(1);
        let candidate = RpcId::String(format!("host-proxy-{id}"));
        if !state
            .proxy_to_upstream
            .keys()
            .any(|key| key.id == candidate)
        {
            return candidate;
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
    for pending in state.pending.values_mut() {
        pending.proxies.remove(&session);
    }
    state
        .proxy_to_upstream
        .retain(|key, _| key.session != session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str) -> ServerEvent {
        ServerEvent::Request {
            id: RequestId::String(id.to_owned()),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({"command": "cargo test"}),
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        }
    }

    #[tokio::test]
    async fn proxy_ids_use_the_daemon_string_namespace() {
        let router = SessionRouter::new();
        let mut session = router.open_session(4);

        router.handle_server_event(request("codex-request-1"));
        let RpcMessage::Request(message) = session.recv().await.unwrap() else {
            panic!("expected a request")
        };
        assert_eq!(message.id, RpcId::String("host-proxy-1".to_owned()));
    }

    #[tokio::test]
    async fn unknown_notifications_are_fanned_out_losslessly() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        let notification = ServerEvent::Notification {
            method: "future/item/newKind".to_owned(),
            params: json!({"futureField": [1, {"nested": true}]}),
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        };

        router.handle_server_event(notification);

        let expected = RpcMessage::Notification(RpcNotification {
            method: "future/item/newKind".to_owned(),
            params: json!({"futureField": [1, {"nested": true}]}),
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        });
        assert_eq!(first.recv().await, Some(expected.clone()));
        assert_eq!(second.recv().await, Some(expected));
    }

    #[tokio::test]
    async fn sessions_get_distinct_proxies_for_one_server_request() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);

        router.handle_server_event(request("codex-request-1"));

        let RpcMessage::Request(first_request) = first.recv().await.unwrap() else {
            panic!("expected a request for the first session")
        };
        let RpcMessage::Request(second_request) = second.recv().await.unwrap() else {
            panic!("expected a request for the second session")
        };
        assert_ne!(first_request.id, second_request.id);
        assert!(matches!(first_request.id, RpcId::String(ref id) if id.starts_with("host-proxy-")));
        assert!(
            matches!(second_request.id, RpcId::String(ref id) if id.starts_with("host-proxy-"))
        );
    }

    #[tokio::test]
    async fn overflow_unregisters_session_and_removes_aliases() {
        let router = SessionRouter::new();
        let mut session = router.open_session(1);
        let session_id = session.id();

        router.handle_server_event(ServerEvent::Notification {
            method: "turn/started".to_owned(),
            params: json!({"turnId": "t1"}),
            extensions: Map::new(),
        });
        router.handle_server_event(ServerEvent::Notification {
            method: "turn/completed".to_owned(),
            params: json!({"turnId": "t1"}),
            extensions: Map::new(),
        });

        assert_eq!(router.session_count(), 0);
        assert_eq!(
            router.ensure_session(session_id),
            Err(RouteError::UnknownSession(session_id))
        );
        assert!(session.recv().await.is_some());
    }

    #[tokio::test]
    async fn a_late_session_replays_unresolved_requests_with_a_new_proxy() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let first_id = first.id();
        router.handle_server_event(request("codex-request-1"));
        let RpcMessage::Request(first_request) = first.recv().await.unwrap() else {
            panic!("expected the first request")
        };
        router.close_session(first_id);

        let mut late = router.open_session(4);
        let RpcMessage::Request(late_request) = late.recv().await.unwrap() else {
            panic!("expected the replayed request")
        };
        assert_ne!(first_request.id, late_request.id);
        assert_eq!(late_request.method, first_request.method);
        assert_eq!(late_request.params, first_request.params);
        assert_eq!(late_request.extensions, first_request.extensions);
        assert!(matches!(
            router.resolve_response(late.id(), late_request.id),
            ResponseRoute::Forward(RequestId::String(ref id)) if id == "codex-request-1"
        ));
    }

    #[tokio::test]
    async fn first_response_wins_and_removes_all_aliases() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        router.handle_server_event(request("codex-request-1"));

        let RpcMessage::Request(first_request) = first.recv().await.unwrap() else {
            panic!("expected the first request")
        };
        let RpcMessage::Request(second_request) = second.recv().await.unwrap() else {
            panic!("expected the second request")
        };
        assert!(matches!(
            router.resolve_response(first.id(), first_request.id.clone()),
            ResponseRoute::Forward(RequestId::String(ref id)) if id == "codex-request-1"
        ));
        assert_eq!(
            router.resolve_response(second.id(), second_request.id),
            ResponseRoute::Unknown
        );

        let mut late = router.open_session(4);
        assert!(matches!(
            late.receiver
                .as_mut()
                .expect("session receiver is present")
                .try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn dropping_a_session_unregisters_it() {
        let router = SessionRouter::new();
        let session = router.open_session(4);
        let id = session.id();
        assert_eq!(router.session_count(), 1);
        drop(session);
        assert_eq!(router.session_count(), 0);
        assert_eq!(
            router.ensure_session(id),
            Err(RouteError::UnknownSession(id))
        );
    }

    #[tokio::test]
    async fn duplicate_upstream_id_keeps_the_first_request() {
        let router = SessionRouter::new();
        let mut session = router.open_session(4);
        router.handle_server_event(request("codex-request-1"));
        router.handle_server_event(ServerEvent::Request {
            id: RequestId::String("codex-request-1".to_owned()),
            method: "different/method".to_owned(),
            params: json!({"different": true}),
            extensions: Map::new(),
        });

        let RpcMessage::Request(message) = session.recv().await.unwrap() else {
            panic!("expected the first request")
        };
        assert_eq!(message.method, "item/commandExecution/requestApproval");
        assert!(matches!(
            session
                .receiver
                .as_mut()
                .expect("session receiver is present")
                .try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn wrapped_counters_skip_ids_that_are_still_live() {
        let mut state = State::default();
        let (sender, _receiver) = mpsc::channel(1);
        state.sessions.insert(1, sender);
        state.next_session_id = u64::MAX;
        assert_eq!(allocate_session_id(&mut state), u64::MAX);
        assert_eq!(allocate_session_id(&mut state), 2);

        state.proxy_to_upstream.insert(
            ProxyKey {
                session: 1,
                id: RpcId::String("host-proxy-1".to_owned()),
            },
            RequestId::String("codex-request-live".to_owned()),
        );
        state.next_proxy_id = u64::MAX;
        assert_eq!(
            allocate_proxy_id(&mut state),
            RpcId::String(format!("host-proxy-{}", u64::MAX))
        );
        assert_eq!(
            allocate_proxy_id(&mut state),
            RpcId::String("host-proxy-2".to_owned())
        );
    }
}
