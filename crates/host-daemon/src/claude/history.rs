//! Read-only access to Claude's native transcript tree. Never repairs or writes
//! transcripts, and never launches the CLI to list or display a conversation.
use agent_core::{
    models::{Item, Thread, ThreadResponse, Turn},
    session::{ProviderKind, SessionRef},
};
use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
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
        return fs::canonicalize(path).context("Claude storage is unavailable");
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
        id: Some(format!("claude:{id}")),
        session: Some(SessionRef {
            provider: ProviderKind::Claude,
            id: id.into(),
        }),
        path: Some(path.to_string_lossy().into()),
        updated_at: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_secs() as f64),
        status: Some(agent_core::models::ThreadStatus {
            kind: agent_core::models::ThreadStatusKind::NotLoaded,
        }),
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

fn input_blocks(blocks: &[Value]) -> Vec<Value> {
    blocks.iter().map(|block| {
        if block["type"] == "image" && block["source"]["type"] == "base64" {
            json!({"type":"image","url":format!("data:{};base64,{}", block["source"]["media_type"].as_str().unwrap_or("application/octet-stream"), block["source"]["data"].as_str().unwrap_or_default())})
        } else if block["type"] == "image" && block["source"]["type"] == "url" { json!({"type":"image","url":block["source"]["url"]}) }
        else { block.clone() }
    }).collect()
}

pub(super) fn read(path: &Path, limit: usize) -> Result<ThreadResponse> {
    read_with_summary(path, summary(path)?, limit)
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
    let root = fs::canonicalize(&directory).context("native subagent history is unavailable")?;
    let path = fs::canonicalize(directory.join(format!("agent-{agent_id}.jsonl")))
        .context("native subagent history is unavailable")?;
    if !root.starts_with(fs::canonicalize(
        transcript.parent().context("invalid native session path")?,
    )?) || path.parent() != Some(root.as_path())
    {
        return Err(anyhow!("native subagent path escapes its session"));
    }
    let mut thread = Thread {
        id: Some(format!("claude:{session_id}")),
        session: Some(SessionRef {
            provider: ProviderKind::Claude,
            id: session_id.to_string(),
        }),
        path: Some(path.to_string_lossy().into()),
        ..Default::default()
    };
    thread.agent_id = Some(agent_id.into());
    read_with_summary(&path, thread, limit)
}

fn read_with_summary(path: &Path, thread: Thread, limit: usize) -> Result<ThreadResponse> {
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
) -> Result<ThreadResponse> {
    let native_id = &thread
        .session
        .as_ref()
        .context("Claude session ID missing")?
        .id;
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
    let mut turns: Vec<Arc<Turn>> = Vec::new();
    let mut model = None;
    let mut block_indices: HashMap<String, usize> = HashMap::new();
    for node in chain {
        let kind = node["type"].as_str().unwrap_or_default();
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
                _ => warnings.push("native attachment content is not fully decoded"),
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
            model = Some(format!("claude:{name}"));
        }
        let owned;
        let blocks = if let Some(text) = node["message"]["content"].as_str() {
            owned = vec![json!({"type":"text","text":text})];
            &owned
        } else if let Some(blocks) = node["message"]["content"].as_array() {
            blocks
        } else {
            warnings.push("message content is unavailable");
            continue;
        };
        let user_input = kind == "user"
            && node["isMeta"] != true
            && blocks
                .iter()
                .any(|block| matches!(block["type"].as_str(), Some("text" | "image")));
        if user_input || turns.is_empty() {
            turns.push(Arc::new(Turn {
                id: node["uuid"].as_str().unwrap_or_default().into(),
                status: Some("completed".into()),
                items: Some(Vec::new()),
                ..Default::default()
            }));
        }
        let turn = Arc::make_mut(turns.last_mut().unwrap());
        let items = turn.items.as_mut().unwrap();
        if user_input {
            items.push(Arc::new(Item {
                id: serde_json::from_value(node["uuid"].clone())?,
                kind: Some("userMessage".into()),
                content: Some(Value::Array(input_blocks(blocks))),
                client_id: serde_json::from_value(node["uuid"].clone())?,
                ..Default::default()
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
                        item.status = Some(
                            if block["is_error"] == true {
                                "failed"
                            } else {
                                "completed"
                            }
                            .into(),
                        );
                        item.result = Some(block["content"].clone());
                        item.detail_file = node["toolUseResult"]["persistedOutputPath"]
                            .as_str()
                            .map(str::to_owned);
                        item.agent_id =
                            node["toolUseResult"]["agentId"].as_str().map(str::to_owned);
                    } else {
                        warnings.push("tool result has no available tool call");
                    }
                    continue;
                }
                Some("text" | "image") if kind == "user" => continue,
                _ => match super::content_item(id, block, "interrupted")? {
                    Some(item) => item,
                    None => {
                        warnings.push("unsupported message block");
                        continue;
                    }
                },
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
    use agent_core::session::{HistoryReadKind, HistoryReadState};
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
    Ok(ThreadResponse { thread, model })
}

#[cfg(test)]
mod tests {
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
    fn native_cli_fixture_is_read_without_modification_or_execution() {
        let (root, path) = fixture(NATIVE);
        assert_eq!(
            resolve(root.path(), Uuid::parse_str(ID).unwrap()).unwrap(),
            path
        );
        let response = read(&path, 1000).unwrap();
        assert_eq!(
            response.thread.id.as_deref(),
            Some(format!("claude:{ID}").as_str())
        );
        let turns = response.thread.turns.unwrap();
        assert!(!turns.is_empty());
        let items: Vec<_> = turns
            .iter()
            .flat_map(|turn| turn.items.iter().flatten())
            .collect();
        assert!(
            items
                .iter()
                .any(|item| item.kind.as_deref() == Some("userMessage"))
        );
        assert!(
            items
                .iter()
                .any(|item| item.kind.as_deref() == Some("agentMessage"))
        );
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
                agent_core::session::HistoryReadKind::Incomplete
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
                id: Some(format!("claude:{ID}")),
                session: Some(SessionRef {
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
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), 1);
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].id, "call");
        assert_eq!(items[1].result, Some(json!("output")));
        assert_eq!(items[2].id, "m:1");
        assert_eq!(items[2].text.as_deref(), Some("selected answer"));
    }
}
