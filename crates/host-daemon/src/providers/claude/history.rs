//! Read-only access to Claude's native transcript tree. Never repairs or writes
//! transcripts, and never launches the CLI to list or display a conversation.
use super::native;
use agent_protocol::{
    execution::*,
    ids::ItemId,
    models::{Thread, ThreadResponse, Turn},
    session::{ProviderKind, SessionRef},
};
use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;
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
const SUMMARY_WINDOW_BYTES: usize = 128 * 1024;

pub(super) fn home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return dunce::canonicalize(path).context("Claude storage is unavailable");
    }
    directories::BaseDirs::new()
        .map(|base| base.home_dir().join(".claude"))
        .context("Claude home directory is unavailable")
}

/// Cursor is a candidate offset, so search can skip nonmatching transcripts
/// without retaining their bodies or repeatedly decoding earlier pages.
pub(super) fn list_page(
    home: &Path,
    running: Vec<Thread>,
    search: &str,
    cursor: Option<&str>,
    owned: Option<&HashSet<String>>,
    limit: u32,
) -> Result<super::SessionPage> {
    let mut candidates: Vec<_> = files(home)?
        .into_iter()
        .map(|path| {
            let timestamp = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_secs())
                .unwrap_or_default();
            let id = path
                .file_stem()
                .and_then(|id| id.to_str())
                .context("invalid transcript filename")?
                .to_owned();
            Ok((timestamp, id, Some(path), None))
        })
        .collect::<Result<_>>()?;
    let active: HashSet<_> = running
        .iter()
        .filter_map(|t| t.id.as_ref().map(|id| id.id.clone()))
        .collect();
    for thread in running {
        let id = thread.id.as_ref().unwrap().id.clone();
        if !candidates
            .iter()
            .any(|(_, candidate, _, _)| candidate == &id)
        {
            candidates.push((
                thread.updated_at.unwrap_or_default() as u64,
                id,
                None,
                Some(thread),
            ));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let offset = cursor
        .map(str::parse::<usize>)
        .transpose()
        .context("invalid Claude list cursor")?
        .unwrap_or(0);
    if offset > candidates.len() {
        anyhow::bail!("invalid Claude list cursor");
    }
    let total = candidates.len();
    let search = search.trim().to_lowercase();
    let mut data = Vec::new();
    let mut visited = offset;
    for (timestamp, id, path, fallback) in candidates.into_iter().skip(offset) {
        visited += 1;
        let mut summary = if let Some(path) = path {
            match summary(&path) {
                Ok(Summary {
                    automated: true, ..
                }) if owned.is_some_and(|owned| !owned.contains(id.as_str()))
                    && !active.contains(&id) =>
                {
                    continue;
                }
                Ok(Summary { thread, branch, .. }) => super::SessionSummary { thread, branch },
                Err(error) => {
                    let mut thread = Thread {
                        id: Some(SessionRef {
                            provider: ProviderKind::Claude,
                            id,
                        }),
                        updated_at: Some(timestamp as f64),
                        name: Some("Claude履歴を読み取れません".into()),
                        status: SessionStatus::Unknown,
                        ..Default::default()
                    };
                    thread.history_read_state =
                        Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Unavailable,
                            vec![format!("{error:#}")],
                        ));
                    super::SessionSummary {
                        thread,
                        branch: None,
                    }
                }
            }
        } else {
            super::SessionSummary {
                thread: fallback.unwrap(),
                branch: None,
            }
        };
        if summary
            .thread
            .id
            .as_ref()
            .is_some_and(|id| active.contains(&id.id))
        {
            summary.thread.status = SessionStatus::Running;
        }
        if search.is_empty()
            || summary
                .thread
                .name
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&search)
            || summary
                .thread
                .preview
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&search)
        {
            data.push(summary);
            if data.len() >= limit.max(1) as usize {
                break;
            }
        }
    }
    Ok(super::SessionPage {
        data,
        next_cursor: (visited < total).then(|| visited.to_string()),
    })
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

// Ignore tool results and other message bodies while reading list metadata.
// Keep human input raw until a preview is actually needed.
#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SummaryMetadata<'a> {
    #[serde(rename = "type")]
    kind: Value,
    session_id: Value,
    #[serde(rename = "session_id")]
    alternate_session_id: Value,
    cwd: Value,
    git_branch: Value,
    custom_title: Value,
    ai_title: Value,
    is_meta: Value,
    origin: Value,
    prompt_source: Value,
    entrypoint: Value,
    #[serde(borrow)]
    message: Option<&'a serde_json::value::RawValue>,
}

pub(super) struct Summary {
    pub thread: Thread,
    pub branch: Option<String>,
    /// SDK runs can use a custom entrypoint; their first input still has SDK provenance.
    /// Host ownership distinguishes these from the person's Bex conversations.
    pub automated: bool,
}

/// Extract metadata without loading complete tool output or message bodies.
pub(super) fn summary(path: &Path) -> Result<Summary> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut head = Vec::new();
    // Finish the last row so metadata crossing the boundary stays readable.
    (&mut file)
        .take(SUMMARY_WINDOW_BYTES as u64)
        .read_to_end(&mut head)?;
    if !head.ends_with(b"\n") {
        BufReader::new((&mut file).take(MAX_LINE_BYTES as u64)).read_until(b'\n', &mut head)?;
    }
    let mut tail = Vec::new();
    let offset = metadata
        .len()
        .saturating_sub(SUMMARY_WINDOW_BYTES as u64)
        .max(head.len() as u64);
    if offset < metadata.len() {
        file.seek(SeekFrom::Start(offset))?;
        file.take(SUMMARY_WINDOW_BYTES as u64)
            .read_to_end(&mut tail)?;
    }
    let id = path
        .file_stem()
        .and_then(|id| id.to_str())
        .context("invalid native transcript filename")?;
    let mut thread = Thread {
        id: Some(SessionRef {
            provider: ProviderKind::Claude,
            id: id.into(),
        }),
        updated_at: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_secs() as f64),
        status: agent_protocol::models::SessionStatus::Unknown,
        ..Default::default()
    };
    let mut branch = None;
    let mut entrypoint = None;
    let mut sdk_input = None;
    for line in head.split(|byte| *byte == b'\n').chain(
        tail.split(|byte| *byte == b'\n')
            .skip(usize::from(offset > head.len() as u64)),
    ) {
        let Ok(value) = serde_json::from_slice::<SummaryMetadata<'_>>(line) else {
            continue;
        };
        if let Some(session) = value
            .session_id
            .as_str()
            .or(value.alternate_session_id.as_str())
            && session != id
        {
            return Err(anyhow!(
                "Claude transcript session identity does not match its filename"
            ));
        }
        if entrypoint.is_none() {
            entrypoint = value.entrypoint.as_str().map(str::to_owned);
        }
        if sdk_input.is_none() && value.kind == "user" {
            sdk_input = Some(value.prompt_source.as_str() == Some("sdk"));
        }
        if let Some(saved_branch) = value.git_branch.as_str() {
            branch = Some(saved_branch.to_owned());
        }
        if let Some(cwd) = value.cwd.as_str() {
            thread.cwd = Some(cwd.into());
        }
        if let Some(title) = value.custom_title.as_str().or(value.ai_title.as_str()) {
            thread.name = Some(title.into());
        }
        if thread.preview.is_none()
            && value.kind == "user"
            && native::is_human_input(
                value.is_meta == true,
                value.origin["kind"].as_str(),
                value.prompt_source.as_str(),
            )
            && let Some(message) = value.message
            && let Ok(message) = serde_json::from_str::<Value>(message.get())
        {
            thread.preview = input_text(&message["content"]);
        }
    }
    Ok(Summary {
        thread,
        branch,
        automated: sdk_input == Some(true)
            || matches!(entrypoint.as_deref(), Some("sdk-cli" | "sdk-ts" | "sdk-py")),
    })
}

fn input_text(content: &Value) -> Option<String> {
    let text: String = if let Some(text) = content.as_str() {
        text.chars().take(160).collect()
    } else {
        let mut parts = content
            .as_array()?
            .iter()
            .filter_map(|block| block["text"].as_str());
        parts
            .next()?
            .chars()
            .chain(parts.flat_map(|part| std::iter::once('\n').chain(part.chars())))
            .take(160)
            .collect()
    };
    (!text.is_empty()).then_some(text)
}

pub(super) struct NativeHistory {
    pub response: ThreadResponse,
    pub output_paths: BTreeMap<ItemId, String>,
    /// The last assistant message of each turn, where a fork of that turn ends.
    pub fork_points: HashMap<agent_protocol::ids::TurnId, String>,
}
pub(super) fn read_details(path: &Path) -> Result<NativeHistory> {
    read_with_summary(path, summary(path)?.thread, usize::MAX)
}

pub(super) fn read(path: &Path, limit: usize) -> Result<ThreadResponse> {
    read_with_summary(path, summary(path)?.thread, limit).map(|history| history.response)
}

pub(super) fn read_page(path: &Path, end: usize) -> Result<agent_protocol::session::HistoryPage> {
    let response = read(path, usize::MAX)?;
    anyhow::ensure!(
        response
            .thread
            .history_read_state
            .as_ref()
            .is_some_and(|state| {
                state.kind == agent_protocol::session::HistoryReadKind::Complete
            }),
        "Claude native history is incomplete"
    );
    page_turns(
        response.thread.turns.as_deref().unwrap_or_default(),
        end,
        100,
    )
}

fn page_turns(
    turns: &[Arc<Turn>],
    end: usize,
    limit: usize,
) -> Result<agent_protocol::session::HistoryPage> {
    let start = end.saturating_sub(limit);
    Ok(agent_protocol::session::HistoryPage {
        history_read_state: None,
        turns: turns
            .get(start..end)
            .context("Claude history cursor is no longer valid")?
            .to_vec(),
        next_cursor: (start > 0).then(|| start.to_string()),
    })
}

pub(super) fn read_related(
    home: &Path,
    session_id: Uuid,
    agent_id: &str,
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
        ..Default::default()
    };
    thread.agent_id = Some(agent_id.into());
    read_with_summary(&path, thread, usize::MAX).map(|history| history.response)
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

fn transcript_chain<'a>(
    nodes: &'a [Value],
    native_id: &str,
    agent_id: Option<&str>,
) -> Result<(Vec<&'a Value>, Vec<&'static str>)> {
    let mut warnings = Vec::new();
    let mut indexed = HashMap::new();
    let mut leaf = None;
    for (index, node) in nodes.iter().enumerate() {
        if let Some(id) = node["sessionId"].as_str().or(node["session_id"].as_str())
            && id != native_id
        {
            return Err(anyhow!("Claude transcript session identity changed"));
        }
        if let Some(agent_id) = agent_id {
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
    Ok((chain, warnings))
}

/// A transcript row's own item and its content blocks; None skips the row,
/// noting why in `warnings` when that loses content.
fn node_content<'a>(
    kind: &str,
    id: &str,
    node: &'a Value,
    warnings: &mut Vec<&str>,
) -> Option<(
    Option<agent_protocol::items::Item>,
    std::borrow::Cow<'a, [Value]>,
)> {
    match kind {
        "attachment" => match native::attachment_item(id, &node["attachment"]) {
            Ok(Some(item)) => Some((Some(item), std::borrow::Cow::Borrowed(&[] as &[Value]))),
            Ok(None) => None,
            Err(_) => {
                warnings.push("native attachment content is unavailable");
                None
            }
        },
        "user" | "assistant" => {
            let Some(blocks) = native::message_blocks(&node["message"]["content"]) else {
                warnings.push("message content is unavailable");
                return None;
            };
            let input = (kind == "user")
                .then(|| {
                    native::user_item(
                        id,
                        &node["message"]["content"],
                        node["isMeta"] == true,
                        node["origin"]["kind"].as_str(),
                        node["promptSource"].as_str(),
                    )
                })
                .flatten();
            Some((input, blocks))
        }
        "system" => {
            if node["subtype"] == "compact_boundary" {
                warnings.push("history includes a compaction boundary");
            }
            None
        }
        _ => {
            warnings.push("unsupported transcript content");
            None
        }
    }
}

fn is_prompt(item: &agent_protocol::items::Item) -> bool {
    matches!(
        item.body(),
        agent_protocol::items::ItemBody::UserMessage { .. }
    )
}

/// How a transcript row records Claude Code's send-now cut.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SendNowCut {
    None,
    /// The bare interruption marker, which only says the response was cut.
    Marker,
    /// Content the cut stopped, such as an interrupted tool's result.
    Stamped,
}

impl SendNowCut {
    fn of(node: &Value) -> Self {
        if node["type"] != "user" || node["interruptedBySendNow"] != true {
            return Self::None;
        }
        let text_only = native::message_blocks(&node["message"]["content"])
            .is_some_and(|blocks| blocks.iter().all(|block| block["type"] == "text"));
        if text_only {
            Self::Marker
        } else {
            Self::Stamped
        }
    }
}

fn convert(
    mut thread: Thread,
    nodes: Vec<Value>,
    limit: usize,
    mut warnings: Vec<&str>,
) -> Result<NativeHistory> {
    let session = thread
        .id
        .as_ref()
        .context("Claude session ID missing")?
        .clone();
    let (chain, chain_warnings) =
        transcript_chain(&nodes, &session.id, thread.agent_id.as_deref())?;
    warnings.extend(chain_warnings);
    let mut output_paths = BTreeMap::new();
    let mut fork_points = HashMap::new();
    let mut turns: Vec<Arc<Turn>> = Vec::new();
    let mut model = None;
    let mut block_indices: HashMap<&str, usize> = HashMap::new();
    let mut item_positions = HashMap::new();
    // Claude Code's send-now (a priority "now" message, T3 Code's steer) cuts
    // the response, stamps the interruption `interruptedBySendNow`, and runs
    // the message as the next prompt. That prompt continues the cut turn.
    let mut steering = false;
    let mut wake = Wake::default();
    for node in chain {
        let kind = node["type"].as_str().unwrap_or_default();
        let id = node["uuid"].as_str().unwrap_or_default();
        let cut = SendNowCut::of(node);
        steering |= cut != SendNowCut::None;
        if cut == SendNowCut::Marker {
            continue;
        }
        if let Some(cwd) = node["cwd"].as_str() {
            thread.cwd = Some(cwd.into());
        }
        match notification(&mut turns, node, &mut output_paths, &mut wake) {
            Notification::Other => {}
            Notification::Finished => continue,
            Notification::Woke => {
                item_positions.clear();
                continue;
            }
        }
        let Some((initial_item, blocks)) = node_content(kind, id, node, &mut warnings) else {
            continue;
        };
        if let Some(name) = node["message"]["model"].as_str() {
            model = Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: name.into(),
            });
        }
        if let Some(id) = turn_start(
            &turns,
            &wake,
            &mut steering,
            node,
            initial_item.as_ref(),
            id,
        ) {
            item_positions.clear();
            turns.push(Arc::new(Turn {
                id,
                status: TurnStatus::Completed,
                items: Some(Vec::new()),
                ..Default::default()
            }));
        }
        wake.follow(node);
        let turn = Arc::make_mut(turns.last_mut().unwrap());
        let time = node_time(node);
        turn.started_at_ms = turn.started_at_ms.or(time);
        turn.completed_at_ms = time.or(turn.completed_at_ms);
        if kind == "assistant" && !id.is_empty() {
            fork_points.insert(turn.id.clone(), id.to_owned());
        }
        let items = turn.items.as_mut().unwrap();
        if let Some(item) = initial_item {
            item_positions.entry(item.id.clone()).or_insert(items.len());
            items.push(Arc::new(item));
        }
        let message_id = node["message"]["id"]
            .as_str()
            .or(node["uuid"].as_str())
            .unwrap_or_default();
        let base = node["apiBlockIndex"]
            .as_u64()
            .map(|index| index as usize)
            .unwrap_or(*block_indices.get(message_id).unwrap_or(&0));
        let appended = append_blocks(
            std::mem::take(items),
            item_positions,
            blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (format!("{message_id}:{}", base + index), block))
                .filter(|(_, block)| {
                    kind != "user" || !matches!(block["type"].as_str(), Some("text" | "image"))
                }),
            &session,
            thread.cwd.as_deref(),
            &node["toolUseResult"],
            node["message"]["stop_reason"].as_str(),
        )?;
        *items = appended.items;
        item_positions = appended.item_positions;
        output_paths.extend(appended.output_paths);
        warnings.extend(appended.warnings);
        block_indices.insert(message_id, base + blocks.len());
    }
    let page = page_turns(&turns, turns.len(), limit)?;
    let has_more = page.next_cursor.is_some();
    warnings.sort_unstable();
    warnings.dedup();
    thread.history_has_more = Some(has_more);
    thread.history_cursor = page.next_cursor;
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
    thread.turns = Some(page.turns);
    Ok(NativeHistory {
        response: ThreadResponse {
            thread,
            model,
            ..Default::default()
        },
        output_paths,
        fork_points,
    })
}

/// Claude's own turn is named by the answer it follows, as the live Host names it.
fn wake_turn_id(turns: &[Arc<Turn>]) -> agent_protocol::ids::TurnId {
    format!("{}-wake", turns.last().map_or("", |turn| turn.id.as_str())).into()
}

fn node_time(node: &Value) -> Option<u64> {
    node["timestamp"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .and_then(|time| u64::try_from(time.timestamp_millis()).ok())
}

/// Whether Claude's answer ended, so that what follows it starts the turn
/// Claude woke for, as T3 Code's continuation.
#[derive(Default)]
struct Wake {
    /// The message that ended the answer. Claude saves each of its blocks.
    answer: Option<String>,
    /// The current turn is a wake turn that only its notification has started.
    notification: bool,
}

impl Wake {
    fn woke_by(&self, node: &Value) -> bool {
        node["type"] == "assistant"
            && self
                .answer
                .as_deref()
                .is_some_and(|answer| node["message"]["id"].as_str() != Some(answer))
    }

    /// An answer ends with its final message; input after it is what Claude
    /// responds to next.
    fn follow(&mut self, node: &Value) {
        let answer = (node["type"] == "assistant"
            && native::text_phase(node["message"]["stop_reason"].as_str())
                == agent_protocol::items::AssistantPhase::Final)
            .then(|| node["message"]["id"].as_str().map(str::to_owned))
            .flatten();
        *self = Self {
            answer,
            notification: false,
        };
    }
}

/// The turn a transcript node starts: a standalone user message or a response
/// Claude starts on its own after its answer. Queued human input, steers and
/// task activity stay in the running turn. A prompt after Claude Code's send-now cut (`steering`) continues the turn
/// the cut stopped, as a steer.
fn turn_start(
    turns: &[Arc<Turn>],
    wake: &Wake,
    steering: &mut bool,
    node: &Value,
    input: Option<&agent_protocol::items::Item>,
    id: &str,
) -> Option<agent_protocol::ids::TurnId> {
    if wake.woke_by(node) {
        return Some(wake_turn_id(turns));
    }
    let prompt = node["type"] == "user" && input.is_some_and(is_prompt);
    ((prompt && !std::mem::take(steering)) || turns.is_empty())
        .then(|| input.map_or(id, |item| item.id.as_str()).into())
}

enum Notification {
    Other,
    Finished,
    Woke,
}

/// A task notification finishes the item that ran the work and, after an
/// answer, starts the turn Claude woke for with the notification's reports.
fn notification(
    turns: &mut Vec<Arc<Turn>>,
    node: &Value,
    output_paths: &mut BTreeMap<ItemId, String>,
    wake: &mut Wake,
) -> Notification {
    if node["type"] != "user" || node["origin"]["kind"] != "task-notification" {
        return Notification::Other;
    }
    let mut report = BackgroundReport {
        kind: BackgroundTaskKind::BackgroundTask,
        label: None,
        outcome: BackgroundOutcome::Unknown,
        exit_code: None,
    };
    let mut finished = false;
    if let Some(outcome) =
        native::notification_outcome(node["origin"]["kind"].as_str(), &node["message"]["content"])
        && let Some((turn_index, item_index, item)) =
            turns
                .iter()
                .enumerate()
                .rev()
                .find_map(|(turn_index, turn)| {
                    turn.items
                        .as_ref()?
                        .iter()
                        .enumerate()
                        .find_map(|(item_index, item)| {
                            (item.id.as_str() == outcome.tool_id)
                                .then(|| outcome.item(item.id.clone(), item.body()))
                                .flatten()
                                .map(|item| (turn_index, item_index, item))
                        })
                })
    {
        if let Some(path) = outcome.output_path {
            output_paths.insert(item.id.clone(), path.into());
        }
        report = outcome.report(item.body());
        Arc::make_mut(&mut turns[turn_index])
            .items
            .as_mut()
            .unwrap()[item_index] = Arc::new(item);
        finished = true;
    }
    if wake.answer.is_some() {
        let id = wake_turn_id(turns);
        let time = node_time(node);
        turns.push(Arc::new(Turn {
            id: id.clone(),
            status: TurnStatus::Completed,
            items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                format!("{id}:notification").into(),
                ItemStatus::Completed,
                agent_protocol::items::ItemBody::Notification {
                    reports: vec![report],
                },
            ))]),
            started_at_ms: time,
            completed_at_ms: time,
            ..Default::default()
        }));
        *wake = Wake {
            answer: None,
            notification: true,
        };
        return Notification::Woke;
    }
    if wake.notification
        && let Some(item) = Arc::make_mut(turns.last_mut().unwrap())
            .items
            .as_mut()
            .and_then(|items| items.first_mut())
        && let agent_protocol::items::ItemBody::Notification { reports } =
            Arc::make_mut(item).body_mut()
    {
        reports.push(report);
        return Notification::Finished;
    }
    if finished {
        Notification::Finished
    } else {
        Notification::Other
    }
}

struct AppendedBlocks {
    items: Vec<Arc<agent_protocol::items::Item>>,
    item_positions: HashMap<ItemId, usize>,
    output_paths: BTreeMap<agent_protocol::ids::ItemId, String>,
    warnings: Vec<&'static str>,
}

fn append_blocks<'a>(
    mut items: Vec<Arc<agent_protocol::items::Item>>,
    mut item_positions: HashMap<ItemId, usize>,
    blocks: impl Iterator<Item = (String, &'a Value)>,
    session: &SessionRef,
    cwd: Option<&str>,
    tool_result: &Value,
    stop_reason: Option<&str>,
) -> Result<AppendedBlocks> {
    let mut output_paths = BTreeMap::new();
    let mut warnings = Vec::new();
    for (id, block) in blocks {
        let item = match block["type"].as_str() {
            Some("tool_result") => {
                let position = block["tool_use_id"]
                    .as_str()
                    .and_then(|id| item_positions.get(id).copied());
                if let Some(position) = position {
                    let item = &mut items[position];
                    *item = Arc::new(native::tool_result_item(
                        item.id.clone(),
                        item.body(),
                        &block["content"],
                        tool_result,
                        block["is_error"] == true,
                    ));
                    if let Some(path) = tool_result["persistedOutputPath"]
                        .as_str()
                        .filter(|path| !path.is_empty())
                    {
                        output_paths.insert(item.id.clone(), path.into());
                    }
                } else {
                    warnings.push("tool result has no available tool call");
                }
                continue;
            }
            _ => native::content_item(session, id, block, stop_reason, cwd, ItemStatus::Unknown)?,
        };
        item_positions.entry(item.id.clone()).or_insert(items.len());
        items.push(Arc::new(item));
    }
    Ok(AppendedBlocks {
        items,
        item_positions,
        output_paths,
        warnings,
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
    use agent_protocol::items::*;
    use serde_json::json;
    const ID: &str = "12345678-1234-4234-8234-123456789abc";
    const NATIVE: &str = include_str!("../../../tests/fixtures/claude-2.1.266.jsonl");
    fn fixture(bytes: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects").join("an-actual-storage-name");
        fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{ID}.jsonl"));
        fs::write(&path, bytes).unwrap();
        (root, path)
    }

    #[test]
    fn previews_keep_text_block_separators_and_count_unicode_characters() {
        for (content, expected) in [
            (Value::Null, None),
            (json!(42), None),
            (json!({"text":"object"}), None),
            (json!(""), None),
            (json!(" \n"), Some(" \n".into())),
            (json!([]), None),
            (json!([null,42,"ignored",{"text":true}]), None),
            (json!([{"text":""}]), None),
            (json!([{"text":""},{"text":""}]), Some("\n".into())),
            (
                json!([{"text":"a"},null,{"text":""},{"text":42},{"text":"b"}]),
                Some("a\n\nb".into()),
            ),
            (json!([{"type":"image","text":"kept"}]), Some("kept".into())),
            (
                json!([{"text":"a".repeat(159)},{"text":"🦀"}]),
                Some("a".repeat(159) + "\n"),
            ),
            (
                json!([{"text":"a".repeat(158)},{"text":"🦀more"}]),
                Some("a".repeat(158) + "\n🦀"),
            ),
        ] {
            assert_eq!(input_text(&content), expected, "{content}");
        }
        for count in [0, 1, 79, 80, 81, 159, 160, 161, 1000] {
            let text = "🦀é".repeat(count);
            let expected: String = text.chars().take(160).collect();
            let expected = (!expected.is_empty()).then_some(expected);
            for content in [json!(text), json!([{"text":text}])] {
                assert_eq!(input_text(&content), expected);
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn preview_arrays_match_the_prefix_of_their_text_and_separators(
            parts in proptest::collection::vec(proptest::option::of(".{0,100}"),0..32),
        ) {
            let combined=parts.iter().filter_map(Option::as_deref).collect::<Vec<_>>().join("\n");
            let expected: String=combined.chars().take(160).collect();
            let content: Vec<_>=parts.into_iter().enumerate().map(|(index,text)| match text {
                Some(text)=>json!({"text":text}),
                None=>if index%2==0 {Value::Null}else{json!({"text":false})},
            }).collect();
            proptest::prop_assert_eq!(input_text(&json!(content)),(!expected.is_empty()).then_some(expected));
        }
    }

    #[test]
    fn timeline_pages_fail_on_incomplete_history_and_recover_after_repair() {
        let source = [
            json!({"type":"user","uuid":"user","parentUuid":null,"message":{"content":"question"}}),
            json!({"type":"assistant","uuid":"answer","parentUuid":"user","message":{"id":"reply","content":[{"type":"text","text":"answer"}]}}),
        ].map(|row| row.to_string()).join("\n") + "\n";
        let (_root, path) = fixture(&source);
        let page = read_page(&path, 1).unwrap();
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].id.as_str(), "user");
        assert!(page.next_cursor.is_none());
        assert!(read_page(&path, 2).is_err());

        fs::write(&path, source.clone() + "{invalid JSON}\n").unwrap();
        assert!(read_page(&path, 1).is_err());
        fs::write(&path, &source).unwrap();
        assert_eq!(read_page(&path, 1).unwrap().turns[0], page.turns[0]);
        fs::remove_file(&path).unwrap();
        assert!(read_page(&path, 1).is_err());
    }

    #[test]
    fn metadata_pages_decode_only_the_requested_files_and_search_can_reach_old_titles() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects/p");
        fs::create_dir_all(&project).unwrap();
        for index in 1..=8u128 {
            let id = Uuid::from_u128(index).to_string();
            let path = project.join(format!("{id}.jsonl"));
            fs::write(&path,serde_json::json!({"type":"user","sessionId":id,"cwd":"/fixture","message":{"content":format!("title {index}")}}).to_string()+"\n").unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(index as u64)),
                )
                .unwrap();
        }
        let first = list_page(root.path(), vec![], "", None, Some(&HashSet::new()), 3).unwrap();
        assert_eq!(
            first
                .data
                .iter()
                .map(|t| t.thread.updated_at.unwrap() as u64)
                .collect::<Vec<_>>(),
            [8, 7, 6]
        );
        assert_eq!(first.next_cursor.as_deref(), Some("3"));
        let second = list_page(
            root.path(),
            vec![],
            "",
            first.next_cursor.as_deref(),
            Some(&HashSet::new()),
            3,
        )
        .unwrap();
        assert_eq!(
            second
                .data
                .iter()
                .map(|t| t.thread.updated_at.unwrap() as u64)
                .collect::<Vec<_>>(),
            [5, 4, 3]
        );
        let last = list_page(
            root.path(),
            vec![],
            "",
            second.next_cursor.as_deref(),
            Some(&HashSet::new()),
            3,
        )
        .unwrap();
        assert_eq!(last.data.len(), 2);
        assert!(last.next_cursor.is_none());
        let found = list_page(
            root.path(),
            vec![],
            "title 1",
            None,
            Some(&HashSet::new()),
            3,
        )
        .unwrap();
        assert_eq!(found.data.len(), 1);
        assert!(found.next_cursor.is_none());
        assert!(
            list_page(
                root.path(),
                vec![],
                "",
                Some("bad"),
                Some(&HashSet::new()),
                3
            )
            .is_err()
        );
        assert!(list_page(root.path(), vec![], "", Some("9"), Some(&HashSet::new()), 3).is_err());
    }
    #[test]
    fn task_listings_omit_unowned_sdk_runs_started_by_other_agents() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects/p");
        fs::create_dir_all(&project).unwrap();
        // Who started the transcript decides; a later resume by another client does not.
        let transcripts = [
            ("terminal", ["cli", "cli"]),
            ("host", ["sdk-ts", "sdk-ts"]),
            ("host-resumed-by-agent", ["sdk-ts", "sdk-cli"]),
            ("agent", ["sdk-cli", "sdk-cli"]),
            ("agent-opened-by-host", ["sdk-cli", "sdk-ts"]),
            ("agent-running-in-host", ["sdk-cli", "sdk-ts"]),
            ("NSH message text sizing", ["sdk-ts", "sdk-ts"]),
            ("python-agent", ["sdk-py", "sdk-py"]),
            ("terminal-resumed-by-agent", ["cli", "sdk-ts"]),
            ("sdk-later-interactive", ["sdk-ts", "cli"]),
        ];
        for (index, (name, entrypoints)) in transcripts.into_iter().enumerate() {
            let id = Uuid::from_u128(index as u128 + 1).to_string();
            let rows: String = entrypoints
                .into_iter()
                .map(|entrypoint| {
                    json!({"type":"user","sessionId":id,"cwd":"/fixture","entrypoint":entrypoint,"message":{"content":name}}).to_string() + "\n"
                })
                .collect();
            fs::write(project.join(format!("{id}.jsonl")), rows).unwrap();
        }
        let running = Thread {
            id: Some(SessionRef {
                provider: ProviderKind::Claude,
                id: Uuid::from_u128(6).to_string(),
            }),
            ..Default::default()
        };
        let owned: HashSet<_> = [2, 3]
            .map(|id| Uuid::from_u128(id).to_string())
            .into_iter()
            .collect();
        let listed = |owned| {
            let mut names: Vec<_> =
                list_page(root.path(), vec![running.clone()], "", None, owned, 10)
                    .unwrap()
                    .data
                    .into_iter()
                    .map(|summary| summary.thread.preview.unwrap())
                    .collect();
            names.sort();
            names
        };
        assert_eq!(
            listed(Some(&owned)),
            [
                "agent-running-in-host",
                "host",
                "host-resumed-by-agent",
                "terminal",
                "terminal-resumed-by-agent"
            ]
        );
        assert_eq!(listed(None).len(), transcripts.len());
        assert!(
            list_page(
                root.path(),
                vec![],
                "NSH message text sizing",
                None,
                Some(&owned),
                1
            )
            .unwrap()
            .data
            .is_empty()
        );
    }

    #[test]
    fn sdk_input_with_a_custom_entrypoint_stays_out_of_task_lists() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects/p");
        fs::create_dir_all(&project).unwrap();
        let transcripts = [
            ("review", ["sdk", "sdk"]),
            ("host", ["sdk", "sdk"]),
            ("review-resumed-interactively", ["sdk", "cli"]),
            ("terminal-resumed-by-sdk", ["cli", "sdk"]),
            ("legacy-terminal-resumed-by-sdk", ["", "sdk"]),
        ];
        for (index, (name, sources)) in transcripts.into_iter().enumerate() {
            let id = Uuid::from_u128(index as u128 + 1).to_string();
            let mut rows = json!({"type":"attachment","sessionId":id,"entrypoint":"t3-code","promptSource":"sdk"}).to_string() + "\n";
            for source in sources {
                let mut row = json!({"type":"user","sessionId":id,"entrypoint":"t3-code","message":{"content":name}});
                if !source.is_empty() {
                    row["promptSource"] = json!(source);
                }
                rows.push_str(&format!("{row}\n"));
            }
            fs::write(project.join(format!("{id}.jsonl")), rows).unwrap();
        }
        let owned = HashSet::from([Uuid::from_u128(2).to_string()]);
        let listed = |search, owned| {
            let mut names: Vec<_> = list_page(root.path(), vec![], search, None, owned, 10)
                .unwrap()
                .data
                .into_iter()
                .map(|summary| summary.thread.preview.unwrap())
                .collect();
            names.sort();
            names
        };
        assert_eq!(
            listed("", Some(&owned)),
            [
                "host",
                "legacy-terminal-resumed-by-sdk",
                "terminal-resumed-by-sdk"
            ]
        );
        assert!(listed("review", Some(&owned)).is_empty());
        assert_eq!(listed("", None).len(), transcripts.len());
    }
    #[test]
    fn a_send_now_prompt_continues_the_turn_it_cut() {
        let rows = [
            json!({"type":"user","uuid":"first","parentUuid":null,"message":{"content":"first question"}}),
            json!({"type":"assistant","uuid":"tool-call","parentUuid":"first","message":{"id":"call","stop_reason":null,
                "content":[{"type":"tool_use","id":"tool","name":"Bash","input":{"command":"sleep 60"}}]}}),
            // Claude Code records the tool the steer stopped, then the cut.
            json!({"type":"user","uuid":"tool-cut","parentUuid":"tool-call","interruptedBySendNow":true,
                "message":{"content":[{"type":"tool_result","tool_use_id":"tool","is_error":true,
                    "content":"[Tool call did not complete: the turn was ended to deliver the message that follows. Nothing refused it; re-run it if still needed.]"}]}}),
            json!({"type":"user","uuid":"cut","parentUuid":"tool-cut","interruptedBySendNow":true,
                "message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
            json!({"type":"user","uuid":"steer","parentUuid":"cut","message":{"content":"change course"}}),
            json!({"type":"assistant","uuid":"answer","parentUuid":"steer","message":{"id":"reply","stop_reason":"end_turn",
                "content":[{"type":"text","text":"changed"}]}}),
            json!({"type":"user","uuid":"next","parentUuid":"answer","message":{"content":"next question"}}),
        ];
        let source = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let (_root, path) = fixture(&source);
        let turns = read(&path, 100).unwrap().thread.turns.unwrap();
        let ids: Vec<Vec<&str>> = turns
            .iter()
            .map(|turn| {
                turn.items
                    .iter()
                    .flatten()
                    .map(|item| item.id.as_str())
                    .collect()
            })
            .collect();
        assert_eq!(
            ids,
            [vec!["first", "tool", "steer", "reply:0"], vec!["next"],]
        );
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
                "attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"source_uuid":format!("source-{id}"),"prompt":prompt}}));
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
        for ((item, attachment), expected_kind) in items[1..].iter().zip(&attachments).zip([
            AttachmentKind::HookResult,
            AttachmentKind::FileEdit,
            AttachmentKind::SessionUpdate,
            AttachmentKind::Other,
        ]) {
            assert!(
                matches!(item.body(), ItemBody::Attachment {kind, content} if kind == &expected_kind && content == attachment)
            );
            let presentation = agent_core::presentation::item_presentation(
                agent_core::localization::UiLanguage::Japanese,
                item,
                Some(ProviderKind::Claude),
            );
            assert!(presentation.collapsible);
            assert!(matches!(
                agent_core::presentation::ItemMetadata::from(item.as_ref()).kind,
                agent_core::presentation::GroupKind::Activity
            ));
            let body = agent_core::presentation::body::expanded_body(
                agent_core::localization::UiLanguage::Japanese,
                item,
            );
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
    fn session_summary_retains_the_latest_native_workspace_and_branch() {
        let source = [
            json!({"sessionId":ID,"cwd":"/original","gitBranch":"main","type":"user","message":{"content":"question"}}),
            json!({"sessionId":ID,"cwd":"/checkout","gitBranch":"agent/task","type":"assistant"}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        let (_root, path) = fixture(&source);
        let summary = summary(&path).unwrap();
        assert_eq!(summary.thread.cwd.as_deref(), Some("/checkout"));
        assert_eq!(summary.branch.as_deref(), Some("agent/task"));
        assert_eq!(summary.thread.preview.as_deref(), Some("question"));
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn session_summary_keeps_metadata_at_adjacent_and_separated_read_windows() {
        for gap in [0, 256 * 1024] {
            let mut source = format!(
                "{}\n",
                json!({"type":"user","session_id":ID,"message":{"content":"question"}})
            );
            let row = json!({"type":"assistant","message":{"content":""}}).to_string() + "\n";
            let padding = SUMMARY_WINDOW_BYTES - source.len() - row.len();
            source.push_str(
                &(json!({"type":"assistant","message":{"content":"x".repeat(padding)}})
                    .to_string()
                    + "\n"),
            );
            assert_eq!(source.len(), SUMMARY_WINDOW_BYTES);
            if gap > 0 {
                source.push_str(
                    &(json!({"type":"assistant","message":{"content":"x".repeat(gap)}})
                        .to_string()
                        + "\n"),
                );
            }
            source.push_str(&format!("{}\n", json!({"sessionId":ID,"cwd":"/latest","gitBranch":"feature","customTitle":"Saved title"})));
            source.push_str(&format!(
                "{}\n",
                json!({"type":"assistant","message":{"content":"x".repeat(8 * 1024)}})
            ));
            let (_root, path) = fixture(&source);
            let summary = summary(&path).unwrap();
            assert_eq!(summary.thread.cwd.as_deref(), Some("/latest"));
            assert_eq!(summary.branch.as_deref(), Some("feature"));
            assert_eq!(summary.thread.name.as_deref(), Some("Saved title"));
            assert_eq!(summary.thread.preview.as_deref(), Some("question"));
            assert_eq!(fs::read_to_string(path).unwrap(), source);
        }
    }

    #[test]
    fn session_summary_does_not_trust_a_valid_suffix_of_a_corrupt_partial_row() {
        let header = format!(
            "{}\n",
            json!({"type":"user","sessionId":ID,"cwd":"/original","message":{"content":"question"}})
        );
        let corrupt = "x".repeat(2 * 1024 * 1024);
        let forged = format!(
            "{}\n",
            json!({"cwd":"/forged","customTitle":"Forged title"})
        );
        let empty = json!({"type":"assistant","message":{"content":""}}).to_string() + "\n";
        let head_padding = SUMMARY_WINDOW_BYTES - header.len() - empty.len();
        let head = format!(
            "{header}{}\n",
            json!({"type":"assistant","message":{"content":"x".repeat(head_padding)}})
        );
        let padding = SUMMARY_WINDOW_BYTES - forged.len() - empty.len();
        let tail = format!(
            "{}\n",
            json!({"type":"assistant","message":{"content":"x".repeat(padding)}})
        );
        assert_eq!(forged.len() + tail.len(), SUMMARY_WINDOW_BYTES);
        let source = format!("{head}{corrupt}{forged}{tail}");
        let (_root, path) = fixture(&source);
        let summary = summary(&path).unwrap();
        assert_eq!(summary.thread.cwd.as_deref(), Some("/original"));
        assert_eq!(summary.thread.name, None);
        assert_eq!(summary.thread.preview.as_deref(), Some("question"));
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn tool_results_keep_first_duplicates_and_find_calls_appended_in_later_rows() {
        let session = SessionRef::new(ProviderKind::Claude, ID.into()).unwrap();
        for count in [1usize, 63, 64, 127, 128, 129, 512] {
            let mut calls: Vec<_>=(0..count).map(|index|json!({"type":"tool_use","id":format!("tool{index}"),"name":"Bash","input":{"command":format!("command {index}")}})).collect();
            calls.push(json!({"type":"tool_use","id":"tool0","name":"Bash","input":{"command":"duplicate"}}));
            let initial = append_blocks(
                vec![],
                HashMap::new(),
                calls
                    .iter()
                    .enumerate()
                    .map(|(index, block)| (format!("call{index}"), block)),
                &session,
                None,
                &Value::Null,
                None,
            )
            .unwrap();
            let old = initial.items.clone();
            let results = [
                json!({"type":"tool_result","tool_use_id":format!("tool{}",count-1),"content":"last output"}),
                json!({"type":"tool_result","tool_use_id":"tool0","content":"first output"}),
                json!({"type":"tool_result","tool_use_id":null}),
                json!({"type":"tool_result","tool_use_id":7}),
                json!({"type":"tool_result","tool_use_id":"missing"}),
                json!({"type":"tool_result","tool_use_id":""}),
            ];
            let updated = append_blocks(
                initial.items,
                initial.item_positions,
                results
                    .iter()
                    .enumerate()
                    .map(|(index, block)| (format!("result{index}"), block)),
                &session,
                None,
                &json!({"persistedOutputPath":"/work/first.output"}),
                None,
            )
            .unwrap();
            assert_eq!(updated.items.len(), count + 1);
            assert_eq!(
                updated.warnings,
                vec!["tool result has no available tool call"; 4]
            );
            assert_eq!(
                updated.output_paths[&ItemId::from("tool0")],
                "/work/first.output"
            );
            assert!(
                matches!(updated.items[0].body(),ItemBody::CommandExecution{output,..} if output=="first output")
            );
            assert!(
                matches!(updated.items[count].body(),ItemBody::CommandExecution{command,output,..} if command=="duplicate" && output.is_empty())
            );
            assert!(old.iter().all(|item| item.status == ItemStatus::Unknown));
            let later = [
                json!({"type":"tool_use","id":"later","name":"Bash","input":{"command":"later first"}}),
                json!({"type":"tool_use","id":"later","name":"Bash","input":{"command":"later duplicate"}}),
                json!({"type":"tool_result","tool_use_id":"later","content":"later output"}),
            ];
            let later = append_blocks(
                updated.items,
                updated.item_positions,
                later
                    .iter()
                    .enumerate()
                    .map(|(index, block)| (format!("later{index}"), block)),
                &session,
                None,
                &json!({"persistedOutputPath":"/work/later.output"}),
                None,
            )
            .unwrap();
            assert!(later.warnings.is_empty());
            assert_eq!(
                later.output_paths[&ItemId::from("later")],
                "/work/later.output"
            );
            assert!(
                matches!(later.items[count+1].body(),ItemBody::CommandExecution{command,output,..} if command=="later first" && output=="later output")
            );
            assert!(
                matches!(later.items[count+2].body(),ItemBody::CommandExecution{command,output,..} if command=="later duplicate" && output.is_empty())
            );
        }
    }

    #[test]
    fn transcript_item_lookups_include_attachments_and_restart_for_each_turn() {
        let calls: Vec<_>=(0..256).map(|index|json!({"type":"tool_use","id":format!("tool{index}"),"name":"Bash","input":{"command":format!("command {index}")}})).collect();
        let mut nodes = vec![
            json!({"type":"user","message":{"content":"first input"}}),
            json!({"type":"assistant","message":{"id":"first","content":calls}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool255","content":"first turn output"}]}}),
            json!({"type":"attachment","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"notice"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"node3","content":"attachment output"}]},"toolUseResult":{"persistedOutputPath":"/work/attachment.output"}}),
            json!({"type":"user","message":{"content":"second input"}}),
            json!({"type":"assistant","message":{"id":"second","content":[{"type":"tool_use","id":"tool200","name":"Bash","input":{"command":"second command"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool200","content":"second output"}]}}),
            json!({"type":"assistant","apiBlockIndex":11,"message":{"id":"shared","content":[{"type":"text","text":"one"},{"type":"text","text":"two"}]}}),
            json!({"type":"assistant","message":{"id":"shared","content":[{"type":"text","text":"three"}]}}),
        ];
        for (index, node) in nodes.iter_mut().enumerate() {
            node["uuid"] = json!(format!("node{index}"));
            node["parentUuid"] = if index == 0 {
                Value::Null
            } else {
                json!(format!("node{}", index - 1))
            };
        }
        let history = convert(
            Thread {
                id: Some(SessionRef::new(ProviderKind::Claude, ID.into()).unwrap()),
                ..Default::default()
            },
            nodes,
            usize::MAX,
            vec![],
        )
        .unwrap();
        assert_eq!(
            history.output_paths[&ItemId::from("node3")],
            "/work/attachment.output"
        );
        assert_eq!(
            history.response.thread.history_read_state.unwrap().kind,
            agent_protocol::session::HistoryReadKind::Complete
        );
        let turns = history.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        let first = turns[0].items.as_ref().unwrap();
        assert!(
            matches!(first[201].body(),ItemBody::CommandExecution{command,output,..} if command=="command 200" && output.is_empty())
        );
        assert!(
            matches!(first[256].body(),ItemBody::CommandExecution{output,..} if output=="first turn output")
        );
        assert_eq!(first[257].id.as_str(), "node3");
        assert_eq!(first[257].status, ItemStatus::Completed);
        let second = turns[1].items.as_ref().unwrap();
        assert!(
            matches!(second[1].body(),ItemBody::CommandExecution{command,output,..} if command=="second command" && output=="second output")
        );
        assert_eq!(
            second
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["node5", "tool200", "shared:11", "shared:12", "shared:13"]
        );
    }

    #[test]
    fn late_task_notification_updates_its_original_turn() {
        let nodes = vec![
            json!({"type":"user","uuid":"first","parentUuid":null,"message":{"content":"first input"}}),
            json!({"type":"assistant","uuid":"call","parentUuid":"first","message":{"id":"message","content":[{"type":"tool_use","id":"work","name":"Bash","input":{"command":"work"}}]}}),
            json!({"type":"user","uuid":"launch","parentUuid":"call","message":{"content":[{"type":"tool_result","tool_use_id":"work","content":"launched"}]},"toolUseResult":{"backgroundTaskId":"task"}}),
            json!({"type":"user","uuid":"second","parentUuid":"launch","message":{"content":"second input"}}),
            json!({"type":"user","uuid":"notice","parentUuid":"second","origin":{"kind":"task-notification"},"promptSource":"system","turnOrigin":"task_notification","queueSkipAttachments":true,"message":{"content":"<task-notification><tool-use-id>work</tool-use-id><output-file>/work/result.output</output-file><status>failed</status><summary>work failed</summary></task-notification>"}}),
        ];
        let history = convert(
            Thread {
                id: Some(SessionRef::new(ProviderKind::Claude, ID.into()).unwrap()),
                ..Default::default()
            },
            nodes,
            10,
            vec![],
        )
        .unwrap();
        let turns = history.response.thread.turns.unwrap();
        assert_eq!(
            history.output_paths[&ItemId::from("work")],
            "/work/result.output"
        );
        assert_eq!(turns.len(), 2);
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].status, ItemStatus::Failed);
        assert!(items[1].is_deferred());
        assert!(
            matches!(items[1].body(), ItemBody::CommandExecution {output, exit_code:None, ..} if output == "work failed")
        );
        assert_eq!(turns[1].items.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn a_notification_after_the_answer_starts_the_turn_claude_woke_for() {
        let notice = |uuid: &str, parent: &str, tool: &str| {
            json!({"type":"user","uuid":uuid,"parentUuid":parent,"origin":{"kind":"task-notification"},"promptSource":"system","turnOrigin":"task_notification","queueSkipAttachments":true,
                "message":{"content":format!("<task-notification><tool-use-id>{tool}</tool-use-id><status>completed</status><summary>{tool} done</summary></task-notification>")}})
        };
        let nodes = vec![
            json!({"type":"user","uuid":"first","parentUuid":null,"message":{"content":"start the server"}}),
            json!({"type":"assistant","uuid":"call","parentUuid":"first","message":{"id":"call","stop_reason":"tool_use","content":[
                {"type":"tool_use","id":"server","name":"Bash","input":{"command":"npm run dev"}},
                {"type":"tool_use","id":"agent","name":"Agent","input":{"prompt":"Review the change"}}]}}),
            json!({"type":"user","uuid":"launch","parentUuid":"call","message":{"content":[{"type":"tool_result","tool_use_id":"server","content":"launched"}]},"toolUseResult":{"backgroundTaskId":"task"}}),
            json!({"type":"assistant","uuid":"answer","parentUuid":"launch","message":{"id":"answer","stop_reason":"end_turn","content":[{"type":"text","text":"started"}]}}),
            notice("notice", "answer", "server"),
            notice("second-notice", "notice", "agent"),
            json!({"type":"assistant","uuid":"report","parentUuid":"second-notice","message":{"id":"report","stop_reason":"end_turn","content":[{"type":"text","text":"reported"}]}}),
        ];
        let history = convert(
            Thread {
                id: Some(SessionRef::new(ProviderKind::Claude, ID.into()).unwrap()),
                ..Default::default()
            },
            nodes,
            10,
            vec![],
        )
        .unwrap();
        let turns = history.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2, "{turns:#?}");
        assert!(
            turns[0]
                .items
                .iter()
                .flatten()
                .filter(|item| ["server", "agent"].contains(&item.id.as_str()))
                .all(|item| item.status == ItemStatus::Completed),
            "the notifications finish the work in the turn that ran it"
        );
        assert_eq!(
            turns[1].id,
            "first-wake".into(),
            "named by the answer it follows"
        );
        let items = turns[1].items.as_ref().unwrap();
        assert_eq!(
            items[0].body(),
            &ItemBody::Notification {
                reports: vec![
                    BackgroundReport {
                        kind: BackgroundTaskKind::Command,
                        label: Some("npm run dev".into()),
                        outcome: BackgroundOutcome::Completed,
                        exit_code: None,
                    },
                    BackgroundReport {
                        kind: BackgroundTaskKind::Subagent,
                        label: Some("Review the change".into()),
                        outcome: BackgroundOutcome::Completed,
                        exit_code: None,
                    },
                ],
            }
        );
        assert_eq!(item_text(&items[1]), Some("reported"));
    }

    #[test]
    fn queued_task_notifications_stay_in_activity_without_splitting_the_turn() {
        let notification = "<task-notification><status>failed</status><summary>Background command failed with exit code 144</summary></task-notification>";
        for mode in [
            json!("task-notification"),
            json!("unrecognized"),
            Value::Null,
        ] {
            let attachment = json!({"type":"queued_command","commandMode":mode,
                "source_uuid":"notification-source","prompt":notification});
            let rows = [
                json!({"type":"user","uuid":"u","parentUuid":null,
                    "message":{"content":"question"}}),
                json!({"type":"attachment","uuid":"notification","parentUuid":"u",
                    "attachment":attachment}),
                json!({"type":"assistant","uuid":"answer","parentUuid":"notification",
                    "message":{"content":[{"type":"text","text":"handled notification"}]}}),
                // A human can send the same text; classify by native mode, not XML.
                json!({"type":"attachment","uuid":"queued","parentUuid":"answer",
                    "attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},
                        "source_uuid":"human-source","prompt":notification}}),
                json!({"type":"assistant","uuid":"follow-up","parentUuid":"queued",
                    "message":{"content":[{"type":"text","text":"queued answer"}]}}),
                json!({"type":"user","uuid":"next-turn","parentUuid":"follow-up",
                    "message":{"content":"next question"}}),
            ];
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
            let items = turns[0].items.as_ref().unwrap();
            assert_eq!(items.len(), 5);
            assert_eq!(items[1].id, "notification".into());
            assert!(
                matches!(items[1].body(), ItemBody::Attachment {content, ..} if content == &attachment)
            );
            assert!(matches!(
                agent_core::presentation::ItemMetadata::from(items[1].as_ref()).kind,
                agent_core::presentation::GroupKind::Activity
            ));
            assert_eq!(item_text(&items[2]), Some("handled notification"));
            assert_eq!(items[3].id, "human-source".into());
            assert!(
                matches!(items[3].body(), ItemBody::UserMessage {content, ..}
                if content == &[MessagePart::Text {text: notification.into()}])
            );
            assert_eq!(item_text(&items[4]), Some("queued answer"));
        }
    }

    #[test]
    fn current_user_notification_stays_in_activity_and_does_not_become_the_preview() {
        let notice = json!({"type":"user","uuid":"notice","parentUuid":null,
            "version":"2.1.281","origin":{"kind":"task-notification"},"promptSource":"system",
            "turnOrigin":"task_notification","queueSkipAttachments":true,
            "message":{"role":"user","content":"<task-notification><tool-use-id>missing</tool-use-id><status>completed</status><summary>done</summary></task-notification>"}});
        let source = [notice, json!({"type":"user","uuid":"human","parentUuid":"notice","message":{"content":"real question"}})]
            .iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        let (_root, path) = fixture(&source);
        let response = read(&path, 100).unwrap();
        assert_eq!(response.thread.preview.as_deref(), Some("real question"));
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        let item = &turns[0].items.as_ref().unwrap()[0];
        assert!(matches!(item.body(), ItemBody::Attachment { .. }));
        assert!(matches!(
            agent_core::presentation::ItemMetadata::from(item.as_ref()).kind,
            agent_core::presentation::GroupKind::Activity
        ));
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
        let related = read_related(root.path(), session, "agent-fixture").unwrap();
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
        assert!(read_related(root.path(), session, "../elsewhere").is_err());
        assert!(read_related(root.path(), session, "missing").is_err());
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

    #[test]
    fn turns_keep_native_times_and_fork_after_their_last_assistant_message() {
        let rows = [
            json!({"type":"user","uuid":"first","parentUuid":null,"timestamp":"2026-10-28T01:00:00.000Z","message":{"content":"first"}}),
            json!({"type":"assistant","uuid":"call","parentUuid":"first","timestamp":"2026-10-28T01:00:01.000Z","message":{"id":"a","content":[{"type":"tool_use","id":"tool","name":"Bash","input":{"command":"true"}}]}}),
            json!({"type":"user","uuid":"result","parentUuid":"call","timestamp":"2026-10-28T01:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool","content":""}]}}),
            json!({"type":"assistant","uuid":"answer","parentUuid":"result","timestamp":"2026-10-28T01:00:05.500Z","message":{"id":"b","content":[{"type":"text","text":"done"}]}}),
            json!({"type":"attachment","uuid":"hook","parentUuid":"answer","timestamp":"2026-10-28T01:00:06.000Z","attachment":{"type":"hook_success","stdout":"after"}}),
            json!({"type":"user","uuid":"second","parentUuid":"hook","timestamp":"2026-10-28T01:01:00.000Z","message":{"content":"second"}}),
            json!({"type":"assistant","uuid":"reply","parentUuid":"second","timestamp":"2026-10-28T01:01:03.000Z","message":{"id":"c","content":[{"type":"text","text":"again"}]}}),
        ];
        let history = convert(
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
        let turns = history.response.thread.turns.unwrap();
        let start = 1_793_149_200_000;
        assert_eq!(
            turns
                .iter()
                .map(|turn| (turn.started_at_ms, turn.completed_at_ms))
                .collect::<Vec<_>>(),
            [
                (Some(start), Some(start + 6_000)),
                (Some(start + 60_000), Some(start + 63_000))
            ]
        );
        assert_eq!(history.fork_points["first"], "answer");
        assert_eq!(history.fork_points["second"], "reply");
    }
}
