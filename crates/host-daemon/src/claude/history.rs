//! Read-only access to Claude's native transcript tree. Never repairs or writes
//! transcripts, and never launches the CLI to list or display a conversation.
use agent_protocol::{
    execution::*,
    ids::ItemId,
    items::*,
    models::{Item, Thread, ThreadResponse, Turn},
    session::{ProviderKind, SessionRef},
};
use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
use uuid::Uuid;

pub(super) const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILES: usize = 20_000;

pub(super) fn home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return dunce::canonicalize(path).context("Claude storage is unavailable");
    }
    directories::BaseDirs::new()
        .map(|base| base.home_dir().join(".claude"))
        .context("Claude home directory is unavailable")
}

pub(super) fn files(home: &Path) -> Result<Vec<PathBuf>> {
    let projects = match fs::read_dir(home.join("projects")) {
        Ok(projects) => projects,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(anyhow::Error::new(error).context("Claude projects cannot be read"));
        }
    };
    let mut files = Vec::new();
    for project in projects {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(project.path())? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && path.extension().is_some_and(|ext| ext == "jsonl")
                && path
                    .file_stem()
                    .and_then(|id| id.to_str())
                    .is_some_and(|id| Uuid::parse_str(id).is_ok())
            {
                files.push(path);
                if files.len() > MAX_FILES {
                    return Err(anyhow!(
                        "Claude listing exceeds its scan limit; results are incomplete"
                    ));
                }
            }
        }
    }
    Ok(files)
}

pub(super) fn resolve(home: &Path, id: Uuid) -> Result<PathBuf> {
    let filename = format!("{id}.jsonl");
    let mut matches = files(home)?.into_iter().filter(|path| {
        path.file_name()
            .is_some_and(|name| name == filename.as_str())
    });
    let path = matches
        .next()
        .context("Claude native transcript was not found")?;
    if matches.next().is_some() {
        return Err(anyhow!(
            "Claude native session ID is ambiguous across projects"
        ));
    }
    Ok(path)
}

/// Extract metadata without loading complete tool output or message bodies.
pub(super) fn summary(path: &Path) -> Result<Thread> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut head = Vec::new();
    (&mut file).take(128 * 1024).read_to_end(&mut head)?;
    let mut tail = Vec::new();
    let offset = metadata.len().saturating_sub(128 * 1024);
    file.seek(SeekFrom::Start(offset))?;
    file.take(128 * 1024).read_to_end(&mut tail)?;
    let id = path
        .file_stem()
        .and_then(|id| id.to_str())
        .context("invalid native transcript filename")?;
    let mut thread = Thread {
        id: Some(SessionRef {
            provider: ProviderKind::Claude,
            id: id.into(),
        }),
        path: Some(path.to_string_lossy().into()),
        updated_at: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_secs() as f64),
        status: agent_protocol::models::SessionStatus::Unknown,
        ..Default::default()
    };
    for line in head.split(|byte| *byte == b'\n').chain(
        tail.split(|byte| *byte == b'\n')
            .skip(usize::from(offset > 0)),
    ) {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(session) = value["sessionId"].as_str().or(value["session_id"].as_str())
            && session != id
        {
            return Err(anyhow!(
                "Claude transcript session identity does not match its filename"
            ));
        }
        if let Some(cwd) = value["cwd"].as_str() {
            thread.cwd = Some(cwd.into());
        }
        if let Some(title) = value["customTitle"].as_str().or(value["aiTitle"].as_str()) {
            thread.name = Some(title.into());
        }
        if thread.preview.is_none() && value["type"] == "user" && value["isMeta"] != true {
            thread.preview = input_text(&value["message"]["content"]);
        }
    }
    Ok(thread)
}

fn input_text(content: &Value) -> Option<String> {
    let text = if let Some(text) = content.as_str() {
        text.to_owned()
    } else {
        content
            .as_array()?
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    (!text.is_empty()).then(|| text.chars().take(160).collect())
}

fn input_blocks(blocks: &[Value]) -> Vec<MessagePart> {
    blocks
        .iter()
        .filter_map(|block| match block["type"].as_str() {
            Some("text") => Some(MessagePart::Text {
                text: block["text"].as_str()?.into(),
            }),
            Some("image") => {
                let source = &block["source"];
                let source = match source["type"].as_str() {
                    Some("base64") => format!(
                        "data:{};base64,{}",
                        source["media_type"].as_str()?,
                        source["data"].as_str()?
                    ),
                    Some("url") => source["url"].as_str()?.into(),
                    _ => return None,
                };
                Some(MessagePart::Image { source })
            }
            _ => None,
        })
        .collect()
}

pub(super) struct NativeItemDetails {
    pub output_path: Option<String>,
}
pub(super) struct NativeHistory {
    pub response: ThreadResponse,
    pub details: BTreeMap<ItemId, NativeItemDetails>,
}
pub(super) fn read_details(path: &Path, limit: usize) -> Result<NativeHistory> {
    read_with_summary(path, summary(path)?, limit)
}

pub(super) fn read(path: &Path, limit: usize) -> Result<ThreadResponse> {
    read_with_summary(path, summary(path)?, limit).map(|history| history.response)
}

pub(super) fn read_related(
    home: &Path,
    session_id: Uuid,
    agent_id: &str,
    limit: usize,
) -> Result<ThreadResponse> {
    if agent_id.is_empty()
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(anyhow!("invalid native subagent ID"));
    }
    let transcript = resolve(home, session_id)?;
    let directory = transcript.with_extension("").join("subagents");
    let root = dunce::canonicalize(&directory).context("native subagent history is unavailable")?;
    let path = dunce::canonicalize(directory.join(format!("agent-{agent_id}.jsonl")))
        .context("native subagent history is unavailable")?;
    if !root.starts_with(dunce::canonicalize(
        transcript.parent().context("invalid native session path")?,
    )?) || path.parent() != Some(root.as_path())
    {
        return Err(anyhow!("native subagent path escapes its session"));
    }
    let mut thread = Thread {
        id: Some(SessionRef {
            provider: ProviderKind::Claude,
            id: session_id.to_string(),
        }),
        path: Some(path.to_string_lossy().into()),
        ..Default::default()
    };
    thread.agent_id = Some(agent_id.into());
    read_with_summary(&path, thread, limit).map(|history| history.response)
}

fn read_with_summary(path: &Path, thread: Thread, limit: usize) -> Result<NativeHistory> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let offset = file.metadata()?.len().saturating_sub(MAX_FILE_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut input = BufReader::new(file);
    let mut nodes = Vec::new();
    let mut warnings = Vec::new();
    let mut line = Vec::new();
    if offset > 0 {
        warnings.push("only the latest 64 MiB of the transcript was read");
        (&mut input)
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if line.len() > MAX_LINE_BYTES {
            return Err(anyhow!("native row exceeds the transcript read limit"));
        }
    }
    let mut bytes = line.len() as u64;
    loop {
        line.clear();
        // A capped reader prevents a single unterminated row from allocating
        // arbitrarily much memory. Treat either cap as incomplete history.
        let length = (&mut input)
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if length == 0 {
            break;
        }
        bytes += length as u64;
        if length > MAX_LINE_BYTES || bytes > MAX_FILE_BYTES {
            warnings.push("transcript read limit reached");
            break;
        }
        match serde_json::from_slice::<Value>(&line) {
            Ok(value) => nodes.push(value),
            Err(_) if !line.ends_with(b"\n") => {
                warnings.push("transcript has an unfinished trailing row");
                break;
            }
            Err(_) => warnings.push("transcript contains a corrupt complete row"),
        }
    }
    convert(thread, nodes, limit, warnings)
}

fn convert(
    mut thread: Thread,
    nodes: Vec<Value>,
    limit: usize,
    mut warnings: Vec<&str>,
) -> Result<NativeHistory> {
    let native_id = &thread.id.as_ref().context("Claude session ID missing")?.id;
    let mut indexed = HashMap::new();
    let mut leaf = None;
    for (index, node) in nodes.iter().enumerate() {
        if let Some(id) = node["sessionId"].as_str().or(node["session_id"].as_str())
            && id != native_id
        {
            return Err(anyhow!("Claude transcript session identity changed"));
        }
        if let Some(agent_id) = thread.agent_id.as_deref() {
            if node["agentId"].as_str().is_some_and(|id| id != agent_id) {
                return Err(anyhow!("native subagent identity changed"));
            }
        } else if node["isSidechain"] == true {
            continue;
        }
        if let Some(id) = node["uuid"].as_str() {
            indexed.insert(id, index);
            // Last native chain node selects the current branch. Metadata such
            // as last-prompt's leafUuid does not select an older assistant leaf.
            if node.get("parentUuid").is_some() {
                leaf = Some(id);
            }
        }
    }
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    while let Some(id) = leaf {
        if !visited.insert(id) {
            warnings.push("transcript parent cycle");
            break;
        }
        let Some(&index) = indexed.get(id) else {
            warnings.push("transcript parent is unavailable");
            break;
        };
        let node = &nodes[index];
        chain.push(node);
        leaf = node["parentUuid"].as_str();
    }
    chain.reverse();
    if chain.is_empty() && !nodes.is_empty() {
        warnings.push("transcript has no readable message chain");
    }
    let mut details = BTreeMap::new();
    let session = thread.id.as_ref().unwrap().clone();
    let mut turns: Vec<Arc<Turn>> = Vec::new();
    let mut model = None;
    let mut block_indices: HashMap<String, usize> = HashMap::new();
    for node in chain {
        // Queued prompts are user input stored as attachments rather than messages.
        let queued_prompt = (node["type"] == "attachment"
            && node["attachment"]["type"] == "queued_command")
            .then_some(&node["attachment"]["prompt"])
            .filter(|prompt| prompt.is_string() || prompt.is_array());
        let kind = if queued_prompt.is_some() {
            "user"
        } else {
            node["type"].as_str().unwrap_or_default()
        };
        if let Some(cwd) = node["cwd"].as_str() {
            thread.cwd = Some(cwd.into());
        }
        if kind == "attachment" {
            match node["attachment"]["type"].as_str() {
                Some(
                    "deferred_tools_delta"
                    | "agent_listing_delta"
                    | "mcp_instructions_delta"
                    | "skill_listing"
                    | "total_tokens_reminder"
                    | "batching_reminder_sent"
                    | "silent_turn_reminder"
                    | "date_change"
                    | "auto_mode"
                    | "command_permissions"
                    | "environment"
                    | "model"
                    | "session_context"
                    | "date"
                    | "prompt_snapshot"
                    | "deferred_tools_record"
                    | "bash_output_audience_note"
                    | "task_reminder",
                ) => {}
                _ => {
                    let attachment = &node["attachment"];
                    if !attachment.is_object() {
                        warnings.push("native attachment content is unavailable");
                        continue;
                    }
                    if turns.is_empty() {
                        turns.push(Arc::new(Turn {
                            id: node["uuid"].as_str().unwrap_or_default().into(),
                            status: TurnStatus::Completed,
                            items: Some(Vec::new()),
                            ..Default::default()
                        }));
                    }
                    // Keep the complete payload, including unfamiliar attachment
                    // types, inspectable without pretending it was a tool call.
                    Arc::make_mut(turns.last_mut().unwrap())
                        .items
                        .as_mut()
                        .unwrap()
                        .push(Arc::new(Item::new(
                            node["uuid"].as_str().unwrap_or_default().into(),
                            ItemStatus::Unknown,
                            ItemBody::Attachment {
                                kind: match attachment["type"].as_str() {
                                    Some(
                                        "hook_success"
                                        | "hook_error"
                                        | "hook_non_blocking_error"
                                        | "hook_blocking_error",
                                    ) => AttachmentKind::HookResult,
                                    Some("edited_text_file") => AttachmentKind::FileEdit,
                                    Some("remote_session_change") => AttachmentKind::SessionUpdate,
                                    _ => AttachmentKind::Other,
                                },
                                content: attachment.clone(),
                            },
                        )));
                }
            }
            continue;
        }
        if kind == "system" {
            if node["subtype"] == "compact_boundary" {
                warnings.push("history includes a compaction boundary");
            }
            continue;
        }
        if !matches!(kind, "user" | "assistant") {
            warnings.push("unsupported transcript content");
            continue;
        }
        if let Some(name) = node["message"]["model"].as_str() {
            model = Some(agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Claude,
                id: name.into(),
            });
        }
        let content = queued_prompt.unwrap_or(&node["message"]["content"]);
        let user_id = queued_prompt
            .and_then(|_| node["attachment"]["source_uuid"].as_str())
            .or(node["uuid"].as_str());
        let owned;
        let blocks = if let Some(text) = content.as_str() {
            owned = vec![json!({"type":"text","text":text})];
            &owned
        } else if let Some(blocks) = content.as_array() {
            blocks
        } else {
            warnings.push("message content is unavailable");
            continue;
        };
        let user_input = kind == "user"
            && (queued_prompt.is_some() || node["isMeta"] != true)
            && blocks
                .iter()
                .any(|block| matches!(block["type"].as_str(), Some("text" | "image")));
        // Additional input belongs to the running turn, just as it does in
        // the live adapter. Splitting it creates an extra persisted turn that
        // survives the live overlay and duplicates its input and response.
        if (user_input && queued_prompt.is_none()) || turns.is_empty() {
            turns.push(Arc::new(Turn {
                id: user_id.unwrap_or_default().into(),
                status: TurnStatus::Completed,
                items: Some(Vec::new()),
                ..Default::default()
            }));
        }
        let turn = Arc::make_mut(turns.last_mut().unwrap());
        let items = turn.items.as_mut().unwrap();
        if user_input {
            items.push(Arc::new(Item {
                id: user_id.context("user message ID is unavailable")?.into(),
                status: ItemStatus::Unknown,
                client_input_id: user_id.map(Into::into),
                body: ItemContent::Inline {
                    body: Box::new(ItemBody::UserMessage {
                        text: None,
                        content: input_blocks(blocks),
                    }),
                },
            }));
        }
        let message_id = node["message"]["id"]
            .as_str()
            .or(node["uuid"].as_str())
            .unwrap_or_default();
        let base = node["apiBlockIndex"]
            .as_u64()
            .map(|index| index as usize)
            .unwrap_or(*block_indices.get(message_id).unwrap_or(&0));
        for (index, block) in blocks.iter().enumerate() {
            let id = format!("{message_id}:{}", base + index);
            let item = match block["type"].as_str() {
                Some("tool_result") => {
                    if let Some(item) = items
                        .iter_mut()
                        .find(|item| Some(item.id.as_str()) == block["tool_use_id"].as_str())
                    {
                        let item = Arc::make_mut(item);
                        item.status = if block["is_error"] == true {
                            ItemStatus::Failed
                        } else {
                            ItemStatus::Completed
                        };
                        item.body = ItemContent::Inline {
                            body: Box::new(super::tool_result_body(
                                item.body(),
                                &block["content"],
                                &node["toolUseResult"],
                            )),
                        };
                        details.insert(
                            item.id.clone(),
                            NativeItemDetails {
                                output_path: node["toolUseResult"]["persistedOutputPath"]
                                    .as_str()
                                    .map(str::to_owned),
                            },
                        );
                    } else {
                        warnings.push("tool result has no available tool call");
                    }
                    continue;
                }
                Some("text" | "image") if kind == "user" => continue,
                _ => super::content_item(
                    &session,
                    id,
                    block,
                    thread.cwd.as_deref(),
                    ItemStatus::Unknown,
                )?,
            };
            items.push(Arc::new(item));
        }
        block_indices.insert(message_id.into(), base + blocks.len());
    }
    let has_more = turns.len() > limit;
    if has_more {
        turns.drain(..turns.len() - limit);
    }
    warnings.sort_unstable();
    warnings.dedup();
    thread.history_has_more = Some(has_more);
    use agent_protocol::session::{HistoryReadKind, HistoryReadState};
    thread.history_read_state = Some(HistoryReadState::new(
        if warnings.is_empty() {
            if has_more {
                HistoryReadKind::Partial
            } else {
                HistoryReadKind::Complete
            }
        } else {
            HistoryReadKind::Incomplete
        },
        warnings.into_iter().map(str::to_owned).collect(),
    ));
    thread.turns = Some(turns);
    Ok(NativeHistory {
        response: ThreadResponse { thread, model },
        details,
    })
}

#[cfg(test)]
mod tests {
    fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
        match item.body() {
            agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
            agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
            agent_protocol::items::ItemBody::Reasoning { content, .. } => {
                content.first().map(String::as_str)
            }
            _ => None,
        }
    }

    use super::*;
    const ID: &str = "12345678-1234-4234-8234-123456789abc";
    const NATIVE: &str = include_str!("../../tests/fixtures/claude-2.1.266.jsonl");
    fn fixture(bytes: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects").join("an-actual-storage-name");
        fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{ID}.jsonl"));
        fs::write(&path, bytes).unwrap();
        (root, path)
    }
    #[test]
    fn attachments_and_queued_prompts_are_preserved_in_the_active_history() {
        let attachments = [
            json!({"type":"hook_success","hookName":"SessionStart","stdout":"hook output","exitCode":0}),
            json!({"type":"edited_text_file","filename":"example.txt","snippet":"new content"}),
            json!({"type":"remote_session_change","pr":"13","url":null}),
            json!({"type":"unrecognized_attachment","nested":{"content":"retained"}}),
        ];
        let mut rows = vec![
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"content":"question"}}),
        ];
        let mut parent = "u".to_owned();
        for (index, attachment) in attachments.iter().enumerate() {
            let id = format!("attachment-{index}");
            rows.push(
                json!({"type":"attachment","uuid":id,"parentUuid":parent,"attachment":attachment}),
            );
            parent = id;
        }
        for (id, prompt) in [
            ("queued-text", json!("additional instruction")),
            (
                "queued-image",
                json!([
                    {"type":"text","text":"look at this"},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}
                ]),
            ),
        ] {
            rows.push(json!({"type":"attachment","uuid":id,"parentUuid":parent,
                "attachment":{"type":"queued_command","source_uuid":format!("source-{id}"),"prompt":prompt}}));
            parent = id.into();
        }
        rows.push(
            json!({"type":"assistant","uuid":"answer","parentUuid":parent,
            "message":{"id":"reply","content":[{"type":"text","text":"queued answer"}]}}),
        );
        rows.push(
            json!({"type":"user","uuid":"next-turn","parentUuid":"answer",
            "message":{"content":"next question"}}),
        );
        let source = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let (_root, path) = fixture(&source);
        let response = read(&path, 100).unwrap();
        assert_eq!(
            response.thread.history_read_state.unwrap().kind,
            agent_protocol::session::HistoryReadKind::Complete
        );
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].id, "u".into());
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 8);
        for (item, attachment) in items[1..].iter().zip(&attachments) {
            assert!(
                matches!(item.body(), ItemBody::Attachment {content, ..} if content == attachment)
            );
            let presentation =
                agent_core::presentation::item_presentation(item, Some(ProviderKind::Claude));
            assert!(presentation.visible && presentation.collapsible);
            let body = agent_core::presentation::body::expanded_body(item);
            assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), *attachment);
        }
        let text = &items[5];
        assert_eq!(text.id, "source-queued-text".into());
        assert!(matches!(text.body(), ItemBody::UserMessage { .. }));
        assert!(
            matches!(text.body(), ItemBody::UserMessage {content, ..} if content.first() == Some(&MessagePart::Text {text: "additional instruction".into()}))
        );
        let image = &items[6];
        assert_eq!(image.id, "source-queued-image".into());
        assert!(
            matches!(image.body(), ItemBody::UserMessage {content, ..} if content.get(1) == Some(&MessagePart::Image {source: "data:image/png;base64,aW1hZ2U=".into()}))
        );
        assert_eq!(item_text(&(items[7])), Some("queued answer"));
        let page = read(&path, 1).unwrap();
        assert_eq!(page.thread.history_has_more, Some(true));
        assert_eq!(page.thread.turns.unwrap()[0].id, "next-turn".into());
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn native_cli_fixture_is_read_without_modification_or_execution() {
        let (root, path) = fixture(NATIVE);
        assert_eq!(
            resolve(root.path(), Uuid::parse_str(ID).unwrap()).unwrap(),
            path
        );
        let response = read(&path, 1000).unwrap();
        assert_eq!(
            response
                .thread
                .id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some(ID)
        );
        let turns = response.thread.turns.unwrap();
        assert!(!turns.is_empty());
        let items: Vec<_> = turns
            .iter()
            .flat_map(|turn| turn.items.iter().flatten())
            .collect();
        assert!(items.iter().any(|item| matches!(
            item.body(),
            agent_protocol::items::ItemBody::UserMessage { .. }
        )));
        assert!(items.iter().any(|item| matches!(
            item.body(),
            agent_protocol::items::ItemBody::AssistantText { .. }
        )));
        assert_eq!(fs::read_to_string(path).unwrap(), NATIVE);
    }
    #[test]
    fn corruption_and_unfinished_tail_remain_visible_and_are_never_repaired() {
        for (tail, issue) in [
            ("{broken}\n", "corrupt complete row"),
            ("{\"type\":", "unfinished trailing row"),
        ] {
            let source = format!("{NATIVE}{tail}");
            // Keep the owning directory alive throughout the read.
            let (root, path) = fixture(&source);
            let response = read(&path, 5).unwrap();
            assert_eq!(
                response.thread.history_read_state.as_ref().unwrap().kind,
                agent_protocol::session::HistoryReadKind::Incomplete
            );
            assert!(
                response
                    .thread
                    .history_read_state
                    .as_ref()
                    .unwrap()
                    .issues
                    .join("\n")
                    .contains(issue)
            );
            assert_eq!(fs::read_to_string(path).unwrap(), source);
            drop(root);
        }
    }
    #[test]
    fn native_subagents_keep_their_chain_and_never_escape_the_parent_session() {
        let (root, parent) = fixture(NATIVE);
        let session = Uuid::parse_str(parent.file_stem().unwrap().to_str().unwrap()).unwrap();
        let directory = parent.with_extension("").join("subagents");
        fs::create_dir_all(&directory).unwrap();
        let source = NATIVE
            .lines()
            .map(|line| {
                let mut node: Value = serde_json::from_str(line).unwrap();
                node["agentId"] = "agent-fixture".into();
                node["isSidechain"] = true.into();
                node.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let path = directory.join("agent-agent-fixture.jsonl");
        fs::write(&path, &source).unwrap();
        let related = read_related(root.path(), session, "agent-fixture", 1000).unwrap();
        assert_eq!(related.thread.agent_id.as_deref(), Some("agent-fixture"));
        assert!(
            related
                .thread
                .turns
                .iter()
                .flatten()
                .any(|turn| turn.items.as_ref().is_some_and(|items| !items.is_empty()))
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        assert!(read_related(root.path(), session, "../elsewhere", 5).is_err());
        assert!(read_related(root.path(), session, "missing", 5).is_err());
        assert_eq!(fs::read_to_string(parent).unwrap(), NATIVE);
    }
    #[test]
    fn duplicate_full_ids_are_rejected_instead_of_selecting_a_project() {
        let (root, path) = fixture(NATIVE);
        let other = root.path().join("projects/other");
        fs::create_dir_all(&other).unwrap();
        fs::copy(&path, other.join(path.file_name().unwrap())).unwrap();
        assert!(
            resolve(root.path(), Uuid::parse_str(ID).unwrap())
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(resolve(root.path(), Uuid::new_v4()).is_err());
    }
    #[test]
    fn active_branch_and_tool_results_use_native_parent_and_tool_ids() {
        let rows = [
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"content":"question"}}),
            json!({"type":"assistant","uuid":"abandoned","parentUuid":"u","message":{"id":"old","content":[{"type":"text","text":"abandoned answer"}]}}),
            json!({"type":"assistant","uuid":"tool","parentUuid":"u","message":{"id":"m","content":[{"type":"tool_use","id":"call","name":"Read","input":{"file_path":"example.txt"}}]}}),
            json!({"type":"user","uuid":"result","parentUuid":"tool","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"output"}]}}),
            json!({"type":"assistant","uuid":"answer","parentUuid":"result","apiBlockIndex":1,"message":{"id":"m","content":[{"type":"text","text":"selected answer"}]}}),
        ];
        let response = convert(
            Thread {
                id: Some(SessionRef {
                    provider: ProviderKind::Claude,
                    id: ID.into(),
                }),
                ..Default::default()
            },
            rows.into(),
            5,
            Vec::new(),
        )
        .unwrap();
        let turns = response.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 1);
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].id, "call".into());
        assert!(
            matches!(items[1].body(), ItemBody::ToolCall {result: Some(value), ..} if value == "output")
        );
        assert_eq!(items[2].id, "m:1".into());
        assert_eq!(item_text(&(items[2])), Some("selected answer"));
    }
}
