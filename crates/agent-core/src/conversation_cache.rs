//! Device copies of the conversation list and opened conversations, one
//! directory per Host, shown before the Host confirms them.
//!
//! A copy contains the newest T3 snapshot and its resumable event sequence.
//! Writes follow the
//! first change after half a second, then at most every ten seconds, keeping
//! only the latest state; an app about to stop flushes what is pending. Copies have no count or size limit. A damaged or
//! older copy is removed when read; removing a Host removes its directory.
//! Copies are best effort: failed reads and writes leave the Host as the source.
use crate::{
    models::{ListQuery, SessionStatus},
    session::SessionRef,
    state::Snapshot,
};
use agent_protocol::orchestration::shell::{ShellSnapshot, validate_snapshot};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

/// Copies restored at startup, most recently saved first.
const RESTORED: usize = 20;
/// Copies in another format are discarded instead of migrated.
const FORMAT: u32 = 3;
const LIST_FORMAT: u32 = 5;

#[derive(Serialize, Deserialize)]
struct Copy {
    format: u32,
    storage_scope: String,
    session: SessionRef,
    snapshot: agent_protocol::orchestration::projection::ThreadSnapshot,
}

/// Like T3 Code's separate shell/thread caches (cefb544a), the list has no
/// conversation bodies. Search results must never replace the unfiltered list.
#[derive(Clone, Serialize, Deserialize)]
struct ListCopy {
    format: u32,
    storage_scope: String,
    snapshot: ShellSnapshot,
    expanded_projects: BTreeSet<String>,
}

impl ListCopy {
    fn unchanged(&self, previous: &Self) -> bool {
        self.storage_scope == previous.storage_scope
            && self.expanded_projects == previous.expanded_projects
            && self.snapshot == previous.snapshot
    }
    fn write(self, path: &Path) -> std::io::Result<()> {
        write_value(path, self)
    }
}

fn read_list(directory: &Path, storage_scope: &str) -> Option<ListCopy> {
    let path = directory.join("list.cache");
    let copy = std::fs::read(&path)
        .ok()
        .and_then(|bytes| agent_protocol::protocol::decode::<ListCopy>(&bytes).ok())
        .filter(|copy| copy.format == LIST_FORMAT && validate_snapshot(&copy.snapshot).is_ok());
    if copy.is_none() {
        let _ = std::fs::remove_file(path);
    }
    copy.filter(|copy| copy.storage_scope == storage_scope)
}

fn file(directory: &Path, id: &SessionRef) -> PathBuf {
    let name: String = id.id.bytes().map(|byte| format!("{byte:02x}")).collect();
    directory.join(format!(
        "{}-{name}.bin",
        id.provider
            .map_or("bex", agent_protocol::session::ProviderKind::key)
    ))
}

fn copy(
    storage_scope: &str,
    id: &SessionRef,
    projection: &agent_protocol::orchestration::projection::ThreadSnapshot,
) -> Copy {
    Copy {
        format: FORMAT,
        storage_scope: storage_scope.into(),
        session: id.clone(),
        snapshot: projection.clone(),
    }
}

fn read(
    path: &Path,
    storage_scope: &str,
) -> Option<(
    SessionRef,
    agent_protocol::orchestration::projection::ThreadSnapshot,
)> {
    let copy = std::fs::read(path)
        .ok()
        .and_then(|bytes| agent_protocol::protocol::decode::<Copy>(&bytes).ok());
    if copy
        .as_ref()
        .is_some_and(|copy| copy.format == FORMAT && copy.storage_scope != storage_scope)
    {
        return None;
    }
    let loaded = copy
        .filter(|copy| {
            copy.format == FORMAT
                && copy.session.application_id() == copy.snapshot.projection.thread.id
        })
        .map(|copy| (copy.session, copy.snapshot));
    if loaded.is_none() {
        let _ = std::fs::remove_file(path);
    }
    loaded
}

fn write(
    directory: &Path,
    storage_scope: &str,
    id: &SessionRef,
    projection: &agent_protocol::orchestration::projection::ThreadSnapshot,
) -> std::io::Result<()> {
    write_value(&file(directory, id), copy(storage_scope, id, projection))
}

fn write_value(path: &Path, value: impl Serialize) -> std::io::Result<()> {
    use std::io::Write as _;
    let bytes = agent_protocol::protocol::encode(value)?;
    let parent = path.parent().expect("cache files have a directory");
    std::fs::create_dir_all(parent)?;
    // Independent windows/processes must never share a partially written file.
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(&bytes)?;
    pending.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Add the most recently saved copies that `snapshot` does not hold yet.
pub fn restore(directory: &Path, snapshot: &mut Snapshot) {
    if snapshot.lists.shell.is_none()
        && let Some(list) = read_list(directory, &snapshot.connection.storage_scope)
    {
        snapshot.lists.expanded_projects = Arc::new(
            list.expanded_projects
                .into_iter()
                .map(|id| (id, ListQuery::default().limit))
                .collect(),
        );
        snapshot.lists.list_query = Arc::new(ListQuery::default());
        snapshot.lists.shell = Some(Arc::new(list.snapshot));
        crate::state::list_sync::project(snapshot, false);
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "bin")
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.0));
    for (_, path) in files.into_iter().take(RESTORED) {
        if let Some((id, replica)) = read(&path, &snapshot.connection.storage_scope) {
            if snapshot.sessions.conversations.contains_key(&id)
                || snapshot.sessions.projections.contains_key(&id)
            {
                continue;
            }
            let provider = serde_json::from_value(serde_json::Value::String(
                replica.projection.thread.provider_instance_id.clone(),
            ));
            let metadata = snapshot.sessions.metadata.get(&id);
            let Ok(provider) = provider else {
                continue;
            };
            let Ok(projected) = crate::orchestration::project(
                &replica,
                &id,
                metadata.map_or(provider, |metadata| metadata.provider),
                metadata
                    .map(|metadata| metadata.capabilities)
                    .unwrap_or_default(),
                crate::state::conversations::project_root(
                    metadata,
                    snapshot
                        .lists
                        .threads
                        .as_ref()
                        .map_or(&[], |list| list.projects.as_slice()),
                    replica.projection.thread.project_id.as_deref(),
                ),
                &BTreeMap::new(),
            ) else {
                continue;
            };
            let mut thread = projected.thread;
            thread.status = SessionStatus::Unknown;
            thread.requests.clear();
            thread.submissions.clear();
            thread.preparations.clear();
            thread.background.clear();
            thread.queue = Default::default();
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .insert(id.clone(), Arc::new(thread));
            Arc::make_mut(&mut snapshot.sessions.projections).insert(id, Arc::new(replica));
        }
    }
}

/// Remove a Host's copies, such as when it is paired again or removed.
pub fn clear(directory: &Path) {
    let _ = std::fs::remove_dir_all(directory);
}

/// What the copies on disk hold, to write only what changed.
#[derive(Default)]
struct Saved {
    conversations:
        BTreeMap<SessionRef, Arc<agent_protocol::orchestration::projection::ThreadSnapshot>>,
    list: Option<ListCopy>,
}

impl Saved {
    /// Writes the list and cached conversations that differ from the saved
    /// copies, and says whether it wrote anything.
    async fn write_changes(&mut self, directory: &Path, snapshot: &Snapshot) -> bool {
        let mut writing = false;
        if snapshot.connection.connected
            && snapshot.lists.subscription.is_some()
            && let Some(shell) = &snapshot.lists.shell
        {
            let list = ListCopy {
                format: LIST_FORMAT,
                storage_scope: snapshot.connection.storage_scope.clone(),
                snapshot: shell.as_ref().clone(),
                expanded_projects: snapshot.lists.expanded_projects.keys().cloned().collect(),
            };
            if self
                .list
                .as_ref()
                .is_none_or(|previous| !list.unchanged(previous))
            {
                writing = true;
                let path = directory.join("list.cache");
                let pending = list.clone();
                if tokio::task::spawn_blocking(move || pending.write(&path))
                    .await
                    .is_ok_and(|result| result.is_ok())
                {
                    self.list = Some(list);
                }
            }
        }
        let changed: Vec<_> = snapshot
            .sessions
            .projections
            .iter()
            .filter(|(id, thread)| {
                self.conversations
                    .get(*id)
                    .is_none_or(|previous| !Arc::ptr_eq(previous, thread))
                    && !snapshot.sessions.expanded_history.contains(*id)
                    && !thread.projection.runs.iter().any(|run| {
                        matches!(
                            run.status,
                            agent_protocol::orchestration::RunStatus::Preparing
                                | agent_protocol::orchestration::RunStatus::Starting
                                | agent_protocol::orchestration::RunStatus::Running
                                | agent_protocol::orchestration::RunStatus::Waiting
                        )
                    })
            })
            .map(|(id, thread)| (id.clone(), thread.clone()))
            .collect();
        if !changed.is_empty() {
            writing = true;
            let target = directory.to_owned();
            let pending = changed.clone();
            let scope = snapshot.connection.storage_scope.clone();
            let written = tokio::task::spawn_blocking(move || {
                pending
                    .iter()
                    .map(|(id, replica)| write(&target, &scope, id, replica).is_ok())
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            for ((id, thread), written) in changed.into_iter().zip(written) {
                if written {
                    self.conversations.insert(id, thread);
                }
            }
        }
        writing
    }
}

/// A request to write the pending copies now; answered once they are written.
pub type Flush = tokio::sync::oneshot::Sender<()>;

/// Save changed lists and cached conversations until the Store stops
/// publishing. A change is written half a second after it,
/// and a write is followed by ten quiet seconds; a flush writes whatever is
/// pending at once, as the app leaves or quits within them.
pub async fn run(
    directory: PathBuf,
    mut updates: watch::Receiver<Arc<Snapshot>>,
    mut flushes: tokio::sync::mpsc::UnboundedReceiver<Flush>,
) {
    use tokio::time::Instant;
    let mut saved = Saved::default();
    let mut quiet_until = Instant::now();
    // The state published before this task started is written as a change.
    let mut due = Some(Instant::now() + Duration::from_millis(500));
    let mut flushable = true;
    loop {
        let flush = tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() {
                    return;
                }
                due.get_or_insert((Instant::now() + Duration::from_millis(500)).max(quiet_until));
                continue;
            }
            flush = flushes.recv(), if flushable => match flush {
                Some(flush) => Some(flush),
                None => {
                    flushable = false;
                    continue;
                }
            },
            () = tokio::time::sleep_until(due.unwrap_or(quiet_until)), if due.is_some() => None,
        };
        let snapshot = updates.borrow_and_update().clone();
        if saved.write_changes(&directory, &snapshot).await {
            quiet_until = Instant::now() + Duration::from_secs(10);
        }
        due = None;
        if let Some(flush) = flush {
            let _ = flush.send(());
        }
    }
}

#[cfg(test)]
mod tests;
