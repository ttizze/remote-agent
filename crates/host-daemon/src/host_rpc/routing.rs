use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::session_actor::SessionActor;
use agent_core::client::ServerRequest;
use agent_core::peer::RpcMessage;
use agent_core::{
    models::ThreadResponse,
    session::{OpenSession, OpenedSession, ProviderKind, SessionChange, SessionRef, SessionUpdate},
};
use serde_json::Value;
use tokio::sync::mpsc;

pub(super) struct SessionRead {
    router: SessionRouter,
    target: SessionRef,
}
impl Drop for SessionRead {
    fn drop(&mut self) {
        let mut state = lock_state(&self.router.state);
        if let Some(actor) = state.executions.get_mut(&self.target) {
            actor.readers -= 1;
        }
        state.executions.retain(|_, actor| actor.release());
    }
}

/// An identifier allocated by the daemon for one authenticated mobile
/// session. It is never put on the wire.
pub type SessionId = u64;

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
    executions: HashMap<SessionRef, SessionActor>,
    subscriptions: HashMap<uuid::Uuid, (SessionRef, SessionId)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            sessions: HashMap::new(),
            executions: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }
}

impl State {
    fn pending_request(&self, id: &str) -> Option<(SessionRef, agent_core::client::ServerRequest)> {
        self.executions.iter().find_map(|(target, actor)| {
            actor
                .live
                .requests
                .get(id)
                .map(|request| (target.clone(), (**request).clone()))
        })
    }
    fn pending_native(
        &self,
        provider: ProviderKind,
        id: &Value,
    ) -> Option<(SessionRef, agent_core::client::ServerRequest)> {
        self.executions
            .iter()
            .filter(|(target, _)| target.provider == provider)
            .find_map(|(target, actor)| {
                actor
                    .live
                    .requests
                    .values()
                    .find(|request| request.extra.get("nativeRequestId") == Some(id))
                    .map(|request| (target.clone(), (**request).clone()))
            })
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

    pub(super) fn begin_submission(&self, params: &Value) -> Result<SessionRef, String> {
        let target = SessionRef::from_thread_id(
            params["threadId"]
                .as_str()
                .ok_or("session ID is required")?,
        )?;
        let id = params["clientUserMessageId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or("clientUserMessageId is required")?;
        let mut state = lock_state(&self.state);
        let actor = state.executions.entry(target.clone()).or_default();
        if actor.inputs.len() >= 128 || !actor.inputs.insert(id.into()) {
            return Err("submission is already in flight or input capacity reached; read the session before sending again".into());
        }
        Ok(target)
    }

    pub(super) fn reject_submission(&self, target: &SessionRef, id: &str) {
        let mut state = lock_state(&self.state);
        if let Some(actor) = state.executions.get_mut(target) {
            actor.inputs.remove(id);
        }
        state.executions.retain(|_, actor| actor.release());
    }

    pub(crate) fn current_turn(&self, id: &str, turn_id: &str) -> Option<agent_core::models::Turn> {
        lock_state(&self.state)
            .executions
            .get(&SessionRef::from_thread_id(id).ok()?)
            .and_then(|actor| {
                actor
                    .live
                    .turns
                    .as_ref()?
                    .iter()
                    .rfind(|turn| turn.id == turn_id)
            })
            .map(|turn| (**turn).clone())
    }

    pub(super) fn begin_session_read(&self, params: OpenSession) -> Result<SessionRead, String> {
        if params.limit == 0
            || SessionRef::from_thread_id(&params.session.thread_id())? != params.session
        {
            return Err("invalid session reference or zero history limit".into());
        }
        let mut state = lock_state(&self.state);
        state
            .executions
            .entry(params.session.clone())
            .or_default()
            .readers += 1;
        Ok(SessionRead {
            router: self.clone(),
            target: params.session,
        })
    }

    pub(super) fn finish_session_read(
        &self,
        read: SessionRead,
        session: SessionId,
        request: &RpcMessage<'_>,
        mut response: ThreadResponse,
    ) -> Result<(), String> {
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) {
            return Err("connection closed during session open".into());
        }
        if response.thread.id.as_deref() != Some(read.target.thread_id().as_str()) {
            return Err("native session ID does not match".into());
        }
        if let Some(actor) = state.executions.get(&read.target) {
            actor.overlay(&mut response);
        }
        let subscription_id = uuid::Uuid::new_v4();
        let line = request
            .response::<_, ()>(Ok(OpenedSession {
                session: read.target.clone(),
                subscription_id,
                response,
            }))
            .map_err(|error| error.to_string())?;
        state
            .subscriptions
            .insert(subscription_id, (read.target.clone(), session));
        deliver_locked(&mut state, vec![(session, line)]);
        Ok(())
    }

    pub(super) fn close_subscription(&self, session: SessionId, subscription: uuid::Uuid) {
        let mut state = lock_state(&self.state);
        if state
            .subscriptions
            .get(&subscription)
            .is_some_and(|(_, owner)| *owner == session)
        {
            state.subscriptions.remove(&subscription);
        }
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        remove_session_locked(&mut lock_state(&self.state), session);
    }

    pub(crate) fn fail_provider(&self, provider: ProviderKind, message: &str) {
        let mut state = lock_state(&self.state);
        let live: Vec<_> = state
            .executions
            .iter()
            .filter(|(target, _)| target.provider == provider)
            .map(|(target, actor)| (target.clone(), actor.live.clone()))
            .collect();
        for (target, thread) in live {
            for id in thread.requests.keys() {
                change_locked(
                    &mut state,
                    &target,
                    &SessionChange::ResolveRequest {
                        request_id: id.clone(),
                    },
                );
            }
            for turn in thread
                .turns
                .iter()
                .flatten()
                .filter(|turn| turn.status.as_deref() == Some("inProgress"))
            {
                let mut turn = (**turn).clone();
                turn.status = Some("failed".into());
                turn.error = Some(serde_json::json!({"message":message}));
                change_locked(
                    &mut state,
                    &target,
                    &SessionChange::Turn {
                        turn,
                        completed: true,
                    },
                );
            }
            if let Some(actor) = state.executions.get_mut(&target) {
                actor.inputs.clear();
            }
        }
        state.executions.retain(|_, actor| actor.release());
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

    pub(crate) fn request(
        &self,
        provider: ProviderKind,
        mut request: ServerRequest,
    ) -> Result<(), String> {
        let target = SessionRef::from_thread_id(
            request
                .params
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or("request session ID is missing")?,
        )?;
        if target.provider != provider {
            return Err("request provider does not match session".into());
        }
        let mut state = lock_state(&self.state);
        let actor = state
            .executions
            .get(&target)
            .ok_or("request has no owned execution")?;
        if actor.live.requests.len() >= 32
            || serde_json::to_vec(&request)
                .map_err(|e| e.to_string())?
                .len()
                > 64 * 1024
        {
            return Err("pending request capacity reached".into());
        }
        if state.pending_native(provider, &request.id).is_some() {
            return Ok(());
        }
        request
            .extra
            .insert("nativeRequestId".into(), request.id.clone());
        request.id = serde_json::json!([
            target.provider,
            target.id,
            request.params.get("turnId"),
            request.id
        ]);
        change_locked(&mut state, &target, &SessionChange::Request { request });
        Ok(())
    }

    pub(crate) fn session_change(&self, id: &str, change: agent_core::session::SessionChange) {
        if let Ok(target) = SessionRef::from_thread_id(id) {
            change_locked(&mut lock_state(&self.state), &target, &change);
        }
    }

    pub(crate) fn resolve_native_request(&self, provider: ProviderKind, id: &Value) {
        let mut state = lock_state(&self.state);
        if let Some((target, request)) = state.pending_native(provider, id) {
            change_locked(
                &mut state,
                &target,
                &agent_core::session::SessionChange::ResolveRequest {
                    request_id: request.id.to_string(),
                },
            );
        }
    }

    pub(crate) fn broadcast(&self, line: &str) {
        let mut state = lock_state(&self.state);
        let deliveries = state
            .sessions
            .keys()
            .map(|id| (*id, line.to_owned()))
            .collect();
        deliver_locked(&mut state, deliveries);
    }

    pub(super) fn request_session(&self, id: &str) -> Option<SessionRef> {
        lock_state(&self.state)
            .pending_request(id)
            .map(|(session, _)| session)
    }

    pub(crate) fn claim_response(
        &self,
        session: SessionId,
        id: &str,
        result: &Value,
    ) -> Result<(ProviderKind, Value), String> {
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) {
            return Err("connection is closed".into());
        }
        let Some((target, request)) = state.pending_request(id) else {
            return Err("request was already answered or its execution has ended".into());
        };
        if request
            .extra
            .get("deliveryState")
            .is_some_and(|state| state != "awaiting")
        {
            return Err("request was already answered or its execution has ended".into());
        }
        let current = state
            .executions
            .get(&target)
            .ok_or("request execution is unavailable")?;
        let live_turn = current.live.turns.iter().flatten().any(|turn| {
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
            .clone();
        change_locked(
            &mut state,
            &target,
            &agent_core::session::SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_core::session::RequestDelivery::Sending,
            },
        );
        Ok((target.provider, native))
    }

    pub(crate) fn response_unknown(&self, id: &str) {
        let mut state = lock_state(&self.state);
        if let Some((target, _)) = state.pending_request(id) {
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
    let actor = state.executions.entry(target.clone()).or_default();
    if actor.update(change).is_err() {
        let connections: Vec<_> = state
            .subscriptions
            .values()
            .filter(|(id, _)| id == target)
            .map(|(_, connection)| *connection)
            .collect();
        for connection in connections {
            remove_session_locked(state, connection);
        }
        state.executions.retain(|_, actor| actor.release());
        return;
    }
    let deliveries = state
        .subscriptions
        .iter()
        .filter(|(_, (id, _))| id == target)
        .map(|(subscription, (_, connection))| {
            (
                *connection,
                serde_json::json!({"method":"host/session/update", "params":SessionUpdate {
                    subscription_id: *subscription, change: change.clone(),
                }})
                .to_string(),
            )
        })
        .collect();
    deliver_locked(state, deliveries);
    // Background navigation needs activity, not copies of
    // provider turn/item payloads outside a subscription.
    let active = match change {
        agent_core::session::SessionChange::Status { status } => Some(status.kind == "active"),
        agent_core::session::SessionChange::Turn { .. } => {
            state.executions.get(target).map(|actor| {
                actor
                    .live
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
            .map(|id| (*id, line.clone()))
            .collect();
        deliver_locked(state, deliveries);
    }
    state.executions.retain(|_, actor| actor.release());
}

fn deliver_locked(state: &mut State, deliveries: Vec<(SessionId, String)>) {
    // Keep transitions, delivery, and failure cleanup under the same lock.
    // A concurrent response cannot observe aliases for a failed delivery.
    for delivery in deliveries {
        let (session, line) = delivery;
        let failed = state
            .sessions
            .get(&session)
            .is_none_or(|sender| sender.try_send(line).is_err())
            .then_some(session);
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
    state
        .subscriptions
        .retain(|_, (_, owner)| *owner != session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::models::{Item, Thread, Turn};
    use agent_core::session::TextField;
    use serde_json::json;

    fn open(router: &SessionRouter, id: &str, limit: usize) -> SessionRead {
        router
            .begin_session_read(OpenSession {
                session: SessionRef::from_thread_id(id).unwrap(),
                limit,
            })
            .unwrap()
    }

    fn turn(router: &SessionRouter, completed: bool) {
        router.session_change(
            "native",
            SessionChange::Turn {
                turn: Turn {
                    id: "run".into(),
                    ..Default::default()
                },
                completed,
            },
        );
    }

    #[tokio::test]
    async fn history_is_not_retained_or_trimmed_and_subscriptions_do_not_pin_execution() {
        let router = SessionRouter::new();
        let mut connection = router.open_session(16);
        let mut turns: Vec<_> = (0..1001)
            .map(|id| {
                Arc::new(Turn {
                    id: id.to_string(),
                    ..Default::default()
                })
            })
            .collect();
        turns[0] = Arc::new(
            serde_json::from_value(json!({"id":"first","items":[{
                "id":"answer","type":"agentMessage","text":"x".repeat(5 * 1024 * 1024)
            }]}))
            .unwrap(),
        );
        let response = ThreadResponse {
            thread: Thread {
                id: Some("native".into()),
                turns: Some(turns),
                ..Default::default()
            },
            model: None,
            extra: Default::default(),
        };
        let request =
            RpcMessage::parse(r#"{"id":1,"method":"host/session/open","params":{}}"#).unwrap();
        router
            .finish_session_read(
                open(&router, "native", 1001),
                connection.id(),
                &request,
                response,
            )
            .unwrap();
        let reply: Value = serde_json::from_str(&connection.recv().await.unwrap()).unwrap();
        assert_eq!(
            reply["result"]["response"]["thread"]["turns"]
                .as_array()
                .unwrap()
                .len(),
            1001
        );
        assert_eq!(
            reply["result"]["response"]["thread"]["turns"][0]["items"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            5 * 1024 * 1024
        );
        assert!(lock_state(&router.state).executions.is_empty());
        turn(&router, false);
        assert_eq!(lock_state(&router.state).executions.len(), 1);
        turn(&router, true);
        let state = lock_state(&router.state);
        assert!(state.executions.is_empty());
        assert_eq!(state.subscriptions.len(), 1);
    }

    #[tokio::test]
    async fn completion_during_native_read_is_overlaid_before_live_updates() {
        let router = SessionRouter::new();
        let mut connection = router.open_session(16);
        let read = open(&router, "native", 5);
        turn(&router, false);
        router.session_change(
            "native",
            SessionChange::Item {
                turn_id: "run".into(),
                item: Item {
                    id: "answer".into(),
                    kind: Some("agentMessage".into()),
                    text: Some("start".into()),
                    ..Default::default()
                },
            },
        );
        router.session_change(
            "native",
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::Message,
                delta: " final".into(),
            },
        );
        turn(&router, true);
        let request =
            RpcMessage::parse(r#"{"id":1,"method":"host/session/open","params":{}}"#).unwrap();
        router
            .finish_session_read(
                read,
                connection.id(),
                &request,
                serde_json::from_value(json!({
                    "thread":{"id":"native","turns":[{"id":"run","status":"inProgress","items":[]}]}
                }))
                .unwrap(),
            )
            .unwrap();
        // Background activity messages can precede the response; subscription updates cannot.
        let reply = loop {
            let reply: Value = serde_json::from_str(&connection.recv().await.unwrap()).unwrap();
            assert_ne!(reply["method"], "host/session/update");
            if reply["id"] == 1 {
                break reply;
            }
        };
        let current = &reply["result"]["response"]["thread"]["turns"][0];
        assert_eq!(current["status"], "completed");
        assert_eq!(current["items"][0]["text"], "start final");
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[test]
    fn duplicate_input_is_blocked_only_while_execution_is_in_flight() {
        let router = SessionRouter::new();
        let input = json!({"threadId":"native","clientUserMessageId":"send"});
        router.begin_submission(&input).unwrap();
        assert!(router.begin_submission(&input).is_err());
        turn(&router, false);
        assert!(router.begin_submission(&input).is_err());
        turn(&router, true);
        assert!(lock_state(&router.state).executions.is_empty());
        // There is deliberately no completed receipt or time-based cache.
        let target = router.begin_submission(&input).unwrap();
        router.reject_submission(&target, "send");
        assert!(lock_state(&router.state).executions.is_empty());
    }

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
        crate::host_rpc::codex::event(&router, &notification).unwrap();
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
        assert!(lock_state(&router.state).executions.is_empty());
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
        router.session_change(
            thread,
            SessionChange::Turn {
                turn: agent_core::models::Turn {
                    id: "turn".into(),
                    ..Default::default()
                },
                completed: false,
            },
        );
        let line = serde_json::json!({"id":native,"method":"item/commandExecution/requestApproval",
                "params":{"threadId":thread,"turnId":"turn","availableDecisions":["accept","decline"]}}).to_string();
        router
            .request(target.provider, serde_json::from_str(&line).unwrap())
            .unwrap();
        let (_, request) = lock_state(&router.state)
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
            (target.provider, native.clone())
        );
    }
    let not_a_notification =
        serde_json::json!({"id":1,"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    // The Codex adapter never treats a request-shaped message as resolution.
    assert!(
        crate::host_rpc::codex::event(&router, &RpcMessage::parse(&not_a_notification).unwrap())
            .is_err()
    );
    assert!(router.request_session(&ids[0]).is_some());
    let resolved =
        serde_json::json!({"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    crate::host_rpc::codex::event(&router, &RpcMessage::parse(&resolved).unwrap()).unwrap();
    assert!(router.request_session(&ids[0]).is_none());
    assert_eq!(
        router.request_session(&ids[1]).unwrap().provider,
        ProviderKind::Claude
    );
}
