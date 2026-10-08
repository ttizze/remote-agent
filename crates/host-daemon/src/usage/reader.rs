//! Bounded filesystem walking and transcript reads.
use super::transcripts::{self, CodexState, Record};
use agent_protocol::usage::Provider;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedFile {
    pub size: u64,
    pub modified_ms: i64,
    pub provider: Provider,
    pub records: Vec<Record>,
    pub malformed_records: u64,
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
        let cached = cache.files.get(&path).filter(|cached| {
            cached.size == size && cached.modified_ms == modified_ms && cached.provider == provider
        });
        let entry = if let Some(cached) = cached {
            cached.clone()
        } else {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => {
                    result.skipped_files += 1;
                    continue;
                }
            };
            let mut state = CodexState::default();
            let mut records = Vec::new();
            let mut malformed = 0;
            let mut codex_occurrences = HashMap::<String, u64>::new();
            for line in bytes.split(|byte| *byte == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let text = String::from_utf8_lossy(line);
                let usage_line = match provider {
                    Provider::Claude => text.contains("\"usage\""),
                    Provider::Codex => text.contains("\"token_count\""),
                };
                let maybe = match provider {
                    Provider::Claude => {
                        if !transcripts::might_carry_usage(&text, provider) {
                            None
                        } else {
                            transcripts::parse_claude_line(&text)
                        }
                    }
                    Provider::Codex => {
                        if !transcripts::might_carry_usage(&text, provider) {
                            None
                        } else {
                            transcripts::parse_codex_line(&text, &mut state)
                        }
                    }
                };
                if let Some(mut record) = maybe {
                    if provider == Provider::Codex && !record.session_id.is_empty() {
                        let base = format!(
                            "codex\0{}\0{}\0{}\0{}",
                            record.session_id,
                            record.timestamp_ms,
                            record.model,
                            serde_json::to_string(&record.totals).unwrap_or_default(),
                        );
                        let occurrence = codex_occurrences.entry(base.clone()).or_default();
                        *occurrence = occurrence.saturating_add(1);
                        record.dedupe_key = Some(format!("{base}\0{occurrence}"));
                    }
                    records.push(record);
                } else if usage_line && !recognized_usage_payload(&text, provider) {
                    malformed += 1;
                }
            }
            let entry = CachedFile {
                size,
                modified_ms,
                provider,
                records,
                malformed_records: malformed,
            };
            cache.files.insert(path.clone(), entry.clone());
            entry
        };
        result.scanned_files += 1;
        result.malformed_records += entry.malformed_records;
        for record in &entry.records {
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
        return format!("{}:{}", metadata.dev(), metadata.ino());
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
    use serde_json::json;
    use tempfile::tempdir;

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
}
