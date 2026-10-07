//! One thread's folded state, resume cursor, sync status and history window.
use super::history::{HistoryMeta, merge_history_page};
use agent_domain::{CommandId, Fact, FactBody, Item, State, ThreadId, TurnItemId, apply};
use agent_protocol::conversation::{
    ErrorCode, HistoryPage, HistoryRow, SequencedFact, SubscribeThread, ThreadSnapshot,
    ThreadUpdate,
};
use agent_protocol::error::RpcFailure;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

pub const THREAD_SYNC_ERROR: &str = "Could not synchronize the thread.";
pub const HISTORY_ERROR: &str = "Could not load earlier activity.";
pub const NOT_CONNECTED: &str = "Environment is not connected.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadStatus {
    #[default]
    Empty,
    Cached,
    Synchronizing,
    Live,
    Deleted,
}

/// The withheld output of one item, read with `getTurnItem`.
#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Loading,
    Loaded(Box<Item>),
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct ThreadSync {
    pub state: Option<Arc<State>>,
    /// The last applied global sequence; resumes use it as `after_sequence`.
    pub cursor: u64,
    pub status: ThreadStatus,
    pub error: Option<String>,
    pub history: HistoryMeta,
    pub details: Arc<BTreeMap<TurnItemId, Detail>>,
    /// Advances on every change a view could show.
    pub revision: u64,
    /// Advances when the folded state changes without the cursor moving: an
    /// installed snapshot, a merged history page, a deletion.
    pub history_revision: u64,
    /// Advances on every change of `details`.
    pub detail_revision: u64,
    awaiting_completion: bool,
    resuming_live: bool,
    needs_snapshot: bool,
    fold_failures: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resync {
    /// Keep the stream.
    None,
    /// The state could not fold a fact: subscribe again for a snapshot.
    Snapshot,
    /// A repeated fold failure: stop until the next session or foreground.
    Stop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RollbackResult {
    pub command: CommandId,
    pub succeeded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub changed: bool,
    pub deleted: bool,
    pub resync: Resync,
    pub rollbacks: Vec<RollbackResult>,
}
impl Default for Applied {
    fn default() -> Self {
        Self {
            changed: false,
            deleted: false,
            resync: Resync::None,
            rollbacks: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    /// A cold subscription found no thread: it is gone and is not retried.
    Deleted,
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadEarlier {
    Noop,
    Busy,
    Request(String),
}

/// The disk cache form of a settled thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedThread {
    pub snapshot_sequence: u64,
    pub state: Arc<State>,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_ordinal: Option<u64>,
}

fn item_target(body: &FactBody) -> Option<(&TurnItemId, Option<u64>)> {
    use FactBody::*;
    match body {
        ItemStarted { id, ordinal, .. } => Some((id, Some(*ordinal))),
        ItemProjected { item } => Some((&item.id, Some(item.ordinal))),
        ItemTextAppended { id, .. }
        | ItemTextReplaced { id, .. }
        | ItemDetailChanged { id, .. }
        | ItemCompleted { id, .. }
        | ItemReopened { id }
        | ItemMoved { id, .. } => Some((id, None)),
        _ => None,
    }
}

fn oldest_local_ordinal(state: &State) -> Option<u64> {
    let rolled_back: Vec<_> = state
        .runs
        .iter()
        .filter(|run| run.status == agent_domain::RunStatus::RolledBack)
        .map(|run| &run.id)
        .collect();
    state
        .items
        .iter()
        .filter(|item| {
            item.run
                .as_ref()
                .is_none_or(|run| !rolled_back.contains(&run))
        })
        .map(|item| item.ordinal)
        .min()
}

/// An item outside a partial window: at or below the newest local ordinal the
/// window was cut from, or older than the oldest row it holds.
fn outside_window(state: &State, ordinal: u64, latest: Option<u64>) -> bool {
    latest.is_some_and(|latest| ordinal <= latest)
        || oldest_local_ordinal(state).is_some_and(|oldest| ordinal < oldest)
}

impl ThreadSync {
    pub fn from_cache(cached: CachedThread) -> Self {
        Self {
            state: Some(cached.state),
            cursor: cached.snapshot_sequence,
            status: ThreadStatus::Cached,
            history: HistoryMeta {
                cursor: cached.history_cursor,
                has_more: cached.has_more_history,
                latest_local_ordinal: cached.latest_local_ordinal,
                ..HistoryMeta::default()
            },
            ..Self::default()
        }
    }

    /// A retained cache opened again. A live thread stays live: its resume only
    /// replays what it missed.
    pub fn resumed(&self) -> Self {
        let status = match self.status {
            ThreadStatus::Deleted => ThreadStatus::Deleted,
            ThreadStatus::Live if self.state.is_some() => ThreadStatus::Live,
            _ => self.status_without_live_data(),
        };
        Self {
            status,
            error: None,
            history: HistoryMeta {
                loading: false,
                error: None,
                ..self.history.clone()
            },
            awaiting_completion: false,
            resuming_live: false,
            ..self.clone()
        }
    }

    pub fn has_data(&self) -> bool {
        self.state.is_some()
    }

    fn status_without_live_data(&self) -> ThreadStatus {
        if self.state.is_some() {
            ThreadStatus::Cached
        } else {
            ThreadStatus::Empty
        }
    }

    fn touch(&mut self) {
        self.revision += 1;
    }

    /// The thread gained a live consumer; the first subscription of a retained
    /// live thread keeps its status.
    pub fn open(&mut self) {
        self.resuming_live = self.status == ThreadStatus::Live;
        self.mark_synchronizing();
    }

    fn mark_synchronizing(&mut self) {
        if self.resuming_live || self.status == ThreadStatus::Deleted {
            return;
        }
        if self.status != ThreadStatus::Synchronizing || self.error.is_some() {
            self.status = ThreadStatus::Synchronizing;
            self.error = None;
            self.touch();
        }
    }

    /// The next subscription request, or `None` for a thread known to be gone.
    pub fn subscribe(&mut self, thread: &ThreadId) -> Option<SubscribeThread> {
        if self.status == ThreadStatus::Deleted {
            return None;
        }
        self.awaiting_completion = true;
        self.mark_synchronizing();
        self.resuming_live = false;
        Some(SubscribeThread {
            thread_id: thread.clone(),
            after_sequence: (self.state.is_some() && !self.needs_snapshot).then_some(self.cursor),
            request_completion_marker: true,
            accept_bounded_snapshot: true,
        })
    }

    pub fn connecting(&mut self) {
        if self.status != ThreadStatus::Deleted && self.error.is_none() {
            self.mark_synchronizing_from_connection();
        }
    }

    pub fn ready(&mut self) {
        if !matches!(self.status, ThreadStatus::Live | ThreadStatus::Deleted)
            && self.error.is_none()
        {
            self.mark_synchronizing_from_connection();
        }
    }

    fn mark_synchronizing_from_connection(&mut self) {
        if self.status != ThreadStatus::Synchronizing {
            self.status = ThreadStatus::Synchronizing;
            self.touch();
        }
    }

    pub fn disconnected(&mut self) {
        self.awaiting_completion = false;
        if self.status != ThreadStatus::Deleted {
            self.status = self.status_without_live_data();
        }
        self.touch();
    }

    pub fn stream_error(&mut self, message: &str) {
        self.awaiting_completion = false;
        if self.status != ThreadStatus::Deleted {
            self.status = self.status_without_live_data();
        }
        self.error = Some(if message.trim().is_empty() {
            THREAD_SYNC_ERROR.into()
        } else {
            message.into()
        });
        self.touch();
    }

    /// A `Failed` item or a refused subscription.
    pub fn failed(&mut self, failure: &RpcFailure) -> FailureAction {
        if self.state.is_none() && ErrorCode::of(failure) == Some(ErrorCode::ThreadNotFound) {
            self.set_deleted();
            return FailureAction::Deleted;
        }
        self.stream_error(&failure.message);
        FailureAction::Retry
    }

    pub fn set_deleted(&mut self) {
        self.awaiting_completion = false;
        self.state = None;
        self.status = ThreadStatus::Deleted;
        self.error = None;
        self.history = HistoryMeta::default();
        self.details = Arc::default();
        self.history_revision += 1;
        self.detail_revision += 1;
        self.touch();
    }

    pub fn apply(&mut self, updates: Vec<ThreadUpdate>) -> Applied {
        let mut out = Applied::default();
        let mut facts = vec![];
        for update in updates {
            if let ThreadUpdate::Facts(batch) = update {
                facts.extend(batch);
                continue;
            }
            self.apply_facts(std::mem::take(&mut facts), &mut out);
            if out.deleted || out.resync != Resync::None {
                return out;
            }
            match update {
                ThreadUpdate::Snapshot(snapshot) => {
                    self.install(snapshot);
                    out.changed = true;
                }
                ThreadUpdate::Synchronized => {
                    self.awaiting_completion = false;
                    if self.state.is_some()
                        && self.status != ThreadStatus::Deleted
                        && self.error.is_none()
                        && self.status != ThreadStatus::Live
                    {
                        self.status = ThreadStatus::Live;
                        self.touch();
                        out.changed = true;
                    }
                }
                ThreadUpdate::Failed(failure) => {
                    out.deleted = self.failed(&failure) == FailureAction::Deleted;
                    out.changed = true;
                    return out;
                }
                ThreadUpdate::Facts(_) => unreachable!("facts are batched above"),
            }
        }
        self.apply_facts(facts, &mut out);
        out
    }

    fn install(&mut self, snapshot: ThreadSnapshot) {
        self.cursor = snapshot.snapshot_sequence;
        let history = snapshot
            .window
            .as_ref()
            .map(HistoryMeta::from_window)
            .unwrap_or_default();
        if !self.details.is_empty() {
            let previous = self.state.take();
            let kept = self
                .details
                .iter()
                .filter(|(id, _)| {
                    let find = |state: &State| state.items.iter().find(|i| &i.id == *id).cloned();
                    let before = previous.as_deref().and_then(find);
                    before.is_some() && before == find(&snapshot.state)
                })
                .map(|(id, detail)| (id.clone(), detail.clone()))
                .collect();
            self.details = Arc::new(kept);
            self.detail_revision += 1;
        }
        self.state = Some(snapshot.state);
        self.history_revision += 1;
        self.status = if self.error.is_some() {
            ThreadStatus::Cached
        } else if self.awaiting_completion {
            ThreadStatus::Synchronizing
        } else {
            ThreadStatus::Live
        };
        self.history = history;
        self.needs_snapshot = false;
        self.touch();
    }

    fn apply_facts(&mut self, batch: Vec<SequencedFact>, out: &mut Applied) {
        let applied = self.cursor;
        let mut sequence = applied;
        let mut fresh: Vec<Fact> = vec![];
        for item in batch {
            if item.sequence <= sequence {
                continue;
            }
            sequence = item.sequence;
            fresh.push(item.fact);
        }
        if sequence == applied {
            return;
        }
        self.cursor = sequence;
        if fresh.is_empty() || self.status == ThreadStatus::Deleted {
            return;
        }
        if self.state.is_none() {
            if fresh
                .iter()
                .any(|fact| matches!(fact.body, FactBody::ThreadDeleted))
            {
                self.set_deleted();
                out.deleted = true;
                out.changed = true;
            }
            return;
        }
        let partial = self.history.partial();
        let mut latest = self.history.latest_local_ordinal;
        let mut changed = false;
        for fact in fresh {
            if matches!(fact.body, FactBody::ThreadDeleted) {
                self.set_deleted();
                out.deleted = true;
                out.changed = true;
                return;
            }
            let state = self.state.as_ref().expect("thread data");
            if let Some((id, ordinal)) = item_target(&fact.body) {
                let present = state.items.iter().any(|item| &item.id == id);
                if partial
                    && !present
                    && ordinal.is_none_or(|ordinal| outside_window(state, ordinal, latest))
                {
                    continue;
                }
                if self.details.contains_key(id) {
                    Arc::make_mut(&mut self.details).remove(id);
                    self.detail_revision += 1;
                }
            }
            let state = Arc::make_mut(self.state.as_mut().expect("thread data"));
            if apply(state, &fact).is_err() {
                self.needs_snapshot = true;
                self.fold_failures = self.fold_failures.saturating_add(1);
                out.resync = if self.fold_failures > 1 {
                    self.stream_error(THREAD_SYNC_ERROR);
                    Resync::Stop
                } else {
                    Resync::Snapshot
                };
                out.changed = true;
                return;
            }
            changed = true;
            match &fact.body {
                FactBody::ItemStarted { ordinal, .. } if partial => {
                    latest = Some(latest.map_or(*ordinal, |latest| latest.max(*ordinal)));
                }
                FactBody::ItemProjected { item } if partial => {
                    latest = Some(latest.map_or(item.ordinal, |latest| latest.max(item.ordinal)));
                }
                FactBody::RolledBack { command, .. } => out.rollbacks.push(RollbackResult {
                    command: command.clone(),
                    succeeded: true,
                }),
                FactBody::RollbackFailed { command, .. } => out.rollbacks.push(RollbackResult {
                    command: command.clone(),
                    succeeded: false,
                }),
                _ => {}
            }
        }
        if changed {
            self.fold_failures = 0;
            self.status = if self.awaiting_completion {
                ThreadStatus::Synchronizing
            } else {
                ThreadStatus::Live
            };
            self.error = None;
            self.history.latest_local_ordinal = latest;
            self.touch();
            out.changed = true;
        }
    }

    pub fn begin_load_earlier(&mut self) -> LoadEarlier {
        if self.status == ThreadStatus::Deleted || self.state.is_none() || !self.history.has_more {
            return LoadEarlier::Noop;
        }
        let Some(cursor) = self.history.cursor.clone() else {
            return LoadEarlier::Noop;
        };
        if self.history.loading {
            return LoadEarlier::Busy;
        }
        self.history.loading = true;
        self.history.error = None;
        self.touch();
        LoadEarlier::Request(cursor)
    }

    /// Merges a page requested at `request`. A page whose cursor was replaced
    /// meanwhile is dropped.
    pub fn history_loaded(&mut self, request: &str, page: HistoryPage) -> bool {
        if !self.history.is_active_request(request) {
            return false;
        }
        let Some(state) = self
            .state
            .as_ref()
            .filter(|_| self.status != ThreadStatus::Deleted)
        else {
            self.history = HistoryMeta::default();
            self.touch();
            return false;
        };
        if let Some(merged) = merge_history_page(state, &page.rows) {
            self.state = Some(Arc::new(merged));
            self.history_revision += 1;
        }
        self.history = self.history.after_page(&page);
        if self.awaiting_completion {
            self.status = ThreadStatus::Synchronizing;
        }
        self.touch();
        true
    }

    pub fn history_failed(&mut self, request: &str, message: &str) -> bool {
        if !self.history.is_active_request(request) {
            return false;
        }
        self.history.loading = false;
        self.history.error = Some(if message.trim().is_empty() {
            HISTORY_ERROR.into()
        } else {
            message.into()
        });
        self.touch();
        true
    }

    pub fn history_abandoned(&mut self, request: &str) {
        let history = self.history.clear_loading(request);
        if history != self.history {
            self.history = history;
            self.touch();
        }
    }

    /// Starts reading an item's withheld output unless it is loading or loaded.
    pub fn begin_detail(&mut self, item: &TurnItemId) -> bool {
        if matches!(
            self.details.get(item),
            Some(Detail::Loading | Detail::Loaded(_))
        ) {
            return false;
        }
        Arc::make_mut(&mut self.details).insert(item.clone(), Detail::Loading);
        self.detail_revision += 1;
        self.touch();
        true
    }

    /// Stores a detail only while its request is current; a fact that touched
    /// the item meanwhile invalidated it.
    pub fn detail_loaded(&mut self, item: &TurnItemId, row: Option<HistoryRow>) {
        if self.details.get(item) != Some(&Detail::Loading) {
            return;
        }
        let details = Arc::make_mut(&mut self.details);
        match row {
            Some(row) => {
                details.insert(item.clone(), Detail::Loaded(Box::new(row.item)));
            }
            None => {
                details.remove(item);
            }
        }
        self.detail_revision += 1;
        self.touch();
    }

    pub fn detail_failed(&mut self, item: &TurnItemId, message: String) {
        if self.details.get(item) == Some(&Detail::Loading) {
            Arc::make_mut(&mut self.details).insert(item.clone(), Detail::Failed(message));
            self.detail_revision += 1;
            self.touch();
        }
    }
}

#[cfg(test)]
mod tests;
