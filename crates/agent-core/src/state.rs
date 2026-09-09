//! Immutable client state and pure conversation transitions.
use crate::{
    client::{Answer, ServerRequest},
    models::{Item, ListQuery, Model, Thread, ThreadList, ThreadStatus, Turn},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: Option<String>,
    pub effort: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub path: String,
    pub name: String,
    pub is_image: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub conversations: Arc<BTreeMap<String, Arc<Thread>>>,
    pub threads: Option<Arc<ThreadList>>,
    pub models: Arc<Vec<Model>>,
    pub requests: Arc<BTreeMap<String, Arc<ServerRequest>>>,
    pub drafts: Arc<BTreeMap<String, Draft>>,
    pub connected: bool,
    pub error: Option<String>,
}
#[derive(Debug)]
pub enum Intent {
    ListThreads(ListQuery),
    StartThread {
        cwd: Option<String>,
        model: Option<String>,
    },
    ReadThread(String),
    ReadOlder {
        thread_id: String,
        turn_id: Option<String>,
        cursor: Option<String>,
    },
    ReadItem {
        thread_id: String,
        turn_id: String,
        item_id: String,
    },
    LoadModels,
    SetDraft {
        thread_id: String,
        draft: Draft,
    },
    Submit {
        thread_id: String,
        client_user_message_id: String,
    },
    Interrupt {
        thread_id: String,
        turn_id: String,
    },
    Respond {
        request_id: Value,
        answer: Answer,
    },
    Watch {
        thread_id: String,
        watch_key: u64,
        watch_id: u64,
        path: Option<String>,
    },
    Unwatch {
        watch_key: u64,
        watch_id: u64,
    },
}
#[derive(Debug)]
pub enum Event {
    Intent(Intent),
    ItemLoaded {
        thread_id: String,
        turn_id: String,
        item: Item,
    },
    Submitted {
        thread_id: String,
        draft: Draft,
    },
    ThreadRefreshed(Thread),
    OlderLoaded {
        thread_id: String,
        thread: Thread,
        turn_id: Option<String>,
        cursor: Option<String>,
    },
    ThreadsLoaded(ThreadList),
    ModelsLoaded(Vec<Model>),
    ServerRequest(ServerRequest),
    RequestResolved(Value),
    Notification {
        method: String,
        params: Value,
    },
    Connected,
    Disconnected(String),
    Failed(String),
}
#[derive(Debug)]
pub enum Effect {
    Execute(Intent),
}

pub fn reduce(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    match event {
        Event::Intent(Intent::SetDraft { thread_id, draft }) => {
            Arc::make_mut(&mut next.drafts).insert(thread_id, draft);
        }
        Event::Intent(intent) => return (next, vec![Effect::Execute(intent)]),
        Event::ItemLoaded {
            thread_id,
            turn_id,
            item,
        } => {
            return (
                upsert_item(previous, &thread_id, &turn_id, item),
                Vec::new(),
            );
        }
        Event::Submitted { thread_id, draft } => {
            if previous.drafts.get(&thread_id) == Some(&draft) {
                let draft = Arc::make_mut(&mut next.drafts).get_mut(&thread_id).unwrap();
                draft.text.clear();
                draft.attachments.clear();
            }
        }
        Event::ThreadRefreshed(incoming) => {
            let Some(id) = incoming.id.clone().filter(|id| !id.trim().is_empty()) else {
                next.error = Some("thread ID is missing".into());
                return (next, Vec::new());
            };
            let thread = previous
                .conversations
                .get(&id)
                .map_or_else(|| incoming.clone(), |current| refresh(current, &incoming));
            Arc::make_mut(&mut next.conversations).insert(id, Arc::new(thread));
        }
        Event::OlderLoaded {
            thread_id,
            thread,
            turn_id,
            cursor,
        } => {
            let id = &thread_id;
            if let Some(current) = previous.conversations.get(id) {
                match older(current, &thread, turn_id.as_deref(), cursor.as_deref()) {
                    Ok(merged) => {
                        Arc::make_mut(&mut next.conversations).insert(id.clone(), Arc::new(merged));
                    }
                    Err(error) => next.error = Some(error),
                }
            }
        }
        Event::ThreadsLoaded(threads) => next.threads = Some(Arc::new(threads)),
        Event::ModelsLoaded(models) => next.models = Arc::new(models),
        Event::ServerRequest(request) => {
            Arc::make_mut(&mut next.requests).insert(request.id.to_string(), Arc::new(request));
        }
        Event::RequestResolved(id) => {
            Arc::make_mut(&mut next.requests).remove(&id.to_string());
        }
        Event::Notification { method, params } => return notification(previous, &method, params),
        Event::Connected => {
            next.connected = true;
            next.error = None;
        }
        Event::Disconnected(reason) => {
            next.connected = false;
            next.error = Some(reason);
        }
        Event::Failed(error) => next.error = Some(error),
    }
    (next, Vec::new())
}

fn merge_fields(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = previous.clone();
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        status,
        items_view,
        items_has_more,
        items_next_cursor,
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

fn append_items(previous: &[Arc<Item>], incoming: &[Arc<Item>]) -> Vec<Arc<Item>> {
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

/// Prepend older occurrences, matching overlap from the newest end exactly once.
fn prepend<T>(older: &[Arc<T>], newer: &[Arc<T>], id: impl Fn(&T) -> &str) -> Vec<Arc<T>> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for value in newer {
        *counts.entry(id(value)).or_default() += 1;
    }
    let mut prefix = Vec::with_capacity(older.len());
    for value in older.iter().rev() {
        if let Some(count) = counts.get_mut(id(value)).filter(|count| **count > 0) {
            *count -= 1;
        } else {
            prefix.push(value.clone());
        }
    }
    prefix.reverse();
    prefix.extend_from_slice(newer);
    prefix
}

pub(crate) fn older(
    previous: &Thread,
    incoming: &Thread,
    turn_id: Option<&str>,
    cursor: Option<&str>,
) -> Result<Thread, String> {
    if previous.id != incoming.id {
        return Err("thread ID does not match".into());
    }
    let mut merged = previous.clone();
    if let Some(turn_id) = turn_id {
        let Some(index) = previous
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rposition(|turn| turn.id == turn_id)
        else {
            return Ok(merged);
        };
        let current = &previous.turns.as_ref().unwrap()[index];
        if current
            .items_next_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref())
            != cursor
        {
            return Ok(merged);
        }
        let page = incoming
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rfind(|turn| turn.id == turn_id)
            .ok_or("history page is missing the requested turn")?;
        let next_cursor = page
            .items_next_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref());
        if next_cursor == cursor && (cursor.is_some() || page.items_has_more == Some(true)) {
            return Err("history cursor did not advance".into());
        }
        let mut turn = current.as_ref().clone();
        turn.items = Some(prepend(
            page.items.as_deref().unwrap_or_default(),
            current.items.as_deref().unwrap_or_default(),
            |item| &item.id,
        ));
        turn.items_has_more = Some(page.items_has_more.unwrap_or(false));
        turn.items_next_cursor = Some(next_cursor.map(str::to_owned));
        let mut deferred = page.deferred_item_ids.clone().unwrap_or_default();
        deferred.extend(current.deferred_item_ids.iter().flatten().cloned());
        deferred.retain(|id| {
            let loaded_in = |turn: &Turn| {
                turn.items
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|item| &item.id == id)
                    && !turn
                        .deferred_item_ids
                        .as_deref()
                        .unwrap_or_default()
                        .contains(id)
            };
            !loaded_in(current) && !loaded_in(page)
        });
        deferred.sort();
        deferred.dedup();
        turn.deferred_item_ids = Some(deferred);
        merged.turns.as_mut().unwrap()[index] = Arc::new(turn);
    } else {
        if previous
            .history_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref())
            != cursor
        {
            return Ok(merged);
        }
        let next_cursor = incoming
            .history_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref());
        if next_cursor == cursor && cursor.is_some() {
            return Err("history cursor did not advance".into());
        }
        merged.turns = Some(prepend(
            incoming.turns.as_deref().unwrap_or_default(),
            previous.turns.as_deref().unwrap_or_default(),
            |turn| &turn.id,
        ));
        merged.history_cursor = Some(next_cursor.map(str::to_owned));
    }
    Ok(merged)
}

pub(crate) fn refresh(previous: &Thread, incoming: &Thread) -> Thread {
    if incoming.history_cursor.is_none() {
        return incoming.clone();
    }
    let current = previous.turns.as_deref().unwrap_or_default();
    let mut positions: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (index, turn) in current.iter().enumerate() {
        positions.entry(&turn.id).or_default().push_back(index);
    }
    let mut first = None;
    let mut turns = Vec::new();
    for incoming in incoming.turns.as_deref().unwrap_or_default() {
        if let Some(index) = positions
            .get_mut(incoming.id.as_str())
            .and_then(VecDeque::pop_front)
        {
            first = Some(first.map_or(index, |first: usize| first.min(index)));
            turns.push(Arc::new(refresh_turn(&current[index], incoming)));
        } else {
            turns.push(incoming.clone());
        }
    }
    let prefix = &current[..first.unwrap_or(current.len())];
    let mut all = Vec::with_capacity(prefix.len() + turns.len());
    all.extend_from_slice(prefix);
    all.extend(turns);
    let mut merged = incoming.clone();
    merged.turns = Some(all);
    if !prefix.is_empty() || first.is_some() {
        merged.history_cursor = previous.history_cursor.clone();
    }
    merged
}
fn refresh_turn(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = merge_fields(previous, incoming);
    let deferred = incoming.deferred_item_ids.as_deref().unwrap_or_default();
    let mut items = previous.items.clone().unwrap_or_default();
    for item in incoming.items.as_deref().unwrap_or_default() {
        if let Some(index) = items.iter().position(|old| old.id == item.id) {
            if !deferred.contains(&item.id)
                || previous
                    .deferred_item_ids
                    .as_deref()
                    .unwrap_or_default()
                    .contains(&item.id)
            {
                items[index] = item.clone();
            }
        } else {
            items.push(item.clone());
        }
    }
    merged.items = Some(items);
    merged.deferred_item_ids = Some(
        deferred
            .iter()
            .filter(|id| {
                !previous
                    .items
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|item| &item.id == *id)
                    || previous
                        .deferred_item_ids
                        .as_deref()
                        .unwrap_or_default()
                        .contains(id)
            })
            .cloned()
            .collect(),
    );
    if previous.items_has_more != Some(true) {
        merged.items_has_more = Some(false);
        merged.items_next_cursor = Some(None);
    }
    merged
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationParams {
    thread_id: String,
    turn_id: Option<String>,
    turn: Option<Turn>,
    item: Option<Item>,
    item_id: Option<String>,
    status: Option<ThreadStatus>,
    delta: Option<String>,
    error: Option<Value>,
    #[serde(default)]
    will_retry: bool,
    review_id: Option<String>,
    review: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
fn notification(previous: &Snapshot, method: &str, params: Value) -> (Snapshot, Vec<Effect>) {
    if method == "serverRequest/resolved" {
        return reduce(
            previous,
            Event::RequestResolved(params["requestId"].clone()),
        );
    }
    let known = matches!(
        method,
        "host/thread/changed"
            | "host/thread/watchFailed"
            | "thread/status/changed"
            | "turn/started"
            | "turn/completed"
            | "item/started"
            | "item/completed"
            | "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "error"
            | "item/autoApprovalReview/started"
            | "item/autoApprovalReview/completed"
    );
    if !known {
        return (previous.clone(), Vec::new());
    }
    let params: NotificationParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return reduce(
                previous,
                Event::Failed(format!("invalid {method} notification: {error}")),
            );
        }
    };
    if method == "host/thread/changed" {
        return (
            previous.clone(),
            vec![Effect::Execute(Intent::ReadThread(params.thread_id))],
        );
    }
    if method == "host/thread/watchFailed" {
        return reduce(previous, Event::Failed("thread watch failed".into()));
    }
    let Some(current) = previous.conversations.get(&params.thread_id) else {
        return (previous.clone(), Vec::new());
    };
    let mut next = previous.clone();
    if method == "thread/status/changed" {
        if current.status == params.status {
            return (next, Vec::new());
        }
        let thread = Arc::make_mut(
            Arc::make_mut(&mut next.conversations)
                .get_mut(&params.thread_id)
                .unwrap(),
        );
        thread.status = params.status.clone();
        if let Some(list) = next.threads.as_mut().map(Arc::make_mut)
            && let Some(thread) = list
                .data
                .iter_mut()
                .find(|thread| thread.id.as_ref() == Some(&params.thread_id))
        {
            thread.status = params.status;
        }
        return (next, Vec::new());
    }
    let turn_id = params
        .turn
        .as_ref()
        .map(|turn| turn.id.as_str())
        .or(params.turn_id.as_deref());
    let Some(turn_id) = turn_id.filter(|id| !id.is_empty()) else {
        return (next, Vec::new());
    };
    let turn_index = current
        .turns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .rposition(|turn| turn.id == turn_id);
    if matches!(method, "turn/started" | "turn/completed") {
        let Some(incoming) = params.turn else {
            return (next, Vec::new());
        };
        let old = turn_index.map(|index| &current.turns.as_ref().unwrap()[index]);
        if method == "turn/started"
            && old.is_some_and(|old| {
                old.status
                    .as_deref()
                    .is_some_and(|status| status != "inProgress")
            })
        {
            return (next, Vec::new());
        }
        let mut merged = old.map_or_else(|| incoming.clone(), |old| merge_fields(old, &incoming));
        merged.status = Some(if method == "turn/started" {
            "inProgress".into()
        } else {
            incoming
                .status
                .clone()
                .unwrap_or_else(|| "completed".into())
        });
        if let Some(old) = old {
            if incoming.started_at == Some(None) {
                merged.started_at = old.started_at.clone();
            }
            if incoming.completed_at == Some(None) {
                merged.completed_at = old.completed_at.clone();
            }
            if incoming.duration_ms == Some(None) {
                merged.duration_ms = old.duration_ms;
            }
        }
        if let Some(items) = &incoming.items {
            let preserve = items.is_empty()
                || matches!(
                    incoming.items_view.as_deref(),
                    Some("summary" | "notLoaded")
                );
            merged.items = Some(if preserve {
                append_items(
                    old.and_then(|old| old.items.as_deref()).unwrap_or_default(),
                    items,
                )
            } else {
                items.clone()
            });
            if let Some(deferred) = &mut merged.deferred_item_ids {
                deferred.retain(|id| !items.iter().any(|item| &item.id == id));
            }
        }
        if incoming.error.is_none()
            && (merged.status.as_deref() == Some("completed")
                || old.is_some_and(|old| {
                    old.error
                        .as_ref()
                        .is_some_and(|error| error["willRetry"] == true)
                }) && merged.status.as_deref() != Some("inProgress"))
        {
            merged.error = None;
        }
        let thread = Arc::make_mut(
            Arc::make_mut(&mut next.conversations)
                .get_mut(&params.thread_id)
                .unwrap(),
        );
        let turns = thread.turns.get_or_insert_with(Vec::new);
        if let Some(index) = turn_index {
            turns[index] = Arc::new(merged);
        } else {
            turns.push(Arc::new(merged));
        }
        return (next, Vec::new());
    }
    let Some(turn_index) = turn_index else {
        return (next, Vec::new());
    };
    let old = &current.turns.as_ref().unwrap()[turn_index];
    let mut item = params.item;
    let review = matches!(
        method,
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed"
    );
    let remove_review = review
        && params
            .review
            .as_ref()
            .is_some_and(|review| review["status"] == "approved");
    if review {
        let Some(id) = params.review_id else {
            return (next, Vec::new());
        };
        let mut extra = params.extra;
        extra.insert("threadId".into(), Value::String(params.thread_id.clone()));
        extra.insert("turnId".into(), Value::String(turn_id.into()));
        if let Some(value) = params.review {
            extra.insert("review".into(), value);
        }
        item = Some(Item {
            id,
            kind: Some("automaticApprovalReview".into()),
            extra,
            ..Default::default()
        });
    }
    let item_id = item
        .as_ref()
        .map(|item| item.id.as_str())
        .or(params.item_id.as_deref());
    let item_index = item_id.and_then(|id| {
        old.items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .position(|item| item.id == id)
    });
    if method == "error" {
        if params.will_retry && old.status.as_deref() != Some("inProgress") {
            return (next, Vec::new());
        }
        let mut error = match params.error.unwrap_or(Value::Null) {
            Value::Object(error) => error,
            message => Map::from_iter([("message".into(), message)]),
        };
        error.insert("willRetry".into(), Value::Bool(params.will_retry));
        let turn = mutable_turn(&mut next, &params.thread_id, turn_index);
        turn.error = Some(Value::Object(error));
    } else if matches!(method, "item/started" | "item/completed") || review {
        if remove_review && item_index.is_none() {
            return (next, Vec::new());
        }
        let Some(item) = item else {
            return (next, Vec::new());
        };
        let turn = mutable_turn(&mut next, &params.thread_id, turn_index);
        if let Some(deferred) = &mut turn.deferred_item_ids {
            deferred.retain(|id| id != &item.id);
        }
        let items = turn.items.get_or_insert_with(Vec::new);
        if remove_review {
            items.remove(item_index.unwrap());
        } else if let Some(index) = item_index {
            items[index] = Arc::new(item);
        } else {
            items.push(Arc::new(item));
        }
    } else {
        let Some(index) = item_index else {
            return (next, Vec::new());
        };
        let Some(delta) = params.delta.filter(|delta| !delta.is_empty()) else {
            return (next, Vec::new());
        };
        let expected = match method {
            "item/agentMessage/delta" => "agentMessage",
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => "reasoning",
            "item/commandExecution/outputDelta" => "commandExecution",
            "item/fileChange/outputDelta" => "fileChange",
            _ => return (next, Vec::new()),
        };
        if old.items.as_ref().unwrap()[index].kind.as_deref() != Some(expected) {
            return (next, Vec::new());
        }
        let item = Arc::make_mut(
            &mut mutable_turn(&mut next, &params.thread_id, turn_index)
                .items
                .as_mut()
                .unwrap()[index],
        );
        match expected {
            "agentMessage" => item.text.get_or_insert_with(String::new).push_str(&delta),
            "commandExecution" => item
                .aggregated_output
                .get_or_insert_with(String::new)
                .push_str(&delta),
            "reasoning" => append_text(item.extra.entry("summary").or_insert(Value::Null), &delta),
            "fileChange" => {
                let changes = item
                    .extra
                    .entry("changes")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if !changes.is_array() {
                    *changes = Value::Array(Vec::new());
                }
                let changes = changes.as_array_mut().unwrap();
                if changes.is_empty() {
                    changes.push(serde_json::json!({"path":"","kind":"update","diff":""}));
                }
                append_text(&mut changes.last_mut().unwrap()["diff"], &delta);
            }
            _ => unreachable!(),
        }
    }
    (next, Vec::new())
}
fn mutable_turn<'a>(snapshot: &'a mut Snapshot, thread_id: &str, index: usize) -> &'a mut Turn {
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(thread_id)
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[index])
}
fn append_text(value: &mut Value, delta: &str) {
    if !value.is_string() {
        let text = match value.take() {
            Value::Array(parts) => parts
                .into_iter()
                .filter_map(|part| match part {
                    Value::String(text) => Some(text),
                    Value::Object(mut object) => object
                        .remove("text")
                        .and_then(|text| text.as_str().map(str::to_owned)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        *value = Value::String(text);
    }
    if let Value::String(text) = value {
        text.push_str(delta);
    }
}

fn upsert_item(previous: &Snapshot, thread_id: &str, turn_id: &str, item: Item) -> Snapshot {
    let Some(thread) = previous.conversations.get(thread_id) else {
        return previous.clone();
    };
    let Some(index) = thread
        .turns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .rposition(|turn| turn.id == turn_id)
    else {
        return previous.clone();
    };
    let mut next = previous.clone();
    let turn = mutable_turn(&mut next, thread_id, index);
    if let Some(deferred) = &mut turn.deferred_item_ids {
        deferred.retain(|id| id != &item.id);
    }
    let items = turn.items.get_or_insert_with(Vec::new);
    if let Some(index) = items.iter().position(|old| old.id == item.id) {
        items[index] = Arc::new(item);
    } else {
        items.push(Arc::new(item));
    }
    next
}
