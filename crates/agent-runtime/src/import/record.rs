//! Transcript records and the visible user/assistant history they contain.
use super::json::Seg;
use agent_domain::{Driver, Role, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashSet, VecDeque};

pub const MAX_IMPORTED_MESSAGES: usize = 200;
pub const MAX_IMPORT_RECORDS: usize = 100_000;
const FALLBACK_TITLE: &str = "Imported thread";
const TITLE_UTF16_UNITS: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: Role,
    pub text: String,
    pub created_at: Timestamp,
}

/// One native session's visible history, ready to import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionThread {
    pub source: Driver,
    pub instance: String,
    pub session: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMeta {
    pub source: Driver,
    pub instance: String,
    /// Claude transcripts are named by session; Codex rollout names are not resumable IDs.
    pub fallback_session: String,
    pub last_active_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Block {
    kind: Option<String>,
    text: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Content {
    Text(String),
    Blocks(Vec<Block>),
}
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RecordMessage {
    content: Option<Content>,
    model: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Payload {
    id: Option<String>,
    session_id: Option<String>,
    kind: Option<String>,
    role: Option<String>,
    message: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    content: Option<Vec<Block>>,
    passthrough: Option<Value>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Record {
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    ai_title: Option<String>,
    is_sidechain: Option<bool>,
    is_meta: Option<bool>,
    is_compact_summary: Option<bool>,
    message: Option<RecordMessage>,
    payload: Option<Payload>,
}

const RECORD_FIELDS: [&str; 10] = [
    "type",
    "timestamp",
    "cwd",
    "sessionId",
    "aiTitle",
    "isSidechain",
    "isMeta",
    "isCompactSummary",
    "message",
    "payload",
];
const PAYLOAD_TEXT_FIELDS: [&str; 7] = [
    "id",
    "session_id",
    "type",
    "role",
    "message",
    "model",
    "cwd",
];

fn key(seg: &Seg) -> Option<&str> {
    match seg {
        Seg::Key(key) => Some(key),
        Seg::Index(_) => None,
    }
}

fn block_path(path: &[Seg]) -> bool {
    match path {
        [] => true,
        [Seg::Key(field)] => field == "type" || field == "text",
        _ => false,
    }
}

fn blocks_path(path: &[Seg]) -> bool {
    match path {
        [] | [Seg::Key(_), ..] => true,
        [Seg::Index(_), rest @ ..] => block_path(rest),
    }
}

/// The record schema's fields; everything else is skipped while reading.
pub(crate) fn selected(path: &[Seg]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return true;
    };
    match key(first) {
        Some(field) if RECORD_FIELDS.contains(&field) => match (field, rest) {
            (_, []) => true,
            ("message", [Seg::Key(inner), more @ ..]) => match inner.as_str() {
                "role" | "model" => more.is_empty(),
                "content" => more.is_empty() || matches!(more[0], Seg::Key(_)) || blocks_path(more),
                _ => false,
            },
            ("payload", [Seg::Key(inner), more @ ..]) => match inner.as_str() {
                "content" => blocks_path(more),
                "internal_chat_message_metadata_passthrough" => true,
                field => more.is_empty() && PAYLOAD_TEXT_FIELDS.contains(&field),
            },
            _ => false,
        },
        _ => false,
    }
}

type Field<T> = Result<Option<T>, ()>;

fn text(object: &Map<String, Value>, field: &str) -> Field<String> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(()),
    }
}

fn flag(object: &Map<String, Value>, field: &str) -> Field<bool> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(()),
    }
}

fn blocks(value: &Value) -> Result<Vec<Block>, ()> {
    value
        .as_array()
        .ok_or(())?
        .iter()
        .map(|block| {
            let block = block.as_object().ok_or(())?;
            Ok(Block {
                kind: text(block, "type")?,
                text: text(block, "text")?,
            })
        })
        .collect()
}

fn object<'a>(record: &'a Map<String, Value>, field: &str) -> Field<&'a Map<String, Value>> {
    match record.get(field) {
        None => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(()),
    }
}

/// Decodes a parsed record; any field of the wrong type rejects the whole record.
pub(crate) fn decode(value: &Value) -> Option<Record> {
    let record = value.as_object()?;
    let decoded = (|| -> Result<Record, ()> {
        let message = object(record, "message")?
            .map(|message| -> Result<RecordMessage, ()> {
                text(message, "role")?;
                Ok(RecordMessage {
                    content: match message.get("content") {
                        None => None,
                        Some(Value::String(content)) => Some(Content::Text(content.clone())),
                        Some(content) => Some(Content::Blocks(blocks(content)?)),
                    },
                    model: text(message, "model")?,
                })
            })
            .transpose()?;
        let payload = object(record, "payload")?
            .map(|payload| -> Result<Payload, ()> {
                Ok(Payload {
                    id: text(payload, "id")?,
                    session_id: text(payload, "session_id")?,
                    kind: text(payload, "type")?,
                    role: text(payload, "role")?,
                    message: text(payload, "message")?,
                    model: text(payload, "model")?,
                    cwd: text(payload, "cwd")?,
                    content: payload.get("content").map(blocks).transpose()?,
                    passthrough: payload
                        .get("internal_chat_message_metadata_passthrough")
                        .cloned(),
                })
            })
            .transpose()?;
        Ok(Record {
            kind: text(record, "type")?,
            timestamp: text(record, "timestamp")?,
            cwd: text(record, "cwd")?,
            session_id: text(record, "sessionId")?,
            ai_title: text(record, "aiTitle")?,
            is_sidechain: flag(record, "isSidechain")?,
            is_meta: flag(record, "isMeta")?,
            is_compact_summary: flag(record, "isCompactSummary")?,
            message,
            payload,
        })
    })();
    decoded.ok()
}

fn nonblank(value: Option<&String>) -> Option<&str> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

pub(crate) fn record_cwd(record: &Record) -> Option<String> {
    nonblank(record.cwd.as_ref())
        .or_else(|| nonblank(record.payload.as_ref().and_then(|p| p.cwd.as_ref())))
        .map(str::to_owned)
}

/// Records the parser can use; everything else is dropped while reading.
pub(crate) fn retained(source: Driver, record: &Record) -> bool {
    if record_cwd(record).is_some() {
        return true;
    }
    let kind = record.kind.as_deref();
    let payload = record.payload.as_ref();
    let payload_kind = payload.and_then(|p| p.kind.as_deref());
    match source {
        Driver::Claude => {
            matches!(kind, Some("user" | "assistant"))
                || record.session_id.is_some()
                || record.ai_title.is_some()
                || record
                    .message
                    .as_ref()
                    .is_some_and(|message| message.model.is_some())
        }
        Driver::Codex => {
            matches!(kind, Some("session_meta" | "turn_context"))
                || (kind == Some("event_msg") && payload_kind == Some("user_message"))
                || (kind == Some("response_item")
                    && payload_kind == Some("message")
                    && matches!(
                        payload.and_then(|p| p.role.as_deref()),
                        Some("user" | "assistant")
                    ))
        }
    }
}

/// The `cwd` of a metadata line, untrimmed, from the record or its Codex payload.
pub(crate) fn line_cwd(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let record = value.as_object()?;
    let field = |object: &Map<String, Value>| {
        object
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
            .map(str::to_owned)
    };
    field(record).or_else(|| record.get("payload")?.as_object().and_then(field))
}

fn extract_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter(|block| {
            matches!(
                block.kind.as_deref(),
                Some("text" | "input_text" | "output_text")
            )
        })
        .map(|block| block.text.as_deref().unwrap_or_default().trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text(content: Option<&Content>) -> String {
    match content {
        None => String::new(),
        Some(Content::Text(text)) => text.trim().to_owned(),
        Some(Content::Blocks(blocks)) => extract_text(blocks),
    }
}

fn normalize_timestamp(value: Option<&str>, fallback: &Timestamp) -> Timestamp {
    value
        .and_then(|value| Timestamp::parse(value).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn codex_turn_id(metadata: Option<&Value>) -> Option<String> {
    let turn = metadata?.as_object()?.get("turn_id")?.as_str()?;
    (!turn.trim().is_empty()).then(|| turn.to_owned())
}

fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, char) in text.char_indices() {
        used += char.len_utf16();
        if used > units {
            return &text[..index];
        }
    }
    text
}

#[derive(Clone)]
struct Kept {
    seq: u64,
    role: Role,
    text: String,
    at: Timestamp,
    response_user: bool,
}

/// Parses a whole transcript's contents.
pub fn parse_session_transcript(meta: &TranscriptMeta, contents: &str) -> Option<SessionThread> {
    let contents = contents.strip_suffix('\n').unwrap_or(contents);
    let lines: Vec<&str> = contents.split('\n').take(MAX_IMPORT_RECORDS + 1).collect();
    if lines.len() > MAX_IMPORT_RECORDS {
        return None;
    }
    let records: Vec<Record> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| decode(&value))
        .collect();
    parse_records(meta, &records)
}

/// Keeps visible user and assistant text while ignoring tools, reasoning and malformed records.
pub(crate) fn parse_records(meta: &TranscriptMeta, records: &[Record]) -> Option<SessionThread> {
    let fallback = Timestamp::from_millis(meta.last_active_ms)
        .unwrap_or_else(|_| Timestamp::from_millis(0).expect("epoch"));
    let mut session = match meta.source {
        Driver::Codex => String::new(),
        Driver::Claude => meta.fallback_session.clone(),
    };
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut has_codex_session = false;
    let canonical = if meta.source == Driver::Codex {
        canonical_response_users(records)
    } else {
        HashSet::new()
    };

    let mut messages: VecDeque<Kept> = VecDeque::new();
    let mut first_user: Option<Kept> = None;
    let mut next_seq = 0;
    let mut retain = |messages: &mut VecDeque<Kept>,
                      first_user: &mut Option<Kept>,
                      role: Role,
                      text: String,
                      at: Timestamp,
                      response_user: bool| {
        next_seq += 1;
        let kept = Kept {
            seq: next_seq,
            role,
            text,
            at,
            response_user,
        };
        if first_user.is_none() && role == Role::User {
            *first_user = Some(kept.clone());
        }
        messages.push_back(kept);
        if messages.len() > MAX_IMPORTED_MESSAGES {
            messages.pop_front();
        }
    };

    for (index, record) in records.iter().enumerate() {
        let kind = record.kind.as_deref();
        let payload = record.payload.as_ref();
        if meta.source == Driver::Claude {
            if record.is_sidechain == Some(true)
                || record.is_meta == Some(true)
                || record.is_compact_summary == Some(true)
            {
                continue;
            }
            if let Some(id) = nonblank(record.session_id.as_ref()) {
                session = id.to_owned();
            }
            if let Some(ai_title) = nonblank(record.ai_title.as_ref()) {
                title = Some(ai_title.to_owned());
            }
            // `<synthetic>` marks Claude's local error responses, not a selectable model.
            if let Some(found) = nonblank(record.message.as_ref().and_then(|m| m.model.as_ref()))
                && found != "<synthetic>"
            {
                model = Some(found.to_owned());
            }
            let role = match kind {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let text = content_text(record.message.as_ref().and_then(|m| m.content.as_ref()));
            if text.is_empty() {
                continue;
            }
            let at = normalize_timestamp(record.timestamp.as_deref(), &fallback);
            retain(&mut messages, &mut first_user, role, text, at, false);
            continue;
        }

        let payload_kind = payload.and_then(|p| p.kind.as_deref());
        if kind == Some("session_meta") {
            let id = nonblank(payload.and_then(|p| p.id.as_ref()))
                .or_else(|| nonblank(payload.and_then(|p| p.session_id.as_ref())));
            if !has_codex_session && let Some(id) = id {
                session = id.to_owned();
                has_codex_session = true;
            }
            continue;
        }
        if kind == Some("turn_context")
            && let Some(found) = nonblank(payload.and_then(|p| p.model.as_ref()))
        {
            model = Some(found.to_owned());
            continue;
        }
        if kind == Some("event_msg") && payload_kind == Some("user_message") {
            let text = payload.and_then(|p| p.message.clone()).unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            // Codex can write one prompt as both a response item and an event; drop
            // only the matching response copy.
            for position in (0..messages.len()).rev() {
                let message = &messages[position];
                if message.role == Role::Assistant {
                    break;
                }
                if message.response_user && message.text.trim() == text.trim() {
                    if first_user
                        .as_ref()
                        .is_some_and(|first| first.seq == message.seq)
                    {
                        first_user = None;
                    }
                    messages.remove(position);
                    break;
                }
            }
            let at = normalize_timestamp(record.timestamp.as_deref(), &fallback);
            retain(&mut messages, &mut first_user, Role::User, text, at, false);
            continue;
        }
        let role = match (kind, payload_kind, payload.and_then(|p| p.role.as_deref())) {
            (Some("response_item"), Some("message"), Some("user")) => Role::User,
            (Some("response_item"), Some("message"), Some("assistant")) => Role::Assistant,
            _ => continue,
        };
        let text = extract_text(
            payload
                .and_then(|p| p.content.as_deref())
                .unwrap_or_default(),
        );
        if text.is_empty() {
            continue;
        }
        if role == Role::User
            && (canonical.contains(&index) || matching_event_in_turn(&messages, &text))
        {
            continue;
        }
        let at = normalize_timestamp(record.timestamp.as_deref(), &fallback);
        retain(
            &mut messages,
            &mut first_user,
            role,
            text,
            at,
            role == Role::User,
        );
    }

    let first = first_user?;
    if session.trim().is_empty() {
        return None;
    }
    let kept: Vec<Kept> = if messages.iter().any(|message| message.seq == first.seq) {
        messages.into_iter().collect()
    } else {
        let skip = messages.len().saturating_sub(MAX_IMPORTED_MESSAGES - 1);
        std::iter::once(first.clone())
            .chain(messages.into_iter().skip(skip))
            .collect()
    };
    let derived = utf16_prefix(
        first.text.trim().split('\n').next().unwrap_or_default(),
        TITLE_UTF16_UNITS,
    )
    .trim()
    .to_owned();
    let title = title.unwrap_or_else(|| {
        if derived.is_empty() {
            FALLBACK_TITLE.to_owned()
        } else {
            derived
        }
    });
    Some(SessionThread {
        source: meta.source,
        instance: meta.instance.clone(),
        session,
        title,
        model,
        created_at: kept
            .first()
            .map_or_else(|| fallback.clone(), |m| m.at.clone()),
        updated_at: fallback,
        messages: kept
            .into_iter()
            .map(|message| SessionMessage {
                role: message.role,
                text: message.text,
                created_at: message.at,
            })
            .collect(),
    })
}

/// A response item can carry generated setup text beside the real prompt. Suppress
/// response users only when a shared turn ID and a verbatim event prove the prompt.
fn canonical_response_users(records: &[Record]) -> HashSet<usize> {
    let mut canonical = HashSet::new();
    let mut event_texts: HashSet<String> = HashSet::new();
    let mut response_users: Vec<(usize, String, String)> = Vec::new();
    let mut finish = |event_texts: &mut HashSet<String>,
                      response_users: &mut Vec<(usize, String, String)>| {
        let turns: HashSet<String> = response_users
            .iter()
            .filter(|(_, _, text)| event_texts.contains(text))
            .map(|(_, turn, _)| turn.clone())
            .collect();
        for (index, turn, _) in response_users.iter() {
            if turns.contains(turn) {
                canonical.insert(*index);
            }
        }
        event_texts.clear();
        response_users.clear();
    };
    for (index, record) in records.iter().enumerate() {
        let kind = record.kind.as_deref();
        let payload = record.payload.as_ref();
        let payload_kind = payload.and_then(|p| p.kind.as_deref());
        let role = payload.and_then(|p| p.role.as_deref());
        if kind == Some("response_item")
            && payload_kind == Some("message")
            && role == Some("assistant")
        {
            finish(&mut event_texts, &mut response_users);
            continue;
        }
        if kind == Some("event_msg") && payload_kind == Some("user_message") {
            let text = payload
                .and_then(|p| p.message.as_deref())
                .unwrap_or_default()
                .trim();
            if !text.is_empty() {
                event_texts.insert(text.to_owned());
            }
            continue;
        }
        if kind == Some("response_item") && payload_kind == Some("message") && role == Some("user")
        {
            let text = extract_text(
                payload
                    .and_then(|p| p.content.as_deref())
                    .unwrap_or_default(),
            );
            if let Some(turn) = codex_turn_id(payload.and_then(|p| p.passthrough.as_ref()))
                && !text.is_empty()
            {
                response_users.push((index, turn, text));
            }
        }
    }
    finish(&mut event_texts, &mut response_users);
    canonical
}

fn matching_event_in_turn(messages: &VecDeque<Kept>, text: &str) -> bool {
    let text = text.trim();
    for message in messages.iter().rev() {
        if message.role == Role::Assistant {
            return false;
        }
        if message.role == Role::User && !message.response_user && message.text.trim() == text {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub(crate) mod tests;
