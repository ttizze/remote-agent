use super::*;

fn restore(thread: &str) -> UndoRestore {
    UndoRestore::Thread {
        thread: ThreadId::new(thread).unwrap(),
        actions: vec![LifecycleAction::Pin { order: None }],
        reopen: false,
    }
}

fn show(undo: &mut ThreadUndo, action: ThreadUndoAction, kind: &str, key: &str, now: i64) {
    let claim = undo.begin(kind, key);
    undo.show(action, claim, restore(key), now);
}

#[test]
fn keeps_claims_for_different_action_kinds_independent() {
    let mut undo = ThreadUndo::default();
    let pin = undo.begin("pin", "shared");
    let archive = undo.begin("archive", "shared");
    undo.invalidate("pin", "shared");
    assert!(!undo.is_current(&pin));
    assert!(undo.is_current(&archive));
}

#[test]
fn does_not_revive_the_first_undo_after_a_later_pin_and_unpin() {
    let mut undo = ThreadUndo::default();
    let first = undo.begin("pin", "thread");
    undo.invalidate("pin", "thread");
    let second = undo.begin("pin", "thread");
    assert!(!undo.is_current(&first));
    assert!(undo.is_current(&second));
    undo.finish(&first);
    assert!(undo.is_current(&second));
    undo.finish(&second);
    assert!(!undo.is_current(&first));
}

#[test]
fn expires_an_undo_without_invalidating_another_thread() {
    let mut undo = ThreadUndo::default();
    let first = undo.begin("pin", "thread");
    let other = undo.begin("pin", "other");
    undo.finish(&first);
    assert!(!undo.is_current(&first));
    assert!(undo.is_current(&other));
}

#[test]
fn aggregates_consecutive_actions_and_restores_the_group_once() {
    let mut undo = ThreadUndo::default();
    show(&mut undo, ThreadUndoAction::Settled, "settle", "a", 0);
    show(&mut undo, ThreadUndoAction::Settled, "settle", "b", 0);
    let notice = undo.notice(0).unwrap();
    assert_eq!(
        (notice.action, notice.count),
        (ThreadUndoAction::Settled, 2)
    );
    assert_eq!(notice.label, "Settled 2 threads");
    assert_eq!(undo.take_latest(0), [restore("a"), restore("b")]);
    assert!(undo.take_latest(0).is_empty());
    assert_eq!(undo.notice(0), None);
}

#[test]
fn drops_invalidated_claims_immediately_and_restores_only_the_newer_action() {
    let mut undo = ThreadUndo::default();
    show(&mut undo, ThreadUndoAction::Unpinned, "pin", "thread", 0);
    undo.invalidate("pin", "thread");
    assert_eq!(undo.notice(0), None);
    show(&mut undo, ThreadUndoAction::Unpinned, "pin", "thread", 0);
    assert_eq!(undo.notice(0).unwrap().count, 1);
    assert_eq!(undo.take_latest(0), [restore("thread")]);
}

#[test]
fn does_not_show_a_notice_for_a_late_completion_after_a_newer_action() {
    let mut undo = ThreadUndo::default();
    let claim = undo.begin("pin", "thread");
    undo.await_confirmation(
        "command",
        ThreadUndoAction::Unpinned,
        claim,
        restore("thread"),
    );
    undo.invalidate("pin", "thread");
    undo.confirmed("command", 0);
    assert_eq!(undo.notice(0), None);
}

#[test]
fn a_failed_action_offers_no_undo() {
    let mut undo = ThreadUndo::default();
    let claim = undo.begin("archive", "thread");
    undo.await_confirmation(
        "command",
        ThreadUndoAction::Archived,
        claim.clone(),
        restore("thread"),
    );
    undo.failed("command");
    undo.confirmed("command", 0);
    assert_eq!(undo.notice(0), None);
    assert!(!undo.is_current(&claim));
}

#[test]
fn keeps_the_group_available_until_five_seconds_after_the_latest_action() {
    let mut undo = ThreadUndo::default();
    let first = undo.begin("pin", "thread");
    undo.show(
        ThreadUndoAction::Unpinned,
        first.clone(),
        restore("thread"),
        0,
    );
    let second = undo.begin("pin", "second");
    undo.show(
        ThreadUndoAction::Unpinned,
        second.clone(),
        restore("second"),
        4_000,
    );
    assert_eq!(undo.notice(8_999).unwrap().count, 2);
    assert_eq!(undo.notice(8_999).unwrap().expires_at_ms, 9_000);
    assert_eq!(undo.notice(9_000), None);
    assert!(undo.take_latest(9_000).is_empty());
    assert!(!undo.is_current(&first));
    assert!(!undo.is_current(&second));
}

#[test]
fn undoes_the_latest_kind_first_then_reveals_the_preceding_group() {
    let mut undo = ThreadUndo::default();
    show(&mut undo, ThreadUndoAction::Settled, "settle", "a", 0);
    show(&mut undo, ThreadUndoAction::Snoozed, "snooze", "b", 0);
    assert_eq!(undo.take_latest(0), [restore("b")]);
    assert_eq!(undo.notice(0).unwrap().action, ThreadUndoAction::Settled);
    assert_eq!(undo.take_latest(0), [restore("a")]);
    assert!(undo.take_latest(0).is_empty());
}

#[test]
fn names_discarded_drafts() {
    let mut undo = ThreadUndo::default();
    let claim = undo.begin("discard", "new:app");
    undo.show(
        ThreadUndoAction::Discarded,
        claim,
        UndoRestore::Draft {
            key: "new:app".into(),
            draft: Draft::default(),
        },
        0,
    );
    assert_eq!(undo.notice(0).unwrap().label, "Discarded 1 draft");
}
