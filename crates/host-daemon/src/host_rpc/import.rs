//! Best-effort, read-only discovery of native transcripts. Only visible messages enter V2.
use crate::ProjectStore;
use orchestration::{
    store::{Store, StoreError},
    *,
};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
const RAW_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const SELECTED_BUDGET: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_MESSAGES: usize = 200;
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type", default)]
    kind: String,
    timestamp: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    ai_title: Option<String>,
    #[serde(default)]
    is_sidechain: bool,
    #[serde(default)]
    is_meta: bool,
    #[serde(default)]
    is_compact_summary: bool,
    message: Option<Message>,
    payload: Option<Payload>,
}
#[derive(Clone, Default, Deserialize)]
struct Message {
    content: Option<Content>,
    model: Option<String>,
}
#[derive(Clone, Default, Deserialize)]
struct Payload {
    #[serde(rename = "type", default)]
    kind: String,
    id: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    role: Option<String>,
    message: Option<String>,
    content: Option<Content>,
    internal_chat_message_metadata_passthrough: Option<TurnMetadata>,
}
#[derive(Clone, Default, Deserialize)]
struct TurnMetadata {
    turn_id: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}
#[derive(Clone, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}
impl Content {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.trim().into(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter(|block| {
                    matches!(block.kind.as_str(), "text" | "input_text" | "output_text")
                })
                .filter_map(|block| block.text.as_deref().map(str::trim))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}
struct Transcript {
    driver: Driver,
    native: String,
    cwd: PathBuf,
    title: String,
    model: String,
    updated_at: Timestamp,
    messages: Vec<ImportedMessage>,
}
#[derive(Clone)]
struct ImportedMessage {
    role: Role,
    text: String,
    at: Timestamp,
    response_user: bool,
}
fn role(value: &str) -> Option<Role> {
    match value {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        _ => None,
    }
}
fn parse(
    records: &[Record],
    driver: Driver,
    fallback_native: &str,
    at: &Timestamp,
) -> Option<Transcript> {
    let cwd = records
        .iter()
        .find_map(|record| {
            record
                .cwd
                .as_deref()
                .or_else(|| record.payload.as_ref()?.cwd.as_deref())
        })
        .filter(|cwd| !cwd.trim().is_empty())?;
    let mut native = if driver == Driver::Claude {
        fallback_native.to_owned()
    } else {
        String::new()
    };
    let mut model = None;
    let mut title = None;
    // Suppress generated response-user setup only if a verbatim event and native turn ID prove its source.
    let mut suppressed = BTreeSet::new();
    let mut event_texts = BTreeSet::new();
    let mut response_users: Vec<(usize, String, String)> = vec![];
    let finish_turn = |events: &BTreeSet<String>,
                       users: &[(usize, String, String)],
                       suppressed: &mut BTreeSet<usize>| {
        let turns: BTreeSet<_> = users
            .iter()
            .filter(|(_, _, text)| events.contains(text))
            .map(|(_, turn, _)| turn)
            .collect();
        for (index, turn, _) in users {
            if turns.contains(turn) {
                suppressed.insert(*index);
            }
        }
    };
    if driver == Driver::Codex {
        for (index, record) in records.iter().enumerate() {
            if record.kind == "turn_context" {
                finish_turn(&event_texts, &response_users, &mut suppressed);
                event_texts.clear();
                response_users.clear();
            }
            let Some(payload) = &record.payload else {
                continue;
            };
            if record.kind == "event_msg"
                && payload.kind == "user_message"
                && let Some(text) = &payload.message
            {
                event_texts.insert(text.trim().to_owned());
            }
            if record.kind == "response_item"
                && payload.kind == "message"
                && payload.role.as_deref() == Some("user")
                && let (Some(content), Some(turn)) = (
                    &payload.content,
                    payload
                        .internal_chat_message_metadata_passthrough
                        .as_ref()
                        .and_then(|meta| meta.turn_id.as_ref()),
                )
            {
                response_users.push((index, turn.clone(), content.text()));
            }
        }
        finish_turn(&event_texts, &response_users, &mut suppressed);
    }
    let mut messages = VecDeque::<ImportedMessage>::new();
    let mut first = None;
    let mut events_in_turn = BTreeSet::<String>::new();
    for (index, record) in records.iter().enumerate() {
        if record.is_sidechain || record.is_meta || record.is_compact_summary {
            continue;
        }
        let payload = record.payload.as_ref();
        let message = if driver == Driver::Claude {
            if let Some(id) = record
                .session_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
            {
                native = id.trim().into();
            }
            if let Some(value) = record
                .ai_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
            {
                title = Some(value.trim().to_owned());
            }
            if let Some(value) = record
                .message
                .as_ref()
                .and_then(|message| message.model.as_deref())
                .filter(|model| !model.trim().is_empty() && *model != "<synthetic>")
            {
                model = Some(value.to_owned());
            }
            role(&record.kind)
                .zip(
                    record
                        .message
                        .as_ref()
                        .and_then(|message| message.content.as_ref())
                        .map(Content::text),
                )
                .map(|(role, text)| (role, text, false))
        } else {
            let Some(payload) = payload else {
                continue;
            };
            match record.kind.as_str() {
                "session_meta" => {
                    if native.is_empty() {
                        native = payload
                            .id
                            .as_ref()
                            .or(payload.session_id.as_ref())
                            .cloned()
                            .unwrap_or_default();
                    }
                    None
                }
                "turn_context" => {
                    model = payload.model.clone().or(model);
                    events_in_turn.clear();
                    None
                }
                "event_msg" if payload.kind == "user_message" => {
                    payload.message.as_ref().map(|text| {
                        events_in_turn.insert(text.trim().to_owned());
                        let after = messages
                            .iter()
                            .rposition(|message| message.role == Role::Assistant)
                            .map_or(0, |index| index + 1);
                        if let Some(index) = messages
                            .iter()
                            .enumerate()
                            .skip(after)
                            .rev()
                            .find(|(_, message)| {
                                message.response_user && message.text.trim() == text.trim()
                            })
                            .map(|(index, _)| index)
                        {
                            if first.as_ref().is_some_and(|first: &ImportedMessage| {
                                first.response_user
                                    && first.at == messages[index].at
                                    && first.text == messages[index].text
                            }) {
                                first = None;
                            }
                            messages.remove(index);
                        }
                        (Role::User, text.clone(), false)
                    })
                }
                "response_item" if payload.kind == "message" && !suppressed.contains(&index) => {
                    role(payload.role.as_deref().unwrap_or(""))
                        .zip(payload.content.as_ref().map(Content::text))
                        .filter(|(role, text)| {
                            *role != Role::User || !events_in_turn.contains(text.trim())
                        })
                        .map(|(role, text)| (role, text, role == Role::User))
                }
                _ => None,
            }
        };
        let Some((role, text, response_user)) =
            message.filter(|(_, text, _)| !text.trim().is_empty())
        else {
            continue;
        };
        let timestamp = record
            .timestamp
            .as_deref()
            .and_then(|value| Timestamp::parse(value).ok())
            .unwrap_or_else(|| at.clone());
        let message = ImportedMessage {
            role,
            text,
            at: timestamp,
            response_user,
        };
        if first.is_none() && role == Role::User {
            first = Some(message.clone());
        }
        messages.push_back(message);
        if messages.len() > MAX_MESSAGES {
            messages.pop_front();
        }
    }
    let first = first?;
    uuid::Uuid::parse_str(native.trim()).ok()?;
    if !messages.iter().any(|message| {
        message.role == Role::User && message.at == first.at && message.text == first.text
    }) {
        while messages.len() >= MAX_MESSAGES {
            messages.pop_front();
        }
        messages.push_front(first.clone());
    }
    Some(Transcript {
        driver,
        native: native.trim().into(),
        cwd: PathBuf::from(cwd),
        title: title.unwrap_or_else(|| {
            first
                .text
                .trim()
                .lines()
                .next()
                .unwrap_or("Imported thread")
                .chars()
                .take(100)
                .collect()
        }),
        model: model.unwrap_or_else(|| {
            if driver == Driver::Claude {
                "claude-fable-5-1"
            } else {
                "gpt-6-astra"
            }
            .into()
        }),
        updated_at: at.clone(),
        messages: messages.into(),
    })
}
// Oversized tool/image lines are consumed without allocating their payload.
fn read_line(
    reader: &mut impl BufRead,
    budget: &mut u64,
    limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = vec![];
    let mut oversized = false;
    let mut read = false;
    while *budget > 0 {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        read = true;
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1)
            .min(*budget as usize);
        let ended = available[count - 1] == b'\n';
        if !oversized {
            if bytes.len().saturating_add(count) > limit {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count);
        *budget -= count as u64;
        if ended {
            break;
        }
    }
    Ok(read.then_some(bytes))
}
fn scan(
    roots: Vec<(Driver, PathBuf)>,
    shutdown: &tokio::sync::watch::Receiver<bool>,
) -> Vec<Transcript> {
    let now = SystemTime::now();
    let mut result = vec![];
    for (driver, root) in roots {
        let mut pending = vec![root];
        let mut candidates = vec![];
        let mut operations = 0;
        while let Some(directory) = pending.pop() {
            if operations >= 20_000 || candidates.len() >= 5000 {
                break;
            }
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                operations += 1;
                if operations >= 20_000 {
                    break;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    continue;
                }
                if meta.is_dir() {
                    pending.push(entry.path());
                    continue;
                }
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !name.ends_with(".jsonl")
                    || (driver == Driver::Codex && !name.starts_with("rollout-"))
                {
                    continue;
                }
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if now
                    .duration_since(modified)
                    .is_ok_and(|age| age.as_secs() > 30 * 24 * 60 * 60)
                {
                    continue;
                }
                candidates.push((modified, path));
                if candidates.len() >= 5000 {
                    break;
                }
            }
        }
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut budget = RAW_BUDGET;
        for (modified, path) in candidates.into_iter().take(100) {
            if *shutdown.borrow() || shutdown.has_changed().is_err() {
                return result;
            }
            if budget == 0 {
                break;
            }
            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            let mut reader = BufReader::new(file);
            let mut records = vec![];
            let mut selected_bytes = 0;
            for _ in 0..MAX_RECORDS {
                let Ok(Some(line)) = read_line(&mut reader, &mut budget, SELECTED_BUDGET) else {
                    break;
                };
                let Ok(record) = serde_json::from_slice::<Record>(&line) else {
                    continue;
                };
                let bytes = record
                    .message
                    .as_ref()
                    .and_then(|message| message.content.as_ref())
                    .map_or(0, |content| content.text().len())
                    + record.payload.as_ref().map_or(0, |payload| {
                        payload
                            .content
                            .as_ref()
                            .map_or(0, |content| content.text().len())
                            + payload.message.as_ref().map_or(0, String::len)
                    });
                selected_bytes += bytes;
                if selected_bytes > SELECTED_BUDGET {
                    break;
                }
                records.push(record);
            }
            let millis = modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            let Ok(at) = Timestamp::from_millis(millis) else {
                continue;
            };
            if let Some(transcript) = parse(
                &records,
                driver,
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
                &at,
            ) && transcript.cwd.is_absolute()
                && transcript.cwd.is_dir()
            {
                result.push(transcript);
            }
        }
    }
    result
}
fn events(transcript: &Transcript, project_id: ProjectId) -> Vec<DomainEvent> {
    let provider = match transcript.driver {
        Driver::Codex => "codex",
        Driver::Claude => "claude",
    };
    let id = ThreadId::new(format!("import:{provider}:{}", transcript.native)).expect("derived id");
    let created_at = transcript.messages[0].at.clone();
    let provider_thread_id =
        ProviderThreadId::new(format!("provider-thread:{id}")).expect("derived id");
    let instance = ProviderInstanceId::new(provider).expect("constant id");
    let thread = AppThread {
        created_by: CreatedBy::System,
        creation_source: CreationSource::Server,
        id: id.clone(),
        project_id,
        title: transcript.title.clone(),
        provider_instance_id: instance.clone(),
        model_selection: ModelSelection {
            instance_id: instance.clone(),
            model: transcript.model.clone(),
            options: Default::default(),
        },
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: Some(transcript.cwd.to_string_lossy().into_owned()),
        active_provider_thread_id: Some(provider_thread_id.clone()),
        lineage: Lineage {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: id.clone(),
        },
        forked_from: None,
        created_at: created_at.clone(),
        updated_at: transcript.updated_at.clone(),
        archived_at: None,
        settled_override: Some(SettledOverride::Settled),
        settled_at: Some(transcript.updated_at.clone()),
        unsettled_at: None,
        snoozed_until: None,
        snoozed_at: None,
        pinned_at: None,
        auto_settle_disabled_at: None,
        pin_order_key: None,
        active_order_key: None,
        last_visited_at: None,
        deleted_at: None,
        imported: true,
    };
    let provider_thread = ProviderThread {
        id: provider_thread_id,
        driver: transcript.driver,
        provider_instance_id: instance,
        provider_session_id: None,
        app_thread_id: Some(id.clone()),
        owner_node_id: None,
        native_thread_ref: Some(ProviderRef {
            driver: transcript.driver,
            native_id: Some(transcript.native.clone()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        }),
        native_conversation_head_ref: None,
        status: ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from: None,
        pending_background_tasks: vec![],
        context_usage: None,
        native_metadata: None,
        created_at,
        updated_at: transcript.updated_at.clone(),
    };
    let event = |suffix: &str, at: &Timestamp, payload: EventPayload| DomainEvent {
        id: EventId::new(format!("agent-session-import:v2:{id}:{suffix}")).expect("derived id"),
        thread_id: id.clone(),
        occurred_at: at.clone(),
        payload,
    };
    let mut result = vec![
        event(
            "thread",
            &transcript.updated_at,
            EventPayload::ThreadCreated(thread),
        ),
        event(
            "provider-thread",
            &transcript.updated_at,
            EventPayload::ProviderThreadUpdated(provider_thread),
        ),
    ];
    for (index, source) in transcript.messages.iter().enumerate() {
        let message_id = MessageId::new(format!("{id}:{index:06}")).expect("derived id");
        let created_by = if source.role == Role::User {
            CreatedBy::User
        } else {
            CreatedBy::Agent
        };
        let message = ConversationMessage {
            created_by,
            creation_source: CreationSource::Server,
            id: message_id.clone(),
            thread_id: id.clone(),
            run_id: None,
            node_id: None,
            role: source.role,
            text: source.text.clone(),
            context: None,
            attachments: vec![],
            streaming: false,
            created_at: source.at.clone(),
            updated_at: source.at.clone(),
        };
        let body = if source.role == Role::User {
            TurnItemBody::UserMessage {
                created_by,
                creation_source: CreationSource::Server,
                message_id,
                input_intent: InputIntent::TurnStart,
                text: source.text.clone(),
                context: None,
                attachments: vec![],
            }
        } else {
            TurnItemBody::AssistantMessage {
                message_id,
                text: source.text.clone(),
                attachments: vec![],
                streaming: false,
            }
        };
        let item = TurnItem {
            id: TurnItemId::new(format!("agent-session-import:v2:turn-item:{id}:{index:06}"))
                .expect("derived id"),
            thread_id: id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal: index as u64 + 1,
            status: ItemStatus::Completed,
            title: None,
            started_at: Some(source.at.clone()),
            completed_at: Some(source.at.clone()),
            updated_at: source.at.clone(),
            body,
        };
        result.extend([
            event(
                &format!("message:{index:06}"),
                &source.at,
                EventPayload::MessageUpdated(message),
            ),
            event(
                &format!("turn-item:{index:06}"),
                &source.at,
                EventPayload::TurnItemUpdated(item),
            ),
        ]);
    }
    result
}
pub(super) async fn run(
    store: Arc<Store>,
    projects: ProjectStore,
    roots: Vec<(Driver, PathBuf)>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let scan_shutdown = shutdown.clone();
    let Ok(transcripts) = tokio::task::spawn_blocking(move || scan(roots, &scan_shutdown)).await
    else {
        return;
    };
    for transcript in transcripts {
        if *shutdown.borrow() || shutdown.has_changed().is_err() {
            return;
        }
        let provider = match transcript.driver {
            Driver::Codex => "codex",
            Driver::Claude => "claude",
        };
        let thread_id =
            ThreadId::new(format!("import:{provider}:{}", transcript.native)).expect("derived id");
        match store.projection(&thread_id) {
            Ok(_) => continue,
            Err(StoreError::ThreadNotFound) => {}
            Err(_) => continue,
        };
        let Ok(mut snapshot) = projects.load().await else {
            continue;
        };
        if snapshot
            .project_for_workspace(&transcript.cwd.to_string_lossy())
            .is_none()
        {
            if projects.register(&transcript.cwd).await.is_err() {
                continue;
            }
            let Ok(new_snapshot) = projects.load().await else {
                continue;
            };
            snapshot = new_snapshot;
        }
        let Some(project) = snapshot.project_for_workspace(&transcript.cwd.to_string_lossy())
        else {
            continue;
        };
        let Ok(project_id) = ProjectId::new(project) else {
            continue;
        };
        if let Err(error) = store.ingest(
            events(&transcript, project_id),
            None,
            &transcript.updated_at,
        ) {
            tracing::warn!(operation="orchestration.native-import",message=%error);
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn at() -> Timestamp {
        Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
    }
    const NATIVE: &str = "019a1fe1-4ea6-7000-8000-000000000001";
    fn records(values: Vec<serde_json::Value>) -> Vec<Record> {
        values
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect()
    }
    #[test]
    fn codex_import_deduplicates_proven_prompt_copies_and_resumes_native_identity() {
        let source = records(vec![
            json!({"type":"session_meta","payload":{"id":NATIVE,"cwd":"/tmp"}}),
            json!({"type":"turn_context","payload":{"model":"test-model"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"generated setup"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real prompt"}],"internal_chat_message_metadata_passthrough":{"turn_id":"t1"}}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"real prompt"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}),
        ]);
        let transcript = parse(&source, Driver::Codex, "ignored", &at()).unwrap();
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["real prompt", "answer"]
        );
        let store = Store::memory().unwrap();
        let events = events(&transcript, ProjectId::new("project").unwrap());
        let thread_id = events[0].thread_id.clone();
        store.ingest(events, None, &at()).unwrap();
        let projection = store.projection(&thread_id).unwrap();
        assert!(projection.thread.imported);
        assert_eq!(projection.visible_turn_items.len(), 2);
        assert!(projection.runs.is_empty());
        assert_eq!(
            projection.provider_threads[0]
                .native_thread_ref
                .as_ref()
                .unwrap()
                .native_id
                .as_deref(),
            Some(NATIVE)
        );
        let input = Command {
            command_id: CommandId::new("continue").unwrap(),
            thread_id: thread_id.clone(),
            body: CommandBody::MessageDispatch(MessageDispatch {
                created_by: CreatedBy::User,
                creation_source: CreationSource::Desktop,
                message_id: MessageId::new("next").unwrap(),
                text: "continue".into(),
                context: None,
                attachments: vec![],
                model_selection: None,
                delivery_intent: None,
                dispatch_mode: DispatchMode::StartImmediately,
            }),
        };
        store
            .dispatch(
                &input,
                &at(),
                &provider_adapters::capabilities::capabilities(Driver::Codex).turns,
                Driver::Codex,
            )
            .unwrap();
        let next = store.projection(&thread_id).unwrap();
        assert_eq!(next.provider_threads.len(), 1);
        assert_eq!(
            next.runs[0].provider_thread_id.as_ref(),
            Some(&projection.provider_threads[0].id)
        );
    }
    #[test]
    fn claude_import_excludes_sidechains_tools_meta_and_synthetic_model() {
        let source = records(vec![
            json!({"type":"user","cwd":"/tmp","sessionId":NATIVE,"message":{"content":"first"}}),
            json!({"type":"user","isMeta":true,"message":{"content":"meta"}}),
            json!({"type":"assistant","isSidechain":true,"message":{"content":"child"}}),
            json!({"type":"assistant","message":{"model":"test-model","content":[{"type":"text","text":"answer"},{"type":"tool_use","text":"omit"}]}}),
            json!({"type":"assistant","message":{"model":"<synthetic>","content":[]}}),
        ]);
        let transcript = parse(&source, Driver::Claude, NATIVE, &at()).unwrap();
        assert_eq!(transcript.model, "test-model");
        assert_eq!(transcript.messages.len(), 2);
        assert_eq!(transcript.title, "first");
    }
    #[test]
    fn bounded_history_keeps_first_user_and_latest_messages() {
        let values=(0..240).map(|index|json!({"type":"user","cwd":"/tmp","sessionId":NATIVE,"message":{"content":format!("message {index}")}})).collect();
        let transcript = parse(&records(values), Driver::Claude, NATIVE, &at()).unwrap();
        assert_eq!(transcript.messages.len(), 200);
        assert_eq!(transcript.messages[0].text, "message 0");
        assert_eq!(transcript.messages[199].text, "message 239");
    }
    #[test]
    fn oversized_lines_are_skipped_without_stopping_later_records() {
        let mut input = io::Cursor::new(b"oversized payload\nok\n");
        let mut budget = 100;
        assert!(
            read_line(&mut input, &mut budget, 5)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_line(&mut input, &mut budget, 5).unwrap().unwrap(),
            b"ok\n"
        );
        assert!(read_line(&mut input, &mut budget, 5).unwrap().is_none());
    }
}
