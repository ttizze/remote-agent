//! Read-only Codex rollout import. File contents and parser errors never enter
//! diagnostics; malformed records fail the conversation instead of losing data.
use super::agent::{SessionPage, SessionSummary};
use agent_protocol::{
    models::{Thread, ThreadResponse},
    session::{HistoryReadKind, HistoryReadState, SessionRef},
};
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

mod projection;

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 50_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINE: usize = 8 * 1024 * 1024;
const MAX_META: usize = 128 * 1024;
const MAX_BASES: usize = 32;

struct Source {
    summary: SessionSummary,
    path: Option<PathBuf>,
}

pub(super) struct Catalog {
    home: PathBuf,
    epoch: Uuid,
    sources: Vec<Source>,
    rollouts: HashMap<Uuid, PathBuf>,
}

fn filename(path: &Path) -> Option<(Uuid, Uuid)> {
    let name = path.file_name()?.to_str()?.strip_prefix("rollout-")?;
    let stem = name
        .strip_suffix(".jsonl")
        .or_else(|| name.strip_suffix(".jsonl.zst"))?;
    let (thread, rollout) = if let Some((thread, revision)) = stem.rsplit_once('_') {
        (thread, Some(revision))
    } else {
        (stem, None)
    };
    let id = Uuid::parse_str(thread.get(thread.len().checked_sub(36)?..)?).ok()?;
    Some((
        id,
        rollout.map(Uuid::parse_str).transpose().ok()?.unwrap_or(id),
    ))
}

fn scoped(home: &Path, path: &Path) -> Result<PathBuf> {
    let path = dunce::canonicalize(path).context("Codex history file is unavailable")?;
    ensure!(
        path.starts_with(home),
        "Codex history path escapes its configured storage"
    );
    Ok(path)
}

fn plain(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|extension| extension == "zst") {
        path.with_extension("")
    } else {
        path.to_owned()
    }
}

fn reader(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = fs::File::open(path).context("Codex history file cannot be opened")?;
    if path.extension().is_some_and(|extension| extension == "zst") {
        let decoder = zstd::stream::read::Decoder::new(file)
            .context("Codex history compression is invalid")?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn line(reader: &mut dyn BufRead, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .context("Codex history cannot be read")?;
    ensure!(
        bytes.len() <= limit,
        "Codex history record exceeds its read limit"
    );
    Ok(bytes)
}

fn record(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Codex history contains an invalid JSON record"))
}

fn metadata(home: &Path, path: &Path) -> Result<Value> {
    let path = scoped(home, path)?;
    let first = record(&line(reader(&path)?.as_mut(), MAX_META)?)?;
    ensure!(
        first["type"] == "session_meta",
        "Codex history has no initial session metadata"
    );
    let expected = filename(&path)
        .context("Codex history filename is invalid")?
        .0;
    ensure!(
        first["payload"]["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            == Some(expected),
        "Codex history identity does not match its filename"
    );
    Ok(first["payload"].clone())
}

fn preview(home: &Path, path: &Path) -> Option<String> {
    let mut reader = reader(&scoped(home, path).ok()?).ok()?;
    let mut remaining = MAX_META;
    while remaining > 0 {
        let bytes = line(reader.as_mut(), remaining).ok()?;
        if bytes.is_empty() {
            break;
        }
        remaining -= bytes.len();
        let value = record(&bytes).ok()?;
        let payload = &value["payload"];
        let text = if value["type"] == "event_msg" && payload["type"] == "user_message" {
            payload["message"].as_str().map(str::to_owned)
        } else if value["type"] == "event_msg"
            && payload["type"] == "item_completed"
            && payload["item"]["type"] == "UserMessage"
        {
            Some(
                payload["item"]["content"]
                    .as_array()?
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        } else {
            None
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            return Some(text.chars().take(160).collect());
        }
    }
    None
}

struct Indexed {
    path: PathBuf,
    name: String,
    cwd: String,
    updated_at: f64,
    branch: Option<String>,
}

/// The native index chooses the current immutable rollout after a revert. Open
/// it read-only, without Codex migrations, repair, checkpoint, or vacuum calls.
fn index(home: &Path) -> Result<HashMap<Uuid, Result<Indexed>>> {
    let mut databases = Vec::new();
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(version) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("state_")?.strip_suffix(".sqlite"))
            .and_then(|version| version.parse::<u32>().ok())
        {
            databases.push((version, entry.path()));
        }
    }
    let Some((_, database)) = databases.into_iter().max_by_key(|(version, _)| *version) else {
        return Ok(HashMap::new());
    };
    let connection = rusqlite::Connection::open_with_flags(
        scoped(home, &database)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .context("Codex history index cannot be read")?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut statement = connection.prepare("SELECT id, rollout_path, title, cwd, updated_at, git_branch FROM threads ORDER BY updated_at DESC, id LIMIT ?1")
        .context("Codex history index has an unsupported schema")?;
    let mut rows = statement.query([MAX_FILES + 1])?;
    let mut indexed = HashMap::new();
    while let Some(row) = rows.next()? {
        ensure!(
            indexed.len() < MAX_FILES,
            "Codex history index exceeds its scan limit; results are incomplete"
        );
        let bounded = |column| -> Result<String> {
            let text = row.get_ref(column)?.as_str()?;
            ensure!(
                text.len() <= 16 * 1024,
                "Codex history index field exceeds its read limit"
            );
            Ok(text.to_owned())
        };
        let id = bounded(0)?;
        let id = Uuid::parse_str(&id).context("Codex history index has an invalid identity")?;
        let details = (|| -> Result<Indexed> {
            Ok(Indexed {
                path: PathBuf::from(bounded(1)?),
                name: bounded(2)?,
                cwd: bounded(3)?,
                updated_at: row.get::<_, f64>(4)?,
                branch: if matches!(row.get_ref(5)?, rusqlite::types::ValueRef::Null) {
                    None
                } else {
                    Some(bounded(5)?)
                },
            })
        })()
        .map_err(|_| anyhow::anyhow!("Codex history index contains invalid or oversized fields"));
        ensure!(
            indexed.insert(id, details).is_none(),
            "Codex history index repeats a conversation identity"
        );
    }
    Ok(indexed)
}

impl Catalog {
    pub(super) fn scan(home: &Path) -> Result<Self> {
        if !home.exists() {
            return Ok(Self {
                home: home.to_owned(),
                epoch: Uuid::new_v4(),
                sources: Vec::new(),
                rollouts: HashMap::new(),
            });
        }
        let home = dunce::canonicalize(home).context("Codex storage is unavailable")?;
        let indexed = index(&home)?;
        let mut pending = vec![
            (home.join("sessions"), 0),
            (home.join("archived_sessions"), 0),
        ];
        let mut entries = 0;
        let mut paths = BTreeMap::<(Uuid, Uuid), PathBuf>::new();
        while let Some((directory, depth)) = pending.pop() {
            if directory.exists() {
                scoped(&home, &directory)?;
            }
            let listing = match fs::read_dir(directory) {
                Ok(listing) => listing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => bail!("Codex session directory cannot be read"),
            };
            for entry in listing {
                let entry = entry.context("Codex session directory cannot be read")?;
                entries += 1;
                ensure!(
                    entries <= MAX_ENTRIES,
                    "Codex listing exceeds its scan limit; results are incomplete"
                );
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    ensure!(
                        depth < 3,
                        "Codex session directory exceeds its scan depth; results are incomplete"
                    );
                    pending.push((entry.path(), depth + 1));
                }
                if !kind.is_file() {
                    continue;
                }
                let path = entry.path();
                let Some(key) = filename(&path) else {
                    continue;
                };
                if let Some(saved) = paths.get(&key) {
                    ensure!(
                        plain(saved) == plain(&path),
                        "Codex rollout identity is ambiguous across directories"
                    );
                }
                // A plain rollout takes precedence over its compressed copy,
                // matching Codex's own read-path selection.
                if paths.get(&key).is_none_or(|saved| {
                    saved
                        .extension()
                        .is_some_and(|extension| extension == "zst")
                }) {
                    paths.insert(key, path);
                }
                ensure!(
                    paths.len() <= MAX_FILES,
                    "Codex listing exceeds its scan limit; results are incomplete"
                );
            }
        }
        let rollouts = paths
            .iter()
            .map(|((_, revision), path)| (*revision, path.clone()))
            .collect::<HashMap<_, _>>();
        ensure!(
            rollouts.len() == paths.len(),
            "Codex rollout identity is ambiguous"
        );
        let mut grouped = BTreeMap::<Uuid, Vec<PathBuf>>::new();
        for ((id, _), path) in paths {
            grouped.entry(id).or_default().push(path);
        }
        let mut sources = Vec::new();
        for (id, candidates) in grouped {
            let selected = if let Some(native) = indexed.get(&id) {
                native.as_ref().ok().and_then(|native| {
                    let native_path = [
                        native.path.clone(),
                        plain(&native.path),
                        plain(&native.path).with_extension("jsonl.zst"),
                    ]
                    .iter()
                    .find_map(|path| scoped(&home, path).ok());
                    candidates
                        .iter()
                        .find(|candidate| {
                            native_path
                                .as_ref()
                                .is_some_and(|path| plain(candidate) == plain(path))
                        })
                        .cloned()
                })
            } else if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                None
            };
            let details = match indexed.get(&id) {
                Some(Err(error)) => Err(anyhow::anyhow!("{error}")),
                _ => selected
                    .as_ref()
                    .context("Codex current rollout cannot be determined")
                    .and_then(|path| metadata(&home, path)),
            };
            let mut thread = Thread {
                provider: None,
                id: Some(SessionRef { id: id.to_string() }),
                ..Default::default()
            };
            let mut branch = None;
            match details {
                Ok(meta) => {
                    thread.preview = selected.as_ref().and_then(|path| preview(&home, path));
                    thread.cwd = meta["cwd"].as_str().map(str::to_owned);
                    branch = meta["git"]["branch"].as_str().map(str::to_owned);
                    thread.updated_at = selected
                        .as_ref()
                        .and_then(|path| {
                            fs::metadata(path)
                                .ok()?
                                .modified()
                                .ok()?
                                .duration_since(UNIX_EPOCH)
                                .ok()
                        })
                        .map(|duration| duration.as_secs_f64());
                    if let Some(Ok(native)) = indexed.get(&id) {
                        thread.name = (!native.name.is_empty()).then(|| native.name.clone());
                        thread.cwd = Some(native.cwd.clone());
                        thread.updated_at = Some(native.updated_at);
                        branch = native.branch.clone();
                    }
                }
                Err(error) => {
                    thread.name = Some("Codex履歴を読み取れません".into());
                    thread.history_read_state = Some(HistoryReadState::new(
                        HistoryReadKind::Unavailable,
                        vec![error.to_string()],
                    ));
                }
            }
            sources.push(Source {
                summary: SessionSummary { thread, branch },
                path: selected,
            });
        }
        sources.sort_by(|left, right| {
            right
                .summary
                .thread
                .updated_at
                .unwrap_or_default()
                .total_cmp(&left.summary.thread.updated_at.unwrap_or_default())
                .then_with(|| {
                    left.summary
                        .thread
                        .id
                        .as_ref()
                        .unwrap()
                        .id
                        .cmp(&right.summary.thread.id.as_ref().unwrap().id)
                })
        });
        Ok(Self {
            home,
            epoch: Uuid::new_v4(),
            sources,
            rollouts,
        })
    }

    pub(super) fn page(&self, search: &str, cursor: Option<&str>) -> Result<SessionPage> {
        let position = match cursor {
            Some(cursor) => {
                let (epoch, position) = cursor
                    .strip_prefix("codex-file:")
                    .and_then(|cursor| cursor.split_once(':'))
                    .context("Codex history catalog cursor is invalid")?;
                ensure!(
                    Uuid::parse_str(epoch)? == self.epoch,
                    "Codex history catalog cursor expired"
                );
                position.parse::<usize>()?
            }
            None => 0,
        };
        ensure!(
            position <= self.sources.len(),
            "Codex history catalog cursor is out of bounds"
        );
        let search = search.trim().to_lowercase();
        let mut data = Vec::new();
        let mut next_cursor = None;
        for (index, source) in self.sources.iter().enumerate().skip(position) {
            let thread = &source.summary.thread;
            if !search.is_empty()
                && !thread
                    .name
                    .iter()
                    .chain(thread.preview.iter())
                    .any(|text| text.to_lowercase().contains(&search))
            {
                continue;
            }
            if data.len() == 100 {
                next_cursor = Some(format!("codex-file:{}:{index}", self.epoch));
                break;
            }
            data.push(SessionSummary {
                thread: thread.clone(),
                branch: source.summary.branch.clone(),
            });
        }
        Ok(SessionPage { data, next_cursor })
    }

    pub(super) fn read(&self, id: &str) -> Result<ThreadResponse> {
        let id = Uuid::parse_str(id).context("Codex native identity is invalid")?;
        let id = id.to_string();
        let source = self
            .sources
            .iter()
            .find(|source| {
                source
                    .summary
                    .thread
                    .id
                    .as_ref()
                    .is_some_and(|session| session.id == id)
            })
            .context("Codex native history was not found")?;
        let path = source
            .path
            .as_ref()
            .context("Codex current rollout cannot be determined")?;
        let meta = metadata(&self.home, path)?;
        let start = meta["subagent_history_start_ordinal"].as_u64();
        let paginated = match meta["history_mode"].as_str() {
            None | Some("legacy") => false,
            Some("paginated") => true,
            _ => bail!("Codex history mode is unsupported"),
        };
        let mut projection = projection::Projection::new(paginated);
        let mut remaining = MAX_BYTES;
        self.replay(
            path,
            None,
            start,
            &mut HashSet::new(),
            &mut remaining,
            &mut projection,
        )?;
        let (turns, model) = projection.finish();
        let mut thread = source.summary.thread.clone();
        thread.turns = Some(turns.into_iter().map(std::sync::Arc::new).collect());
        thread.history_has_more = Some(false);
        thread.history_read_state = None;
        Ok(ThreadResponse {
            thread,
            model: model.map(|id| agent_protocol::models::ModelRef {
                instance_id: "codex"
                    .parse::<agent_protocol::session::ProviderInstanceId>()
                    .unwrap(),
                id,
            }),
        })
    }

    fn replay(
        &self,
        path: &Path,
        prefix: Option<(u64, u64)>,
        start: Option<u64>,
        seen: &mut HashSet<PathBuf>,
        remaining: &mut u64,
        projection: &mut projection::Projection,
    ) -> Result<()> {
        let path = scoped(&self.home, path)?;
        ensure!(
            seen.len() < MAX_BASES && seen.insert(path.clone()),
            "Codex inherited history is cyclic or exceeds its depth limit"
        );
        let meta = metadata(&self.home, &path)?;
        if let Some(base) = meta.get("history_base").filter(|base| !base.is_null()) {
            let revision = base["thread_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .context("Codex history base identity is invalid")?;
            let offset = base["end_byte_offset"]
                .as_u64()
                .context("Codex history base offset is invalid")?;
            let ordinal = base["end_ordinal_exclusive"]
                .as_u64()
                .context("Codex history base ordinal is invalid")?;
            let parent = self
                .rollouts
                .get(&revision)
                .context("Codex inherited rollout was not found")?;
            self.replay(
                parent,
                Some((offset, ordinal)),
                start,
                seen,
                remaining,
                projection,
            )?;
        }
        let mut reader = reader(&path)?;
        let mut offset = 0;
        let mut last_ordinal = None;
        loop {
            if prefix.is_some_and(|(end, _)| offset == end) {
                break;
            }
            let bytes = line(reader.as_mut(), MAX_LINE.min(*remaining as usize))?;
            if bytes.is_empty() {
                break;
            }
            *remaining = remaining
                .checked_sub(bytes.len() as u64)
                .context("Codex conversation exceeds its read limit")?;
            offset += bytes.len() as u64;
            ensure!(
                prefix.is_none_or(|(end, _)| offset <= end),
                "Codex history base splits a record"
            );
            let value = record(&bytes)?;
            let ordinal = value["ordinal"].as_u64();
            if let Some((_, end)) = prefix {
                ensure!(
                    ordinal.is_some_and(|ordinal| ordinal < end),
                    "Codex history base ordinal does not match its records"
                );
            }
            if let Some(ordinal) = ordinal {
                ensure!(
                    last_ordinal.is_none_or(|last| ordinal > last),
                    "Codex history ordinals are not increasing"
                );
                last_ordinal = Some(ordinal);
            }
            if start.is_none_or(|start| ordinal.is_some_and(|ordinal| ordinal >= start)) {
                projection.apply(&value)?;
            }
        }
        if let Some((end, ordinal)) = prefix {
            ensure!(
                offset == end
                    && last_ordinal
                        .map_or(ordinal == 0, |last| last.checked_add(1) == Some(ordinal)),
                "Codex history base is incomplete"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        execution::TurnStatus,
        items::{ItemBody, MessagePart},
    };
    use serde_json::json;
    use std::io::Write;

    fn meta(id: Uuid, mode: &str) -> Value {
        json!({"timestamp":"2026-10-05T00:00:00Z","ordinal":0,"type":"session_meta","payload":{"id":id,"cwd":"/fixture/project","history_mode":mode,"git":{"branch":"feature/import"}}})
    }

    fn event(ordinal: u64, payload: Value) -> Value {
        json!({"timestamp":"2026-10-05T00:00:00Z","ordinal":ordinal,"type":"event_msg","payload":payload})
    }

    fn write(home: &Path, id: Uuid, revision: Uuid, records: &[Value]) -> PathBuf {
        let directory = home.join("sessions/2026/10/05");
        fs::create_dir_all(&directory).unwrap();
        let suffix = if id == revision {
            String::new()
        } else {
            format!("_{revision}")
        };
        let path = directory.join(format!("rollout-2026-10-05T00-00-00-{id}{suffix}.jsonl"));
        let bytes = records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect::<String>();
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn native_catalog_pages_search_and_cursor_expiry_without_loading_bodies() {
        let home = tempfile::tempdir().unwrap();
        for index in 0..205 {
            let id = Uuid::from_u128(index + 1);
            write(
                home.path(),
                id,
                id,
                &[
                    meta(id, "legacy"),
                    event(
                        1,
                        json!({"type":"user_message","message":format!("会話 {index}"),"images":[]}),
                    ),
                ],
            );
        }
        let catalog = Catalog::scan(home.path()).unwrap();
        let first = catalog.page("", None).unwrap();
        assert_eq!(first.data.len(), 100);
        assert!(first.data.iter().all(|row| row.thread.turns.is_none()));
        assert_eq!(first.data[0].branch.as_deref(), Some("feature/import"));
        let second = catalog.page("", first.next_cursor.as_deref()).unwrap();
        assert_eq!(second.data.len(), 100);
        let last = catalog.page("", second.next_cursor.as_deref()).unwrap();
        assert_eq!(last.data.len(), 5);
        assert!(last.next_cursor.is_none());
        let ids = first
            .data
            .iter()
            .chain(&second.data)
            .chain(&last.data)
            .map(|row| row.thread.id.clone().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 205);
        assert_eq!(catalog.page("会話 204", None).unwrap().data.len(), 1);
        assert!(
            catalog
                .page("", Some(&format!("codex-file:{}:206", catalog.epoch)))
                .is_err()
        );
        assert!(
            Catalog::scan(home.path())
                .unwrap()
                .page("", first.next_cursor.as_deref())
                .is_err()
        );
    }

    #[test]
    fn legacy_import_keeps_turns_reasoning_images_and_complete_tool_output() {
        let home = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(1);
        let path = write(
            home.path(),
            id,
            id,
            &[
                meta(id, "legacy"),
                event(
                    1,
                    json!({"type":"task_started","turn_id":"first","started_at":100}),
                ),
                event(
                    2,
                    json!({"type":"user_message","message":"最初の入力","images":["data:image/png;base64,fixture"],"local_images":["/fixture/image.png"]}),
                ),
                event(3, json!({"type":"agent_reasoning","text":"summary"})),
                event(
                    4,
                    json!({"type":"agent_reasoning_raw_content","text":"full reasoning"}),
                ),
                event(
                    5,
                    json!({"type":"exec_command_begin","call_id":"tool","turn_id":"first","command":["printf","two words"],"cwd":"/fixture/project"}),
                ),
                event(
                    6,
                    json!({"type":"exec_command_end","call_id":"tool","turn_id":"first","command":["printf","two words"],"cwd":"/fixture/project","status":"completed","aggregated_output":"full output\n","exit_code":0}),
                ),
                event(
                    7,
                    json!({"type":"agent_message","message":"done","phase":"final_answer"}),
                ),
                event(
                    8,
                    json!({"type":"task_complete","turn_id":"first","completed_at":101,"duration_ms":1000}),
                ),
                event(9, json!({"type":"task_started","turn_id":"second"})),
                event(10, json!({"type":"user_message","message":"follow up"})),
                event(11, json!({"type":"turn_aborted","turn_id":"second"})),
            ],
        );
        let original = fs::read(&path).unwrap();
        let catalog = Catalog::scan(home.path()).unwrap();
        let response = catalog.read(&id.to_string()).unwrap();
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].id.as_str(), "first");
        assert_eq!(turns[0].started_at, Some(100.0));
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert_eq!(turns[1].status, TurnStatus::Interrupted);
        assert_eq!(turns[0].duration_ms, Some(1000));
        assert_eq!(turns[0].completed_at_ms, Some(101_000));
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 4);
        assert!(
            items
                .iter()
                .all(|item| item.status == agent_protocol::execution::ItemStatus::Completed)
        );
        assert!(
            matches!(items[0].body(),ItemBody::UserMessage{content,..} if content.iter().filter(|part| matches!(part,MessagePart::Image{..})).count()==2)
        );
        assert!(
            matches!(items[1].body(),ItemBody::Reasoning{content,summary} if content==&["full reasoning"] && summary==&["summary"])
        );
        assert!(
            matches!(items[2].body(),ItemBody::CommandExecution{command,output,exit_code,..} if command=="printf 'two words'" && output=="full output\n" && *exit_code==Some(0))
        );
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(catalog.page("最初", None).unwrap().data.len(), 1);
    }

    #[test]
    fn paginated_items_and_compressed_files_preserve_opaque_tool_keys() {
        let home = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(1);
        let path = write(
            home.path(),
            id,
            id,
            &[
                meta(id, "paginated"),
                event(1, json!({"type":"task_started","turn_id":"turn"})),
                event(
                    2,
                    json!({"type":"item_completed","turn_id":"turn","item":{"type":"UserMessage","id":"user","content":[{"type":"text","text":"import me"},{"type":"image","image_url":"data:image/png;base64,fixture"},{"type":"local_image","path":"/fixture/image.png"}]}}),
                ),
                event(
                    3,
                    json!({"type":"item_started","turn_id":"turn","item":{"type":"McpToolCall","id":"mcp","server":"fixture","tool":"read","arguments":{"raw_key":{"keep_me":true}},"status":"inProgress"}}),
                ),
                event(
                    4,
                    json!({"type":"item_completed","turn_id":"turn","item":{"type":"McpToolCall","id":"mcp","server":"fixture","tool":"read","arguments":{"raw_key":{"keep_me":true}},"result":{"content":[{"type":"text","text":"full result"}]},"status":"completed"}}),
                ),
                event(5, json!({"type":"task_complete","turn_id":"turn"})),
            ],
        );
        let original = fs::read(&path).unwrap();
        let compressed = path.with_extension("jsonl.zst");
        fs::write(
            &compressed,
            zstd::stream::encode_all(original.as_slice(), 1).unwrap(),
        )
        .unwrap();
        fs::remove_file(&path).unwrap();
        let catalog = Catalog::scan(home.path()).unwrap();
        let response = catalog.read(&id.to_string()).unwrap();
        let turns = response.thread.turns.unwrap();
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item.status == agent_protocol::execution::ItemStatus::Completed)
        );
        assert!(matches!(items[0].body(),ItemBody::UserMessage{content,..} if content.len()==3));
        assert!(
            matches!(items[1].body(),ItemBody::ToolCall{arguments,result,..} if arguments==&json!({"raw_key":{"keep_me":true}}) && result.as_ref().unwrap()["content"][0]["text"]=="full result")
        );
        assert_eq!(
            fs::read(&compressed).unwrap(),
            zstd::stream::encode_all(original.as_slice(), 1).unwrap()
        );
    }

    #[test]
    fn current_index_selects_revert_and_inherits_only_the_recorded_prefix() {
        let home = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(1);
        let revision = Uuid::from_u128(2);
        let parent = write(
            home.path(),
            id,
            id,
            &[
                meta(id, "paginated"),
                event(1, json!({"type":"task_started","turn_id":"old"})),
                event(
                    2,
                    json!({"type":"item_completed","turn_id":"old","item":{"type":"AgentMessage","id":"kept","content":[{"type":"Text","text":"keep this"}]}}),
                ),
                event(
                    3,
                    json!({"type":"item_completed","turn_id":"old","item":{"type":"AgentMessage","id":"discarded","content":[{"type":"Text","text":"discard this"}]}}),
                ),
            ],
        );
        let prefix = fs::read(&parent)
            .unwrap()
            .split_inclusive(|byte| *byte == b'\n')
            .take(3)
            .map(|line| line.len() as u64)
            .sum::<u64>();
        let mut metadata = meta(id, "paginated");
        metadata["ordinal"] = json!(3);
        metadata["payload"]["history_base"] =
            json!({"thread_id":id,"end_byte_offset":prefix,"end_ordinal_exclusive":3});
        let current = write(
            home.path(),
            id,
            revision,
            &[
                metadata,
                event(4, json!({"type":"task_started","turn_id":"new"})),
                event(
                    5,
                    json!({"type":"item_completed","turn_id":"new","item":{"type":"AgentMessage","id":"new","content":[{"type":"Text","text":"new answer"}]}}),
                ),
            ],
        );
        let ambiguous = Catalog::scan(home.path()).unwrap();
        assert!(ambiguous.read(&id.to_string()).is_err());
        assert_eq!(
            ambiguous.page("", None).unwrap().data[0]
                .thread
                .history_read_state
                .as_ref()
                .unwrap()
                .kind,
            HistoryReadKind::Unavailable
        );
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT,rollout_path TEXT,title TEXT,cwd TEXT,updated_at INTEGER,git_branch TEXT);").unwrap();
        db.execute(
            "INSERT INTO threads VALUES(?1,?2,'Saved title','/fixture/project',100,'saved-branch')",
            rusqlite::params![id.to_string(), current.to_str().unwrap()],
        )
        .unwrap();
        drop(db);
        let before = fs::read(home.path().join("state_5.sqlite")).unwrap();
        let catalog = Catalog::scan(home.path()).unwrap();
        let result = catalog.read(&id.to_string()).unwrap();
        let turns = result.thread.turns.unwrap();
        assert_eq!(result.thread.name.as_deref(), Some("Saved title"));
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].items.as_ref().unwrap().len(), 1);
        assert!(
            matches!(turns[0].items.as_ref().unwrap()[0].body(),ItemBody::AssistantText{text,..} if text=="keep this")
        );
        assert!(
            matches!(turns[1].items.as_ref().unwrap()[0].body(),ItemBody::AssistantText{text,..} if text=="new answer")
        );
        assert_eq!(
            fs::read(home.path().join("state_5.sqlite")).unwrap(),
            before
        );
    }

    #[test]
    fn malformed_conversation_does_not_hide_healthy_catalog_rows_or_expose_input_in_errors() {
        let home = tempfile::tempdir().unwrap();
        let bad = Uuid::from_u128(1);
        let good = Uuid::from_u128(2);
        let path = write(home.path(), bad, bad, &[meta(bad, "legacy")]);
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{private fixture content\n")
            .unwrap();
        write(
            home.path(),
            good,
            good,
            &[
                meta(good, "legacy"),
                event(1, json!({"type":"user_message","message":"healthy"})),
            ],
        );
        let catalog = Catalog::scan(home.path()).unwrap();
        assert_eq!(catalog.page("", None).unwrap().data.len(), 2);
        let error = catalog.read(&bad.to_string()).unwrap_err().to_string();
        assert!(!error.contains("private fixture content"));
        assert!(error.contains("invalid JSON"));
        assert_eq!(
            catalog
                .read(&good.to_string())
                .unwrap()
                .thread
                .turns
                .unwrap()
                .len(),
            1
        );
        let mut reader = BufReader::new(&b"12345678901234567\n"[..]);
        assert!(line(&mut reader, 16).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn transcript_replaced_with_an_external_symlink_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(1);
        let path = write(home.path(), id, id, &[meta(id, "legacy")]);
        let catalog = Catalog::scan(home.path()).unwrap();
        let external = outside.path().join("private.jsonl");
        fs::write(&external, b"private fixture content").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&external, &path).unwrap();
        assert!(
            catalog
                .read(&id.to_string())
                .unwrap_err()
                .to_string()
                .contains("escapes")
        );
        assert!(
            Catalog::scan(home.path())
                .unwrap()
                .page("", None)
                .unwrap()
                .data
                .is_empty()
        );
    }

    #[test]
    fn inherited_history_rejects_cycles_split_records_and_incorrect_ordinals() {
        let home = tempfile::tempdir().unwrap();
        let parent = Uuid::from_u128(1);
        let child = Uuid::from_u128(2);
        let parent_path = write(home.path(), parent, parent, &[meta(parent, "paginated")]);
        let prefix = fs::metadata(&parent_path).unwrap().len();
        for (offset, ordinal, expected) in [
            (prefix - 1, 1, "splits a record"),
            (prefix, 2, "incomplete"),
            (prefix + 1, 1, "incomplete"),
        ] {
            let mut metadata = meta(child, "paginated");
            metadata["payload"]["history_base"] = json!({"thread_id":parent,"end_byte_offset":offset,"end_ordinal_exclusive":ordinal});
            write(home.path(), child, child, &[metadata]);
            let error = Catalog::scan(home.path())
                .unwrap()
                .read(&child.to_string())
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
        let mut metadata = meta(child, "paginated");
        metadata["payload"]["history_base"] =
            json!({"thread_id":child,"end_byte_offset":0,"end_ordinal_exclusive":0});
        write(home.path(), child, child, &[metadata]);
        let error = Catalog::scan(home.path())
            .unwrap()
            .read(&child.to_string())
            .unwrap_err();
        assert!(error.to_string().contains("cyclic"));
    }

    #[test]
    fn subagent_cutoff_uses_root_history_mode_and_decompressed_budget_is_bounded() {
        let home = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(1);
        let mut metadata = meta(id, "paginated");
        metadata["payload"]["subagent_history_start_ordinal"] = json!(3);
        let path = write(
            home.path(),
            id,
            id,
            &[
                metadata,
                event(1, json!({"type":"task_started","turn_id":"inherited"})),
                event(
                    2,
                    json!({"type":"item_completed","turn_id":"inherited","item":{"type":"AgentMessage","id":"parent","content":[{"type":"Text","text":"parent answer"}]}}),
                ),
                event(3, json!({"type":"task_started","turn_id":"child"})),
                event(
                    4,
                    json!({"type":"agent_message","message":"legacy duplicate"}),
                ),
                event(
                    5,
                    json!({"type":"item_completed","turn_id":"child","item":{"type":"AgentMessage","id":"child","content":[{"type":"Text","text":"child answer"}]}}),
                ),
            ],
        );
        let bytes = fs::read(&path).unwrap();
        let compressed = path.with_extension("jsonl.zst");
        fs::write(
            &compressed,
            zstd::stream::encode_all(bytes.as_slice(), 1).unwrap(),
        )
        .unwrap();
        fs::remove_file(&path).unwrap();
        let catalog = Catalog::scan(home.path()).unwrap();
        let turns = catalog.read(&id.to_string()).unwrap().thread.turns.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id.as_str(), "child");
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].body(),ItemBody::AssistantText{text,..} if text=="child answer"));
        let mut remaining = bytes.len() as u64 - 1;
        let error = catalog
            .replay(
                &compressed,
                None,
                Some(3),
                &mut HashSet::new(),
                &mut remaining,
                &mut projection::Projection::new(true),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read limit"));
        let mut remaining = bytes.len() as u64;
        catalog
            .replay(
                &compressed,
                None,
                Some(3),
                &mut HashSet::new(),
                &mut remaining,
                &mut projection::Projection::new(true),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn oversized_index_entry_does_not_prevent_importing_other_conversations() {
        let home = tempfile::tempdir().unwrap();
        let bad = Uuid::from_u128(1);
        let good = Uuid::from_u128(2);
        let bad_path = write(home.path(), bad, bad, &[meta(bad, "legacy")]);
        write(
            home.path(),
            good,
            good,
            &[
                meta(good, "legacy"),
                event(1, json!({"type":"user_message","message":"healthy"})),
            ],
        );
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT,rollout_path TEXT,title TEXT,cwd TEXT,updated_at INTEGER,git_branch TEXT);").unwrap();
        db.execute(
            "INSERT INTO threads VALUES(?1,?2,?3,'/fixture',0,NULL)",
            rusqlite::params![
                bad.to_string(),
                bad_path.to_str().unwrap(),
                "x".repeat(16 * 1024 + 1)
            ],
        )
        .unwrap();
        drop(db);
        let catalog = Catalog::scan(home.path()).unwrap();
        assert_eq!(catalog.page("", None).unwrap().data.len(), 2);
        assert!(catalog.read(&bad.to_string()).is_err());
        assert_eq!(
            catalog
                .read(&good.to_string())
                .unwrap()
                .thread
                .turns
                .unwrap()
                .len(),
            1
        );
    }

    proptest::proptest! {
        #[test]
        fn rollout_filename_keeps_thread_and_revision_distinct(thread in proptest::prelude::any::<u128>(), revision in proptest::prelude::any::<u128>()) {
            let thread=Uuid::from_u128(thread);let revision=Uuid::from_u128(revision);
            let path=PathBuf::from(format!("rollout-2026-10-05T00-00-00-{thread}_{revision}.jsonl.zst"));
            proptest::prop_assert_eq!(filename(&path),Some((thread,revision)));
            let unrelated = PathBuf::from(format!("not-rollout-{thread}.jsonl"));
            proptest::prop_assert!(filename(&unrelated).is_none());
        }
    }
}
