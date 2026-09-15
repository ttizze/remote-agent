use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::providers::Provider;
use agent_core::peer::{RpcMessage, RpcMessageKind};
use serde_json::Value;
use tokio::sync::mpsc;

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is never put on the wire.
pub type SessionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseRoute {
    Forward(UpstreamRequest),
    Unknown,
}

/// Provider-owned request identity; never inferred from the wire ID's spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UpstreamRequest {
    pub provider: Provider,
    pub id: String,
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
    sessions: HashMap<SessionId, mpsc::Sender<String>>,
    routing: Routing,
}

/// Owned protocol data. Transitions neither lock nor send to client queues.
struct Routing {
    next_proxy_id: u64,
    pending: HashMap<UpstreamRequest, PendingServerRequest>,
    proxy_to_upstream: HashMap<ProxyKey, UpstreamRequest>,
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
            sessions: HashMap::new(),
            routing: Routing::default(),
        }
    }
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            next_proxy_id: 1,
            pending: HashMap::new(),
            proxy_to_upstream: HashMap::new(),
        }
    }
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum Delivery {
    Send(SessionId, String),
    Close(SessionId),
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
            .routing
            .pending
            .iter()
            .filter(|(_, request)| !request.response_claimed)
            .map(|(upstream_id, request)| (upstream_id.clone(), request.line.clone()))
            .collect::<Vec<_>>();
        for (upstream_id, line) in pending {
            let (routing, line) = std::mem::take(&mut state.routing).replay(id, upstream_id, &line);
            state.routing = routing;
            if let Some(line) = line
                && sender.try_send(line).is_err()
            {
                remove_session_locked(&mut state, id);
                break;
            }
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

    /// Resolve only requests belonging to the stopped provider. Other
    /// approvals and authenticated client connections remain live.
    pub(crate) fn resolve_provider_requests(&self, provider: Provider) {
        let mut state = lock_state(&self.state);
        let (routing, deliveries) =
            std::mem::take(&mut state.routing).resolve_provider_requests(provider);
        state.routing = routing;
        deliver_locked(&mut state, deliveries);
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
    pub(crate) fn handle_server_message(&self, provider: Provider, message: &RpcMessage<'_>) {
        let mut state = lock_state(&self.state);
        let (routing, deliveries) = std::mem::take(&mut state.routing).message(
            provider,
            message,
            state.sessions.keys().copied(),
        );
        state.routing = routing;
        deliver_locked(&mut state, deliveries);
    }

    /// First valid response wins. Retain aliases until the backend resolves the request
    /// so every device receives its own proxy id, but stop accepting/replaying it.
    pub(crate) fn resolve_response(&self, session: SessionId, id: &str) -> ResponseRoute {
        let mut state = lock_state(&self.state);
        let (routing, route) = std::mem::take(&mut state.routing).respond(session, id);
        state.routing = routing;
        route
    }
}

impl Routing {
    fn resolve_provider_requests(mut self, provider: Provider) -> (Self, Vec<Delivery>) {
        let ids: Vec<_> = self
            .pending
            .keys()
            .filter(|request| request.provider == provider)
            .cloned()
            .collect();
        let mut deliveries = Vec::new();
        for upstream in ids {
            let id = &upstream.id;
            let notification =
                format!(r#"{{"method":"serverRequest/resolved","params":{{"requestId":{id}}}}}"#);
            resolve_request(&mut self, &upstream, &notification, &mut deliveries);
        }
        (self, deliveries)
    }

    fn replay(
        mut self,
        session: SessionId,
        upstream: UpstreamRequest,
        line: &str,
    ) -> (Self, Option<String>) {
        let line = RpcMessage::parse(line)
            .ok()
            .and_then(|message| self.alias(session, upstream, &message));
        (self, line)
    }

    fn alias(
        &mut self,
        session: SessionId,
        upstream: UpstreamRequest,
        message: &RpcMessage<'_>,
    ) -> Option<String> {
        let proxy = allocate_proxy_id(self);
        let line = message.rewrite_id(&proxy).ok()?;
        self.pending
            .get_mut(&upstream)?
            .proxies
            .insert(session, proxy.clone());
        self.proxy_to_upstream
            .insert(ProxyKey { session, id: proxy }, upstream);
        Some(line)
    }

    fn message(
        mut self,
        provider: Provider,
        message: &RpcMessage<'_>,
        sessions: impl Iterator<Item = SessionId>,
    ) -> (Self, Vec<Delivery>) {
        let mut deliveries = Vec::new();
        match message.kind() {
            RpcMessageKind::Notification => {
                let resolved = resolved_server_request_id(message).is_some_and(|id| {
                    resolve_request(
                        &mut self,
                        &UpstreamRequest { provider, id },
                        message.line(),
                        &mut deliveries,
                    )
                });
                if !resolved {
                    deliveries.extend(
                        sessions.map(|session| Delivery::Send(session, message.line().to_owned())),
                    );
                }
            }
            RpcMessageKind::Request => {
                route_request(&mut self, provider, message, sessions, &mut deliveries)
            }
            RpcMessageKind::Response => {}
        }
        (self, deliveries)
    }

    fn respond(mut self, session: SessionId, id: &str) -> (Self, ResponseRoute) {
        let Some(upstream) = self.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: id.to_owned(),
        }) else {
            return (self, ResponseRoute::Unknown);
        };
        let Some(request) = self.pending.get_mut(&upstream) else {
            return (self, ResponseRoute::Unknown);
        };
        request.response_claimed = true;
        for (session, id) in &request.proxies {
            self.proxy_to_upstream.remove(&ProxyKey {
                session: *session,
                id: id.clone(),
            });
        }
        (self, ResponseRoute::Forward(upstream))
    }

    fn close(mut self, session: SessionId) -> Self {
        for pending in self.pending.values_mut() {
            pending.proxies.remove(&session);
        }
        self.proxy_to_upstream
            .retain(|key, _| key.session != session);
        self
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
            Delivery::Close(session) => Some(session),
        };
        if let Some(session) = failed {
            remove_session_locked(state, session);
        }
    }
}

fn route_request(
    state: &mut Routing,
    provider: Provider,
    message: &RpcMessage<'_>,
    sessions: impl Iterator<Item = SessionId>,
    deliveries: &mut Vec<Delivery>,
) {
    let upstream_id = UpstreamRequest {
        provider,
        id: message
            .raw_id()
            .expect("classified request has an ID")
            .to_owned(),
    };
    if state.pending.contains_key(&upstream_id) {
        return;
    }
    state.pending.insert(
        upstream_id.to_owned(),
        PendingServerRequest {
            response_claimed: false,
            line: message.line().to_owned(),
            proxies: HashMap::new(),
        },
    );
    deliveries.extend(sessions.map(|session| {
        match state.alias(session, upstream_id.to_owned(), message) {
            Some(line) => Delivery::Send(session, line),
            None => Delivery::Close(session),
        }
    }));
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
fn resolve_request(
    state: &mut Routing,
    upstream_id: &UpstreamRequest,
    line: &str,
    deliveries: &mut Vec<Delivery>,
) -> bool {
    let Some(pending) = state.pending.remove(upstream_id) else {
        return false;
    };
    let Ok(mut notification) = serde_json::from_str::<Value>(line) else {
        state.pending.insert(upstream_id.to_owned(), pending);
        return false;
    };
    for (session, proxy_id) in pending.proxies {
        state.proxy_to_upstream.remove(&ProxyKey {
            session,
            id: proxy_id.clone(),
        });
        let Ok(proxy_value) = serde_json::from_str::<Value>(&proxy_id) else {
            deliveries.push(Delivery::Close(session));
            continue;
        };
        let Some(request_id) = notification
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .and_then(|params| params.get_mut("requestId"))
        else {
            deliveries.push(Delivery::Close(session));
            continue;
        };
        *request_id = proxy_value;
        let Ok(proxy_line) = serde_json::to_string(&notification) else {
            deliveries.push(Delivery::Close(session));
            continue;
        };
        deliveries.push(Delivery::Send(session, proxy_line));
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
fn allocate_proxy_id(state: &mut Routing) -> String {
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
    state.routing = std::mem::take(&mut state.routing).close(session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::peer::raw_object;

    #[test]
    fn routing_decisions_are_reproducible_without_queues_or_a_runtime() {
        let request =
            RpcMessage::parse(r#"{"id":17,"method":"request","params":{"future":true}}"#).unwrap();
        let (routing, deliveries) =
            Routing::default().message(Provider::Codex, &request, [3, 9].into_iter());
        let (_, repeated) =
            Routing::default().message(Provider::Codex, &request, [3, 9].into_iter());
        assert_eq!(deliveries, repeated);
        assert_eq!(deliveries.len(), 2);
        let ids: Vec<_> = deliveries
            .iter()
            .map(|delivery| {
                let Delivery::Send(session, line) = delivery else {
                    panic!("valid request closes no session")
                };
                (
                    *session,
                    RpcMessage::parse(line)
                        .unwrap()
                        .raw_id()
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        let (routing, first) = routing.respond(ids[0].0, &ids[0].1);
        assert_eq!(
            first,
            ResponseRoute::Forward(UpstreamRequest {
                provider: Provider::Codex,
                id: "17".into()
            })
        );
        let (routing, second) = routing.respond(ids[1].0, &ids[1].1);
        assert_eq!(second, ResponseRoute::Unknown);
        assert!(
            routing.pending[&UpstreamRequest {
                provider: Provider::Codex,
                id: "17".into()
            }]
                .response_claimed
        );
        assert_eq!(
            routing.pending[&UpstreamRequest {
                provider: Provider::Codex,
                id: "17".into()
            }]
                .proxies
                .len(),
            2
        );
        let resolved =
            RpcMessage::parse(r#"{"method":"serverRequest/resolved","params":{"requestId":17}}"#)
                .unwrap();
        let (routing, deliveries) = routing.message(Provider::Codex, &resolved, [3, 9].into_iter());
        assert!(routing.pending.is_empty());
        assert!(routing.proxy_to_upstream.is_empty());
        assert_eq!(deliveries.len(), 2);
        for delivery in deliveries {
            let Delivery::Send(session, line) = delivery else {
                panic!("resolution must be delivered")
            };
            let notification: Value = serde_json::from_str(&line).unwrap();
            let id = &ids.iter().find(|(id, _)| *id == session).unwrap().1;
            assert_eq!(
                notification["params"]["requestId"],
                serde_json::from_str::<Value>(id).unwrap()
            );
        }
    }

    #[test]
    fn colliding_provider_ids_keep_responses_replay_and_shutdown_independent() {
        let router = SessionRouter::new();
        let mut first = router.open_session(8);
        let line = r#"{"id":"claude-permission:shared","method":"request","params":{}}"#;
        let request = RpcMessage::parse(line).unwrap();
        router.handle_server_message(Provider::Codex, &request);
        router.handle_server_message(Provider::Claude, &request);
        let codex = first.receiver.try_recv().unwrap();
        let claude = first.receiver.try_recv().unwrap();
        let id = |line: &str| {
            RpcMessage::parse(line)
                .unwrap()
                .raw_id()
                .unwrap()
                .to_owned()
        };
        assert_ne!(id(&codex), id(&claude));
        assert_eq!(
            router.resolve_response(first.id(), &id(&codex)),
            ResponseRoute::Forward(UpstreamRequest {
                provider: Provider::Codex,
                id: request.raw_id().unwrap().into(),
            })
        );
        // Stopping Codex retires even an ID spelled like a Claude approval.
        router.resolve_provider_requests(Provider::Codex);
        let resolved: Value = serde_json::from_str(&first.receiver.try_recv().unwrap()).unwrap();
        assert_eq!(
            resolved["params"]["requestId"],
            serde_json::from_str::<Value>(&id(&codex)).unwrap()
        );
        assert!(first.receiver.try_recv().is_err());
        drop(first);
        let mut reconnected = router.open_session(8);
        let replay = reconnected.receiver.try_recv().unwrap();
        assert!(reconnected.receiver.try_recv().is_err());
        assert_eq!(
            router.resolve_response(reconnected.id(), &id(&replay)),
            ResponseRoute::Forward(UpstreamRequest {
                provider: Provider::Claude,
                id: request.raw_id().unwrap().into(),
            })
        );
        let resolved = RpcMessage::parse(r#"{"method":"serverRequest/resolved","params":{"requestId":"claude-permission:shared"}}"#).unwrap();
        router.handle_server_message(Provider::Claude, &resolved);
        let resolution: Value =
            serde_json::from_str(&reconnected.receiver.try_recv().unwrap()).unwrap();
        assert_eq!(
            resolution["params"]["requestId"],
            serde_json::from_str::<Value>(&id(&replay)).unwrap()
        );
        assert!(router.open_session(8).receiver.try_recv().is_err());
    }

    #[test]
    fn failed_delivery_removes_aliases_and_retains_requests_for_reconnect() {
        let router = SessionRouter::new();
        let mut slow = router.open_session(1);
        let mut healthy = router.open_session(4);
        let notification = r#"{"method":"event","params":{}}"#;
        router.handle_server_message(Provider::Codex, &RpcMessage::parse(notification).unwrap());
        router.handle_server_message(
            Provider::Codex,
            &RpcMessage::parse(r#"{"id":8,"method":"request","params":{}}"#).unwrap(),
        );
        assert!(router.ensure_session(slow.id()).is_err());
        assert!(router.ensure_session(healthy.id()).is_ok());
        assert_eq!(slow.receiver.try_recv().unwrap(), notification);
        assert!(matches!(
            slow.receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
        assert_eq!(healthy.receiver.try_recv().unwrap(), notification);
        let delivered = healthy.receiver.try_recv().unwrap();
        let old_id = RpcMessage::parse(&delivered)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();
        let mut reconnected = router.open_session(4);
        let replay = reconnected.receiver.try_recv().unwrap();
        let replay_id = RpcMessage::parse(&replay)
            .unwrap()
            .raw_id()
            .unwrap()
            .to_owned();
        assert_ne!(old_id, replay_id);
        assert!(
            !lock_state(&router.state).routing.pending[&UpstreamRequest {
                provider: Provider::Codex,
                id: "8".into()
            }]
                .proxies
                .contains_key(&slow.id())
        );
        assert_eq!(
            router.resolve_response(reconnected.id(), &replay_id),
            ResponseRoute::Forward(UpstreamRequest {
                provider: Provider::Codex,
                id: "8".into()
            })
        );
        assert_eq!(
            router.resolve_response(healthy.id(), &old_id),
            ResponseRoute::Unknown
        );
        assert!(router.open_session(4).receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn notifications_are_forwarded_byte_for_byte() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        let line = r#" {"method":"future/event","params":{"unknown":[1,{"id":2}]} } "#;
        router.handle_server_message(Provider::Codex, &RpcMessage::parse(line).unwrap());
        assert_eq!(first.recv().await.unwrap(), line);
        assert_eq!(second.recv().await.unwrap(), line);
    }

    #[tokio::test]
    async fn resolved_requests_use_each_phone_proxy_id_and_are_not_replayed() {
        let router = SessionRouter::new();
        let mut first = router.open_session(4);
        let mut second = router.open_session(4);
        router.handle_server_message(Provider::Codex, &RpcMessage::parse(r#"{"id":"codex-1","method":"item/tool/requestUserInput","params":{"threadId":"thread-1","future":{"id":7}},"unknown":{"keep":true}}"#).unwrap());
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

        assert_ne!(first_id, second_id);
        let object = raw_object(&first_request).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(object["params"].get()).unwrap(),
            serde_json::json!({"threadId":"thread-1","future":{"id":7}})
        );
        assert_eq!(object["unknown"].get(), r#"{"keep":true}"#);
        assert_eq!(
            router.resolve_response(first.id(), &first_id),
            ResponseRoute::Forward(UpstreamRequest {
                provider: Provider::Codex,
                id: r#""codex-1""#.to_owned()
            })
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

        router.handle_server_message(Provider::Codex, &RpcMessage::parse(r#"{"method":"serverRequest/resolved","params":{"threadId":"thread-1","requestId":"codex-1"}}"#).unwrap());

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
            Provider::Codex,
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
