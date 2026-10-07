use super::*;
use crate::sync::cache::{ThreadCacheEntry, cached_thread};
use crate::sync::fixtures::*;
use agent_domain::{ItemKind, ItemStatus};
use agent_protocol::conversation::ConversationError;
use proptest::prelude::*;

const CACHED: u64 = 7;

fn cached(title: &str, cursor: Option<&str>, has_more: bool, latest: Option<u64>) -> ThreadSync {
    ThreadSync::from_cache(CachedThread {
        snapshot_sequence: CACHED,
        state: Arc::new(thread_state(title)),
        history_cursor: cursor.map(Into::into),
        has_more_history: has_more,
        latest_local_ordinal: latest,
    })
}
fn opened(mut sync: ThreadSync) -> (ThreadSync, SubscribeThread) {
    sync.open();
    let request = sync.subscribe(&thread_id()).unwrap();
    (sync, request)
}
fn data_title(sync: &ThreadSync) -> &str {
    title(sync.state.as_ref().unwrap())
}
fn failure(error: ConversationError) -> RpcFailure {
    error.into()
}
fn unavailable(message: &str) -> RpcFailure {
    failure(ConversationError::Unavailable(message.into()))
}
fn item_ids(sync: &ThreadSync) -> Vec<String> {
    let state = sync.state.as_ref().unwrap();
    state.items.iter().map(|item| item.id.to_string()).collect()
}

#[test]
fn publishes_cached_data_immediately_from_a_warm_cache() {
    let sync = cached("Cached thread", None, false, None);
    assert_eq!(sync.status, ThreadStatus::Cached);
    assert_eq!(data_title(&sync), "Cached thread");
    assert_eq!(sync.error, None);
}

#[test]
fn resumes_a_warm_cache_via_after_sequence() {
    let (mut sync, request) = opened(cached("Cached thread", None, false, None));
    assert_eq!(request.after_sequence, Some(CACHED));
    assert!(request.accept_bounded_snapshot && request.request_completion_marker);
    sync.apply(vec![title_update("Live title", CACHED + 1)]);
    sync.apply(vec![ThreadUpdate::Synchronized]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(data_title(&sync), "Live title");
}

#[test]
fn reduces_live_events_and_persists_the_latest_thread() {
    let (mut sync, _) = opened(cached("Cached thread", None, false, None));
    let mut entry = ThreadCacheEntry::default();
    sync.apply(vec![
        snapshot(thread_state("Cached thread"), 1, None),
        title_update("Live title", 2),
        ThreadUpdate::Synchronized,
    ]);
    entry.changed(&sync, 0);
    assert_eq!(sync.status, ThreadStatus::Live);
    let saved = entry.due(500).unwrap();
    assert_eq!(title(&saved.state), "Live title");
    assert_eq!(saved.snapshot_sequence, 2);
}

#[test]
fn keeps_progressive_history_metadata_from_a_bounded_socket_snapshot() {
    let (mut sync, request) = opened(ThreadSync::default());
    assert_eq!(request.after_sequence, None);
    assert!(request.accept_bounded_snapshot);
    sync.apply(vec![
        snapshot(
            thread_state("Bounded"),
            14,
            Some(window(Some("socket-history-cursor"), true, Some(47))),
        ),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(
        sync.history,
        HistoryMeta {
            cursor: Some("socket-history-cursor".into()),
            has_more: true,
            loading: false,
            error: None,
            expanded: false,
            latest_local_ordinal: Some(47),
        }
    );
    let saved = cached_thread(&sync).unwrap();
    assert_eq!(
        (
            saved.snapshot_sequence,
            saved.history_cursor.as_deref(),
            saved.has_more_history,
            saved.latest_local_ordinal
        ),
        (14, Some("socket-history-cursor"), true, Some(47))
    );
    let LoadEarlier::Request(cursor) = sync.begin_load_earlier() else {
        panic!("history request")
    };
    assert_eq!(cursor, "socket-history-cursor");
    let older = command_item("item:older", 1);
    assert!(sync.history_loaded(
        &cursor,
        HistoryPage {
            rows: vec![HistoryRow {
                position: 0,
                source: thread_id(),
                inherited: false,
                item: older.clone(),
                message: None,
                plan: None,
            }],
            next_cursor: None,
            has_more: false,
        }
    ));
    assert!(item_ids(&sync).contains(&older.id.to_string()));
    assert!(!sync.history.has_more);
    assert!(sync.history.expanded);
}

#[test]
fn installs_bounded_history_and_resumes_via_after_sequence() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![
        snapshot(
            thread_state("Bounded title"),
            11,
            Some(window(Some("opaque-cursor"), true, None)),
        ),
        title_update("Live after bounded", 12),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(data_title(&sync), "Live after bounded");
    assert_eq!(sync.history.cursor.as_deref(), Some("opaque-cursor"));
    assert!(sync.history.has_more && !sync.history.expanded && !sync.history.loading);
    let request = sync.subscribe(&thread_id()).unwrap();
    assert_eq!(request.after_sequence, Some(12));
    assert!(request.accept_bounded_snapshot);
}

#[test]
fn persists_progressive_meta_with_a_settled_bounded_snapshot() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut entry = ThreadCacheEntry::default();
    sync.apply(vec![snapshot(
        thread_state("Bounded cache title"),
        4,
        Some(window(Some("cursor-oldest"), true, None)),
    )]);
    entry.changed(&sync, 0);
    let first = entry.due(500).unwrap();
    entry.written(first.clone(), true, 500);
    assert_eq!(first.snapshot_sequence, 4);
    assert_eq!(first.history_cursor.as_deref(), Some("cursor-oldest"));
    assert!(first.has_more_history);
    sync.apply(vec![title_update("Settled bounded", 5)]);
    entry.changed(&sync, 600);
    let saved = entry.due(10_500).unwrap();
    assert_eq!(saved.snapshot_sequence, 5);
    assert_eq!(title(&saved.state), "Settled bounded");
    assert_eq!(saved.history_cursor.as_deref(), Some("cursor-oldest"));
    assert!(saved.has_more_history);
}

#[test]
fn warm_resume_restores_progressive_history_meta() {
    let (sync, request) = opened(cached(
        "Cached bounded",
        Some("warm-cursor"),
        true,
        Some(33),
    ));
    assert_eq!(sync.history.cursor.as_deref(), Some("warm-cursor"));
    assert!(sync.history.has_more && !sync.history.expanded);
    assert_eq!(sync.history.latest_local_ordinal, Some(33));
    assert_eq!(request.after_sequence, Some(CACHED));
    assert!(request.accept_bounded_snapshot);
}

#[test]
fn a_complete_cache_offers_no_load_earlier() {
    let mut sync = cached("Thread", None, false, None);
    assert!(!sync.history.shows_load_earlier());
    assert_eq!(sync.begin_load_earlier(), LoadEarlier::Noop);
}

#[test]
fn a_full_snapshot_clears_progressive_history_meta() {
    let (mut sync, _) = opened(cached("Warm progressive", Some("stale-cursor"), true, None));
    sync.apply(vec![
        snapshot(thread_state("Full socket snapshot"), CACHED + 1, None),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(sync.history, HistoryMeta::default());
    let saved = cached_thread(&sync).unwrap();
    assert_eq!(title(&saved.state), "Full socket snapshot");
    assert_eq!(saved.history_cursor, None);
    assert!(!saved.has_more_history);
}

#[test]
fn a_bounded_snapshot_replaces_progressive_meta_during_resume() {
    let (mut sync, request) = opened(cached(
        "Warm progressive",
        Some("stale-cursor"),
        true,
        Some(3),
    ));
    assert_eq!(request.after_sequence, Some(CACHED));
    sync.apply(vec![
        snapshot(
            thread_state("Bounded resume fallback"),
            CACHED + 1,
            Some(window(Some("replacement-cursor"), true, Some(9))),
        ),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(
        sync.history,
        HistoryMeta {
            cursor: Some("replacement-cursor".into()),
            has_more: true,
            latest_local_ordinal: Some(9),
            ..HistoryMeta::default()
        }
    );
}

#[test]
fn live_events_preserve_progressive_history_meta() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![
        snapshot(
            thread_state("Bounded seed"),
            8,
            Some(window(Some("keep-me"), true, None)),
        ),
        title_update("Live preserves meta", 9),
    ]);
    assert_eq!(data_title(&sync), "Live preserves meta");
    assert_eq!(sync.history.cursor.as_deref(), Some("keep-me"));
    assert!(sync.history.has_more && !sync.history.expanded);
}

#[test]
fn a_dropped_partial_window_item_is_a_true_noop() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut state = thread_state("Partial noop");
    let recent = command_item("item-window", 10);
    state.items.push(recent.clone());
    sync.apply(vec![
        snapshot(
            state,
            5,
            Some(window(Some("partial-cursor"), true, Some(10))),
        ),
        ThreadUpdate::Synchronized,
    ]);
    let mut entry = ThreadCacheEntry::default();
    entry.changed(&sync, 0);
    let saved = entry.due(500).unwrap();
    entry.written(saved, true, 500);
    let revision = sync.revision;
    let mut older = command_item("item-old-outside", 3);
    older.text = "must-not-append".into();
    let applied = sync.apply(vec![facts(vec![(6, projected(older))])]);
    // Only applied changes reach the cache writer.
    assert!(!applied.changed);
    assert_eq!(sync.revision, revision);
    assert_eq!(entry.next_due(), None);
    sync.apply(vec![title_update("After dropped event", 7)]);
    assert_eq!(item_ids(&sync), [recent.id.to_string()]);
    assert_eq!(sync.history.latest_local_ordinal, Some(10));
    assert_eq!(sync.history.cursor.as_deref(), Some("partial-cursor"));
    assert!(sync.history.has_more);
}

#[test]
fn installs_and_advances_the_latest_local_ordinal_for_partial_windows() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![snapshot(
        thread_state("Watermark seed"),
        3,
        Some(window(Some("wm-cursor"), true, Some(15))),
    )]);
    assert_eq!(sync.history.latest_local_ordinal, Some(15));
    assert_eq!(cached_thread(&sync).unwrap().latest_local_ordinal, Some(15));
    let newer = command_item("item-newer-live", 22);
    sync.apply(vec![
        facts(vec![(4, projected(newer.clone()))]),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(sync.history.latest_local_ordinal, Some(22));
    assert!(item_ids(&sync).contains(&newer.id.to_string()));
    assert_eq!(sync.history.cursor.as_deref(), Some("wm-cursor"));
}

#[test]
fn a_partial_window_skips_changes_to_items_it_does_not_hold() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![snapshot(
        thread_state("Partial"),
        3,
        Some(window(Some("cursor"), true, Some(15))),
    )]);
    let outside = fact(agent_domain::FactBody::ItemCompleted {
        id: agent_domain::TurnItemId::new("item-outside").unwrap(),
        status: ItemStatus::Completed,
    });
    let applied = sync.apply(vec![facts(vec![(4, outside)])]);
    assert_eq!(applied.resync, Resync::None);
    assert_eq!(sync.cursor, 4);
}

#[test]
fn warm_resume_restores_the_latest_local_ordinal() {
    let sync = cached("Warm watermark", Some("warm-wm-cursor"), true, Some(33));
    assert_eq!(
        sync.history,
        HistoryMeta {
            cursor: Some("warm-wm-cursor".into()),
            has_more: true,
            latest_local_ordinal: Some(33),
            ..HistoryMeta::default()
        }
    );
}

#[test]
fn marks_a_cold_definitive_miss_deleted_without_retrying() {
    let (mut sync, _) = opened(ThreadSync::default());
    let action = sync.failed(&failure(ConversationError::ThreadNotFound(thread_id())));
    assert_eq!(action, FailureAction::Deleted);
    assert_eq!(sync.status, ThreadStatus::Deleted);
    assert!(sync.state.is_none() && sync.error.is_none());
    // Neither a replacement session nor a foreground wakeup subscribes again.
    assert_eq!(sync.subscribe(&thread_id()), None);
    let mut retained = sync.resumed();
    retained.open();
    assert_eq!(retained.subscribe(&thread_id()), None);
    assert_eq!(retained.status, ThreadStatus::Deleted);
}

#[test]
fn a_warm_not_found_failure_keeps_data_and_retries() {
    let (mut sync, _) = opened(cached("Cached thread", None, false, None));
    let action = sync.failed(&failure(ConversationError::ThreadNotFound(thread_id())));
    assert_eq!(action, FailureAction::Retry);
    assert_eq!(data_title(&sync), "Cached thread");
}

#[test]
fn ignores_replayed_events_at_or_below_the_snapshot_sequence() {
    let (mut sync, _) = opened(cached("Cached thread", None, false, None));
    sync.apply(vec![
        snapshot(thread_state("Cached thread"), 1, None),
        title_update("Replayed title", 1),
        title_update("Live title", 2),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(data_title(&sync), "Live title");
    assert_eq!(sync.status, ThreadStatus::Live);
}

#[test]
fn a_deletion_clears_the_data_and_its_pending_cache_write() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut entry = ThreadCacheEntry::default();
    sync.apply(vec![snapshot(thread_state("Thread"), 1, None)]);
    entry.changed(&sync, 0);
    let saved = entry.due(500).unwrap();
    entry.written(saved, true, 500);
    sync.apply(vec![title_update("Queued before deletion", 2)]);
    entry.changed(&sync, 600);
    let applied = sync.apply(vec![facts(vec![(
        3,
        fact(agent_domain::FactBody::ThreadDeleted),
    )])]);
    assert!(applied.deleted);
    assert_eq!(sync.status, ThreadStatus::Deleted);
    assert!(sync.state.is_none());
    entry.deleted();
    assert_eq!(entry.due(20_000), None);
    assert_eq!(entry.teardown(&sync), None);
}

#[test]
fn preserves_data_after_a_domain_failure_and_resumes_on_a_replacement_session() {
    let (mut sync, _) = opened(cached("Cached thread", None, false, None));
    sync.apply(vec![snapshot(thread_state("Cached thread"), 1, None)]);
    let applied = sync.apply(vec![ThreadUpdate::Failed(unavailable("stream failed"))]);
    assert!(!applied.deleted);
    assert_eq!(sync.error.as_deref(), Some("stream failed"));
    assert_eq!(data_title(&sync), "Cached thread");
    sync.subscribe(&thread_id()).unwrap();
    sync.apply(vec![
        snapshot(thread_state("Recovered thread"), 2, None),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(sync.error, None);
    assert_eq!(data_title(&sync), "Recovered thread");
}

#[test]
fn recovers_from_a_transient_failure_on_the_next_subscription() {
    let (mut sync, _) = opened(ThreadSync::default());
    assert_eq!(
        sync.failed(&unavailable("thread not found yet")),
        FailureAction::Retry
    );
    assert_eq!(sync.error.as_deref(), Some("thread not found yet"));
    let request = sync.subscribe(&thread_id()).unwrap();
    assert_eq!(request.after_sequence, None);
    sync.apply(vec![
        snapshot(thread_state("Materialized thread"), 1, None),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    assert_eq!(sync.error, None);
}

#[test]
fn a_ready_connection_does_not_downgrade_a_live_thread() {
    let (mut sync, _) = opened(cached("Thread", None, false, None));
    sync.connecting();
    sync.apply(vec![
        snapshot(thread_state("Thread"), 8, None),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    sync.ready();
    assert_eq!(sync.status, ThreadStatus::Live);
}

#[test]
fn keeps_replayed_updates_synchronizing_until_the_completion_marker() {
    let (mut sync, request) = opened(cached("Thread", None, false, None));
    assert!(request.request_completion_marker);
    assert_eq!(sync.status, ThreadStatus::Synchronizing);
    sync.apply(vec![title_update("Caught-up title", CACHED + 1)]);
    assert_eq!(sync.status, ThreadStatus::Synchronizing);
    assert_eq!(data_title(&sync), "Caught-up title");
    sync.apply(vec![ThreadUpdate::Synchronized]);
    assert_eq!(sync.status, ThreadStatus::Live);
}

#[test]
fn resubscribes_from_the_latest_applied_sequence() {
    let (mut sync, _) = opened(cached("Thread", None, false, None));
    sync.apply(vec![
        title_update("Latest title", CACHED + 1),
        ThreadUpdate::Synchronized,
    ]);
    assert_eq!(sync.status, ThreadStatus::Live);
    // A replacement session and a foreground wakeup resume the same way.
    for _ in 0..2 {
        let request = sync.subscribe(&thread_id()).unwrap();
        assert_eq!(request.after_sequence, Some(CACHED + 1));
        assert!(request.request_completion_marker);
        assert_eq!(sync.status, ThreadStatus::Synchronizing);
        sync.apply(vec![ThreadUpdate::Synchronized]);
        assert_eq!(sync.status, ThreadStatus::Live);
        assert_eq!(data_title(&sync), "Latest title");
    }
}

#[test]
fn a_retained_live_thread_stays_live_on_its_first_resume_only() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![
        snapshot(thread_state("Thread"), 3, None),
        ThreadUpdate::Synchronized,
    ]);
    let mut retained = sync.resumed();
    assert_eq!(retained.status, ThreadStatus::Live);
    retained.open();
    retained.subscribe(&thread_id()).unwrap();
    assert_eq!(retained.status, ThreadStatus::Live);
    retained.subscribe(&thread_id()).unwrap();
    assert_eq!(retained.status, ThreadStatus::Synchronizing);
}

#[test]
fn a_fold_failure_requests_one_snapshot_and_then_stops() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![snapshot(thread_state("Thread"), 7, None)]);
    let missing = || {
        fact(agent_domain::FactBody::ItemCompleted {
            id: agent_domain::TurnItemId::new("missing").unwrap(),
            status: ItemStatus::Completed,
        })
    };
    let applied = sync.apply(vec![facts(vec![(8, missing())])]);
    assert_eq!(applied.resync, Resync::Snapshot);
    assert_eq!(sync.subscribe(&thread_id()).unwrap().after_sequence, None);
    sync.apply(vec![snapshot(thread_state("Thread"), 8, None)]);
    let applied = sync.apply(vec![facts(vec![(9, missing())])]);
    assert_eq!(applied.resync, Resync::Stop);
    assert_eq!(sync.error.as_deref(), Some(THREAD_SYNC_ERROR));
    // The next session still starts from a snapshot.
    assert_eq!(sync.subscribe(&thread_id()).unwrap().after_sequence, None);
}

#[test]
fn history_results_apply_only_to_the_cursor_that_requested_them() {
    let (mut sync, _) = opened(ThreadSync::default());
    sync.apply(vec![snapshot(
        thread_state("Thread"),
        2,
        Some(window(Some("a"), true, None)),
    )]);
    assert_eq!(sync.begin_load_earlier(), LoadEarlier::Request("a".into()));
    assert_eq!(sync.begin_load_earlier(), LoadEarlier::Busy);
    sync.apply(vec![snapshot(
        thread_state("Thread"),
        3,
        Some(window(Some("b"), true, None)),
    )]);
    let page = HistoryPage {
        rows: vec![],
        next_cursor: None,
        has_more: false,
    };
    assert!(!sync.history_loaded("a", page.clone()));
    assert!(!sync.history_failed("a", "late failure"));
    assert_eq!(sync.history.cursor.as_deref(), Some("b"));
    assert_eq!(sync.history.error, None);
    assert_eq!(sync.begin_load_earlier(), LoadEarlier::Request("b".into()));
    assert!(sync.history_failed("b", ""));
    assert_eq!(sync.history.error.as_deref(), Some(HISTORY_ERROR));
    assert!(!sync.history.loading);
}

#[test]
fn a_fact_touching_an_item_invalidates_its_loaded_detail() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut state = thread_state("Thread");
    let mut item = command_item("command", 1);
    item.status = ItemStatus::Running;
    item.output_omitted = true;
    state.items.push(item.clone());
    sync.apply(vec![snapshot(state, 2, None)]);
    assert!(sync.begin_detail(&item.id));
    assert!(!sync.begin_detail(&item.id));
    let mut done = item.clone();
    done.status = ItemStatus::Completed;
    sync.apply(vec![facts(vec![(3, projected(done.clone()))])]);
    assert_eq!(sync.details.get(&item.id), None);
    let row = HistoryRow {
        position: 0,
        source: thread_id(),
        inherited: false,
        item: done.clone(),
        message: None,
        plan: None,
    };
    sync.detail_loaded(&item.id, Some(row.clone()));
    assert_eq!(sync.details.get(&item.id), None);
    assert!(sync.begin_detail(&item.id));
    sync.detail_loaded(&item.id, Some(row));
    assert_eq!(
        sync.details.get(&item.id),
        Some(&Detail::Loaded(Box::new(done)))
    );
}

#[test]
fn reports_folded_rollback_results() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut state = thread_state("Thread");
    let command = CommandId::new("rollback").unwrap();
    state.rollbacks.push(agent_domain::PendingRollback {
        command: command.clone(),
        checkpoint: agent_domain::CheckpointId::new("checkpoint").unwrap(),
        restore_files: false,
        rewinding: Default::default(),
    });
    sync.apply(vec![snapshot(state, 2, None)]);
    let applied = sync.apply(vec![facts(vec![(
        3,
        fact(agent_domain::FactBody::RollbackFailed {
            command: command.clone(),
            message: "failed".into(),
        }),
    )])]);
    assert_eq!(
        applied.rollbacks,
        [RollbackResult {
            command,
            succeeded: false
        }]
    );
}

fn streamed_items(count: u64) -> Vec<(u64, Fact)> {
    (1..=count)
        .map(|index| {
            let mut item = command_item(&format!("item-{index}"), index);
            item.kind = ItemKind::Reasoning;
            (index + 1, projected(item))
        })
        .collect()
}

proptest! {
    #[test]
    fn the_cursor_never_moves_back_under_duplicate_and_out_of_order_replays(
        order in prop::collection::vec(0usize..12, 1..60),
    ) {
        let all = streamed_items(12);
        let (mut sync, _) = opened(ThreadSync::default());
        sync.apply(vec![snapshot(thread_state("Thread"), 1, None)]);
        let mut highest = 1;
        for index in order {
            let (sequence, fact) = all[index].clone();
            let before = sync.cursor;
            sync.apply(vec![facts(vec![(sequence, fact)])]);
            prop_assert!(sync.cursor >= before);
            highest = highest.max(sequence);
            prop_assert_eq!(sync.cursor, highest);
        }
        let state = sync.state.as_ref().unwrap();
        let mut ids: Vec<_> = state.items.iter().map(|item| item.id.clone()).collect();
        ids.dedup();
        prop_assert_eq!(ids.len(), state.items.len());
    }

    #[test]
    fn folding_facts_in_split_batches_equals_folding_them_whole(
        cuts in prop::collection::vec(1usize..20, 0..6),
    ) {
        let all = streamed_items(20);
        let (mut whole, _) = opened(ThreadSync::default());
        whole.apply(vec![snapshot(thread_state("Thread"), 1, None)]);
        whole.apply(vec![facts(all.clone())]);
        let (mut split, _) = opened(ThreadSync::default());
        split.apply(vec![snapshot(thread_state("Thread"), 1, None)]);
        let mut bounds: Vec<_> = cuts.into_iter().chain([0, all.len()]).collect();
        bounds.sort_unstable();
        bounds.dedup();
        let updates = bounds
            .windows(2)
            .map(|range| facts(all[range[0]..range[1]].to_vec()))
            .collect();
        split.apply(updates);
        prop_assert_eq!(&whole.state, &split.state);
        prop_assert_eq!(whole.cursor, split.cursor);
    }
}

fn row_key(sync: &ThreadSync) -> (u64, u64, u64) {
    (sync.cursor, sync.history_revision, sync.detail_revision)
}

#[test]
fn the_row_key_changes_with_facts_history_pages_and_details_only() {
    let (mut sync, _) = opened(ThreadSync::default());
    let mut state = thread_state("Thread");
    let mut item = command_item("command", 2);
    item.output_omitted = true;
    state.items.push(item.clone());
    sync.apply(vec![snapshot(
        state,
        2,
        Some(window(Some("cursor"), true, Some(2))),
    )]);
    let installed = row_key(&sync);
    sync.stream_error("offline");
    assert_eq!(row_key(&sync), installed);
    sync.apply(vec![title_update("Renamed", 3)]);
    assert_eq!(row_key(&sync), (3, installed.1, installed.2));
    assert!(sync.begin_detail(&item.id));
    let loading = row_key(&sync);
    assert_eq!((loading.0, loading.1), (3, installed.1));
    assert_ne!(loading.2, installed.2);
    let LoadEarlier::Request(cursor) = sync.begin_load_earlier() else {
        panic!("history request")
    };
    assert_eq!(row_key(&sync), loading);
    assert!(sync.history_loaded(
        &cursor,
        HistoryPage {
            rows: vec![HistoryRow {
                position: 0,
                source: thread_id(),
                inherited: false,
                item: command_item("older", 1),
                message: None,
                plan: None,
            }],
            next_cursor: None,
            has_more: false,
        }
    ));
    let merged = row_key(&sync);
    assert_eq!((merged.0, merged.2), (3, loading.2));
    assert_ne!(merged.1, loading.1);
}
