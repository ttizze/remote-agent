use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use host_protocol::{RpcMessageKind, classify_message, rewrite_top_level_id};
use serde_json::Value;
use tokio::sync::mpsc;

/// An authenticated session ID, never put on the wire.
pub type SessionId = u64;

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
    queues: Weak<Mutex<SessionQueues>>,
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
        if let Some(queues) = self.queues.upgrade() {
            close_session(&queues, self.id);
        }
    }
}

/// I/O ownership only. Alias decisions below operate on an exclusively borrowed value.
pub(crate) struct SessionQueues {
    next_session_id: SessionId,
    outbound: HashMap<SessionId, mpsc::Sender<String>>,
    routes: RequestRoutes,
}

pub(crate) fn session_queues() -> Arc<Mutex<SessionQueues>> {
    Arc::new(Mutex::new(SessionQueues {
        next_session_id: 1,
        outbound: HashMap::new(),
        routes: RequestRoutes {
            next_proxy_id: 1,
            pending: HashMap::new(),
            proxies: HashMap::new(),
        },
    }))
}

pub(crate) fn open_session(queues: &Arc<Mutex<SessionQueues>>, capacity: usize) -> CodexSession {
    assert!(capacity > 0, "a Codex session queue must have capacity");
    let (sender, receiver) = mpsc::channel(capacity);
    let mut state = lock_queues(queues);
    let id = loop {
        let id = state.next_session_id;
        state.next_session_id = id.checked_add(1).unwrap_or(1);
        if !state.outbound.contains_key(&id) {
            break id;
        }
    };
    state.outbound.insert(id, sender);
    let SessionQueues {
        outbound, routes, ..
    } = &mut *state;
    // The iterator rewrites each retained request directly; no copy of all
    // pending bodies or a temporary replay vector is needed.
    if routes
        .replay(id)
        .try_for_each(|line| outbound[&id].try_send(line))
        .is_err()
    {
        remove_session(&mut state, id);
    }
    CodexSession {
        id,
        receiver,
        queues: Arc::downgrade(queues),
    }
}

pub(crate) fn close_session(queues: &Mutex<SessionQueues>, session: SessionId) {
    remove_session(&mut lock_queues(queues), session);
}

pub(crate) fn close_all(queues: &Mutex<SessionQueues>) {
    let mut state = lock_queues(queues);
    state.outbound.clear();
    state.routes.proxies.clear();
    for request in state.routes.pending.values_mut() {
        request.proxies.clear();
    }
}

pub(crate) fn ensure_session(
    queues: &Mutex<SessionQueues>,
    session: SessionId,
) -> Result<(), RouteError> {
    if lock_queues(queues).outbound.contains_key(&session) {
        Ok(())
    } else {
        Err(RouteError::UnknownSession(session))
    }
}

pub(crate) fn send_line(
    queues: &Mutex<SessionQueues>,
    session: SessionId,
    line: String,
) -> Result<(), RouteError> {
    send_line_locked(&mut lock_queues(queues), session, line)
}

fn send_line_locked(
    state: &mut SessionQueues,
    session: SessionId,
    line: String,
) -> Result<(), RouteError> {
    match state
        .outbound
        .get(&session)
        .ok_or(RouteError::UnknownSession(session))?
        .try_send(line)
    {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            remove_session(state, session);
            Err(RouteError::QueueFull { session })
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            remove_session(state, session);
            Err(RouteError::UnknownSession(session))
        }
    }
}

pub(crate) fn handle_server_line(queues: &Mutex<SessionQueues>, line: &str) {
    let Ok(message) = classify_message(line) else {
        return;
    };
    let mut state = lock_queues(queues);
    match message.kind() {
        RpcMessageKind::Notification => {
            // Ordinary streamed deltas do not need a second JSON parse.
            if message.method() == Some("serverRequest/resolved") {
                if let Some((mut notification, proxies)) = state.routes.resolve_notification(line) {
                    for (session, proxy_id) in proxies {
                        if !state.outbound.contains_key(&session) {
                            continue;
                        }
                        if let Some(line) = resolved_line(&mut notification, &proxy_id) {
                            let _ = send_line_locked(&mut state, session, line);
                        } else {
                            remove_session(&mut state, session);
                        }
                    }
                    return;
                }
            }
            let SessionQueues {
                outbound, routes, ..
            } = &mut *state;
            outbound.retain(|session, sender| {
                if sender.try_send(line.to_owned()).is_ok() {
                    true
                } else {
                    routes.remove_session(*session);
                    false
                }
            });
        }
        RpcMessageKind::Request => {
            let upstream = message.raw_id().unwrap_or_default();
            let SessionQueues {
                outbound, routes, ..
            } = &mut *state;
            if routes.pending.contains_key(upstream) {
                return;
            }
            routes.pending.insert(
                upstream.to_owned(),
                PendingRequest {
                    response_claimed: false,
                    line: line.to_owned(),
                    proxies: HashMap::new(),
                },
            );
            outbound.retain(|session, sender| {
                if routes
                    .alias_request(upstream, *session)
                    .is_some_and(|line| sender.try_send(line).is_ok())
                {
                    true
                } else {
                    routes.remove_session(*session);
                    false
                }
            });
        }
        RpcMessageKind::Response => {}
    }
}

pub(crate) fn resolve_response(
    queues: &Mutex<SessionQueues>,
    session: SessionId,
    id: &str,
) -> ResponseRoute {
    lock_queues(queues).routes.claim_response(session, id)
}

fn lock_queues(queues: &Mutex<SessionQueues>) -> std::sync::MutexGuard<'_, SessionQueues> {
    queues
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn remove_session(state: &mut SessionQueues, session: SessionId) {
    state.outbound.remove(&session);
    state.routes.remove_session(session);
}

/// Functional routing state: no channels, locks, callbacks, clocks or shared ownership.
struct RequestRoutes {
    next_proxy_id: u64,
    pending: HashMap<String, PendingRequest>,
    // Proxy IDs are globally unique; the owner is checked before accepting a response.
    proxies: HashMap<String, (SessionId, String)>,
}

struct PendingRequest {
    response_claimed: bool,
    line: String,
    proxies: HashMap<SessionId, String>,
}

impl RequestRoutes {
    fn claim_response(&mut self, session: SessionId, id: &str) -> ResponseRoute {
        if !self
            .proxies
            .get(id)
            .is_some_and(|(owner, _)| *owner == session)
        {
            return ResponseRoute::Unknown;
        }
        let (_, upstream) = self.proxies.remove(id).unwrap();
        let Some(request) = self.pending.get_mut(&upstream) else {
            return ResponseRoute::Unknown;
        };
        request.response_claimed = true;
        for alias in request.proxies.values() {
            self.proxies.remove(alias);
        }
        ResponseRoute::Forward(upstream)
    }

    fn alias_request(&mut self, upstream: &str, session: SessionId) -> Option<String> {
        let proxy = next_proxy_id(&mut self.next_proxy_id, &self.proxies);
        let request = self.pending.get_mut(upstream)?;
        let line = rewrite_top_level_id(&request.line, &proxy).ok()?;
        request.proxies.insert(session, proxy.clone());
        self.proxies.insert(proxy, (session, upstream.to_owned()));
        Some(line)
    }

    fn replay(&mut self, session: SessionId) -> impl Iterator<Item = String> + '_ {
        let Self {
            pending,
            next_proxy_id: next,
            proxies,
        } = self;
        pending.iter_mut().filter_map(move |(upstream, request)| {
            if request.response_claimed {
                return None;
            }
            let proxy = next_proxy_id(next, proxies);
            let line = rewrite_top_level_id(&request.line, &proxy).ok()?;
            request.proxies.insert(session, proxy.clone());
            proxies.insert(proxy, (session, upstream.clone()));
            Some(line)
        })
    }

    fn resolve_notification(&mut self, line: &str) -> Option<(Value, HashMap<SessionId, String>)> {
        let notification: Value = serde_json::from_str(line).ok()?;
        let upstream = serde_json::to_string(notification.get("params")?.get("requestId")?).ok()?;
        let request = self.pending.remove(&upstream)?;
        for proxy in request.proxies.values() {
            self.proxies.remove(proxy);
        }
        Some((notification, request.proxies))
    }

    fn remove_session(&mut self, session: SessionId) {
        for request in self.pending.values_mut() {
            request.proxies.remove(&session);
        }
        self.proxies.retain(|_, (owner, _)| *owner != session);
    }
}

fn resolved_line(notification: &mut Value, proxy: &str) -> Option<String> {
    *notification.get_mut("params")?.get_mut("requestId")? = serde_json::from_str(proxy).ok()?;
    serde_json::to_string(notification).ok()
}

fn next_proxy_id(next: &mut u64, proxies: &HashMap<String, (SessionId, String)>) -> String {
    loop {
        let id = *next;
        *next = id.checked_add(1).unwrap_or(1);
        let candidate = format!(r#""host-proxy-{id}""#);
        if !proxies.contains_key(&candidate) {
            return candidate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_protocol::raw_object;

    #[tokio::test]
    async fn server_requests_get_unique_ids_and_first_response_wins() {
        let router = session_queues();
        let mut first = open_session(&router, 4);
        let mut second = open_session(&router, 4);
        let line = r#"{"id":"codex-1","method":"item/request","params":{"future":{"id":7}},"unknown":{"keep":true}}"#;
        handle_server_line(&router, line);

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
            resolve_response(&router, second.id(), &first_id),
            ResponseRoute::Unknown
        );
        assert_eq!(
            resolve_response(&router, 1, &first_id),
            ResponseRoute::Forward(r#""codex-1""#.to_owned())
        );
        assert_eq!(
            resolve_response(&router, 2, &second_id),
            ResponseRoute::Unknown
        );
    }

    #[tokio::test]
    async fn notifications_are_forwarded_byte_for_byte() {
        let router = session_queues();
        let mut first = open_session(&router, 4);
        let mut second = open_session(&router, 4);
        let line = r#" {"method":"future/event","params":{"unknown":[1,{"id":2}]} } "#;
        handle_server_line(&router, line);
        assert_eq!(first.recv().await.unwrap(), line);
        assert_eq!(second.recv().await.unwrap(), line);
    }

    #[tokio::test]
    async fn resolved_requests_use_each_phone_proxy_id_and_are_not_replayed() {
        let router = session_queues();
        let mut first = open_session(&router, 4);
        let mut second = open_session(&router, 4);
        handle_server_line(
            &router,
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

        assert_eq!(
            resolve_response(&router, first.id(), &first_id),
            ResponseRoute::Forward(r#""codex-1""#.to_owned())
        );
        assert_eq!(
            resolve_response(&router, second.id(), &second_id),
            ResponseRoute::Unknown
        );
        let mut after_answer = open_session(&router, 4);
        assert!(
            after_answer.receiver.try_recv().is_err(),
            "an answered request must not be replayed while Codex resolves it"
        );

        handle_server_line(
            &router,
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
            resolve_response(&router, 1, &first_id),
            ResponseRoute::Unknown
        );

        let mut later = open_session(&router, 4);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), later.recv())
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn unresolved_requests_are_replayed_to_later_sessions() {
        let router = session_queues();
        let mut first = open_session(&router, 4);
        handle_server_line(&router, r#"{"id":1,"method":"request","params":{}}"#);
        let _ = first.recv().await.unwrap();
        drop(first);

        let mut later = open_session(&router, 4);
        let replay = later.recv().await.unwrap();
        let replay_message = classify_message(&replay).unwrap();
        assert_eq!(replay_message.method(), Some("request"));
        assert_ne!(replay_message.raw_id(), Some("1"));
    }
}
