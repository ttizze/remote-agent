//! Timeline windows and history pages, ported from T3 `threadHistoryPaging.ts`.
//! A row is one visible item (local or inherited) with the message and plan it shows.
use super::wire::strip_transfers;
use agent_domain::{
    InputIntent, Item, ItemKind, Message, MessageAuthor, MessageId, Plan, RunAttemptId, RunId,
    RunStatus, State, ThreadId, TurnItemId,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const HISTORY_MAX_USER_TURNS: usize = 10;
pub const HISTORY_MAX_ITEMS: usize = 75;
pub const HISTORY_MAX_ENCODED_BYTES: u64 = 1_048_576;
pub const OLDER_HISTORY_USER_TURNS: usize = 20;
/// Agent-started turns ride along with user turns up to this many turn starts.
pub const HISTORY_MAX_RAW_TURNS: usize = 150;
pub const HISTORY_CURSOR_MAX_LEN: usize = 4_096;
/// Room kept for the snapshot envelope around the timeline and control state.
const SNAPSHOT_ENVELOPE_BYTES: u64 = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePolicy {
    /// Whole user turns per page. Item and byte budgets apply only to timelines
    /// without user turns, so tool activity never splits a conversation turn.
    pub max_user_turns: Option<usize>,
    pub max_items: usize,
    pub max_encoded_bytes: u64,
}
impl PagePolicy {
    pub const RECENT: Self = Self {
        max_user_turns: Some(HISTORY_MAX_USER_TURNS),
        max_items: HISTORY_MAX_ITEMS,
        max_encoded_bytes: HISTORY_MAX_ENCODED_BYTES,
    };
    pub const OLDER: Self = Self {
        max_user_turns: Some(OLDER_HISTORY_USER_TURNS),
        ..Self::RECENT
    };
    pub const fn budget(max_items: usize, max_encoded_bytes: u64) -> Self {
        Self {
            max_user_turns: None,
            max_items,
            max_encoded_bytes,
        }
    }
}

/// Opaque to clients; identifies the oldest row a client holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub v: u32,
    #[serde(rename = "seq")]
    pub snapshot_seq: u64,
    #[serde(rename = "st")]
    pub source: String,
    #[serde(rename = "si")]
    pub item: String,
    #[serde(rename = "p")]
    pub position: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid thread history cursor")]
pub struct InvalidCursor;

impl HistoryCursor {
    pub fn new(snapshot_seq: u64, source: &ThreadId, item: &TurnItemId, position: usize) -> Self {
        Self {
            v: 1,
            snapshot_seq,
            source: source.to_string(),
            item: item.to_string(),
            position,
        }
    }
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("cursor serializes"))
    }
    pub fn decode(cursor: &str) -> Result<Self, InvalidCursor> {
        if cursor.is_empty() || cursor.len() > HISTORY_CURSOR_MAX_LEN {
            return Err(InvalidCursor);
        }
        let json = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| InvalidCursor)?;
        let decoded: Self = serde_json::from_slice(&json).map_err(|_| InvalidCursor)?;
        if decoded.v != 1 || decoded.source.is_empty() || decoded.item.is_empty() {
            return Err(InvalidCursor);
        }
        Ok(decoded)
    }
}

/// One timeline row as sent in a history page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRow {
    pub position: usize,
    pub source: ThreadId,
    pub inherited: bool,
    pub item: Item,
    pub message: Option<Message>,
    pub plan: Option<Plan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub rows: Vec<HistoryRow>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Row<'a> {
    pub source: &'a ThreadId,
    pub inherited: bool,
    pub item: &'a Item,
    pub message: Option<&'a Message>,
    pub plan: Option<&'a Plan>,
}

#[derive(Serialize)]
struct RowRef<'a> {
    position: usize,
    source: &'a ThreadId,
    inherited: bool,
    item: &'a Item,
    message: Option<&'a Message>,
    plan: Option<&'a Plan>,
}

impl Row<'_> {
    pub(crate) fn owned(&self, position: usize) -> HistoryRow {
        HistoryRow {
            position,
            source: self.source.clone(),
            inherited: self.inherited,
            item: self.item.clone(),
            message: self.message.cloned(),
            plan: self.plan.cloned(),
        }
    }
    /// A user prompt that starts a turn; steering stays inside its turn.
    fn turn_start(&self) -> bool {
        matches!(self.item.kind, ItemKind::UserMessage { .. })
            && self.message.is_none_or(|m| {
                matches!(m.intent, InputIntent::TurnStart | InputIntent::QueuedTurn)
            })
    }
    fn user_turn(&self) -> bool {
        self.turn_start()
            && self
                .message
                .is_none_or(|m| m.created_by == MessageAuthor::User)
    }
}

pub(crate) fn json_len(value: &impl Serialize) -> u64 {
    serde_json::to_vec(value).map_or(0, |json| json.len() as u64)
}

/// Bytes of a row in a history page.
pub(crate) fn page_row_bytes(row: &Row<'_>, position: usize) -> u64 {
    json_len(&RowRef {
        position,
        source: row.source,
        inherited: row.inherited,
        item: row.item,
        message: row.message,
        plan: row.plan,
    })
}

/// Bytes a row adds to a snapshot: the item and its message. Plans are control state.
pub(crate) fn snapshot_row_bytes(row: &Row<'_>, _: usize) -> u64 {
    json_len(row.item) + row.message.map_or(0, json_len)
}

fn message_of(item: &Item) -> Option<&MessageId> {
    match &item.kind {
        ItemKind::UserMessage { message } | ItemKind::AssistantMessage { message } => Some(message),
        _ => None,
    }
}

/// The visible timeline in display order (ordinal, id).
pub(crate) fn timeline(state: &State) -> Vec<Row<'_>> {
    let Some(thread) = &state.thread else {
        return vec![];
    };
    let parent = thread.parent.as_ref().unwrap_or(&thread.id);
    // Inherited rows show the parent's messages, with their intent and author.
    let messages: HashMap<&MessageId, &Message> = state
        .inherited_messages
        .iter()
        .chain(&state.messages)
        .map(|m| (&m.id, m))
        .collect();
    let plans: HashMap<_, &Plan> = state.plans.iter().map(|p| (&p.id, p)).collect();
    let inherited = state.inherited_items.as_ptr_range();
    state
        .visible_items()
        .into_iter()
        .map(|item| {
            let inherited = inherited.contains(&std::ptr::from_ref(item));
            Row {
                source: if inherited { parent } else { &thread.id },
                inherited,
                item,
                message: message_of(item).and_then(|id| messages.get(id).copied()),
                plan: match &item.kind {
                    ItemKind::ProposedPlan { plan } | ItemKind::TodoList { plan } => {
                        plans.get(plan).copied()
                    }
                    _ => None,
                },
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub start: usize,
    pub end: usize,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Walks backward from the exclusive `end`, collecting whole user turns. Timelines
/// without user turns use the item and byte budgets and always admit one row, so an
/// oversized item cannot stall paging.
pub(crate) fn select_older(
    rows: &[Row<'_>],
    end: usize,
    snapshot_seq: u64,
    policy: PagePolicy,
    cost: fn(&Row<'_>, usize) -> u64,
) -> Selection {
    let end = end.min(rows.len());
    let turn_limit = policy
        .max_user_turns
        .filter(|_| rows[..end].iter().any(Row::user_turn));
    let (mut start, mut bytes, mut user_turns, mut raw_turns) = (end, 0, 0, 0);
    while start > 0 {
        let row = &rows[start - 1];
        let row_bytes = if turn_limit.is_none() {
            cost(row, start - 1)
        } else {
            0
        };
        let selected = end - start;
        let full = match turn_limit {
            None => selected >= policy.max_items || bytes + row_bytes > policy.max_encoded_bytes,
            Some(limit) => user_turns >= limit || raw_turns >= HISTORY_MAX_RAW_TURNS,
        };
        if selected > 0 && full {
            break;
        }
        start -= 1;
        bytes += row_bytes;
        user_turns += usize::from(row.user_turn());
        raw_turns += usize::from(row.turn_start());
    }
    let has_more = start > 0 && start < end;
    Selection {
        start,
        end,
        next_cursor: has_more.then(|| {
            let oldest = &rows[start];
            HistoryCursor::new(snapshot_seq, oldest.source, &oldest.item.id, start).encode()
        }),
        has_more,
    }
}

fn page(rows: &[Row<'_>], selection: Selection) -> HistoryPage {
    HistoryPage {
        rows: rows[selection.start..selection.end]
            .iter()
            .enumerate()
            .map(|(position, row)| row.owned(position))
            .collect(),
        next_cursor: selection.next_cursor,
        has_more: selection.has_more,
    }
}

/// The newest page of the timeline.
pub fn recent_history(state: &State, snapshot_seq: u64, policy: PagePolicy) -> HistoryPage {
    let rows = timeline(state);
    let selection = select_older(&rows, rows.len(), snapshot_seq, policy, page_row_bytes);
    page(&rows, selection)
}

/// The page before `cursor`. A cursor whose item is gone resumes from its recorded
/// position, clamped to the current timeline.
pub fn history_before(
    state: &State,
    cursor: &str,
    snapshot_seq: u64,
    policy: Option<PagePolicy>,
) -> Result<HistoryPage, InvalidCursor> {
    let cursor = HistoryCursor::decode(cursor)?;
    let rows = timeline(state);
    let anchor = rows
        .iter()
        .position(|row| row.source.as_str() == cursor.source && row.item.id.as_str() == cursor.item)
        .unwrap_or(cursor.position.min(rows.len()));
    let selection = select_older(
        &rows,
        anchor,
        snapshot_seq,
        policy.unwrap_or(PagePolicy::OLDER),
        page_row_bytes,
    );
    Ok(page(&rows, selection))
}

/// A snapshot whose timeline is a recent window. Control state (thread, runs,
/// attempts, requests, tasks, checkpoints, transfers, plans) stays complete so later
/// facts fold onto it; older rows come from history pages.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedState {
    pub state: State,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    /// Highest local item ordinal of the full projection.
    pub latest_local_ordinal: Option<u64>,
    pub payload_budget_exceeded: bool,
}

fn retained_runs(state: &State) -> HashSet<&RunId> {
    let latest = state.runs.iter().max_by_key(|run| run.ordinal);
    state
        .runs
        .iter()
        .filter(|run| {
            latest.is_some_and(|latest| latest.id == run.id)
                || run.status.blocking()
                || run.status == RunStatus::Queued
        })
        .map(|run| &run.id)
        .collect()
}

/// Bounds the projection like T3's `buildBoundedThreadProjection`. Beyond the recent
/// window it keeps every item a later fact can still touch (non-terminal items and
/// the items of live attempts), every interrupt request, and the messages of those
/// items and of the latest, active and queued runs.
pub fn bounded_state(state: &State, snapshot_seq: u64, policy: PagePolicy) -> BoundedState {
    let latest_local_ordinal = state.items.iter().map(|item| item.ordinal).max();
    let live: HashSet<&RunAttemptId> = state
        .runs
        .iter()
        .filter(|run| run.status.blocking())
        .filter_map(|run| run.attempt.as_ref())
        .chain(state.native_owner.as_ref())
        .collect();
    let dependency = |item: &Item| {
        matches!(item.kind, ItemKind::RunInterruptRequest)
            || !item.status.terminal()
            || item.attempt.as_ref().is_some_and(|a| live.contains(a))
    };
    let messages: HashMap<&MessageId, &Message> =
        state.messages.iter().map(|m| (&m.id, m)).collect();
    let reserve: u64 = state
        .items
        .iter()
        .filter(|item| dependency(item))
        .map(|item| {
            json_len(item)
                + message_of(item)
                    .and_then(|id| messages.get(id))
                    .map_or(0, json_len)
        })
        .sum();

    let mut bounded = state.clone();
    strip_transfers(&mut bounded);
    let active_runs: HashSet<&RunId> = state
        .runs
        .iter()
        .filter(|run| !run.status.terminal())
        .map(|run| &run.id)
        .collect();
    for plan in &mut bounded.plans {
        if !active_runs.contains(&plan.run) {
            // The plan item keeps the text.
            plan.markdown.clear();
        }
    }
    let items = std::mem::take(&mut bounded.items);
    let all_messages = std::mem::take(&mut bounded.messages);
    let inherited = std::mem::take(&mut bounded.inherited_items);
    let inherited_messages = std::mem::take(&mut bounded.inherited_messages);
    let control = json_len(&bounded);

    let rows = timeline(state);
    let window_policy = PagePolicy {
        max_encoded_bytes: policy
            .max_encoded_bytes
            .saturating_sub(reserve + control + SNAPSHOT_ENVELOPE_BYTES),
        ..policy
    };
    let window = select_older(
        &rows,
        rows.len(),
        snapshot_seq,
        window_policy,
        snapshot_row_bytes,
    );
    let windowed = &rows[window.start..window.end];
    let local: HashSet<&TurnItemId> = windowed
        .iter()
        .filter(|row| !row.inherited)
        .map(|row| &row.item.id)
        .collect();
    let inherited_ids: HashSet<&TurnItemId> = windowed
        .iter()
        .filter(|row| row.inherited)
        .map(|row| &row.item.id)
        .collect();
    bounded.items = items
        .into_iter()
        .filter(|item| local.contains(&item.id) || dependency(item))
        .collect();
    bounded.inherited_items = inherited
        .into_iter()
        .filter(|item| inherited_ids.contains(&item.id))
        .collect();
    let inherited_shown: HashSet<&MessageId> = bounded
        .inherited_items
        .iter()
        .filter_map(message_of)
        .collect();
    bounded.inherited_messages = inherited_messages
        .into_iter()
        .filter(|m| inherited_shown.contains(&m.id))
        .collect();

    let shown: HashSet<&MessageId> = bounded.items.iter().filter_map(message_of).collect();
    let runs = retained_runs(state);
    let run_messages: HashSet<&MessageId> = state
        .runs
        .iter()
        .filter(|run| runs.contains(&run.id))
        .map(|run| &run.message)
        .collect();
    bounded.messages = all_messages
        .into_iter()
        .filter(|m| {
            shown.contains(&m.id)
                || run_messages.contains(&m.id)
                || m.run.as_ref().is_some_and(|run| runs.contains(run))
                || m.streaming
        })
        .collect();

    let payload_budget_exceeded = json_len(&bounded) > policy.max_encoded_bytes;
    BoundedState {
        state: bounded,
        history_cursor: window.next_cursor,
        has_more_history: window.has_more,
        latest_local_ordinal,
        payload_budget_exceeded,
    }
}

#[cfg(test)]
pub(crate) mod tests;
