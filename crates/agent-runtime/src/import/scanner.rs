//! Read-only, bounded discovery of transcripts and the projects they ran in.
use super::fs::{EntryKind, FileStat, TranscriptFs};
use super::git::{GitIdentity, ProjectGit, read_git_identity};
use super::json::{LimitExceeded, RecordReader};
use super::paths::{Exclusions, comparison_key, expand_home, resolve};
use super::record::{
    MAX_IMPORT_RECORDS, Record, SessionThread, TranscriptMeta, decode, line_cwd, parse_records,
    record_cwd, retained, selected,
};
use crate::Clock;
use agent_domain::{Driver, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MIB: u64 = 1024 * 1024;
/// Chunk size for full transcript reads.
const TRANSCRIPT_CHUNK_BYTES: u64 = 32 * 1024;
/// Small reads keep long Codex instruction headers from spending the metadata budget.
const METADATA_READ_BYTES: u64 = 8 * 1024;
const MAX_TRANSCRIPT_SCAN_BYTES: u64 = MIB;
/// Newest-first ordering means this cap only drops stale sessions.
pub const MAX_TRANSCRIPTS_PER_SOURCE: usize = 5000;
const MAX_DISCOVERY_OPERATIONS_PER_SOURCE: usize = MAX_TRANSCRIPTS_PER_SOURCE * 4;
const MAX_METADATA_BYTES_PER_SOURCE: u64 = 64 * MIB;
const MAX_METADATA_OPERATIONS_PER_SOURCE: usize = MAX_TRANSCRIPTS_PER_SOURCE * 4;
const MAX_METADATA_RECORDS_PER_SOURCE: usize = 100_000;
const MAX_METADATA_RECORDS_PER_TRANSCRIPT: usize = 1_000;
pub const RECENT_THREAD_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Tool results (screenshots) can make an ordinary transcript several GiB; reads
/// are streamed and only the selected history is held.
const MAX_IMPORTED_TRANSCRIPT_BYTES: u64 = 4 * 1024 * MIB;
const MAX_IMPORT_HISTORY_BYTES: usize = 32 * MIB as usize;
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * MIB;
pub const MAX_IMPORT_TRANSCRIPTS: usize = 100;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A provider instance's transcript home: Codex `CODEX_HOME`, Claude `CLAUDE_CONFIG_DIR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportHome {
    pub driver: Driver,
    pub instance: String,
    pub path: PathBuf,
}

/// The built-in instance owns sessions in a home that several instances share.
pub fn default_instance(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "codex",
        Driver::Claude => "claude",
    }
}

#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Enabled instances in configured order.
    pub homes: Vec<ImportHome>,
    /// The user's home: `~` expansion and the home, Downloads and Codex scratch exclusions.
    pub home_dir: PathBuf,
    pub temp_dir: PathBuf,
    /// The Host's own data and worktree directories; nothing under them is a project.
    pub managed_dirs: Vec<PathBuf>,
}

/// A registered project, owned outside the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProject {
    pub id: String,
    pub root: PathBuf,
}

/// One imported transcript file. A replaced file has a new identity and is read again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSource {
    pub provider: Driver,
    pub instance: String,
    pub session: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<u64>,
    pub birthtime_ms: Option<i64>,
}

impl ImportSource {
    fn identity(&self) -> Identity {
        Identity {
            path: self.path.clone(),
            size: self.size,
            mtime_ms: self.mtime_ms,
            device: self.device,
            inode: self.inode,
            birthtime_ms: self.birthtime_ms,
        }
    }
    pub(crate) fn fingerprint(&self) -> String {
        let optional = |value: Option<i64>| value.map_or_else(|| "-".to_owned(), |v| v.to_string());
        format!(
            "{}:{}:{}:{}:{}",
            self.size,
            optional(self.mtime_ms),
            self.device,
            self.inode.map_or_else(|| "-".to_owned(), |v| v.to_string()),
            optional(self.birthtime_ms)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    path: PathBuf,
    size: u64,
    mtime_ms: Option<i64>,
    device: u64,
    inode: Option<u64>,
    birthtime_ms: Option<i64>,
}

fn identity(path: &Path, stat: &FileStat) -> Identity {
    Identity {
        path: path.to_path_buf(),
        size: stat.size,
        mtime_ms: stat.mtime_ms,
        device: stat.device,
        inode: stat.inode,
        birthtime_ms: stat.birthtime_ms,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecentThread {
    Importable {
        thread: SessionThread,
        source: ImportSource,
    },
    AlreadyImported {
        source: ImportSource,
    },
    /// Another copy of a session this run already yielded.
    Duplicate {
        source: ImportSource,
    },
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectCandidate {
    pub path: PathBuf,
    pub title: String,
    pub project: Option<String>,
    pub sources: Vec<Driver>,
    pub thread_count: usize,
    pub last_active_at: Option<Timestamp>,
    pub already_imported: bool,
    /// `None` when the directory is not the root of a git repository.
    pub git: Option<ProjectGit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanResult {
    pub candidates: Vec<ProjectCandidate>,
    pub scanned_at: Timestamp,
    pub truncated: bool,
}

struct TranscriptCandidate {
    path: PathBuf,
    mtime_ms: i64,
    instance: String,
    size: u64,
}

#[derive(Debug, Clone)]
struct RawCandidate {
    cwd: String,
    source: Driver,
    instance: String,
    last_active_ms: i64,
    transcripts: Vec<(PathBuf, i64)>,
}

struct MetadataBudget {
    bytes: u64,
    operations: usize,
    records: usize,
    truncated: bool,
}

struct DirectoryBudget {
    remaining: usize,
    truncated: bool,
}
impl DirectoryBudget {
    fn spent(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
        }
        self.remaining == 0
    }
    fn read_dir(&mut self, fs: &dyn TranscriptFs, dir: &Path) -> Vec<String> {
        if self.spent() {
            return Vec::new();
        }
        self.remaining -= 1;
        fs.read_dir(dir).unwrap_or_default()
    }
    fn stat_transcript(
        &mut self,
        fs: &dyn TranscriptFs,
        path: PathBuf,
        instance: &str,
        found: &mut Vec<TranscriptCandidate>,
    ) {
        self.remaining -= 1;
        if let Ok(stat) = fs.stat(&path)
            && stat.kind == EntryKind::File
            && let Some(mtime_ms) = stat.mtime_ms
        {
            found.push(TranscriptCandidate {
                path,
                mtime_ms,
                instance: instance.to_owned(),
                size: stat.size,
            });
        }
    }
}

fn newest_first(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.reverse();
    names
}

/// Gives each account a turn before taking another file from the same home.
fn round_robin(transcripts: Vec<TranscriptCandidate>) -> Vec<TranscriptCandidate> {
    let mut groups: Vec<(String, VecDeque<TranscriptCandidate>)> = Vec::new();
    for transcript in transcripts {
        match groups
            .iter_mut()
            .find(|(instance, _)| *instance == transcript.instance)
        {
            Some((_, group)) => group.push_back(transcript),
            None => groups.push((transcript.instance.clone(), VecDeque::from([transcript]))),
        }
    }
    let mut selected = Vec::new();
    while selected.len() < MAX_TRANSCRIPTS_PER_SOURCE && groups.iter().any(|(_, g)| !g.is_empty()) {
        for (_, group) in &mut groups {
            if selected.len() == MAX_TRANSCRIPTS_PER_SOURCE {
                break;
            }
            if let Some(next) = group.pop_front() {
                selected.push(next);
            }
        }
    }
    selected
}

struct HistoryBudget {
    history: usize,
    record: usize,
}
impl HistoryBudget {
    fn reserve(&mut self, bytes: usize) -> Result<(), LimitExceeded> {
        self.record += bytes;
        if self.history + self.record > MAX_IMPORT_HISTORY_BYTES {
            return Err(LimitExceeded);
        }
        Ok(())
    }
}

/// Splits streamed bytes into records and keeps the ones the parser can use.
struct Collector {
    source: Driver,
    limit: usize,
    reader: RecordReader,
    budget: HistoryBudget,
    records: Vec<Record>,
    count: usize,
    started: bool,
}
impl Collector {
    /// `Ok(false)` once the record limit is passed.
    fn feed(&mut self, chunk: &[u8]) -> Result<bool, LimitExceeded> {
        let mut start = 0;
        while start < chunk.len() {
            let newline = chunk[start..].iter().position(|&b| b == b'\n');
            let end = newline.map_or(chunk.len(), |at| start + at);
            self.started = true;
            let budget = &mut self.budget;
            self.reader
                .write(&chunk[start..end], &mut |bytes| budget.reserve(bytes))?;
            if newline.is_none() {
                break;
            }
            if !self.finish_record()? {
                return Ok(false);
            }
            start = end + 1;
        }
        Ok(true)
    }

    fn finish_record(&mut self) -> Result<bool, LimitExceeded> {
        self.count += 1;
        if self.count > self.limit {
            return Ok(false);
        }
        let reader = std::mem::replace(&mut self.reader, RecordReader::new(selected));
        let budget = &mut self.budget;
        let value = reader.finish(&mut |bytes| budget.reserve(bytes))?;
        if let Some(record) = value.as_ref().and_then(decode)
            && retained(self.source, &record)
        {
            self.records.push(record);
            self.budget.history += self.budget.record;
        }
        self.budget.record = 0;
        self.started = false;
        Ok(true)
    }
}

struct Snapshot {
    records: Vec<Record>,
    count: usize,
}

/// Transcript discovery and reading. Candidates found by `scan` are cached for
/// later `recent_threads` calls, so use one scanner per import session.
#[derive(Clone)]
pub struct Scanner {
    inner: Arc<Inner>,
}
struct Inner {
    config: ScanConfig,
    fs: Arc<dyn TranscriptFs>,
    clock: Arc<dyn Clock>,
    exclusions: Exclusions,
    cache: Mutex<Option<Arc<Vec<RawCandidate>>>>,
    /// One transcript holds the selected-history budget at a time.
    reading: Mutex<()>,
}

impl Scanner {
    pub fn new(config: ScanConfig, fs: Arc<dyn TranscriptFs>, clock: Arc<dyn Clock>) -> Self {
        let exclusions = Exclusions::new(
            &config.home_dir,
            &config.temp_dir,
            &config.managed_dirs,
            |path| fs.real_path(path).ok(),
        );
        Self {
            inner: Arc::new(Inner {
                config,
                fs,
                clock,
                exclusions,
                cache: Mutex::new(None),
                reading: Mutex::new(()),
            }),
        }
    }

    fn fs(&self) -> &dyn TranscriptFs {
        self.inner.fs.as_ref()
    }

    fn excluded(&self, path: &Path) -> bool {
        self.inner.exclusions.excluded(path)
    }

    fn expand(&self, value: &str) -> PathBuf {
        expand_home(value, &self.inner.config.home_dir)
    }

    /// Matches directory aliases without assuming a case-insensitive volume.
    fn directory_identity(&self, target: &Path, known: Option<&FileStat>) -> String {
        let resolved = resolve(target);
        let stat = known.copied().or_else(|| self.fs().stat(&resolved).ok());
        if let Some(FileStat {
            inode: Some(inode),
            device,
            ..
        }) = stat
            && inode > 0
            && inode <= MAX_SAFE_INTEGER
        {
            return format!("inode:{device}:{inode}");
        }
        let real = self.fs().real_path(&resolved).unwrap_or(resolved);
        format!("path:{}", comparison_key(&real))
    }

    /// A large history snapshot can precede session metadata, so complete records
    /// are read in bounded chunks until one names its cwd.
    fn read_cwd(
        &self,
        transcript: &TranscriptCandidate,
        budget: &mut MetadataBudget,
    ) -> Option<String> {
        if transcript.size == 0 {
            return None;
        }
        if budget.bytes == 0 || budget.operations < 2 || budget.records == 0 {
            budget.truncated = true;
            return None;
        }
        budget.operations -= 1;
        let mut file = self.fs().open(&transcript.path).ok()?;
        let mut remaining: Vec<u8> = Vec::new();
        let mut bytes_read = 0;
        let mut records_read = 0;
        let max_bytes = MAX_TRANSCRIPT_SCAN_BYTES.min(transcript.size);
        let mut reserve_record = |budget: &mut MetadataBudget| {
            if records_read == MAX_METADATA_RECORDS_PER_TRANSCRIPT || budget.records == 0 {
                budget.truncated = true;
                return false;
            }
            records_read += 1;
            budget.records -= 1;
            true
        };
        let read_last =
            |remaining: &[u8],
             budget: &mut MetadataBudget,
             reserve_record: &mut dyn FnMut(&mut MetadataBudget) -> bool| {
                if remaining.is_empty() || !reserve_record(budget) {
                    return None;
                }
                line_cwd(String::from_utf8_lossy(remaining).trim())
            };
        while bytes_read < max_bytes {
            if budget.bytes == 0 || budget.operations == 0 {
                budget.truncated = true;
                return None;
            }
            let size = METADATA_READ_BYTES
                .min(max_bytes - bytes_read)
                .min(budget.bytes);
            budget.operations -= 1;
            budget.bytes -= size;
            let chunk = match file.read(size as usize) {
                Err(_) => return None,
                Ok(None) => return read_last(&remaining, budget, &mut reserve_record),
                Ok(Some(chunk)) => chunk,
            };
            bytes_read += chunk.len() as u64;
            let searched = remaining.len();
            remaining.extend_from_slice(&chunk);
            let mut start = 0;
            let mut from = searched;
            while let Some(at) = remaining[from..].iter().position(|&b| b == b'\n') {
                let newline = from + at;
                if !reserve_record(budget) {
                    return None;
                }
                if let Some(cwd) =
                    line_cwd(String::from_utf8_lossy(&remaining[start..newline]).trim())
                {
                    return Some(cwd);
                }
                start = newline + 1;
                from = start;
            }
            remaining.drain(..start);
        }
        if bytes_read < transcript.size {
            budget.truncated = true;
            return None;
        }
        read_last(&remaining, budget, &mut reserve_record)
    }

    /// Projects history fields while reading and checks the file identity on both sides
    /// of the read. A history budget failure rejects the whole transcript.
    fn read_transcript(
        &self,
        path: &Path,
        expected: &Identity,
        record_limit: usize,
        source: Driver,
    ) -> Option<Snapshot> {
        if expected.size > MAX_IMPORTED_TRANSCRIPT_BYTES {
            return None;
        }
        let mut file = match self.fs().open(path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "could not read an imported transcript");
                return None;
            }
        };
        let same = |file: &dyn super::TranscriptFile| {
            file.stat()
                .is_ok_and(|stat| identity(path, &stat) == *expected)
        };
        if !same(file.as_ref()) {
            return None;
        }
        let mut collector = Collector {
            source,
            limit: record_limit,
            reader: RecordReader::new(selected),
            budget: HistoryBudget {
                history: 0,
                record: 0,
            },
            records: Vec::new(),
            count: 0,
            started: false,
        };
        let mut bytes_read = 0;
        while bytes_read < expected.size {
            let chunk = match file
                .read(TRANSCRIPT_CHUNK_BYTES.min(expected.size - bytes_read) as usize)
            {
                Ok(Some(chunk)) => chunk,
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "could not read an imported transcript");
                    return None;
                }
            };
            bytes_read += chunk.len() as u64;
            match collector.feed(&chunk) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(LimitExceeded) => {
                    tracing::warn!(path = %path.display(), "imported transcript exceeds the history budget");
                    return None;
                }
            }
        }
        if collector.started && !collector.finish_record().unwrap_or(false) {
            return None;
        }
        same(file.as_ref()).then_some(Snapshot {
            records: collector.records,
            count: collector.count,
        })
    }

    fn discover_claude(
        &self,
        home: &Path,
        instance: &str,
        budget: &mut DirectoryBudget,
    ) -> Vec<TranscriptCandidate> {
        let projects = home.join("projects");
        let mut found = Vec::new();
        for directory in budget.read_dir(self.fs(), &projects) {
            if budget.spent() {
                break;
            }
            let directory = projects.join(directory);
            let files: Vec<PathBuf> = budget
                .read_dir(self.fs(), &directory)
                .into_iter()
                .filter(|entry| entry.ends_with(".jsonl"))
                .map(|entry| directory.join(entry))
                .collect();
            for file in files {
                if budget.spent() {
                    break;
                }
                budget.stat_transcript(self.fs(), file, instance, &mut found);
            }
        }
        found
    }

    /// Date-partitioned directories sort chronologically, so walking them in reverse
    /// spends each home's budget on recent sessions.
    fn discover_codex(
        &self,
        home: &Path,
        instance: &str,
        budget: &mut DirectoryBudget,
    ) -> Vec<TranscriptCandidate> {
        let sessions = home.join("sessions");
        let mut found = Vec::new();
        for year in newest_first(budget.read_dir(self.fs(), &sessions)) {
            if budget.spent() {
                break;
            }
            let year = sessions.join(year);
            for month in newest_first(budget.read_dir(self.fs(), &year)) {
                if budget.spent() {
                    break;
                }
                let month = year.join(month);
                for day in newest_first(budget.read_dir(self.fs(), &month)) {
                    if budget.spent() {
                        break;
                    }
                    let day = month.join(day);
                    for entry in newest_first(budget.read_dir(self.fs(), &day)) {
                        if !entry.starts_with("rollout-") || !entry.ends_with(".jsonl") {
                            continue;
                        }
                        if budget.spent() {
                            break;
                        }
                        budget.stat_transcript(self.fs(), day.join(entry), instance, &mut found);
                    }
                }
            }
        }
        found
    }

    fn group_by_cwd(
        &self,
        source: Driver,
        transcripts: Vec<TranscriptCandidate>,
        budget: &mut MetadataBudget,
    ) -> Vec<RawCandidate> {
        let mut groups: Vec<RawCandidate> = Vec::new();
        let mut index: HashMap<(String, String), usize> = HashMap::new();
        for transcript in transcripts {
            let Some(cwd) = self.read_cwd(&transcript, budget) else {
                continue;
            };
            match index.get(&(transcript.instance.clone(), cwd.clone())) {
                Some(&at) => {
                    let group = &mut groups[at];
                    group.last_active_ms = group.last_active_ms.max(transcript.mtime_ms);
                    group
                        .transcripts
                        .push((transcript.path, transcript.mtime_ms));
                }
                None => {
                    index.insert((transcript.instance.clone(), cwd.clone()), groups.len());
                    groups.push(RawCandidate {
                        cwd,
                        source,
                        instance: transcript.instance,
                        last_active_ms: transcript.mtime_ms,
                        transcripts: vec![(transcript.path, transcript.mtime_ms)],
                    });
                }
            }
        }
        groups
    }

    fn collect(&self) -> (Vec<RawCandidate>, bool) {
        let mut raw = Vec::new();
        let mut truncated = false;
        for source in [Driver::Claude, Driver::Codex] {
            let mut instances: Vec<&ImportHome> = self
                .inner
                .config
                .homes
                .iter()
                .filter(|home| home.driver == source)
                .collect();
            // A shared home holds one copy of each session; the built-in instance owns it.
            instances.sort_by_key(|home| home.instance != default_instance(source));
            let mut seen = HashSet::new();
            let mut homes = Vec::new();
            for home in instances {
                let path = resolve(&home.path);
                if seen.insert(self.directory_identity(&path, None)) {
                    homes.push((path, home.instance.as_str()));
                }
            }
            let count = homes.len().max(1);
            let (base, extra) = (
                MAX_DISCOVERY_OPERATIONS_PER_SOURCE / count,
                MAX_DISCOVERY_OPERATIONS_PER_SOURCE % count,
            );
            let mut transcripts = Vec::new();
            for (index, (path, instance)) in homes.iter().enumerate() {
                let operations = base + usize::from(index < extra);
                if operations == 0 {
                    truncated = true;
                    continue;
                }
                let mut budget = DirectoryBudget {
                    remaining: operations,
                    truncated: false,
                };
                transcripts.extend(match source {
                    Driver::Claude => self.discover_claude(path, instance, &mut budget),
                    Driver::Codex => self.discover_codex(path, instance, &mut budget),
                });
                truncated |= budget.truncated;
            }
            transcripts.sort_by(|left, right| {
                right
                    .mtime_ms
                    .cmp(&left.mtime_ms)
                    .then_with(|| left.path.cmp(&right.path))
            });
            if transcripts.len() > MAX_TRANSCRIPTS_PER_SOURCE {
                truncated = true;
            }
            let mut budget = MetadataBudget {
                bytes: MAX_METADATA_BYTES_PER_SOURCE,
                operations: MAX_METADATA_OPERATIONS_PER_SOURCE,
                records: MAX_METADATA_RECORDS_PER_SOURCE,
                truncated: false,
            };
            raw.extend(self.group_by_cwd(source, round_robin(transcripts), &mut budget));
            truncated |= budget.truncated;
        }
        (raw, truncated)
    }

    fn candidates(&self) -> Arc<Vec<RawCandidate>> {
        if let Some(cached) = self.inner.cache.lock().expect("scanner cache").clone() {
            return cached;
        }
        let collected = Arc::new(self.collect().0);
        *self.inner.cache.lock().expect("scanner cache") = Some(collected.clone());
        collected
    }

    /// Directories the configured homes have run sessions in, newest first, marking
    /// those already registered as projects. Blocking.
    pub fn scan(&self, projects: &[ImportProject]) -> ScanResult {
        let (raw, truncated) = self.collect();
        *self.inner.cache.lock().expect("scanner cache") = Some(Arc::new(raw.clone()));

        struct Merged {
            path: PathBuf,
            sources: Vec<Driver>,
            thread_count: usize,
            last_active_ms: i64,
        }
        let mut merged: Vec<(String, Merged)> = Vec::new();
        let mut directory_keys: HashMap<PathBuf, Option<String>> = HashMap::new();
        let mut git: HashMap<String, Option<ProjectGit>> = HashMap::new();
        for candidate in &raw {
            let expanded = self.expand(candidate.cwd.trim());
            if !expanded.is_absolute() {
                continue;
            }
            let resolved = resolve(&expanded);
            if self.excluded(&resolved) {
                continue;
            }
            let key = match directory_keys.get(&resolved) {
                Some(key) => key.clone(),
                None => {
                    let key = self.directory_key(&resolved, &mut git);
                    directory_keys.insert(resolved.clone(), key.clone());
                    key
                }
            };
            let Some(key) = key else { continue };
            match merged.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, entry)) => {
                    if !entry.sources.contains(&candidate.source) {
                        entry.sources.push(candidate.source);
                    }
                    entry.thread_count += candidate.transcripts.len();
                    entry.last_active_ms = entry.last_active_ms.max(candidate.last_active_ms);
                }
                None => merged.push((
                    key,
                    Merged {
                        path: resolved,
                        sources: vec![candidate.source],
                        thread_count: candidate.transcripts.len(),
                        last_active_ms: candidate.last_active_ms,
                    },
                )),
            }
        }

        // A project and a transcript can name different symlinks to one directory.
        let mut registered: HashMap<String, &ImportProject> = HashMap::new();
        for project in projects {
            let root = resolve(&self.expand(&project.root.to_string_lossy()));
            registered.insert(comparison_key(&root), project);
            registered.insert(self.directory_identity(&root, None), project);
        }
        let mut candidates: Vec<ProjectCandidate> = merged
            .into_iter()
            .map(|(key, entry)| {
                let project = registered
                    .get(&comparison_key(&entry.path))
                    .or_else(|| registered.get(&key));
                let path = project.map_or_else(|| entry.path.clone(), |p| p.root.clone());
                ProjectCandidate {
                    title: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned()),
                    path,
                    project: project.map(|p| p.id.clone()),
                    sources: entry.sources,
                    thread_count: entry.thread_count,
                    last_active_at: Timestamp::from_millis(entry.last_active_ms).ok(),
                    already_imported: project.is_some(),
                    git: git.get(&key).cloned().flatten(),
                }
            })
            .collect();
        candidates.sort_by(
            |left, right| match (&left.last_active_at, &right.last_active_at) {
                (Some(l), Some(r)) if l != r => r.cmp(l),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => left.path.cmp(&right.path),
            },
        );
        ScanResult {
            candidates,
            scanned_at: self.inner.clock.now(),
            truncated,
        }
    }

    /// The identity key of an existing, non-excluded directory that is not a linked worktree.
    fn directory_key(
        &self,
        resolved: &Path,
        git: &mut HashMap<String, Option<ProjectGit>>,
    ) -> Option<String> {
        let stat = self.fs().stat(resolved).ok()?;
        if stat.kind != EntryKind::Directory {
            return None;
        }
        // A symlink can point into an excluded directory under an innocent spelling.
        let real = self
            .fs()
            .real_path(resolved)
            .unwrap_or_else(|_| resolved.to_path_buf());
        if self.excluded(&real) {
            return None;
        }
        let identity = match read_git_identity(self.fs(), resolved) {
            GitIdentity::Worktree => return None,
            GitIdentity::Repository(repository) => Some(repository),
            GitIdentity::NotGit => None,
        };
        let key = self.directory_identity(resolved, Some(&stat));
        git.insert(key.clone(), identity);
        Some(key)
    }

    /// Recent sessions whose cwd is exactly `root`, newest first and read lazily.
    /// `completed` transcripts with an unchanged identity are not read again. Blocking.
    pub fn recent_threads(&self, root: &Path, completed: &[ImportSource]) -> RecentThreads {
        let root = resolve(&self.expand(&root.to_string_lossy()));
        let real_root = self.fs().real_path(&root).unwrap_or_else(|_| root.clone());
        let mut threads = RecentThreads {
            scanner: self.clone(),
            root_identity: String::new(),
            eligible: VecDeque::new(),
            completed: HashMap::new(),
            imported: HashSet::new(),
            bytes: MAX_IMPORT_BYTES,
            transcripts: MAX_IMPORT_TRANSCRIPTS,
            records: MAX_IMPORT_RECORDS,
        };
        if self.excluded(&root) || self.excluded(&real_root) {
            return threads;
        }
        threads.root_identity = self.directory_identity(&root, None);
        let now = self.inner.clock.now().millis();
        let cutoff = now - RECENT_THREAD_WINDOW_MS;
        let mut eligible = Vec::new();
        for candidate in self.candidates().iter() {
            let expanded = self.expand(candidate.cwd.trim());
            if !expanded.is_absolute()
                || self.directory_identity(&resolve(&expanded), None) != threads.root_identity
            {
                continue;
            }
            for (path, mtime_ms) in &candidate.transcripts {
                if (cutoff..=now).contains(mtime_ms) {
                    eligible.push(Eligible {
                        source: candidate.source,
                        instance: candidate.instance.clone(),
                        path: path.clone(),
                        mtime_ms: *mtime_ms,
                    });
                }
            }
        }
        eligible.sort_by(|left, right| {
            right
                .mtime_ms
                .cmp(&left.mtime_ms)
                .then_with(|| left.path.cmp(&right.path))
        });
        threads.eligible = eligible.into();
        for source in completed {
            threads
                .completed
                .entry((source.instance.clone(), source.path.clone()))
                .or_default()
                .push(source.clone());
        }
        threads
    }
}

struct Eligible {
    source: Driver,
    instance: String,
    path: PathBuf,
    mtime_ms: i64,
}

/// Lazily reads one transcript per item, within the per-import byte, file and record budgets.
pub struct RecentThreads {
    scanner: Scanner,
    root_identity: String,
    eligible: VecDeque<Eligible>,
    completed: HashMap<(String, PathBuf), Vec<ImportSource>>,
    imported: HashSet<(String, String)>,
    bytes: u64,
    transcripts: usize,
    records: usize,
}

impl Iterator for RecentThreads {
    type Item = RecentThread;

    fn next(&mut self) -> Option<RecentThread> {
        loop {
            let eligible = self.eligible.pop_front()?;
            let inner = self.scanner.inner.clone();
            let _reading = inner.reading.lock().expect("transcript read lock");
            if let Some(outcome) = self.outcome(eligible) {
                return Some(outcome);
            }
        }
    }
}

impl RecentThreads {
    fn outcome(&mut self, eligible: Eligible) -> Option<RecentThread> {
        let scanner = self.scanner.clone();
        let completed = self
            .completed
            .get(&(eligible.instance.clone(), eligible.path.clone()));
        if completed.is_none() && (self.transcripts == 0 || self.bytes == 0 || self.records == 0) {
            return Some(RecentThread::Skipped);
        }
        let Ok(stat) = scanner.fs().stat(&eligible.path) else {
            return Some(RecentThread::Skipped);
        };
        if stat.kind != EntryKind::File {
            return Some(RecentThread::Skipped);
        }
        let found = identity(&eligible.path, &stat);
        if let Some(done) = completed.and_then(|sources| {
            sources
                .iter()
                .find(|source| source.provider == eligible.source && source.identity() == found)
        }) {
            if !self
                .imported
                .insert((done.instance.clone(), done.session.clone()))
            {
                return None;
            }
            return Some(RecentThread::AlreadyImported {
                source: done.clone(),
            });
        }
        if self.transcripts == 0
            || self.records == 0
            || found.size > MAX_IMPORTED_TRANSCRIPT_BYTES
            || found.size > self.bytes
        {
            return Some(RecentThread::Skipped);
        }
        // The whole file is reserved even if its read or parse fails.
        self.transcripts -= 1;
        self.bytes -= found.size;
        let Some(snapshot) =
            scanner.read_transcript(&eligible.path, &found, self.records, eligible.source)
        else {
            return Some(RecentThread::Skipped);
        };
        self.records = self.records.saturating_sub(snapshot.count);

        // A replaced file can belong to a different project than the cached candidate.
        let Some(cwd) = snapshot.records.iter().find_map(record_cwd) else {
            return Some(RecentThread::Skipped);
        };
        let expanded = scanner.expand(cwd.trim());
        if !expanded.is_absolute()
            || scanner.directory_identity(&resolve(&expanded), None) != self.root_identity
        {
            return Some(RecentThread::Skipped);
        }
        let file_name = eligible
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let meta = TranscriptMeta {
            source: eligible.source,
            instance: eligible.instance.clone(),
            fallback_session: file_name
                .strip_suffix(".jsonl")
                .unwrap_or(&file_name)
                .to_owned(),
            last_active_ms: eligible.mtime_ms,
        };
        let Some(thread) = parse_records(&meta, &snapshot.records) else {
            return Some(RecentThread::Skipped);
        };
        let source = ImportSource {
            provider: thread.source,
            instance: thread.instance.clone(),
            session: thread.session.clone(),
            path: found.path,
            size: found.size,
            mtime_ms: found.mtime_ms,
            device: found.device,
            inode: found.inode,
            birthtime_ms: found.birthtime_ms,
        };
        if !self
            .imported
            .insert((thread.instance.clone(), thread.session.clone()))
        {
            return Some(RecentThread::Duplicate { source });
        }
        Some(RecentThread::Importable { thread, source })
    }
}

#[cfg(test)]
pub(crate) mod tests;
