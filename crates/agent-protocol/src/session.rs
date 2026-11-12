//! Shared, pure conversation updates. Neither provider IO nor client navigation
//! belongs here: owners apply the returned value to their current conversation.
use crate::execution::ExecutionError;
use crate::models::{Item, ItemBody, SessionStatus, Thread, Turn, TurnStatus};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Codex,
    Claude,
    #[serde(rename = "deepseek")]
    DeepSeek,
}

impl ProviderKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::DeepSeek => "deepseek",
        }
    }
}

/// A conversation address, scoped by its originating provider. The Host may
/// bind an admitted task to a native provider session after issuing its address.
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
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionChange {
    Workspace {
        cwd: String,
    },
    Preparation {
        preparation: Option<SessionPreparation>,
    },
    WorkspaceSetup {
        setup: Option<crate::workspace::WorkspaceSetup>,
    },
    Submission {
        id: crate::ids::ClientInputId,
        delivery: SubmissionDelivery,
    },
    /// The whole queue of input waiting for the running turn, in order.
    Queue {
        queue: crate::operations::InputQueue,
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
    /// The running turn answered and waits on this work; empty otherwise.
    Background {
        tasks: Vec<crate::execution::BackgroundTask>,
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
    #[serde(other)]
    Unsupported,
}

/// Host delivery evidence for an input whose execution is still owned by the Host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SubmissionDelivery {
    Sending,
    /// Queued input has no turn until the Host starts it.
    Accepted {
        turn_id: Option<crate::ids::TurnId>,
    },
    Unknown,
    Rejected,
    /// Removed from the queue before it ran; nothing will run it.
    Withdrawn,
}

/// Durable Host admission, independent of the provider acknowledging execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPreparation {
    pub submission: crate::operations::Submission,
    pub status: PreparationStatus,
    pub stage: PreparationStage,
}

/// Retained through failure and interruption so retry reuses a prepared workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreparationStage {
    Workspace,
    Worktree,
    SetupScript,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PreparationStatus {
    Preparing,
    Sending,
    Failed { error: ExecutionError },
    Interrupted,
    Unknown,
}

impl PreparationStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Preparing | Self::Sending)
    }
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
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub turns: Option<Vec<Arc<Turn>>>,
    pub requests: std::collections::BTreeMap<crate::ids::RequestId, Arc<crate::requests::Request>>,
    pub submissions: std::collections::BTreeMap<crate::ids::ClientInputId, SubmissionDelivery>,
    /// Input waiting for the running turn, in the order it will start.
    pub queue: crate::operations::InputQueue,
    /// None means no observation; Some(None) explicitly clears preparation.
    pub preparation: Option<Option<SessionPreparation>>,
    pub workspace_setup: Option<Option<crate::workspace::WorkspaceSetup>>,
    pub background: Vec<crate::execution::BackgroundTask>,
}

/// Locate an item within the newest occurrence that actually contains it.
pub fn item_position(
    turns: &[Arc<Turn>],
    turn_id: &crate::ids::TurnId,
    item_id: &crate::ids::ItemId,
) -> Option<(usize, usize)> {
    turns
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, turn)| &turn.id == turn_id)
        .find_map(|(turn, value)| {
            Some((
                turn,
                value
                    .items
                    .as_ref()?
                    .iter()
                    .position(|item| &item.id == item_id)?,
            ))
        })
}

impl SessionChange {
    pub fn item_id(&self) -> Option<&crate::ids::ItemId> {
        match self {
            Self::Item { item, .. } => Some(&item.id),
            Self::RemoveItem { item_id, .. }
            | Self::Text { item_id, .. }
            | Self::ReasoningPart { item_id, .. } => Some(item_id),
            _ => None,
        }
    }

    /// Item identities disambiguate repeated turns. Disjoint lifecycle items
    /// complete the active occurrence, or append after a finished occurrence.
    pub fn turn_index(&self, turns: &[Arc<Turn>]) -> Option<usize> {
        let id = self.turn_id()?;
        if let Self::Turn { turn, .. } = self {
            let exact = turns
                .iter()
                .rposition(|old| &old.id == id && old.shares_items(turn));
            return exact.or_else(|| {
                turns.iter().rposition(|old| {
                    &old.id == id
                        && (matches!(old.status, TurnStatus::Running | TurnStatus::Unknown)
                            || old.matches_occurrence(turn))
                })
            });
        }
        self.item_id()
            .and_then(|item| item_position(turns, id, item).map(|(turn, _)| turn))
            .or_else(|| turns.iter().rposition(|turn| &turn.id == id))
    }

    /// The turn a turn-scoped change updates.
    pub fn turn_id(&self) -> Option<&crate::ids::TurnId> {
        match self {
            Self::Turn { turn, .. } => Some(&turn.id),
            Self::Item { turn_id, .. }
            | Self::TurnItems { turn_id, .. }
            | Self::RemoveItem { turn_id, .. }
            | Self::Text { turn_id, .. }
            | Self::ReasoningPart { turn_id, .. }
            | Self::Error { turn_id, .. } => Some(turn_id),
            Self::Status { .. }
            | Self::Background { .. }
            | Self::Workspace { .. }
            | Self::Preparation { .. }
            | Self::WorkspaceSetup { .. }
            | Self::Submission { .. }
            | Self::Queue { .. }
            | Self::Request { .. }
            | Self::RequestDelivery { .. }
            | Self::ResolveRequest { .. }
            | Self::Unsupported => None,
        }
    }
    pub fn apply(&self, previous: &Thread) -> Result<Thread, UpdateError> {
        let next = previous.clone();
        let (timeline, result) = self.apply_timeline(Timeline {
            cwd: None,
            status: next.status,
            turns: next.turns,
            requests: next.requests,
            submissions: next.submissions,
            queue: next.queue,
            preparation: Some(next.preparation),
            workspace_setup: Some(next.workspace_setup),
            background: next.background,
        });
        result?;
        Ok(Thread {
            cwd: timeline.cwd.or(next.cwd),
            status: timeline.status,
            turns: timeline.turns,
            requests: timeline.requests,
            submissions: timeline.submissions,
            queue: timeline.queue,
            preparation: timeline.preparation.flatten(),
            workspace_setup: timeline.workspace_setup.flatten(),
            background: timeline.background,
            ..next
        })
    }

    /// Pure ownership transfer. An execution owner can append without copying its
    /// accumulated body; immutable client snapshots retain copy-on-write semantics.
    /// Invalid updates return the unchanged timeline to its owner.
    pub fn apply_timeline(&self, mut next: Timeline) -> (Timeline, Result<(), UpdateError>) {
        let result = (|| {
            match self {
                Self::Unsupported => return Ok(()),
                Self::Workspace { cwd } => {
                    next.cwd = Some(cwd.clone());
                    return Ok(());
                }
                Self::Preparation { preparation } => {
                    next.preparation = Some(preparation.clone());
                    return Ok(());
                }
                Self::WorkspaceSetup { setup } => {
                    next.workspace_setup = Some(setup.clone());
                    return Ok(());
                }
                Self::Submission { id, delivery } => {
                    next.submissions.insert(id.clone(), delivery.clone());
                    return Ok(());
                }
                Self::Queue { queue } => {
                    next.queue = queue.clone();
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
                Self::Background { tasks } => {
                    next.background = tasks.clone();
                    return Ok(());
                }
                _ => {}
            }
            if let Self::Status { status } = self {
                next.status = *status;
                return Ok(());
            }
            let turn_id = self.turn_id().expect("other changes returned above");
            if turn_id.is_empty() {
                return Err(UpdateError::InvalidTurnId);
            }
            let index = self.turn_index(next.turns.as_deref().unwrap_or_default());
            if let Self::Turn { turn, completed } = self {
                let old = index.map(|index| &next.turns.as_ref().unwrap()[index]);
                let Some(merged) = merge_turn(old.map(AsRef::as_ref), turn, *completed) else {
                    return Ok(());
                };
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
                    let mut error = error.clone();
                    // A retry the provider does not number is the one after
                    // the turn's previous retry.
                    if let Some(retry) = error.retry.as_mut().filter(|retry| retry.retrying) {
                        let previous = turn
                            .error
                            .as_ref()
                            .and_then(|error| error.retry.clone())
                            .filter(|retry| retry.retrying)
                            .unwrap_or_default();
                        *retry = previous.followed_by(retry);
                    }
                    turn.error = Some(error);
                }
                Self::Workspace { .. }
                | Self::Preparation { .. }
                | Self::WorkspaceSetup { .. }
                | Self::Submission { .. }
                | Self::Queue { .. }
                | Self::Status { .. }
                | Self::Background { .. }
                | Self::Turn { .. }
                | Self::Request { .. }
                | Self::RequestDelivery { .. }
                | Self::ResolveRequest { .. }
                | Self::Unsupported => unreachable!(),
            }
            Ok(())
        })();
        (next, result)
    }
}

/// Returns `None` when a running notification would reopen a terminal turn.
fn merge_turn(previous: Option<&Turn>, incoming: &Turn, completed: bool) -> Option<Turn> {
    if !completed
        && previous.is_some_and(|old| {
            old.status != TurnStatus::Running && old.status != TurnStatus::Unknown
        })
    {
        return None;
    }
    let mut merged = previous.unwrap_or(incoming).clone();
    merged.queued |= incoming.queued;
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        provider,
        supports_fork,
        started_at,
        duration_ms,
        error,
        started_at_ms,
        completed_at_ms
    );

    merged.status = match (completed, incoming.status) {
        (false, _) => TurnStatus::Running,
        (true, TurnStatus::Unknown) => TurnStatus::Completed,
        (true, status) => status,
    };
    if let Some(items) = &incoming.items {
        // Turn lifecycle and item notifications are independent. Even
        // a native "full" turn can omit an item whose output is still
        // arriving. Only RemoveItem removes a streamed item; a new
        // history response replaces the conversation at its owner.
        let previous_items = if previous.is_some() {
            merged.items.take().unwrap_or_default()
        } else {
            Vec::new()
        };
        merged.items = Some(append_items(previous_items, items));
    }
    if incoming.error.is_none()
        && (merged.status == TurnStatus::Completed
            || previous.is_some_and(|old| {
                old.error
                    .as_ref()
                    .is_some_and(|e| e.retry.as_ref().is_some_and(|retry| retry.retrying))
            }))
        && merged.status != TurnStatus::Running
    {
        merged.error = None;
    }
    Some(merged)
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use crate::execution::{ExecutionError, RetryEvidence};

    #[test]
    fn an_answered_turn_waits_on_its_roster_until_the_owner_clears_it() {
        let roster = vec![crate::execution::BackgroundTask {
            id: "shell".into(),
            kind: crate::execution::BackgroundTaskKind::Command,
            description: Some("dev server".into()),
        }];
        let turn = |completed| SessionChange::Turn {
            turn: Turn {
                id: "turn".into(),
                status: if completed {
                    TurnStatus::Completed
                } else {
                    TurnStatus::Running
                },
                ..Default::default()
            },
            completed,
        };
        let running = turn(false).apply(&Thread::default()).unwrap();
        let waiting = turn(true)
            .apply(
                &SessionChange::Background {
                    tasks: roster.clone(),
                }
                .apply(&running)
                .unwrap(),
            )
            .unwrap();
        assert_eq!(waiting.background, roster, "the answered turn waits on it");
        assert_eq!(waiting.status, SessionStatus::Idle);
        assert_eq!(
            waiting.active_turn_id(),
            Some("turn".into()),
            "Stop ends the work the answered turn left running"
        );
        let roundtrip: Thread =
            serde_json::from_value(serde_json::to_value(&waiting).unwrap()).unwrap();
        assert_eq!(roundtrip.background, roster);
        let cleared = SessionChange::Background { tasks: Vec::new() }
            .apply(&waiting)
            .unwrap();
        assert!(cleared.background.is_empty());
        assert_eq!(cleared.active_turn_id(), None);
    }

    #[test]
    fn retries_keep_the_providers_attempt_or_count_from_the_turns_last_retry() {
        let running = SessionChange::Turn {
            turn: Turn {
                id: "turn".into(),
                status: TurnStatus::Running,
                ..Default::default()
            },
            completed: false,
        }
        .apply(&Thread::default())
        .unwrap();
        let retry = |thread: &Thread, attempt, max| {
            let next = SessionChange::Error {
                turn_id: "turn".into(),
                error: ExecutionError {
                    retry: Some(RetryEvidence::new(attempt, max)),
                    ..Default::default()
                },
            }
            .apply(thread)
            .unwrap();
            let retry = next.turns.as_ref().unwrap()[0]
                .error
                .as_ref()
                .unwrap()
                .retry
                .clone()
                .unwrap();
            (next, (retry.attempt, retry.max_attempts))
        };
        let (first, numbered) = retry(&running, None, None);
        assert_eq!(numbered, (Some(1), None), "the first retry is attempt 1");
        let (second, numbered) = retry(&first, None, None);
        assert_eq!(
            numbered,
            (Some(2), None),
            "an unnumbered retry follows the last"
        );
        let (reported, numbered) = retry(&second, Some(4), Some(5));
        assert_eq!(numbered, (Some(4), Some(5)), "the provider's numbers win");
        assert_eq!(retry(&reported, None, None).1, (Some(5), Some(5)));
    }

    #[test]
    fn item_positions_require_both_ids_and_prefer_the_latest_matching_occurrence() {
        let turn = |id: &str, items: &[&str]| {
            Arc::new(Turn {
                id: id.into(),
                items: Some(
                    items
                        .iter()
                        .map(|id| {
                            Arc::new(Item::new(
                                (*id).into(),
                                Default::default(),
                                ItemBody::Sleep {},
                            ))
                        })
                        .collect(),
                ),
                ..Default::default()
            })
        };
        let turns = [
            turn("repeat", &["old", "shared"]),
            turn("repeat", &["new", "shared"]),
            turn("other", &["old", "shared"]),
            Arc::new(Turn {
                id: "repeat".into(),
                ..Default::default()
            }),
        ];
        for (turn, item, expected) in [
            ("repeat", "old", Some((0, 0))),
            ("repeat", "shared", Some((1, 1))),
            ("repeat", "new", Some((1, 0))),
            ("other", "shared", Some((2, 1))),
            ("missing", "old", None),
            ("repeat", "missing", None),
        ] {
            assert_eq!(item_position(&turns, &turn.into(), &item.into()), expected);
        }
    }

    #[test]
    fn completion_can_add_disjoint_items_to_an_active_occurrence() {
        for status in [TurnStatus::Running, TurnStatus::Unknown] {
            let item =
                |id: &str| Arc::new(Item::new(id.into(), Default::default(), ItemBody::Sleep {}));
            let previous = Thread {
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status,
                    items: Some(vec![item("streamed")]),
                    ..Default::default()
                })]),
                ..Default::default()
            };
            let updated = SessionChange::Turn {
                turn: Turn {
                    id: "turn".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![item("completed")]),
                    ..Default::default()
                },
                completed: true,
            }
            .apply(&previous)
            .unwrap();
            let turns = updated.turns.as_ref().unwrap();
            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].status, TurnStatus::Completed);
            assert_eq!(
                turns[0]
                    .items
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                ["streamed", "completed"]
            );
        }
    }

    #[test]
    fn new_items_append_only_to_the_named_turn() {
        let other = Arc::new(Turn {
            id: "other".into(),
            ..Default::default()
        });
        let previous = Thread {
            turns: Some(vec![
                Arc::new(Turn {
                    id: "target".into(),
                    ..Default::default()
                }),
                other.clone(),
            ]),
            ..Default::default()
        };
        let updated = SessionChange::Item {
            turn_id: "target".into(),
            item: Arc::new(Item::new(
                "new".into(),
                Default::default(),
                ItemBody::Sleep {},
            )),
        }
        .apply(&previous)
        .unwrap();
        let turns = updated.turns.as_ref().unwrap();
        assert_eq!(turns[0].items.as_ref().unwrap()[0].id.as_str(), "new");
        assert!(Arc::ptr_eq(&turns[1], &other));
    }

    #[test]
    fn a_new_occurrence_with_a_reused_turn_id_keeps_completed_history() {
        let item =
            |id: &str| Arc::new(Item::new(id.into(), Default::default(), ItemBody::Sleep {}));
        let old = Arc::new(Turn {
            id: "repeat".into(),
            status: TurnStatus::Completed,
            items: Some(vec![item("old")]),
            ..Default::default()
        });
        for completed in [false, true] {
            let previous = Thread {
                turns: Some(vec![old.clone()]),
                ..Default::default()
            };
            let next = SessionChange::Turn {
                completed,
                turn: Turn {
                    id: "repeat".into(),
                    status: if completed {
                        TurnStatus::Completed
                    } else {
                        TurnStatus::Running
                    },
                    items: Some(vec![item("new")]),
                    ..Default::default()
                },
            }
            .apply(&previous)
            .unwrap();
            let turns = next.turns.as_ref().unwrap();
            assert_eq!(turns.len(), 2);
            assert!(Arc::ptr_eq(&turns[0], &old));
            assert_eq!(turns[1].items.as_ref().unwrap()[0].id.as_str(), "new");
        }
    }

    #[test]
    fn late_completion_keeps_a_later_occurrence_running() {
        let item =
            |id: &str| Arc::new(Item::new(id.into(), Default::default(), ItemBody::Sleep {}));
        for items in [None, Some(vec![]), Some(vec![item("new")])] {
            let latest = Arc::new(Turn {
                id: "repeat".into(),
                status: TurnStatus::Running,
                items,
                ..Default::default()
            });
            let old = Turn {
                id: "repeat".into(),
                status: TurnStatus::Completed,
                items: Some(vec![item("old")]),
                ..Default::default()
            };
            let previous = Thread {
                turns: Some(vec![Arc::new(old.clone()), latest.clone()]),
                ..Default::default()
            };
            let updated = SessionChange::Turn {
                turn: Turn {
                    status: TurnStatus::Failed,
                    ..old
                },
                completed: true,
            }
            .apply(&previous)
            .unwrap();
            let turns = updated.turns.as_ref().unwrap();
            assert_eq!(turns.len(), 2);
            assert_eq!(turns[0].status, TurnStatus::Failed);
            assert!(Arc::ptr_eq(&turns[1], &latest));
            assert_eq!(updated.status, SessionStatus::Running);
            for items in [None, Some(vec![])] {
                let completed = SessionChange::Turn {
                    turn: Turn {
                        id: "repeat".into(),
                        status: TurnStatus::Completed,
                        items,
                        ..Default::default()
                    },
                    completed: true,
                }
                .apply(&updated)
                .unwrap();
                let turns = completed.turns.as_ref().unwrap();
                assert_eq!(turns.len(), 2);
                assert_eq!(turns[0].status, TurnStatus::Failed);
                assert_eq!(turns[1].status, TurnStatus::Completed);
                assert_eq!(completed.status, SessionStatus::Idle);
            }
        }
    }

    #[test]
    fn item_updates_follow_the_item_when_turn_ids_repeat() {
        let item = |id: &str, text: &str| {
            Arc::new(Item::new(
                id.into(),
                Default::default(),
                ItemBody::AssistantText {
                    text: text.into(),
                    phase: crate::items::AssistantPhase::Final,
                },
            ))
        };
        let latest = Arc::new(Turn {
            id: "repeat".into(),
            status: TurnStatus::Running,
            items: Some(vec![item("latest", "latest")]),
            ..Default::default()
        });
        let mut thread = Thread {
            turns: Some(vec![
                Arc::new(Turn {
                    id: "repeat".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![item("older", "summary")]),
                    ..Default::default()
                }),
                latest.clone(),
            ]),
            ..Default::default()
        };
        for change in [
            SessionChange::Item {
                turn_id: "repeat".into(),
                item: item("older", "full"),
            },
            SessionChange::Text {
                turn_id: "repeat".into(),
                item_id: "older".into(),
                field: TextField::AssistantText,
                delta: " tail".into(),
            },
        ] {
            thread = change.apply(&thread).unwrap();
            let turns = thread.turns.as_ref().unwrap();
            assert!(
                Arc::ptr_eq(&turns[1], &latest),
                "another occurrence must stay untouched"
            );
            assert_eq!(turns[0].items.as_ref().unwrap().len(), 1);
        }
        assert!(
            matches!(thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(),
            ItemBody::AssistantText {text, ..} if text == "full tail")
        );
        let removed = SessionChange::RemoveItem {
            turn_id: "repeat".into(),
            item_id: "older".into(),
        }
        .apply(&thread)
        .unwrap();
        assert!(
            removed.turns.as_ref().unwrap()[0]
                .items
                .as_ref()
                .unwrap()
                .is_empty()
        );
        assert!(Arc::ptr_eq(&removed.turns.as_ref().unwrap()[1], &latest));
    }

    #[test]
    fn turn_updates_keep_provider_and_fork_attribution_when_it_arrives_later() {
        for supports_fork in [false, true] {
            let mut thread = Thread {
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: TurnStatus::Running,
                    ..Default::default()
                })]),
                ..Default::default()
            };
            for completed in [false, true] {
                thread = SessionChange::Turn {
                    completed,
                    turn: Turn {
                        id: "turn".into(),
                        provider: Some(ProviderKind::Claude),
                        supports_fork: Some(supports_fork),
                        status: if completed {
                            TurnStatus::Completed
                        } else {
                            TurnStatus::Running
                        },
                        ..Default::default()
                    },
                }
                .apply(&thread)
                .unwrap();
                let turn = &thread.turns.as_ref().unwrap()[0];
                assert_eq!(turn.provider, Some(ProviderKind::Claude));
                assert_eq!(turn.supports_fork, Some(supports_fork));
            }
            let next = SessionChange::Turn {
                completed: true,
                turn: Turn {
                    id: "turn".into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
            }
            .apply(&thread)
            .unwrap();
            let turn = &next.turns.as_ref().unwrap()[0];
            assert_eq!(turn.provider, Some(ProviderKind::Claude));
            assert_eq!(turn.supports_fork, Some(supports_fork));
        }
    }

    #[test]
    fn workspace_updates_replace_only_the_directory() {
        let previous = Thread {
            cwd: Some("/before".into()),
            name: Some("conversation".into()),
            status: SessionStatus::Running,
            ..Default::default()
        };
        let next = SessionChange::Workspace {
            cwd: "/after".into(),
        }
        .apply(&previous)
        .unwrap();
        assert_eq!(
            next,
            Thread {
                cwd: Some("/after".into()),
                ..previous.clone()
            }
        );
        assert_eq!(previous.cwd.as_deref(), Some("/before"));
    }

    #[test]
    fn running_notifications_do_not_reopen_terminal_turns() {
        for status in [
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let previous = Turn {
                status,
                ..Default::default()
            };
            assert!(merge_turn(Some(&previous), &Turn::default(), false).is_none());
        }
        for status in [TurnStatus::Unknown, TurnStatus::Running] {
            let previous = Turn {
                status,
                ..Default::default()
            };
            assert_eq!(
                merge_turn(Some(&previous), &Turn::default(), false)
                    .unwrap()
                    .status,
                TurnStatus::Running
            );
        }
    }

    #[test]
    fn completed_unknown_notifications_become_completed() {
        assert_eq!(
            merge_turn(None, &Turn::default(), true).unwrap().status,
            TurnStatus::Completed
        );
        let previous = Turn {
            status: TurnStatus::Running,
            ..Default::default()
        };
        assert_eq!(
            merge_turn(Some(&previous), &Turn::default(), true)
                .unwrap()
                .status,
            TurnStatus::Completed
        );
    }

    #[test]
    fn terminal_notifications_clear_only_retrying_errors_unless_completed() {
        for retrying in [false, true] {
            let previous = Turn {
                error: Some(ExecutionError {
                    message: "retry failed".into(),
                    retry: Some(RetryEvidence {
                        retrying,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            for status in [TurnStatus::Failed, TurnStatus::Interrupted] {
                let incoming = Turn {
                    status,
                    ..Default::default()
                };
                assert_eq!(
                    merge_turn(Some(&previous), &incoming, true)
                        .unwrap()
                        .error
                        .is_none(),
                    retrying
                );
            }
            assert!(
                merge_turn(
                    Some(&previous),
                    &Turn {
                        status: TurnStatus::Completed,
                        ..Default::default()
                    },
                    true
                )
                .unwrap()
                .error
                .is_none()
            );
            assert!(
                merge_turn(Some(&previous), &Turn::default(), false)
                    .unwrap()
                    .error
                    .is_some()
            );
        }
    }
}

pub fn append_items(mut previous: Vec<Arc<Item>>, incoming: &[Arc<Item>]) -> Vec<Arc<Item>> {
    match incoming {
        [] => return previous,
        // Single-item notifications need no index allocation or hashing.
        [item] => {
            if let Some(current) = previous.iter_mut().find(|current| current.id == item.id) {
                *current = item.clone();
            } else {
                previous.push(item.clone());
            }
            return previous;
        }
        _ => {}
    }
    // Last update wins, while the first occurrence keeps its original position.
    // Borrow incoming identities so indexing does not copy their strings.
    let mut updates: HashMap<_, _> = incoming.iter().map(|item| (&item.id, item)).collect();
    for current in &mut previous {
        if let Some(item) = updates.remove(&current.id) {
            *current = item.clone();
        }
    }
    previous.reserve(updates.len());
    for item in incoming {
        if let Some(item) = updates.remove(&item.id) {
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
    /// The transcript version of the window the client already holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_version: Option<u64>,
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
    /// Availability of the source ranges, independent of the continuation cursor.
    pub history_read_state: Option<HistoryReadState>,
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
        thread.history_read_state = self.history_read_state.clone().or_else(|| {
            Some(HistoryReadState::new(
                if thread.history_cursor.is_some() {
                    HistoryReadKind::Partial
                } else {
                    HistoryReadKind::Complete
                },
                Vec::new(),
            ))
        });
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
    let mut hydrated = incoming.to_vec();
    for (index, cached) in previous[prefix..].iter().enumerate() {
        let fresh = &mut hydrated[index];
        if (fresh.items_summary || fresh.items.is_none()) && cached.items.is_some() {
            let cached_ids: HashSet<_> =
                cached.items.iter().flatten().map(|item| &item.id).collect();
            let changed = fresh.items.is_some()
                && (fresh.status == TurnStatus::Running
                    || fresh.status != cached.status
                    || fresh.duration_ms != cached.duration_ms
                    || fresh
                        .items
                        .iter()
                        .flatten()
                        .any(|item| !cached_ids.contains(&item.id)));
            let fresh = Arc::make_mut(fresh);
            let updates: Vec<_> = fresh
                .items
                .iter()
                .flatten()
                .filter(|item| !item.is_deferred() || changed || !cached_ids.contains(&item.id))
                .cloned()
                .collect();
            fresh.items = Some(append_items(
                cached.items.clone().unwrap_or_default(),
                &updates,
            ));
            fresh.items_summary = cached.items_summary || changed;
        }
    }
    Some(prepend_history(&previous[..prefix], &hydrated))
}

fn ordered_overlap(previous: &[Arc<Turn>], incoming: &[Arc<Turn>]) -> usize {
    if previous
        .iter()
        .map(|turn| &turn.id)
        .collect::<HashSet<_>>()
        .len()
        != previous.len()
        || incoming
            .iter()
            .map(|turn| &turn.id)
            .collect::<HashSet<_>>()
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
    /// The known window is current: the response omits its turns and paging.
    #[serde(default)]
    pub unchanged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub subscription_id: uuid::Uuid,
    pub change: SessionChange,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Capabilities {
    /// Input can join a running turn (T3 Code's active steering). Every
    /// provider can queue input behind one; the Host owns that queue.
    pub steer: bool,
    pub fork: bool,
    pub rename: bool,
    pub model_change: bool,
}
pub fn direct_input_unavailable_reason(can_accept_direct_input: Option<bool>) -> Option<String> {
    (can_accept_direct_input == Some(false))
        .then(|| "このサブエージェントの会話は閲覧専用です。親の会話から指示してください。".into())
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
    use std::slice::from_ref;

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

    #[test]
    fn item_updates_keep_positions_and_use_the_last_incoming_body() {
        let old = turn(0, 0..3, false).items.clone().unwrap();
        let mut incoming = turn(0, 2..5, false).items.clone().unwrap();
        let last = Arc::new(Item::new(
            "item-2".into(),
            Default::default(),
            ItemBody::Plan {
                text: "latest".into(),
            },
        ));
        incoming.extend([incoming[1].clone(), last.clone()]);
        let mut previous = old.clone();
        previous.push(old[2].clone());
        let merged = append_items(previous.clone(), &incoming);
        assert_eq!(
            merged
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["item-0", "item-1", "item-2", "item-2", "item-3", "item-4"]
        );
        assert!(Arc::ptr_eq(&merged[2], &last));
        assert!(
            Arc::ptr_eq(&merged[3], &old[2]),
            "later stored occurrences stay intact"
        );
        assert_eq!(append_items(previous.clone(), &[]), previous);
        let only_new = append_items(Vec::new(), &incoming);
        assert_eq!(
            only_new
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["item-2", "item-3", "item-4"]
        );
        assert!(Arc::ptr_eq(&only_new[0], &last));
    }

    proptest! {
        #[test]
        fn item_merges_preserve_order_and_last_writes(
            existing in prop::collection::btree_set(0u8..32, 0..32),
            updates in prop::collection::vec(0u8..48, 0..64),
        ) {
            let item = |id: u8| Arc::new(Item::new(id.to_string().into(), Default::default(), ItemBody::Sleep {}));
            let previous: Vec<_> = existing.iter().copied().map(item).collect();
            let incoming: Vec<_> = updates.iter().copied().map(item).collect();
            let merged = append_items(previous.clone(), &incoming);
            let new_ids: std::collections::BTreeSet<_> = updates.iter().filter(|id| !existing.contains(id)).collect();
            prop_assert_eq!(merged.len(), existing.len() + new_ids.len());
            prop_assert!(merged[..previous.len()].iter().map(|item| &item.id).eq(previous.iter().map(|item| &item.id)));
            let positions: Vec<_> = merged[previous.len()..].iter()
                .map(|item| incoming.iter().position(|update| update.id == item.id).unwrap()).collect();
            prop_assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            for actual in &merged {
                let expected = incoming.iter().rfind(|item| item.id == actual.id)
                    .or_else(|| previous.iter().find(|item| item.id == actual.id)).unwrap();
                prop_assert!(Arc::ptr_eq(actual, expected));
            }
        }

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
        let retained = retained_history(from_ref(&cached), &[fresh]).unwrap();
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
        let retained = retained_history(from_ref(&cached), &[fresh]).unwrap();
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
        let retained = retained_history(from_ref(&cached), &[unchanged]).unwrap();
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
        let retained = retained_history(&[cached], from_ref(&update)).unwrap();
        assert_eq!(retained[0].items, update.items);
        assert!(!retained[0].items_summary);
    }

    #[test]
    fn full_and_not_loaded_views_keep_their_distinct_cache_semantics() {
        let cached = turn(0, 0..8, false);
        let full = turn(0, 0..2, false);
        assert_eq!(
            retained_history(from_ref(&cached), from_ref(&full)).unwrap(),
            vec![full]
        );
        let mut omitted = turn(0, 0..0, false);
        Arc::make_mut(&mut omitted).items = None;
        let retained = retained_history(from_ref(&cached), &[omitted]).unwrap();
        assert_eq!(retained[0].items, cached.items);
        assert!(!retained[0].items_summary);
        let mut absent = cached.clone();
        Arc::make_mut(&mut absent).items = None;
        let summary = turn(0, 0..1, true);
        assert_eq!(
            retained_history(&[absent], from_ref(&summary)).unwrap(),
            vec![summary]
        );
        for changed in ["status", "duration"] {
            let mut summary = turn(0, 0..1, true);
            if changed == "status" {
                Arc::make_mut(&mut summary).status = TurnStatus::Failed;
            } else {
                Arc::make_mut(&mut summary).duration_ms = Some(500);
            }
            let retained = retained_history(from_ref(&cached), from_ref(&summary)).unwrap();
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
