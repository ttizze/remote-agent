use agent_protocol::protocol;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::session_actor::SessionActor;
use agent_protocol::{
    models::ThreadResponse,
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
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    state: Weak<Mutex<State>>,
}

/// The response stream owns its subscription; dropping it unsubscribes.
pub struct HostSubscription {
    id: uuid::Uuid,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    state: Weak<Mutex<State>>,
}
impl HostSubscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let line = self.receiver.recv().await?;
        self.queued_bytes.fetch_sub(
            frame_cost(line.capacity()),
            std::sync::atomic::Ordering::Relaxed,
        );
        Some(line)
    }
}
impl Drop for HostSubscription {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            lock_state(&state).subscriptions.remove(&self.id);
        }
        while let Ok(line) = self.receiver.try_recv() {
            self.queued_bytes.fetch_sub(
                frame_cost(line.capacity()),
                std::sync::atomic::Ordering::Relaxed,
            );
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
        self.queued_bytes.fetch_sub(
            frame_cost(line.capacity()),
            std::sync::atomic::Ordering::Relaxed,
        );
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

// Count allocated payload capacity and the entry, including empty frames.
// The channel has no message-count cutoff: a buffered provider burst must not
// disconnect a healthy client before its network writer gets scheduled.
fn frame_cost(capacity: usize) -> usize {
    capacity + std::mem::size_of::<Vec<u8>>()
}
#[derive(Clone)]
struct Outbound {
    principal: String,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    bytes: Arc<std::sync::atomic::AtomicUsize>,
}
impl Outbound {
    fn try_send(
        &self,
        session: SessionId,
        line: Vec<u8>,
    ) -> Result<(), mpsc::error::TrySendError<Vec<u8>>> {
        use std::sync::atomic::Ordering::Relaxed;
        let length = frame_cost(line.capacity());
        if self
            .bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(length)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!(
                "session={session} reason=byte_limit queued_bytes={} frame_bytes={} frame_cost={length} limit_bytes={MAX_QUEUED_BYTES}",
                self.bytes.load(Relaxed), line.len()));
            return Err(mpsc::error::TrySendError::Full(line));
        }
        self.sender.send(line).map_err(|error| {
            self.bytes.fetch_sub(length, Relaxed);
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!(
                "session={session} reason=receiver_closed frame_bytes={}", error.0.len()));
            mpsc::error::TrySendError::Closed(error.0)
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
    fn pending_request(&self, id: &str) -> Option<(SessionRef, &super::requests::PendingRequest)> {
        self.executions.iter().find_map(|(target, actor)| {
            actor
                .pending_requests
                .get(id)
                .map(|pending| (target.clone(), pending))
        })
    }
    fn pending_native(
        &self,
        instance: uuid::Uuid,
        id: &Value,
    ) -> Option<(SessionRef, agent_protocol::ids::RequestId)> {
        self.executions.iter().find_map(|(target, actor)| {
            actor
                .pending_requests
                .values()
                .find(|pending| {
                    pending.origin.instance == instance && &pending.origin.native_id == id
                })
                .map(|pending| (target.clone(), pending.request.id.clone()))
        })
    }
}

impl SessionRouter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub(crate) fn open_session(&self) -> HostSession {
        self.open_authenticated_session(None)
    }

    pub(crate) fn principal(&self, session: SessionId) -> Result<String, String> {
        lock_state(&self.state)
            .sessions
            .get(&session)
            .map(|outbound| outbound.principal.clone())
            .ok_or_else(|| "connection is closed".into())
    }

    pub(crate) fn open_authenticated_session(&self, principal: Option<String>) -> HostSession {
        let (sender, receiver) = mpsc::unbounded_channel();
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

    pub(crate) fn execution_workspace(&self, target: &SessionRef, cwd: Option<&str>) {
        if let Some(actor) = lock_state(&self.state).executions.get_mut(target) {
            actor.live.cwd = cwd.map(str::to_owned);
        }
    }
    pub(crate) fn active_sessions_in(
        &self,
        dir: &std::path::Path,
    ) -> std::collections::HashSet<SessionRef> {
        lock_state(&self.state)
            .executions
            .iter()
            .filter(|(_, actor)| {
                actor.live.status == agent_protocol::models::SessionStatus::Running
                    && actor
                        .live
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| std::path::Path::new(cwd).starts_with(dir))
            })
            .map(|(session, _)| session.clone())
            .collect()
    }

    pub(crate) fn overlay_execution(&self, target: &SessionRef, response: &mut ThreadResponse) {
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
                    .rfind(|turn| turn.id.as_str() == turn_id)
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
        if response.thread.id.as_ref() != Some(&read.target) {
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
        let (sender, receiver) = mpsc::unbounded_channel();
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
            .map(|(target, actor)| {
                (
                    target.clone(),
                    actor.live.clone(),
                    actor.pending_requests.keys().cloned().collect::<Vec<_>>(),
                )
            })
            .collect();
        for (target, thread, requests) in live {
            for id in requests {
                change_locked(
                    &mut state,
                    &target,
                    &SessionChange::ResolveRequest { request_id: id },
                );
            }
            for turn in thread
                .turns
                .iter()
                .flatten()
                .filter(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
            {
                let mut turn = (**turn).clone();
                turn.status = agent_protocol::execution::TurnStatus::Failed;
                turn.error = Some(agent_protocol::execution::ExecutionError {
                    category: agent_protocol::execution::ErrorCategory::Network,
                    message: message.into(),
                    ..Default::default()
                });
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
                    status: agent_protocol::models::SessionStatus::Unavailable,
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
        match sender.try_send(session, line) {
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
        origin: super::requests::RequestOrigin,
        adapted: super::requests::AdaptedRequest,
    ) -> Result<(), String> {
        let provider = match origin.destination {
            super::requests::RequestDestination::Codex { .. } => ProviderKind::Codex,
            super::requests::RequestDestination::Claude { .. } => ProviderKind::Claude,
        };
        if target.provider != provider {
            return Err("request provider does not match session".into());
        }
        if !origin.is_alive() {
            return Err("request source is closed".into());
        }
        let mut state = lock_state(&self.state);
        if state
            .pending_native(origin.instance, &origin.native_id)
            .is_some()
        {
            return Ok(());
        }
        if state
            .executions
            .values()
            .map(|actor| actor.pending_requests.len())
            .sum::<usize>()
            >= 256
            || state
                .executions
                .get(&target)
                .is_some_and(|actor| actor.pending_requests.len() >= 32)
            || serde_json::to_vec(&adapted.request)
                .map_err(|e| e.to_string())?
                .len()
                > 64 * 1024
        {
            return Err("pending request capacity reached".into());
        }
        if let agent_protocol::requests::RequestTarget::Turn { turn_id, .. } =
            &adapted.request.target
            && state.executions.get(&target).is_none_or(|actor| {
                !actor.live.turns.iter().flatten().any(|turn| {
                    &turn.id == turn_id
                        && turn.status == agent_protocol::execution::TurnStatus::Running
                })
            })
        {
            return Err("request has no owned live turn".into());
        }
        let request = adapted.request;
        state
            .executions
            .entry(target.clone())
            .or_default()
            .pending_requests
            .insert(
                request.id.clone(),
                super::requests::PendingRequest {
                    request: request.clone(),
                    origin,
                    answers: adapted.answers,
                },
            );
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

    pub(crate) fn resolve_native_request(&self, instance: uuid::Uuid, id: &Value) {
        let mut state = lock_state(&self.state);
        if let Some((target, request_id)) = state.pending_native(instance, id) {
            change_locked(
                &mut state,
                &target,
                &SessionChange::ResolveRequest { request_id },
            );
        }
    }

    pub(crate) fn close_request_source(&self, instance: uuid::Uuid) {
        let mut state = lock_state(&self.state);
        let requests: Vec<_> = state
            .executions
            .iter()
            .flat_map(|(target, actor)| {
                actor
                    .pending_requests
                    .values()
                    .filter(move |pending| pending.origin.instance == instance)
                    .map(move |pending| (target.clone(), pending.request.id.clone()))
            })
            .collect();
        for (target, request_id) in requests {
            change_locked(
                &mut state,
                &target,
                &SessionChange::ResolveRequest { request_id },
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
        answer: &agent_protocol::requests::Answer,
    ) -> Result<(super::requests::RequestOrigin, Value), String> {
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) {
            return Err("connection is closed".into());
        }
        let (target, pending) = state
            .pending_request(id)
            .ok_or("request was already answered or its execution has ended")?;
        if pending.request.delivery != agent_protocol::session::RequestDelivery::Awaiting
            || !pending.origin.is_alive()
        {
            return Err("request was already answered or its source has ended".into());
        }
        if let agent_protocol::requests::RequestTarget::Turn { turn_id, .. } =
            &pending.request.target
            && !state.executions[&target]
                .live
                .turns
                .iter()
                .flatten()
                .any(|turn| {
                    &turn.id == turn_id
                        && turn.status == agent_protocol::execution::TurnStatus::Running
                })
        {
            return Err("request execution has ended".into());
        }
        let result = pending.answers.translate(&pending.request.body, answer)?;
        let origin = pending.origin.clone();
        change_locked(
            &mut state,
            &target,
            &SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_protocol::session::RequestDelivery::Sending,
            },
        );
        Ok((origin, result))
    }

    pub(crate) fn response_delivery(
        &self,
        id: &str,
        delivery: agent_protocol::session::RequestDelivery,
    ) {
        let mut state = lock_state(&self.state);
        if let Some((target, pending)) = state.pending_request(id)
            && pending.request.delivery == agent_protocol::session::RequestDelivery::Sending
        {
            change_locked(
                &mut state,
                &target,
                &SessionChange::RequestDelivery {
                    request_id: id.into(),
                    state: delivery,
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
    if let SessionChange::Turn {
        turn,
        completed: true,
    } = change
    {
        let expired: Vec<_> = state.executions.get(target).into_iter().flat_map(|actor| actor.pending_requests.values()).filter(|pending| matches!(&pending.request.target, agent_protocol::requests::RequestTarget::Turn { turn_id, .. } if turn_id == &turn.id)).map(|pending| pending.request.id.clone()).collect();
        for request_id in expired {
            change_locked(state, target, &SessionChange::ResolveRequest { request_id });
        }
    }
    if let SessionChange::Item { turn_id, item } = change {
        let terminal = matches!(
            item.status,
            agent_protocol::execution::ItemStatus::Completed
                | agent_protocol::execution::ItemStatus::Failed
                | agent_protocol::execution::ItemStatus::Declined
                | agent_protocol::execution::ItemStatus::Interrupted
        );
        let resolved: Vec<_> = state.executions.get(target).into_iter().flat_map(|actor| actor.pending_requests.values()).filter(|pending| (terminal || pending.request.delivery != agent_protocol::session::RequestDelivery::Awaiting) && matches!(&pending.request.target, agent_protocol::requests::RequestTarget::Turn {turn_id: request_turn, item_id} if request_turn == turn_id && item_id.as_ref() == Some(&item.id))).map(|pending| pending.request.id.clone()).collect();
        for request_id in resolved {
            change_locked(state, target, &SessionChange::ResolveRequest { request_id });
        }
    }
    let actor = state.executions.entry(target.clone()).or_default();
    match change {
        SessionChange::Request { .. } => {}
        SessionChange::RequestDelivery { request_id, state } => {
            if let Some(pending) = actor.pending_requests.get_mut(request_id) {
                pending.request.delivery = *state;
            }
        }
        SessionChange::ResolveRequest { request_id } => {
            actor.pending_requests.remove(request_id);
        }
        _ => {
            // Native providers can finish a turn before independent item
            // notifications arrive. Completed history belongs to subscribers,
            // not this live execution owner; forward those updates below.
            let turn_id = match change {
                SessionChange::Item { turn_id, .. }
                | SessionChange::RemoveItem { turn_id, .. }
                | SessionChange::Text { turn_id, .. }
                | SessionChange::ReasoningPart { turn_id, .. }
                | SessionChange::Error { turn_id, .. } => Some(turn_id),
                _ => None,
            };
            if turn_id.is_none_or(|id| actor.live.turns.iter().flatten().any(|turn| &turn.id == id))
            {
                actor.live = match change.apply(&actor.live) {
                    Ok(next) => next,
                    Err(reason) => {
                        tracing::warn!(target: "bex", operation = "host.session.invalid_update", message = %reason);
                        state.executions.retain(|_, actor| actor.release());
                        return;
                    }
                };
            }
        }
    }
    let mut failed = Vec::new();
    let line = protocol::encode(change).expect("change encodes");
    for (id, connection, output) in state.subscriptions.values() {
        if id != target {
            continue;
        }
        if output.try_send(*connection, line.clone()).is_err() {
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
            Some(*status == agent_protocol::models::SessionStatus::Running)
        }
        agent_protocol::session::SessionChange::Turn { .. } => {
            state.executions.get(target).map(|actor| {
                actor
                    .live
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
            })
        }
        _ => None,
    };
    if let Some(active) = active {
        let line = protocol::encode(Notification::Activity {
            session: target.clone(), active,
            finished: !active && matches!(change, agent_protocol::session::SessionChange::Turn {completed:true, turn} if turn.status == agent_protocol::execution::TurnStatus::Completed),
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
            .is_none_or(|sender| sender.try_send(session, line).is_err())
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
            .retain_execution(
                SessionRef::new(agent_protocol::session::ProviderKind::Codex, id.to_string())
                    .unwrap(),
            )
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
    async fn malformed_delta_keeps_all_subscriptions_and_the_last_valid_item() {
        let router = SessionRouter::new();
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let connections = [router.open_session(), router.open_session()];
        let response = ThreadResponse {
            thread: Thread {
                id: Some(target.clone()),
                ..Default::default()
            },
            model: None,
        };
        let mut streams = Vec::new();
        for connection in &connections {
            streams.push(
                router
                    .finish_session_read(open(&router, "native"), connection.id(), response.clone())
                    .unwrap()
                    .updates
                    .unwrap(),
            );
        }
        turn(&router, false);
        router.session_change(
            &target,
            SessionChange::Item {
                turn_id: "run".into(),
                item: Arc::new(Item::new(
                    "item".into(),
                    agent_protocol::execution::ItemStatus::Running,
                    agent_protocol::items::ItemBody::AssistantText {
                        text: "kept".into(),
                        phase: agent_protocol::items::AssistantPhase::Unknown,
                        citation: None,
                    },
                )),
            },
        );
        for stream in &mut streams {
            stream.recv().await.unwrap();
            stream.recv().await.unwrap();
        }
        for (item, field) in [
            ("missing", TextField::AssistantText),
            ("item", TextField::CommandOutput),
        ] {
            router.session_change(
                &target,
                SessionChange::Text {
                    turn_id: "run".into(),
                    item_id: item.into(),
                    field,
                    delta: "bad".into(),
                },
            );
        }
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: "item".into(),
                field: TextField::AssistantText,
                delta: " good".into(),
            },
        );
        for (connection, stream) in connections.iter().zip(&mut streams) {
            router.ensure_session(connection.id()).unwrap();
            stream.recv().await.unwrap();
        }
        let turn = router.current_turn(&target, "run").unwrap();
        assert!(
            matches!(turn.items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::AssistantText {text,..} if text == "kept good")
        );
    }

    #[test]
    fn resolving_unsubscribed_elicitation_releases_its_execution_immediately() {
        use super::super::requests::{RequestDestination, RequestOrigin};
        let router = SessionRouter::new();
        let target = SessionRef::new(ProviderKind::Claude, "native".into()).unwrap();
        let (input, _receiver) = tokio::sync::mpsc::channel(1);
        let instance = uuid::Uuid::new_v4();
        let adapted = super::super::requests::claude(
            "request".into(),
            &"turn".into(),
            &json!({"subtype":"elicitation","requested_schema":{"type":"object","properties":{}}}),
        )
        .unwrap();
        router
            .request(
                target.clone(),
                RequestOrigin {
                    instance,
                    native_id: json!("native-request"),
                    destination: RequestDestination::Claude { input },
                },
                adapted,
            )
            .unwrap();
        assert!(lock_state(&router.state).executions.contains_key(&target));
        router.resolve_native_request(instance, &json!("native-request"));
        assert!(!lock_state(&router.state).executions.contains_key(&target));
    }
    #[tokio::test]
    async fn history_is_not_retained_or_trimmed_and_subscriptions_do_not_pin_execution() {
        let router = SessionRouter::new();
        let connection = router.open_session();
        let mut turns: Vec<_> = (0..1001)
            .map(|id| {
                Arc::new(Turn {
                    id: id.to_string().into(),
                    ..Default::default()
                })
            })
            .collect();
        turns[0] = Arc::new(
            serde_json::from_value(json!({"id":"first","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(5 * 1024 * 1024),"phase":"unknown"}}}}}],"status":"unknown"}))
            .unwrap(),
        );
        let response = ThreadResponse {
            thread: Thread {
                id: Some(SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "native".into(),
                }),
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
            reply["result"]["response"]["thread"]["turns"][0]["items"][0]["body"]["deferred"]["summary"]["assistantText"]["text"]
                .as_str()
                .unwrap()
                .len(),
            256
        );
        assert!(
            reply["result"]["response"]["thread"]["turns"][0]["items"][0]["body"]["deferred"]
                .is_object()
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
        let connection = router.open_session();
        let mut items: Vec<_> = (0..40).map(|id| json!({"id":id.to_string(),"status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(512 * 1024),"phase":"unknown"}}}}})).collect();
        items.push(
            json!({"id":"tool","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"z".repeat(8192),"exitCode":null,"durationMs":null}}}}}),
        );
        let response = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[{"id":"turn","items":items,"status":"unknown"}]}}),
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
        assert_eq!(
            turn["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["body"]["deferred"].is_object())
                .count(),
            41
        );
        router.ensure_session(connection.id()).unwrap();
        assert!(lock_state(&router.state).executions.is_empty());
    }

    #[tokio::test]
    async fn oversized_rpc_returns_an_error_without_dropping_the_connection_or_subscribing() {
        let router = SessionRouter::new();
        let connection = router.open_session();
        for _ in 0..2 {
            let response = serde_json::from_value(
                json!({"thread":{"id":{"provider":"codex","id":"native"},"name":"x".repeat(MAX_QUEUED_BYTES + 1)}}),
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
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let connection = router.open_session();
        let read = open(&router, "native");
        turn(&router, false);
        router.session_change(
            &target,
            SessionChange::Item {
                turn_id: "run".into(),
                item: Item::new(
                    "answer".into(),
                    agent_protocol::execution::ItemStatus::Running,
                    agent_protocol::items::ItemBody::AssistantText {
                        citation: None,
                        text: "start".into(),
                        phase: agent_protocol::items::AssistantPhase::Unknown,
                    },
                )
                .into(),
            },
        );
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::AssistantText,
                delta: " final".into(),
            },
        );
        router.session_change(&target, SessionChange::Turn {
            turn: serde_json::from_value(json!({"id":"run","status":"completed","items":[{"id":"other","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"done","phase":"final"}}}}}]})).unwrap(),
            completed: true,
        });
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::AssistantText,
                delta: " suffix".into(),
            },
        );
        let response = router
            .finish_session_read(
                read,
                connection.id(),
                serde_json::from_value(json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[{"id":"run","status":"running","items":[]}]}}))
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
        assert_eq!(
            current["items"][0]["body"]["inline"]["body"]["assistantText"]["text"],
            "start final suffix"
        );
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
        let mut response: ThreadResponse = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
        )
        .unwrap();
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
        use agent_protocol::{models::SessionStatus, session::SubmissionDelivery};
        let router = SessionRouter::new();
        let target = SessionRef::new(
            agent_protocol::session::ProviderKind::Codex,
            "native".to_string(),
        )
        .unwrap();
        router.begin_submission(&target, "input").unwrap();
        router.session_change(
            &target,
            SessionChange::Status {
                status: SessionStatus::Running,
            },
        );
        assert_eq!(
            lock_state(&router.state).executions[&target].live.status,
            SessionStatus::Running
        );
        router.fail_provider(ProviderKind::Codex, "provider stopped");
        let state = lock_state(&router.state);
        let live = &state.executions[&target].live;
        assert_eq!(live.status, SessionStatus::Unknown);
        assert_eq!(live.submissions["input"], SubmissionDelivery::Unknown);
    }

    #[tokio::test]
    async fn byte_budget_disconnects_only_the_slow_connection() {
        let router = SessionRouter::new();
        let slow = router.open_session();
        let mut healthy = router.open_session();
        router
            .send_frame(slow.id(), vec![0; MAX_QUEUED_BYTES - frame_cost(0)])
            .unwrap();
        assert!(router.send_frame(slow.id(), b"overflow".to_vec()).is_err());
        router
            .send_frame(healthy.id(), b"current".to_vec())
            .unwrap();
        assert_eq!(healthy.recv().await.as_deref(), Some(b"current".as_slice()));
    }

    #[tokio::test]
    async fn burst_of_small_updates_keeps_the_connection_and_delivers_every_update() {
        let router = SessionRouter::new();
        let mut connection = router.open_session();
        let response: ThreadResponse = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
        )
        .unwrap();
        let mut subscription = router
            .finish_session_read(open(&router, "native"), connection.id(), response)
            .unwrap()
            .updates
            .unwrap();
        // A provider can deliver a buffered burst before the network writer is scheduled.
        // This is less than 100 KiB, well within the connection's memory budget.
        for index in 0..512 {
            router.session_change(
                &SessionRef::new(
                    agent_protocol::session::ProviderKind::Codex,
                    "native".to_string(),
                )
                .unwrap(),
                SessionChange::Status {
                    status: if index % 2 == 0 {
                        agent_protocol::models::SessionStatus::Running
                    } else {
                        agent_protocol::models::SessionStatus::Idle
                    },
                },
            );
        }
        router
            .ensure_session(connection.id())
            .expect("a small burst must not disconnect the client");
        for index in 0..512 {
            let change =
                protocol::decode::<SessionChange>(&subscription.recv().await.unwrap()).unwrap();
            let SessionChange::Status { status } = change else {
                panic!("expected status")
            };
            assert_eq!(
                status == agent_protocol::models::SessionStatus::Running,
                index % 2 == 0
            );
            let activity =
                protocol::decode::<Notification>(&connection.recv().await.unwrap()).unwrap();
            let Notification::Activity { active, .. } = activity else {
                panic!("expected activity")
            };
            assert_eq!(active, index % 2 == 0);
        }
        assert_eq!(
            connection
                .queued_bytes
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn slow_session_subscribers_do_not_block_other_devices_or_destroy_current_state() {
        let router = SessionRouter::new();
        let slow = router.open_session();
        let mut healthy = router.open_session();
        let target = SessionRef::new(
            agent_protocol::session::ProviderKind::Codex,
            "native".to_string(),
        )
        .unwrap();
        let _execution = open(&router, "native");
        let response: ThreadResponse = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
        )
        .unwrap();
        let mut streams = Vec::new();
        for connection in [slow.id(), healthy.id()] {
            streams.push(
                router
                    .finish_session_read(open(&router, "native"), connection, response.clone())
                    .unwrap()
                    .updates
                    .unwrap(),
            );
        }
        let change = SessionChange::Turn {
            completed: false,
            turn: serde_json::from_value(json!({"id":"turn","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(1024 * 1024),"phase":"unknown"}}}}}],"status":"unknown"}))
            .unwrap(),
        };
        for _ in 0..20 {
            router.session_change(&target, change.clone());
            assert!(streams[1].recv().await.is_some());
            assert!(healthy.recv().await.is_some());
        }
        assert!(router.ensure_session(slow.id()).is_err());
        router.ensure_session(healthy.id()).unwrap();
        while streams[0].recv().await.is_some() {}
        assert!(router.current_turn(&target, "turn").is_some());
    }

    #[test]
    fn subscriptions_share_the_connection_budget_and_drop_releases_unread_bytes() {
        let router = SessionRouter::new();
        let connection = router.open_session();
        let response: ThreadResponse = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
        )
        .unwrap();
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
        a.try_send(connection.id(), Vec::with_capacity(MAX_QUEUED_BYTES / 2))
            .unwrap();
        assert!(
            b.try_send(connection.id(), vec![0; MAX_QUEUED_BYTES / 2 + 1])
                .is_err()
        );
        drop(first);
        assert!(matches!(
            a.try_send(connection.id(), vec![]),
            Err(mpsc::error::TrySendError::Closed(_))
        ));
        assert_eq!(budget.load(std::sync::atomic::Ordering::Relaxed), 0);
        b.try_send(connection.id(), vec![0; MAX_QUEUED_BYTES / 2 + 1])
            .unwrap();
        drop(second);
        assert_eq!(budget.load(std::sync::atomic::Ordering::Relaxed), 0);
        router.ensure_session(connection.id()).unwrap();
    }

    #[test]
    fn cancellation_and_disconnect_release_an_unfinished_open() {
        let router = SessionRouter::new();
        let connection = router.open_session();
        let read = router
            .retain_execution(
                SessionRef::new(
                    agent_protocol::session::ProviderKind::Codex,
                    "native".to_string(),
                )
                .unwrap(),
            )
            .unwrap();
        drop(connection);
        drop(read);
        // Fill and evict idle entries: leaked leases would eventually prevent admission.
        for id in 0..200 {
            let read = router
                .retain_execution(
                    SessionRef::new(agent_protocol::session::ProviderKind::Codex, id.to_string())
                        .unwrap(),
                )
                .unwrap();
            drop(read);
        }
    }
}
#[test]
fn identical_native_request_ids_keep_their_source_instance() {
    use super::requests::{RequestDestination, RequestOrigin};
    use agent_protocol::requests::Answer;
    let router = SessionRouter::new();
    let connection = router.open_session();
    let native = serde_json::json!("claude-permission:shared-native-id");
    let (input, _receiver) = tokio::sync::mpsc::channel(1);
    let mut requests = Vec::new();
    for provider in [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::Codex,
    ] {
        let instance = uuid::Uuid::new_v4();
        let target = SessionRef::new(provider, "shared-native-session".into()).unwrap();
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
        let adapted = if provider == ProviderKind::Codex {
            super::requests::codex(
                uuid::Uuid::new_v4().to_string().into(),
                "item/commandExecution/requestApproval",
                &serde_json::json!({"turnId":"turn","availableDecisions":["accept","decline"]}),
            )
            .unwrap()
        } else {
            super::requests::claude(uuid::Uuid::new_v4().to_string().into(), &"turn".into(), &serde_json::json!({"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool","input":{"command":"pwd"}})).unwrap()
        };
        let request_id = adapted.request.id.clone();
        let choice_id = adapted.request.body.choices()[0].id.clone();
        let destination = if provider == ProviderKind::Codex {
            RequestDestination::Codex {
                stopped: Default::default(),
            }
        } else {
            RequestDestination::Claude {
                input: input.clone(),
            }
        };
        router
            .request(
                target.clone(),
                RequestOrigin {
                    instance,
                    native_id: native.clone(),
                    destination,
                },
                adapted,
            )
            .unwrap();
        let (origin, _) = router
            .claim_response(
                connection.id(),
                &request_id,
                &Answer::Approval { choice_id },
            )
            .unwrap();
        assert_eq!(origin.instance, instance);
        assert_eq!(origin.native_id, native);
        requests.push((instance, target, request_id));
    }
    let malformed =
        serde_json::json!({"id":1,"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    assert!(
        super::codex::event_change(requests[0].0, &RpcMessage::parse(&malformed).unwrap()).is_err()
    );
    let resolved =
        serde_json::json!({"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    super::codex::event_change(requests[0].0, &RpcMessage::parse(&resolved).unwrap())
        .unwrap()
        .unwrap()
        .apply(&router)
        .unwrap();
    assert!(router.request_session(&requests[0].2).is_none());
    for (_, target, id) in &requests[1..] {
        assert_eq!(router.request_session(id).as_ref(), Some(target));
    }
}
