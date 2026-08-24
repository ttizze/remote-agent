use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use codex_app_server::{
    CodexAppServer, Error as AppServerError, RequestId, ServerEvent, ServerResponse,
};
use host_protocol::{
    RpcError, RpcId, RpcMessage, RpcNotification, RpcOutcome, RpcRequest, RpcResponse,
};
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc};

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is intentionally not exposed on the wire.
pub type SessionId = u64;

/// The result of submitting a response to a Codex-originated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDisposition {
    /// This was the first response for the request and was forwarded to
    /// Codex.
    Accepted,
    /// Another session already answered the request. The response is
    /// intentionally ignored: first response wins.
    AlreadyResolved,
    /// The id did not identify a request issued by this service/session.
    Unknown,
}

/// Errors produced by the gateway itself. Errors from Codex are encoded as a
/// normal `RpcResponse::Failure` and delivered to the requesting session so
/// the raw error shape is not lost.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("RPC session {0} is not open")]
    UnknownSession(SessionId),
    #[error("RPC session {session} outbound queue is full")]
    QueueFull { session: SessionId },
    #[error("RPC session {session} outbound queue is closed")]
    QueueClosed { session: SessionId },
    #[error("Codex server notification channel closed")]
    EventSourceClosed,
    #[error("Codex server event stream lagged by {0} messages")]
    EventSourceLagged(u64),
    #[error("Codex App Server response failed: {0}")]
    Upstream(#[source] Box<AppServerError>),
    #[error("RPC response could not be encoded: {0}")]
    Serialization(String),
    #[error("method {method} is owned by the host daemon")]
    DaemonOwnedMethod { method: String },
}

/// A live authenticated session's bounded outbound queue.
///
/// The runtime owns this value and reads `RpcMessage`s from it while reading
/// client messages from the corresponding QUIC stream. Dropping the session
/// unregisters it from the service and removes its proxy aliases.
pub struct CodexSession {
    id: SessionId,
    receiver: Option<mpsc::Receiver<RpcMessage>>,
    state: Weak<Mutex<DispatcherState>>,
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

    pub fn try_recv(&mut self) -> Result<RpcMessage, mpsc::error::TryRecvError> {
        self.receiver
            .as_mut()
            .expect("session receiver was already taken")
            .try_recv()
    }

    /// Split the session id and bounded receiver for a transport loop. The
    /// returned receiver is not self-unregistering; the runtime must call
    /// [`CodexRpcService::close_session`] when its transport loop exits.
    pub fn into_parts(mut self) -> (SessionId, mpsc::Receiver<RpcMessage>) {
        let id = self.id;
        let receiver = self
            .receiver
            .take()
            .expect("session receiver was already taken");
        self.state = Weak::new();
        (id, receiver)
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
        if self.receiver.is_some() {
            close_session_state(&self.state, self.id);
        }
    }
}

/// Routes raw Codex RPC messages to authenticated mobile sessions.
#[derive(Clone)]
pub struct CodexRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    app_server: Arc<CodexAppServer>,
    state: Arc<Mutex<DispatcherState>>,
    event_pump_started: OnceLock<()>,
}

struct DispatcherState {
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

impl Default for DispatcherState {
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

impl CodexRpcService {
    pub fn new(app_server: Arc<CodexAppServer>) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                app_server,
                state: Arc::new(Mutex::new(DispatcherState::default())),
                event_pump_started: OnceLock::new(),
            }),
        }
    }

    /// Open a session with a bounded outbound queue.
    ///
    /// The event pump is started lazily because `CodexRpcService::new` is
    /// intentionally synchronous and may be constructed before a Tokio
    /// runtime is entered. `open_session` is called by the async daemon
    /// runtime, so spawning here is safe.
    pub fn open_session(&self, capacity: usize) -> CodexSession {
        assert!(capacity > 0, "a Codex session queue must have capacity");
        self.start_event_pump();

        let (sender, receiver) = mpsc::channel(capacity);
        let (id, pending) = {
            let mut state = lock_state(&self.inner.state);
            let id = allocate_session_id(&mut state);
            state.sessions.insert(id, sender.clone());

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
            (id, pending)
        };

        // Re-send unresolved Codex requests to a session that joins after the
        // original broadcast. An empty newly-created queue cannot overflow;
        // if a future queue implementation changes that invariant, dropping
        // the session is safer than buffering without a bound.
        for (upstream_id, method, params, extensions) in pending {
            let proxy_id = {
                let mut state = lock_state(&self.inner.state);
                if !state.pending.contains_key(&upstream_id) {
                    None
                } else {
                    let proxy_id = allocate_proxy_id(&mut state);
                    state
                        .pending
                        .get_mut(&upstream_id)
                        .expect("pending entry checked above")
                        .proxies
                        .insert(id, proxy_id.clone());
                    state.proxy_to_upstream.insert(
                        ProxyKey {
                            session: id,
                            id: proxy_id.clone(),
                        },
                        upstream_id,
                    );
                    Some(proxy_id)
                }
            };
            let Some(proxy_id) = proxy_id else {
                continue;
            };
            let message = RpcMessage::Request(RpcRequest {
                id: proxy_id,
                method,
                params,
                extensions,
            });
            if sender.try_send(message).is_err() {
                close_session_state(&Arc::downgrade(&self.inner.state), id);
                break;
            }
        }

        CodexSession {
            id,
            receiver: Some(receiver),
            state: Arc::downgrade(&self.inner.state),
        }
    }

    /// Unregister a session explicitly. Dropping `CodexSession` has the same
    /// effect, but the explicit form is useful when a QUIC connection closes
    /// while its receiver is still held by another task.
    pub fn close_session(&self, session: SessionId) {
        close_session_state(&Arc::downgrade(&self.inner.state), session);
    }

    /// Dispatch one client-originated RPC message.
    ///
    /// Requests receive a response on their own bounded session queue.
    /// Responses answer a Codex-originated request, and notifications are
    /// passed to Codex's raw notification endpoint. The runtime should call
    /// this for every incoming `RpcMessage` after authentication.
    pub async fn dispatch_message(
        &self,
        session: SessionId,
        message: RpcMessage,
    ) -> Result<ResponseDisposition, DispatchError> {
        match message {
            RpcMessage::Request(request) => {
                self.dispatch_request(session, request).await?;
                Ok(ResponseDisposition::Accepted)
            }
            RpcMessage::Response(response) => self.dispatch_response(session, response).await,
            RpcMessage::Notification(notification) => {
                self.dispatch_notification(session, notification).await?;
                Ok(ResponseDisposition::Accepted)
            }
        }
    }

    /// Forward a mobile request to Codex, preserving the method, params, and
    /// response/error JSON shape. `initialize` is daemon-owned because the
    /// App Server is initialized once when it starts.
    pub async fn dispatch_request(
        &self,
        session: SessionId,
        request: RpcRequest,
    ) -> Result<(), DispatchError> {
        self.ensure_session(session)?;
        let RpcRequest {
            id,
            method,
            params,
            extensions,
            ..
        } = request;
        if is_daemon_lifecycle(&method) {
            return self
                .send_response(
                    session,
                    RpcResponse {
                        id,
                        outcome: RpcOutcome::Failure {
                            error: daemon_owned_error(&method),
                        },
                        extensions,
                    },
                )
                .await;
        }

        let outcome = match self
            .inner
            .app_server
            .request_json(&method, params, extensions.clone())
            .await
        {
            Ok(result) => RpcOutcome::Success { result },
            Err(error) => RpcOutcome::Failure {
                error: rpc_error_from_app_server(error),
            },
        };
        self.send_response(
            session,
            RpcResponse {
                id,
                outcome,
                extensions,
            },
        )
        .await
    }

    /// Forward a mobile notification to Codex. The App Server's lifecycle
    /// notification is local because the daemon performs it during startup.
    pub async fn dispatch_notification(
        &self,
        session: SessionId,
        notification: RpcNotification,
    ) -> Result<(), DispatchError> {
        self.ensure_session(session)?;
        if is_daemon_lifecycle(&notification.method) {
            return Err(DispatchError::DaemonOwnedMethod {
                method: notification.method,
            });
        }
        self.inner
            .app_server
            .notify_json(
                &notification.method,
                notification.params,
                notification.extensions,
            )
            .await
            .map_err(|error| DispatchError::Upstream(Box::new(error)))
    }

    /// Accept a response from one session to a Codex-originated request.
    ///
    /// Every session receives a distinct proxy id. The first response that
    /// maps to an unresolved upstream id wins; aliases from other sessions
    /// are removed before writing the response upstream, so a concurrent
    /// second response cannot race and answer the same Codex request.
    pub async fn dispatch_response(
        &self,
        session: SessionId,
        response: RpcResponse,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.ensure_session(session)?;
        let proxy_key = ProxyKey {
            session,
            id: response.id.clone(),
        };
        let upstream_id = {
            let mut state = lock_state(&self.inner.state);
            let Some(upstream_id) = state.proxy_to_upstream.remove(&proxy_key) else {
                return Ok(ResponseDisposition::Unknown);
            };
            let Some(pending) = state.pending.remove(&upstream_id) else {
                return Ok(ResponseDisposition::AlreadyResolved);
            };
            for (alias_session, proxy_id) in pending.proxies {
                state.proxy_to_upstream.remove(&ProxyKey {
                    session: alias_session,
                    id: proxy_id,
                });
            }
            upstream_id
        };

        let (payload, extensions) = server_response(&response)?;
        self.inner
            .app_server
            .respond_json(upstream_id, payload, extensions)
            .await
            .map_err(|error| DispatchError::Upstream(Box::new(error)))?;
        Ok(ResponseDisposition::Accepted)
    }

    /// Number of currently registered authenticated sessions.
    pub fn session_count(&self) -> usize {
        lock_state(&self.inner.state).sessions.len()
    }

    fn ensure_session(&self, session: SessionId) -> Result<(), DispatchError> {
        if lock_state(&self.inner.state)
            .sessions
            .contains_key(&session)
        {
            Ok(())
        } else {
            Err(DispatchError::UnknownSession(session))
        }
    }

    async fn send_response(
        &self,
        session: SessionId,
        response: RpcResponse,
    ) -> Result<(), DispatchError> {
        self.send_message(session, RpcMessage::Response(response))
    }

    fn send_message(&self, session: SessionId, message: RpcMessage) -> Result<(), DispatchError> {
        let sender = {
            let state = lock_state(&self.inner.state);
            state.sessions.get(&session).cloned()
        }
        .ok_or(DispatchError::UnknownSession(session))?;

        match sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                close_session_state(&Arc::downgrade(&self.inner.state), session);
                Err(DispatchError::QueueFull { session })
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                close_session_state(&Arc::downgrade(&self.inner.state), session);
                Err(DispatchError::QueueClosed { session })
            }
        }
    }

    fn start_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Keep only a weak reference in the task. A strong App Server Arc
        // captured here would outlive the service and prevent runtime
        // shutdown from reclaiming the child process with `Arc::try_unwrap`.
        let app_server = Arc::downgrade(&self.inner.app_server);
        let state = Arc::downgrade(&self.inner.state);
        tokio::spawn(async move {
            let Some(app_server) = app_server.upgrade() else {
                return;
            };
            let mut events = app_server.subscribe();
            drop(app_server);
            loop {
                match events.recv().await {
                    Ok(event) => fanout_event(&state, event),
                    Err(broadcast::error::RecvError::Closed) => {
                        close_all_sessions(&state);
                        return;
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // A broadcast receiver cannot reconstruct messages
                        // after lagging. Close all bounded queues so clients
                        // observe a disconnect and can reconnect/synchronize;
                        // never invent a partial event stream.
                        close_all_sessions(&state);
                        let _ = skipped;
                        return;
                    }
                }
            }
        });
    }
}

fn is_daemon_lifecycle(method: &str) -> bool {
    matches!(method, "initialize" | "initialized")
}

fn daemon_owned_error(method: &str) -> RpcError {
    RpcError {
        code: json!("daemon_owned_method"),
        message: format!("{method} is handled by the Host daemon"),
        data: None,
        extensions: Map::new(),
    }
}

fn rpc_error_from_app_server(error: AppServerError) -> RpcError {
    match error {
        AppServerError::Remote { detail, .. } => RpcError {
            code: detail.code,
            message: detail.message,
            data: detail.data,
            extensions: detail.additional_fields,
        },
        other => RpcError {
            code: json!("codex_unavailable"),
            message: other.to_string(),
            data: None,
            extensions: Map::new(),
        },
    }
}

fn server_response(
    response: &RpcResponse,
) -> Result<(ServerResponse, Map<String, Value>), DispatchError> {
    match &response.outcome {
        RpcOutcome::Success { result } => Ok((
            ServerResponse::Result {
                result: result.clone(),
            },
            response.extensions.clone(),
        )),
        RpcOutcome::Failure { error } => Ok((
            ServerResponse::Error {
                error: serde_json::to_value(error)
                    .map_err(|error| DispatchError::Serialization(error.to_string()))?,
            },
            response.extensions.clone(),
        )),
    }
}

fn fanout_event(state: &Weak<Mutex<DispatcherState>>, event: ServerEvent) {
    match event {
        ServerEvent::Notification {
            method,
            params,
            extensions,
        } => {
            let message = RpcMessage::Notification(RpcNotification {
                method,
                params,
                extensions,
            });
            broadcast_message(state, message);
        }
        ServerEvent::Request {
            id,
            method,
            params,
            extensions,
        } => fanout_server_request(state, id, method, params, extensions),
    }
}

fn fanout_server_request(
    state: &Weak<Mutex<DispatcherState>>,
    upstream_id: RequestId,
    method: String,
    params: Value,
    extensions: Map<String, Value>,
) {
    let Some(state_arc) = state.upgrade() else {
        return;
    };
    let mut state = lock_state(&state_arc);
    if state.pending.contains_key(&upstream_id) {
        // Codex request ids are expected to be unique while pending. Keep the
        // first request if a broken server violates that contract; this
        // avoids rewriting an already delivered request under a new payload.
        return;
    }

    let session_ids = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut pending = PendingServerRequest {
        method: method.clone(),
        params: params.clone(),
        extensions: extensions.clone(),
        proxies: HashMap::new(),
    };
    let mut failed_sessions = HashSet::new();
    for session in session_ids {
        let proxy_id = allocate_proxy_id(&mut state);
        let message = RpcMessage::Request(RpcRequest {
            id: proxy_id.clone(),
            method: method.clone(),
            params: params.clone(),
            extensions: extensions.clone(),
        });
        let Some(sender) = state.sessions.get(&session).cloned() else {
            failed_sessions.insert(session);
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
            Err(_) => {
                failed_sessions.insert(session);
            }
        }
    }
    for session in failed_sessions {
        remove_session_locked(&mut state, session);
    }
    state.pending.insert(upstream_id, pending);
}

fn broadcast_message(state: &Weak<Mutex<DispatcherState>>, message: RpcMessage) {
    let Some(state_arc) = state.upgrade() else {
        return;
    };
    let mut state = lock_state(&state_arc);
    let sessions = state.sessions.keys().copied().collect::<Vec<_>>();
    let mut failed = Vec::new();
    for session in sessions {
        let Some(sender) = state.sessions.get(&session).cloned() else {
            continue;
        };
        if sender.try_send(message.clone()).is_err() {
            failed.push(session);
        }
    }
    for session in failed {
        remove_session_locked(&mut state, session);
    }
}

fn allocate_session_id(state: &mut DispatcherState) -> SessionId {
    let id = state.next_session_id;
    state.next_session_id = state.next_session_id.checked_add(1).unwrap_or(1);
    id
}

fn allocate_proxy_id(state: &mut DispatcherState) -> RpcId {
    let id = state.next_proxy_id;
    state.next_proxy_id = state.next_proxy_id.checked_add(1).unwrap_or(1);
    RpcId::String(format!("host-proxy-{id}"))
}

fn lock_state(state: &Mutex<DispatcherState>) -> std::sync::MutexGuard<'_, DispatcherState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn close_session_state(state: &Weak<Mutex<DispatcherState>>, session: SessionId) {
    if let Some(state) = state.upgrade() {
        remove_session_locked(&mut lock_state(&state), session);
    }
}

fn close_all_sessions(state: &Weak<Mutex<DispatcherState>>) {
    if let Some(state) = state.upgrade() {
        let mut state = lock_state(&state);
        state.sessions.clear();
        state.proxy_to_upstream.clear();
        for pending in state.pending.values_mut() {
            pending.proxies.clear();
        }
    }
}

fn remove_session_locked(state: &mut DispatcherState, session: SessionId) {
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

    #[test]
    fn lifecycle_methods_are_owned_by_the_daemon() {
        assert!(is_daemon_lifecycle("initialize"));
        assert!(is_daemon_lifecycle("initialized"));
        assert!(!is_daemon_lifecycle("thread/list"));
    }

    #[test]
    fn proxy_ids_are_strings_in_a_daemon_namespace() {
        let mut state = DispatcherState::default();
        assert_eq!(
            allocate_proxy_id(&mut state),
            RpcId::String("host-proxy-1".to_owned())
        );
        assert_eq!(
            allocate_proxy_id(&mut state),
            RpcId::String("host-proxy-2".to_owned())
        );
    }

    #[test]
    fn server_response_preserves_result_and_structured_error() {
        let response = RpcResponse {
            id: RpcId::Integer(4),
            outcome: RpcOutcome::Failure {
                error: RpcError {
                    code: json!(-32000),
                    message: "nope".to_owned(),
                    data: Some(json!({"retryable": true})),
                    extensions: Map::from_iter([(String::from("vendor"), json!(true))]),
                },
            },
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        };
        assert_eq!(
            server_response(&response).unwrap(),
            (
                ServerResponse::Error {
                    error: json!({
                        "code": -32000,
                        "message": "nope",
                        "data": {"retryable": true},
                        "vendor": true
                    })
                },
                Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            )
        );
    }

    #[tokio::test]
    async fn fanout_forwards_unknown_notifications_without_translation() {
        let state = Arc::new(Mutex::new(DispatcherState::default()));
        let (sender, mut receiver) = mpsc::channel(4);
        lock_state(&state).sessions.insert(7, sender);

        fanout_event(
            &Arc::downgrade(&state),
            ServerEvent::Notification {
                method: "future/item/newKind".to_owned(),
                params: json!({"futureField": [1, {"nested": true}]}),
                extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            },
        );

        assert_eq!(
            receiver.recv().await,
            Some(RpcMessage::Notification(RpcNotification {
                method: "future/item/newKind".to_owned(),
                params: json!({"futureField": [1, {"nested": true}]}),
                extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            }))
        );
    }

    #[tokio::test]
    async fn server_request_gets_distinct_proxy_ids_for_each_session() {
        let state = Arc::new(Mutex::new(DispatcherState::default()));
        let (first_sender, mut first_receiver) = mpsc::channel(4);
        let (second_sender, mut second_receiver) = mpsc::channel(4);
        {
            let mut state = lock_state(&state);
            state.sessions.insert(1, first_sender);
            state.sessions.insert(2, second_sender);
        }

        fanout_event(
            &Arc::downgrade(&state),
            ServerEvent::Request {
                id: RequestId::String("codex-request-1".to_owned()),
                method: "item/commandExecution/requestApproval".to_owned(),
                params: json!({"command": "cargo test"}),
                extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            },
        );

        let RpcMessage::Request(first) = first_receiver.recv().await.unwrap() else {
            panic!("expected a request for the first session")
        };
        let RpcMessage::Request(second) = second_receiver.recv().await.unwrap() else {
            panic!("expected a request for the second session")
        };
        assert_ne!(first.id, second.id);
        assert_eq!(first.method, second.method);
        assert_eq!(first.params, second.params);
        assert_eq!(first.extensions, second.extensions);
        let state = lock_state(&state);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.proxy_to_upstream.len(), 2);
    }

    #[tokio::test]
    async fn full_bounded_queue_unregisters_session_instead_of_buffering() {
        let state = Arc::new(Mutex::new(DispatcherState::default()));
        let (sender, mut receiver) = mpsc::channel(1);
        lock_state(&state).sessions.insert(7, sender);

        let event = || ServerEvent::Notification {
            method: "turn/started".to_owned(),
            params: json!({"turnId": "t1"}),
            extensions: Map::new(),
        };
        fanout_event(&Arc::downgrade(&state), event());
        fanout_event(&Arc::downgrade(&state), event());

        assert!(lock_state(&state).sessions.is_empty());
        assert!(receiver.recv().await.is_some());
    }
}
