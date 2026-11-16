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
        let replica = crate::test_conversations::projection(
            crate::models::ThreadResponse {
                thread: thread.clone(),
                ..Default::default()
            },
            1,
        );
        Arc::make_mut(&mut snapshot.sessions.projections).insert(id.clone(), Arc::new(replica));
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(id, Arc::new(thread));
    }
    Arc::new(snapshot)
}

fn save_thread(directory: &Path, thread: Thread) {
    let id = thread.id.clone().unwrap();
    let replica = crate::test_conversations::projection(
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
            last_input_at: Some(index as i64),
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
async fn an_unexpanded_window_with_more_than_ten_background_turns_is_cached() {
    let directory = tempfile::tempdir().unwrap();
    let source = snapshot([thread("wakes", 11, TurnStatus::Completed)]);
    assert!(
        Saved::default()
            .write_changes(directory.path(), &source)
            .await
    );
    assert_eq!(restored(directory.path()).len(), 1);
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
        restored.lists.shell.as_ref().unwrap().snapshot_sequence,
        source.lists.shell.as_ref().unwrap().snapshot_sequence
    );
    assert_eq!(restored.lists.shell.as_ref().unwrap().threads.len(), 24);
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
        expanded.lists.shell.as_ref().unwrap().snapshot_sequence,
        restored.lists.shell.as_ref().unwrap().snapshot_sequence
    );
    assert!(!saved.write_changes(directory.path(), &expanded).await);
    let mut other = Snapshot::default();
    other.connection.storage_scope = "another-volume".into();
    restore(directory.path(), &mut other);
    assert!(other.lists.shell.is_none());
}

#[tokio::test]
async fn cached_project_disclosure_keeps_its_choice_but_restarts_at_five_rows() {
    let directory = tempfile::tempdir().unwrap();
    let mut source = listed();
    let mut list = source.lists.threads.as_deref().unwrap().clone();
    // Seed all canonical rows, not the initial five-row presentation.
    list.data = source
        .lists
        .shell
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .map(|thread| agent_protocol::orchestration::shell::summary(thread, None).unwrap())
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
    assert!(empty.lists.shell.is_none());
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
        fresh_context: false,
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
    let projection = live.sessions.projections.values().next().unwrap();
    let session = live.sessions.projections.keys().next().unwrap();
    let live = crate::orchestration::project(
        projection,
        session,
        ProviderKind::Claude,
        crate::session::Capabilities::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap()
    .thread;
    assert!(
        !live.requests.is_empty()
            && !live.submissions.is_empty()
            && !live.preparations.is_empty()
            && !live.background.is_empty()
            && !live.queue.inputs.is_empty()
    );
    let mut expanded = thread("expanded", 11, TurnStatus::Completed);
    expanded.history_cursor = Some("older".into());
    let mut settled = thread("settled", 2, TurnStatus::Completed);
    let command = crate::models::Item::new(
        "command".into(),
        crate::models::ItemStatus::Completed,
        crate::models::ItemBody::CommandExecution {
            command: "git status".into(),
            cwd: None,
            output: "working tree clean".into(),
            exit_code: Some(0),
            output_omitted: false,
        },
    );
    Arc::make_mut(&mut settled.turns.as_mut().unwrap()[0]).items =
        Some(vec![Arc::new(command.clone())]);
    let mut incoming = snapshot([settled, running, expanded]);
    let id = SessionRef::new(ProviderKind::Claude, "expanded".into()).unwrap();
    Arc::make_mut(&mut Arc::make_mut(&mut incoming).sessions.expanded_history).insert(id);
    updates.send_replace(incoming);
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
        copy.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].as_ref(),
        &command,
        "already received tool bodies remain available offline"
    );
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
