//! The shell and settled threads on disk with their cursors, so a warm start
//! shows them at once and resumes with `after_sequence`.
use super::thread::{CachedThread, ThreadStatus, ThreadSync};
use agent_domain::{RunStatus, STATE_FORMAT, ThreadId};
use agent_protocol::conversation::ShellSnapshot;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const FIRST_WRITE_DELAY_MS: u64 = 500;
pub const WRITE_COOLDOWN_MS: u64 = 10_000;

/// Writes only the latest pending value: the first one after 500 ms, later
/// ones at least 10 s after the previous write finished.
#[derive(Debug, Clone)]
pub struct CacheWriter<T> {
    pending: Option<T>,
    due_at: Option<u64>,
    cool_until: Option<u64>,
    started: bool,
    writing: bool,
}
impl<T> Default for CacheWriter<T> {
    fn default() -> Self {
        Self {
            pending: None,
            due_at: None,
            cool_until: None,
            started: false,
            writing: false,
        }
    }
}
impl<T> CacheWriter<T> {
    pub fn offer(&mut self, value: T, now: u64) {
        self.pending = Some(value);
        if self.writing || self.due_at.is_some() {
            return;
        }
        self.due_at = Some(if self.started {
            self.cool_until.map_or(now, |until| until.max(now))
        } else {
            self.started = true;
            now + FIRST_WRITE_DELAY_MS
        });
    }
    pub fn next_due(&self) -> Option<u64> {
        self.due_at.filter(|_| self.pending.is_some())
    }
    /// The value to write now; call `written` once it is stored.
    pub fn due(&mut self, now: u64) -> Option<T> {
        if self.due_at.is_none_or(|due| due > now) {
            return None;
        }
        self.due_at = None;
        let value = self.pending.take()?;
        self.writing = true;
        Some(value)
    }
    pub fn written(&mut self, now: u64) {
        self.writing = false;
        let until = now + WRITE_COOLDOWN_MS;
        self.cool_until = Some(until);
        if self.pending.is_some() {
            self.due_at = Some(until);
        }
    }
    /// Teardown and disconnect write the latest value without waiting.
    pub fn flush(&mut self) -> Option<T> {
        self.due_at = None;
        self.pending.take()
    }
    pub fn discard(&mut self) {
        self.pending = None;
        self.due_at = None;
    }
}

/// Settled threads only: an active run changes many times a second, and an
/// expanded timeline grows without bound.
pub fn should_persist_thread(sync: &ThreadSync) -> bool {
    sync.status != ThreadStatus::Deleted
        && !sync.history.expanded
        && sync.state.as_ref().is_some_and(|state| {
            !state.runs.iter().any(|run| {
                matches!(
                    run.status,
                    RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
                )
            })
        })
}

pub fn cached_thread(sync: &ThreadSync) -> Option<CachedThread> {
    Some(CachedThread {
        snapshot_sequence: sync.cursor,
        state: sync.state.clone()?,
        history_cursor: sync.history.cursor.clone(),
        has_more_history: sync.history.has_more,
        latest_local_ordinal: sync.history.latest_local_ordinal,
    })
}

fn same_snapshot(left: &CachedThread, right: &CachedThread) -> bool {
    left.snapshot_sequence == right.snapshot_sequence
        && Arc::ptr_eq(&left.state, &right.state)
        && left.history_cursor == right.history_cursor
        && left.has_more_history == right.has_more_history
        && (!(left.has_more_history || left.history_cursor.is_some())
            || left.latest_local_ordinal == right.latest_local_ordinal)
}

/// Write bookkeeping for one thread.
#[derive(Debug, Clone, Default)]
pub struct ThreadCacheEntry {
    writer: CacheWriter<CachedThread>,
    persisted: Option<CachedThread>,
}
impl ThreadCacheEntry {
    pub fn loaded(cached: &CachedThread) -> Self {
        Self {
            persisted: Some(cached.clone()),
            ..Self::default()
        }
    }
    fn unsaved(&self, sync: &ThreadSync) -> Option<CachedThread> {
        if !should_persist_thread(sync) {
            return None;
        }
        let cached = cached_thread(sync)?;
        (!self
            .persisted
            .as_ref()
            .is_some_and(|persisted| same_snapshot(persisted, &cached)))
        .then_some(cached)
    }
    pub fn changed(&mut self, sync: &ThreadSync, now: u64) {
        if let Some(cached) = self.unsaved(sync) {
            self.writer.offer(cached, now);
        }
    }
    pub fn next_due(&self) -> Option<u64> {
        self.writer.next_due()
    }
    pub fn due(&mut self, now: u64) -> Option<CachedThread> {
        self.writer.due(now)
    }
    pub fn written(&mut self, cached: CachedThread, stored: bool, now: u64) {
        self.writer.written(now);
        if stored {
            self.persisted = Some(cached);
        }
    }
    /// What the thread's teardown stores: the latest settled state not yet saved.
    pub fn teardown(&mut self, sync: &ThreadSync) -> Option<CachedThread> {
        self.writer.discard();
        self.unsaved(sync)
    }
    pub fn deleted(&mut self) {
        self.writer.discard();
        self.persisted = None;
    }
}

/// Shell write bookkeeping; `revision` identifies each applied shell value.
#[derive(Debug, Clone, Default)]
pub struct ShellCacheEntry {
    writer: CacheWriter<(u64, ShellSnapshot)>,
    persisted: Option<u64>,
    latest: Option<(u64, ShellSnapshot)>,
}
impl ShellCacheEntry {
    pub fn changed(&mut self, revision: u64, snapshot: &ShellSnapshot, now: u64) {
        self.latest = Some((revision, snapshot.clone()));
        self.writer.offer((revision, snapshot.clone()), now);
    }
    pub fn next_due(&self) -> Option<u64> {
        self.writer.next_due()
    }
    pub fn due(&mut self, now: u64) -> Option<(u64, ShellSnapshot)> {
        self.writer
            .due(now)
            .filter(|(revision, _)| self.persisted != Some(*revision))
    }
    pub fn written(&mut self, revision: u64, stored: bool, now: u64) {
        self.writer.written(now);
        if stored {
            self.persisted = Some(revision);
        }
    }
    /// Disconnect and teardown store the latest live value if it is not saved.
    pub fn flush(&mut self) -> Option<(u64, ShellSnapshot)> {
        self.writer.flush();
        self.latest
            .clone()
            .filter(|(revision, _)| self.persisted != Some(*revision))
    }
}

#[derive(Serialize, Deserialize)]
struct Stored<T> {
    format: u32,
    value: T,
}

/// One Host's cache directory.
#[derive(Debug, Clone)]
pub struct DiskCache {
    root: PathBuf,
}
impl DiskCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn shell_path(&self) -> PathBuf {
        self.root.join("shell.bin")
    }
    fn thread_path(&self, thread: &ThreadId) -> PathBuf {
        let name: String = thread
            .as_str()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.root.join("threads").join(format!("{name}.bin"))
    }
    fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
        let bytes = std::fs::read(path).ok()?;
        let stored: Stored<T> = agent_protocol::protocol::decode(&bytes).ok()?;
        (stored.format == STATE_FORMAT).then_some(stored.value)
    }
    fn write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = agent_protocol::protocol::encode(Stored {
            format: STATE_FORMAT,
            value,
        })?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(temporary, path)
    }
    pub fn load_shell(&self) -> Option<ShellSnapshot> {
        Self::read(&self.shell_path())
    }
    pub fn save_shell(&self, shell: &ShellSnapshot) -> io::Result<()> {
        Self::write(&self.shell_path(), shell)
    }
    pub fn load_thread(&self, thread: &ThreadId) -> Option<CachedThread> {
        Self::read(&self.thread_path(thread))
    }
    pub fn save_thread(&self, thread: &ThreadId, cached: &CachedThread) -> io::Result<()> {
        Self::write(&self.thread_path(thread), cached)
    }
    pub fn remove_thread(&self, thread: &ThreadId) -> io::Result<()> {
        match std::fs::remove_file(self.thread_path(thread)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::*;

    #[test]
    fn coalesces_streaming_writes_and_flushes_the_latest_on_teardown() {
        let mut writer = CacheWriter::default();
        let mut saved = vec![];
        let mut now = 0;
        let step = |writer: &mut CacheWriter<u64>, now: u64, saved: &mut Vec<u64>| {
            if let Some(value) = writer.due(now) {
                saved.push(value);
                writer.written(now);
            }
        };
        writer.offer(1, now);
        now += 500;
        step(&mut writer, now, &mut saved);
        assert_eq!(saved, [1]);
        for sequence in 2..=10 {
            writer.offer(sequence, now);
            step(&mut writer, now, &mut saved);
            now += 1_000;
        }
        step(&mut writer, now, &mut saved);
        assert_eq!(saved, [1]);
        now += 1_000;
        step(&mut writer, now, &mut saved);
        assert_eq!(saved, [1, 10]);
        writer.offer(11, now);
        saved.extend(writer.flush());
        assert_eq!(saved, [1, 10, 11]);
    }

    #[test]
    fn flushes_newer_data_after_an_older_write_completes() {
        let mut writer = CacheWriter::default();
        writer.offer(8, 0);
        let first = writer.due(500).unwrap();
        writer.offer(9, 600);
        assert_eq!(writer.due(700), None);
        writer.written(800);
        assert_eq!(writer.due(800 + WRITE_COOLDOWN_MS), Some(9));
        assert_eq!(first, 8);
    }

    #[test]
    fn persists_a_settled_thread_once_and_skips_an_unchanged_warm_return() {
        let mut sync = ThreadSync::default();
        sync.subscribe(&thread_id());
        sync.apply(vec![snapshot(thread_state("Cached thread"), 7, None)]);
        let mut entry = ThreadCacheEntry::default();
        entry.changed(&sync, 0);
        let cached = entry.due(500).unwrap();
        entry.written(cached.clone(), true, 500);
        // A warm return from that disk entry has nothing to write.
        let mut warm = ThreadCacheEntry::loaded(&cached);
        let reopened = ThreadSync::from_cache(cached);
        warm.changed(&reopened, 0);
        assert_eq!(warm.next_due(), None);
        assert_eq!(warm.teardown(&reopened), None);
    }

    #[test]
    fn persists_a_complete_bounded_window_with_its_meta() {
        let mut sync = ThreadSync::default();
        sync.subscribe(&thread_id());
        sync.apply(vec![snapshot(
            thread_state("Bounded"),
            7,
            Some(window(None, false, Some(12))),
        )]);
        let cached = cached_thread(&sync).unwrap();
        assert_eq!(
            (
                cached.snapshot_sequence,
                cached.history_cursor,
                cached.has_more_history,
                cached.latest_local_ordinal
            ),
            (7, None, false, Some(12))
        );
    }

    #[test]
    fn retries_a_failed_write_at_teardown() {
        let mut sync = ThreadSync::default();
        sync.subscribe(&thread_id());
        sync.apply(vec![snapshot(thread_state("Thread"), 7, None)]);
        let mut entry = ThreadCacheEntry::default();
        entry.changed(&sync, 0);
        let cached = entry.due(500).unwrap();
        entry.written(cached, false, 500);
        assert!(entry.teardown(&sync).is_some());
    }

    #[test]
    fn does_not_persist_active_expanded_or_deleted_threads() {
        let mut sync = ThreadSync::default();
        sync.subscribe(&thread_id());
        let mut state = thread_state("Thread");
        state
            .runs
            .push(run("run", 1, agent_domain::RunStatus::Running));
        sync.apply(vec![snapshot(state, 3, None)]);
        assert!(!should_persist_thread(&sync));
        let mut settled = ThreadSync::default();
        settled.subscribe(&thread_id());
        settled.apply(vec![snapshot(thread_state("Thread"), 3, None)]);
        assert!(should_persist_thread(&settled));
        settled.history.expanded = true;
        assert!(!should_persist_thread(&settled));
        settled.history.expanded = false;
        let mut entry = ThreadCacheEntry::default();
        entry.changed(&settled, 0);
        settled.set_deleted();
        entry.deleted();
        assert_eq!(entry.due(10_000), None);
        assert_eq!(entry.teardown(&settled), None);
    }

    #[test]
    fn throttles_shell_writes_and_flushes_the_latest_snapshot_on_disconnect() {
        let shell = |sequence| ShellSnapshot {
            snapshot_sequence: sequence,
            projects: vec![],
            threads: vec![],
        };
        let mut entry = ShellCacheEntry::default();
        let mut saved = vec![];
        let mut now = 0;
        entry.changed(1, &shell(1), now);
        now += 500;
        let (revision, value) = entry.due(now).unwrap();
        saved.push(value.snapshot_sequence);
        entry.written(revision, true, now);
        for sequence in 2..=10 {
            entry.changed(sequence, &shell(sequence), now);
            assert_eq!(entry.due(now), None);
            now += 1_000;
        }
        assert_eq!(saved, [1]);
        now += 1_000;
        let (revision, value) = entry.due(now).unwrap();
        saved.push(value.snapshot_sequence);
        entry.written(revision, true, now);
        assert_eq!(saved, [1, 10]);
        entry.changed(11, &shell(11), now);
        let (revision, value) = entry.flush().unwrap();
        saved.push(value.snapshot_sequence);
        entry.written(revision, true, now);
        assert_eq!(saved, [1, 10, 11]);
        assert_eq!(entry.flush(), None);
    }

    #[test]
    fn rewrites_a_same_sequence_shell_with_different_content() {
        let mut entry = ShellCacheEntry::default();
        let shell = ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![],
            threads: vec![],
        };
        entry.changed(1, &shell, 0);
        let (revision, _) = entry.flush().unwrap();
        entry.written(revision, true, 0);
        let mut enriched = shell.clone();
        enriched.projects.push(Default::default());
        entry.changed(2, &enriched, 1);
        assert_eq!(entry.flush().unwrap().1, enriched);
    }

    #[test]
    fn disk_entries_round_trip_and_removal_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(directory.path());
        assert_eq!(cache.load_shell(), None);
        let shell = ShellSnapshot {
            snapshot_sequence: 4,
            projects: vec![],
            threads: vec![agent_domain::shell(&thread_state("Thread")).unwrap()],
        };
        cache.save_shell(&shell).unwrap();
        assert_eq!(cache.load_shell(), Some(shell));
        let cached = CachedThread {
            snapshot_sequence: 9,
            state: Arc::new(thread_state("Thread")),
            history_cursor: Some("cursor".into()),
            has_more_history: true,
            latest_local_ordinal: Some(3),
        };
        cache.save_thread(&thread_id(), &cached).unwrap();
        assert_eq!(cache.load_thread(&thread_id()), Some(cached));
        cache.remove_thread(&thread_id()).unwrap();
        cache.remove_thread(&thread_id()).unwrap();
        assert_eq!(cache.load_thread(&thread_id()), None);
        std::fs::write(directory.path().join("shell.bin"), b"damaged").unwrap();
        assert_eq!(cache.load_shell(), None);
    }
}
