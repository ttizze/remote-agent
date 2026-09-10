//! Protocol notifications mutate the same immutable snapshot as RPC responses.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    WatchChanged,
    WatchFailed,
    ThreadStatus,
    TurnStarted,
    TurnCompleted,
    Item,
    Review,
    Delta(&'static str),
    Error,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationParams {
    thread_id: String,
    watch_id: Option<u64>,
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
pub(super) fn notification(
    previous: &Snapshot,
    method: &str,
    params: Value,
) -> (Snapshot, Vec<Effect>) {
    if matches!(method, "process/outputDelta" | "process/exited") {
        return process(previous, method, params);
    }
    if method == "serverRequest/resolved" {
        return reduce(
            previous,
            Event::RequestResolved(params["requestId"].clone()),
        );
    }
    let kind = match method {
        "host/thread/changed" => Kind::WatchChanged,
        "host/thread/watchFailed" => Kind::WatchFailed,
        "thread/status/changed" => Kind::ThreadStatus,
        "turn/started" => Kind::TurnStarted,
        "turn/completed" => Kind::TurnCompleted,
        "item/started" | "item/completed" => Kind::Item,
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => Kind::Review,
        "item/agentMessage/delta" => Kind::Delta("agentMessage"),
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => Kind::Delta("reasoning"),
        "item/commandExecution/outputDelta" => Kind::Delta("commandExecution"),
        "item/fileChange/outputDelta" => Kind::Delta("fileChange"),
        "error" => Kind::Error,
        _ => return (previous.clone(), Vec::new()),
    };
    let params: NotificationParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return reduce(
                previous,
                Event::Failed(format!("invalid {method} notification: {error}")),
            );
        }
    };
    if matches!(kind, Kind::WatchChanged | Kind::WatchFailed)
        && (previous.navigation.watch_id != params.watch_id
            || previous.navigation.watch_thread_id.as_deref() != Some(&params.thread_id))
    {
        return (previous.clone(), Vec::new());
    }
    if kind == Kind::WatchChanged {
        return (
            previous.clone(),
            vec![Effect::execute(op::ReadThread::new(params.thread_id))],
        );
    }
    if kind == Kind::WatchFailed {
        return reduce(previous, Event::Failed("thread watch failed".into()));
    }
    let mut next = previous.clone();
    let current = previous.conversations.get(&params.thread_id);
    let late_start = kind == Kind::TurnStarted
        && params.turn.as_ref().is_some_and(|incoming| {
            current
                .and_then(|thread| thread.turns.as_ref())
                .is_some_and(|turns| {
                    turns.iter().any(|turn| {
                        turn.id == incoming.id
                            && turn
                                .status
                                .as_deref()
                                .is_some_and(|status| status != "inProgress")
                    })
                })
        });
    let active = match kind {
        Kind::ThreadStatus => params.status.as_ref().map(|status| status.kind == "active"),
        Kind::TurnStarted if !late_start => Some(true),
        Kind::TurnCompleted => Some(false),
        _ => None,
    };
    if let Some(active) = active {
        let unread = if active {
            false
        } else if kind == Kind::TurnCompleted
            && params
                .turn
                .as_ref()
                .is_some_and(|turn| turn.status.as_deref() == Some("completed"))
            && previous.navigation.thread_id.as_deref() != Some(&params.thread_id)
        {
            true
        } else {
            previous.activity.unread.contains(&params.thread_id)
        };
        if previous.activity.active.get(&params.thread_id) != Some(&active)
            || previous.activity.unread.contains(&params.thread_id) != unread
        {
            let activity = Arc::make_mut(&mut next.activity);
            activity.active.insert(params.thread_id.clone(), active);
            if unread {
                activity.unread.insert(params.thread_id.clone());
            } else {
                activity.unread.remove(&params.thread_id);
            }
        }
    }
    let mut effects = Vec::new();
    if active == Some(false)
        && previous.activity.active.get(&params.thread_id) == Some(&true)
        && current.is_some_and(|thread| thread.cwd.as_deref() == Some(&next.navigation.cwd))
    {
        effects.extend(op::review_workspace(&mut next));
    }
    let Some(current) = current else {
        return (next, effects);
    };
    if kind == Kind::ThreadStatus {
        if current.status == params.status {
            return (next, effects);
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
        return (next, effects);
    }
    let turn_id = params
        .turn
        .as_ref()
        .map(|turn| turn.id.as_str())
        .or(params.turn_id.as_deref());
    let Some(turn_id) = turn_id.filter(|id| !id.is_empty()) else {
        return (next, effects);
    };
    let turn_index = current
        .turns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .rposition(|turn| turn.id == turn_id);
    if matches!(kind, Kind::TurnStarted | Kind::TurnCompleted) {
        let (next, followup) = turn(previous, next, kind, params, turn_index);
        effects.extend(followup);
        return (next, effects);
    }
    let refresh_review = method == "item/completed"
        && params.item.as_ref().is_some_and(|item| {
            matches!(
                item.kind.as_deref(),
                Some("commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall")
            )
        })
        && current.cwd.as_deref() == Some(&next.navigation.cwd);
    let (mut next, followup) = match turn_index {
        Some(index) => item(previous, next, kind, params, index),
        None => (next, Vec::new()),
    };
    effects.extend(followup);
    if refresh_review {
        effects.extend(op::review_workspace(&mut next));
    }
    (next, effects)
}

fn item(
    previous: &Snapshot,
    mut next: Snapshot,
    kind: Kind,
    params: NotificationParams,
    turn_index: usize,
) -> (Snapshot, Vec<Effect>) {
    let old = &previous.conversations[&params.thread_id]
        .turns
        .as_ref()
        .unwrap()[turn_index];
    let turn_id = old.id.as_str();
    let mut item = params.item;
    let review = kind == Kind::Review;
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
    if kind == Kind::Error {
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
    } else if kind == Kind::Item || review {
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
        let Kind::Delta(expected) = kind else {
            return (next, Vec::new());
        };
        if old.items.as_ref().unwrap()[index].kind.as_deref() != Some(expected) {
            return (next, Vec::new());
        }
        if expected == "fileChange"
            && old.items.as_ref().unwrap()[index]
                .extra
                .get("changes")
                .is_some_and(|changes| {
                    changes.as_array().is_none_or(|changes| {
                        changes.last().is_some_and(|change| !change.is_object())
                    })
                })
        {
            next.error = Some("invalid file change delta target".into());
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
                let changes = changes.as_array_mut().unwrap();
                if changes.is_empty() {
                    changes.push(serde_json::json!({"path":"","kind":"update","diff":""}));
                }
                if let Some(change) = changes.last_mut().and_then(Value::as_object_mut) {
                    append_text(change.entry("diff").or_insert(Value::Null), &delta);
                }
            }
            _ => unreachable!(),
        }
    }
    if kind == Kind::Item {
        reconcile_pending(&mut next, &params.thread_id);
    }
    (next, Vec::new())
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

fn process(previous: &Snapshot, method: &str, params: Value) -> (Snapshot, Vec<Effect>) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ProcessEvent {
        process_handle: String,
        delta_base64: Option<String>,
        cap_reached: Option<bool>,
        exit_code: Option<i32>,
    }
    let params: ProcessEvent = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return reduce(
                previous,
                Event::Failed(format!("invalid {method} notification: {error}")),
            );
        }
    };
    let Some(current) = previous.terminals.get(&params.process_handle) else {
        return (previous.clone(), Vec::new());
    };
    if matches!(
        current.phase,
        TerminalPhase::Closed | TerminalPhase::Exited(_)
    ) {
        return (previous.clone(), Vec::new());
    }
    let mut next = previous.clone();
    let terminal = Arc::make_mut(
        Arc::make_mut(&mut next.terminals)
            .get_mut(&params.process_handle)
            .unwrap(),
    );
    if method == "process/exited" {
        let Some(code) = params.exit_code else {
            return reduce(
                previous,
                Event::Failed("process exit code is missing".into()),
            );
        };
        terminal.phase = TerminalPhase::Exited(code);
    } else {
        let Some(data) = params.delta_base64 else {
            return reduce(previous, Event::Failed("process output is missing".into()));
        };
        terminal.sequence += 1;
        terminal.output.push_back(Arc::new(TerminalOutput {
            sequence: terminal.sequence,
            data,
            cap_reached: params.cap_reached.unwrap_or(false),
        }));
    }
    (next, Vec::new())
}

fn turn(
    previous: &Snapshot,
    mut next: Snapshot,
    kind: Kind,
    params: NotificationParams,
    turn_index: Option<usize>,
) -> (Snapshot, Vec<Effect>) {
    let Some(incoming) = params.turn else {
        return (next, Vec::new());
    };
    let old = turn_index.map(|index| {
        &previous.conversations[&params.thread_id]
            .turns
            .as_ref()
            .unwrap()[index]
    });
    if kind == Kind::TurnStarted
        && old.is_some_and(|old| {
            old.status
                .as_deref()
                .is_some_and(|status| status != "inProgress")
        })
    {
        return (next, Vec::new());
    }
    let mut merged = old.map_or_else(|| incoming.clone(), |old| merge_fields(old, &incoming));
    merged.status = Some(if kind == Kind::TurnStarted {
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
    reconcile_pending(&mut next, &params.thread_id);
    (next, Vec::new())
}
