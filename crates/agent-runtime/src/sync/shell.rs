//! `subscribeShell`: the thread list as a snapshot or a replay of changed rows, then
//! live changes batched (50 ms / 512) and coalesced per thread. Ported from T3
//! `ShellStream.ts` and ws.ts.
use super::history::json_len;
use super::live::{LIVE_STREAM_MAX_BYTES, LiveReceiver, LiveSender, live_channel};
use crate::{CommitListener, CommitNotice, RuntimeError, ShellRow, Store, StoreError};
use agent_domain::{FactBody, ThreadId};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;

pub const SHELL_BATCH_WINDOW: Duration = Duration::from_millis(50);
pub const SHELL_BATCH_MAX: usize = 512;
pub const SHELL_REPLAY_MAX_ROWS: usize = 1_000;
pub const SHELL_REPLAY_MAX_BYTES: u64 = 8 * 1024 * 1024;
const SHELL_HUB_BUFFER: usize = 4_096;

/// A project row; projects are owned outside the conversation runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectShell {
    pub id: String,
    pub payload: serde_json::Value,
}

/// Live projects. Threads of other projects are left out of search.
pub trait ProjectDirectory: Send + Sync {
    fn projects(&self) -> Vec<ProjectShell>;
    fn project(&self, id: &str) -> Option<ProjectShell> {
        self.projects().into_iter().find(|project| project.id == id)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellLocation {
    #[default]
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellSubscribe {
    /// The last global sequence the subscriber has applied; `None` asks for a snapshot.
    pub after_global_seq: Option<u64>,
    pub request_completion_marker: bool,
    pub location: ShellLocation,
    /// Live updates buffered before a slow subscriber is closed.
    pub capacity: usize,
    /// Serialized row bytes buffered before a slow subscriber is closed.
    pub max_bytes: u64,
}
impl Default for ShellSubscribe {
    fn default() -> Self {
        Self {
            after_global_seq: None,
            request_completion_marker: false,
            location: ShellLocation::Active,
            capacity: 1024,
            max_bytes: LIVE_STREAM_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellThread {
    pub thread: ThreadId,
    pub row: ShellRow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShellSnapshot {
    pub snapshot_seq: u64,
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ShellThread>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShellUpdate {
    Snapshot(ShellSnapshot),
    ThreadUpdated {
        sequence: u64,
        thread: ShellThread,
    },
    ThreadRemoved {
        sequence: u64,
        thread: ThreadId,
    },
    ProjectUpdated {
        sequence: u64,
        project: ProjectShell,
    },
    ProjectRemoved {
        sequence: u64,
        project: String,
    },
    /// Every live project; the subscriber drops the ones it holds that are missing.
    /// Opens a resumed stream, since project changes are not replayed.
    Projects {
        sequence: u64,
        projects: Vec<ProjectShell>,
    },
    Synchronized,
}

impl ShellUpdate {
    /// What the update holds against a live budget: its serialized rows.
    pub fn live_bytes(&self) -> u64 {
        match self {
            Self::ThreadUpdated { thread, .. } => json_len(thread),
            Self::ProjectUpdated { project, .. } => json_len(project),
            Self::Projects { projects, .. } => json_len(projects),
            Self::Snapshot(_)
            | Self::ThreadRemoved { .. }
            | Self::ProjectRemoved { .. }
            | Self::Synchronized => 0,
        }
    }
}

/// The stream closes when the subscriber falls behind; resubscribe with `after_global_seq`.
pub struct ShellSubscription {
    pub updates: LiveReceiver<ShellUpdate>,
}

/// One committed change to a list aggregate.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellChange {
    Thread {
        sequence: u64,
        thread: ShellThread,
        /// The change unarchived the thread.
        left_archive: bool,
    },
    Project {
        sequence: u64,
        project: String,
    },
}
impl ShellChange {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Thread { sequence, .. } | Self::Project { sequence, .. } => *sequence,
        }
    }
    fn key(&self) -> (bool, &str) {
        match self {
            Self::Thread { thread, .. } => (false, thread.thread.as_str()),
            Self::Project { project, .. } => (true, project),
        }
    }
}

/// Keeps the newest change per thread or project, in sequence order.
pub fn coalesce_shell_changes(changes: Vec<ShellChange>) -> Vec<ShellChange> {
    let mut latest: HashMap<(bool, String), ShellChange> = HashMap::new();
    for change in changes {
        let (project, id) = change.key();
        latest.insert((project, id.to_owned()), change);
    }
    let mut coalesced: Vec<_> = latest.into_values().collect();
    coalesced.sort_by_key(ShellChange::sequence);
    coalesced
}

/// The list for one location; deleted rows never appear, archived rows only in the archive.
pub fn shell_snapshot(
    location: ShellLocation,
    snapshot_seq: u64,
    projects: Vec<ProjectShell>,
    threads: Vec<ShellThread>,
) -> ShellSnapshot {
    ShellSnapshot {
        snapshot_seq,
        projects,
        threads: threads
            .into_iter()
            .filter(|thread| in_location(&thread.row, location))
            .collect(),
    }
}

fn in_location(row: &ShellRow, location: ShellLocation) -> bool {
    !row.deleted && row.archived == (location == ShellLocation::Archived)
}

/// A thread change for the active list: archived and deleted threads leave it.
pub fn active_shell_update(sequence: u64, thread: ShellThread) -> ShellUpdate {
    if in_location(&thread.row, ShellLocation::Active) {
        ShellUpdate::ThreadUpdated { sequence, thread }
    } else {
        ShellUpdate::ThreadRemoved {
            sequence,
            thread: thread.thread,
        }
    }
}

/// A thread change for the archive, or `None` when archive membership is untouched.
pub fn archived_shell_update(
    sequence: u64,
    thread: ShellThread,
    left_archive: bool,
) -> Option<ShellUpdate> {
    if in_location(&thread.row, ShellLocation::Archived) {
        return Some(ShellUpdate::ThreadUpdated { sequence, thread });
    }
    (left_archive || thread.row.deleted && thread.row.archived).then_some(
        ShellUpdate::ThreadRemoved {
            sequence,
            thread: thread.thread,
        },
    )
}

fn update_for(location: ShellLocation, change: ShellChange) -> Option<ShellUpdate> {
    let ShellChange::Thread {
        sequence,
        thread,
        left_archive,
    } = change
    else {
        return None;
    };
    match location {
        ShellLocation::Active => Some(active_shell_update(sequence, thread)),
        ShellLocation::Archived => archived_shell_update(sequence, thread, left_archive),
    }
}

/// Runs on the store writer after each commit and fans changes out to subscribers.
struct ShellFeed {
    changes: broadcast::Sender<ShellChange>,
    latest: Arc<AtomicU64>,
}
impl CommitListener for ShellFeed {
    fn committed(&self, notice: &CommitNotice) {
        self.latest
            .fetch_max(notice.head.global_seq, Ordering::SeqCst);
        let Some(row) = &notice.shell else {
            return;
        };
        let left_archive = notice.facts.iter().any(|stored| {
            matches!(
                stored.fact.body,
                FactBody::ThreadArchived { archived: false }
            )
        });
        let _ = self.changes.send(ShellChange::Thread {
            sequence: notice.head.global_seq,
            thread: ShellThread {
                thread: notice.thread.clone(),
                row: row.clone(),
            },
            left_archive,
        });
    }
}

/// The global shell publisher. Thread rows arrive from commits in global order;
/// the Host reports project changes with [`ShellHub::project_changed`].
pub struct ShellHub {
    store: Store,
    projects: Arc<dyn ProjectDirectory>,
    changes: broadcast::Sender<ShellChange>,
    latest: Arc<AtomicU64>,
}

enum Initial {
    Snapshot(ShellSnapshot),
    Replay {
        high_water: u64,
        rows: Vec<(u64, ShellThread)>,
    },
}

impl ShellHub {
    pub fn new(store: Store, projects: Arc<dyn ProjectDirectory>) -> Result<Arc<Self>, StoreError> {
        let (changes, _) = broadcast::channel(SHELL_HUB_BUFFER);
        let latest = Arc::new(AtomicU64::new(store.latest_global_seq()?));
        store.add_listener(Arc::new(ShellFeed {
            changes: changes.clone(),
            latest: latest.clone(),
        }));
        Ok(Arc::new(Self {
            store,
            projects,
            changes,
            latest,
        }))
    }

    pub fn project_changed(&self, project: &str) {
        let _ = self.changes.send(ShellChange::Project {
            sequence: self.latest.load(Ordering::SeqCst),
            project: project.to_owned(),
        });
    }

    /// Registers for live changes before reading, so nothing committed in between is lost.
    pub async fn subscribe(
        &self,
        options: ShellSubscribe,
    ) -> Result<ShellSubscription, RuntimeError> {
        let live = self.changes.subscribe();
        let initial = self
            .store
            .blocking(move |store| store.read(|c| initial(c, options.after_global_seq)))
            .await?;
        let (start, mut first) = match initial {
            Initial::Snapshot(snapshot) => {
                let snapshot = shell_snapshot(
                    options.location,
                    snapshot.snapshot_seq,
                    self.projects.projects(),
                    snapshot.threads,
                );
                (snapshot.snapshot_seq, vec![ShellUpdate::Snapshot(snapshot)])
            }
            Initial::Replay { high_water, rows } => (
                high_water,
                std::iter::once(ShellUpdate::Projects {
                    sequence: high_water,
                    projects: self.projects.projects(),
                })
                .chain(rows.into_iter().filter_map(|(sequence, thread)| {
                    // Replayed rows lack the facts that left the archive.
                    let left_archive = !thread.row.archived;
                    update_for(
                        options.location,
                        ShellChange::Thread {
                            sequence,
                            thread,
                            left_archive,
                        },
                    )
                }))
                .collect(),
            ),
        };
        if options.request_completion_marker {
            first.push(ShellUpdate::Synchronized);
        }
        let (sender, updates) =
            live_channel(options.capacity.max(4) + first.len(), options.max_bytes);
        tokio::spawn(forward(
            first,
            start,
            options.location,
            live,
            self.projects.clone(),
            sender,
        ));
        Ok(ShellSubscription { updates })
    }
}

async fn forward(
    first: Vec<ShellUpdate>,
    start: u64,
    location: ShellLocation,
    mut live: broadcast::Receiver<ShellChange>,
    projects: Arc<dyn ProjectDirectory>,
    sender: LiveSender<ShellUpdate>,
) {
    let offer = |update: ShellUpdate| {
        let bytes = update.live_bytes();
        sender.offer(update, bytes)
    };
    if !first.into_iter().all(offer) {
        return;
    }
    loop {
        let change = tokio::select! {
            () = sender.closed() => return,
            change = live.recv() => match change {
                Ok(change) => change,
                Err(_) => return,
            },
        };
        let mut batch = vec![change];
        let deadline = tokio::time::Instant::now() + SHELL_BATCH_WINDOW;
        while batch.len() < SHELL_BATCH_MAX {
            match tokio::time::timeout_at(deadline, live.recv()).await {
                Ok(Ok(change)) => batch.push(change),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => return,
                Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
            }
        }
        for change in coalesce_shell_changes(batch) {
            let update = match change {
                ShellChange::Thread { sequence, .. } if sequence <= start => continue,
                ShellChange::Project { sequence, project } => {
                    Some(match projects.project(&project) {
                        Some(project) => ShellUpdate::ProjectUpdated { sequence, project },
                        None => ShellUpdate::ProjectRemoved { sequence, project },
                    })
                }
                thread => update_for(location, thread),
            };
            if let Some(update) = update
                && !offer(update)
            {
                return;
            }
        }
    }
}

fn high_water(c: &Connection) -> Result<u64, StoreError> {
    Ok(c.query_row(
        "SELECT COALESCE(MAX(global_seq), 0) FROM facts",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64)
}

type RawShell = (String, i64, String, bool, bool, bool, String);

fn shell_thread(raw: RawShell) -> Result<(u64, ShellThread), StoreError> {
    let (thread, sequence, project, archived, deleted, needs_recovery, payload) = raw;
    Ok((
        sequence as u64,
        ShellThread {
            thread: ThreadId::new(thread)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            row: ShellRow {
                project,
                archived,
                deleted,
                needs_recovery,
                payload: serde_json::from_str(&payload)?,
            },
        },
    ))
}

const SHELL_COLUMNS: &str =
    "thread_id, global_seq, project, archived, deleted, needs_recovery, payload";

fn raw_shell(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawShell> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

/// One read transaction: the changed rows after `after` when the replay is small
/// enough, otherwise every live row.
fn initial(c: &Connection, after: Option<u64>) -> Result<Initial, StoreError> {
    let high = high_water(c)?;
    if let Some(after) = after.filter(|after| *after <= high) {
        let mut statement = c.prepare_cached(&format!(
            "SELECT {SHELL_COLUMNS}, octet_length(payload) FROM thread_shells
             WHERE global_seq > ?1 ORDER BY global_seq LIMIT ?2"
        ))?;
        let raw = statement
            .query_map(
                params![after as i64, SHELL_REPLAY_MAX_ROWS as i64 + 1],
                |row| Ok((raw_shell(row)?, row.get::<_, i64>(7)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let bytes: u64 = raw.iter().map(|(_, length)| *length as u64).sum();
        if raw.len() <= SHELL_REPLAY_MAX_ROWS && bytes <= SHELL_REPLAY_MAX_BYTES {
            return Ok(Initial::Replay {
                high_water: high,
                rows: raw
                    .into_iter()
                    .map(|(raw, _)| shell_thread(raw))
                    .collect::<Result<_, _>>()?,
            });
        }
    }
    let mut statement = c.prepare_cached(&format!(
        "SELECT {SHELL_COLUMNS} FROM thread_shells WHERE deleted = 0 ORDER BY global_seq"
    ))?;
    let threads = statement
        .query_map([], raw_shell)?
        .map(|raw| Ok(shell_thread(raw?)?.1))
        .collect::<Result<_, StoreError>>()?;
    Ok(Initial::Snapshot(ShellSnapshot {
        snapshot_seq: high,
        projects: vec![],
        threads,
    }))
}

#[cfg(test)]
mod tests;
