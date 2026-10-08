use super::{requests::RequestOrigin, session_actor::SessionActor};
use agent_protocol::{
    models::{Thread, ThreadResponse},
    protocol::{self, Notification},
    session::{OpenedSession, ProviderKind, SessionChange, SessionRef},
};
#[cfg(test)]
use agent_transport::peer::RpcMessage;
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    },
};
use tokio::sync::mpsc;

pub type SessionId = u64;
const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
fn frame_cost(capacity: usize) -> usize {
    capacity + std::mem::size_of::<Vec<u8>>()
}

pub(super) struct SessionLease {
    router: SessionRouter,
    target: SessionRef,
    actor: Arc<Mutex<SessionActor>>,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut actor = lock_state(&self.actor);
        actor.leases -= 1;
        drop(actor);
        self.router.prune(&self.target, &self.actor);
    }
}

pub struct HostSession {
    id: SessionId,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued_bytes: Arc<AtomicUsize>,
    state: Weak<Mutex<State>>,
}
impl HostSession {
    pub fn id(&self) -> SessionId {
        self.id
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let frame = self.receiver.recv().await?;
        self.queued_bytes
            .fetch_sub(frame_cost(frame.capacity()), Relaxed);
        Some(frame)
    }
}
impl fmt::Debug for HostSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Drop for HostSession {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            SessionRouter { state }.close_session(self.id);
        }
    }
}

/// The stream owns its subscription. Conversation content is never owned by a connection.
pub struct HostSubscription {
    id: uuid::Uuid,
    target: SessionRef,
    actor: Weak<Mutex<SessionActor>>,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued_bytes: Arc<AtomicUsize>,
    state: Weak<Mutex<State>>,
}
impl HostSubscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        let frame = self.receiver.recv().await?;
        self.queued_bytes
            .fetch_sub(frame_cost(frame.capacity()), Relaxed);
        Some(frame)
    }
}
impl Drop for HostSubscription {
    fn drop(&mut self) {
        if let Some(actor) = self.actor.upgrade() {
            lock_state(&actor).subscriptions.remove(&self.id);
            if let Some(state) = self.state.upgrade() {
                lock_state(&state).subscriptions.remove(&self.id);
                SessionRouter { state }.prune(&self.target, &actor);
            }
        }
        while let Ok(frame) = self.receiver.try_recv() {
            self.queued_bytes
                .fetch_sub(frame_cost(frame.capacity()), Relaxed);
        }
    }
}
pub struct HostReply {
    pub initial: Vec<u8>,
    pub updates: Option<HostSubscription>,
}
impl From<protocol::Response> for HostReply {
    fn from(response: protocol::Response) -> Self {
        Self {
            initial: protocol::response_frame(response).expect("response encodes"),
            updates: None,
        }
    }
}

#[derive(Clone)]
pub(super) struct Outbound {
    principal: String,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    bytes: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
}
impl Outbound {
    fn try_send(
        &self,
        session: SessionId,
        frame: Vec<u8>,
    ) -> Result<(), mpsc::error::TrySendError<Vec<u8>>> {
        if !self.alive.load(Relaxed) {
            return Err(mpsc::error::TrySendError::Closed(frame));
        }
        let cost = frame_cost(frame.capacity());
        if self
            .bytes
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=byte_limit queued_bytes={} frame_bytes={} frame_cost={cost} limit_bytes={MAX_QUEUED_BYTES}", self.bytes.load(Relaxed), frame.len()));
            return Err(mpsc::error::TrySendError::Full(frame));
        }
        self.sender.send(frame).map_err(|error| {
            self.bytes.fetch_sub(cost, Relaxed);
            tracing::warn!(target: "bex", operation = "host.session.queue_failed", message = %format_args!("session={session} reason=receiver_closed frame_bytes={}", error.0.len()));
            mpsc::error::TrySendError::Closed(error.0)
        })
    }
}

/// Only the connection and identity registries share this lock. History transforms,
/// encoders, arbitration and queue delivery run under their conversation's lock.
#[derive(Default)]
struct State {
    apns: Option<Arc<crate::apns::Apns>>,
    next_session_id: SessionId,
    sessions: HashMap<SessionId, Outbound>,
    executions: HashMap<SessionRef, Arc<Mutex<SessionActor>>>,
    subscriptions: HashMap<uuid::Uuid, (SessionRef, SessionId)>,
    requests: HashMap<agent_protocol::ids::RequestId, SessionRef>,
    native_requests: HashMap<(uuid::Uuid, String), agent_protocol::ids::RequestId>,
}
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
}
fn native_key(instance: uuid::Uuid, id: &Value) -> (uuid::Uuid, String) {
    (
        instance,
        serde_json::to_string(id).expect("native ID serializes"),
    )
}
fn request_target_is_live(
    target: &agent_protocol::requests::RequestTarget,
    turns: Option<&[Arc<agent_protocol::models::Turn>]>,
) -> bool {
    match target {
        agent_protocol::requests::RequestTarget::Session => true,
        agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => {
            turns.into_iter().flatten().any(|turn| {
                &turn.id == turn_id && turn.status == agent_protocol::execution::TurnStatus::Running
            })
        }
    }
}
fn lock_state<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl SessionRouter {
    pub(super) fn set_apns(&self, apns: Option<Arc<crate::apns::Apns>>) {
        lock_state(&self.state).apns = apns;
    }
    pub(super) fn apns(&self) -> Option<Arc<crate::apns::Apns>> {
        lock_state(&self.state).apns.clone()
    }
    pub(super) fn revoke_device(&self, principal: &str) {
        let state = lock_state(&self.state);
        for output in state
            .sessions
            .values()
            .filter(|output| output.principal == principal)
        {
            output.alive.store(false, Relaxed);
        }
        if let Some(apns) = &state.apns {
            apns.revoke(principal);
        }
    }
    pub(super) fn register_live_activity(
        &self,
        connection: SessionId,
        params: &agent_protocol::live_activity::RegisterLiveActivity,
        native: Thread,
    ) -> Result<(), String> {
        if native.id.as_ref() != Some(&params.session) {
            return Err("native session ID does not match".into());
        }
        let actor = self.actor(&params.session);
        let owned = lock_state(&actor);
        let thread = owned.overlay(native);
        let state = lock_state(&self.state);
        let principal = state
            .sessions
            .get(&connection)
            .filter(|output| output.alive.load(Relaxed))
            .ok_or("connection is closed")?;
        let apns = state.apns.as_ref().ok_or("APNs is unavailable")?;
        let phase = agent_protocol::live_activity::task_phase(
            !matches!(
                thread.status,
                agent_protocol::models::SessionStatus::Unknown
                    | agent_protocol::models::SessionStatus::Unavailable
            ),
            thread.status == agent_protocol::models::SessionStatus::Running,
            thread.requests.values().any(|request| {
                request.delivery == agent_protocol::session::RequestDelivery::Awaiting
            }),
            thread
                .turns
                .as_ref()
                .and_then(|turns| turns.last())
                .map(|turn| turn.status),
        );
        apns.register(
            &principal.principal,
            params,
            agent_protocol::models::task_title(thread.name.as_deref(), thread.preview.as_deref()),
            phase,
        )
        .map_err(str::to_owned)
    }
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                next_session_id: 1,
                ..Default::default()
            })),
        }
    }
    fn actor(&self, target: &SessionRef) -> Arc<Mutex<SessionActor>> {
        lock_state(&self.state)
            .executions
            .entry(target.clone())
            .or_default()
            .clone()
    }
    fn existing(&self, target: &SessionRef) -> Option<Arc<Mutex<SessionActor>>> {
        lock_state(&self.state).executions.get(target).cloned()
    }
    fn prune(&self, target: &SessionRef, actor: &Arc<Mutex<SessionActor>>) {
        let mut owned = lock_state(actor);
        if owned.release() || !owned.subscriptions.is_empty() {
            return;
        }
        let mut state = lock_state(&self.state);
        if Arc::strong_count(actor) == 2
            && state
                .executions
                .get(target)
                .is_some_and(|current| Arc::ptr_eq(current, actor))
        {
            state.executions.remove(target);
        }
    }
    pub(crate) fn open_session(&self) -> HostSession {
        self.open_authenticated_session(None)
    }
    pub(crate) fn open_authenticated_session(&self, principal: Option<String>) -> HostSession {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = lock_state(&self.state);
        let id = loop {
            let id = state.next_session_id;
            state.next_session_id = state.next_session_id.checked_add(1).unwrap_or(1);
            if !state.sessions.contains_key(&id) {
                break id;
            }
        };
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        state.sessions.insert(
            id,
            Outbound {
                principal: principal.unwrap_or_else(|| format!("session:{id}")),
                sender,
                bytes: queued_bytes.clone(),
                alive: Arc::new(AtomicBool::new(true)),
            },
        );
        HostSession {
            id,
            receiver,
            queued_bytes,
            state: Arc::downgrade(&self.state),
        }
    }
    pub(crate) fn principal(&self, session: SessionId) -> Result<String, String> {
        self.connection(session)
            .map(|output| output.principal)
            .ok_or_else(|| "connection is closed".into())
    }
    fn connection(&self, session: SessionId) -> Option<Outbound> {
        lock_state(&self.state)
            .sessions
            .get(&session)
            .filter(|output| output.alive.load(Relaxed))
            .cloned()
    }
    pub(crate) fn ensure_session(&self, session: SessionId) -> Result<(), String> {
        self.connection(session)
            .map(|_| ())
            .ok_or_else(|| "connection is closed".into())
    }
    pub(super) fn submission_lock(&self, target: &SessionRef) -> Arc<tokio::sync::Mutex<()>> {
        lock_state(&self.actor(target)).submission_lock.clone()
    }
    pub(crate) fn overlay_execution(&self, target: &SessionRef, mut thread: Thread) -> Thread {
        if let Some(actor) = self.existing(target) {
            thread = lock_state(&actor).overlay(thread);
            self.prune(target, &actor);
        }
        thread
    }
    pub(super) fn execution_targets(&self) -> Vec<SessionRef> {
        lock_state(&self.state).executions.keys().cloned().collect()
    }
    pub(super) fn submission_receipt(
        &self,
        target: &SessionRef,
        id: &str,
    ) -> Option<agent_protocol::operations::SubmissionReceipt> {
        let actor = self.existing(target)?;
        let receipt = match lock_state(&actor).timeline.submissions.get(id) {
            Some(agent_protocol::session::SubmissionDelivery::Accepted { turn_id }) => {
                Some(agent_protocol::operations::SubmissionReceipt {
                    turn_id: turn_id.clone(),
                })
            }
            _ => None,
        };
        self.prune(target, &actor);
        receipt
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
        let actor = self.actor(target);
        let mut owned = lock_state(&actor);
        if owned.timeline.submissions.get(id).is_some_and(|delivery| {
            *delivery != agent_protocol::session::SubmissionDelivery::Rejected
        }) {
            return Err(Failure::unknown(
                "submission_outcome_unknown",
                "submission is already in flight; read the session before sending again",
            ));
        }
        if owned.timeline.submissions.len() >= 128 {
            owned.timeline.submissions.retain(|_, delivery| {
                *delivery != agent_protocol::session::SubmissionDelivery::Rejected
            });
        }
        if owned.timeline.submissions.len() >= 128 {
            return Err(Failure::new(
                "input_capacity_reached",
                "active input capacity reached",
            ));
        }
        let mut failed = Vec::new();
        self.change_locked(
            &mut owned,
            target,
            &SessionChange::Submission {
                id: id.into(),
                delivery: agent_protocol::session::SubmissionDelivery::Sending,
            },
            &mut failed,
        );
        drop(owned);
        self.close_failed(failed);
        Ok(())
    }
    pub(super) fn finish_submission(
        &self,
        target: &SessionRef,
        id: &str,
        delivery: agent_protocol::session::SubmissionDelivery,
    ) {
        let Some(actor) = self.existing(target) else {
            return;
        };
        let mut owned = lock_state(&actor);
        let mut failed = Vec::new();
        if owned.timeline.submissions.contains_key(id) {
            self.change_locked(
                &mut owned,
                target,
                &SessionChange::Submission {
                    id: id.into(),
                    delivery,
                },
                &mut failed,
            );
        }
        drop(owned);
        self.close_failed(failed);
        self.prune(target, &actor);
    }
    pub(crate) fn current_turn(
        &self,
        target: &SessionRef,
        turn: &str,
    ) -> Option<Arc<agent_protocol::models::Turn>> {
        let actor = self.existing(target)?;
        let result = lock_state(&actor)
            .timeline
            .turns
            .as_ref()
            .and_then(|turns| {
                turns
                    .iter()
                    .rfind(|current| current.id.as_str() == turn)
                    .cloned()
            });
        self.prune(target, &actor);
        result
    }

    pub(super) fn retain_execution(&self, target: SessionRef) -> Result<SessionLease, String> {
        target.validate()?;
        let actor = self.actor(&target);
        lock_state(&actor).leases += 1;
        Ok(SessionLease {
            router: self.clone(),
            target,
            actor,
        })
    }
    /// Publish hydration on the existing ordered stream, between live changes.
    /// Native history stays in the requesting client's cache.
    pub(super) fn finish_turn_read(
        &self,
        read: SessionLease,
        session: SessionId,
        turn_id: agent_protocol::ids::TurnId,
        native_items: Vec<Arc<agent_protocol::models::Item>>,
    ) -> Result<(), String> {
        let actor = lock_state(&read.actor);
        if !actor
            .subscriptions
            .values()
            .any(|(owner, _)| *owner == session)
        {
            return Err("conversation subscription changed during detail read".into());
        }
        let live = actor
            .timeline
            .turns
            .iter()
            .flatten()
            .rfind(|current| current.id == turn_id);
        let mut items = agent_protocol::session::append_items(
            native_items,
            live.and_then(|turn| turn.items.as_deref())
                .unwrap_or_default(),
        );
        agent_protocol::models::defer_items(
            &mut items,
            agent_protocol::models::MAX_INLINE_ITEM_BYTES,
        );
        let mut change = SessionChange::TurnItems { turn_id, items };
        let mut frame = protocol::encode(&change).map_err(|error| error.to_string())?;
        if frame.len() > MAX_QUEUED_BYTES {
            if let SessionChange::TurnItems { items, .. } = &mut change {
                agent_protocol::models::defer_items(items, 0);
            }
            frame = protocol::encode(&change).map_err(|error| error.to_string())?;
        }
        let failed = actor
            .subscriptions
            .values()
            .filter(|(owner, _)| *owner == session)
            .any(|(owner, output)| output.try_send(*owner, frame.clone()).is_err());
        drop(actor);
        if failed {
            self.close_failed(vec![session]);
            return Err("conversation detail stream is unavailable".into());
        }
        Ok(())
    }
    pub(super) fn finish_session_read(
        &self,
        read: SessionLease,
        session: SessionId,
        mut response: ThreadResponse,
    ) -> Result<HostReply, String> {
        let connection = self
            .connection(session)
            .ok_or("connection closed during session open")?;
        let mut actor = lock_state(&read.actor);
        if response.thread.id.as_ref() != Some(&read.target) {
            return Err("native session ID does not match".into());
        }
        response.thread = actor.overlay(response.thread);
        response
            .thread
            .defer_item_details(agent_protocol::models::MAX_INLINE_ITEM_BYTES);
        let mut opened = OpenedSession {
            session: read.target.clone(),
            subscription_id: uuid::Uuid::nil(),
            response,
        };
        let mut initial = protocol::encode(protocol::Response::Success { result: &opened })
            .map_err(|e| e.to_string())?;
        if initial.len() > MAX_QUEUED_BYTES {
            opened.response.thread.defer_item_details(0);
            initial = protocol::encode(protocol::Response::Success { result: &opened })
                .map_err(|e| e.to_string())?;
        }
        if initial.len() > MAX_QUEUED_BYTES {
            return Ok(protocol::Response::error(
                "response_too_large",
                &"Session metadata exceeds the RPC limit; request fewer turns",
            )
            .into());
        }
        let id = uuid::Uuid::new_v4();
        let (sender, receiver) = mpsc::unbounded_channel();
        // Conversation -> registry is the only nested lock order. Registration,
        // removal and claims all verify the same live connection here.
        let mut state = lock_state(&self.state);
        if !state.sessions.contains_key(&session) || !connection.alive.load(Relaxed) {
            return Err("connection closed during session open".into());
        }
        state
            .subscriptions
            .insert(id, (read.target.clone(), session));
        actor.subscriptions.insert(
            id,
            (
                session,
                Outbound {
                    sender,
                    ..connection.clone()
                },
            ),
        );
        drop(state);
        drop(actor);
        Ok(HostReply {
            initial,
            updates: Some(HostSubscription {
                id,
                target: read.target.clone(),
                actor: Arc::downgrade(&read.actor),
                receiver,
                queued_bytes: connection.bytes,
                state: Arc::downgrade(&self.state),
            }),
        })
    }
    pub(crate) fn close_session(&self, session: SessionId) {
        let actors: HashMap<_, _> = {
            let mut state = lock_state(&self.state);
            if let Some(connection) = state.sessions.remove(&session) {
                connection.alive.store(false, Relaxed);
            }
            let targets: Vec<_> = state
                .subscriptions
                .values()
                .filter(|(_, owner)| *owner == session)
                .map(|(target, _)| target.clone())
                .collect();
            state
                .subscriptions
                .retain(|_, (_, owner)| *owner != session);
            targets
                .into_iter()
                .filter_map(|target| {
                    state
                        .executions
                        .get(&target)
                        .cloned()
                        .map(|actor| (target, actor))
                })
                .collect()
        };
        for (target, actor) in actors {
            lock_state(&actor)
                .subscriptions
                .retain(|_, (owner, _)| *owner != session);
            self.prune(&target, &actor);
        }
    }
    fn close_failed(&self, failed: Vec<SessionId>) {
        for session in failed {
            self.close_session(session);
        }
    }
    pub(crate) fn send(
        &self,
        session: SessionId,
        notification: Notification,
    ) -> Result<(), String> {
        self.send_frame(
            session,
            protocol::encode(notification).map_err(|e| e.to_string())?,
        )
    }
    fn send_frame(&self, session: SessionId, frame: Vec<u8>) -> Result<(), String> {
        let output = self
            .connection(session)
            .ok_or_else(|| format!("RPC session {session} is not open"))?;
        output.try_send(session, frame).map_err(|_| {
            self.close_session(session);
            format!("RPC session {session} outbound queue failed")
        })
    }
    fn broadcast_frames(&self, frame: Vec<u8>) -> Vec<SessionId> {
        let outputs: Vec<_> = lock_state(&self.state)
            .sessions
            .iter()
            .map(|(id, output)| (*id, output.clone()))
            .collect();
        outputs
            .into_iter()
            .filter_map(|(id, output)| output.try_send(id, frame.clone()).is_err().then_some(id))
            .collect()
    }
    pub(crate) fn broadcast(&self, notification: Notification) {
        let failed =
            self.broadcast_frames(protocol::encode(notification).expect("notification encodes"));
        self.close_failed(failed);
    }
    pub(crate) fn request(
        &self,
        target: SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
    ) -> Result<(), String> {
        if target.provider != origin.provider {
            return Err("request provider does not match session".into());
        }
        if !origin.source.is_alive() {
            return Err("request source is closed".into());
        }
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            > 64 * 1024
        {
            return Err("pending request capacity reached".into());
        }
        let native = native_key(origin.instance, &origin.native_id);
        let lease = self.retain_execution(target.clone())?;
        let actor = &lease.actor;
        let mut owned = lock_state(actor);
        let mut state = lock_state(&self.state);
        if state.native_requests.contains_key(&native) {
            return Ok(());
        }
        if state.requests.len() >= 256 || owned.timeline.requests.len() >= 32 {
            return Err("pending request capacity reached".into());
        }
        if !request_target_is_live(&request.target, owned.timeline.turns.as_deref()) {
            return Err("request has no owned live turn".into());
        }
        state.requests.insert(request.id.clone(), target.clone());
        state.native_requests.insert(native, request.id.clone());
        drop(state);
        owned.request_origins.insert(request.id.clone(), origin);
        let mut failed = Vec::new();
        self.change_locked(
            &mut owned,
            &target,
            &SessionChange::Request { request },
            &mut failed,
        );
        drop(owned);
        self.close_failed(failed);
        Ok(())
    }
    pub(crate) fn session_change(&self, target: &SessionRef, change: SessionChange) {
        let actor = self.actor(target);
        let mut failed = Vec::new();
        self.change_locked(&mut lock_state(&actor), target, &change, &mut failed);
        self.close_failed(failed);
        self.prune(target, &actor);
    }
    pub(crate) fn resolve_native_request(&self, instance: uuid::Uuid, native: &Value) {
        let resolved = {
            let state = lock_state(&self.state);
            state
                .native_requests
                .get(&native_key(instance, native))
                .and_then(|id| {
                    state
                        .requests
                        .get(id)
                        .map(|target| (target.clone(), id.clone()))
                })
        };
        if let Some((target, request_id)) = resolved {
            self.session_change(&target, SessionChange::ResolveRequest { request_id });
        }
    }
    pub(crate) fn close_request_source(&self, instance: uuid::Uuid) {
        let requests: Vec<_> = {
            let state = lock_state(&self.state);
            state
                .native_requests
                .iter()
                .filter(|((source, _), _)| *source == instance)
                .filter_map(|(_, id)| {
                    state
                        .requests
                        .get(id)
                        .map(|target| (target.clone(), id.clone()))
                })
                .collect()
        };
        for (target, request_id) in requests {
            self.session_change(&target, SessionChange::ResolveRequest { request_id });
        }
    }
    pub(super) fn request_session(&self, id: &str) -> Option<SessionRef> {
        lock_state(&self.state).requests.get(id).cloned()
    }
    pub(crate) fn claim_response(
        &self,
        session: SessionId,
        id: &str,
        answer: &agent_protocol::requests::Answer,
    ) -> Result<(RequestOrigin, agent_protocol::requests::RequestBody), String> {
        let target = self
            .request_session(id)
            .ok_or("request was already answered or its execution has ended")?;
        let actor = self
            .existing(&target)
            .ok_or("request execution has ended")?;
        let mut owned = lock_state(&actor);
        let request = owned
            .timeline
            .requests
            .get(id)
            .ok_or("request was already answered or its execution has ended")?;
        let origin = owned
            .request_origins
            .get(id)
            .ok_or("request source has ended")?;
        if request.delivery != agent_protocol::session::RequestDelivery::Awaiting
            || !origin.source.is_alive()
        {
            return Err("request was already answered or its source has ended".into());
        }
        if !request_target_is_live(&request.target, owned.timeline.turns.as_deref()) {
            return Err("request execution has ended".into());
        }
        agent_protocol::requests::validate_answer(&request.body, answer)?;
        let body = request.body.clone();
        let origin = origin.clone();
        let state = lock_state(&self.state);
        if !state
            .sessions
            .get(&session)
            .is_some_and(|output| output.alive.load(Relaxed))
        {
            return Err("connection is closed".into());
        }
        drop(state);
        let mut failed = Vec::new();
        self.change_locked(
            &mut owned,
            &target,
            &SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_protocol::session::RequestDelivery::Sending,
            },
            &mut failed,
        );
        drop(owned);
        self.close_failed(failed);
        Ok((origin, body))
    }
    pub(crate) fn response_delivery(
        &self,
        id: &str,
        delivery: agent_protocol::session::RequestDelivery,
    ) {
        let Some(target) = self.request_session(id) else {
            return;
        };
        let Some(actor) = self.existing(&target) else {
            return;
        };
        let mut owned = lock_state(&actor);
        let mut failed = Vec::new();
        if owned.timeline.requests.get(id).is_some_and(|request| {
            request.delivery == agent_protocol::session::RequestDelivery::Sending
        }) {
            self.change_locked(
                &mut owned,
                &target,
                &SessionChange::RequestDelivery {
                    request_id: id.into(),
                    state: delivery,
                },
                &mut failed,
            );
        }
        drop(owned);
        self.close_failed(failed);
        self.prune(&target, &actor);
    }
    pub(crate) fn fail_provider(&self, provider: ProviderKind, message: &str) {
        let actors: Vec<_> = lock_state(&self.state)
            .executions
            .iter()
            .filter(|(target, _)| target.provider == provider)
            .map(|(target, actor)| (target.clone(), actor.clone()))
            .collect();
        for (target, actor) in actors {
            let mut owned = lock_state(&actor);
            let mut changes: Vec<_> = owned
                .timeline
                .requests
                .keys()
                .cloned()
                .map(|request_id| SessionChange::ResolveRequest { request_id })
                .collect();
            changes.extend(
                owned
                    .timeline
                    .turns
                    .iter()
                    .flatten()
                    .filter(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
                    .map(|turn| {
                        let mut turn = (**turn).clone();
                        turn.status = agent_protocol::execution::TurnStatus::Failed;
                        turn.error = Some(agent_protocol::execution::ExecutionError {
                            category: agent_protocol::execution::ErrorCategory::Network,
                            message: message.into(),
                            ..Default::default()
                        });
                        SessionChange::Turn {
                            turn,
                            completed: true,
                        }
                    }),
            );
            changes.extend(
                owned
                    .timeline
                    .submissions
                    .iter()
                    .filter(|(_, delivery)| {
                        matches!(
                            delivery,
                            agent_protocol::session::SubmissionDelivery::Sending
                        )
                    })
                    .map(|(id, _)| SessionChange::Submission {
                        id: id.clone(),
                        delivery: agent_protocol::session::SubmissionDelivery::Unknown,
                    }),
            );
            changes.push(SessionChange::Status {
                status: agent_protocol::models::SessionStatus::Unavailable,
            });
            let mut failed = Vec::new();
            for change in changes {
                self.change_locked(&mut owned, &target, &change, &mut failed);
            }
            drop(owned);
            self.close_failed(failed);
            self.prune(&target, &actor);
        }
    }
    fn change_locked(
        &self,
        actor: &mut SessionActor,
        target: &SessionRef,
        change: &SessionChange,
        failed: &mut Vec<SessionId>,
    ) {
        let expired: Vec<_> = actor.timeline.requests.values().filter(|request| match change {
            SessionChange::Turn { turn, completed: true } => matches!(&request.target, agent_protocol::requests::RequestTarget::Turn { turn_id, .. } if turn_id == &turn.id),
            SessionChange::Item { turn_id, item } => {
                let terminal = matches!(item.status, agent_protocol::execution::ItemStatus::Completed | agent_protocol::execution::ItemStatus::Failed | agent_protocol::execution::ItemStatus::Declined | agent_protocol::execution::ItemStatus::Interrupted);
                (terminal || request.delivery != agent_protocol::session::RequestDelivery::Awaiting) && matches!(&request.target, agent_protocol::requests::RequestTarget::Turn { turn_id: request_turn, item_id } if request_turn == turn_id && item_id.as_ref() == Some(&item.id))
            }
            _ => false,
        }).map(|request| request.id.clone()).collect();
        for request_id in expired {
            self.change_locked(
                actor,
                target,
                &SessionChange::ResolveRequest { request_id },
                failed,
            );
        }
        if let SessionChange::ResolveRequest { request_id } = change
            && let Some(origin) = actor.request_origins.remove(request_id)
        {
            let mut state = lock_state(&self.state);
            state.requests.remove(request_id);
            state
                .native_requests
                .remove(&native_key(origin.instance, &origin.native_id));
        }
        let (next, result) = change.apply_timeline(std::mem::take(&mut actor.timeline));
        actor.timeline = next;
        // An update may target native history that the Host deliberately does
        // not retain. Subscribers still apply it to their own history window.
        if let Err(reason) = result
            && reason != agent_protocol::session::UpdateError::MissingTurn
        {
            tracing::warn!(target: "bex", operation = "host.session.invalid_update", message = %reason);
            actor.release();
            return;
        }
        let frame = protocol::encode(change).expect("change encodes");
        for (connection, output) in actor.subscriptions.values() {
            if output.try_send(*connection, frame.clone()).is_err() {
                failed.push(*connection);
            }
        }
        if matches!(
            change,
            SessionChange::Status { .. } | SessionChange::Turn { .. }
        ) {
            let active = actor.timeline.status == agent_protocol::models::SessionStatus::Running;
            let frame = protocol::encode(Notification::Activity { session: target.clone(), active, finished: !active && matches!(change, SessionChange::Turn { completed: true, turn } if turn.status == agent_protocol::execution::TurnStatus::Completed) }).expect("activity encodes");
            failed.extend(self.broadcast_frames(frame));
        }
        if let Some(apns) = self.apns() {
            let timeline = &actor.timeline;
            apns.update(
                target,
                agent_protocol::live_activity::task_phase(
                    !matches!(
                        timeline.status,
                        agent_protocol::models::SessionStatus::Unknown
                            | agent_protocol::models::SessionStatus::Unavailable
                    ),
                    timeline.status == agent_protocol::models::SessionStatus::Running,
                    timeline.requests.values().any(|request| {
                        request.delivery == agent_protocol::session::RequestDelivery::Awaiting
                    }),
                    timeline
                        .turns
                        .as_ref()
                        .and_then(|turns| turns.last())
                        .map(|turn| turn.status),
                ),
            );
        }
        actor.release();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::models::{Item, Thread, Turn};

    #[test]
    fn live_activity_survives_disconnect_and_gets_terminal_result_before_execution_is_pruned() {
        use agent_protocol::{
            execution::TurnStatus,
            live_activity::{PushEnvironment, RegisterLiveActivity},
            models::SessionStatus,
        };
        let router = SessionRouter::new();
        let apns = crate::apns::Apns::testing();
        router.set_apns(Some(apns.clone()));
        let phone = router.open_authenticated_session(Some("phone".into()));
        let target = SessionRef {
            provider: ProviderKind::Codex,
            id: "task".into(),
        };
        let params = RegisterLiveActivity {
            session: target.clone(),
            activity_id: "activity".into(),
            token: vec![1; 32],
            environment: PushEnvironment::Sandbox,
        };
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: Turn {
                    id: "turn".into(),
                    status: TurnStatus::Running,
                    ..Default::default()
                },
                completed: false,
            },
        );
        router
            .register_live_activity(
                phone.id(),
                &params,
                Thread {
                    id: Some(target.clone()),
                    name: Some("タスク".into()),
                    status: SessionStatus::Idle,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            apns.test_payloads(crate::apns::now())[0]["aps"]["content-state"]["status"],
            "running"
        );
        drop(phone);
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: Turn {
                    id: "turn".into(),
                    status: TurnStatus::Failed,
                    ..Default::default()
                },
                completed: true,
            },
        );
        assert!(router.execution_targets().is_empty());
        let final_payload = &apns.test_payloads(crate::apns::now() + 2)[0]["aps"];
        assert_eq!(final_payload["event"], "end");
        assert_eq!(final_payload["content-state"]["status"], "failed");
        let replacement = router.open_authenticated_session(Some("phone".into()));
        router.revoke_device("phone");
        assert!(apns.test_payloads(crate::apns::now() + 3).is_empty());
        assert!(
            router
                .register_live_activity(
                    replacement.id(),
                    &params,
                    Thread {
                        id: Some(target),
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn detail_reads_are_ordered_with_live_changes_and_stay_on_the_requesting_connection() {
        let router = SessionRouter::new();
        let first = router.open_session();
        let second = router.open_session();
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(target.clone()),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    items_summary: true,
                    items: Some(vec![]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            model: None,
        };
        let mut first_updates = router
            .finish_session_read(open(&router, "native"), first.id(), response.clone())
            .unwrap()
            .updates
            .unwrap();
        let mut second_updates = router
            .finish_session_read(open(&router, "native"), second.id(), response)
            .unwrap()
            .updates
            .unwrap();
        let item = |text: &str| {
            Arc::new(Item::new(
                "answer".into(),
                Default::default(),
                agent_protocol::models::ItemBody::AssistantText {
                    text: text.into(),
                    phase: Default::default(),
                },
            ))
        };
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: Turn {
                    id: "turn".into(),
                    items: Some(vec![item("current")]),
                    ..Default::default()
                },
                completed: false,
            },
        );
        let _: SessionChange = protocol::decode(&first_updates.recv().await.unwrap()).unwrap();
        let _: SessionChange = protocol::decode(&second_updates.recv().await.unwrap()).unwrap();
        router
            .finish_turn_read(
                open(&router, "native"),
                first.id(),
                "turn".into(),
                vec![item("old")],
            )
            .unwrap();
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "turn".into(),
                item_id: "answer".into(),
                field: agent_protocol::session::TextField::AssistantText,
                delta: " + delta".into(),
            },
        );
        let hydration: SessionChange =
            protocol::decode(&first_updates.recv().await.unwrap()).unwrap();
        let SessionChange::TurnItems { items, .. } = &hydration else {
            panic!("detail patch missing")
        };
        assert!(
            matches!(items[0].body(), agent_protocol::models::ItemBody::AssistantText {text, ..} if text == "current")
        );
        assert!(matches!(
            protocol::decode::<SessionChange>(&first_updates.recv().await.unwrap()).unwrap(),
            SessionChange::Text { .. }
        ));
        assert!(matches!(
            protocol::decode::<SessionChange>(&second_updates.recv().await.unwrap()).unwrap(),
            SessionChange::Text { .. }
        ));
        assert!(second_updates.receiver.try_recv().is_err());
        drop(first_updates);
        assert!(
            router
                .finish_turn_read(open(&router, "native"), first.id(), "turn".into(), vec![],)
                .is_err()
        );
        assert!(second_updates.receiver.try_recv().is_err());
    }
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

    fn assert_no_execution(router: &SessionRouter) {
        let actors: Vec<_> = lock_state(&router.state)
            .executions
            .values()
            .cloned()
            .collect();
        for actor in actors {
            let actor = lock_state(&actor);
            assert!(actor.timeline.turns.is_none());
            assert!(actor.timeline.submissions.is_empty());
            assert!(actor.timeline.requests.is_empty());
            assert!(actor.request_origins.is_empty());
            assert_eq!(actor.leases, 0);
        }
    }
    #[test]
    fn a_busy_conversation_does_not_hold_the_registry_or_another_conversation() {
        let router = SessionRouter::new();
        let a = SessionRef::new(ProviderKind::Codex, "A".into()).unwrap();
        let b = SessionRef::new(ProviderKind::Codex, "B".into()).unwrap();
        let lease = router.retain_execution(a.clone()).unwrap();
        let guard = lock_state(&lease.actor);
        let stalled = std::thread::spawn({
            let router = router.clone();
            move || {
                router.session_change(
                    &a,
                    SessionChange::Status {
                        status: agent_protocol::models::SessionStatus::Running,
                    },
                )
            }
        });
        let (send, receive) = std::sync::mpsc::channel();
        let independent = std::thread::spawn({
            let router = router.clone();
            move || {
                router.session_change(
                    &b,
                    SessionChange::Turn {
                        turn: Turn {
                            id: "turn".into(),
                            ..Default::default()
                        },
                        completed: false,
                    },
                );
                send.send(router.current_turn(&b, "turn").is_some())
                    .unwrap();
            }
        });
        let progress = receive.recv_timeout(std::time::Duration::from_secs(2));
        drop(guard);
        stalled.join().unwrap();
        independent.join().unwrap();
        assert!(progress.unwrap());
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
        let router = SessionRouter::new();
        let target = SessionRef::new(ProviderKind::Claude, "native".into()).unwrap();
        let (input, _receiver) = tokio::sync::mpsc::channel(1);
        let instance = uuid::Uuid::new_v4();
        let adapted = super::super::requests::claude(
            "request".into(),
            &"turn".into(),
            &crate::claude::SdkRequest::Elicitation {
                server_name: String::new(),
                message: String::new(),
                mode: None,
                url: None,
                requested_schema: Some(json!({"type":"object","properties":{}})),
            },
        )
        .unwrap();
        router
            .request(
                target.clone(),
                crate::claude::request_origin(
                    instance,
                    json!("native-request"),
                    input,
                    adapted.answers,
                ),
                adapted.request,
            )
            .unwrap();
        assert!(lock_state(&router.state).executions.contains_key(&target));
        router.resolve_native_request(instance, &json!("native-request"));
        assert!(!lock_state(&router.state).executions.contains_key(&target));
    }
    #[test]
    fn turn_requests_require_live_ownership_and_retire_only_with_their_target() {
        use agent_protocol::{execution::ItemStatus, requests::Answer, session::RequestDelivery};
        let router = SessionRouter::new();
        let connection = router.open_session();
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        // An outstanding native read also retains the completed turn below.
        let _read = open(&router, "native");
        let instance = uuid::Uuid::new_v4();
        let request = || {
            super::super::requests::codex(
                "request".into(),
                "item/commandExecution/requestApproval",
                &json!({"turnId":"wanted","itemId":"tool","availableDecisions":["accept","decline"]}),
            )
            .unwrap()
            .request
        };
        let register = || {
            router.request(
                target.clone(),
                super::super::requests::unavailable_origin(
                    instance,
                    json!("native-request"),
                    Default::default(),
                ),
                request(),
            )
        };
        let pending = || {
            router
                .overlay_execution(&target, Thread::default())
                .requests
        };
        let lifecycle = |id: &str, completed| {
            router.session_change(
                &target,
                SessionChange::Turn {
                    turn: Turn {
                        id: id.into(),
                        ..Default::default()
                    },
                    completed,
                },
            );
        };
        let item = |turn: &str, id: &str, status| {
            router.session_change(
                &target,
                SessionChange::Item {
                    turn_id: turn.into(),
                    item: Arc::new(Item::new(
                        id.into(),
                        status,
                        agent_protocol::items::ItemBody::CommandExecution {
                            command: "true".into(),
                            cwd: None,
                            output: String::new(),
                            exit_code: None,
                        },
                    )),
                },
            );
        };
        assert!(register().is_err());
        lifecycle("other", false);
        assert!(register().is_err());
        lifecycle("wanted", false);
        register().unwrap();
        let previous = pending();
        for (turn, id) in [("other", "tool"), ("wanted", "other"), ("wanted", "tool")] {
            item(turn, id, ItemStatus::Running);
            assert_eq!(pending()["request"].delivery, RequestDelivery::Awaiting);
        }
        router
            .claim_response(
                connection.id(),
                "request",
                &Answer::Approval {
                    choice_id: request().body.choices()[0].id.clone(),
                },
            )
            .unwrap();
        assert_eq!(previous["request"].delivery, RequestDelivery::Awaiting);
        assert_eq!(pending()["request"].delivery, RequestDelivery::Sending);
        router.response_delivery("request", RequestDelivery::Unknown);
        router.response_delivery("request", RequestDelivery::Awaiting);
        assert_eq!(pending()["request"].delivery, RequestDelivery::Unknown);
        item("other", "tool", ItemStatus::Completed);
        assert!(pending().contains_key("request"));
        item("wanted", "tool", ItemStatus::Running);
        assert!(pending().is_empty());
        // Native identity indexes are released along with the normalized request.
        register().unwrap();
        item("wanted", "tool", ItemStatus::Completed);
        assert!(pending().is_empty());
        register().unwrap();
        lifecycle("wanted", true);
        assert!(pending().is_empty());
        assert!(register().is_err());
        assert_eq!(previous["request"].delivery, RequestDelivery::Awaiting);
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
        let mut response = router
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
        assert_no_execution(&router);
        let delta = SessionChange::Text {
            turn_id: "first".into(),
            item_id: "answer".into(),
            field: TextField::AssistantText,
            delta: " forwarded to native history".into(),
        };
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        router.session_change(&target, delta.clone());
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            response.updates.as_mut().unwrap().recv(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(protocol::decode::<SessionChange>(&frame).unwrap(), delta);
        assert_no_execution(&router);
        turn(&router, false);
        assert_eq!(lock_state(&router.state).executions.len(), 1);
        turn(&router, true);
        let state = lock_state(&router.state);
        assert_eq!(state.subscriptions.len(), 1);
        drop(state);
        assert_no_execution(&router);
    }

    #[tokio::test]
    async fn aggregate_item_bodies_are_deferred_to_fit_one_rpc() {
        let router = SessionRouter::new();
        let connection = router.open_session();
        let mut items: Vec<_> = (0..40).map(|id| json!({"id":id.to_string(),"status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(512 * 1024),"phase":"unknown"}}}}})).collect();
        items.push(
            json!({"id":"tool","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"z".repeat(8192),"exitCode":null}}}}}),
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
        assert_no_execution(&router);
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
        assert_no_execution(&router);
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
        assert_no_execution(&router);
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
        let thread = router.overlay_execution(
            &target,
            serde_json::from_value(json!({"id":{"provider":"codex","id":"native"},"turns":[]}))
                .unwrap(),
        );
        assert_eq!(
            thread.submissions["send"],
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
            lock_state(&router.actor(&target)).timeline.status,
            SessionStatus::Running
        );
        router.fail_provider(ProviderKind::Codex, "provider stopped");
        let actor = router.actor(&target);
        let state = lock_state(&actor);
        let live = &state.timeline;
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
        let budget = lock_state(&router.state).sessions[&connection.id()]
            .bytes
            .clone();
        let actor = router.actor(&SessionRef::new(ProviderKind::Codex, "native".into()).unwrap());
        let (a, b) = {
            let actor = lock_state(&actor);
            (
                actor.subscriptions[&first.id].1.clone(),
                actor.subscriptions[&second.id].1.clone(),
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
            super::requests::claude(
                uuid::Uuid::new_v4().to_string().into(),
                &"turn".into(),
                &crate::claude::SdkRequest::Tool {
                    tool_name: "Bash".into(),
                    tool_use_id: Some("tool".into()),
                    input: serde_json::json!({"command":"pwd"}),
                },
            )
            .unwrap()
        };
        let request_id = adapted.request.id.clone();
        let choice_id = adapted.request.body.choices()[0].id.clone();
        let origin = if provider == ProviderKind::Codex {
            super::requests::unavailable_origin(instance, native.clone(), Default::default())
        } else {
            crate::claude::request_origin(instance, native.clone(), input.clone(), adapted.answers)
        };
        router
            .request(target.clone(), origin, adapted.request)
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
