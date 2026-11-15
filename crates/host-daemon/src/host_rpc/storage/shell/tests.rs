use super::super::test_support::{NOW, thread as thread_record};
use super::*;
use serde_json::json;

fn create(storage: &Storage, id: &str) {
    storage
        .thread_command(
            super::super::commands::Command {
                id: &format!("create:{id}"),
                thread_id: id,
                kind: "thread.create",
                now: NOW,
                item_ids: &[],
            },
            Some(&thread_record(id)),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
}
fn update(storage: &Storage, id: &str, title: &str) {
    storage
        .observe_thread(id, &[], NOW, |state| {
            let mut thread = state.thread.clone();
            thread.title = title.into();
            Ok(model::Decision {
                changes: vec![model::Change::Thread(thread)],
                ..Default::default()
            })
        })
        .unwrap();
}
#[test]
fn shell_reads_metadata_and_counts_without_deserializing_message_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    create(&storage, "thread");
    storage.with(|connection| {
        let tx=connection.transaction().unwrap();
        let item:model::TurnItem=serde_json::from_value(json!({"id":"body","threadId":"thread","runId":null,"nodeId":null,
            "providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,"parentItemId":null,"ordinal":1,
            "status":"completed","title":null,"startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,
            "type":"assistant_message","messageId":"body","text":"body","attachments":[],"streaming":false})).unwrap();
        projections::save(&tx,Some("thread"),&item).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_turn_items SET payload_json='{\"type\":\"unrecognized-body\",\"text\":\"body must stay unread\"}' WHERE turn_item_id='body'",[]).unwrap();
        tx.commit().unwrap();Ok(())
    }).unwrap();
    let shell = storage.shell_snapshot().unwrap();
    assert_eq!(
        (
            shell.threads[0].item_count,
            shell.threads[0].visible_item_count
        ),
        (1, 1)
    );
    assert!(shell.threads[0].latest_visible_message.is_none());
    assert!(
        !serde_json::to_string(&shell)
            .unwrap()
            .contains("body must stay unread")
    );
}
#[test]
fn reconnect_replays_compact_changes_and_recovers_future_and_pruned_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    create(&storage, "thread");
    let before = storage.shell_snapshot().unwrap();
    update(&storage, "thread", "new title");
    let (current, items) = storage.open_shell(Some(before.snapshot_sequence)).unwrap();
    assert!(
        matches!(&items[..],[ShellStreamItem::ThreadUpdated {thread,..},ShellStreamItem::Synchronized] if thread.thread.title=="new title")
    );
    let (_, fresh) = storage
        .open_shell(Some(current.snapshot_sequence + 1))
        .unwrap();
    assert!(matches!(
        fresh.first(),
        Some(ShellStreamItem::Snapshot { .. })
    ));
    storage.with(|connection| {
        connection.execute("INSERT INTO orchestration_v2_projection_metadata(projection_name,schema_version,last_sequence,updated_at) VALUES ('event-log-pruned-through',2,?1,?2)",params![integer(current.snapshot_sequence)?,NOW]).unwrap();Ok(())
    }).unwrap();
    let (_, fresh) = storage.open_shell(Some(before.snapshot_sequence)).unwrap();
    assert!(matches!(
        fresh.first(),
        Some(ShellStreamItem::Snapshot { .. })
    ));
    assert_eq!(
        storage
            .open_shell(Some(current.snapshot_sequence))
            .unwrap()
            .1,
        [ShellStreamItem::Synchronized]
    );
    update(&storage, "thread", "after pruning");
    let (_, replay) = storage.open_shell(Some(current.snapshot_sequence)).unwrap();
    assert!(
        matches!(&replay[..], [ShellStreamItem::ThreadUpdated { thread, .. }, ShellStreamItem::Synchronized]
        if thread.thread.title == "after pruning")
    );
}
#[test]
fn shell_replay_limits_match_the_pinned_gap_and_byte_boundaries() {
    let mut db = super::super::test_support::database();
    let tx = db.transaction().unwrap();
    for index in 0..1001 {
        event_store::append_metadata(
            &tx,
            "project",
            &index.to_string(),
            "project.deleted",
            NOW,
            &json!({"projectId":index,"deletedAt":NOW}),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert!(within_resume_budget(&db, 1, 1001).unwrap());
    assert!(!within_resume_budget(&db, 0, 1001).unwrap());
    // UTF-8 byte length, not character count, governs the bounded replay.
    let boundary = "あ".repeat((8 * 1024 * 1024 - 4) / 3) + "x";
    db.execute(
        "UPDATE orchestration_events SET payload_json=?1,metadata_json='{}' WHERE sequence=1001",
        [serde_json::to_string(&boundary).unwrap()],
    )
    .unwrap();
    assert!(within_resume_budget(&db, 1000, 1001).unwrap());
    db.execute(
        "UPDATE orchestration_events SET payload_json=?1 WHERE sequence=1001",
        [serde_json::to_string(&(boundary + "x")).unwrap()],
    )
    .unwrap();
    assert!(!within_resume_budget(&db, 1000, 1001).unwrap());
}
#[test]
fn project_and_script_changes_share_the_shell_cursor_and_unchanged_writes_do_not() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let initial = storage.shell_snapshot().unwrap();
    let project = agent_protocol::models::Project {
        id: "project".into(),
        name: "Project".into(),
        roots: vec![agent_protocol::models::ProjectRoot {
            path: "/workspace".into(),
        }],
        icon: None,
    };
    storage.register_project(&project).unwrap();
    let (registered, events) = storage.shell_changes(&initial).unwrap();
    assert!(
        matches!(&events[..],[ShellStreamItem::ProjectUpdated {project,..}] if project.id=="project")
    );
    storage.register_project(&project).unwrap();
    assert_eq!(storage.shell_changes(&registered).unwrap().1, []);
    storage
        .save_workspace_script(
            "/workspace",
            None,
            agent_protocol::models::ProjectScript {
                name: "check".into(),
                command: "just check".into(),
            },
        )
        .unwrap();
    let (saved, events) = storage.shell_changes(&registered).unwrap();
    assert!(
        matches!(&events[..],[ShellStreamItem::ProjectUpdated {project,..}] if project.scripts[0].command=="just check")
    );
    storage
        .save_workspace_script(
            "/workspace",
            Some("check"),
            agent_protocol::models::ProjectScript {
                name: "check".into(),
                command: "just check".into(),
            },
        )
        .unwrap();
    assert_eq!(storage.shell_changes(&saved).unwrap().1, []);
    storage
        .remove_workspace_script("/workspace", "check")
        .unwrap();
    let (removed, events) = storage.shell_changes(&saved).unwrap();
    assert!(
        matches!(&events[..],[ShellStreamItem::ProjectUpdated {project,..}] if project.scripts.is_empty())
    );
    storage.replace_projects(&[]).unwrap();
    assert!(
        matches!(&storage.shell_changes(&removed).unwrap().1[..],[ShellStreamItem::ProjectRemoved {project_id,..}] if project_id=="project")
    );
}
#[test]
fn read_receipts_acknowledge_only_the_observed_completion_and_replay_once() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    create(&storage, "thread");
    for (ordinal, id) in [(1, "old"), (2, "new")] {
        storage.observe_thread("thread",&[],NOW,|_| Ok(model::Decision {changes:vec![model::Change::Run(serde_json::from_value(json!({
            "id":id,"threadId":"thread","ordinal":ordinal,"providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"example"},
            "providerThreadId":null,"userMessageId":id,"rootNodeId":null,"activeAttemptId":null,"status":"completed",
            "queuePosition":null,"queueHeld":false,"requestedAt":NOW,"startedAt":NOW,"completedAt":NOW,"checkpointId":null,"contextHandoffId":null
        })).unwrap())],..Default::default()})).unwrap();
    }
    let id = SessionRef::new(None, "thread".into()).unwrap();
    let before = storage.shell_snapshot().unwrap();
    storage.mark_read(&id, &"old".into()).unwrap();
    assert_eq!(storage.shell_changes(&before).unwrap().1, []);
    assert!(storage.shell_snapshot().unwrap().threads[0].unread());
    storage.mark_read(&id, &"new".into()).unwrap();
    let read = storage.shell_snapshot().unwrap();
    assert!(!read.threads[0].unread());
    assert!(
        matches!(&storage.shell_changes(&before).unwrap().1[..],[ShellStreamItem::ThreadUpdated {thread,..}] if !thread.unread())
    );
    storage.mark_read(&id, &"new".into()).unwrap();
    assert_eq!(storage.shell_changes(&read).unwrap().1, []);
}
