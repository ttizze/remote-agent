use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use agent_core::peer::{RpcMessage, RpcMessageKind, rewrite_top_level_id};
use serde_json::Value;
use tokio::sync::mpsc;

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is never put on the wire.
pub type SessionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    Forward(String),
    Unknown,
}

/// A live authenticated session's bounded outbound queue.
pub struct HostSession {
    id: SessionId,
    receiver: mpsc::Receiver<String>,
    state: Weak<Mutex<State>>,
}

impl HostSession {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<String> {
        self.receiver.recv().await
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

/// Owns bounded session queues and aliases used for backend server requests.
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
    response_claimed: bool,
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

    pub(crate) fn open_session(&self, capacity: usize) -> HostSession {
        assert!(capacity > 0, "a Host session queue must have capacity");
        let (sender, receiver) = mpsc::channel(capacity);
        let mut state = lock_state(&self.state);
        let id = allocate_session_id(&mut state);
        state.sessions.insert(id, sender.clone());

        // A request that was sent while no phone was connected remains
        // pending. Replay it to this new session with a fresh proxy id.
        let pending = state
            .pending
            .iter()
            .filter(|(_, request)| !request.response_claimed)
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

        HostSession {
            id,
            receiver,
            state: Arc::downgrade(&self.state),
        }
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        remove_session_locked(&mut lock_state(&self.state), session);
    }

    /// Resolve only requests belonging to the stopped Codex backend. Claude
    /// approvals and authenticated client connections remain live.
    pub(crate) fn resolve_codex_requests(&self) {
        let mut state = lock_state(&self.state);
        let ids: Vec<_> = state
            .pending
            .keys()
            .filter(|id| !crate::claude::is_permission_id(id))
            .cloned()
            .collect();
        for id in ids {
            let notification =
                format!(r#"{{"method":"serverRequest/resolved","params":{{"requestId":{id}}}}}"#);
            fanout_resolved_request_locked(&mut state, &id, &notification);
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

    /// Fan out a backend notification or server request. Notifications are
    /// sent unchanged; server requests get one unique id per phone.
    pub(crate) fn handle_server_message(&self, message: &RpcMessage<'_>) {
        let line = message.line();
        let mut state = lock_state(&self.state);
        match message.kind() {
            RpcMessageKind::Notification => {
                let resolved = resolved_server_request_id(message).is_some_and(|upstream_id| {
                    fanout_resolved_request_locked(&mut state, &upstream_id, line)
                });
                if !resolved {
                    broadcast_line_locked(&mut state, line);
                }
            }
            RpcMessageKind::Request => fanout_request_locked(&mut state, message),
            RpcMessageKind::Response => {
                // Responses are consumed by the backend adapter before dispatch.
            }
        }
    }

    /// First valid response wins. Retain aliases until the backend resolves the request
    /// so every device receives its own proxy id, but stop accepting/replaying it.
    pub(crate) fn resolve_response(&self, session: SessionId, id: &str) -> ResponseRoute {
        let mut state = lock_state(&self.state);
        let Some(upstream_id) = state.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: id.to_owned(),
        }) else {
            return ResponseRoute::Unknown;
        };
        let State {
            pending,
            proxy_to_upstream,
            ..
        } = &mut *state;
        let Some(request) = pending.get_mut(&upstream_id) else {
            return ResponseRoute::Unknown;
        };
        request.response_claimed = true;
        for (alias_session, proxy_id) in &request.proxies {
            proxy_to_upstream.remove(&ProxyKey {
                session: *alias_session,
                id: proxy_id.clone(),
            });
        }
        ResponseRoute::Forward(upstream_id)
    }
}

fn fanout_request_locked(state: &mut State, message: &RpcMessage<'_>) {
    let upstream_id = message.raw_id().expect("classified request has an ID");
    let line = message.line();
    if state.pending.contains_key(upstream_id) {
        return;
    }
    let mut pending = PendingServerRequest {
        response_claimed: false,
        line: line.to_owned(),
        proxies: HashMap::new(),
    };
    let sessions = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut failed = Vec::new();
    for session in sessions {
        let proxy_id = allocate_proxy_id(state);
        let Ok(proxy_line) = message.rewrite_id(&proxy_id) else {
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
    let mut failed = Vec::new();
    for (&session, sender) in &state.sessions {
        if sender.try_send(line.to_owned()).is_err() {
            failed.push(session);
        }
    }
    for session in failed {
        remove_session_locked(state, session);
    }
}

fn resolved_server_request_id(message: &RpcMessage<'_>) -> Option<String> {
    if message.method()? != "serverRequest/resolved" {
        return None;
    }
    serde_json::to_string(message.params::<Value>().ok()?.get("requestId")?).ok()
}

/// A backend identifies a resolved request with its upstream id. Each phone only
/// knows its session-local proxy id, so fan out one correlated notification
/// per session and retire the replayable pending request atomically.
fn fanout_resolved_request_locked(state: &mut State, upstream_id: &str, line: &str) -> bool {
    let Some(pending) = state.pending.remove(upstream_id) else {
        return false;
    };
    let Ok(mut notification) = serde_json::from_str::<Value>(line) else {
        state.pending.insert(upstream_id.to_owned(), pending);
        return false;
    };
    let mut failed = Vec::new();
    for (session, proxy_id) in pending.proxies {
        state.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: proxy_id.clone(),
        });
        let Some(sender) = state.sessions.get(&session) else {
            continue;
        };
        let Ok(proxy_value) = serde_json::from_str::<Value>(&proxy_id) else {
            failed.push(session);
            continue;
        };
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
    use agent_core::peer::raw_object;

    #[tokio::test]
    async fn server_requests_get_unique_ids_and_first_response_wins() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        let line = r#"{"id":"codex-1","method":"item/request","params":{"future":{"id":7}},"unknown":{"keep":true}}"#;
        router.handle_server_message(&RpcMessage::parse(line).unwrap());

        let first_line = first.recv().await.unwrap();
        let second_line = second.recv().await.unwrap();
        let first_message = RpcMessage::parse(&first_line).unwrap();
        let second_message = RpcMessage::parse(&second_line).unwrap();
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
        router.handle_server_message(&RpcMessage::parse(line).unwrap());
        assert_eq!(first.recv().await.unwrap(), line);
        assert_eq!(second.recv().await.unwrap(), line);
    }

    #[tokio::test]
    async fn resolved_requests_use_each_phone_proxy_id_and_are_not_replayed() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        router.handle_server_message(&RpcMessage::parse(r#"{"id":"codex-1","method":"item/tool/requestUserInput","params":{"threadId":"thread-1"}}"#).unwrap());
        let first_request = first.recv().await.unwrap();
        let second_request = second.recv().await.unwrap();
        let first_id = RpcMessage::parse(&first_request)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();
        let second_id = RpcMessage::parse(&second_request)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();

        assert_eq!(
            router.resolve_response(first.id(), &first_id),
            ResponseRoute::Forward(r#""codex-1""#.to_owned())
        );
        assert_eq!(
            router.resolve_response(second.id(), &second_id),
            ResponseRoute::Unknown
        );
        let mut after_answer = router.open_session(4);
        assert!(
            after_answer.receiver.try_recv().is_err(),
            "an answered request must not be replayed while Codex resolves it"
        );

        router.handle_server_message(&RpcMessage::parse(r#"{"method":"serverRequest/resolved","params":{"threadId":"thread-1","requestId":"codex-1"}}"#).unwrap());

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
        router.handle_server_message(
            &RpcMessage::parse(r#"{"id":1,"method":"request","params":{}}"#).unwrap(),
        );
        let _ = first.recv().await.unwrap();
        drop(first);

        let mut later = router.open_session(4);
        let replay = later.recv().await.unwrap();
        let replay_message = RpcMessage::parse(&replay).unwrap();
        assert_eq!(replay_message.method(), Some("request"));
        assert_ne!(replay_message.raw_id(), Some("1"));
    }
}
