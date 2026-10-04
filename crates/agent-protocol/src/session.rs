//! Shared, pure conversation updates. Neither provider IO nor client navigation
//! belongs here: owners apply the returned value to their current conversation.
use crate::execution::ExecutionError;
use crate::models::{Item, ItemBody, SessionStatus, Thread, Turn, TurnStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Codex,
    Claude,
}

/// A native provider ID, scoped by provider. Paths and abbreviated IDs are not
/// resolved here; only the provider adapter can resolve a native session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "SessionIdentity")]
pub struct SessionRef {
    pub provider: ProviderKind,
    pub id: String,
}

#[derive(Deserialize)]
struct SessionIdentity {
    provider: ProviderKind,
    id: String,
}
impl TryFrom<SessionIdentity> for SessionRef {
    type Error = &'static str;
    fn try_from(value: SessionIdentity) -> Result<Self, Self::Error> {
        Self::new(value.provider, value.id)
    }
}

impl SessionRef {
    pub fn new(provider: ProviderKind, id: String) -> Result<Self, &'static str> {
        let session = Self { provider, id };
        session.validate()?;
        Ok(session)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_empty() || self.id.len() > 4096 || self.id.trim() != self.id {
            return Err("native session ID is required");
        }
        Ok(())
    }
}
impl std::fmt::Display for SessionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        serde_json::to_string(self)
            .expect("session serializes")
            .fmt(f)
    }
}

impl std::str::FromStr for SessionRef {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextField {
    AssistantText,
    ReasoningContent { index: u32 },
    ReasoningSummary { index: u32 },
    CommandOutput,
    FileOutput,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningField {
    Content,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("turn ID is required")]
    InvalidTurnId,
    #[error("turn is not available")]
    MissingTurn,
    #[error("item is not available")]
    MissingItem,
    #[error("text field does not match item body")]
    WrongBody,
    #[error("reasoning part index exceeds the allocation bound")]
    InvalidPartIndex,
    #[error("request is no longer pending")]
    MissingRequest,
}

/// A small change to the current conversation, never a persistent event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionChange {
    Submission {
        id: crate::ids::ClientInputId,
        delivery: SubmissionDelivery,
    },
    Request {
        request: crate::requests::Request,
    },
    RequestDelivery {
        request_id: crate::ids::RequestId,
        state: RequestDelivery,
    },
    ResolveRequest {
        request_id: crate::ids::RequestId,
    },
    Status {
        status: SessionStatus,
    },
    Turn {
        turn: Turn,
        completed: bool,
    },
    Item {
        turn_id: crate::ids::TurnId,
        item: Arc<Item>,
    },
    TurnItems {
        turn_id: crate::ids::TurnId,
        items: Vec<Arc<Item>>,
    },
    RemoveItem {
        turn_id: crate::ids::TurnId,
        item_id: crate::ids::ItemId,
    },
    Text {
        turn_id: crate::ids::TurnId,
        item_id: crate::ids::ItemId,
        field: TextField,
        delta: String,
    },
    ReasoningPart {
        turn_id: crate::ids::TurnId,
        item_id: crate::ids::ItemId,
        field: ReasoningField,
        index: u32,
    },
    Error {
        turn_id: crate::ids::TurnId,
        error: ExecutionError,
    },
}

/// Host delivery evidence for an input whose execution is still owned by the Host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SubmissionDelivery {
    Sending,
    Accepted { turn_id: Option<crate::ids::TurnId> },
    Unknown,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestDelivery {
    Awaiting,
    Sending,
    Sent,
    Unknown,
}

/// Conversation content independent of list metadata, navigation and native history IO.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub status: SessionStatus,
    pub turns: Option<Vec<Arc<Turn>>>,
    pub requests: std::collections::BTreeMap<crate::ids::RequestId, Arc<crate::requests::Request>>,
    pub submissions: std::collections::BTreeMap<crate::ids::ClientInputId, SubmissionDelivery>,
}

impl SessionChange {
    pub fn apply(&self, previous: &Thread) -> Result<Thread, UpdateError> {
        let next = previous.clone();
        let (timeline, result) = self.apply_timeline(Timeline {
            status: next.status,
            turns: next.turns,
            requests: next.requests,
            submissions: next.submissions,
        });
        result?;
        Ok(Thread {
            status: timeline.status,
            turns: timeline.turns,
            requests: timeline.requests,
            submissions: timeline.submissions,
            ..next
        })
    }

    /// Pure ownership transfer. An execution owner can append without copying its
    /// accumulated body; immutable client snapshots retain copy-on-write semantics.
    /// Invalid updates return the unchanged timeline to its owner.
    pub fn apply_timeline(&self, mut next: Timeline) -> (Timeline, Result<(), UpdateError>) {
        let result = (|| {
            match self {
                Self::Submission { id, delivery } => {
                    next.submissions.insert(id.clone(), delivery.clone());
                    return Ok(());
                }
                Self::Request { request } => {
                    next.requests
                        .insert(request.id.clone(), Arc::new(request.clone()));
                    return Ok(());
                }
                Self::RequestDelivery { request_id, state } => {
                    let request = next
                        .requests
                        .get_mut(request_id)
                        .ok_or(UpdateError::MissingRequest)?;
                    Arc::make_mut(request).delivery = *state;
                    return Ok(());
                }
                Self::ResolveRequest { request_id } => {
                    next.requests.remove(request_id);
                    return Ok(());
                }
                _ => {}
            }
            if let Self::Status { status } = self {
                next.status = *status;
                return Ok(());
            }
            let turn_id = match self {
                Self::Turn { turn, .. } => &turn.id,
                Self::Item { turn_id, .. }
                | Self::TurnItems { turn_id, .. }
                | Self::RemoveItem { turn_id, .. }
                | Self::Text { turn_id, .. }
                | Self::ReasoningPart { turn_id, .. }
                | Self::Error { turn_id, .. } => turn_id,
                Self::Status { .. }
                | Self::Submission { .. }
                | Self::Request { .. }
                | Self::RequestDelivery { .. }
                | Self::ResolveRequest { .. } => unreachable!(),
            };
            if turn_id.is_empty() {
                return Err(UpdateError::InvalidTurnId);
            }
            let index = next
                .turns
                .as_deref()
                .unwrap_or_default()
                .iter()
                .rposition(|turn| &turn.id == turn_id);
            if let Self::Turn { turn, completed } = self {
                let old = index.map(|index| &next.turns.as_ref().unwrap()[index]);
                if !completed
                    && old.is_some_and(|old| {
                        old.status != TurnStatus::Running && old.status != TurnStatus::Unknown
                    })
                {
                    return Ok(());
                }
                let mut merged = old.map_or_else(|| turn.clone(), |old| merge_fields(old, turn));
                merged.status = if *completed {
                    if turn.status == TurnStatus::Unknown {
                        TurnStatus::Completed
                    } else {
                        turn.status
                    }
                } else {
                    TurnStatus::Running
                };
                if let Some(items) = &turn.items {
                    // Turn lifecycle and item notifications are independent. Even
                    // a native "full" turn can omit an item whose output is still
                    // arriving. Only RemoveItem removes a streamed item; a new
                    // history response replaces the conversation at its owner.
                    merged.items = Some(append_items(
                        old.and_then(|old| old.items.as_ref())
                            .cloned()
                            .unwrap_or_default(),
                        items,
                    ));
                }
                if turn.error.is_none()
                    && (merged.status == TurnStatus::Completed
                        || old.is_some_and(|old| {
                            old.error.as_ref().is_some_and(|e| {
                                e.retry.as_ref().is_some_and(|retry| retry.retrying)
                            })
                        }))
                    && merged.status != TurnStatus::Running
                {
                    merged.error = None;
                }
                let turns = next.turns.get_or_insert_default();
                if let Some(index) = index {
                    turns[index] = Arc::new(merged);
                } else {
                    turns.push(Arc::new(merged));
                }
                let active = turns.iter().any(|turn| turn.status == TurnStatus::Running);
                next.status = if active {
                    SessionStatus::Running
                } else {
                    SessionStatus::Idle
                };
                return Ok(());
            }
            if index.is_none() && matches!(self, Self::TurnItems { .. }) {
                return Ok(());
            }
            let index = index.ok_or(UpdateError::MissingTurn)?;
            let previous_status = next.turns.as_ref().unwrap()[index].status;
            let turn = Arc::make_mut(&mut next.turns.as_mut().unwrap()[index]);
            match self {
                Self::TurnItems { items, .. } => {
                    turn.items = Some(items.clone());
                    turn.items_summary = false;
                }
                Self::Item { item, .. } => {
                    let items = turn.items.get_or_insert_default();
                    if let Some(index) = items.iter().position(|current| current.id == item.id) {
                        items[index] = item.clone();
                    } else {
                        items.push(item.clone());
                    }
                }
                Self::RemoveItem { item_id, .. } => {
                    if let Some(items) = &mut turn.items {
                        items.retain(|item| &item.id != item_id);
                    }
                }
                Self::Text {
                    item_id,
                    field,
                    delta,
                    ..
                } => {
                    let item = turn
                        .items
                        .as_mut()
                        .and_then(|items| items.iter_mut().find(|item| &item.id == item_id))
                        .ok_or(UpdateError::MissingItem)?;
                    let item = Arc::make_mut(item);
                    if item.is_deferred() {
                        return Ok(());
                    }
                    match (field, item.body_mut()) {
                        (TextField::AssistantText, ItemBody::AssistantText { text, .. }) => {
                            text.push_str(delta)
                        }
                        (TextField::CommandOutput, ItemBody::CommandExecution { output, .. })
                        | (TextField::FileOutput, ItemBody::FileChange { output, .. }) => {
                            output.push_str(delta)
                        }
                        (
                            TextField::ReasoningContent { index },
                            ItemBody::Reasoning { content, .. },
                        ) => append_part(content, *index, delta)?,
                        (
                            TextField::ReasoningSummary { index },
                            ItemBody::Reasoning { summary, .. },
                        ) => append_part(summary, *index, delta)?,
                        _ => return Err(UpdateError::WrongBody),
                    }
                }
                Self::ReasoningPart {
                    item_id,
                    field,
                    index,
                    ..
                } => {
                    let item = turn
                        .items
                        .as_mut()
                        .and_then(|items| items.iter_mut().find(|item| &item.id == item_id))
                        .ok_or(UpdateError::MissingItem)?;
                    let item = Arc::make_mut(item);
                    if item.is_deferred() {
                        return Ok(());
                    }
                    let ItemBody::Reasoning { content, summary } = item.body_mut() else {
                        return Err(UpdateError::WrongBody);
                    };
                    append_part(
                        match field {
                            ReasoningField::Content => content,
                            ReasoningField::Summary => summary,
                        },
                        *index,
                        "",
                    )?;
                }
                Self::Error { error, .. } => {
                    if error.retry.as_ref().is_some_and(|retry| retry.retrying)
                        && previous_status != TurnStatus::Running
                    {
                        return Ok(());
                    }
                    turn.error = Some(error.clone());
                }
                Self::Submission { .. }
                | Self::Status { .. }
                | Self::Turn { .. }
                | Self::Request { .. }
                | Self::RequestDelivery { .. }
                | Self::ResolveRequest { .. } => unreachable!(),
            }
            Ok(())
        })();
        (next, result)
    }
}

fn merge_fields(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = previous.clone();
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        started_at,
        duration_ms,
        error,
        started_at_ms,
        completed_at_ms
    );

    merged
}

pub fn append_items(mut previous: Vec<Arc<Item>>, incoming: &[Arc<Item>]) -> Vec<Arc<Item>> {
    for item in incoming {
        if let Some(index) = previous.iter().position(|current| current.id == item.id) {
            previous[index] = item.clone();
        } else {
            previous.push(item.clone());
        }
    }
    previous
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSession {
    pub session: SessionRef,
    pub limit: usize,
    #[serde(default)]
    pub include_activity: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadHistory {
    pub session: SessionRef,
    pub cursor: String,
    #[serde(default)]
    pub include_activity: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTurnItems {
    pub session: SessionRef,
    pub turn_id: crate::ids::TurnId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub turns: Vec<Arc<Turn>>,
    pub next_cursor: Option<String>,
}

impl HistoryPage {
    /// Join this older page before the thread's turns and advance its cursor.
    pub fn prepend_to(&self, thread: &mut crate::models::Thread) {
        thread.turns = Some(prepend_history(
            &self.turns,
            thread.turns.as_deref().unwrap_or_default(),
        ));
        thread.history_cursor = self.next_cursor.clone();
        thread.history_has_more = Some(thread.history_cursor.is_some());
        thread.history_read_state = Some(HistoryReadState::new(
            if thread.history_cursor.is_some() {
                HistoryReadKind::Partial
            } else {
                HistoryReadKind::Complete
            },
            Vec::new(),
        ));
    }
}

/// Native turn cursors prove adjacency; repeated IDs remain separate occurrences.
pub fn prepend_history(older: &[Arc<Turn>], current: &[Arc<Turn>]) -> Vec<Arc<Turn>> {
    older.iter().chain(current).cloned().collect()
}

/// Retain an older cache only when the refreshed window overlaps in order.
/// Repeated identities cannot prove which historical occurrence overlaps.
pub fn retained_history(previous: &[Arc<Turn>], incoming: &[Arc<Turn>]) -> Option<Vec<Arc<Turn>>> {
    let overlap = ordered_overlap(previous, incoming);
    if overlap == 0 {
        return None;
    }
    let prefix = previous.len() - overlap;
    let older = previous[..prefix].to_vec();
    let mut hydrated = incoming.to_vec();
    for (index, cached) in previous[prefix..].iter().enumerate() {
        let fresh = &mut hydrated[index];
        if (fresh.items_summary || fresh.items.is_none()) && cached.items.is_some() {
            let changed =
                fresh.items.is_some()
                    && (fresh.status == TurnStatus::Running
                        || fresh.status != cached.status
                        || fresh.duration_ms != cached.duration_ms
                        || fresh.items.iter().flatten().any(|item| {
                            !cached.items.iter().flatten().any(|old| old.id == item.id)
                        }));
            let fresh = Arc::make_mut(fresh);
            let updates: Vec<_> = fresh
                .items
                .iter()
                .flatten()
                .filter(|item| {
                    !item.is_deferred()
                        || changed
                        || !cached.items.iter().flatten().any(|old| old.id == item.id)
                })
                .cloned()
                .collect();
            fresh.items = Some(append_items(
                cached.items.clone().unwrap_or_default(),
                &updates,
            ));
            fresh.items_summary = cached.items_summary || changed;
        }
    }
    Some(prepend_history(&older, &hydrated))
}

fn ordered_overlap(previous: &[Arc<Turn>], incoming: &[Arc<Turn>]) -> usize {
    if previous
        .iter()
        .map(|turn| &turn.id)
        .collect::<std::collections::HashSet<_>>()
        .len()
        != previous.len()
        || incoming
            .iter()
            .map(|turn| &turn.id)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != incoming.len()
    {
        return 0;
    }
    (1..=previous.len().min(incoming.len()))
        .rev()
        .find(|&count| {
            previous[previous.len() - count..]
                .iter()
                .map(|turn| &turn.id)
                .eq(incoming[..count].iter().map(|turn| &turn.id))
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedSession {
    pub session: SessionRef,
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
    pub response: crate::models::ThreadResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub subscription_id: uuid::Uuid,
    pub change: SessionChange,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub additional_input: bool,
    pub fork: bool,
    pub rename: bool,
    pub model_change: bool,
}
pub fn input_unavailable_reason(thread: &Thread) -> Option<String> {
    (!thread.capabilities.unwrap_or_default().additional_input && thread.turns.iter().flatten().any(|turn| turn.status == TurnStatus::Running))
        .then(|| "このプロバイダは実行中の追加送信に対応していません。完了を待つか、停止してから送信してください。".into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryReadKind {
    Complete,
    Partial,
    Incomplete,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryReadState {
    #[serde(rename = "type")]
    pub kind: HistoryReadKind,
    #[serde(default)]
    pub issues: Vec<String>,
}
impl HistoryReadState {
    pub fn new(kind: HistoryReadKind, issues: Vec<String>) -> Self {
        Self { kind, issues }
    }
}

fn append_part(parts: &mut Vec<String>, index: u32, delta: &str) -> Result<(), UpdateError> {
    let index = index as usize;
    // Native summaries can announce indexes out of order. Bound sparse allocation.
    if index >= 4096 {
        return Err(UpdateError::InvalidPartIndex);
    }
    if index >= parts.len() {
        parts.resize_with(index + 1, String::new);
    }
    parts[index].push_str(delta);
    Ok(())
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use proptest::prelude::*;

    fn turn(id: usize, range: std::ops::Range<usize>, summary: bool) -> Arc<Turn> {
        Arc::new(Turn {
            id: format!("turn-{id}").into(),
            status: TurnStatus::Completed,
            items_summary: summary,
            items: Some(
                range
                    .map(|item| {
                        Arc::new(Item::new(
                            format!("item-{item}").into(),
                            Default::default(),
                            ItemBody::Compaction {},
                        ))
                    })
                    .collect(),
            ),
            ..Default::default()
        })
    }

    proptest! {
        #[test]
        fn cursor_pages_preserve_every_turn_and_item_occurrence(
            sizes in prop::collection::vec(1usize..25, 1..12), cut in 0usize..12,
        ) {
            let full: Vec<_> = sizes.iter().enumerate().map(|(id, size)| turn(id % 2, 0..*size, true)).collect();
            let cut = cut.min(full.len());
            prop_assert_eq!(prepend_history(&full[..cut], &full[cut..]), full);
        }

        #[test]
        fn summary_refresh_keeps_details_only_in_an_overlapping_window(
            count in 1usize..64, added in 0usize..12,
        ) {
            let cached: Vec<_> = (0..count).map(|id| turn(id, 0..8, false)).collect();
            let first = (count + added).saturating_sub(5);
            let fresh: Vec<_> = (first..count+added).map(|id| turn(id, 0..1, true)).collect();
            let retained = retained_history(&cached, &fresh);
            if added >= 5 {
                prop_assert!(retained.is_none());
            } else {
                let retained = retained.unwrap();
                prop_assert_eq!(retained.len(), count + added);
                for (id, actual) in retained.iter().enumerate() {
                    prop_assert_eq!(&actual.id, &format!("turn-{id}").into());
                    prop_assert_eq!(actual.items.as_ref().unwrap().len(), if id < count {8} else {1});
                    prop_assert_eq!(actual.items_summary, id >= count);
                }
            }
            prop_assert!(cached.iter().all(|turn| !turn.items_summary && turn.items.as_ref().unwrap().len() == 8));
        }
    }

    #[test]
    fn changed_summaries_and_repeated_boundaries_do_not_claim_complete_details() {
        let cached = turn(0, 0..8, false);
        let fresh = turn(0, 7..9, true);
        let retained = retained_history(&[cached.clone()], &[fresh]).unwrap();
        assert!(retained[0].items_summary);
        assert_eq!(retained[0].items.as_ref().unwrap().len(), 9);
        assert_eq!(
            retained_history(&[cached.clone(), cached.clone()], &[cached]),
            None
        );
    }

    #[test]
    fn refresh_keeps_unchanged_bodies_but_includes_new_deferred_items_and_reloads_running_activity()
    {
        let cached = turn(0, 0..1, false);
        let mut fresh = turn(0, 0..2, true);
        crate::models::defer_item_details(std::slice::from_mut(&mut fresh), 0);
        let retained = retained_history(&[cached.clone()], &[fresh]).unwrap();
        assert_eq!(retained[0].items.as_ref().unwrap().len(), 2);
        assert!(retained[0].items.as_ref().unwrap()[0].is_deferred());
        assert!(retained[0].items_summary);

        let mut running = cached.clone();
        Arc::make_mut(&mut running).status = TurnStatus::Running;
        let mut summary = running.clone();
        Arc::make_mut(&mut summary).items_summary = true;
        assert!(retained_history(&[running], &[summary]).unwrap()[0].items_summary);

        let mut unchanged = turn(0, 0..1, true);
        crate::models::defer_item_details(std::slice::from_mut(&mut unchanged), 0);
        let retained = retained_history(&[cached.clone()], &[unchanged]).unwrap();
        assert_eq!(retained[0].items, cached.items);
        assert!(!retained[0].items_summary);
        let mut update = turn(0, 0..1, true);
        Arc::make_mut(&mut update).items = Some(vec![Arc::new(Item::new(
            "item-0".into(),
            Default::default(),
            ItemBody::AssistantText {
                text: "fresh answer".into(),
                phase: crate::models::AssistantPhase::Final,
            },
        ))]);
        let retained = retained_history(&[cached], &[update.clone()]).unwrap();
        assert_eq!(retained[0].items, update.items);
        assert!(!retained[0].items_summary);
    }

    #[test]
    fn full_and_not_loaded_views_keep_their_distinct_cache_semantics() {
        let cached = turn(0, 0..8, false);
        let full = turn(0, 0..2, false);
        assert_eq!(
            retained_history(&[cached.clone()], &[full.clone()]).unwrap(),
            vec![full]
        );
        let mut omitted = turn(0, 0..0, false);
        Arc::make_mut(&mut omitted).items = None;
        let retained = retained_history(&[cached.clone()], &[omitted]).unwrap();
        assert_eq!(retained[0].items, cached.items);
        assert!(!retained[0].items_summary);
        let mut absent = cached.clone();
        Arc::make_mut(&mut absent).items = None;
        let summary = turn(0, 0..1, true);
        assert_eq!(
            retained_history(&[absent], &[summary.clone()]).unwrap(),
            vec![summary]
        );
        for changed in ["status", "duration"] {
            let mut summary = turn(0, 0..1, true);
            if changed == "status" {
                Arc::make_mut(&mut summary).status = TurnStatus::Failed;
            } else {
                Arc::make_mut(&mut summary).duration_ms = Some(500);
            }
            let retained = retained_history(&[cached.clone()], &[summary.clone()]).unwrap();
            assert!(retained[0].items_summary);
            assert_eq!(retained[0].status, summary.status);
            assert_eq!(retained[0].duration_ms, summary.duration_ms);
        }
    }

    #[test]
    fn hydration_replaces_only_items_and_later_deltas_continue_in_order() {
        let mut source = Thread {
            status: SessionStatus::Running,
            turns: Some(vec![turn(0, 0..1, true)]),
            ..Default::default()
        };
        let item = Arc::new(Item::new(
            "answer".into(),
            Default::default(),
            ItemBody::AssistantText {
                text: "prefix".into(),
                phase: crate::models::AssistantPhase::Final,
            },
        ));
        source = SessionChange::TurnItems {
            turn_id: "turn-0".into(),
            items: vec![item],
        }
        .apply(&source)
        .unwrap();
        source = SessionChange::Text {
            turn_id: "turn-0".into(),
            item_id: "answer".into(),
            field: TextField::AssistantText,
            delta: " + delta".into(),
        }
        .apply(&source)
        .unwrap();
        assert_eq!(source.status, SessionStatus::Running);
        source = SessionChange::Status {
            status: SessionStatus::Idle,
        }
        .apply(&source)
        .unwrap();
        assert_eq!(source.status, SessionStatus::Idle);
        let turn = &source.turns.as_ref().unwrap()[0];
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(!turn.items_summary);
        assert!(
            matches!(turn.items.as_ref().unwrap()[0].body(), ItemBody::AssistantText {text, ..} if text == "prefix + delta")
        );
        assert_eq!(
            SessionChange::TurnItems {
                turn_id: "no-longer-visible".into(),
                items: vec![]
            }
            .apply(&source)
            .unwrap(),
            source
        );
    }
}
