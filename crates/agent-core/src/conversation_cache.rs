//! Device copies of the conversation list and opened conversations, one
//! directory per Host, shown before the Host confirms them.
//!
//! A copy is the newest window of a settled conversation: running and
//! expanded conversations keep their last settled copy. Writes follow the
//! first change after half a second, then at most every ten seconds, keeping
//! only the latest state. Copies have no count or size limit. A damaged or
//! older copy is removed when read; removing a Host removes its directory.
//! Copies are best effort: failed reads and writes leave the Host as the source.
use crate::{
    models::{ListQuery, SessionStatus, Thread, ThreadList, TurnStatus},
    session::{HistoryReadKind, SessionRef},
    state::{
        Snapshot,
        operations::{HISTORY_WINDOW_TURNS, OperationKey},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

/// Copies restored at startup, most recently saved first.
const RESTORED: usize = 20;
/// Copies in another format are discarded instead of migrated.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Copy {
    format: u32,
    thread: Thread,
}

/// Like T3 Code's separate shell/thread caches (cefb544a), the list has no
/// conversation bodies. Search results must never replace the unfiltered list.
#[derive(Clone, Serialize, Deserialize)]
struct ListCopy {
    format: u32,
    storage_scope: String,
    query: Arc<ListQuery>,
    threads: Arc<ThreadList>,
    projects: Arc<BTreeMap<String, Arc<ThreadList>>>,
    expanded: Arc<BTreeMap<String, u32>>,
}

impl ListCopy {
    fn unchanged(&self, previous: &Self) -> bool {
        self.storage_scope == previous.storage_scope
            && self.query == previous.query
            && Arc::ptr_eq(&self.threads, &previous.threads)
            && Arc::ptr_eq(&self.projects, &previous.projects)
            && self.expanded == previous.expanded
    }

    fn write(mut self, path: &Path) -> std::io::Result<()> {
        self.threads = Arc::new(list_metadata(&self.threads));
        self.projects = Arc::new(
            self.projects
                .iter()
                .map(|(id, page)| (id.clone(), Arc::new(list_metadata(page))))
                .collect(),
        );
        write_value(path, self)
    }
}

fn list_metadata(list: &ThreadList) -> ThreadList {
    ThreadList {
        data: list
            .data
            .iter()
            .map(|thread| Thread {
                id: thread.id.clone(),
                active_provider: thread.active_provider,
                parent_id: thread.parent_id.clone(),
                name: thread.name.clone(),
                cwd: thread.cwd.clone(),
                worktree_status: thread.worktree_status,
                git_branch: thread.git_branch.clone(),
                project_id: thread.project_id.clone(),
                preview: thread.preview.clone(),
                updated_at: thread.updated_at,
                agent_id: thread.agent_id.clone(),
                ..Default::default()
            })
            .collect(),
        ..list.clone()
    }
}

fn read_list(directory: &Path, storage_scope: &str) -> Option<ListCopy> {
    let path = directory.join("list.cache");
    let copy = std::fs::read(&path)
        .ok()
        .and_then(|bytes| agent_protocol::protocol::decode::<ListCopy>(&bytes).ok())
        .filter(|copy| copy.format == FORMAT && copy.query.search_term.trim().is_empty());
    if copy.is_none() {
        let _ = std::fs::remove_file(path);
    }
    copy.filter(|copy| copy.storage_scope == storage_scope)
}

fn file(directory: &Path, id: &SessionRef) -> PathBuf {
    let name: String = id.id.bytes().map(|byte| format!("{byte:02x}")).collect();
    directory.join(format!("{}-{name}.bin", id.provider.key()))
}

fn savable(thread: &Thread) -> bool {
    thread.id.is_some()
        && thread.preparation.is_none()
        && thread.turns.as_ref().is_some_and(|turns| {
            turns.len() <= HISTORY_WINDOW_TURNS as usize
                && turns.iter().all(|turn| turn.status != TurnStatus::Running)
        })
        && thread.history_read_state.as_ref().is_none_or(|state| {
            !matches!(
                state.kind,
                HistoryReadKind::Unavailable | HistoryReadKind::Incomplete
            )
        })
}

/// Live execution and pending input belong to the Host, not to a copy.
fn copy(thread: &Thread) -> Thread {
    Thread {
        requests: Default::default(),
        submissions: Default::default(),
        status: SessionStatus::Unknown,
        list_stale: None,
        ..thread.clone()
    }
}

fn read(path: &Path) -> Option<Thread> {
    let copy = std::fs::read(path)
        .ok()
        .and_then(|bytes| agent_protocol::protocol::decode::<Copy>(&bytes).ok())
        .filter(|copy| copy.format == FORMAT && copy.thread.id.is_some());
    if copy.is_none() {
        let _ = std::fs::remove_file(path);
    }
    copy.map(|copy| copy.thread)
}

fn write(directory: &Path, thread: &Thread) -> std::io::Result<()> {
    let id = thread.id.as_ref().expect("savable threads have IDs");
    write_value(
        &file(directory, id),
        Copy {
            format: FORMAT,
            thread: copy(thread),
        },
    )
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
    if snapshot.lists.threads.is_none()
        && let Some(mut list) = read_list(directory, &snapshot.connection.storage_scope)
    {
        // Cached rows can appear immediately, but every list starts from its
        // first page on each launch, including previously expanded tasks.
        let query = ListQuery::default();
        let project_limit = query.project_limit;
        let threads = Arc::make_mut(&mut list.threads);
        threads.has_more_projects |= threads.projects.len() > project_limit as usize;
        threads.projects.truncate(project_limit as usize);
        let visible: std::collections::HashSet<_> = threads
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect();
        threads
            .project_pages
            .retain(|id, _| visible.contains(id.as_str()));
        Arc::make_mut(&mut list.projects).retain(|id, _| visible.contains(id.as_str()));
        Arc::make_mut(&mut list.expanded).retain(|id, _| visible.contains(id.as_str()));
        for page in
            std::iter::once(&mut list.threads).chain(Arc::make_mut(&mut list.projects).values_mut())
        {
            let page = Arc::make_mut(page);
            page.limit = query.limit;
            page.has_more |= page.data.len() > query.limit as usize;
            page.data.truncate(query.limit as usize);
        }
        for limit in Arc::make_mut(&mut list.expanded).values_mut() {
            *limit = query.limit;
        }
        snapshot.lists.list_query = Arc::new(query);
        snapshot.lists.threads = Some(list.threads);
        snapshot.lists.project_threads = list.projects;
        snapshot.lists.expanded_projects = list.expanded;
        snapshot.lists.unconfirmed_lists = Arc::new(
            std::iter::once(OperationKey::SessionList)
                .chain(snapshot.lists.project_threads.keys().map(|project_id| {
                    OperationKey::ProjectList {
                        project_id: project_id.clone(),
                    }
                }))
                .collect(),
        );
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
    let conversations = Arc::make_mut(&mut snapshot.sessions.conversations);
    for (_, path) in files.into_iter().take(RESTORED) {
        if let Some(thread) = read(&path) {
            let id = thread.id.clone().expect("read copies have IDs");
            conversations.entry(id).or_insert_with(|| Arc::new(thread));
        }
    }
}

/// Remove a Host's copies, such as when it is paired again or removed.
pub fn clear(directory: &Path) {
    let _ = std::fs::remove_dir_all(directory);
}

/// Save changed lists and settled conversations until the Store stops publishing.
pub async fn run(directory: PathBuf, mut updates: watch::Receiver<Arc<Snapshot>>) {
    // The state published before this task started is written as a change.
    let mut saved = BTreeMap::<SessionRef, Arc<Thread>>::new();
    let mut saved_list: Option<ListCopy> = None;
    updates.mark_changed();
    while updates.changed().await.is_ok() {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let snapshot = updates.borrow_and_update().clone();
        let mut writing = false;
        if snapshot.connection.connected
            && !snapshot.lists.unconfirmed_lists.contains(&OperationKey::SessionList)
            && snapshot.lists.list_query.search_term.trim().is_empty()
            // A pending or failed search reset still holds the previous query's rows.
            && !snapshot.operations.get(&OperationKey::SessionList).is_some()
            && let Some(threads) = &snapshot.lists.threads
        {
            let list = ListCopy {
                format: FORMAT,
                storage_scope: snapshot.connection.storage_scope.clone(),
                query: snapshot.lists.list_query.clone(),
                threads: threads.clone(),
                projects: snapshot.lists.project_threads.clone(),
                expanded: snapshot.lists.expanded_projects.clone(),
            };
            if saved_list
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
                    saved_list = Some(list);
                }
            }
        }
        let changed: Vec<_> = snapshot
            .sessions
            .conversations
            .iter()
            .filter(|(id, thread)| {
                saved
                    .get(*id)
                    .is_none_or(|previous| !Arc::ptr_eq(previous, thread))
                    && savable(thread)
            })
            .map(|(id, thread)| (id.clone(), thread.clone()))
            .collect();
        if !changed.is_empty() {
            writing = true;
            let target = directory.clone();
            let pending = changed.clone();
            let written = tokio::task::spawn_blocking(move || {
                pending
                    .iter()
                    .map(|(_, thread)| write(&target, thread).is_ok())
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            for ((id, thread), written) in changed.into_iter().zip(written) {
                if written {
                    saved.insert(id, thread);
                }
            }
        }
        if writing {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Project, ProjectMembership, Turn};
    use crate::session::ProviderKind;
    use crate::state::{Event, Intent, operations as op, reduce};
    use op::Operation;

    fn thread(id: &str, turns: usize, status: TurnStatus) -> Thread {
        Thread {
            id: Some(SessionRef::new(ProviderKind::Claude, id.into()).unwrap()),
            name: Some(id.into()),
            status: SessionStatus::Running,
            transcript_version: Some(7),
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
        Arc::new(Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    threads
                        .into_iter()
                        .map(|thread| (thread.id.clone().unwrap(), Arc::new(thread)))
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn restored(directory: &Path) -> BTreeMap<SessionRef, Arc<Thread>> {
        let mut snapshot = Snapshot::default();
        restore(directory, &mut snapshot);
        snapshot.sessions.conversations.as_ref().clone()
    }

    fn name(directory: &Path) -> Option<String> {
        restored(directory).values().next()?.name.clone()
    }

    fn page(data: Vec<Thread>, projects: Vec<Project>) -> Arc<ThreadList> {
        Arc::new(ThreadList {
            revision: 17,
            limit: 5,
            data,
            projects,
            has_more: true,
            has_more_projects: false,
            project_pages: Default::default(),
        })
    }

    fn listed(title: &str) -> Arc<Snapshot> {
        let mut standalone = thread(title, 2, TurnStatus::Completed);
        standalone.project_id = ProjectMembership::Unassigned {};
        let mut child = thread("project chat", 1, TurnStatus::Completed);
        child.project_id = ProjectMembership::Assigned("p".into());
        let mut state = (*snapshot([standalone.clone(), child.clone()])).clone();
        state.connection.storage_scope = "volume".into();
        state.connection.connected = true;
        state.lists.threads = Some(page(
            vec![standalone],
            vec![Project {
                id: "p".into(),
                name: "Project".into(),
                ..Default::default()
            }],
        ));
        state.lists.project_threads =
            Arc::new(BTreeMap::from([("p".into(), page(vec![child], vec![]))]));
        state.lists.expanded_projects = Arc::new(BTreeMap::from([("p".into(), 5)]));
        Arc::new(state)
    }

    fn restored_state(directory: &Path, scope: &str) -> Snapshot {
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                storage_scope: scope.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        restore(directory, &mut snapshot);
        snapshot
    }

    async fn list_written(directory: &Path, title: &str) {
        for _ in 0..1000 {
            let state = restored_state(directory, "volume");
            if state
                .thread_list()
                .is_some_and(|list| list.threads.first().is_some_and(|row| row.title == title))
                && state.sessions.conversations.len() == 2
            {
                return;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("list {title} was not written");
    }

    #[tokio::test(start_paused = true)]
    async fn first_host_results_are_saved_without_an_offline_startup_cooldown() {
        let directory = tempfile::tempdir().unwrap();
        let (updates, receiver) = watch::channel(Arc::new(Snapshot::default()));
        let saving = tokio::spawn(run(directory.path().into(), receiver));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!directory.path().join("list.cache").exists());
        updates.send_replace(listed("first host result"));
        tokio::time::sleep(Duration::from_millis(600)).await;
        list_written(directory.path(), "first host result").await;
        drop(updates);
        saving.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cached_list_and_project_chats_open_before_connection_and_refresh_without_disappearing()
    {
        let directory = tempfile::tempdir().unwrap();
        let (updates, receiver) = watch::channel(listed("saved chat"));
        let saving = tokio::spawn(run(directory.path().into(), receiver));
        tokio::time::sleep(Duration::from_millis(600)).await;
        list_written(directory.path(), "saved chat").await;
        drop(updates);
        saving.await.unwrap();

        let offline = restored_state(directory.path(), "volume");
        assert!(!offline.connection.connected);
        let list = offline.thread_list().unwrap();
        assert_eq!(list.threads[0].title, "saved chat");
        assert!(
            !list.threads[0].active,
            "a cache is not live execution status"
        );
        assert!(list.has_more);
        assert_eq!(list.projects[0].name, "Project");
        assert_eq!(list.projects[0].threads[0].title, "project chat");
        assert!(
            offline.lists.threads.as_ref().unwrap().data[0]
                .turns
                .is_none(),
            "list records do not duplicate conversation bodies"
        );

        let id = list.threads[0].id.clone();
        let (mut selected, _) = reduce(
            &offline,
            Event::Intent(Intent::ReadThread(op::ReadThread::open(id))),
        );
        assert_eq!(
            selected
                .conversation_thread()
                .unwrap()
                .turns
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        let (mut connected, _) = reduce(&selected, Event::Connected);
        assert_eq!(
            connected.thread_list().unwrap().threads[0].title,
            "saved chat"
        );

        // A lower revision from a restarted Host must replace the saved rows.
        let fresh = listed("fresh chat");
        let mut response = fresh.lists.threads.as_ref().unwrap().as_ref().clone();
        response.revision = 1;
        op::ListSessions::new((*connected.lists.list_query).clone())
            .apply(&mut connected, response);
        assert_eq!(
            connected.thread_list().unwrap().threads[0].title,
            "fresh chat"
        );

        // Reopening a held project shows its rows and refreshes them.
        Arc::make_mut(&mut connected.lists.expanded_projects).clear();
        let (opened, _) = reduce(
            &connected,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "p".into(),
                expanded: true,
            }),
        );
        assert_eq!(
            opened.thread_list().unwrap().projects[0].threads[0].title,
            "project chat"
        );

        // Reading a bad list must leave local drafts and saved bodies usable.
        assert!(
            restored_state(directory.path(), "another volume")
                .thread_list()
                .is_none()
        );
        let path = directory.path().join("list.cache");
        std::fs::write(&path, b"damaged list").unwrap();
        Arc::make_mut(&mut selected.input.drafts).insert(
            "draft".into(),
            Arc::new(crate::state::Draft {
                text: "keep this".into(),
                ..Default::default()
            }),
        );
        selected.lists.threads = None;
        restore(directory.path(), &mut selected);
        assert!(!path.exists());
        assert!(selected.thread_list().is_none());
        assert_eq!(selected.input.drafts[&"draft".into()].text, "keep this");
        assert_eq!(selected.sessions.conversations.len(), 2);
        clear(directory.path());
        assert!(restored(directory.path()).is_empty());
    }

    #[rstest::rstest]
    #[case(5, false)]
    #[case(5, true)]
    #[case(15, false)]
    #[case(25, true)]
    #[tokio::test(start_paused = true)]
    async fn cached_project_pagination_restarts_at_five(
        #[case] count: u32,
        #[case] has_more: bool,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let mut saved = (*listed("saved chat")).clone();
        Arc::make_mut(&mut saved.lists.list_query).project_limit = count;
        let root = Arc::make_mut(saved.lists.threads.as_mut().unwrap());
        root.has_more_projects = has_more;
        for index in 1..count {
            let id = format!("p{index}");
            root.projects.push(Project {
                id: id.clone(),
                name: id.clone(),
                ..Default::default()
            });
            Arc::make_mut(&mut saved.lists.project_threads)
                .insert(id.clone(), page(vec![], vec![]));
            Arc::make_mut(&mut saved.lists.expanded_projects).insert(id, 5);
        }
        root.project_pages = root
            .projects
            .iter()
            .map(|project| {
                (
                    project.id.clone(),
                    crate::models::ListPage {
                        limit: 5,
                        has_more: true,
                    },
                )
            })
            .collect();
        Arc::make_mut(&mut saved.lists.expanded_projects).remove("p1");
        let (updates, receiver) = watch::channel(Arc::new(saved));
        let saving = tokio::spawn(run(directory.path().into(), receiver));
        tokio::time::sleep(Duration::from_millis(600)).await;
        list_written(directory.path(), "saved chat").await;
        drop(updates);
        saving.await.unwrap();

        let offline = restored_state(directory.path(), "volume");
        let list = offline.thread_list().unwrap();
        assert_eq!(list.projects.len(), 5);
        assert_eq!(list.projects[0].name, "Project");
        assert_eq!(list.projects[0].threads[0].title, "project chat");
        assert!(list.projects[0].expanded);
        assert!(!list.projects[1].expanded);
        assert_eq!(list.has_more_projects, has_more || count > 5);
        assert_eq!(offline.lists.project_threads.len(), 5);
        assert_eq!(
            offline.lists.threads.as_ref().unwrap().project_pages.len(),
            5
        );
        assert_eq!(offline.lists.expanded_projects.len(), 4);
        assert_eq!(offline.lists.unconfirmed_lists.len(), 6);

        let (connected, _) = reduce(&offline, Event::Connected);
        let query = op::ListSessions::new((*connected.lists.list_query).clone())
            .capture(&connected)
            .unwrap();
        assert_eq!(query.project_limit, 5);
        assert_eq!(
            query.project_limits.unwrap(),
            [
                ("p".into(), 5),
                ("p2".into(), 5),
                ("p3".into(), 5),
                ("p4".into(), 5)
            ]
            .into()
        );

        let (expanded, _) = reduce(&connected, Event::Intent(Intent::ExpandProjects));
        assert_eq!(expanded.lists.list_query.project_limit, 15);
    }

    #[rstest::rstest]
    #[case(0, false)]
    #[case(4, false)]
    #[case(5, false)]
    #[case(5, true)]
    #[case(6, false)]
    #[case(15, false)]
    #[case(25, true)]
    fn cached_task_pagination_restarts_at_five(
        #[case] count: u32,
        #[case] has_more: bool,
        #[values(false, true)] expanded: bool,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let rows = |project: ProjectMembership| {
            (0..count)
                .map(|index| Thread {
                    id: Some(
                        SessionRef::new(ProviderKind::Codex, format!("task-{index}")).unwrap(),
                    ),
                    name: Some(format!("Task {index}")),
                    project_id: project.clone(),
                    ..Default::default()
                })
                .collect()
        };
        let mut saved = (*listed("saved chat")).clone();
        Arc::make_mut(&mut saved.lists.list_query).limit = count.max(5);
        let root = Arc::make_mut(saved.lists.threads.as_mut().unwrap());
        root.data = rows(ProjectMembership::Unassigned {});
        root.limit = count.max(5);
        root.has_more = has_more;
        let project = Arc::make_mut(
            Arc::make_mut(&mut saved.lists.project_threads)
                .get_mut("p")
                .unwrap(),
        );
        project.data = rows(ProjectMembership::Assigned("p".into()));
        project.limit = count.max(5);
        project.has_more = has_more;
        saved.lists.expanded_projects = Arc::new(
            expanded
                .then_some(("p".into(), count.max(5)))
                .into_iter()
                .collect(),
        );
        ListCopy {
            format: FORMAT,
            storage_scope: "volume".into(),
            query: saved.lists.list_query,
            threads: saved.lists.threads.unwrap(),
            projects: saved.lists.project_threads,
            expanded: saved.lists.expanded_projects,
        }
        .write(&directory.path().join("list.cache"))
        .unwrap();

        let offline = restored_state(directory.path(), "volume");
        assert_eq!(*offline.lists.list_query, ListQuery::default());
        for page in std::iter::once(offline.lists.threads.as_ref().unwrap())
            .chain(offline.lists.project_threads.values())
        {
            assert_eq!(page.limit, 5);
            assert_eq!(page.data.len(), count.min(5) as usize);
            assert_eq!(page.has_more, has_more || count > 5);
            for (index, row) in page.data.iter().enumerate() {
                assert_eq!(row.name.as_deref(), Some(format!("Task {index}").as_str()));
            }
        }
        let (connected, _) = reduce(&offline, Event::Connected);
        let query = op::ListSessions::new((*connected.lists.list_query).clone())
            .capture(&connected)
            .unwrap();
        assert_eq!(query.limit, 5);
        assert_eq!(
            query.project_limits.unwrap(),
            expanded.then_some(("p".into(), 5)).into_iter().collect()
        );
        let (opened, _) = reduce(
            &connected,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "p".into(),
                expanded: true,
            }),
        );
        let project = &opened.thread_list().unwrap().projects[0];
        assert!(project.expanded);
        assert_eq!(project.threads.len(), count.min(5) as usize);
        assert_eq!(project.has_more, has_more || count > 5);

        let (more, _) = reduce(
            &opened,
            Event::Intent(Intent::ExpandThreadList {
                project_id: Some("p".into()),
            }),
        );
        assert_eq!(more.lists.expanded_projects["p"], 15);
        let (more, _) = reduce(
            &more,
            Event::Intent(Intent::ExpandThreadList { project_id: None }),
        );
        assert_eq!(more.lists.list_query.limit, 15);
    }

    #[rstest::rstest]
    #[case("search")]
    #[case("offline")]
    #[case("search reset pending")]
    #[case("search reset failed")]
    #[tokio::test(start_paused = true)]
    async fn search_and_offline_changes_do_not_replace_the_saved_list(#[case] state: &str) {
        let directory = tempfile::tempdir().unwrap();
        let (updates, receiver) = watch::channel(listed("saved chat"));
        let saving = tokio::spawn(run(directory.path().into(), receiver));
        tokio::time::sleep(Duration::from_millis(600)).await;
        list_written(directory.path(), "saved chat").await;
        let mut next = (*listed("not the full live list")).clone();
        match state {
            "search" => Arc::make_mut(&mut next.lists.list_query).search_term = "filtered".into(),
            "offline" => next.connection.connected = false,
            _ => {
                next.operations.insert(
                    OperationKey::SessionList,
                    op::OperationState {
                        generation: 1,
                        phase: if state == "search reset pending" {
                            op::OperationPhase::Running
                        } else {
                            op::OperationPhase::Failed {
                                message: "unavailable".into(),
                            }
                        },
                    },
                );
            }
        }
        updates.send_replace(Arc::new(next));
        drop(updates);
        saving.await.unwrap();
        assert_eq!(
            restored_state(directory.path(), "volume")
                .thread_list()
                .unwrap()
                .threads[0]
                .title,
            "saved chat"
        );
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
    async fn settled_windows_are_saved_after_a_pause_and_restored() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("host-a/b");
        let (updates, receiver) = watch::channel(snapshot([]));
        let saving = tokio::spawn(run(directory.clone(), receiver));
        updates.send_replace(snapshot([
            thread("settled", 2, TurnStatus::Completed),
            thread("running", 2, TurnStatus::Running),
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
        assert_eq!(copies.len(), 1);
        let copy = copies.values().next().unwrap();
        assert_eq!(copy.name.as_deref(), Some("settled"));
        assert_eq!(copy.transcript_version, Some(7));
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
        write(directory, &thread("held", 1, TurnStatus::Completed)).unwrap();
        write(directory, &thread("saved", 1, TurnStatus::Completed)).unwrap();
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
