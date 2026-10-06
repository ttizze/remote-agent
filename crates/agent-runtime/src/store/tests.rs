use super::*;
use crate::{CommitBatch, SearchChanges};
use agent_domain::{
    Driver, InteractionMode, ModelSelection, RuntimeMode, State, ThreadId, Timestamp, fold,
};
use std::collections::BTreeMap;
use std::sync::mpsc as sync_mpsc;

pub(crate) fn temp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("runtime.sqlite")).unwrap();
    (dir, store)
}
pub(crate) fn at() -> Timestamp {
    Timestamp::parse("2026-10-06T00:00:00Z").unwrap()
}
pub(crate) fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}
pub(crate) fn selection() -> ModelSelection {
    ModelSelection {
        instance: "codex".into(),
        driver: Driver::Codex,
        model: "gpt-6-luna".into(),
        options: BTreeMap::new(),
    }
}
fn created(id: &ThreadId) -> Fact {
    Fact {
        at: at(),
        body: FactBody::ThreadCreated {
            id: id.clone(),
            project: "project".into(),
            title: format!("Thread {id}"),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    }
}
/// The list summary of a freshly created thread with this title.
pub(crate) fn thread_shell(id: &str, title: &str) -> agent_domain::ThreadShell {
    let mut state = State::default();
    agent_domain::apply(&mut state, &created(&thread(id))).unwrap();
    let mut shell = agent_domain::shell(&state).unwrap();
    shell.title = title.into();
    shell
}
fn renamed(title: String) -> Fact {
    Fact {
        at: at(),
        body: FactBody::ThreadRenamed { title },
    }
}
fn batch(thread: &ThreadId, base: u64, input_seq: u64, facts: Vec<Fact>) -> CommitBatch {
    CommitBatch {
        thread: thread.clone(),
        at: at(),
        base_thread_seq: base,
        input_seq,
        receipt: None,
        facts,
        effects: vec![],
        settle: None,
        shell: None,
        needs_recovery: false,
        search: SearchChanges::default(),
        snapshot: None,
    }
}
fn history(id: &ThreadId, count: usize, title: &str) -> Vec<Fact> {
    std::iter::once(created(id))
        .chain((1..count).map(|index| renamed(format!("{title} {index}"))))
        .collect()
}
fn count(store: &Store, table: &str) -> i64 {
    store
        .read(|c| {
            Ok(
                c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap()
}

#[tokio::test]
async fn a_failed_write_leaves_no_partial_step() {
    let (_dir, store) = temp_store();
    let id = thread("thread:atomic");
    store
        .on_writer(|c| {
            c.execute_batch(
                "CREATE TEMP TRIGGER fail_receipts BEFORE INSERT ON receipts
                 BEGIN SELECT RAISE(ABORT, 'injected crash'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut failing = batch(&id, 0, 1, history(&id, 3, "Partial"));
    failing.receipt = Some(agent_domain::Receipt {
        command: CommandId::new("command:atomic").unwrap(),
        fingerprint: "fingerprint".into(),
        reply: Reply::Accepted,
    });
    failing.snapshot = Some(b"{}".to_vec());
    assert!(store.commit(failing.clone()).await.is_err());
    for table in ["facts", "threads", "receipts", "thread_snapshots"] {
        assert_eq!(count(&store, table), 0, "{table}");
    }
    assert_eq!(store.latest_global_seq().unwrap(), 0);
    store
        .on_writer(|c| Ok(c.execute_batch("DROP TRIGGER fail_receipts")?))
        .await
        .unwrap();
    failing.snapshot = None;
    let outcome = store.commit(failing).await.unwrap();
    assert_eq!(
        outcome.head,
        ThreadHead {
            thread_seq: 3,
            input_seq: 1,
            global_seq: 3
        }
    );
}

#[tokio::test]
async fn uncommitted_writes_from_a_lost_process_are_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.sqlite");
    let id = thread("thread:lost");
    {
        let store = Store::open(&path).unwrap();
        store
            .commit(batch(&id, 0, 1, history(&id, 1, "")))
            .await
            .unwrap();
    }
    {
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "INSERT INTO facts (thread_id, thread_seq, input_seq, kind, at, payload)
             VALUES (?1, 2, 2, 'ThreadRenamed', ?2, '{\"ThreadRenamed\":{\"title\":\"lost\"}}')",
            params![id.as_str(), at().as_str()],
        )
        .unwrap();
        tx.execute(
            "UPDATE threads SET thread_seq = 2 WHERE thread_id = ?1",
            [id.as_str()],
        )
        .unwrap();
        std::mem::forget(tx);
    }
    let store = Store::open(&path).unwrap();
    let loaded = store.load_thread(&id).unwrap();
    assert_eq!(loaded.head.thread_seq, 1);
    assert_eq!(loaded.state.thread.unwrap().title, "Thread thread:lost");
}

#[tokio::test]
async fn rejects_a_stale_base_sequence() {
    let (_dir, store) = temp_store();
    let id = thread("thread:stale");
    store
        .commit(batch(&id, 0, 1, history(&id, 2, "t")))
        .await
        .unwrap();
    let error = store
        .commit(batch(&id, 1, 2, vec![renamed("late".into())]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Stale { stored: 2, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_command_id_belongs_to_one_thread() {
    let (_dir, store) = temp_store();
    let receipt = agent_domain::Receipt {
        command: CommandId::new("command:shared").unwrap(),
        fingerprint: "f".into(),
        reply: Reply::Accepted,
    };
    let first = thread("thread:first");
    let second = thread("thread:second");
    let mut owner = batch(&first, 0, 1, history(&first, 1, ""));
    owner.receipt = Some(receipt.clone());
    store.commit(owner).await.unwrap();
    let mut loser = batch(&second, 0, 1, history(&second, 1, ""));
    loser.receipt = Some(receipt);
    let error = store.commit(loser).await.unwrap_err();
    assert!(matches!(error, StoreError::CommandConflict(ref t) if *t == first));
    assert_eq!(store.thread_head(&second).unwrap(), ThreadHead::default());
}

#[tokio::test]
async fn paginates_catch_up_beyond_the_read_limit() {
    let (_dir, store) = temp_store();
    let id = thread("thread:foundation-large-catch-up");
    let event_count = 1_005;
    store
        .commit(batch(
            &id,
            0,
            1,
            history(&id, event_count, "Catch-up update"),
        ))
        .await
        .unwrap();
    let replayed = store.facts_after(None, 0).unwrap();
    assert_eq!(replayed.len(), event_count);
    assert_eq!(
        replayed.iter().map(|f| f.global_seq).collect::<Vec<_>>(),
        (1..=event_count as u64).collect::<Vec<_>>()
    );
}

// octet_length over the replay limit plus 1.
#[tokio::test]
async fn measures_a_replay_gap_in_utf8_bytes_within_the_replay_limit() {
    let (_dir, store) = temp_store();
    let id = thread("thread:replay-gap");
    let wide = "é".repeat(4_000);
    let facts: Vec<Fact> = std::iter::once(created(&id))
        .chain((0..9).map(|_| renamed(wide.clone())))
        .collect();
    let stored_bytes = |facts: &[Fact]| -> u64 {
        facts
            .iter()
            .map(|fact| serde_json::to_value(&fact.body).unwrap().to_string().len() as u64)
            .sum()
    };
    store.commit(batch(&id, 0, 1, facts.clone())).await.unwrap();

    let all = store.fact_gap(&id, 0, 100).unwrap();
    assert_eq!(all.facts, 10);
    assert_eq!(all.bytes, stored_bytes(&facts));
    assert!(all.bytes > 9 * 8_000);
    assert!(all.contains_created);

    let bounded = store.fact_gap(&id, 0, 3).unwrap();
    assert_eq!(bounded.facts, 4);
    assert_eq!(bounded.bytes, stored_bytes(&facts[..4]));
    let after_creation = store.fact_gap(&id, 1, 3).unwrap();
    assert_eq!(after_creation.facts, 4);
    assert!(!after_creation.contains_created);
}

#[tokio::test]
async fn rebuilds_event_history_one_bounded_page_at_a_time() {
    let (_dir, store) = temp_store();
    let id = thread("thread:foundation-paged-rebuild");
    let event_count = 1_005;
    store
        .commit(batch(
            &id,
            0,
            1,
            history(&id, event_count, "Rebuilt update"),
        ))
        .await
        .unwrap();
    let applied = std::cell::Cell::new(0usize);
    let mut applied_at_read = vec![];
    let (rebuilt, folded) = fold_pages(State::default(), 0, FACT_PAGE, |after, limit| {
        applied_at_read.push(applied.get());
        let page = load::thread_facts_for_test(&store, &id, after, limit)?;
        applied.set(applied.get() + page.len());
        Ok(page)
    })
    .unwrap();
    assert_eq!(applied.get(), event_count);
    assert_eq!(folded, event_count as u64);
    assert_eq!(applied_at_read, [0, 500, 1_000]);
    assert_eq!(rebuilt.thread.unwrap().title, "Rebuilt update 1004");

    applied.set(0);
    let failed = fold_pages(State::default(), 0, FACT_PAGE, |after, limit| {
        if applied.get() >= 500 {
            return Err(StoreError::Corrupt("read failed".into()));
        }
        let page = load::thread_facts_for_test(&store, &id, after, limit)?;
        applied.set(applied.get() + page.len());
        Ok(page)
    });
    assert!(failed.is_err());
    assert_eq!(applied.get(), 500);
    assert_eq!(
        store.load_thread(&id).unwrap().state.thread.unwrap().title,
        "Rebuilt update 1004"
    );
}

#[tokio::test]
async fn snapshot_plus_tail_equals_the_full_fold_and_survives_format_changes() {
    let (_dir, store) = temp_store();
    let id = thread("thread:snapshot");
    let facts = history(&id, 300, "Snapshot");
    let at_256 = fold(&State::default(), &facts[..256]).unwrap();
    let mut head = batch(&id, 0, 1, facts[..256].to_vec());
    head.snapshot = Some(serde_json::to_vec(&at_256).unwrap());
    store.commit(head).await.unwrap();
    store
        .commit(batch(&id, 256, 2, facts[256..].to_vec()))
        .await
        .unwrap();
    let full = fold(&State::default(), &facts).unwrap();
    let loaded = store.load_thread(&id).unwrap();
    assert!(!loaded.snapshot_stale);
    assert_eq!(loaded.state, full);
    assert_eq!(loaded.head.thread_seq, 300);
    assert_eq!(loaded.last_at, Some(at()));

    store
        .write(|tx| Ok(tx.execute("UPDATE thread_snapshots SET format = 'retired'", [])?))
        .await
        .unwrap();
    let rebuilt = store.load_thread(&id).unwrap();
    assert!(rebuilt.snapshot_stale);
    assert_eq!(rebuilt.state, full);

    store
        .write(|tx| {
            Ok(tx.execute(
                "UPDATE thread_snapshots SET format = ?1, blob = x'00'",
                [SNAPSHOT_FORMAT],
            )?)
        })
        .await
        .unwrap();
    let unreadable = store.load_thread(&id).unwrap();
    assert!(unreadable.snapshot_stale);
    assert_eq!(unreadable.state, full);
}

struct PausingListener {
    first_committed: std::sync::Mutex<Option<sync_mpsc::Sender<()>>>,
    release: std::sync::Mutex<sync_mpsc::Receiver<()>>,
    published: std::sync::Mutex<Vec<u64>>,
}
impl CommitListener for PausingListener {
    fn committed(&self, notice: &CommitNotice) {
        if let Some(first) = self.first_committed.lock().unwrap().take() {
            first.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
        self.published
            .lock()
            .unwrap()
            .extend(notice.facts.iter().map(|f| f.global_seq));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_live_events_in_commit_order_across_concurrent_writers() {
    let (_dir, store) = temp_store();
    let (first_tx, first_committed) = sync_mpsc::channel();
    let (release_tx, release) = sync_mpsc::channel();
    let listener = Arc::new(PausingListener {
        first_committed: std::sync::Mutex::new(Some(first_tx)),
        release: std::sync::Mutex::new(release),
        published: std::sync::Mutex::new(vec![]),
    });
    store.add_listener(listener.clone());
    let first = thread("thread:foundation-publish-order:first");
    let second = thread("thread:foundation-publish-order:second");
    let first_write = tokio::spawn({
        let store = store.clone();
        let mut batch = batch(&first, 0, 1, history(&first, 1, ""));
        batch.effects = vec![agent_domain::Effect {
            id: "effect:foundation-publish-order:first".into(),
            attempt: None,
            body: agent_domain::EffectBody::DeleteAttachments { paths: vec![] },
        }];
        async move { store.commit(batch).await }
    });
    tokio::task::spawn_blocking(move || first_committed.recv().unwrap())
        .await
        .unwrap();
    let second_write = tokio::spawn({
        let store = store.clone();
        let batch = batch(&second, 0, 1, history(&second, 1, ""));
        async move { store.commit(batch).await }
    });
    tokio::task::yield_now().await;
    release_tx.send(()).unwrap();
    first_write.await.unwrap().unwrap();
    second_write.await.unwrap().unwrap();
    let published = listener.published.lock().unwrap().clone();
    assert_eq!(published.len(), 2);
    let mut sorted = published.clone();
    sorted.sort();
    assert_eq!(published, sorted);
}

#[tokio::test]
async fn refuses_an_unknown_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .write(|tx| {
            Ok(tx.execute(
                "UPDATE runtime_meta SET value = '0' WHERE key = 'schema_version'",
                [],
            )?)
        })
        .await
        .unwrap();
    drop(store);
    assert!(matches!(Store::open(&path), Err(StoreError::Schema(v)) if v == "0"));
}
