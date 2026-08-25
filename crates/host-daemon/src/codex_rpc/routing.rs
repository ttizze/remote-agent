use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use host_protocol::{RpcMessageKind, classify_message, rewrite_top_level_id};
use serde_json::Value;
use tokio::sync::mpsc;

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is never put on the wire.
pub type SessionId = u64;

/// The result of submitting a response to a Codex-originated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDisposition {
    Accepted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    Forward(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RouteError {
    #[error("RPC session {0} is not open")]
    UnknownSession(SessionId),
    #[error("RPC session {session} outbound queue is full")]
    QueueFull { session: SessionId },
}

/// A live authenticated session's bounded outbound queue.
pub struct CodexSession {
    id: SessionId,
    receiver: mpsc::Receiver<String>,
    state: Weak<Mutex<State>>,
}

impl CodexSession {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<String> {
        self.receiver.recv().await
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

/// Owns bounded session queues and aliases used for Codex server requests.
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
}

struct State {
    next_session_id: SessionId,
    next_proxy_id: u64,
    sessions: HashMap<SessionId, mpsc::Sender<String>>,
    pending: HashMap<String, PendingServerRequest>,
    proxy_to_upstream: HashMap<ProxyKey, String>,
}

struct PendingServerRequest {
    line: String,
    proxies: HashMap<SessionId, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProxyKey {
    session: SessionId,
    id: String,
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

    pub(crate) fn open_session(&self, capacity: usize) -> CodexSession {
        assert!(capacity > 0, "a Codex session queue must have capacity");
        let (sender, receiver) = mpsc::channel(capacity);
        let mut state = lock_state(&self.state);
        let id = allocate_session_id(&mut state);
        state.sessions.insert(id, sender.clone());

        // A request that was sent while no phone was connected remains
        // pending. Replay it to this new session with a fresh proxy id.
        let pending = state
            .pending
            .iter()
            .map(|(upstream_id, request)| (upstream_id.clone(), request.line.clone()))
            .collect::<Vec<_>>();
        for (upstream_id, line) in pending {
            let proxy_id = allocate_proxy_id(&mut state);
            let Ok(proxy_line) = rewrite_top_level_id(&line, &proxy_id) else {
                continue;
            };
            if sender.try_send(proxy_line).is_err() {
                remove_session_locked(&mut state, id);
                break;
            }
            if let Some(request) = state.pending.get_mut(&upstream_id) {
                request.proxies.insert(id, proxy_id.clone());
            }
            state.proxy_to_upstream.insert(
                ProxyKey {
                    session: id,
                    id: proxy_id,
                },
                upstream_id,
            );
        }

        CodexSession {
            id,
            receiver,
            state: Arc::downgrade(&self.state),
        }
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        remove_session_locked(&mut lock_state(&self.state), session);
    }

    /// Close phones but retain unresolved Codex requests for later replay.
    pub(crate) fn close_all(&self) {
        let mut state = lock_state(&self.state);
        state.sessions.clear();
        state.proxy_to_upstream.clear();
        for pending in state.pending.values_mut() {
            pending.proxies.clear();
        }
    }

    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), RouteError> {
        if lock_state(&self.state).sessions.contains_key(&session) {
            Ok(())
        } else {
            Err(RouteError::UnknownSession(session))
        }
    }

    pub(crate) fn send_line(&self, session: SessionId, line: String) -> Result<(), RouteError> {
        let mut state = lock_state(&self.state);
        let sender = state
            .sessions
            .get(&session)
            .cloned()
            .ok_or(RouteError::UnknownSession(session))?;
        match sender.try_send(line) {
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

    /// Fan out a raw Codex notification or server request. Notifications are
    /// sent unchanged; server requests get one unique id per phone.
    pub(crate) fn handle_server_line(&self, line: &str) {
        let Ok(message) = classify_message(line) else {
            return;
        };
        let mut state = lock_state(&self.state);
        match message.kind() {
            RpcMessageKind::Notification => {
                let resolved = resolved_server_request_id(line).is_some_and(|upstream_id| {
                    fanout_resolved_request_locked(&mut state, &upstream_id, line)
                });
                if !resolved {
                    broadcast_line_locked(&mut state, line);
                }
            }
            RpcMessageKind::Request => {
                fanout_request_locked(&mut state, message.raw_id().unwrap_or_default(), line)
            }
            RpcMessageKind::Response => {
                // Responses are consumed by CodexAppServer's own peer and are
                // not expected on its event broadcast.
            }
        }
    }

    /// First valid response wins. All aliases are removed before the caller
    /// forwards the response to Codex.
    pub(crate) fn resolve_response(&self, session: SessionId, id: &str) -> ResponseRoute {
        let mut state = lock_state(&self.state);
        let Some(upstream_id) = state.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: id.to_owned(),
        }) else {
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

fn fanout_request_locked(state: &mut State, upstream_id: &str, line: &str) {
    if state.pending.contains_key(upstream_id) {
        return;
    }
    let mut pending = PendingServerRequest {
        line: line.to_owned(),
        proxies: HashMap::new(),
    };
    let sessions = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut failed = Vec::new();
    for session in sessions {
        let proxy_id = allocate_proxy_id(state);
        let Ok(proxy_line) = rewrite_top_level_id(line, &proxy_id) else {
            failed.push(session);
            continue;
        };
        let Some(sender) = state.sessions.get(&session).cloned() else {
            failed.push(session);
            continue;
        };
        if sender.try_send(proxy_line).is_err() {
            failed.push(session);
            continue;
        }
        pending.proxies.insert(session, proxy_id.clone());
        state.proxy_to_upstream.insert(
            ProxyKey {
                session,
                id: proxy_id,
            },
            upstream_id.to_owned(),
        );
    }
    for session in failed {
        remove_session_locked(state, session);
    }
    state.pending.insert(upstream_id.to_owned(), pending);
}

fn broadcast_line_locked(state: &mut State, line: &str) {
    let sessions = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut failed = Vec::new();
    for session in sessions {
        let Some(sender) = state.sessions.get(&session).cloned() else {
            continue;
        };
        if sender.try_send(line.to_owned()).is_err() {
            failed.push(session);
        }
    }
    for session in failed {
        remove_session_locked(state, session);
    }
}

fn resolved_server_request_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if value.get("method")?.as_str()? != "serverRequest/resolved" {
        return None;
    }
    serde_json::to_string(value.get("params")?.get("requestId")?).ok()
}

/// Codex identifies a resolved request with its upstream id. Each phone only
/// knows its session-local proxy id, so fan out one correlated notification
/// per session and retire the replayable pending request atomically.
fn fanout_resolved_request_locked(state: &mut State, upstream_id: &str, line: &str) -> bool {
    let Some(pending) = state.pending.remove(upstream_id) else {
        return false;
    };
    let Ok(base) = serde_json::from_str::<Value>(line) else {
        state.pending.insert(upstream_id.to_owned(), pending);
        return false;
    };
    let mut failed = Vec::new();
    for (session, proxy_id) in pending.proxies {
        state.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: proxy_id.clone(),
        });
        let Some(sender) = state.sessions.get(&session).cloned() else {
            continue;
        };
        let Ok(proxy_value) = serde_json::from_str::<Value>(&proxy_id) else {
            failed.push(session);
            continue;
        };
        let mut notification = base.clone();
        let Some(request_id) = notification
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .and_then(|params| params.get_mut("requestId"))
        else {
            failed.push(session);
            continue;
        };
        *request_id = proxy_value;
        let Ok(proxy_line) = serde_json::to_string(&notification) else {
            failed.push(session);
            continue;
        };
        if sender.try_send(proxy_line).is_err() {
            failed.push(session);
        }
    }
    for session in failed {
        remove_session_locked(state, session);
    }
    true
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

/// Return a JSON value, not a bare string, because it is inserted into the
/// top-level JSON-RPC id field.
fn allocate_proxy_id(state: &mut State) -> String {
    loop {
        let id = state.next_proxy_id;
        state.next_proxy_id = state.next_proxy_id.checked_add(1).unwrap_or(1);
        let candidate = format!(r#""host-proxy-{id}""#);
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
    use host_protocol::raw_object;

    #[tokio::test]
    async fn server_requests_get_unique_ids_and_first_response_wins() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        let line = r#"{"id":"codex-1","method":"item/request","params":{"future":{"id":7}},"unknown":{"keep":true}}"#;
        router.handle_server_line(line);

        let first_line = first.recv().await.unwrap();
        let second_line = second.recv().await.unwrap();
        let first_message = classify_message(&first_line).unwrap();
        let second_message = classify_message(&second_line).unwrap();
        assert_ne!(first_message.raw_id(), second_message.raw_id());
        assert_eq!(first_message.method(), Some("item/request"));
        let first_object = raw_object(&first_line).unwrap();
        assert_eq!(first_object["params"].get(), r#"{"future":{"id":7}}"#);
        assert_eq!(first_object["unknown"].get(), r#"{"keep":true}"#);

        let first_id = first_message.raw_id().unwrap().to_owned();
        let second_id = second_message.raw_id().unwrap().to_owned();
        assert_eq!(
            router.resolve_response(1, &first_id),
            ResponseRoute::Forward(r#""codex-1""#.to_owned())
        );
        assert_eq!(
            router.resolve_response(2, &second_id),
            ResponseRoute::Unknown
        );
    }

    #[tokio::test]
    async fn notifications_are_forwarded_byte_for_byte() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        let line = r#" {"method":"future/event","params":{"unknown":[1,{"id":2}]} } "#;
        router.handle_server_line(line);
        assert_eq!(first.recv().await.unwrap(), line);
        assert_eq!(second.recv().await.unwrap(), line);
    }

    #[tokio::test]
    async fn resolved_requests_use_each_phone_proxy_id_and_are_not_replayed() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        router.handle_server_line(
            r#"{"id":"codex-1","method":"item/tool/requestUserInput","params":{"threadId":"thread-1"}}"#,
        );
        let first_request = first.recv().await.unwrap();
        let second_request = second.recv().await.unwrap();
        let first_id = classify_message(&first_request)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();
        let second_id = classify_message(&second_request)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();

        router.handle_server_line(
            r#"{"method":"serverRequest/resolved","params":{"threadId":"thread-1","requestId":"codex-1"}}"#,
        );

        let first_resolved: Value = serde_json::from_str(&first.recv().await.unwrap()).unwrap();
        let second_resolved: Value = serde_json::from_str(&second.recv().await.unwrap()).unwrap();
        assert_eq!(
            first_resolved["params"]["requestId"],
            serde_json::from_str::<Value>(&first_id).unwrap(),
        );
        assert_eq!(
            second_resolved["params"]["requestId"],
            serde_json::from_str::<Value>(&second_id).unwrap(),
        );
        assert_eq!(
            router.resolve_response(1, &first_id),
            ResponseRoute::Unknown
        );

        let mut later = router.open_session(4);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), later.recv())
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn unresolved_requests_are_replayed_to_later_sessions() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        router.handle_server_line(r#"{"id":1,"method":"request","params":{}}"#);
        let _ = first.recv().await.unwrap();
        drop(first);

        let mut later = router.open_session(4);
        let replay = later.recv().await.unwrap();
        let replay_message = classify_message(&replay).unwrap();
        assert_eq!(replay_message.method(), Some("request"));
        assert_ne!(replay_message.raw_id(), Some("1"));
    }
}
