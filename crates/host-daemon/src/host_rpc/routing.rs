//! Live conversation routing. One registry lock covers connections, task facts
//! and request ownership; a conversation actor lock is always taken first.
use super::session_actor::SessionActor;
use crate::providers::{AgentChange, Failure, RequestOrigin};
use agent_protocol::live_activity::TaskState;
use agent_protocol::{
    models::{Thread, ThreadResponse},
    protocol::{self, Notification},
    session::{OpenedSession, ProviderKind, SessionChange, SessionRef},
};
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

mod activity;
mod connections;
mod read_state;
mod requests;
#[cfg(test)]
mod tests;

use activity::record_task_facts;
pub use connections::HostSession;
pub(super) use connections::Outbound;
use connections::{MAX_QUEUED_BYTES, frame_cost};
use requests::native_key;

pub type SessionId = u64;

/// Activity bodies sent with an opened turn. A typical turn's commands and
/// diffs fit, so expanding a row needs no further round trip, while one huge
/// turn still opens with a bounded frame.
const OPENED_ACTIVITY_BYTES: usize = 128 * 1024;

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
        prune(&self.router.state, &self.target, &self.actor);
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
                prune(&state, &self.target, &actor);
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

/// Connections, subscriptions and request routing. Activity has its own lock.
#[derive(Default)]
struct State {
    list_revision: u64,
    next_session_id: SessionId,
    sessions: HashMap<SessionId, Outbound>,
    executions: HashMap<SessionRef, Arc<Mutex<SessionActor>>>,
    subscriptions: HashMap<uuid::Uuid, (SessionRef, SessionId)>,
    requests: HashMap<agent_protocol::ids::RequestId, SessionRef>,
    native_requests: HashMap<(uuid::Uuid, String), agent_protocol::ids::RequestId>,
    store: Option<Arc<super::store::ConversationStore>>,
}
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
    activity: Arc<Mutex<activity::ActivityState>>,
}
fn lock_state<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}
fn record_failure(result: Result<(), String>) {
    if let Err(message) = result {
        tracing::error!(target: "agent", operation = "host.transcript.write", message);
    }
}

fn prune(state: &Mutex<State>, target: &SessionRef, actor: &Arc<Mutex<SessionActor>>) {
    let mut owned = lock_state(actor);
    if owned.release() || !owned.subscriptions.is_empty() {
        return;
    }
    let mut state = lock_state(state);
    if Arc::strong_count(actor) == 2
        && state
            .executions
            .get(target)
            .is_some_and(|current| Arc::ptr_eq(current, actor))
    {
        state.executions.remove(target);
    }
}

impl SessionRouter {
    /// Apply one routed provider change to live sessions and subscribers.
    pub(crate) fn apply(
        &self,
        change: AgentChange,
        bindings: &[(SessionRef, ProviderKind)],
    ) -> Result<(), String> {
        match change {
            AgentChange::TaskParent { .. } => {}
            AgentChange::Session { session, change } => self.session_change(&session, change),
            AgentChange::Request {
                session,
                origin,
                request,
            } => return self.request(session, origin, request),
            AgentChange::Resolved {
                instance,
                native_id,
            } => self.resolve_native_request(instance, &native_id),
            AgentChange::SourceClosed(instance) => self.close_request_source(instance),
            AgentChange::Renamed { session, name } => {
                let store = lock_state(&self.state).store.clone();
                if let Some(store) = store {
                    record_failure(store.rename(&session, name.as_deref()));
                }
                self.broadcast(Notification::SessionRenamed {
                    session,
                    name,
                    revision: 0,
                })
            }
            AgentChange::Stopped { provider, reason } => {
                self.fail_provider(provider, &reason, bindings)
            }
        }
        Ok(())
    }
    pub(crate) fn new() -> Self {
        Self {
            activity: Default::default(),
            state: Arc::new(Mutex::new(State {
                next_session_id: 1,
                ..Default::default()
            })),
        }
    }
    /// Finished turns are recorded while their conversation is still locked,
    /// so the store observes them in execution order.
    pub(super) fn with_store(self, store: Arc<super::store::ConversationStore>) -> Self {
        {
            lock_state(&self.activity).reads = read_state::ReadState::load(store.clone());
            lock_state(&self.state).store = Some(store);
        }
        self
    }
    /// Running turns end with the Host; record what they produced.
    pub(super) fn record_running(&self) {
        let (store, actors) = {
            let state = lock_state(&self.state);
            let actors: Vec<_> = state
                .executions
                .iter()
                .map(|(target, actor)| (target.clone(), actor.clone()))
                .collect();
            (state.store.clone(), actors)
        };
        let Some(store) = store else {
            return;
        };
        for (target, actor) in actors {
            let actor = lock_state(&actor);
            for turn in actor.timeline.turns.iter().flatten() {
                if turn.status == agent_protocol::execution::TurnStatus::Running {
                    record_failure(store.record(&target, turn, None));
                }
            }
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
    pub(super) fn submission_lock(&self, target: &SessionRef) -> Arc<tokio::sync::Mutex<()>> {
        lock_state(&self.actor(target)).submission_lock.clone()
    }
    pub(crate) fn overlay_execution(&self, target: &SessionRef, mut thread: Thread) -> Thread {
        if let Some(actor) = self.existing(target) {
            thread = lock_state(&actor).overlay(thread);
            prune(&self.state, target, &actor);
        }
        thread
    }
    /// A rewind releases delivery evidence and live overlays from the old runtime,
    /// while retaining the actor's subscriptions and serialization lock.
    pub(super) fn reset_history(&self, target: &SessionRef) {
        let actor = self.actor(target);
        let mut owned = lock_state(&actor);
        owned.timeline = Default::default();
        owned.request_origins.clear();
        owned.queued_turns.clear();
        let mut state = lock_state(&self.activity);
        state.tasks.remove(target);
        state.reads.forget(target);
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
        prune(&self.state, target, &actor);
        receipt
    }
    /// The conversation's input waiting for its running turn.
    pub(super) fn queue(&self, target: &SessionRef) -> agent_protocol::operations::InputQueue {
        let Some(actor) = self.existing(target) else {
            return Default::default();
        };
        let queue = lock_state(&actor).timeline.queue.clone();
        prune(&self.state, target, &actor);
        queue
    }
    /// Input waits and nothing holds the queue.
    pub(super) fn queue_ready(&self, target: &SessionRef) -> bool {
        self.existing(target).is_some_and(|actor| {
            let owned = lock_state(&actor);
            !owned.timeline.queue.held && !owned.timeline.queue.inputs.is_empty()
        })
    }
    /// The turn that runs now, if the Host observed one.
    pub(super) fn running_turn(&self, target: &SessionRef) -> Option<agent_protocol::ids::TurnId> {
        let actor = self.existing(target)?;
        let turn = lock_state(&actor)
            .timeline
            .turns
            .iter()
            .flatten()
            .rfind(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
            .map(|turn| turn.id.clone());
        prune(&self.state, target, &actor);
        turn
    }
    /// Marks the turn that queued input started, now and when it changes later,
    /// as T3 Code labels a message that waited in its queue.
    pub(super) fn mark_queued_turn(
        &self,
        target: &SessionRef,
        turn_id: agent_protocol::ids::TurnId,
    ) {
        let actor = self.actor(target);
        let mut owned = lock_state(&actor);
        let current = owned
            .timeline
            .turns
            .iter()
            .flatten()
            .rfind(|turn| turn.id == turn_id)
            .cloned();
        owned.queued_turns.insert(turn_id);
        let mut failed = Vec::new();
        if let Some(turn) = current.filter(|turn| !turn.queued) {
            let completed = turn.status != agent_protocol::execution::TurnStatus::Running;
            let turn = agent_protocol::models::Turn {
                items: None,
                ..turn.as_ref().clone()
            };
            self.change_locked(
                &mut owned,
                target,
                &SessionChange::Turn { turn, completed },
                &mut failed,
            );
        }
        drop(owned);
        self.close_failed(failed);
        prune(&self.state, target, &actor);
    }
    pub(super) fn running(&self, target: &SessionRef) -> bool {
        self.existing(target).is_some_and(|actor| {
            lock_state(&actor).timeline.status == agent_protocol::models::SessionStatus::Running
        })
    }

    pub(super) fn begin_submission(&self, target: &SessionRef, id: &str) -> Result<(), Failure> {
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
                !matches!(
                    delivery,
                    agent_protocol::session::SubmissionDelivery::Rejected
                        | agent_protocol::session::SubmissionDelivery::Withdrawn
                )
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
        prune(&self.state, target, &actor);
    }
    pub(crate) fn item(
        &self,
        target: &SessionRef,
        turn_id: &agent_protocol::ids::TurnId,
        item_id: &agent_protocol::ids::ItemId,
    ) -> Option<Arc<agent_protocol::models::Item>> {
        let actor = self.existing(target)?;
        let owned = lock_state(&actor);
        let turns = owned.timeline.turns.as_deref().unwrap_or_default();
        let result = agent_protocol::session::item_position(turns, turn_id, item_id)
            .map(|(turn, item)| turns[turn].items.as_ref().unwrap()[item].clone());
        drop(owned);
        prune(&self.state, target, &actor);
        result
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
        prune(&self.state, target, &actor);
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
        agent_protocol::models::defer_turn_items(
            &mut items,
            agent_protocol::models::MAX_INLINE_ITEM_BYTES,
            OPENED_ACTIVITY_BYTES,
        );
        let mut change = SessionChange::TurnItems { turn_id, items };
        let mut frame = protocol::encode(&change).map_err(|error| error.to_string())?;
        if frame.len() > MAX_QUEUED_BYTES {
            if let SessionChange::TurnItems { items, .. } = &mut change {
                agent_protocol::models::defer_turn_items(items, 0, 0);
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
        known: bool,
    ) -> Result<HostReply, String> {
        let connection = self
            .connection(session)
            .ok_or("connection closed during session open")?;
        let mut actor = lock_state(&read.actor);
        if response.thread.id.as_ref() != Some(&read.target) {
            return Err("native session ID does not match".into());
        }
        // Without live turns, a client holding this transcript version needs
        // only the current metadata; its turns and paging are unchanged.
        let unchanged = known && actor.timeline.turns.is_none();
        response.thread = actor.overlay(response.thread);
        if unchanged {
            let thread = &mut response.thread;
            thread.turns = None;
            thread.history_cursor = None;
            thread.history_has_more = None;
            thread.history_read_state = None;
        }
        response
            .thread
            .defer_item_details(agent_protocol::models::MAX_INLINE_ITEM_BYTES);
        let mut opened = OpenedSession {
            session: read.target.clone(),
            subscription_id: uuid::Uuid::nil(),
            unchanged,
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
        // Conversation -> registry follows the router lock order. Registration,
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
    pub(crate) fn session_change(&self, target: &SessionRef, change: SessionChange) {
        let actor = self.actor(target);
        let mut failed = Vec::new();
        self.change_locked(&mut lock_state(&actor), target, &change, &mut failed);
        self.close_failed(failed);
        prune(&self.state, target, &actor);
    }
    pub(crate) fn fail_provider(
        &self,
        provider: ProviderKind,
        message: &str,
        bindings: &[(SessionRef, ProviderKind)],
    ) {
        let actors: Vec<_> = lock_state(&self.state)
            .executions
            .iter()
            .filter(|(target, _)| {
                bindings
                    .iter()
                    .find(|(root, _)| root == *target)
                    .map_or(target.provider, |(_, active)| *active)
                    == provider
            })
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
            // The provider's background work ended with it.
            if !owned.timeline.background.is_empty() {
                changes.push(SessionChange::Background { tasks: Vec::new() });
            }
            changes.push(SessionChange::Status {
                status: agent_protocol::models::SessionStatus::Unavailable,
            });
            let mut failed = Vec::new();
            for change in changes {
                self.change_locked(&mut owned, &target, &change, &mut failed);
            }
            drop(owned);
            self.close_failed(failed);
            prune(&self.state, &target, &actor);
        }
    }
    fn change_locked(
        &self,
        actor: &mut SessionActor,
        target: &SessionRef,
        change: &SessionChange,
        failed: &mut Vec<SessionId>,
    ) {
        let stamped;
        let change = match change {
            SessionChange::Turn { turn, completed }
                if !turn.queued && actor.queued_turns.contains(&turn.id) =>
            {
                stamped = SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        queued: true,
                        ..turn.clone()
                    },
                    completed: *completed,
                };
                &stamped
            }
            change => change,
        };
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
        let previous_turns = actor.timeline.turns.as_deref().unwrap_or_default();
        let removed_from_history = matches!(change, SessionChange::RemoveItem { turn_id, item_id }
            if agent_protocol::session::item_position(previous_turns, turn_id, item_id).is_none());
        let updated = change.turn_index(previous_turns);
        let store = lock_state(&self.state).store.clone();
        let archived_item = if let (Some(store), SessionChange::Item { turn_id, item }) =
            (&store, change)
        {
            agent_protocol::session::item_position(previous_turns, turn_id, &item.id).is_none()
                && match store.item(target, turn_id, &item.id, false) {
                    Ok(item) => item.is_some(),
                    Err(message) => {
                        tracing::error!(target: "agent", operation = "host.transcript.read", message);
                        false
                    }
                }
        } else {
            false
        };
        let timeline = std::mem::take(&mut actor.timeline);
        let (next, result) = if archived_item {
            (
                timeline,
                Err(agent_protocol::session::UpdateError::MissingItem),
            )
        } else {
            change.apply_timeline(timeline)
        };
        actor.timeline = next;
        // Finished turns, and later updates to them, are recorded as they change.
        let turns = actor.timeline.turns.as_deref().unwrap_or_default();
        let finished = updated
            .or_else(|| change.turn_index(turns))
            .and_then(|index| {
                turns
                    .get(index)
                    .filter(|current| {
                        result.is_ok()
                            && !removed_from_history
                            && current.status != agent_protocol::execution::TurnStatus::Running
                    })
                    .cloned()
            });
        // An update may target a finished turn that live execution no longer
        // retains; the transcript applies it, and so do subscribers.
        let late = removed_from_history
            || matches!(
                result,
                Err(agent_protocol::session::UpdateError::MissingTurn
                    | agent_protocol::session::UpdateError::MissingItem)
            );
        if let Err(reason) = result
            && !late
        {
            tracing::warn!(target: "agent", operation = "host.session.invalid_update", message = %reason);
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
            let active = agent_protocol::models::task_active(None, actor.timeline.status);
            let frame = protocol::encode(Notification::Activity {
                session: target.clone(),
                active,
            })
            .expect("activity encodes");
            failed.extend(self.broadcast_frames(frame));
        }
        {
            let mut activity = lock_state(&self.activity);
            if let Some(update) = record_task_facts(&mut activity, target, &actor.timeline, change)
            {
                failed.extend(activity::publish_task_activity(
                    activity.apns.as_deref(),
                    &lock_state(&self.state).sessions,
                    &update,
                ));
            }
        }
        // The queue survives a Host restart, as T3 Code's queued runs do.
        if let (Some(store), SessionChange::Queue { queue }) = (&store, change) {
            record_failure(store.save_queue(target, queue));
        }
        if let (Some(store), Some(turn)) = (&store, finished) {
            let removed_item = match change {
                SessionChange::RemoveItem { item_id, .. } => Some(item_id),
                _ => None,
            };
            record_failure(store.record(target, &turn, removed_item));
        }
        if let (Some(store), true, Some(turn_id)) = (&store, late, change.turn_id()) {
            record_failure(
                store.update_turn(target, turn_id, change.item_id(), |turn| {
                    let (timeline, result) =
                        change.apply_timeline(agent_protocol::session::Timeline {
                            turns: Some(vec![Arc::new(turn)]),
                            ..Default::default()
                        });
                    result.ok()?;
                    timeline.turns?.pop().map(Arc::unwrap_or_clone)
                }),
            );
        }
        actor.release();
    }
}
