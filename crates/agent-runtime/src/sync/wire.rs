//! What clients receive, ported from T3 `WireProjection.ts` and `toolOutput.ts`.
//! Storage keeps everything; delivery withholds tool output, file bodies and
//! transfer transcripts, and bounds long detail text. `getTurnItem` reads the
//! withheld parts with a larger bound.
use crate::StoredFact;
use agent_domain::{FactBody, HistoricalContext, Item, ItemKind, ItemStatus, Json, State, Task};
use regex::Regex;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

pub const MAX_DETAIL_STRING_BYTES: usize = 32_768;
pub const MAX_DYNAMIC_VALUE_BYTES: usize = 16_384;
pub const MAX_ON_DEMAND_BYTES: usize = 256 * 1024;
pub const TRUNCATION_MARKER: &str = "\n… output truncated for transport";
const SUMMARY_CHARS: usize = 160;
const MAX_PARSED_BYTES: usize = 16_384;
const MAX_METADATA_BYTES: usize = 8_192;
const MAX_ID_LENGTH: usize = 256;
const MAX_THREADS: usize = 100;
const MAX_CONTENT_BLOCKS: usize = 32;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: i64 = 128;
/// Keys that carry file bodies in provider file-change payloads.
const FILE_BODY_KEYS: [&str; 6] = [
    "diff",
    "old_string",
    "new_string",
    "content",
    "edits",
    "new_source",
];

/// JavaScript `\s`, which `trim` and T3's summaries use.
pub fn js_space(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The longest prefix within `max_units` UTF-16 units and `max_bytes` UTF-8 bytes.
fn prefix(text: &str, max_units: usize, max_bytes: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > max_units || index + c.len_utf8() > max_bytes {
            return &text[..index];
        }
    }
    text
}

/// T3 `truncateDetail`: at most `max_bytes` UTF-8 bytes on a character boundary,
/// followed by the transport marker.
pub fn truncate_detail(value: &str, max_bytes: usize) -> Cow<'_, str> {
    if value.len() <= max_bytes {
        return Cow::Borrowed(value);
    }
    Cow::Owned(format!(
        "{}{TRUNCATION_MARKER}",
        prefix(value, max_bytes, max_bytes)
    ))
}

/// T3 `summarizeDynamicValue`: a large input becomes its first nonblank line.
pub fn summarize_dynamic_value(value: &Value) -> Cow<'_, Value> {
    let serialized: Cow<str> = match value {
        Value::String(text) if utf16_len(text) > MAX_DYNAMIC_VALUE_BYTES => Cow::Borrowed(text),
        _ => {
            let json = serde_json::to_string(value).unwrap_or_default();
            if json.len() <= MAX_DYNAMIC_VALUE_BYTES {
                return Cow::Borrowed(value);
            }
            match value {
                Value::String(text) => Cow::Borrowed(text),
                _ => Cow::Owned(json),
            }
        }
    };
    let mut first_line = String::new();
    let mut units = 0;
    match serialized.find(|c: char| !js_space(c)) {
        None => first_line.push_str("Large tool output"),
        Some(start) => {
            let mut pending_space = false;
            for c in serialized[start..].chars() {
                if c == '\n' {
                    break;
                }
                if js_space(c) {
                    pending_space = true;
                    continue;
                }
                if pending_space {
                    first_line.push(' ');
                    units += 1;
                }
                first_line.push(c);
                units += c.len_utf16();
                pending_space = false;
                if units > SUMMARY_CHARS {
                    break;
                }
            }
        }
    }
    let summary = if utf16_len(&first_line) <= SUMMARY_CHARS {
        first_line
    } else {
        format!(
            "{}…",
            prefix(&first_line, SUMMARY_CHARS - 1, usize::MAX).trim_end_matches(js_space)
        )
    };
    Cow::Owned(serde_json::json!({ "summary": summary, "truncated": true }))
}

/// JavaScript `typeof value === "object" && value !== null`.
fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

struct ReadBudget {
    remaining_bytes: usize,
    remaining_nodes: i64,
    exceeded: bool,
}

#[derive(Default)]
struct Envelope<'a> {
    data: Option<Cow<'a, Map<String, Value>>>,
    failed: bool,
}

/// T3 `readResult`: walks MCP result envelopes for the data object and failure flags.
fn read_result<'a>(
    value: Option<&'a Value>,
    budget: &mut ReadBudget,
    depth: usize,
) -> Envelope<'a> {
    if depth > MAX_ENVELOPE_DEPTH || {
        budget.remaining_nodes -= 1;
        budget.remaining_nodes < 0
    } {
        budget.exceeded = true;
        return Envelope::default();
    }
    match value {
        Some(Value::String(text)) => {
            if utf16_len(text) > budget.remaining_bytes || text.len() > budget.remaining_bytes {
                budget.exceeded = true;
                return Envelope::default();
            }
            budget.remaining_bytes -= text.len();
            match serde_json::from_str::<Value>(text) {
                Ok(parsed) => {
                    let nested = read_result(Some(&parsed), budget, depth + 1);
                    Envelope {
                        data: nested.data.map(|data| Cow::Owned(data.into_owned())),
                        failed: nested.failed,
                    }
                }
                Err(_) => Envelope::default(),
            }
        }
        Some(Value::Array(blocks)) => {
            if blocks.len() > MAX_CONTENT_BLOCKS {
                budget.exceeded = true;
                return Envelope::default();
            }
            let mut envelope = Envelope::default();
            for block in blocks {
                let text = block.as_object().and_then(|block| block.get("text"));
                let text = match text {
                    Some(text) if is_object(text) => match text.get("text") {
                        Some(inner) if !inner.is_null() => Some(inner),
                        _ => Some(text),
                    },
                    text => text,
                };
                let result = read_result(text, budget, depth + 1);
                if envelope.data.is_none() {
                    envelope.data = result.data;
                }
                envelope.failed |= result.failed;
                if budget.exceeded {
                    break;
                }
            }
            envelope
        }
        Some(Value::Object(object)) => {
            let failed = object.get("isError") == Some(&Value::Bool(true))
                || object.get("is_error") == Some(&Value::Bool(true))
                || object.get("_tag").and_then(Value::as_str) == Some("OrchestratorMcpFailure")
                || object.get("error").is_some_and(|error| !error.is_null());
            let content = match object.get("structuredContent") {
                Some(content) if !content.is_null() => Some(content),
                _ => object.get("content"),
            };
            match content {
                Some(content) => {
                    let nested = read_result(Some(content), budget, depth + 1);
                    Envelope {
                        data: nested.data,
                        failed: failed || nested.failed,
                    }
                }
                None => Envelope {
                    data: Some(Cow::Borrowed(object)),
                    failed,
                },
            }
        }
        _ => Envelope::default(),
    }
}

fn bounded_id(value: Option<&Value>) -> Option<String> {
    let id = value?.as_str()?;
    (utf16_len(id) <= MAX_ID_LENGTH && !id.trim_matches(js_space).is_empty()).then(|| id.into())
}

/// T3 `compactDynamicToolOutput`: only result identities and failure metadata.
pub fn compact_dynamic_tool_output(value: &Value) -> Option<Value> {
    let mut budget = ReadBudget {
        remaining_bytes: MAX_PARSED_BYTES,
        remaining_nodes: MAX_ENVELOPE_NODES,
        exceeded: false,
    };
    let result = read_result(Some(value), &mut budget, 0);
    let mut output = Map::new();
    if result.failed {
        output.insert("isError".into(), Value::Bool(true));
    }
    let data = result.data.filter(|_| !budget.exceeded);
    if let Some(data) = data {
        for key in ["threadId", "messageId", "taskId", "scheduledTaskId"] {
            if let Some(id) = bounded_id(data.get(key)) {
                output.insert(key.into(), Value::String(id));
            }
        }
        if data.get("status").and_then(Value::as_str) == Some("rolled_back") {
            output.insert("status".into(), "rolled_back".into());
        }
        if let Some(thread) = data.get("thread").filter(|thread| is_object(thread))
            && let Some(id) = bounded_id(thread.get("threadId"))
        {
            output.insert("thread".into(), serde_json::json!({ "threadId": id }));
        }
        if let Some(Value::Array(entries)) = data.get("threads") {
            let mut complete = entries.len() <= MAX_THREADS;
            let mut threads = vec![];
            if complete {
                for entry in entries {
                    if !is_object(entry) {
                        complete = false;
                        break;
                    }
                    let id = bounded_id(entry.get("threadId"));
                    let rolled_back =
                        entry.get("status").and_then(Value::as_str) == Some("rolled_back");
                    if id.is_none() && !rolled_back {
                        complete = false;
                        break;
                    }
                    let mut thread = Map::new();
                    if let Some(id) = id {
                        thread.insert("threadId".into(), Value::String(id));
                    }
                    if rolled_back {
                        thread.insert("status".into(), "rolled_back".into());
                    }
                    threads.push(Value::Object(thread));
                }
            }
            if complete {
                output.insert("threads".into(), Value::Array(threads));
            } else {
                output.remove("threadId");
                output.remove("status");
            }
        }
    }
    if serde_json::to_vec(&output).map_or(0, |json| json.len()) > MAX_METADATA_BYTES {
        for key in ["threads", "threadId", "status"] {
            output.remove(key);
        }
    }
    (!output.is_empty()).then_some(Value::Object(output))
}

static FAILURE_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        r"(?i)file not found|no files found|enoent|no such file|commandnotfoundexception|command not found|is not recognized as the name of a cmdlet|a parameter cannot be found that matches parameter name",
        r"(?i)<exited with exit code\s+[1-9][0-9]*\s*>",
        r"(?i)exit(?:ed)? with exit code\s+[1-9][0-9]*",
        r"(?i)exit code\s*[:\s]\s*[1-9][0-9]*(?-u:\b)",
    ]
    .map(|pattern| Regex::new(pattern).expect("failure pattern compiles"))
});

/// T3 `toolOutputIndicatesFailure`: some providers report completion even when the
/// output describes a failure.
pub fn tool_output_indicates_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    FAILURE_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(text))
        || (lower.contains("cannot find path") && lower.contains("because it does not exist"))
        || (lower.contains("is not recognized") && lower.contains("the term '"))
}

/// T3 `hasDynamicValue`.
fn has_dynamic_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim_matches(js_space).is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(object) => !object.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

/// File identity without the bodies of the edit.
fn without_file_bodies(changes: &Value) -> Value {
    let strip = |value: &Value| match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !FILE_BODY_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        value => value.clone(),
    };
    match changes {
        Value::Array(entries) => Value::Array(entries.iter().map(strip).collect()),
        value => strip(value),
    }
}

/// Whether delivery changes items of this kind.
pub fn withholds_detail(kind: &ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::CommandExecution { .. }
            | ItemKind::FileChange { .. }
            | ItemKind::DynamicTool { .. }
    )
}

/// The kind as delivered: file bodies dropped, large tool input summarized and
/// tool output reduced to its identities.
pub fn wire_kind(kind: &ItemKind) -> Cow<'_, ItemKind> {
    match kind {
        ItemKind::FileChange { changes } => Cow::Owned(ItemKind::FileChange {
            changes: Json(without_file_bodies(&changes.0)),
        }),
        ItemKind::DynamicTool {
            presentation,
            name,
            input,
            output,
        } => Cow::Owned(ItemKind::DynamicTool {
            presentation: presentation.clone(),
            name: name.clone(),
            input: Json(summarize_dynamic_value(&input.0).into_owned()),
            output: output
                .as_ref()
                .and_then(|output| compact_dynamic_tool_output(&output.0))
                .map(Json),
        }),
        kind => Cow::Borrowed(kind),
    }
}

fn with_text(item: &Item, kind: ItemKind, text: String) -> Item {
    Item {
        id: item.id.clone(),
        run: item.run.clone(),
        attempt: item.attempt.clone(),
        native_key: item.native_key.clone(),
        ordinal: item.ordinal,
        kind,
        status: item.status,
        text,
        started_at: item.started_at.clone(),
        completed_at: item.completed_at.clone(),
        output_omitted: item.output_omitted,
        output_indicates_failure: item.output_indicates_failure,
    }
}

/// T3 `projectTurnItemForWire`.
pub fn wire_item(item: &Item) -> Cow<'_, Item> {
    let projected = match &item.kind {
        ItemKind::CommandExecution { exit_code, .. } => {
            let output = prefix(&item.text, MAX_DETAIL_STRING_BYTES, usize::MAX);
            let mut projected = with_text(item, item.kind.clone(), String::new());
            projected.output_indicates_failure |=
                exit_code.is_some_and(|code| code != 0) || tool_output_indicates_failure(output);
            projected.output_omitted |= !item.text.trim_matches(js_space).is_empty();
            projected
        }
        ItemKind::FileChange { .. } => {
            // A failed edit keeps the provider's error where the diff would be.
            let text = if item.status == ItemStatus::Failed
                && !item.text.trim_matches(js_space).is_empty()
            {
                truncate_detail(&item.text, MAX_DETAIL_STRING_BYTES).into_owned()
            } else {
                String::new()
            };
            with_text(item, wire_kind(&item.kind).into_owned(), text)
        }
        ItemKind::DynamicTool { output, .. } => {
            let mut projected = with_text(item, wire_kind(&item.kind).into_owned(), String::new());
            projected.output_omitted |= output.as_ref().is_some_and(|o| has_dynamic_value(&o.0))
                || !item.text.trim_matches(js_space).is_empty();
            projected
        }
        _ => return Cow::Borrowed(item),
    };
    Cow::Owned(projected)
}

fn bound_dynamic_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_detail(text, MAX_ON_DEMAND_BYTES).into()),
        value => {
            let json = serde_json::to_string(value).unwrap_or_default();
            if json.len() <= MAX_ON_DEMAND_BYTES {
                value.clone()
            } else {
                Value::String(truncate_detail(&json, MAX_ON_DEMAND_BYTES).into())
            }
        }
    }
}

/// T3 `projectTurnItemForDetail`: the input and output the timeline withholds,
/// bounded so a huge result cannot stall the connection.
pub fn detail_item(item: &Item) -> Item {
    let text = truncate_detail(&item.text, MAX_ON_DEMAND_BYTES).into_owned();
    match &item.kind {
        ItemKind::CommandExecution {
            command,
            cwd,
            exit_code,
            title,
        } => with_text(
            item,
            ItemKind::CommandExecution {
                command: truncate_detail(command, MAX_ON_DEMAND_BYTES).into(),
                cwd: cwd.clone(),
                exit_code: *exit_code,
                title: title.clone(),
            },
            text,
        ),
        ItemKind::DynamicTool {
            presentation,
            name,
            input,
            output,
        } => with_text(
            item,
            ItemKind::DynamicTool {
                presentation: presentation.clone(),
                name: name.clone(),
                input: Json(bound_dynamic_value(&input.0)),
                output: output
                    .as_ref()
                    .map(|output| Json(bound_dynamic_value(&output.0))),
            },
            text,
        ),
        ItemKind::FileChange { .. } => wire_item(item).into_owned(),
        _ => item.clone(),
    }
}

fn wire_task(task: &Task) -> Option<Task> {
    let prompt = truncate_detail(&task.prompt, MAX_DETAIL_STRING_BYTES);
    let progress = task
        .progress
        .as_deref()
        .map(|text| truncate_detail(text, MAX_DETAIL_STRING_BYTES));
    let result = task
        .result
        .as_deref()
        .map(|text| truncate_detail(text, MAX_DETAIL_STRING_BYTES));
    let owned = |text: &Option<Cow<str>>| matches!(text, Some(Cow::Owned(_)));
    if matches!(prompt, Cow::Borrowed(_)) && !owned(&progress) && !owned(&result) {
        return None;
    }
    let mut projected = task.clone();
    projected.prompt = prompt.into_owned();
    projected.progress = progress.map(Cow::into_owned);
    projected.result = result.map(Cow::into_owned);
    Some(projected)
}

fn empty_history() -> HistoricalContext {
    HistoricalContext {
        messages: vec![],
        context: String::new(),
        omitted_items: 0,
        omitted_item_ids: vec![],
    }
}

fn carries_history(history: &HistoricalContext) -> bool {
    history != &empty_history()
}

fn projects_item(item: &Item) -> bool {
    matches!(wire_item(item), Cow::Owned(_))
}

fn needs_projection(state: &State) -> bool {
    state.transfers.iter().any(|t| carries_history(&t.history))
        || state
            .items
            .iter()
            .chain(&state.inherited_items)
            .any(projects_item)
        || state.tasks.iter().any(|task| wire_task(task).is_some())
}

/// The projection a client folds (T3 `projectThreadProjectionForWire`).
pub fn client_state(state: &Arc<State>) -> Arc<State> {
    if !needs_projection(state) {
        return state.clone();
    }
    let mut projected = State::clone(state);
    for transfer in &mut projected.transfers {
        transfer.history = empty_history();
    }
    for item in projected
        .items
        .iter_mut()
        .chain(&mut projected.inherited_items)
    {
        if let Cow::Owned(wire) = wire_item(item) {
            *item = wire;
        }
    }
    for task in &mut projected.tasks {
        if let Some(wire) = wire_task(task) {
            *task = wire;
        }
    }
    Arc::new(projected)
}

/// One fact as clients receive it, given the state after it was committed.
fn client_fact(state: &State, body: &FactBody) -> Option<FactBody> {
    let truncated = |text: &str| match truncate_detail(text, MAX_DETAIL_STRING_BYTES) {
        Cow::Owned(text) => Some(text),
        Cow::Borrowed(_) => None,
    };
    match body {
        FactBody::TransferOpened { history, .. } if carries_history(history) => {
            let mut body = body.clone();
            if let FactBody::TransferOpened { history, .. } = &mut body {
                *history = empty_history();
            }
            Some(body)
        }
        FactBody::ForkAccepted {
            parent,
            boundary,
            history,
            messages,
        } if history.iter().any(projects_item) => Some(FactBody::ForkAccepted {
            parent: parent.clone(),
            boundary: *boundary,
            history: history
                .iter()
                .map(|item| wire_item(item).into_owned())
                .collect(),
            messages: messages.clone(),
        }),
        FactBody::ItemStarted { id, kind, .. } if withholds_detail(kind) => {
            if let Some(item) = state.items.iter().find(|item| &item.id == id) {
                return Some(FactBody::ItemProjected {
                    item: wire_item(item).into_owned(),
                });
            }
            let mut body = body.clone();
            if let FactBody::ItemStarted { kind, .. } = &mut body {
                *kind = wire_kind(kind).into_owned();
            }
            Some(body)
        }
        FactBody::ItemTextAppended { id, .. }
        | FactBody::ItemTextReplaced { id, .. }
        | FactBody::ItemDetailChanged { id, .. }
        | FactBody::ItemCompleted { id, .. }
        | FactBody::ItemReopened { id } => {
            let item = state.items.iter().find(|item| &item.id == id)?;
            withholds_detail(&item.kind).then(|| FactBody::ItemProjected {
                item: wire_item(item).into_owned(),
            })
        }
        FactBody::TaskStarted { prompt, .. } | FactBody::TaskReopened { prompt, .. } => {
            let text = truncated(prompt)?;
            let mut body = body.clone();
            if let FactBody::TaskStarted { prompt, .. } | FactBody::TaskReopened { prompt, .. } =
                &mut body
            {
                *prompt = text;
            }
            Some(body)
        }
        FactBody::TaskProgressed {
            id,
            progress: Some(progress),
            model,
        } => truncated(progress).map(|progress| FactBody::TaskProgressed {
            id: id.clone(),
            progress: Some(progress),
            model: model.clone(),
        }),
        FactBody::TaskFinished { id, status, result } => {
            truncated(result).map(|result| FactBody::TaskFinished {
                id: id.clone(),
                status: *status,
                result,
            })
        }
        _ => None,
    }
}

/// Facts as clients receive them (T3 `projectDomainEventForWire`). `state` is the
/// thread after the facts; a fact that changed a tool item carries that item.
pub fn client_facts(state: &State, facts: &Arc<[StoredFact]>) -> Arc<[StoredFact]> {
    let projected: Vec<Option<FactBody>> = facts
        .iter()
        .map(|stored| client_fact(state, &stored.fact.body))
        .collect();
    if projected.iter().all(Option::is_none) {
        return facts.clone();
    }
    facts
        .iter()
        .zip(projected)
        .map(|(stored, body)| match body {
            Some(body) => StoredFact {
                global_seq: stored.global_seq,
                thread_seq: stored.thread_seq,
                fact: agent_domain::Fact {
                    at: stored.fact.at.clone(),
                    body,
                },
            },
            None => stored.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
