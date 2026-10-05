//! `forkSession` of claude-agent-sdk 0.3.276 as a pure transcript transform.
//! The caller resolves the project directory, reads the source transcript
//! and writes the result to `<project dir>/<session_id>.jsonl` with mode 0600.
use crate::*;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The project directory name for a working directory, after the caller has
/// resolved its real path (NFC-normalized on macOS).
pub fn claude_project_key(dir: &str) -> String {
    let key = dir
        .encode_utf16()
        .map(|unit| {
            if unit < 128 && (unit as u8).is_ascii_alphanumeric() {
                unit as u8 as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    if key.len() <= 200 {
        return key;
    }
    let hash = dir
        .encode_utf16()
        .fold(0i32, |hash, unit| {
            hash.wrapping_shl(5)
                .wrapping_sub(hash)
                .wrapping_add(unit as i32)
        })
        .unsigned_abs();
    format!("{}-{}", &key[..200], radix36(hash as u64))
}
fn radix36(mut value: u64) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut digits = vec![];
    while value > 0 {
        digits.push(char::from_digit((value % 36) as u32, 36).unwrap());
        value /= 36;
    }
    digits.iter().rev().collect()
}
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeForkedSession {
    pub session_id: String,
    /// JSON lines, each terminated by a newline.
    pub transcript: String,
}
#[derive(Default)]
struct Parsed {
    transcript: Vec<Value>,
    replacements: Vec<Value>,
    relocated: Option<String>,
    suppressed: bool,
    latch: Option<String>,
}
fn parse(source: &str, session: &str) -> Parsed {
    let mut parsed = Parsed::default();
    for line in source.split('\n') {
        let Ok(entry) = serde_json::from_str::<Value>(line.trim_start()) else {
            continue;
        };
        let kind = entry["type"].as_str().unwrap_or_default();
        let own = entry["sessionId"] == session;
        if matches!(
            kind,
            "user" | "assistant" | "attachment" | "system" | "progress"
        ) && entry["uuid"].is_string()
        {
            parsed.transcript.push(entry);
        } else if kind == "history-suppression" {
            parsed.suppressed = true;
        } else if kind == "atis-latch"
            && own
            && entry["atis"]
                .as_str()
                .is_some_and(|atis| atis.bytes().all(|b| (0x21..=0x7e).contains(&b)))
        {
            parsed.latch = entry["atis"].as_str().map(str::to_owned);
        } else if kind == "content-replacement" && own && entry["replacements"].is_array() {
            parsed
                .replacements
                .extend(entry["replacements"].as_array().unwrap().iter().cloned());
        } else if kind == "relocated"
            && own
            && entry["relocatedCwd"]
                .as_str()
                .is_some_and(|cwd| !cwd.is_empty())
        {
            parsed.relocated = entry["relocatedCwd"].as_str().map(str::to_owned);
        }
    }
    parsed
}
fn uuid(entry: &Value) -> &str {
    entry["uuid"].as_str().unwrap_or_default()
}
fn parent(entry: &Value) -> Option<&str> {
    entry["parentUuid"]
        .as_str()
        .filter(|parent| !parent.is_empty())
}
fn queued_source(entry: &Value) -> Option<&str> {
    let attachment = &entry["attachment"];
    (entry["type"] == "attachment" && attachment["type"] == "queued_command")
        .then(|| attachment["source_uuid"].as_str())
        .flatten()
        .filter(|source| !source.is_empty())
}
fn conversational(entry: &Value) -> bool {
    matches!(entry["type"].as_str(), Some("user" | "assistant"))
        && entry["isMeta"] != true
        && !truthy(&entry["teamName"])
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}
/// The conversation's main chain from root to the latest leaf, with
/// compaction relinks and split assistant messages.
fn main_chain(entries: &[Value]) -> Vec<Value> {
    let mut order = vec![];
    let mut by_uuid = BTreeMap::<String, Value>::new();
    for entry in entries {
        if !by_uuid.contains_key(uuid(entry)) {
            order.push(uuid(entry).to_owned());
        }
        by_uuid.insert(uuid(entry).to_owned(), entry.clone());
    }
    let set_parent = |map: &mut BTreeMap<String, Value>, id: &str, parent: &Value| {
        if let Some(entry) = map.get_mut(id) {
            entry["parentUuid"] = parent.clone();
        }
    };
    for id in order.clone() {
        let entry = by_uuid[&id].clone();
        if entry["type"] != "system" || entry["subtype"] != "compact_boundary" {
            continue;
        }
        let metadata = &entry["compactMetadata"];
        let preserved = &metadata["preservedMessages"];
        let segment = &metadata["preservedSegment"];
        if preserved.is_object() {
            let uuids = preserved["uuids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if uuids.is_empty() || uuids.iter().any(|id| !by_uuid.contains_key(id)) {
                continue;
            }
            let anchor = preserved["anchorUuid"].clone();
            let mut previous = anchor.clone();
            for id in &uuids {
                set_parent(&mut by_uuid, id, &previous);
                previous = json!(id);
            }
            let (first, last) = (uuids[0].clone(), uuids.last().unwrap().clone());
            for id in order.clone() {
                if by_uuid[&id]["parentUuid"] == anchor && id != first {
                    set_parent(&mut by_uuid, &id, &json!(last));
                }
            }
        } else if segment.is_object() {
            let head = segment["headUuid"].as_str().unwrap_or_default().to_owned();
            set_parent(&mut by_uuid, &head, &segment["anchorUuid"]);
            for id in order.clone() {
                if by_uuid[&id]["parentUuid"] == segment["anchorUuid"] && id != head {
                    set_parent(&mut by_uuid, &id, &segment["tailUuid"]);
                }
            }
        }
    }
    let index = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (uuid(entry).to_owned(), i))
        .collect::<BTreeMap<_, _>>();
    let parents = by_uuid
        .values()
        .filter_map(|entry| parent(entry).map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let mut leaves = vec![];
    for id in order.iter().filter(|id| !parents.contains(*id)) {
        let mut seen = BTreeSet::new();
        let mut current = by_uuid.get(id);
        while let Some(entry) = current {
            if !seen.insert(uuid(entry).to_owned()) {
                break;
            }
            if matches!(entry["type"].as_str(), Some("user" | "assistant")) {
                leaves.push(entry.clone());
                break;
            }
            current = parent(entry).and_then(|parent| by_uuid.get(parent));
        }
    }
    if leaves.is_empty() {
        return vec![];
    }
    let latest = |candidates: Vec<&Value>| {
        candidates
            .into_iter()
            .reduce(|best, entry| {
                if index.get(uuid(entry)).copied().map_or(-1, |i| i as i64)
                    > index.get(uuid(best)).copied().map_or(-1, |i| i as i64)
                {
                    entry
                } else {
                    best
                }
            })
            .cloned()
    };
    let main = leaves
        .iter()
        .filter(|entry| {
            entry["isSidechain"] != true && !truthy(&entry["teamName"]) && entry["isMeta"] != true
        })
        .collect::<Vec<_>>();
    let leaf = if main.is_empty() {
        latest(leaves.iter().collect())
    } else {
        latest(main)
    }
    .unwrap();
    let mut chain = vec![];
    let mut seen = BTreeSet::new();
    let mut current = by_uuid.get(uuid(&leaf));
    while let Some(entry) = current {
        if !seen.insert(uuid(entry).to_owned()) {
            break;
        }
        chain.push(entry.clone());
        current = parent(entry).and_then(|parent| by_uuid.get(parent));
    }
    chain.reverse();
    split_assistants(&order, &by_uuid, chain, seen)
}
fn message_id(entry: &Value) -> Option<&str> {
    (entry["type"] == "assistant")
        .then(|| entry["message"]["id"].as_str())
        .flatten()
}
fn tool_result_user(entry: &Value) -> bool {
    entry["type"] == "user"
        && parent(entry).is_some()
        && entry["message"]["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|block| block["type"] == "tool_result"))
}
fn split_assistants(
    order: &[String],
    by_uuid: &BTreeMap<String, Value>,
    chain: Vec<Value>,
    mut included: BTreeSet<String>,
) -> Vec<Value> {
    let assistants = chain
        .iter()
        .filter(|entry| entry["type"] == "assistant")
        .collect::<Vec<_>>();
    if assistants.is_empty() {
        return chain;
    }
    let mut anchor = BTreeMap::<String, String>::new();
    for entry in &assistants {
        if let Some(id) = message_id(entry) {
            anchor.insert(id.into(), uuid(entry).into());
        }
    }
    let mut by_message = BTreeMap::<String, Vec<&Value>>::new();
    let mut results = BTreeMap::<String, Vec<&Value>>::new();
    for id in order {
        let entry = &by_uuid[id];
        if let Some(message) = message_id(entry) {
            by_message.entry(message.into()).or_default().push(entry);
        } else if tool_result_user(entry) {
            results
                .entry(parent(entry).unwrap().into())
                .or_default()
                .push(entry);
        }
    }
    let mut seen = BTreeSet::new();
    let mut inserts = BTreeMap::<String, Vec<Value>>::new();
    for entry in assistants {
        let Some(message) = message_id(entry) else {
            continue;
        };
        if !seen.insert(message.to_owned()) {
            continue;
        }
        let parts = by_message
            .get(message)
            .cloned()
            .unwrap_or_else(|| vec![entry]);
        let mut siblings = parts
            .iter()
            .filter(|part| !included.contains(uuid(part)))
            .map(|part| (*part).clone())
            .collect::<Vec<_>>();
        let mut answers = parts
            .iter()
            .flat_map(|part| results.get(uuid(part)).cloned().unwrap_or_default())
            .filter(|result| !included.contains(uuid(result)))
            .cloned()
            .collect::<Vec<_>>();
        if siblings.is_empty() && answers.is_empty() {
            continue;
        }
        let by_time = |a: &Value, b: &Value| {
            a["timestamp"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["timestamp"].as_str().unwrap_or_default())
        };
        siblings.sort_by(by_time);
        answers.sort_by(by_time);
        siblings.extend(answers);
        for added in &siblings {
            included.insert(uuid(added).to_owned());
        }
        inserts.insert(anchor[message].clone(), siblings);
    }
    if inserts.is_empty() {
        return chain;
    }
    let mut result = vec![];
    for entry in chain {
        let added = inserts.remove(uuid(&entry));
        result.push(entry);
        result.extend(added.into_iter().flatten());
    }
    result
}
/// Drops conversational branches after the boundary that are off the main chain.
fn prune_branches(
    entries: Vec<Value>,
    chain: &[Value],
    index: &BTreeMap<String, usize>,
) -> Vec<Value> {
    let Some(last) = entries.last() else {
        return entries;
    };
    let Some(start) = chain
        .first()
        .and_then(|first| index.get(uuid(first)).copied())
    else {
        return entries;
    };
    if conversational(last) {
        return entries;
    }
    let by_uuid = entries
        .iter()
        .map(|entry| (uuid(entry).to_owned(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut on_path = chain
        .iter()
        .map(|entry| uuid(entry).to_owned())
        .collect::<BTreeSet<_>>();
    let mut current = Some(last);
    while let Some(entry) = current.filter(|entry| !on_path.contains(uuid(entry))) {
        on_path.insert(uuid(entry).to_owned());
        current = parent(entry).and_then(|parent| by_uuid.get(parent).copied());
    }
    let after = entries
        .iter()
        .rposition(|entry| conversational(entry) && on_path.contains(uuid(entry)))
        .map_or(0, |i| i + 1);
    let mut dropped = BTreeSet::new();
    for entry in &entries[after..] {
        if !conversational(entry) || on_path.contains(uuid(entry)) {
            continue;
        }
        let mut current = Some(entry);
        while let Some(node) = current.filter(|node| {
            !on_path.contains(uuid(node))
                && !dropped.contains(uuid(node))
                && index.get(uuid(node)).copied().unwrap_or(start) >= start
        }) {
            dropped.insert(uuid(node).to_owned());
            current = parent(node).and_then(|parent| by_uuid.get(parent).copied());
        }
    }
    if dropped.is_empty() {
        return entries;
    }
    for entry in &entries {
        if parent(entry).is_some_and(|parent| dropped.contains(parent)) {
            dropped.insert(uuid(entry).to_owned());
        }
    }
    entries
        .into_iter()
        .filter(|entry| !dropped.contains(uuid(entry)))
        .collect()
}
/// The last `"field":"value"` string in raw transcript text.
fn field_text(text: &str, field: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut best: Option<(usize, String)> = None;
    for prefix in [format!("\"{field}\":\""), format!("\"{field}\": \"")] {
        let mut from = 0;
        while let Some(offset) = text[from..].find(&prefix) {
            let at = from + offset;
            let start = at + prefix.len();
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            if end >= bytes.len() {
                break;
            }
            if best.as_ref().is_none_or(|(best, _)| at > *best) {
                let raw = &text[start..end];
                let value = if raw.contains('\\') {
                    serde_json::from_str::<String>(&format!("\"{raw}\""))
                        .unwrap_or_else(|_| raw.to_owned())
                } else {
                    raw.to_owned()
                };
                best = Some((at, value));
            }
            from = end + 1;
        }
    }
    best.map(|(_, value)| value)
}
/// The first prompt of the transcript, as the session list shows it.
fn first_prompt(text: &str) -> Option<String> {
    let mut command = None;
    for line in text.split('\n') {
        if !(line.contains("\"type\":\"user\"") || line.contains("\"type\": \"user\""))
            || line.contains("\"tool_result\"")
        {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] != "user" || entry["isMeta"] == true || entry["isCompactSummary"] == true {
            continue;
        }
        let content = &entry["message"]["content"];
        let texts = match content {
            Value::String(text) => vec![text.clone()],
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str().map(str::to_owned))
                .collect(),
            _ => vec![],
        };
        for text in texts {
            let text = text.replace('\n', " ").trim().to_owned();
            if text.is_empty() {
                continue;
            }
            if let Some(start) = text.find("<command-name>")
                && let Some(end) = text[start + 14..].find("</command-name>")
            {
                command.get_or_insert_with(|| text[start + 14..start + 14 + end].to_owned());
                continue;
            }
            if let Some(start) = text.find("<bash-input>")
                && let Some(end) = text[start + 12..].find("</bash-input>")
            {
                return Some(format!("! {}", text[start + 12..start + 12 + end].trim()));
            }
            let markup = text.trim_start().starts_with('<')
                && text
                    .trim_start()
                    .chars()
                    .nth(1)
                    .is_some_and(|c| c.is_ascii_lowercase())
                || text.starts_with("[Request interrupted by user");
            if markup {
                continue;
            }
            let units = text.encode_utf16().collect::<Vec<_>>();
            return Some(if units.len() > 200 {
                let mut end = 200;
                if (0xD800..=0xDBFF).contains(&units[199]) {
                    end = 199;
                }
                format!("{}…", String::from_utf16_lossy(&units[..end]).trim())
            } else {
                text
            });
        }
    }
    command
}
fn derived_title(source: &str) -> Option<String> {
    const WINDOW: usize = 65_536;
    let floor = |mut at: usize| {
        while !source.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let head = &source[..floor(source.len().min(WINDOW))];
    let tail = &source[floor(source.len().saturating_sub(WINDOW))..];
    let nonempty = |value: Option<String>| value.filter(|value| !value.is_empty());
    match field_text(tail, "customTitle") {
        Some(title) => nonempty(Some(title)),
        None => nonempty(field_text(head, "customTitle")),
    }
    .or_else(|| nonempty(field_text(tail, "aiTitle")))
    .or_else(|| nonempty(field_text(head, "aiTitle")))
    .or_else(|| nonempty(first_prompt(head)))
}
/// Copies the source session through `up_to` into a new session.
pub fn claude_fork_session(
    source: &str,
    session: &str,
    up_to: Option<&str>,
    title: Option<&str>,
    mut new_uuid: impl FnMut() -> String,
    now: &str,
) -> Result<ClaudeForkedSession, ProtocolError> {
    let parsed = parse(source, session);
    let no_messages =
        || ProtocolError::Invalid(format!("Session {session} has no messages to fork"));
    let mut entries = parsed
        .transcript
        .iter()
        .filter(|entry| entry["isSidechain"] != true)
        .cloned()
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err(no_messages());
    }
    if let Some(up_to) = up_to {
        let chain = main_chain(&entries);
        let mut index = BTreeMap::new();
        for (i, entry) in entries.iter().enumerate() {
            index.entry(uuid(entry).to_owned()).or_insert(i);
        }
        let end = chain
            .iter()
            .filter_map(|entry| index.get(uuid(entry)).copied())
            .max()
            .map_or(0, |i| i + 1);
        let path = chain
            .iter()
            .chain(&entries[end.min(entries.len())..])
            .collect::<Vec<_>>();
        let find = |candidates: &[&Value]| {
            candidates
                .iter()
                .position(|entry| uuid(entry) == up_to)
                .or_else(|| {
                    candidates
                        .iter()
                        .position(|entry| queued_source(entry) == Some(up_to))
                })
        };
        let boundary = match find(&path) {
            Some(found) => index.get(uuid(path[found])).copied(),
            None => find(&entries.iter().collect::<Vec<_>>()),
        }
        .ok_or_else(|| {
            ProtocolError::MissingBoundary(format!(
                "Message {up_to} not found in session {session}"
            ))
        })?;
        entries = prune_branches(entries[..=boundary].to_vec(), &chain, &index);
    }
    let mapping = entries
        .iter()
        .map(|entry| (uuid(entry).to_owned(), new_uuid()))
        .collect::<BTreeMap<_, _>>();
    let by_uuid = entries
        .iter()
        .map(|entry| (uuid(entry).to_owned(), entry))
        .collect::<BTreeMap<_, _>>();
    let kept = entries
        .iter()
        .filter(|entry| entry["type"] != "progress")
        .collect::<Vec<_>>();
    if kept.is_empty() {
        return Err(no_messages());
    }
    let session_id = new_uuid();
    let mut output = vec![];
    if parsed.suppressed {
        output.push(json!({"type":"history-suppression","sessionId":session_id,"cause":"fork_inherit","ts":now}));
    }
    for (position, entry) in kept.iter().enumerate() {
        let mut parent_uuid = Value::Null;
        let mut current = parent(entry);
        let mut visited = BTreeSet::new();
        while let Some(id) = current {
            let Some(node) = by_uuid.get(id) else {
                break;
            };
            if node["type"] != "progress" {
                parent_uuid = mapping.get(id).map_or(Value::Null, |id| json!(id));
                break;
            }
            if !visited.insert(id.to_owned()) {
                parent_uuid = mapping.get(id).map_or(Value::Null, |id| json!(id));
                break;
            }
            current = parent(node);
        }
        let mut fork = entry.as_object().cloned().unwrap_or_else(Map::new);
        if entry["type"] == "system" && entry["subtype"] == "model_refusal_fallback" {
            fork.insert("neutralizedByFork".into(), json!(true));
        }
        if entry["type"] == "attachment"
            && entry["attachment"]["type"] == "deferred_tools_record"
            && let Some(names) = entry["attachment"]["nameOnlyAnnouncements"].as_array()
        {
            let mut attachment = entry["attachment"].clone();
            attachment["nameOnlyAnnouncements"] = json!(
                names
                    .iter()
                    .filter_map(|name| name.as_str().and_then(|name| mapping.get(name)))
                    .collect::<Vec<_>>()
            );
            fork.insert("attachment".into(), attachment);
        }
        if let Some(source) = queued_source(entry).and_then(|source| mapping.get(source)) {
            let mut attachment = fork["attachment"].clone();
            attachment["source_uuid"] = json!(source);
            fork.insert("attachment".into(), attachment);
        }
        fork.insert("uuid".into(), json!(mapping[uuid(entry)]));
        fork.insert("parentUuid".into(), parent_uuid);
        match entry.get("logicalParentUuid") {
            None => {}
            Some(Value::Null) => {
                fork.insert("logicalParentUuid".into(), Value::Null);
            }
            Some(logical) => {
                fork.insert(
                    "logicalParentUuid".into(),
                    logical
                        .as_str()
                        .and_then(|id| mapping.get(id))
                        .map_or(Value::Null, |id| json!(id)),
                );
            }
        }
        fork.insert("sessionId".into(), json!(session_id));
        if position + 1 == kept.len() {
            fork.insert("timestamp".into(), json!(now));
        }
        fork.insert("isSidechain".into(), json!(false));
        for key in [
            "teamName",
            "agentName",
            "sessionKind",
            "slug",
            "sourceToolAssistantUUID",
        ] {
            fork.remove(key);
        }
        fork.insert(
            "forkedFrom".into(),
            json!({"sessionId":session,"messageUuid":uuid(entry)}),
        );
        output.push(Value::Object(fork));
    }
    if !parsed.replacements.is_empty() {
        output.push(json!({"type":"content-replacement","sessionId":session_id,"replacements":parsed.replacements,"uuid":new_uuid(),"timestamp":now}));
    }
    if let Some(latch) = parsed.latch {
        output.push(json!({"type":"atis-latch","sessionId":session_id,"atis":latch}));
    }
    if let Some(cwd) = parsed.relocated {
        output.push(json!({"type":"relocated","sessionId":session_id,"relocatedCwd":cwd}));
    }
    let title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{} (fork)",
                derived_title(source).unwrap_or_else(|| "Forked session".into())
            )
        });
    output.push(json!({"type":"custom-title","sessionId":session_id,"customTitle":title,"uuid":new_uuid(),"timestamp":now}));
    let mut transcript = String::new();
    for entry in output {
        transcript.push_str(&entry.to_string());
        transcript.push('\n');
    }
    Ok(ClaudeForkedSession {
        session_id,
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(value: Value) -> String {
        value.to_string()
    }
    fn source() -> String {
        [
            line(json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s","timestamp":"t1","message":{"role":"user","content":"hello fork"}})),
            line(json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s","timestamp":"t2","slug":"x","message":{"id":"m1","content":[{"type":"text","text":"one"}]}})),
            line(json!({"type":"progress","uuid":"p1","parentUuid":"a1","sessionId":"s","timestamp":"t3"})),
            line(json!({"type":"user","uuid":"u2","parentUuid":"p1","sessionId":"s","timestamp":"t4","message":{"role":"user","content":"second"}})),
            line(json!({"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"s","timestamp":"t5","message":{"id":"m2","content":[{"type":"text","text":"two"}]}})),
            line(json!({"type":"summary","summary":"ignored"})),
            "not json".into(),
        ]
        .join("\n")
    }
    fn ids() -> impl FnMut() -> String {
        let mut next = 0;
        move || {
            next += 1;
            format!("new-{next}")
        }
    }
    fn entries(fork: &ClaudeForkedSession) -> Vec<Value> {
        fork.transcript
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    #[test]
    fn a_fork_copies_the_conversation_through_the_boundary_into_a_new_session() {
        let fork = claude_fork_session(&source(), "s", Some("a1"), None, ids(), "now").unwrap();
        assert!(fork.transcript.ends_with('\n'));
        let entries = entries(&fork);
        assert_eq!(fork.session_id, "new-3");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["uuid"], "new-1");
        assert_eq!(entries[0]["sessionId"], "new-3");
        assert_eq!(entries[0]["timestamp"], "t1");
        assert_eq!(
            entries[0]["forkedFrom"],
            json!({"sessionId":"s","messageUuid":"u1"})
        );
        assert_eq!(entries[1]["parentUuid"], "new-1");
        assert_eq!(entries[1]["timestamp"], "now");
        assert!(entries[1].get("slug").is_none());
        assert_eq!(entries[1]["isSidechain"], false);
        assert_eq!(
            entries[2],
            json!({"type":"custom-title","sessionId":"new-3","customTitle":"hello fork (fork)","uuid":"new-4","timestamp":"now"})
        );
    }
    #[test]
    fn a_full_fork_skips_progress_links_and_rejects_missing_boundaries() {
        let fork =
            claude_fork_session(&source(), "s", None, Some(" Named "), ids(), "now").unwrap();
        let entries = entries(&fork);
        let u2 = entries
            .iter()
            .find(|entry| entry["forkedFrom"]["messageUuid"] == "u2")
            .unwrap();
        let a1 = entries
            .iter()
            .find(|entry| entry["forkedFrom"]["messageUuid"] == "a1")
            .unwrap();
        assert_eq!(u2["parentUuid"], a1["uuid"]);
        assert!(!entries.iter().any(|entry| entry["type"] == "progress"));
        assert_eq!(entries.last().unwrap()["customTitle"], "Named");
        assert!(matches!(
            claude_fork_session(&source(), "s", Some("missing"), None, ids(), "now"),
            Err(ProtocolError::MissingBoundary(message)) if message == "Message missing not found in session s"
        ));
        assert!(matches!(
            claude_fork_session("", "s", None, None, ids(), "now"),
            Err(ProtocolError::Invalid(message)) if message == "Session s has no messages to fork"
        ));
    }
    #[test]
    fn project_keys_replace_non_alphanumerics_and_hash_long_paths() {
        assert_eq!(
            claude_project_key("/tmp/claude-replay"),
            "-tmp-claude-replay"
        );
        let long = format!("/{}", "a".repeat(250));
        let key = claude_project_key(&long);
        assert!(key.starts_with(&format!("-{}", "a".repeat(199))));
        assert!(key.len() > 201);
        assert_eq!(&key[200..201], "-");
    }
}
