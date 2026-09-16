//! Current provider state and subscriptions, bounded and never persisted.
//! The router holds the lock through snapshot adoption and queue delivery.
use super::routing::SessionId;
use agent_core::{
    models::ThreadResponse,
    session::{OpenedSession, SessionChange, SessionRef, SessionUpdate},
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const MAX_SESSIONS: usize = 128;
pub(super) const MAX_LIMIT: usize = 1000;
pub(super) const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct SessionRuntime {
    entries: HashMap<SessionRef, Entry>,
    clock: u64,
}

#[derive(Default)]
struct Entry {
    revision: u64,
    bytes: usize,
    complete: bool,
    response: Option<ThreadResponse>,
    limit: usize,
    active: bool,
    submissions: HashSet<String>,
    active_turn: Option<String>,
    last_used: u64,
    readers: usize,
    subscriptions: HashMap<Uuid, SessionId>,
}

pub(super) struct ReadToken {
    target: SessionRef,
    revision: u64,
    pub(super) limit: usize,
}

impl SessionRuntime {
    fn entry(&mut self, target: &SessionRef) -> Result<&mut Entry, &'static str> {
        if !self.entries.contains_key(target) && self.entries.len() >= MAX_SESSIONS {
            let candidate = self
                .entries
                .iter()
                .filter(|(_, e)| {
                    !e.active
                        && e.submissions.is_empty()
                        && e.readers == 0
                        && e.subscriptions.is_empty()
                        && e.response
                            .as_ref()
                            .is_none_or(|response| response.thread.requests.is_empty())
                })
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| id.clone());
            let Some(candidate) = candidate else {
                return Err("session capacity reached; close an idle conversation and retry");
            };
            self.entries.remove(&candidate);
        }
        self.clock += 1;
        let entry = self.entries.entry(target.clone()).or_default();
        entry.last_used = self.clock;
        Ok(entry)
    }

    pub(super) fn submission(
        &mut self,
        target: &SessionRef,
        id: &str,
        pending: bool,
    ) -> Result<(), &'static str> {
        let entry = self.entry(target)?;
        if pending {
            if entry.submissions.len() >= 128 {
                return Err("session has too many unconfirmed inputs");
            }
            entry.submissions.insert(id.into());
        } else {
            entry.submissions.remove(id);
        }
        Ok(())
    }

    pub(super) fn created(&mut self, response: ThreadResponse) -> Result<(), &'static str> {
        let target = SessionRef::from_thread_id(
            response
                .thread
                .id
                .as_deref()
                .ok_or("native session ID missing")?,
        )?;
        if self
            .entries
            .get(&target)
            .is_some_and(|entry| entry.response.is_some())
        {
            return Ok(());
        }
        let entry = self.entry(&target)?;
        let bytes = serde_json::to_vec(&response)
            .map_err(|_| "invalid created session")?
            .len();
        if bytes > MAX_SNAPSHOT_BYTES {
            return Err("created session exceeds byte limit");
        }
        entry.bytes = bytes;
        entry.complete = response.thread.turns.as_ref().is_some_and(Vec::is_empty);
        entry.response = Some(response);
        Ok(())
    }

    pub(super) fn begin_read(
        &mut self,
        target: SessionRef,
        limit: usize,
    ) -> Result<ReadToken, &'static str> {
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err("session limit must be between 1 and 1000");
        }
        // Reuse the full native ID; a malformed reference must never become a
        // request for another session through the legacy provider prefix.
        if SessionRef::from_thread_id(&target.thread_id())? != target {
            return Err("invalid session reference");
        }
        let entry = self.entry(&target)?;
        entry.readers += 1;
        let limit = limit.max(entry.limit);
        Ok(ReadToken {
            target,
            revision: entry.revision,
            limit,
        })
    }

    pub(super) fn cached(&self, token: &ReadToken) -> Option<ThreadResponse> {
        let entry = self.entries.get(&token.target)?;
        (entry.response.as_ref().is_some_and(|response| {
            response
                .thread
                .extra
                .get("historyReadState")
                .is_none_or(|state| state["type"] != "unavailable")
        }) && (entry.complete || entry.limit >= token.limit))
            .then(|| entry.response.clone())
            .flatten()
    }

    pub(super) fn cancel_read(&mut self, token: ReadToken) {
        if let Some(entry) = self.entries.get_mut(&token.target) {
            entry.readers = entry.readers.saturating_sub(1);
        }
    }

    /// Returns a snapshot only when nothing changed during native hydration.
    /// Concurrent updates are observed through a revision, not a replay log.
    pub(super) fn finish_read(
        &mut self,
        token: ReadToken,
        connection: SessionId,
        response: ThreadResponse,
    ) -> Result<OpenedSession, &'static str> {
        let entry = self
            .entries
            .get_mut(&token.target)
            .ok_or("session read expired")?;
        entry.readers = entry.readers.saturating_sub(1);
        if entry.revision != token.revision {
            return Err("session changed during history read; open again");
        }
        if response.thread.id.as_deref() != Some(token.target.thread_id().as_str()) {
            return Err("native session ID does not match");
        }
        // An older, smaller concurrent read must not shrink another view's
        // already-adopted range even when no provider event separated them.
        let (mut response, limit) = match &entry.response {
            Some(current) if entry.limit > token.limit => (current.clone(), entry.limit),
            _ => (response, token.limit),
        };
        response.thread.requests = entry
            .response
            .as_ref()
            .map(|current| current.thread.requests.clone())
            .unwrap_or_default();
        let bytes = bound_snapshot(&mut response)?;
        entry.active = response
            .thread
            .status
            .as_ref()
            .is_some_and(|status| status.kind == "active")
            || response
                .thread
                .turns
                .iter()
                .flatten()
                .any(|turn| turn.status.as_deref() == Some("inProgress"));
        entry.complete = response
            .thread
            .extra
            .get("historyReadState")
            .is_none_or(|state| state["type"] == "complete")
            && response.thread.extra.get("historyHasMore") == Some(&serde_json::Value::Bool(false))
            && !response
                .thread
                .turns
                .iter()
                .flatten()
                .any(|turn| turn.items_has_more == Some(true));
        entry.active_turn = response
            .thread
            .turns
            .iter()
            .flatten()
            .rev()
            .find(|turn| turn.status.as_deref() == Some("inProgress"))
            .map(|turn| turn.id.clone());
        entry.limit = limit;
        entry.bytes = bytes;
        entry.response = Some(response.clone());
        if entry.subscriptions.len() >= 256 {
            return Err("session subscription capacity reached");
        }
        let subscription_id = Uuid::new_v4();
        entry.subscriptions.insert(subscription_id, connection);
        Ok(OpenedSession {
            session: token.target,
            subscription_id,
            revision: entry.revision,
            response,
        })
    }

    pub(super) fn close(&mut self, connection: SessionId, id: Uuid) {
        for entry in self.entries.values_mut() {
            if entry.subscriptions.get(&id) == Some(&connection) {
                entry.subscriptions.remove(&id);
            }
        }
        self.release_idle();
    }

    fn release_idle(&mut self) {
        self.entries.retain(|_, entry| {
            entry.active
                || !entry.submissions.is_empty()
                || entry.readers > 0
                || !entry.subscriptions.is_empty()
                || entry
                    .response
                    .as_ref()
                    .is_some_and(|response| !response.thread.requests.is_empty())
        });
    }

    pub(super) fn disconnect(&mut self, connection: SessionId) {
        for entry in self.entries.values_mut() {
            entry.subscriptions.retain(|_, owner| *owner != connection);
        }
        self.release_idle();
    }

    pub(super) fn update(
        &mut self,
        target: &SessionRef,
        change: &SessionChange,
    ) -> Result<Vec<(SessionId, SessionUpdate)>, &'static str> {
        let Some(entry) = self.entries.get_mut(target) else {
            return Ok(Vec::new());
        };
        let mut bounded = change.clone();
        match &mut bounded {
            SessionChange::Item { item, .. } => {
                item.defer_large_detail();
            }
            SessionChange::Turn { turn, .. } => {
                for item in turn.items.iter_mut().flatten() {
                    std::sync::Arc::make_mut(item).defer_large_detail();
                }
            }
            SessionChange::Text {
                turn_id, item_id, ..
            } => {
                if let Some(response) = &entry.response {
                    let current = response
                        .thread
                        .turns
                        .iter()
                        .flatten()
                        .rev()
                        .find(|turn| turn.id == *turn_id)
                        .and_then(|turn| turn.items.as_ref())
                        .and_then(|items| items.iter().find(|item| item.id == *item_id));
                    if current.is_some_and(|item| {
                        item.extra.get("detailDeferred") == Some(&serde_json::Value::Bool(true))
                    }) {
                        return Ok(Vec::new());
                    }
                    let updated = change.apply(&response.thread)?;
                    if let Some(item) = updated
                        .turns
                        .iter()
                        .flatten()
                        .rev()
                        .find(|turn| turn.id == *turn_id)
                        .and_then(|turn| turn.items.as_ref())
                        .and_then(|items| items.iter().find(|item| item.id == *item_id))
                    {
                        let mut item = (**item).clone();
                        if item.defer_large_detail() {
                            bounded = SessionChange::Item {
                                turn_id: turn_id.clone(),
                                item,
                            };
                        }
                    }
                }
            }
            _ => {}
        }
        let change = &bounded;
        match change {
            SessionChange::Item { item, .. } => {
                if let Some(id) = &item.client_id {
                    entry.submissions.remove(id);
                }
            }
            SessionChange::Turn { turn, .. } => {
                for item in turn.items.iter().flatten() {
                    if let Some(id) = &item.client_id {
                        entry.submissions.remove(id);
                    }
                }
            }
            _ => {}
        }
        entry.revision += 1;
        if matches!(change, SessionChange::Turn { .. }) {
            entry.complete = false;
        }
        match change {
            SessionChange::Status { status } => entry.active = status.kind == "active",
            SessionChange::Turn {
                completed: false,
                turn,
            } => {
                entry.active = true;
                entry.active_turn = Some(turn.id.clone());
            }
            SessionChange::Turn {
                completed: true,
                turn,
            } if entry.active_turn.as_deref() == Some(&turn.id) => {
                entry.active = false;
                entry.active_turn = None;
            }
            _ => {}
        }
        if let Some(response) = &mut entry.response {
            let previous = response.clone();
            response.thread = match change.apply(&response.thread) {
                Ok(thread) => thread,
                Err(error) => {
                    entry.response = None;
                    return Err(error);
                }
            };
            entry.limit = entry
                .limit
                .max(response.thread.turns.as_ref().map_or(0, Vec::len));
            // Text deltas account only for their encoded growth. Do not encode
            // the entire conversation for every streaming character.
            entry.bytes = match change {
                SessionChange::Text { delta, .. } => entry.bytes.saturating_add(
                    serde_json::to_vec(delta)
                        .map_err(|_| "invalid text delta")?
                        .len(),
                ),
                _ => serde_json::to_vec(response)
                    .map_err(|_| "invalid session update")?
                    .len(),
            };
            if entry.bytes > MAX_SNAPSHOT_BYTES {
                entry.bytes = match bound_snapshot(response) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        *response = previous;
                        entry.complete = false;
                        return Err(error);
                    }
                };
                entry.complete = false;
                return Err(
                    "session display window reached its byte limit; reopen the conversation",
                );
            }
        }
        Ok(entry
            .subscriptions
            .iter()
            .map(|(id, connection)| {
                (
                    *connection,
                    SessionUpdate {
                        subscription_id: *id,
                        revision: entry.revision,
                        change: change.clone(),
                    },
                )
            })
            .collect())
    }

    pub(super) fn pending_request(
        &self,
        id: &str,
    ) -> Option<(SessionRef, agent_core::client::ServerRequest)> {
        self.entries.iter().find_map(|(target, entry)| {
            let request = entry.response.as_ref()?.thread.requests.get(id)?;
            Some((target.clone(), (**request).clone()))
        })
    }
    pub(super) fn pending_native(
        &self,
        provider: agent_core::session::ProviderKind,
        id: &serde_json::Value,
    ) -> Option<(SessionRef, agent_core::client::ServerRequest)> {
        self.entries.iter().find_map(|(target, entry)| {
            if target.provider != provider {
                return None;
            }
            let request = entry
                .response
                .as_ref()?
                .thread
                .requests
                .values()
                .find(|request| {
                    request
                        .extra
                        .get("nativeRequestId")
                        .is_some_and(|native| native == id)
                })?;
            Some((target.clone(), (**request).clone()))
        })
    }
    pub(super) fn pending_ids(&self) -> Vec<String> {
        self.entries
            .values()
            .filter_map(|entry| entry.response.as_ref())
            .flat_map(|response| response.thread.requests.keys().cloned())
            .collect()
    }

    pub(super) fn invalidate(
        &mut self,
        target: Option<&SessionRef>,
        discard_current: bool,
    ) -> Vec<SessionId> {
        let mut connections = Vec::new();
        for (id, entry) in &mut self.entries {
            if target.is_none_or(|target| target == id) {
                if discard_current {
                    entry.response = None;
                }
                entry.revision += 1;
                connections.extend(entry.subscriptions.values().copied());
            }
        }
        connections
    }

    pub(super) fn current(&self, target: &SessionRef) -> Option<ThreadResponse> {
        self.entries.get(target)?.response.clone()
    }
}

/// Keep execution and authorization state while bounding the visible history.
/// A trimmed window is explicit and causes subscribers to reopen, never silently
/// dropping a delta while leaving a client believing its history is complete.
fn bound_snapshot(response: &mut ThreadResponse) -> Result<usize, &'static str> {
    for turn in response.thread.turns.iter_mut().flatten() {
        if let Some(opening) = &mut std::sync::Arc::make_mut(turn).opening_user_message {
            std::sync::Arc::make_mut(opening).defer_large_detail();
        }
        for item in std::sync::Arc::make_mut(turn).items.iter_mut().flatten() {
            std::sync::Arc::make_mut(item).defer_large_detail();
        }
    }
    loop {
        let bytes = serde_json::to_vec(response)
            .map_err(|_| "invalid session snapshot")?
            .len();
        if bytes <= MAX_SNAPSHOT_BYTES {
            return Ok(bytes);
        }
        // Defer inline image bodies before reducing the requested turn range.
        let mut deferred = false;
        let mut remaining = bytes;
        'images: for turn in response.thread.turns.iter_mut().flatten() {
            for item in std::sync::Arc::make_mut(turn).items.iter_mut().flatten() {
                if remaining <= MAX_SNAPSHOT_BYTES {
                    break 'images;
                }
                if item.kind.as_deref() == Some("imageGeneration") && item.result.is_some() {
                    let before = serde_json::to_vec(item)
                        .map_err(|_| "invalid image item")?
                        .len();
                    std::sync::Arc::make_mut(item).defer_detail();
                    let after = serde_json::to_vec(item)
                        .map_err(|_| "invalid image header")?
                        .len();
                    remaining = remaining.saturating_sub(before.saturating_sub(after));
                    deferred = true;
                }
            }
        }
        if deferred {
            continue;
        }
        let thread = &mut response.thread;
        let turns = thread
            .turns
            .as_mut()
            .ok_or("session metadata exceeds byte limit")?;
        if turns.len() > 1
            && let Some(index) = turns.iter().position(|turn| {
                turn.status.as_deref() != Some("inProgress")
                    && !thread.requests.values().any(|request| {
                        request
                            .params
                            .get("turnId")
                            .and_then(serde_json::Value::as_str)
                            == Some(&turn.id)
                    })
            })
        {
            turns.remove(index);
        } else if let Some(turn) = turns
            .iter_mut()
            .find(|turn| turn.items.as_ref().is_some_and(|items| items.len() > 2))
        {
            let turn = std::sync::Arc::make_mut(turn);
            let items = turn.items.as_mut().unwrap();
            if turn.opening_user_message.is_none() {
                turn.opening_user_message = items
                    .iter()
                    .find(|item| item.kind.as_deref() == Some("userMessage"))
                    .cloned();
            }
            items.drain(..items.len() / 2);
            turn.items_has_more = Some(true);
        } else {
            return Err("session snapshot exceeds byte limit; native details are required");
        }
        thread.extra.insert("historyHasMore".into(), true.into());
        thread.extra.insert("historyReadState".into(), serde_json::json!({"type":"partial","issues":["表示範囲が4 MiBの上限に達しました。全文は項目の詳細から取得してください。"]}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::models::{Thread, ThreadStatus};
    fn target() -> SessionRef {
        SessionRef::from_thread_id("native").unwrap()
    }
    fn response() -> ThreadResponse {
        ThreadResponse {
            thread: Thread {
                id: Some("native".into()),
                turns: Some(Vec::new()),
                ..Default::default()
            },
            model: None,
            extra: Default::default(),
        }
    }
    fn active() -> SessionChange {
        SessionChange::Status {
            status: ThreadStatus {
                kind: "active".into(),
                extra: Default::default(),
            },
        }
    }
    #[test]
    fn image_bodies_are_deferred_without_losing_the_requested_history() {
        let mut snapshot = response();
        snapshot.thread.turns = Some(
            (0..8)
                .map(|index| {
                    std::sync::Arc::new(agent_core::models::Turn {
                        id: index.to_string(),
                        items: Some(vec![std::sync::Arc::new(agent_core::models::Item {
                            id: format!("image-{index}"),
                            kind: Some("imageGeneration".into()),
                            result: Some(serde_json::Value::String("a".repeat(1_200_000))),
                            ..Default::default()
                        })]),
                        ..Default::default()
                    })
                })
                .collect(),
        );
        assert!(bound_snapshot(&mut snapshot).unwrap() <= MAX_SNAPSHOT_BYTES);
        let turns = snapshot.thread.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 8);
        assert_eq!(
            turns[0].items.as_ref().unwrap()[0].extra["detailDeferred"],
            true
        );
        assert!(
            turns.last().unwrap().items.as_ref().unwrap()[0]
                .result
                .is_some()
        );
        assert_ne!(
            snapshot.thread.extra.get("historyHasMore"),
            Some(&true.into())
        );
    }
    #[test]
    fn unknown_inputs_pin_state_after_the_last_view_closes() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        let opened = runtime.finish_read(token, 1, response()).unwrap();
        runtime.submission(&target(), "send", true).unwrap();
        runtime.close(1, opened.subscription_id);
        assert!(runtime.current(&target()).is_some());
        runtime.submission(&target(), "send", false).unwrap();
        runtime.disconnect(1);
        assert!(runtime.current(&target()).is_none());
    }
    #[test]
    fn an_update_during_initial_read_requires_a_fresh_snapshot() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        assert!(runtime.update(&target(), &active()).unwrap().is_empty());
        assert!(runtime.finish_read(token, 1, response()).is_err());
        let token = runtime.begin_read(target(), 5).unwrap();
        let open = runtime.finish_read(token, 1, response()).unwrap();
        let events = runtime.update(&target(), &active()).unwrap();
        assert_eq!(events[0].1.subscription_id, open.subscription_id);
        assert_eq!(events[0].1.revision, open.revision + 1);
    }
    #[test]
    fn closing_a_view_keeps_execution_and_other_subscribers() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        let first = runtime.finish_read(token, 1, response()).unwrap();
        let token = runtime.begin_read(target(), 5).unwrap();
        let second = runtime.finish_read(token, 2, response()).unwrap();
        runtime.close(2, first.subscription_id);
        assert_eq!(runtime.update(&target(), &active()).unwrap().len(), 2);
        runtime.close(1, first.subscription_id);
        let events = runtime.update(&target(), &active()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.subscription_id, second.subscription_id);
        runtime.disconnect(2);
        assert!(runtime.entries[&target()].active);
        assert!(runtime.entries[&target()].subscriptions.is_empty());
    }
    #[test]
    fn stale_subscription_ids_cannot_close_a_reopened_conversation() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        let first = runtime.finish_read(token, 1, response()).unwrap();
        let token = runtime.begin_read(target(), 5).unwrap();
        let current = runtime.finish_read(token, 1, response()).unwrap();
        assert_ne!(first.subscription_id, current.subscription_id);
        runtime.close(1, first.subscription_id);
        assert_eq!(
            runtime.update(&target(), &active()).unwrap()[0]
                .1
                .subscription_id,
            current.subscription_id
        );
    }
    #[test]
    fn capacity_does_not_evict_active_or_subscribed_sessions() {
        let mut runtime = SessionRuntime::default();
        for id in 0..MAX_SESSIONS {
            let target = SessionRef::from_thread_id(&id.to_string()).unwrap();
            let token = runtime.begin_read(target.clone(), 5).unwrap();
            runtime.update(&target, &active()).unwrap();
            runtime.cancel_read(token);
        }
        assert!(runtime.begin_read(target(), 5).is_err());
    }
    #[test]
    fn failed_reads_do_not_clear_a_cached_conversation() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        runtime.finish_read(token, 1, response()).unwrap();
        let token = runtime.begin_read(target(), 20).unwrap();
        runtime.cancel_read(token);
        let token = runtime.begin_read(target(), 5).unwrap();
        assert_eq!(runtime.cached(&token), Some(response()));
        runtime.cancel_read(token);
        assert_eq!(runtime.entries[&target()].readers, 0);
    }

    #[test]
    fn reopening_preserves_the_larger_history_window() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 30).unwrap();
        runtime.finish_read(token, 1, response()).unwrap();
        let token = runtime.begin_read(target(), 5).unwrap();
        assert_eq!(token.limit, 30);
        runtime.cancel_read(token);
    }

    #[test]
    fn oversized_snapshots_and_mismatched_native_ids_are_rejected() {
        let mut runtime = SessionRuntime::default();
        let token = runtime.begin_read(target(), 5).unwrap();
        let mut wrong = response();
        wrong.thread.id = Some("different".into());
        assert!(runtime.finish_read(token, 1, wrong).is_err());
        let token = runtime.begin_read(target(), 5).unwrap();
        let mut huge = response();
        huge.thread.preview = Some("x".repeat(MAX_SNAPSHOT_BYTES));
        assert!(runtime.finish_read(token, 1, huge).is_err());
        assert!(runtime.entries[&target()].response.is_none());
        assert_eq!(runtime.entries[&target()].readers, 0);
    }
    #[test]
    fn a_smaller_concurrent_read_cannot_shrink_another_devices_range() {
        let mut runtime = SessionRuntime::default();
        let small = runtime.begin_read(target(), 5).unwrap();
        let large = runtime.begin_read(target(), 30).unwrap();
        let mut history = response();
        history.thread.turns = Some(
            (0..30)
                .map(|id| {
                    std::sync::Arc::new(agent_core::models::Turn {
                        id: id.to_string(),
                        ..Default::default()
                    })
                })
                .collect(),
        );
        runtime.finish_read(large, 1, history.clone()).unwrap();
        let opened = runtime.finish_read(small, 2, response()).unwrap();
        assert_eq!(opened.response, history);
        assert_eq!(runtime.entries[&target()].limit, 30);
    }
}
