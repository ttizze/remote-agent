//! Shared, pure conversation updates. Neither provider IO nor client navigation
//! belongs here: owners apply the returned value to their current conversation.
use crate::models::{Item, Thread, ThreadStatus, Turn, append_text};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Codex,
    Claude,
}

/// A native provider ID, scoped by provider. Paths and abbreviated IDs are not
/// resolved here; only the provider adapter can resolve a native session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    pub provider: ProviderKind,
    pub id: String,
}

impl SessionRef {
    pub fn from_thread_id(id: &str) -> Result<Self, &'static str> {
        let (provider, id) = match id.strip_prefix("claude:") {
            Some(id) => (ProviderKind::Claude, id),
            None => (ProviderKind::Codex, id),
        };
        if id.is_empty() || id.len() > 4096 || id.trim() != id {
            return Err("native session ID is required");
        }
        Ok(Self {
            provider,
            id: id.into(),
        })
    }

    pub fn thread_id(&self) -> String {
        match self.provider {
            ProviderKind::Codex => self.id.clone(),
            ProviderKind::Claude => format!("claude:{}", self.id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextField {
    Message,
    Reasoning,
    CommandOutput,
    FileChange,
}

/// A small change to the current conversation, never a persistent event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionChange {
    Request {
        request: crate::client::ServerRequest,
    },
    RequestDelivery {
        request_id: String,
        state: RequestDelivery,
    },
    ResolveRequest {
        request_id: String,
    },
    Status {
        status: ThreadStatus,
    },
    Turn {
        turn: Turn,
        completed: bool,
    },
    Item {
        turn_id: String,
        item: Item,
    },
    RemoveItem {
        turn_id: String,
        item_id: String,
    },
    Text {
        turn_id: String,
        item_id: String,
        field: TextField,
        delta: String,
    },
    Error {
        turn_id: String,
        error: Value,
        will_retry: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestDelivery {
    Awaiting,
    Sending,
    Unknown,
}

impl SessionChange {
    /// Updates only the supplied conversation. The caller owns assignment,
    /// subscriptions, unread state, drafts and any follow-up IO.
    pub fn apply(&self, previous: &Thread) -> Result<Thread, &'static str> {
        let mut next = previous.clone();
        match self {
            Self::Request { request } => {
                next.requests
                    .insert(request.id.to_string(), Arc::new(request.clone()));
                return Ok(next);
            }
            Self::RequestDelivery { request_id, state } => {
                let request = next
                    .requests
                    .get_mut(request_id)
                    .ok_or("request is no longer pending")?;
                Arc::make_mut(request).extra.insert(
                    "deliveryState".into(),
                    serde_json::to_value(state).map_err(|_| "invalid delivery state")?,
                );
                return Ok(next);
            }
            Self::ResolveRequest { request_id } => {
                next.requests.remove(request_id);
                return Ok(next);
            }
            _ => {}
        }
        if let Self::Status { status } = self {
            next.status = Some(status.clone());
            return Ok(next);
        }
        let turn_id = match self {
            Self::Turn { turn, .. } => &turn.id,
            Self::Item { turn_id, .. }
            | Self::RemoveItem { turn_id, .. }
            | Self::Text { turn_id, .. }
            | Self::Error { turn_id, .. } => turn_id,
            Self::Status { .. }
            | Self::Request { .. }
            | Self::RequestDelivery { .. }
            | Self::ResolveRequest { .. } => unreachable!(),
        };
        if turn_id.is_empty() {
            return Err("turn ID is required");
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
                && old.is_some_and(|old| old.status.as_deref().is_some_and(|s| s != "inProgress"))
            {
                return Ok(next);
            }
            let mut merged = old.map_or_else(|| turn.clone(), |old| merge_fields(old, turn));
            merged.status = Some(if *completed {
                turn.status.clone().unwrap_or_else(|| "completed".into())
            } else {
                "inProgress".into()
            });
            if let Some(old) = old {
                if turn.started_at == Some(None) {
                    merged.started_at = old.started_at.clone();
                }
                if turn.completed_at == Some(None) {
                    merged.completed_at = old.completed_at.clone();
                }
                if turn.duration_ms == Some(None) {
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
                if let Some(deferred) = &mut merged.deferred_item_ids {
                    deferred.retain(|id| !items.iter().any(|item| &item.id == id));
                }
            }
            if turn.error.is_none()
                && (merged.status.as_deref() == Some("completed")
                    || old.is_some_and(|old| {
                        old.error.as_ref().is_some_and(|e| e["willRetry"] == true)
                    }))
                && merged.status.as_deref() != Some("inProgress")
            {
                merged.error = None;
            }
            let turns = next.turns.get_or_insert_default();
            if let Some(index) = index {
                turns[index] = Arc::new(merged);
            } else {
                turns.push(Arc::new(merged));
            }
            let active = turns
                .iter()
                .any(|turn| turn.status.as_deref() == Some("inProgress"));
            next.status = Some(ThreadStatus {
                kind: if active { "active" } else { "idle" }.into(),
                extra: Map::new(),
            });
            return Ok(next);
        }
        let Some(index) = index else {
            return Ok(next);
        };
        let old = &previous.turns.as_ref().unwrap()[index];
        let turn = Arc::make_mut(&mut next.turns.as_mut().unwrap()[index]);
        match self {
            Self::Item { item, .. } => {
                if let Some(deferred) = &mut turn.deferred_item_ids {
                    deferred.retain(|id| id != &item.id);
                }
                let items = turn.items.get_or_insert_default();
                if let Some(index) = items.iter().position(|current| current.id == item.id) {
                    items[index] = Arc::new(item.clone());
                } else {
                    items.push(Arc::new(item.clone()));
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
                if delta.is_empty() {
                    return Ok(previous.clone());
                }
                let Some(item) = turn
                    .items
                    .as_mut()
                    .and_then(|items| items.iter_mut().find(|item| &item.id == item_id))
                else {
                    return Ok(previous.clone());
                };
                let expected = match field {
                    TextField::Message => "agentMessage",
                    TextField::Reasoning => "reasoning",
                    TextField::CommandOutput => "commandExecution",
                    TextField::FileChange => "fileChange",
                };
                if item.kind.as_deref() != Some(expected) {
                    return Ok(previous.clone());
                }
                if *field == TextField::FileChange
                    && item
                        .changes
                        .as_ref()
                        .is_some_and(|changes| !changes.accepts_delta())
                {
                    return Err("invalid file change delta target");
                }
                let item = Arc::make_mut(item);
                match field {
                    TextField::Message => item.text.get_or_insert_default().push_str(delta),
                    TextField::CommandOutput => item
                        .aggregated_output
                        .get_or_insert_default()
                        .push_str(delta),
                    TextField::Reasoning => {
                        append_text(item.extra.entry("summary").or_insert(Value::Null), delta)
                    }
                    TextField::FileChange => item
                        .changes
                        .get_or_insert_with(|| crate::models::ItemChanges::Files(Vec::new()))
                        .append_delta(delta),
                }
            }
            Self::Error {
                error, will_retry, ..
            } => {
                if *will_retry && old.status.as_deref() != Some("inProgress") {
                    return Ok(previous.clone());
                }
                let mut error = match error.clone() {
                    Value::Object(error) => error,
                    message => Map::from_iter([("message".into(), message)]),
                };
                error.insert("willRetry".into(), Value::Bool(*will_retry));
                turn.error = Some(Value::Object(error));
            }
            Self::Status { .. }
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
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        status,
        items_view,
        items_has_more,
        deferred_item_ids,
        opening_user_message,
        started_at,
        completed_at,
        duration_ms,
        error
    );
    merged.extra.extend(incoming.extra.clone());
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
    pub subscription_id: uuid::Uuid,
    pub response: crate::models::ThreadResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub subscription_id: uuid::Uuid,
    pub change: SessionChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSession {
    pub subscription_id: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub additional_input: bool,
    pub fork: bool,
    pub rename: bool,
    pub model_change: bool,
}
impl SessionRef {
    pub fn capabilities(&self) -> Capabilities {
        match self.provider {
            ProviderKind::Codex => Capabilities {
                additional_input: true,
                fork: true,
                rename: true,
                model_change: true,
            },
            ProviderKind::Claude => Capabilities {
                additional_input: false,
                fork: false,
                rename: false,
                model_change: true,
            },
        }
    }
}
pub fn capabilities(thread: &Thread) -> Capabilities {
    thread
        .extra
        .get("capabilities")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| {
            SessionRef::from_thread_id(thread.id.as_deref().unwrap_or("unknown"))
                .map(|session| session.capabilities())
                .unwrap_or(Capabilities {
                    additional_input: false,
                    fork: false,
                    rename: false,
                    model_change: false,
                })
        })
}
pub fn input_unavailable_reason(thread: &Thread) -> Option<String> {
    (!capabilities(thread).additional_input && thread.turns.iter().flatten().any(|turn| turn.status.as_deref() == Some("inProgress")))
        .then(|| "このプロバイダは実行中の追加送信に対応していません。完了を待つか、停止してから送信してください。".into())
}
