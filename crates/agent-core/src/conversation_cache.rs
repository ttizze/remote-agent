//! Device copies of the conversation list and opened conversations, one
//! directory per Host, shown before the Host confirms them.
//!
//! A copy contains the newest window of rows and its resumable cursor.
//! Writes follow the
//! first change after half a second, then at most every ten seconds, keeping
//! only the latest state; an app about to stop flushes what is pending. Copies have no count or size limit. A damaged or
//! older copy is removed when read; removing a Host removes its directory.
//! Copies are best effort: failed reads and writes leave the Host as the source.
use crate::{
    models::{ListQuery, SessionStatus},
    session::SessionRef,
    state::{Snapshot, operations::HISTORY_WINDOW_TURNS},
};
use agent_protocol::{
    conversation::records,
    sync::{Message, Position, Scope},
};
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
const FORMAT: u32 = 2;
const LIST_FORMAT: u32 = 4;

#[derive(Serialize, Deserialize)]
struct Copy {
    format: u32,
    storage_scope: String,
    session: SessionRef,
    snapshot: records::Message,
}

/// Like T3 Code's separate shell/thread caches (cefb544a), the list has no
/// conversation bodies. Search results must never replace the unfiltered list.
#[derive(Clone, Serialize, Deserialize)]
struct ListCopy {
    format: u32,
    storage_scope: String,
    snapshot: records::Message,
    expanded_projects: BTreeSet<String>,
}

impl ListCopy {
    fn unchanged(&self, previous: &Self) -> bool {
        fn cursor(message: &records::Message) -> Option<agent_protocol::sync::Cursor> {
            match message {
                Message::Snapshot { cursor, .. } => Some(*cursor),
                _ => None,
            }
        }
        self.storage_scope == previous.storage_scope
            && self.expanded_projects == previous.expanded_projects
            && cursor(&self.snapshot) == cursor(&previous.snapshot)
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
        .filter(|copy| {
            copy.format == LIST_FORMAT && records::validate_list(&copy.snapshot).is_ok()
        });
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

fn copy(storage_scope: &str, id: &SessionRef, replica: &records::Replica) -> Option<Copy> {
    let Message::Snapshot {
        cursor,
        window,
        mut rows,
    } = replica.snapshot()?
    else {
        return None;
    };
    // Tool bodies and media are loaded on demand; offline copies retain their
    // row identity/version and metadata without retaining their large bodies.
    for row in &mut rows {
        let record = match &row.content {
            agent_protocol::sync::Content::Full(value)
            | agent_protocol::sync::Content::Summary(value) => value,
        };
        if let records::Record::Item(item) = record.as_ref()
            && !matches!(
                item.content.body(),
                crate::models::ItemBody::UserMessage { .. }
                    | crate::models::ItemBody::AssistantText { .. }
            )
        {
            records::defer(row);
        }
    }
    Some(Copy {
        format: FORMAT,
        storage_scope: storage_scope.into(),
        session: id.clone(),
        snapshot: Message::Snapshot {
            cursor,
            window,
            rows,
        },
    })
}

fn read(path: &Path, storage_scope: &str) -> Option<(SessionRef, records::Replica)> {
    let copy = std::fs::read(path)
        .ok()
        .and_then(|bytes| agent_protocol::protocol::decode::<Copy>(&bytes).ok());
    if copy
        .as_ref()
        .is_some_and(|copy| copy.format == FORMAT && copy.storage_scope != storage_scope)
    {
        return None;
    }
    let copy = copy.filter(|copy| copy.format == FORMAT);
    let loaded = copy.and_then(|copy| {
        let replica = records::Replica::new(Scope::Conversation(copy.session.clone()), 0)
            .apply(0, copy.snapshot)
            .ok()?;
        records::project(&replica)?;
        Some((copy.session, replica))
    });
    if loaded.is_none() {
        let _ = std::fs::remove_file(path);
    }
    loaded
}

fn write(
    directory: &Path,
    storage_scope: &str,
    id: &SessionRef,
    replica: &records::Replica,
) -> std::io::Result<()> {
    let Some(copy) = copy(storage_scope, id, replica) else {
        return Ok(());
    };
    write_value(&file(directory, id), copy)
}

fn write_value(path: &Path, value: impl Serialize) -> std::io::Result<()> {
    let bytes = agent_protocol::protocol::encode(value)?;
    std::fs::create_dir_all(path.parent().expect("cache files have a directory"))?;
    let pending = path.with_extension("pending");
    std::fs::write(&pending, bytes)?;
    std::fs::rename(pending, path)
}

/// Add the most recently saved copies that `snapshot` does not hold yet.
pub fn restore(directory: &Path, snapshot: &mut Snapshot) {
    if snapshot.lists.replica.is_none()
        && let Some(list) = read_list(directory, &snapshot.connection.storage_scope)
        && let Ok(replica) = records::Replica::new(Scope::List, 0).apply(0, list.snapshot)
    {
        snapshot.lists.expanded_projects = Arc::new(
            list.expanded_projects
                .into_iter()
                .map(|id| (id, ListQuery::default().limit))
                .collect(),
        );
        snapshot.lists.list_query = Arc::new(ListQuery::default());
        snapshot.lists.replica = Some(Arc::new(replica));
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
                || snapshot.sessions.replicas.contains_key(&id)
            {
                continue;
            }
            let mut thread = records::project(&replica)
                .expect("validated cache projection")
                .thread;
            thread.status = SessionStatus::Unknown;
            thread.requests.clear();
            thread.submissions.clear();
            thread.preparations.clear();
            thread.background.clear();
            thread.queue = Default::default();
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .insert(id.clone(), Arc::new(thread));
            Arc::make_mut(&mut snapshot.sessions.replicas).insert(id, Arc::new(replica));
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
    conversations: BTreeMap<SessionRef, Arc<records::Replica>>,
    list: Option<ListCopy>,
}

impl Saved {
    /// Writes the list and cached conversations that differ from the saved
    /// copies, and says whether it wrote anything.
    async fn write_changes(&mut self, directory: &Path, snapshot: &Snapshot) -> bool {
        let mut writing = false;
        if snapshot.connection.connected
            && snapshot.lists.subscription.is_some()
            && let Some(replica) = &snapshot.lists.replica
            && let Some(message) = replica.snapshot()
        {
            let list = ListCopy {
                format: LIST_FORMAT,
                storage_scope: snapshot.connection.storage_scope.clone(),
                snapshot: message,
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
            .replicas
            .iter()
            .filter(|(id, thread)| {
                self.conversations
                    .get(*id)
                    .is_none_or(|previous| !Arc::ptr_eq(previous, thread))
                    && thread.cursor().is_some()
                    && !thread.expanded()
                    && thread.rows().values().filter(|row| matches!(row.position, Position::Turn { .. })).count() <= HISTORY_WINDOW_TURNS as usize
                    // As T3 shouldPersistThread, keep the last settled copy while
                    // active work runs. A cold client resumes its durable cursor.
                    && !thread.rows().values().any(|row| {
                        let (agent_protocol::sync::Content::Full(value) | agent_protocol::sync::Content::Summary(value)) = &row.content;
                        matches!(value.as_ref(), records::Record::Turn(turn) if matches!(turn.state, records::Execution::Running { .. } | records::Execution::Waiting(agent_protocol::conversation::Wait::Retry { .. })))
                            || matches!(value.as_ref(), records::Record::Input(input) if matches!(input.state, records::Delivery::Preparing(_) | records::Delivery::Delivering))
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
mod tests {
    use super::*;

    /// Flushes no one asks for.
    fn unflushed() -> tokio::sync::mpsc::UnboundedReceiver<Flush> {
        tokio::sync::mpsc::unbounded_channel().1
    }
    use crate::models::TurnStatus;
    use crate::models::{Thread, ThreadList, Turn};
    use crate::session::ProviderKind;
    use crate::state::{Event, Intent, reduce};

    fn thread(id: &str, turns: usize, status: TurnStatus) -> Thread {
        Thread {
            id: Some(SessionRef::new(ProviderKind::Claude, id.into()).unwrap()),
            name: Some(id.into()),
            status: SessionStatus::Running,
            turns: Some(
                (0..turns)
                    .map(|index| {
                        Arc::new(Turn {
                            id: format!("{id}-{index}").into(),
                            status,
                            items: Some(vec![]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn snapshot(threads: impl IntoIterator<Item = Thread>) -> Arc<Snapshot> {
        let mut snapshot = Snapshot::default();
        for thread in threads {
            let id = thread.id.clone().unwrap();
            let replica = crate::test_conversations::replica(
                crate::models::ThreadResponse {
                    thread: thread.clone(),
                    ..Default::default()
                },
                1,
            );
            Arc::make_mut(&mut snapshot.sessions.replicas).insert(id.clone(), Arc::new(replica));
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(id, Arc::new(thread));
        }
        Arc::new(snapshot)
    }

    fn save_thread(directory: &Path, thread: Thread) {
        let id = thread.id.clone().unwrap();
        let replica = crate::test_conversations::replica(
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
            1,
        );
        write(directory, "", &id, &replica).unwrap();
    }

    fn restored(directory: &Path) -> BTreeMap<SessionRef, Arc<Thread>> {
        let mut snapshot = Snapshot::default();
        restore(directory, &mut snapshot);
        snapshot.sessions.conversations.as_ref().clone()
    }

    fn name(directory: &Path) -> Option<String> {
        restored(directory)
            .values()
            .find(|thread| matches!(thread.name.as_deref(), Some("settled" | "renamed")))?
            .name
            .clone()
    }

    fn listed() -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        snapshot.connection.storage_scope = "volume".into();
        let data = (0..24)
            .map(|index| Thread {
                id: Some(SessionRef::new(ProviderKind::Claude, format!("task-{index}")).unwrap()),
                name: Some(format!("Task {index}")),
                updated_at: Some(index as f64),
                status: SessionStatus::Running,
                ..Default::default()
            })
            .collect();
        crate::state::list_sync::seed(
            &mut snapshot,
            ThreadList {
                data,
                ..Default::default()
            },
        );
        snapshot
    }

    #[tokio::test]
    async fn first_host_results_persist_the_complete_list_cursor_for_offline_local_pagination() {
        let directory = tempfile::tempdir().unwrap();
        let source = listed();
        let mut saved = Saved::default();
        assert!(saved.write_changes(directory.path(), &source).await);
        let mut restored = Snapshot::default();
        restored.connection.storage_scope = "volume".into();
        restore(directory.path(), &mut restored);
        assert_eq!(
            restored.lists.replica.as_ref().unwrap().cursor(),
            source.lists.replica.as_ref().unwrap().cursor()
        );
        assert_eq!(restored.lists.replica.as_ref().unwrap().rows().len(), 24);
        assert_eq!(restored.lists.threads.as_ref().unwrap().data.len(), 5);
        assert!(
            restored
                .lists
                .threads
                .as_ref()
                .unwrap()
                .data
                .iter()
                .all(|row| row.status == SessionStatus::Unknown)
        );
        assert!(restored.lists.subscription.is_none());
        let (expanded, effects) = reduce(
            &restored,
            "",
            Event::Intent(Intent::ExpandThreadList { project_id: None }),
        );
        assert!(effects.is_empty());
        assert_eq!(expanded.lists.threads.as_ref().unwrap().data.len(), 15);
        assert_eq!(
            expanded.lists.replica.as_ref().unwrap().cursor(),
            restored.lists.replica.as_ref().unwrap().cursor()
        );
        assert!(!saved.write_changes(directory.path(), &expanded).await);
        let mut other = Snapshot::default();
        other.connection.storage_scope = "another-volume".into();
        restore(directory.path(), &mut other);
        assert!(other.lists.replica.is_none());
    }

    #[tokio::test]
    async fn cached_project_disclosure_keeps_its_choice_but_restarts_at_five_rows() {
        let directory = tempfile::tempdir().unwrap();
        let mut source = listed();
        let mut list = source.lists.threads.as_deref().unwrap().clone();
        // Seed all canonical rows, not the initial five-row presentation.
        list.data = source
            .lists
            .replica
            .as_ref()
            .unwrap()
            .rows()
            .values()
            .filter_map(|row| {
                let value = match &row.content {
                    agent_protocol::sync::Content::Full(value)
                    | agent_protocol::sync::Content::Summary(value) => value,
                };
                match (&row.key, value.as_ref()) {
                    (agent_protocol::sync::Key::Thread(id), records::Record::Thread(header)) => {
                        Some(records::summary(id, header))
                    }
                    _ => None,
                }
            })
            .collect();
        for thread in &mut list.data {
            thread.project_id = crate::models::ProjectMembership::Assigned("project".into());
        }
        list.projects = vec![crate::models::Project {
            id: "project".into(),
            name: "Project".into(),
            ..Default::default()
        }];
        crate::state::list_sync::seed(&mut source, list);
        Arc::make_mut(&mut source.lists.expanded_projects).insert("project".into(), 15);
        crate::state::list_sync::project(&mut source, false);
        assert_eq!(source.lists.project_threads["project"].data.len(), 15);
        assert!(
            Saved::default()
                .write_changes(directory.path(), &source)
                .await
        );
        let mut restored = Snapshot::default();
        restored.connection.storage_scope = "volume".into();
        restore(directory.path(), &mut restored);
        assert_eq!(restored.lists.expanded_projects["project"], 5);
        assert_eq!(restored.lists.project_threads["project"].data.len(), 5);
        assert!(restored.lists.project_threads["project"].has_more);
        let (expanded, effects) = reduce(
            &restored,
            "",
            Event::Intent(Intent::ExpandThreadList {
                project_id: Some("project".into()),
            }),
        );
        assert!(effects.is_empty());
        assert_eq!(expanded.lists.project_threads["project"].data.len(), 15);
    }

    #[tokio::test]
    async fn search_results_never_replace_the_saved_canonical_list_and_damage_is_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let mut source = listed();
        let mut saved = Saved::default();
        assert!(saved.write_changes(directory.path(), &source).await);
        Arc::make_mut(&mut source.lists.list_query).search_term = "needle".into();
        source.lists.search_results = Some(("needle".into(), Arc::new(ThreadList::default())));
        crate::state::list_sync::project(&mut source, false);
        assert!(!saved.write_changes(directory.path(), &source).await);
        let mut restored = Snapshot::default();
        restored.connection.storage_scope = "volume".into();
        restore(directory.path(), &mut restored);
        assert_eq!(restored.thread_list().unwrap().threads[0].title, "Task 23");
        assert!(restored.lists.list_query.search_term.is_empty());
        std::fs::write(directory.path().join("list.cache"), b"broken").unwrap();
        let mut empty = Snapshot::default();
        empty.connection.storage_scope = "volume".into();
        restore(directory.path(), &mut empty);
        assert!(empty.lists.replica.is_none());
        assert!(!directory.path().join("list.cache").exists());
    }

    /// Writes run on blocking threads, outside the paused clock.
    async fn written(directory: &Path, expected: &str) {
        for _ in 0..1000 {
            if name(directory).as_deref() == Some(expected) {
                return;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("{expected} was not written");
    }

    #[tokio::test(start_paused = true)]
    async fn windows_are_saved_after_a_pause_and_restore_latest_ten_turns() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("host-a/b");
        let (updates, receiver) = watch::channel(snapshot([]));
        let saving = tokio::spawn(run(directory.clone(), receiver, unflushed()));
        let mut running = thread("running", 2, TurnStatus::Running);
        let submission = agent_protocol::operations::Submission {
            thread_id: running.id.clone().unwrap(),
            client_user_message_id: "preparing".into(),
            input: Vec::new(),
            model: None,
            effort: None,
            service_tier: None,
            follow_up: Default::default(),
        };
        running
            .preparations
            .push(agent_protocol::session::SessionPreparation {
                submission: submission.clone(),
                status: agent_protocol::session::PreparationStatus::Preparing,
                stage: agent_protocol::session::PreparationStage::Provider,
            });
        running
            .queue
            .inputs
            .push(agent_protocol::operations::Submission {
                client_user_message_id: "queued".into(),
                ..submission
            });
        running
            .background
            .push(agent_protocol::execution::BackgroundTask {
                id: "job".into(),
                kind: agent_protocol::execution::BackgroundTaskKind::Command,
                description: None,
            });
        running.requests.insert(
            "approval".into(),
            Arc::new(agent_protocol::requests::Request {
                id: "approval".into(),
                target: agent_protocol::requests::RequestTarget::Turn {
                    turn_id: "running-1".into(),
                    item_id: None,
                },
                delivery: agent_protocol::session::RequestDelivery::Awaiting,
                body: agent_protocol::requests::RequestBody::Permission {
                    description: "permission".into(),
                    details: String::new(),
                    choices: Vec::new(),
                },
            }),
        );
        let live = snapshot([running.clone()]);
        let live = records::project(live.sessions.replicas.values().next().unwrap())
            .unwrap()
            .thread;
        assert!(
            !live.requests.is_empty()
                && !live.submissions.is_empty()
                && !live.preparations.is_empty()
                && !live.background.is_empty()
                && !live.queue.inputs.is_empty()
        );
        updates.send_replace(snapshot([
            thread("settled", 2, TurnStatus::Completed),
            running,
            thread(
                "expanded",
                HISTORY_WINDOW_TURNS as usize + 1,
                TurnStatus::Completed,
            ),
        ]));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            restored(&directory).is_empty(),
            "the first write waits half a second"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        written(&directory, "settled").await;
        let copies = restored(&directory);
        assert_eq!(
            copies.len(),
            1,
            "active and expanded conversations retain their previous settled cache"
        );
        assert!(
            copies
                .values()
                .all(|thread| thread.name.as_deref() == Some("settled"))
        );
        let copy = copies
            .values()
            .find(|thread| thread.name.as_deref() == Some("settled"))
            .unwrap();
        assert_eq!(copy.name.as_deref(), Some("settled"));
        assert_eq!(
            copy.status,
            SessionStatus::Unknown,
            "live status is not a copy"
        );

        // Later changes coalesce into one write per ten seconds.
        let mut renamed = thread("settled", 2, TurnStatus::Completed);
        renamed.name = Some("renamed".into());
        updates.send_replace(snapshot([renamed]));
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(name(&directory).as_deref(), Some("settled"));
        tokio::time::sleep(Duration::from_secs(6)).await;
        written(&directory, "renamed").await;
        drop(updates);
        saving.await.unwrap();
    }

    #[test]
    fn restoring_keeps_held_conversations_and_removes_damaged_copies() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path();
        save_thread(directory, thread("held", 1, TurnStatus::Completed));
        save_thread(directory, thread("saved", 1, TurnStatus::Completed));
        let damaged = directory.join("claude-00.bin");
        std::fs::write(&damaged, b"not a copy").unwrap();
        let mut snapshot = Snapshot::default();
        let held = thread("held", 3, TurnStatus::Completed);
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(held.id.clone().unwrap(), Arc::new(held));
        restore(directory, &mut snapshot);
        let conversations = &snapshot.sessions.conversations;
        assert_eq!(conversations.len(), 2);
        assert_eq!(
            conversations
                .values()
                .find(|thread| thread.name.as_deref() == Some("held"))
                .unwrap()
                .turns
                .as_ref()
                .unwrap()
                .len(),
            3,
            "memory is newer than a copy"
        );
        assert!(!damaged.exists());
        clear(directory);
        assert!(restored(directory).is_empty());
    }
}
