//! Bounded filesystem walking and transcript reads.
use super::transcripts::{self, CodexState, Record};
use agent_protocol::usage::Provider;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const GUARD_LENGTH: usize = 64;

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedFile {
    pub size: u64,
    pub modified_ms: i64,
    pub provider: Provider,
    pub records: Vec<Record>,
    pub tail_records: Vec<Record>,
    pub malformed_records: u64,
    pub tail: Vec<u8>,
    pub resume_offset: u64,
    pub guard_length: u8,
    pub guard_hash: u32,
    pub head_hash: u32,
    pub tail_hash: u32,
    pub codex_state: Option<CodexState>,
    pub codex_occurrences: HashMap<String, u64>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ScanCache {
    pub files: std::collections::BTreeMap<PathBuf, CachedFile>,
}

#[derive(Debug, Default)]
pub(crate) struct ReadFiles {
    pub records: Vec<Record>,
    pub scanned_files: u64,
    pub skipped_files: u64,
    pub malformed_records: u64,
    pub sessions: BTreeSet<String>,
}

pub(crate) fn list_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                files.push(path);
                if files.len() > MAX_FILES {
                    return Err("使用履歴が多すぎるため、完全には読み込めません。".into());
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy)]
struct FileFingerprint {
    head_hash: u32,
    tail_hash: u32,
}

fn read_window(file: &mut fs::File, offset: u64, length: usize) -> std::io::Result<Vec<u8>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn fingerprint(path: &Path, size: u64) -> std::io::Result<FileFingerprint> {
    let mut file = fs::File::open(path)?;
    let head_length = size.min(GUARD_LENGTH as u64) as usize;
    let tail_length = size.min(GUARD_LENGTH as u64) as usize;
    let head = read_window(&mut file, 0, head_length)?;
    let tail = read_window(
        &mut file,
        size.saturating_sub(tail_length as u64),
        tail_length,
    )?;
    Ok(FileFingerprint {
        head_hash: fnv1a(&head),
        tail_hash: fnv1a(&tail),
    })
}

fn guard_matches(path: &Path, position: u64, length: u8, expected: u32) -> bool {
    let length = usize::from(length);
    if length == 0 || length > GUARD_LENGTH || position < length as u64 {
        return position == 0 && length == 0;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(bytes) = read_window(&mut file, position - length as u64, length) else {
        return false;
    };
    fnv1a(&bytes) == expected
}

fn recognized_timestamp(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::String(value)) => {
            chrono::DateTime::parse_from_rfc3339(value).is_ok()
        }
        Some(serde_json::Value::Number(value)) => {
            value.as_f64().is_some_and(|value| value.is_finite())
        }
        _ => false,
    }
}

fn recognized_usage_payload(text: &str, provider: Provider) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    match provider {
        Provider::Claude => {
            value.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
                && value
                    .get("message")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|message| {
                        message
                            .get("model")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|model| !model.is_empty())
                            && message
                                .get("usage")
                                .is_some_and(serde_json::Value::is_object)
                            && recognized_timestamp(value.get("timestamp"))
                    })
        }
        Provider::Codex => value
            .get("payload")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|payload| {
                payload.get("type").and_then(serde_json::Value::as_str) == Some("token_count")
                    && payload
                        .get("info")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|info| info.get("last_token_usage"))
                        .is_some_and(serde_json::Value::is_object)
                    && recognized_timestamp(value.get("timestamp"))
            }),
    }
}

fn complete_prefix_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or_default()
}

fn codex_dedupe_key(record: &Record, occurrences: &mut HashMap<String, u64>) -> String {
    let base = format!(
        "codex\0{}\0{}\0{}\0{}",
        record.session_id,
        record.timestamp_ms,
        record.model,
        serde_json::to_string(&record.totals).unwrap_or_default(),
    );
    let occurrence = occurrences.entry(base.clone()).or_default();
    *occurrence = occurrence.saturating_add(1);
    format!("{base}\0{occurrence}")
}

fn parse_line(
    line: &[u8],
    provider: Provider,
    state: &mut CodexState,
    occurrences: &mut HashMap<String, u64>,
    records: &mut Vec<Record>,
    malformed_records: &mut u64,
    count_malformed: bool,
) {
    if line.is_empty() {
        return;
    }
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let text = String::from_utf8_lossy(line);
    let candidate = match provider {
        Provider::Claude => text.contains("\"usage\""),
        Provider::Codex => text.contains("\"token_count\""),
    };
    let maybe = if !transcripts::might_carry_usage(&text, provider) {
        None
    } else {
        match provider {
            Provider::Claude => transcripts::parse_claude_line(&text),
            Provider::Codex => transcripts::parse_codex_line(&text, state),
        }
    };
    if let Some(mut record) = maybe {
        if provider == Provider::Codex && !record.session_id.is_empty() {
            record.dedupe_key = Some(codex_dedupe_key(&record, occurrences));
        }
        records.push(record);
    } else if count_malformed && candidate && !recognized_usage_payload(&text, provider) {
        *malformed_records = malformed_records.saturating_add(1);
    }
}

#[derive(Debug, Default)]
struct ParsedBytes {
    records: Vec<Record>,
    tail_records: Vec<Record>,
    malformed_records: u64,
    resume_offset: usize,
    state: CodexState,
    occurrences: HashMap<String, u64>,
}

fn parse_bytes(bytes: &[u8], provider: Provider, mut state: CodexState) -> ParsedBytes {
    let resume_offset = complete_prefix_len(bytes);
    let mut parsed = ParsedBytes {
        resume_offset,
        state: state.clone(),
        ..ParsedBytes::default()
    };
    for line in bytes[..resume_offset].split(|byte| *byte == b'\n') {
        parse_line(
            line,
            provider,
            &mut state,
            &mut parsed.occurrences,
            &mut parsed.records,
            &mut parsed.malformed_records,
            true,
        );
    }
    parsed.state = state;
    let tail = &bytes[resume_offset..];
    if !tail.is_empty() {
        let mut tail_state = parsed.state.clone();
        let mut tail_occurrences = parsed.occurrences.clone();
        let mut ignored_malformed = 0;
        parse_line(
            tail,
            provider,
            &mut tail_state,
            &mut tail_occurrences,
            &mut parsed.tail_records,
            &mut ignored_malformed,
            false,
        );
    }
    parsed
}

fn cached_entry(
    provider: Provider,
    size: u64,
    modified_ms: i64,
    bytes: &[u8],
    parsed: ParsedBytes,
) -> CachedFile {
    let resume_offset = parsed.resume_offset as u64;
    let guard_length = resume_offset.min(GUARD_LENGTH as u64) as u8;
    let guard_start = parsed
        .resume_offset
        .saturating_sub(usize::from(guard_length));
    let guard_hash = fnv1a(&bytes[guard_start..parsed.resume_offset]);
    let file_fingerprint = FileFingerprint {
        head_hash: fnv1a(&bytes[..bytes.len().min(GUARD_LENGTH)]),
        tail_hash: fnv1a(&bytes[bytes.len().saturating_sub(GUARD_LENGTH)..]),
    };
    CachedFile {
        size,
        modified_ms,
        provider,
        records: parsed.records,
        tail_records: parsed.tail_records,
        malformed_records: parsed.malformed_records,
        tail: bytes[parsed.resume_offset..].to_vec(),
        resume_offset,
        guard_length,
        guard_hash,
        head_hash: file_fingerprint.head_hash,
        tail_hash: file_fingerprint.tail_hash,
        codex_state: (provider == Provider::Codex).then_some(parsed.state),
        codex_occurrences: parsed.occurrences,
    }
}

fn append_cached(
    path: &Path,
    cached: &CachedFile,
    provider: Provider,
    size: u64,
    modified_ms: i64,
) -> std::io::Result<CachedFile> {
    let mut file = fs::File::open(path)?;
    // `cached.tail` starts at `resume_offset` and is the incomplete line
    // already retained from the previous scan. Read only the bytes appended
    // after that tail before prepending it for one complete parse.
    file.seek(SeekFrom::Start(
        cached
            .resume_offset
            .saturating_add(cached.tail.len() as u64),
    ))?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended)?;
    let mut bytes = Vec::with_capacity(cached.tail.len() + appended.len());
    bytes.extend_from_slice(&cached.tail);
    bytes.extend_from_slice(&appended);
    let mut parsed = parse_bytes(
        &bytes,
        provider,
        cached.codex_state.clone().unwrap_or_default(),
    );
    let mut records = cached.records.clone();
    let mut occurrences = cached.codex_occurrences.clone();
    // `parse_bytes` starts with an empty occurrence table. Rebuild appended
    // keys from the persisted count so a resume has exactly the same
    // within-file identity as a cold scan.
    for record in &mut parsed.records {
        if provider == Provider::Codex && !record.session_id.is_empty() {
            record.dedupe_key = Some(codex_dedupe_key(record, &mut occurrences));
        }
    }
    let mut tail_records = parsed.tail_records;
    if provider == Provider::Codex {
        let mut tail_occurrences = occurrences.clone();
        for record in &mut tail_records {
            if !record.session_id.is_empty() {
                record.dedupe_key = Some(codex_dedupe_key(record, &mut tail_occurrences));
            }
        }
    }
    records.append(&mut parsed.records);
    let resume_offset = cached.resume_offset + parsed.resume_offset as u64;
    let guard_length = resume_offset.min(GUARD_LENGTH as u64) as u8;
    let guard_hash = if resume_offset >= u64::from(guard_length) {
        let guard_start = resume_offset - u64::from(guard_length);
        if guard_start >= cached.resume_offset {
            let start = (guard_start - cached.resume_offset) as usize;
            let end = start + usize::from(guard_length);
            fnv1a(&bytes[start..end])
        } else {
            let mut guard = vec![0; usize::from(guard_length)];
            file.seek(SeekFrom::Start(guard_start))?;
            file.read_exact(&mut guard)?;
            fnv1a(&guard)
        }
    } else {
        fnv1a(&[])
    };
    let file_fingerprint = fingerprint(path, size)?;
    Ok(CachedFile {
        size,
        modified_ms,
        provider,
        records,
        tail_records,
        malformed_records: cached
            .malformed_records
            .saturating_add(parsed.malformed_records),
        tail: bytes[parsed.resume_offset..].to_vec(),
        resume_offset,
        guard_length,
        guard_hash,
        head_hash: file_fingerprint.head_hash,
        tail_hash: file_fingerprint.tail_hash,
        codex_state: (provider == Provider::Codex).then_some(parsed.state),
        codex_occurrences: occurrences,
    })
}

fn tail_matches(path: &Path, offset: u64, expected: &[u8]) -> bool {
    if expected.is_empty() {
        return true;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(bytes) = read_window(&mut file, offset, expected.len()) else {
        return false;
    };
    bytes == expected
}

pub(crate) fn read_files(
    root: &Path,
    provider: Provider,
    cache: &mut ScanCache,
) -> Result<ReadFiles, String> {
    let mut result = ReadFiles::default();
    let files = list_files(root)?;
    for path in files {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                result.skipped_files += 1;
                continue;
            }
        };
        if metadata.len() > MAX_FILE_BYTES {
            result.skipped_files += 1;
            continue;
        }
        let size = metadata.len();
        let modified_ms = modified_ms(&metadata);
        let cached = cache
            .files
            .get(&path)
            .filter(|cached| cached.provider == provider)
            .cloned();
        let entry = match cached {
            Some(cached) if cached.size == size => {
                let unchanged = fingerprint(&path, size).ok().is_some_and(|fingerprint| {
                    fingerprint.head_hash == cached.head_hash
                        && fingerprint.tail_hash == cached.tail_hash
                        && guard_matches(
                            &path,
                            cached.resume_offset,
                            cached.guard_length,
                            cached.guard_hash,
                        )
                        && tail_matches(&path, cached.resume_offset, &cached.tail)
                });
                if unchanged {
                    let mut cached = cached;
                    cached.modified_ms = modified_ms;
                    cached
                } else {
                    let bytes = match fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            result.skipped_files += 1;
                            continue;
                        }
                    };
                    cached_entry(
                        provider,
                        size,
                        modified_ms,
                        &bytes,
                        parse_bytes(&bytes, provider, CodexState::default()),
                    )
                }
            }
            Some(cached) if size > cached.size => {
                let can_resume = cached.resume_offset > 0
                    && fingerprint(&path, size).ok().is_some_and(|fingerprint| {
                        fingerprint.head_hash == cached.head_hash
                            && guard_matches(
                                &path,
                                cached.resume_offset,
                                cached.guard_length,
                                cached.guard_hash,
                            )
                            && tail_matches(&path, cached.resume_offset, &cached.tail)
                    });
                if can_resume {
                    match append_cached(&path, &cached, provider, size, modified_ms) {
                        Ok(entry) => entry,
                        Err(_) => {
                            result.skipped_files += 1;
                            continue;
                        }
                    }
                } else {
                    let bytes = match fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            result.skipped_files += 1;
                            continue;
                        }
                    };
                    cached_entry(
                        provider,
                        size,
                        modified_ms,
                        &bytes,
                        parse_bytes(&bytes, provider, CodexState::default()),
                    )
                }
            }
            _ => {
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        result.skipped_files += 1;
                        continue;
                    }
                };
                cached_entry(
                    provider,
                    size,
                    modified_ms,
                    &bytes,
                    parse_bytes(&bytes, provider, CodexState::default()),
                )
            }
        };
        cache.files.insert(path.clone(), entry.clone());
        result.scanned_files += 1;
        result.malformed_records += entry.malformed_records;
        for record in &entry.records {
            if !record.session_id.is_empty() {
                result.sessions.insert(record.session_id.clone());
            }
            result.records.push(record.clone());
        }
        for record in &entry.tail_records {
            if !record.session_id.is_empty() {
                result.sessions.insert(record.session_id.clone());
            }
            result.records.push(record.clone());
        }
    }
    Ok(result)
}

pub(crate) fn source_root(home: &Path, provider: Provider) -> PathBuf {
    home.join(match provider {
        Provider::Claude => "projects",
        Provider::Codex => "sessions",
    })
}

pub(crate) fn volume_id(path: &Path) -> String {
    let Ok(metadata) = fs::metadata(path) else {
        return String::new();
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::tempdir;

    fn claude_line(id: &str, output_tokens: u64) -> String {
        json!({
            "type": "assistant",
            "timestamp": "2030-01-01T00:00:00Z",
            "sessionId": "session",
            "requestId": format!("request-{id}"),
            "message": {
                "id": id,
                "model": "model",
                "usage": {"input_tokens": 1, "output_tokens": output_tokens}
            }
        })
        .to_string()
    }

    #[test]
    fn cache_reuses_unchanged_transcript_and_reports_malformed_usage_rows() {
        let directory = tempdir().unwrap();
        let projects = directory.path().join("projects");
        fs::create_dir_all(&projects).unwrap();
        let path = projects.join("session.jsonl");
        fs::write(
            &path,
            "{\"type\":\"assistant\",\"message\":{\"usage\":{}}}\n",
        )
        .unwrap();
        let mut cache = ScanCache::default();
        let first = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        assert_eq!(first.scanned_files, 1);
        assert_eq!(first.malformed_records, 1);
        let second = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        assert_eq!(second.scanned_files, 1);
        assert_eq!(cache.files.len(), 1);
    }

    #[test]
    fn invalid_usage_timestamp_is_reported_as_malformed() {
        let directory = tempdir().unwrap();
        let projects = directory.path().join("projects");
        fs::create_dir_all(&projects).unwrap();
        fs::write(
            projects.join("session.jsonl"),
            "{\"type\":\"assistant\",\"timestamp\":\"not-a-date\",\"message\":{\"model\":\"model\",\"usage\":{}}}\n",
        )
        .unwrap();
        let result = read_files(&projects, Provider::Claude, &mut ScanCache::default()).unwrap();
        assert_eq!(result.malformed_records, 1);
    }

    #[test]
    fn codex_copies_get_matching_occurrence_keys_across_files() {
        let directory = tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let lines = [
            json!({
                "type":"session_meta",
                "timestamp":"2030-01-01T00:00:00Z",
                "payload":{"id":"session"}
            }),
            json!({
                "type":"turn_context",
                "timestamp":"2030-01-01T00:00:00Z",
                "payload":{"model":"model"}
            }),
            json!({
                "timestamp":"2030-01-01T00:00:01Z",
                "payload":{"type":"token_count","info":{"last_token_usage":{
                    "input_tokens":10,"cached_input_tokens":2,"output_tokens":3
                }}}
            }),
        ]
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
        let contents = format!("{}\n", lines.join("\n"));
        fs::write(sessions.join("a.jsonl"), &contents).unwrap();
        fs::write(sessions.join("b.jsonl"), &contents).unwrap();
        let result = read_files(&sessions, Provider::Codex, &mut ScanCache::default()).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.records[0].dedupe_key, result.records[1].dedupe_key);
        assert_eq!(result.malformed_records, 0);
    }

    #[test]
    fn incomplete_tail_is_replaced_when_the_writer_finishes_the_line() {
        let directory = tempdir().unwrap();
        let projects = directory.path().join("projects");
        fs::create_dir_all(&projects).unwrap();
        let path = projects.join("session.jsonl");
        let complete = claude_line("first", 5);
        let tail = claude_line("second", 7);
        fs::write(&path, format!("{complete}\n{tail}")).unwrap();

        let mut cache = ScanCache::default();
        let first = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        let cached = cache.files.get(&path).unwrap();
        assert_eq!(first.records.len(), 2);
        assert_eq!(cached.records.len(), 1);
        assert_eq!(cached.tail_records.len(), 1);
        assert!(!cached.tail.is_empty());

        fs::write(&path, format!("{complete}\n{tail}\n")).unwrap();
        let second = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        let cached = cache.files.get(&path).unwrap();
        assert_eq!(second.records.len(), 2);
        assert_eq!(cached.records.len(), 2);
        assert!(cached.tail_records.is_empty());
        assert!(cached.tail.is_empty());
    }

    #[test]
    fn codex_append_preserves_model_and_duplicate_scan_state() {
        let directory = tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("history.jsonl");
        let first = [
            json!({"type":"session_meta","timestamp":"2030-01-01T00:00:00Z","payload":{"id":"session"}}),
            json!({"type":"turn_context","timestamp":"2030-01-01T00:00:00Z","payload":{"model":"model-one"}}),
            json!({"timestamp":"2030-01-01T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":3}}}}),
        ]
        .into_iter()
        .map(|line| line.to_string() + "\n")
        .collect::<String>();
        fs::write(&path, &first).unwrap();
        let mut cache = ScanCache::default();
        let initial = read_files(&sessions, Provider::Codex, &mut cache).unwrap();
        assert_eq!(initial.records.len(), 1);

        let duplicate = json!({"timestamp":"2030-01-01T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":3}}}});
        let appended = [
            duplicate,
            json!({"type":"turn_context","timestamp":"2030-01-01T00:00:02Z","payload":{"model":"model-two"}}),
            json!({"timestamp":"2030-01-01T00:00:03Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":12,"output_tokens":4}}}}),
        ]
        .into_iter()
        .map(|line| line.to_string() + "\n")
        .collect::<String>();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(appended.as_bytes())
            .unwrap();

        let result = read_files(&sessions, Provider::Codex, &mut cache).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.records[0].model, "model-one");
        assert_eq!(result.records[1].model, "model-two");
        assert_eq!(
            cache
                .files
                .get(&path)
                .unwrap()
                .codex_state
                .as_ref()
                .unwrap()
                .model,
            "model-two"
        );
    }

    #[test]
    fn codex_fork_suppression_state_survives_an_incremental_scan() {
        let directory = tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("fork.jsonl");
        let first = [
            json!({"type":"session_meta","timestamp":"2030-01-01T00:00:00Z","payload":{"id":"session","forked_from_id":"parent"}}),
            json!({"type":"turn_context","timestamp":"2030-01-01T00:00:00Z","payload":{"model":"model"}}),
            json!({"timestamp":"2030-01-01T00:00:00.500Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":3}}}}),
        ]
        .into_iter()
        .map(|line| line.to_string() + "\n")
        .collect::<String>();
        fs::write(&path, first).unwrap();
        let mut cache = ScanCache::default();
        let initial = read_files(&sessions, Provider::Codex, &mut cache).unwrap();
        assert!(initial.records.is_empty());
        assert!(
            cache
                .files
                .get(&path)
                .unwrap()
                .codex_state
                .as_ref()
                .unwrap()
                .suppressing_fork_copies
        );

        let next = json!({"timestamp":"2030-01-01T00:00:02Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":12,"output_tokens":4}}}}).to_string() + "\n";
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(next.as_bytes())
            .unwrap();
        let resumed = read_files(&sessions, Provider::Codex, &mut cache).unwrap();
        assert_eq!(resumed.records.len(), 1);
        assert_eq!(resumed.records[0].totals.output_tokens, 4);
    }

    #[test]
    fn replacement_and_truncation_never_serve_the_old_records() {
        let directory = tempdir().unwrap();
        let projects = directory.path().join("projects");
        fs::create_dir_all(&projects).unwrap();
        let path = projects.join("session.jsonl");
        let first = claude_line("first", 5);
        let replacement = claude_line("other", 9);
        fs::write(&path, format!("{first}\n")).unwrap();
        let mut cache = ScanCache::default();
        assert_eq!(
            read_files(&projects, Provider::Claude, &mut cache)
                .unwrap()
                .records[0]
                .totals
                .output_tokens,
            5
        );

        fs::write(&path, format!("{replacement}\n")).unwrap();
        let replaced = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        assert_eq!(replaced.records.len(), 1);
        assert_eq!(replaced.records[0].totals.output_tokens, 9);

        fs::write(&path, "").unwrap();
        let truncated = read_files(&projects, Provider::Claude, &mut cache).unwrap();
        assert!(truncated.records.is_empty());
        assert!(cache.files.get(&path).unwrap().records.is_empty());
    }

    proptest! {
        #[test]
        fn complete_prefix_always_ends_at_a_newline(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let end = complete_prefix_len(&bytes);
            prop_assert!(end <= bytes.len());
            prop_assert!(end == 0 || bytes[end - 1] == b'\n');
            prop_assert!(!bytes[end..].contains(&b'\n'));
        }
    }
}
