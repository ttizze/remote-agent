use super::test_support::{NOW, thread};
use super::*;
use agent_protocol::orchestration as model;

#[test]
fn unsupported_destinations_are_rejected_without_rewriting_their_bytes() {
    for version in [0, 2, 3, 4, 5, 6] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE future(data TEXT); INSERT INTO future VALUES ('preserve');",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        let storage = Storage::open(&path);
        assert!(storage.check_ready().is_err());
        drop(storage);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn storage_uses_durable_wal_and_reports_an_invalid_path_to_every_consumer() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    storage
        .with(|connection| {
            let mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            let synchronous: i64 = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            assert_eq!(synchronous, 2);
            Ok(())
        })
        .unwrap();
    let invalid = Storage::open(directory.path());
    assert!(invalid.check_ready().is_err());
    assert!(invalid.shell_snapshot().is_err());
    assert!(invalid.thread_state("invalid", &[]).is_err());
    assert!(invalid.projects().is_err());
}

#[test]
fn reopening_preserves_canonical_receipts_and_never_creates_legacy_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let command = || commands::Command {
        id: "create",
        thread_id: "thread",
        kind: "thread.create",
        now: NOW,
        item_ids: &[],
    };
    let receipt = storage
        .thread_command(command(), Some(&thread("thread")), |_| {
            Ok(model::Decision::default())
        })
        .unwrap();
    let before = storage.shell_snapshot().unwrap();
    drop(storage);
    let storage = Storage::open(&path);
    assert_eq!(storage.shell_snapshot().unwrap(), before);
    assert_eq!(
        storage
            .thread_command(command(), None, |_| panic!(
                "a saved command must not execute again"
            ))
            .unwrap()
            .receipt,
        receipt.receipt
    );
    storage
        .with(|db| {
            let mut query = db
                .prepare("SELECT name FROM sqlite_schema WHERE type='table'")
                .unwrap();
            let names = query
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            for name in [
                "threads",
                "items",
                "inputs",
                "turns",
                "requests",
                "bindings",
                "attempts",
                "jobs",
                "events",
                "rows",
                "catalogs",
                "provider_catalogs",
            ] {
                assert!(
                    !names.iter().any(|saved| saved == name),
                    "obsolete table: {name}"
                );
            }
            assert!(names.iter().any(|name| name == "orchestration_events"));
            Ok(())
        })
        .unwrap();
}

#[test]
fn only_successful_committed_writes_wake_streams() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let mut changed = storage.changes();
    storage.shell_snapshot().unwrap();
    assert!(!changed.has_changed().unwrap());
    storage
        .thread_command(
            commands::Command {
                id: "create",
                thread_id: "thread",
                kind: "thread.create",
                now: NOW,
                item_ids: &[],
            },
            Some(&thread("thread")),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
    assert!(changed.has_changed().unwrap());
    changed.borrow_and_update();
    let before = storage.shell_snapshot().unwrap();
    let failed = storage.observe_thread("thread", &[], NOW, |state| {
        Ok(model::Decision {
            changes: vec![model::Change::Thread(model::Thread {
                title: "must roll back".into(),
                active_provider_thread_id: Some("missing".into()),
                ..state.thread.clone()
            })],
            effects: vec![],
        })
    });
    assert!(failed.is_err());
    assert!(!changed.has_changed().unwrap());
    assert_eq!(storage.shell_snapshot().unwrap(), before);
}
