use agent_protocol::protocol;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::session_actor::SessionActor;
use agent_protocol::{
    models::ThreadResponse,
    operations::ServerRequest,
    protocol::Notification,
    session::{OpenedSession, ProviderKind, SessionChange, SessionRef},
};
#[cfg(test)]
use agent_transport::peer::RpcMessage;
use serde_json::Value;
use tokio::sync::mpsc;

pub(super) struct SessionLease {
    router: SessionRouter,
    target: SessionRef,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut state = lock_state(&self.router.state);
        if let Some(actor) = state.executions.get_mut(&self.target) {
            actor.leases -= 1;
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
    receiver: mpsc::Receiver<Vec<u8>>,
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    state: Weak<Mutex<State>>,
}

/// The response stream owns its subscription; dropping it unsubscribes.
pub struct HostSubscription {
    id: uuid::Uuid,
    receiver: mpsc::Receiver<Vec<u8>>,
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    state: Weak<Mutex<State>>,
}
impl HostSubscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let line = self.receiver.recv().await?;
        self.queued_bytes
            .fetch_sub(line.len(), std::sync::atomic::Ordering::Relaxed);
        Some(line)
    }
}
impl Drop for HostSubscription {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            lock_state(&state).subscriptions.remove(&self.id);
        }
        while let Ok(line) = self.receiver.try_recv() {
            self.queued_bytes
                .fetch_sub(line.len(), std::sync::atomic::Ordering::Relaxed);
        }
    }
}
pub struct HostReply {
    pub initial: Vec<u8>,
    pub updates: Option<HostSubscription>,
}
impl From<protocol::Response> for HostReply {
    fn from(initial: protocol::Response) -> Self {
        Self {
            initial: protocol::response_frame(initial).expect("response encodes"),
            updates: None,
        }
    }
}

impl HostSession {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
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
    principal: String,
    sender: mpsc::Sender<Vec<u8>>,
    bytes: Arc<std::sync::atomic::AtomicUsize>,
}
impl Outbound {
    fn try_send(&self, line: Vec<u8>) -> Result<(), mpsc::error::TrySendError<Vec<u8>>> {
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
    subscriptions: HashMap<uuid::Uuid, (SessionRef, SessionId, Outbound)>,
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
    fn pending_request(
        &self,
        id: &str,
    ) -> Option<(SessionRef, agent_protocol::operations::ServerRequest)> {
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
    ) -> Option<(SessionRef, agent_protocol::operations::ServerRequest)> {
        self.executions
            .iter()
            .filter(|(target, _)| target.provider == provider)
            .find_map(|(target, actor)| {
                actor
                    .live
                    .requests
                    .values()
                    .find(|request| request.native_request_id.as_ref() == Some(id))
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
        self.open_authenticated_session(capacity, None)
    }

    pub(crate) fn principal(&self, session: SessionId) -> Result<String, String> {
        lock_state(&self.state)
            .sessions
            .get(&session)
            .map(|outbound| outbound.principal.clone())
            .ok_or_else(|| "connection is closed".into())
    }

    pub(crate) fn open_authenticated_session(
        &self,
        capacity: usize,
        principal: Option<String>,
    ) -> HostSession {
        assert!(capacity > 0, "a Host session queue must have capacity");
        let (sender, receiver) = mpsc::channel(capacity);
        let mut state = lock_state(&self.state);
        let id = allocate_session_id(&mut state);
        let queued_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        state.sessions.insert(
            id,
            Outbound {
                principal: principal.unwrap_or_else(|| format!("session:{id}")),
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

    pub(super) fn submission_lock(&self, target: &SessionRef) -> Arc<tokio::sync::Mutex<()>> {
        lock_state(&self.state)
            .executions
            .entry(target.clone())
            .or_default()
            .submission_lock
            .clone()
    }

    pub(super) fn overlay_execution(&self, target: &SessionRef, response: &mut ThreadResponse) {
        if let Some(actor) = lock_state(&self.state).executions.get(target) {
            actor.overlay(response);
        }
    }

    pub(super) fn submission_receipt(
        &self,
        target: &SessionRef,
        id: &str,
    ) -> Option<agent_protocol::operations::SubmissionReceipt> {
        let state = lock_state(&self.state);
        match state.executions.get(target)?.live.submissions.get(id)? {
            agent_protocol::session::SubmissionDelivery::Accepted { turn_id } => {
                Some(agent_protocol::operations::SubmissionReceipt {
                    turn_id: turn_id.clone(),
                })
            }
            _ => None,
        }
    }

    pub(super) fn begin_submission(
        &self,
        target: &SessionRef,
        id: &str,
    ) -> Result<(), super::service::Failure> {
        use super::service::Failure;
        if id.is_empty() || id.len() > 256 {
            return Err(Failure::new(
                "invalid_params",
                "clientUserMessageId is required",
            ));
        }
        let mut state = lock_state(&self.state);
        let actor = state.executions.entry(target.clone()).or_default();
        if actor.live.submissions.get(id).is_some_and(|delivery| {
            *delivery != agent_protocol::session::SubmissionDelivery::Rejected
        }) {
            return Err(Failure::unknown(
                "submission_outcome_unknown",
                "submission is already in flight; read the session before sending again",
            ));
        }
        // Rejections have no provider side effect. Keep them available for a
        // reconnect until another input needs the bounded execution capacity.
        if actor.live.submissions.len() >= 128 {
            actor.live.submissions.retain(|_, delivery| {
                *delivery != agent_protocol::session::SubmissionDelivery::Rejected
            });
        }
        if actor.live.submissions.len() >= 128 {
            return Err(Failure::new(
                "input_capacity_reached",
                "active input capacity reached",
            ));
        }
        change_locked(
            &mut state,
            target,
            &SessionChange::Submission {
                id: id.into(),
                delivery: agent_protocol::session::SubmissionDelivery::Sending,
            },
        );
        Ok(())
    }

    pub(super) fn finish_submission(
        &self,
        target: &SessionRef,
        id: &str,
        delivery: agent_protocol::session::SubmissionDelivery,
    ) {
        let mut state = lock_state(&self.state);
        // A completed execution may already have retired its inputs. Do not
        // recreate execution state from a late RPC completion.
        if state
            .executions
            .get(target)
            .is_some_and(|actor| actor.live.submissions.contains_key(id))
        {
            change_locked(
                &mut state,
                target,
                &SessionChange::Submission {
                    id: id.into(),
                    delivery,
                },
            );
        }
    }

    pub(crate) fn current_turn(
        &self,
        target: &SessionRef,
        turn_id: &str,
    ) -> Option<agent_protocol::models::Turn> {
        lock_state(&self.state)
            .executions
            .get(target)
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

    pub(super) fn retain_execution(&self, target: SessionRef) -> Result<SessionLease, String> {
        target.validate()?;
        let mut state = lock_state(&self.state);
        state.executions.entry(target.clone()).or_default().leases += 1;
        Ok(SessionLease {
            router: self.clone(),
            target,
        })
    }

    pub(super) fn finish_session_read(
        &self,
        read: SessionLease,
        session: SessionId,
        mut response: ThreadResponse,
    ) -> Result<HostReply, String> {
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
        response
            .thread
            .defer_item_details(agent_protocol::models::MAX_INLINE_ITEM_BYTES);
        let subscription_id = uuid::Uuid::new_v4();
        let mut opened = OpenedSession {
            session: read.target.clone(),
            subscription_id: uuid::Uuid::nil(),
            response,
        };
        let mut line = protocol::encode(protocol::Response::Success { result: &opened })
            .map_err(|error| error.to_string())?;
        if line.len() > MAX_QUEUED_BYTES {
            // Many individually small items can also exceed one physical RPC.
            // Defer bodies without discarding turns or keeping a Host snapshot.
            opened.response.thread.defer_item_details(0);
            line = protocol::encode(protocol::Response::Success { result: &opened })
                .map_err(|error| error.to_string())?;
        }
        if line.len() > MAX_QUEUED_BYTES {
            let error = protocol::Response::error(
                "response_too_large",
                &"Session metadata exceeds the RPC limit; request fewer turns",
            )
            .map_err(|error| error.to_string())?;
            return Ok(error.into());
        }
        let (sender, receiver) = mpsc::channel(state.sessions[&session].sender.max_capacity());
        let queued_bytes = state.sessions[&session].bytes.clone();
        let principal = state.sessions[&session].principal.clone();
        state.subscriptions.insert(
            subscription_id,
            (
                read.target.clone(),
                session,
                Outbound {
                    principal,
                    sender,
                    bytes: queued_bytes.clone(),
                },
            ),
        );
        Ok(HostReply {
            initial: line,
            updates: Some(HostSubscription {
                id: subscription_id,
                receiver,
                queued_bytes,
                state: Arc::downgrade(&self.state),
            }),
        })
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
            for (id, delivery) in &thread.submissions {
                if matches!(
                    delivery,
                    agent_protocol::session::SubmissionDelivery::Sending
                ) {
                    change_locked(
                        &mut state,
                        &target,
                        &SessionChange::Submission {
                            id: id.clone(),
                            delivery: agent_protocol::session::SubmissionDelivery::Unknown,
                        },
                    );
                }
            }
            change_locked(
                &mut state,
                &target,
                &SessionChange::Status {
                    status: agent_protocol::models::ThreadStatus {
                        kind: agent_protocol::models::ThreadStatusKind::SystemError,
                    },
                },
            );
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

    pub(crate) fn send(
        &self,
        session: SessionId,
        notification: Notification,
    ) -> Result<(), String> {
        self.send_frame(
            session,
            protocol::encode(notification).map_err(|error| error.to_string())?,
        )
    }
    fn send_frame(&self, session: SessionId, line: Vec<u8>) -> Result<(), String> {
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
        target: SessionRef,
        mut request: ServerRequest,
    ) -> Result<(), String> {
        if request.params.get("threadId").and_then(Value::as_str)
            != Some(target.thread_id().as_str())
        {
            return Err("request provider does not match session".into());
        }
        let provider = target.provider;
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
        request.native_request_id = Some(request.id.clone());
        request.id = serde_json::json!([
            target.provider,
            target.id,
            request.params.get("turnId"),
            request.id
        ]);
        change_locked(&mut state, &target, &SessionChange::Request { request });
        Ok(())
    }

    pub(crate) fn session_change(
        &self,
        target: &SessionRef,
        change: agent_protocol::session::SessionChange,
    ) {
        change_locked(&mut lock_state(&self.state), target, &change);
    }

    pub(crate) fn resolve_native_request(&self, provider: ProviderKind, id: &Value) {
        let mut state = lock_state(&self.state);
        if let Some((target, request)) = state.pending_native(provider, id) {
            change_locked(
                &mut state,
                &target,
                &agent_protocol::session::SessionChange::ResolveRequest {
                    request_id: request.id.to_string(),
                },
            );
        }
    }

    pub(crate) fn broadcast(&self, notification: Notification) {
        let line = protocol::encode(notification).expect("notification serializes");
        let mut state = lock_state(&self.state);
        let deliveries = state
            .sessions
            .keys()
            .map(|id| (*id, line.clone()))
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
            .delivery_state
            .is_some_and(|state| state != agent_protocol::session::RequestDelivery::Awaiting)
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
        agent_protocol::operations::validate_answer(&request, result)?;
        let native = request
            .native_request_id
            .as_ref()
            .ok_or("request execution is unavailable")?
            .clone();
        change_locked(
            &mut state,
            &target,
            &agent_protocol::session::SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_protocol::session::RequestDelivery::Sending,
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
                &agent_protocol::session::SessionChange::RequestDelivery {
                    request_id: id.into(),
                    state: agent_protocol::session::RequestDelivery::Unknown,
                },
            );
        }
    }
}

fn change_locked(
    state: &mut State,
    target: &SessionRef,
    change: &agent_protocol::session::SessionChange,
) {
    let actor = state.executions.entry(target.clone()).or_default();
    let Ok(next) = change.apply(&actor.live) else {
        let connections: Vec<_> = state
            .subscriptions
            .values()
            .filter(|(id, _, _)| id == target)
            .map(|(_, connection, _)| *connection)
            .collect();
        for connection in connections {
            remove_session_locked(state, connection);
        }
        state.executions.retain(|_, actor| actor.release());
        return;
    };
    actor.live = next;
    let mut failed = Vec::new();
    let line = protocol::encode(change).expect("change encodes");
    for (id, connection, output) in state.subscriptions.values() {
        if id != target {
            continue;
        }
        if output.try_send(line.clone()).is_err() {
            failed.push(*connection);
        }
    }
    for connection in failed {
        remove_session_locked(state, connection);
    }
    // Background navigation needs activity, not copies of
    // provider turn/item payloads outside a subscription.
    let active = match change {
        agent_protocol::session::SessionChange::Status { status } => {
            Some(status.kind == agent_protocol::models::ThreadStatusKind::Active)
        }
        agent_protocol::session::SessionChange::Turn { .. } => {
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
        let line = protocol::encode(Notification::Activity {
            session: target.clone(), active,
            finished: !active && matches!(change, agent_protocol::session::SessionChange::Turn {completed:true, turn} if turn.status.as_deref() == Some("completed")),
        }).expect("activity encodes");
        let deliveries = state
            .sessions
            .keys()
            .map(|id| (*id, line.clone()))
            .collect();
        deliver_locked(state, deliveries);
    }
    state.executions.retain(|_, actor| actor.release());
}

fn deliver_locked(state: &mut State, deliveries: Vec<(SessionId, Vec<u8>)>) {
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
        .retain(|_, (_, owner, _)| *owner != session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::models::{Item, Thread, Turn};
    use agent_protocol::session::TextField;
    use serde_json::json;

    fn open(router: &SessionRouter, id: &str) -> SessionLease {
        router
            .retain_execution(SessionRef::from_thread_id(id).unwrap())
            .unwrap()
    }

    fn turn(router: &SessionRouter, completed: bool) {
        router.session_change(
            &SessionRef {
                provider: ProviderKind::Codex,
                id: "native".into(),
            },
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
        let connection = router.open_session(16);
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
        };
        let response = router
            .finish_session_read(open(&router, "native"), connection.id(), response)
            .unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
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
            256
        );
        assert_eq!(
            reply["result"]["response"]["thread"]["turns"][0]["deferredItemIds"],
            json!(["answer"])
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
    async fn aggregate_item_bodies_are_deferred_to_fit_one_rpc() {
        let router = SessionRouter::new();
        let connection = router.open_session(16);
        let mut items: Vec<_> = (0..40).map(|id| json!({"id":id.to_string(),"type":"agentMessage","text":"x".repeat(512 * 1024)})).collect();
        items.push(
            json!({"id":"tool","type":"commandExecution","aggregatedOutput":"z".repeat(8192)}),
        );
        let response = serde_json::from_value(
            json!({"thread":{"id":"native","turns":[{"id":"turn","items":items}]}}),
        )
        .unwrap();
        let response = router
            .finish_session_read(open(&router, "native"), connection.id(), response)
            .unwrap();
        let line = response.initial;
        assert!(line.len() < 64 * 1024);
        let reply =
            protocol::decode::<protocol::Response<agent_protocol::session::OpenedSession>>(&line)
                .unwrap()
                .into_value();
        let turn = &reply["result"]["response"]["thread"]["turns"][0];
        assert_eq!(turn["items"].as_array().unwrap().len(), 41);
        assert_eq!(turn["deferredItemIds"].as_array().unwrap().len(), 41);
        router.ensure_session(connection.id()).unwrap();
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[tokio::test]
    async fn oversized_rpc_returns_an_error_without_dropping_the_connection_or_subscribing() {
        let router = SessionRouter::new();
        let connection = router.open_session(16);
        for _ in 0..2 {
            let response = serde_json::from_value(
                json!({"thread":{"id":"native","name":"x".repeat(MAX_QUEUED_BYTES + 1)}}),
            )
            .unwrap();
            let response = router
                .finish_session_read(open(&router, "native"), connection.id(), response)
                .unwrap();
            let reply = agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
            >(&response.initial)
            .unwrap()
            .into_value();
            assert_eq!(reply["error"]["code"], "response_too_large");
            router.ensure_session(connection.id()).unwrap();
        }
        assert!(lock_state(&router.state).subscriptions.is_empty());
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[tokio::test]
    async fn completion_during_native_read_is_overlaid_before_live_updates() {
        let router = SessionRouter::new();
        let connection = router.open_session(16);
        let read = open(&router, "native");
        turn(&router, false);
        router.session_change(
            &SessionRef {
                provider: ProviderKind::Codex,
                id: "native".into(),
            },
            SessionChange::Item {
                turn_id: "run".into(),
                item: Item {
                    id: "answer".into(),
                    kind: Some("agentMessage".into()),
                    text: Some("start".into()),
                    ..Default::default()
                }
                .into(),
            },
        );
        router.session_change(
            &SessionRef {
                provider: ProviderKind::Codex,
                id: "native".into(),
            },
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::Message,
                delta: " final".into(),
            },
        );
        turn(&router, true);
        let response = router
            .finish_session_read(
                read,
                connection.id(),
                serde_json::from_value(json!({
                    "thread":{"id":"native","turns":[{"id":"run","status":"inProgress","items":[]}]}
                }))
                .unwrap(),
            )
            .unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        let current = &reply["result"]["response"]["thread"]["turns"][0];
        assert_eq!(current["status"], "completed");
        assert_eq!(current["items"][0]["text"], "start final");
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[test]
    fn completion_does_not_discard_unconfirmed_input_and_accepted_receipts_replay() {
        let router = SessionRouter::new();
        let target = SessionRef {
            provider: ProviderKind::Codex,
            id: "native".into(),
        };
        router.begin_submission(&target, "send").unwrap();
        assert!(router.begin_submission(&target, "send").is_err());
        turn(&router, false);
        assert!(router.begin_submission(&target, "send").is_err());
        turn(&router, true);
        assert!(router.begin_submission(&target, "send").is_err());
        router.finish_submission(
            &target,
            "send",
            agent_protocol::session::SubmissionDelivery::Accepted {
                turn_id: Some("run".into()),
            },
        );
        assert_eq!(
            router
                .submission_receipt(&target, "send")
                .unwrap()
                .turn_id
                .as_deref(),
            Some("run")
        );
        let mut response: ThreadResponse =
            serde_json::from_value(json!({"thread":{"id":"native","turns":[]}})).unwrap();
        router.overlay_execution(&target, &mut response);
        assert_eq!(
            response.thread.submissions["send"],
            agent_protocol::session::SubmissionDelivery::Accepted {
                turn_id: Some("run".into())
            }
        );
    }

    #[test]
    fn provider_exit_clears_activity_but_preserves_uncertain_delivery() {
        use agent_protocol::{
            models::{ThreadStatus, ThreadStatusKind},
            session::SubmissionDelivery,
        };
        let router = SessionRouter::new();
        let target = SessionRef::from_thread_id("native").unwrap();
        router.begin_submission(&target, "input").unwrap();
        router.session_change(
            &target,
            SessionChange::Status {
                status: ThreadStatus {
                    kind: ThreadStatusKind::Active,
                },
            },
        );
        assert_eq!(
            lock_state(&router.state).executions[&target]
                .live
                .status
                .as_ref()
                .unwrap()
                .kind,
            ThreadStatusKind::Active
        );
        router.fail_provider(ProviderKind::Codex, "provider stopped");
        let state = lock_state(&router.state);
        let live = &state.executions[&target].live;
        assert!(live.status.is_none());
        assert_eq!(live.submissions["input"], SubmissionDelivery::Unknown);
    }

    #[tokio::test]
    async fn byte_budget_disconnects_only_the_slow_connection() {
        let router = SessionRouter::new();
        let slow = router.open_session(100);
        let mut healthy = router.open_session(100);
        router
            .send_frame(slow.id(), vec![0; MAX_QUEUED_BYTES])
            .unwrap();
        assert!(router.send_frame(slow.id(), b"overflow".to_vec()).is_err());
        router
            .send_frame(healthy.id(), b"current".to_vec())
            .unwrap();
        assert_eq!(healthy.recv().await.as_deref(), Some(b"current".as_slice()));
    }

    #[test]
    fn slow_session_subscribers_do_not_block_other_devices_or_destroy_current_state() {
        let router = SessionRouter::new();
        let slow = router.open_session(1);
        let healthy = router.open_session(8);
        let response: ThreadResponse =
            serde_json::from_value(serde_json::json!({"thread":{"id":"native","turns":[]}}))
                .unwrap();
        let mut streams = Vec::new();
        for connection in [slow.id(), healthy.id()] {
            let read = router
                .retain_execution(SessionRef::from_thread_id("native").unwrap())
                .unwrap();
            let response = router
                .finish_session_read(read, connection, response.clone())
                .unwrap();
            streams.push(response);
        }
        let notification = RpcMessage::parse(r#"{"method":"thread/status/changed","params":{"threadId":"native","status":{"type":"active"}}}"#).unwrap();
        crate::host_rpc::codex::event(&router, &notification).unwrap();
        crate::host_rpc::codex::event(&router, &notification).unwrap();
        assert!(router.ensure_session(slow.id()).is_err());
        assert!(router.ensure_session(healthy.id()).is_ok());
        let initial =
            protocol::decode::<protocol::Response<agent_protocol::session::OpenedSession>>(
                &streams[1].initial,
            )
            .unwrap()
            .into_value();
        let update = protocol::decode::<SessionChange>(
            &streams[1]
                .updates
                .as_mut()
                .unwrap()
                .receiver
                .try_recv()
                .unwrap(),
        )
        .unwrap();
        assert!(initial.get("result").is_some());
        assert!(initial["result"].get("subscriptionId").is_none());
        assert!(matches!(update, SessionChange::Status { .. }));
        assert!(
            streams[0]
                .updates
                .as_mut()
                .unwrap()
                .receiver
                .try_recv()
                .is_ok()
        );
        assert!(matches!(
            streams[0].updates.as_mut().unwrap().receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[test]
    fn subscriptions_share_the_connection_budget_and_drop_releases_unread_bytes() {
        let router = SessionRouter::new();
        let connection = router.open_session(8);
        let response: ThreadResponse =
            serde_json::from_value(json!({"thread":{"id":"native","turns":[]}})).unwrap();
        let first = router
            .finish_session_read(open(&router, "native"), connection.id(), response.clone())
            .unwrap()
            .updates
            .unwrap();
        let second = router
            .finish_session_read(open(&router, "native"), connection.id(), response)
            .unwrap()
            .updates
            .unwrap();
        let (a, b, budget) = {
            let state = lock_state(&router.state);
            (
                state.subscriptions[&first.id].2.clone(),
                state.subscriptions[&second.id].2.clone(),
                state.sessions[&connection.id()].bytes.clone(),
            )
        };
        a.try_send(vec![0; MAX_QUEUED_BYTES / 2]).unwrap();
        assert!(b.try_send(vec![0; MAX_QUEUED_BYTES / 2 + 1]).is_err());
        drop(first);
        b.try_send(vec![0; MAX_QUEUED_BYTES / 2 + 1]).unwrap();
        drop(second);
        assert_eq!(budget.load(std::sync::atomic::Ordering::Relaxed), 0);
        router.ensure_session(connection.id()).unwrap();
    }

    #[test]
    fn cancellation_and_disconnect_release_an_unfinished_open() {
        let router = SessionRouter::new();
        let connection = router.open_session(4);
        let read = router
            .retain_execution(SessionRef::from_thread_id("native").unwrap())
            .unwrap();
        drop(connection);
        drop(read);
        // Fill and evict idle entries: leaked leases would eventually prevent admission.
        for id in 0..200 {
            let read = router
                .retain_execution(SessionRef::from_thread_id(&id.to_string()).unwrap())
                .unwrap();
            drop(read);
        }
    }
}
#[test]
fn identical_native_request_ids_keep_their_provider_owner() {
    let router = SessionRouter::new();
    let connection = router.open_session(32);
    let native = serde_json::json!("claude-permission:shared-native-id");
    let mut ids = Vec::new();
    for thread in ["codex-native", "claude:claude-native"] {
        let target = SessionRef::from_thread_id(thread).unwrap();
        let read = router.retain_execution(target.clone()).unwrap();
        let response = serde_json::from_value(serde_json::json!({"thread":{"id":thread,
                "turns":[{"id":"turn","status":"inProgress","items":[]}]}}))
        .unwrap();
        let _response = router
            .finish_session_read(read, connection.id(), response)
            .unwrap();
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "turn".into(),
                    ..Default::default()
                },
                completed: false,
            },
        );
        let line = serde_json::json!({"id":native,"method":"item/commandExecution/requestApproval",
                "params":{"threadId":thread,"turnId":"turn","availableDecisions":["accept","decline"]}}).to_string();
        router
            .request(target.clone(), serde_json::from_str(&line).unwrap())
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
