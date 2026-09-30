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

impl SessionChange {
    /// Updates only the supplied conversation. The caller owns assignment,
    /// subscriptions, unread state, drafts and any follow-up IO.
    pub fn apply(&self, previous: &Thread) -> Result<Thread, UpdateError> {
        let mut next = previous.clone();
        match self {
            Self::Submission { id, delivery } => {
                next.submissions.insert(id.clone(), delivery.clone());
                return Ok(next);
            }
            Self::Request { request } => {
                next.requests
                    .insert(request.id.clone(), Arc::new(request.clone()));
                return Ok(next);
            }
            Self::RequestDelivery { request_id, state } => {
                let request = next
                    .requests
                    .get_mut(request_id)
                    .ok_or(UpdateError::MissingRequest)?;
                Arc::make_mut(request).delivery = *state;
                return Ok(next);
            }
            Self::ResolveRequest { request_id } => {
                next.requests.remove(request_id);
                return Ok(next);
            }
            _ => {}
        }
        if let Self::Status { status } = self {
            next.status = *status;
            return Ok(next);
        }
        let turn_id = match self {
            Self::Turn { turn, .. } => &turn.id,
            Self::Item { turn_id, .. }
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
        let index = previous
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rposition(|turn| &turn.id == turn_id);
        if let Self::Turn { turn, completed } = self {
            let old = index.map(|index| &previous.turns.as_ref().unwrap()[index]);
            if !completed
                && old.is_some_and(|old| {
                    old.status != TurnStatus::Running && old.status != TurnStatus::Unknown
                })
            {
                return Ok(next);
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
            if let Some(old) = old {
                if turn.started_at.is_none() {
                    merged.started_at = old.started_at;
                }
                if turn.completed_at.is_none() {
                    merged.completed_at = old.completed_at;
                }
                if turn.duration_ms.is_none() {
                    merged.duration_ms = old.duration_ms;
                }
            }
            if let Some(items) = &turn.items {
                merged.items = Some(
                    if items.is_empty()
                        || matches!(turn.items_view.as_deref(), Some("summary" | "notLoaded"))
                    {
                        append_items(
                            old.and_then(|old| old.items.as_deref()).unwrap_or_default(),
                            items,
                        )
                    } else {
                        items.clone()
                    },
                );
            }
            if turn.error.is_none()
                && (merged.status == TurnStatus::Completed
                    || old.is_some_and(|old| {
                        old.error
                            .as_ref()
                            .is_some_and(|e| e.retry.as_ref().is_some_and(|retry| retry.retrying))
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
            return Ok(next);
        }
        let index = index.ok_or(UpdateError::MissingTurn)?;
        let old = &previous.turns.as_ref().unwrap()[index];
        let turn = Arc::make_mut(&mut next.turns.as_mut().unwrap()[index]);
        match self {
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
                    return Ok(next);
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
                    return Ok(next);
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
                    && old.status != TurnStatus::Running
                {
                    return Ok(previous.clone());
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
        Ok(next)
    }
}

pub(crate) fn merge_fields(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = previous.clone();
    if incoming.status != TurnStatus::Unknown {
        merged.status = incoming.status;
    }
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        items_view,
        items_has_more,
        opening_user_message,
        started_at,
        completed_at,
        duration_ms,
        error,
        started_at_ms,
        completed_at_ms
    );

    merged
}

pub(crate) fn append_items(previous: &[Arc<Item>], incoming: &[Arc<Item>]) -> Vec<Arc<Item>> {
    let mut merged = previous.to_vec();
    for item in incoming {
        if let Some(index) = merged.iter().position(|current| current.id == item.id) {
            merged[index] = item.clone();
        } else {
            merged.push(item.clone());
        }
    }
    merged
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSession {
    pub session: SessionRef,
    pub limit: usize,
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
    #[serde(other)]
    Other,
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
