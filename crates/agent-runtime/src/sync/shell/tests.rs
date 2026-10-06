//! Ported from T3 `ShellStream.test.ts` (snapshot, coalescing, shell-to-item and
//! archive cases) and the shell-row case of `WireProjection.test.ts`. Repository
//! identity enrichment is not part of this runtime.
use super::*;
use crate::store::tests::{selection, temp_store, thread_shell};
use crate::sync::history::tests::{created, fold_into};
use crate::{ActorContext, ActorHandle, CommandOrigin, ShellProjector, ThreadShellProjector};
use agent_domain::{Command, CommandId, InteractionMode, Role, RuntimeMode};
use std::sync::Mutex;

fn shell(id: &str, archived: bool, deleted: bool) -> ShellThread {
    ShellThread {
        thread: ThreadId::new(id).unwrap(),
        row: ShellRow {
            project: "project".into(),
            archived,
            deleted,
            needs_recovery: false,
            summary: Box::new(thread_shell(id, id)),
        },
    }
}
fn thread_change(sequence: u64, id: &str) -> ShellChange {
    ShellChange::Thread {
        sequence,
        thread: shell(id, false, false),
        left_archive: false,
    }
}
fn project_change(sequence: u64, id: &str) -> ShellChange {
    ShellChange::Project {
        sequence,
        project: id.into(),
    }
}
fn removed(sequence: u64, id: &str) -> ShellUpdate {
    ShellUpdate::ThreadRemoved {
        sequence,
        thread: ThreadId::new(id).unwrap(),
    }
}

#[test]
fn never_duplicates_archived_rows_into_the_regular_shell() {
    let active = shell("thread-a", false, false);
    let archived = shell("thread-archived", true, false);
    let deleted = shell("thread-deleted", false, true);
    assert_eq!(
        shell_snapshot(
            ShellLocation::Active,
            7,
            vec![],
            vec![active.clone(), archived.clone(), deleted.clone()]
        ),
        ShellSnapshot {
            snapshot_seq: 7,
            projects: vec![],
            threads: vec![active],
        }
    );
    assert_eq!(
        shell_snapshot(
            ShellLocation::Archived,
            7,
            vec![],
            vec![archived.clone(), deleted]
        )
        .threads,
        [archived]
    );
}

#[test]
fn keeps_the_newest_change_per_aggregate_and_preserves_sequence_order() {
    let coalesced = coalesce_shell_changes(vec![
        thread_change(2, "thread-a"),
        project_change(3, "project-a"),
        thread_change(4, "thread-b"),
        thread_change(5, "thread-a"),
        project_change(6, "project-a"),
    ]);
    assert_eq!(
        coalesced
            .iter()
            .map(ShellChange::sequence)
            .collect::<Vec<_>>(),
        [4, 5, 6]
    );
}

#[test]
fn keeps_the_newest_stored_change_per_thread_and_preserves_sequence_order() {
    let coalesced = coalesce_shell_changes(vec![
        thread_change(2, "thread-a"),
        thread_change(3, "thread-b"),
        thread_change(5, "thread-a"),
    ]);
    assert_eq!(
        coalesced
            .iter()
            .map(ShellChange::sequence)
            .collect::<Vec<_>>(),
        [3, 5]
    );
}

#[test]
fn emits_an_active_thread_update_when_the_shell_is_not_archived() {
    let thread = shell("thread-a", false, false);
    assert_eq!(
        active_shell_update(4, thread.clone()),
        ShellUpdate::ThreadUpdated {
            sequence: 4,
            thread
        }
    );
}

#[test]
fn removes_archived_threads_from_the_active_only_shell() {
    assert_eq!(
        active_shell_update(4, shell("thread-a", true, false)),
        removed(4, "thread-a")
    );
}

#[test]
fn emits_an_active_shell_removal_when_an_archived_thread_is_deleted() {
    assert_eq!(
        active_shell_update(6, shell("thread-a", true, true)),
        removed(6, "thread-a")
    );
}

#[test]
fn emits_a_removal_from_the_active_list_for_other_missing_shells() {
    assert_eq!(
        active_shell_update(6, shell("thread-a", false, true)),
        removed(6, "thread-a")
    );
}

#[test]
fn emits_an_update_for_an_archived_shell() {
    let thread = shell("thread-a", true, false);
    assert_eq!(
        archived_shell_update(4, thread.clone(), false),
        Some(ShellUpdate::ThreadUpdated {
            sequence: 4,
            thread
        })
    );
}

#[test]
fn ignores_active_threads_that_never_touched_the_archive() {
    assert_eq!(
        archived_shell_update(4, shell("thread-a", false, false), false),
        None
    );
}

#[test]
fn emits_a_removal_when_a_thread_leaves_the_archive() {
    assert_eq!(
        archived_shell_update(4, shell("thread-a", false, false), true),
        Some(removed(4, "thread-a"))
    );
}

#[test]
fn emits_a_removal_when_an_archived_thread_is_deleted() {
    assert_eq!(
        archived_shell_update(4, shell("thread-a", true, true), false),
        Some(removed(4, "thread-a"))
    );
}

#[test]
fn keeps_transcript_bodies_out_of_shell_rows() {
    let mut state = created();
    fold_into(
        &mut state,
        agent_domain::FactBody::MessageCreated {
            id: agent_domain::MessageId::new("message-shell-budget").unwrap(),
            run: None,
            role: Role::Assistant,
            text: "x".repeat(1_000_000),
            attachments: vec![],
            intent: agent_domain::InputIntent::TurnStart,
            created_by: agent_domain::MessageAuthor::Agent,
            creation_source: "provider".into(),
        },
    );
    let row = ThreadShellProjector.project(&state).unwrap();
    assert!(serde_json::to_string(&row).unwrap().len() < 2_000);
}

#[derive(Default)]
struct Projects(Mutex<Vec<HostProject>>);
impl ProjectDirectory for Projects {
    fn projects(&self) -> Vec<HostProject> {
        self.0.lock().unwrap().clone()
    }
}
fn project(id: &str) -> HostProject {
    HostProject {
        id: id.into(),
        name: id.into(),
        root: format!("/work/{id}"),
    }
}

#[tokio::test(start_paused = true)]
async fn batches_and_coalesces_live_changes_per_aggregate() {
    let (changes, live) = broadcast::channel(64);
    let (sender, mut updates) = live_channel(64, LIVE_STREAM_MAX_BYTES);
    for change in [
        thread_change(3, "thread-old"),
        thread_change(11, "thread-a"),
        project_change(12, "project-a"),
        thread_change(13, "thread-b"),
        thread_change(14, "thread-a"),
        project_change(15, "project-gone"),
    ] {
        changes.send(change).unwrap();
    }
    let projects = Arc::new(Projects(Mutex::new(vec![project("project-a")])));
    let task = tokio::spawn(forward(
        vec![ShellUpdate::Synchronized],
        10,
        ShellLocation::Active,
        live,
        projects,
        sender,
    ));
    let mut received = vec![];
    for _ in 0..5 {
        received.push(updates.recv().await.unwrap());
    }
    assert_eq!(
        received,
        [
            ShellUpdate::Synchronized,
            ShellUpdate::ProjectUpdated {
                sequence: 12,
                project: project("project-a")
            },
            ShellUpdate::ThreadUpdated {
                sequence: 13,
                thread: shell("thread-b", false, false)
            },
            ShellUpdate::ThreadUpdated {
                sequence: 14,
                thread: shell("thread-a", false, false)
            },
            ShellUpdate::ProjectRemoved {
                sequence: 15,
                project: "project-gone".into()
            },
        ]
    );
    drop(changes);
    task.await.unwrap();
    assert!(updates.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn closes_a_subscriber_that_falls_behind_the_hub() {
    let (changes, live) = broadcast::channel(2);
    let (sender, mut updates) = live_channel(4, LIVE_STREAM_MAX_BYTES);
    for sequence in 1..=4 {
        changes.send(thread_change(sequence, "thread-a")).unwrap();
    }
    forward(
        vec![],
        0,
        ShellLocation::Active,
        live,
        Arc::new(Projects::default()),
        sender,
    )
    .await;
    assert!(updates.recv().await.is_none());
    assert!(updates.overflowed());
}

fn changes_for(changes: &broadcast::Sender<ShellChange>, threads: usize, payload: &str) {
    for sequence in 1..=threads {
        let mut thread = shell(&format!("thread-{sequence}"), false, false);
        thread.row.summary.title = payload.into();
        changes
            .send(ShellChange::Thread {
                sequence: sequence as u64,
                thread,
                left_archive: false,
            })
            .unwrap();
    }
}
async fn drained(updates: &mut LiveReceiver<ShellUpdate>) -> usize {
    let mut received = 0;
    while updates.recv().await.is_some() {
        received += 1;
    }
    received
}

#[tokio::test(start_paused = true)]
async fn closes_a_slow_subscriber_instead_of_waiting_for_it() {
    let (changes, live) = broadcast::channel(64);
    let (sender, mut updates) = live_channel(2, LIVE_STREAM_MAX_BYTES);
    let forwarding = tokio::spawn(forward(
        vec![],
        0,
        ShellLocation::Active,
        live,
        Arc::new(Projects::default()),
        sender,
    ));
    changes_for(&changes, 5, "row");
    tokio::time::timeout(Duration::from_secs(1), forwarding)
        .await
        .expect("forwarding ends without the subscriber reading")
        .unwrap();
    assert_eq!(drained(&mut updates).await, 2);
    assert!(updates.overflowed());
}

#[tokio::test(start_paused = true)]
async fn closes_a_subscriber_whose_undelivered_rows_exceed_the_byte_budget() {
    let (changes, live) = broadcast::channel(64);
    let mut row = shell("thread-1", false, false);
    row.row.summary.title = "x".repeat(250);
    let row_bytes = ShellUpdate::ThreadUpdated {
        sequence: 1,
        thread: row,
    }
    .live_bytes();
    assert!(row_bytes > 250);
    let (sender, mut updates) = live_channel(64, row_bytes * 5 / 2);
    let forwarding = tokio::spawn(forward(
        vec![],
        0,
        ShellLocation::Active,
        live,
        Arc::new(Projects::default()),
        sender,
    ));
    changes_for(&changes, 5, &"x".repeat(250));
    tokio::time::timeout(Duration::from_secs(1), forwarding)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(drained(&mut updates).await, 2);
}

struct Host {
    _dir: tempfile::TempDir,
    store: Store,
    context: ActorContext,
    hub: Arc<ShellHub>,
}
fn host() -> Host {
    let (dir, store) = temp_store();
    let hub = ShellHub::new(store.clone(), Arc::new(Projects::default())).unwrap();
    Host {
        _dir: dir,
        context: ActorContext::new(store.clone()),
        store,
        hub,
    }
}
async fn thread(context: &ActorContext, id: &str) -> ActorHandle {
    let thread = ThreadId::new(id).unwrap();
    let handle = ActorHandle::spawn(context.clone(), thread.clone())
        .await
        .unwrap();
    dispatch(
        &handle,
        &format!("create:{id}"),
        Command::Create {
            workspace: None,
            thread,
            project: "project".into(),
            title: id.into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
        },
    )
    .await;
    handle
}
async fn dispatch(handle: &ActorHandle, id: &str, command: Command) -> u64 {
    handle
        .dispatch(CommandId::new(id).unwrap(), command, CommandOrigin::Client)
        .await
        .unwrap()
        .global_seq
}
async fn next(subscription: &mut ShellSubscription) -> ShellUpdate {
    tokio::time::timeout(Duration::from_secs(5), subscription.updates.recv())
        .await
        .unwrap()
        .unwrap()
}
fn thread_ids(updates: &[ShellUpdate]) -> Vec<(&'static str, String)> {
    updates
        .iter()
        .map(|update| match update {
            ShellUpdate::ThreadUpdated { thread, .. } => ("updated", thread.thread.to_string()),
            ShellUpdate::ThreadRemoved { thread, .. } => ("removed", thread.to_string()),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn sends_a_snapshot_then_live_changes_after_it() {
    let h = host();
    let a = thread(&h.context, "thread-a").await;
    let b = thread(&h.context, "thread-b").await;
    dispatch(&b, "archive-b", Command::Archive { archived: true }).await;
    let mut subscription = h
        .hub
        .subscribe(ShellSubscribe {
            request_completion_marker: true,
            ..ShellSubscribe::default()
        })
        .await
        .unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut subscription).await else {
        panic!()
    };
    assert_eq!(snapshot.snapshot_seq, h.store.latest_global_seq().unwrap());
    assert_eq!(
        snapshot
            .threads
            .iter()
            .map(|thread| thread.thread.as_str())
            .collect::<Vec<_>>(),
        ["thread-a"]
    );
    assert_eq!(next(&mut subscription).await, ShellUpdate::Synchronized);

    let renamed = dispatch(&a, "rename-a", Command::Rename { title: "A".into() }).await;
    let ShellUpdate::ThreadUpdated { sequence, thread } = next(&mut subscription).await else {
        panic!()
    };
    assert_eq!((sequence, thread.thread.as_str()), (renamed, "thread-a"));
    let archived = dispatch(&a, "archive-a", Command::Archive { archived: true }).await;
    assert_eq!(next(&mut subscription).await, removed(archived, "thread-a"));
}

#[tokio::test]
async fn stops_forwarding_once_an_idle_subscriber_is_dropped() {
    let h = host();
    let mut subscription = h.hub.subscribe(ShellSubscribe::default()).await.unwrap();
    assert!(matches!(
        next(&mut subscription).await,
        ShellUpdate::Snapshot(_)
    ));
    assert_eq!(h.hub.changes.receiver_count(), 1);
    drop(subscription);
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.hub.changes.receiver_count() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the forwarding task releases its hub receiver");
}

#[tokio::test]
async fn replays_changed_rows_after_the_cursor() {
    let h = host();
    let a = thread(&h.context, "thread-a").await;
    let b = thread(&h.context, "thread-b").await;
    let after = h.store.latest_global_seq().unwrap();
    dispatch(&b, "archive-b", Command::Archive { archived: true }).await;
    dispatch(&a, "rename-a", Command::Rename { title: "A".into() }).await;
    let _c = thread(&h.context, "thread-c").await;

    let resume = |location| ShellSubscribe {
        after_global_seq: Some(after),
        request_completion_marker: true,
        location,
        ..ShellSubscribe::default()
    };
    let mut active = h
        .hub
        .subscribe(resume(ShellLocation::Active))
        .await
        .unwrap();
    let high_water = h.store.latest_global_seq().unwrap();
    assert_eq!(
        next(&mut active).await,
        ShellUpdate::Projects {
            sequence: high_water,
            projects: vec![]
        }
    );
    let mut updates = vec![];
    loop {
        match next(&mut active).await {
            ShellUpdate::Synchronized => break,
            update => updates.push(update),
        }
    }
    assert_eq!(
        thread_ids(&updates),
        [
            ("removed", "thread-b".into()),
            ("updated", "thread-a".into()),
            ("updated", "thread-c".into())
        ]
    );
    let mut archive = h
        .hub
        .subscribe(resume(ShellLocation::Archived))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut archive).await,
        ShellUpdate::Projects { .. }
    ));
    assert!(matches!(
        next(&mut archive).await,
        ShellUpdate::ThreadUpdated { thread, .. } if thread.thread.as_str() == "thread-b"
    ));
}

#[tokio::test]
async fn falls_back_to_a_snapshot_when_the_replay_is_too_large() {
    let h = host();
    h.store
        .write(|tx| {
            for index in 0..=SHELL_REPLAY_MAX_ROWS {
                tx.execute(
                    "INSERT INTO thread_shells
                         (thread_id, global_seq, project, archived, deleted, needs_recovery, payload)
                     VALUES (?1, ?2, 'project', 0, 0, 0, ?3)",
                    params![
                        format!("thread-{index}"),
                        index as i64 + 1,
                        serde_json::to_string(&thread_shell(&format!("thread-{index}"), "")).unwrap()
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut subscription = h
        .hub
        .subscribe(ShellSubscribe {
            after_global_seq: Some(0),
            ..ShellSubscribe::default()
        })
        .await
        .unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut subscription).await else {
        panic!()
    };
    assert_eq!(snapshot.threads.len(), SHELL_REPLAY_MAX_ROWS + 1);
}

#[tokio::test]
async fn measures_the_replay_budget_in_utf8_bytes() {
    let h = host();
    // Under the byte budget in characters, over it in UTF-8 bytes.
    let title = "é".repeat(SHELL_REPLAY_MAX_BYTES as usize / 2 + 1);
    assert!(title.chars().count() < SHELL_REPLAY_MAX_BYTES as usize);
    let payload = serde_json::to_string(&thread_shell("thread-wide", &title)).unwrap();
    h.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO thread_shells
                     (thread_id, global_seq, project, archived, deleted, needs_recovery, payload)
                 VALUES ('thread-wide', 1, 'project', 0, 0, 0, ?1)",
                [payload],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut subscription = h
        .hub
        .subscribe(ShellSubscribe {
            after_global_seq: Some(0),
            ..ShellSubscribe::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        next(&mut subscription).await,
        ShellUpdate::Snapshot(snapshot) if snapshot.threads.len() == 1
    ));
}

// T3 ws.ts replays durable project events on resume; projects here live outside the
// fact log, so a resumed stream starts from the complete live project list. Each
// project change still takes a global sequence.
#[tokio::test]
async fn resumes_with_project_changes_made_while_disconnected() {
    let (_dir, store) = temp_store();
    let projects = Arc::new(Projects(Mutex::new(vec![
        project("project-a"),
        project("project-b"),
    ])));
    let hub = ShellHub::new(store.clone(), projects.clone()).unwrap();
    let context = ActorContext::new(store.clone());
    let _thread = thread(&context, "thread-a").await;
    let mut first = hub.subscribe(ShellSubscribe::default()).await.unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut first).await else {
        panic!()
    };
    drop(first);

    let mut renamed = project("project-a");
    renamed.name = "Renamed".into();
    *projects.0.lock().unwrap() = vec![renamed.clone(), project("project-c")];
    for id in ["project-a", "project-b", "project-c"] {
        hub.project_changed(id).await.unwrap();
    }
    let high_water = store.latest_global_seq().unwrap();
    assert_eq!(high_water, snapshot.snapshot_seq + 3);

    let mut resumed = hub
        .subscribe(ShellSubscribe {
            after_global_seq: Some(snapshot.snapshot_seq),
            request_completion_marker: true,
            ..ShellSubscribe::default()
        })
        .await
        .unwrap();
    assert_eq!(
        next(&mut resumed).await,
        ShellUpdate::Projects {
            sequence: high_water,
            projects: vec![renamed, project("project-c")]
        }
    );
    assert_eq!(next(&mut resumed).await, ShellUpdate::Synchronized);
}

#[tokio::test]
async fn reports_project_changes_from_the_directory() {
    let (dir, store) = temp_store();
    let projects = Arc::new(Projects(Mutex::new(vec![project("project-a")])));
    let hub = ShellHub::new(store, projects.clone()).unwrap();
    let mut subscription = hub.subscribe(ShellSubscribe::default()).await.unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut subscription).await else {
        panic!()
    };
    assert_eq!(snapshot.projects, [project("project-a")]);
    projects.0.lock().unwrap().clear();
    hub.project_changed("project-a").await.unwrap();
    // T3 shellReducer drops a change at or below the snapshot sequence.
    assert_eq!(
        next(&mut subscription).await,
        ShellUpdate::ProjectRemoved {
            sequence: snapshot.snapshot_seq + 1,
            project: "project-a".into()
        }
    );
    drop(dir);
}

#[tokio::test]
async fn orders_project_changes_between_thread_commits() {
    let (_dir, store) = temp_store();
    let projects = Arc::new(Projects(Mutex::new(vec![project("project-a")])));
    let hub = ShellHub::new(store.clone(), projects.clone()).unwrap();
    let context = ActorContext::new(store.clone());
    let mut subscription = hub.subscribe(ShellSubscribe::default()).await.unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut subscription).await else {
        panic!()
    };
    hub.project_changed("project-a").await.unwrap();
    let ShellUpdate::ProjectUpdated {
        sequence: changed, ..
    } = next(&mut subscription).await
    else {
        panic!()
    };
    let _thread = thread(&context, "thread-a").await;
    let ShellUpdate::ThreadUpdated {
        sequence: created, ..
    } = next(&mut subscription).await
    else {
        panic!()
    };
    assert!(snapshot.snapshot_seq < changed && changed < created);

    let mut resumed = hub
        .subscribe(ShellSubscribe {
            after_global_seq: Some(changed),
            ..ShellSubscribe::default()
        })
        .await
        .unwrap();
    assert!(
        matches!(next(&mut resumed).await, ShellUpdate::Projects { sequence, .. } if sequence == created)
    );
    assert!(
        matches!(next(&mut resumed).await, ShellUpdate::ThreadUpdated { sequence, .. } if sequence == created)
    );
}

// T3 ProjectionStore lists shell rows by `updated_at`, then thread id.
#[tokio::test]
async fn lists_snapshot_rows_by_update_time_then_thread_id() {
    let (_dir, store) = temp_store();
    let hub = ShellHub::new(store.clone(), Arc::new(Projects::default())).unwrap();
    let clock = Arc::new(crate::ManualClock::new(
        &agent_domain::Timestamp::from_millis(1_000).unwrap(),
    ));
    let mut context = ActorContext::new(store);
    context.clock = clock.clone();
    let late = thread(&context, "thread-0").await;
    thread(&context, "thread-b").await;
    thread(&context, "thread-a").await;
    clock.advance(1_000);
    dispatch(
        &late,
        "rename",
        Command::Rename {
            title: "Later".into(),
        },
    )
    .await;
    let mut subscription = hub.subscribe(ShellSubscribe::default()).await.unwrap();
    let ShellUpdate::Snapshot(snapshot) = next(&mut subscription).await else {
        panic!()
    };
    let ids: Vec<_> = snapshot
        .threads
        .iter()
        .map(|thread| thread.thread.as_str())
        .collect();
    assert_eq!(ids, ["thread-a", "thread-b", "thread-0"]);
}
