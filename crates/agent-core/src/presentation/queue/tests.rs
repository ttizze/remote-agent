use super::*;
use agent_protocol::session::ProviderKind;

fn session() -> SessionRef {
    SessionRef::new(ProviderKind::Codex, "queue-owner".into()).unwrap()
}

fn queue() -> QueuePresentation {
    QueuePresentation {
        messages: ["a", "b", "c", "pending"]
            .into_iter()
            .map(|id| QueuedMessage {
                id: id.into(),
                text: format!("{id} text"),
                attachments: 0,
                fresh_context: false,
                pending: id == "pending",
            })
            .collect(),
        held: false,
        resumable: false,
        editing: None,
        busy: false,
        can_steer: true,
        skips_compaction: false,
        can_change: true,
        edit_blocked: false,
        session: Some(session()),
        language: UiLanguage::English,
    }
}

fn action(intent: Option<Intent>) -> Option<QueueAction> {
    match intent {
        Some(Intent::Queue { thread_id, action }) => {
            assert_eq!(thread_id, session());
            Some(action)
        }
        None => None,
        _ => panic!("queue controls must remain in their own conversation"),
    }
}

fn moved(id: &str, before: Option<&str>) -> Option<QueueAction> {
    Some(QueueAction::Move {
        client_user_message_id: id.into(),
        before: before.map(Into::into),
    })
}

#[test]
fn only_a_new_hidden_editor_opens_the_long_queue() {
    // QueuedRunsControl shows the first two of four; three messages all fit.
    let mut queue = queue();
    queue.messages[3].id = "d".into();
    queue.messages[3].pending = false;
    assert!(!queue.reveal_edit(None));
    for (id, hidden) in [
        ("a", false),
        ("b", false),
        ("c", true),
        ("d", true),
        ("gone", false),
    ] {
        queue.editing = Some(id.into());
        assert_eq!(queue.reveal_edit(None), hidden);
        assert!(
            !queue.reveal_edit(Some(&id.into())),
            "a user's later collapse wins"
        );
    }
    queue.messages.pop();
    queue.editing = Some("c".into());
    assert!(!queue.reveal_edit(None));
}

#[test]
fn arrows_and_drag_resolve_current_confirmed_neighbors_and_reject_stale_targets() {
    let mut queue = queue();
    assert_eq!(action(queue.action("a".into(), QueueControl::MoveUp)), None);
    assert_eq!(
        action(queue.action("a".into(), QueueControl::MoveDown)),
        moved("a", Some("c"))
    );
    assert_eq!(
        action(queue.action("c".into(), QueueControl::MoveDown)),
        None
    );
    assert_eq!(
        action(queue.drop_on("c".into(), "a".into(), false)),
        moved("c", Some("a"))
    );
    assert_eq!(
        action(queue.drop_on("a".into(), "b".into(), true)),
        moved("a", Some("c"))
    );
    for (from, to) in [
        ("pending", "a"),
        ("a", "pending"),
        ("gone", "a"),
        ("a", "gone"),
    ] {
        assert!(queue.drop_on(from.into(), to.into(), false).is_none());
    }
    assert!(queue.drop_on("a".into(), "a".into(), false).is_none());
    assert!(queue.drop_on("a".into(), "a".into(), true).is_none());
    let old = queue.clone();
    queue.messages.swap(0, 1);
    assert_eq!(
        action(queue.action("a".into(), QueueControl::MoveDown)),
        moved("a", None)
    );
    assert_eq!(
        action(old.action("a".into(), QueueControl::MoveDown)),
        moved("a", Some("c"))
    );
    assert_eq!(
        action(queue.steer_next()),
        Some(QueueAction::Steer {
            client_user_message_id: "b".into()
        })
    );
    assert_eq!(
        action(queue.edit_latest()),
        Some(QueueAction::Edit {
            client_user_message_id: "c".into()
        })
    );
}

#[test]
fn unavailable_busy_pending_and_unowned_controls_never_emit_a_change() {
    // A queued message has a Host identity before it can be changed. A failed
    // operation becomes retryable; a running operation cannot be sent twice.
    for (available, busy, pending, enabled) in [
        (true, false, false, true),
        (false, false, false, false),
        (true, true, false, false),
        (true, false, true, false),
        (false, true, true, false),
    ] {
        let mut queue = queue();
        queue.can_change = available;
        queue.busy = busy;
        queue.messages[0].pending = pending;
        for control in [
            QueueControl::Edit,
            QueueControl::Steer,
            QueueControl::Remove,
        ] {
            let shown = queue
                .controls("a".into())
                .into_iter()
                .find(|shown| shown.action == control)
                .unwrap();
            assert_eq!(shown.enabled, enabled);
            assert_eq!(queue.action("a".into(), control).is_some(), enabled);
        }
        assert_eq!(
            queue.drop_on("a".into(), "c".into(), false).is_some(),
            enabled
        );
    }
    let mut unowned = queue();
    unowned.session = None;
    assert!(
        unowned
            .controls("a".into())
            .iter()
            .all(|control| !control.enabled)
    );
    assert!(
        unowned
            .action("gone".into(), QueueControl::Remove)
            .is_none()
    );
    assert!(unowned.drop_on("a".into(), "c".into(), false).is_none());
}

#[test]
fn questions_hold_the_editor_while_cancel_and_send_next_keep_their_own_meanings() {
    let mut queue = queue();
    queue.edit_blocked = true;
    assert!(queue.action("a".into(), QueueControl::Edit).is_none());
    assert!(queue.edit_latest().is_none());
    assert!(queue.action("a".into(), QueueControl::Remove).is_some());
    assert!(queue.steer_next().is_some());
    queue.edit_blocked = false;
    queue.editing = Some("a".into());
    assert!(
        queue.action("a".into(), QueueControl::Edit).is_none(),
        "reopening must not discard an unsaved edit"
    );
    assert!(queue.edit_latest().is_none());
    assert!(
        queue.steer_next().is_some(),
        "T3's send-next shortcut still addresses the first queued message"
    );
    assert_eq!(
        queue
            .controls("a".into())
            .iter()
            .map(|control| control.action)
            .collect::<Vec<_>>(),
        [
            QueueControl::CancelEdit,
            QueueControl::MoveUp,
            QueueControl::MoveDown
        ]
    );
    queue.can_change = false;
    queue.busy = true;
    assert_eq!(
        action(queue.action("a".into(), QueueControl::CancelEdit)),
        Some(QueueAction::CancelEdit)
    );
    assert!(queue.action("b".into(), QueueControl::CancelEdit).is_none());
}

#[test]
fn fresh_context_does_not_steer_or_skip_the_first_queued_message() {
    let mut queue = queue();
    queue.messages[0].fresh_context = true;
    assert!(queue.steer_next().is_none());
    assert!(
        !queue
            .controls("a".into())
            .iter()
            .any(|control| control.action == QueueControl::Steer)
    );
    assert!(queue.action("b".into(), QueueControl::Steer).is_some());
    queue.skips_compaction = true;
    assert!(queue.steer_next().is_some());
    queue.can_steer = false;
    assert!(queue.steer_next().is_none());
}

#[test]
fn a_long_queue_shows_the_first_two_and_keeps_the_full_count_and_plain_content() {
    let mut queue = queue();
    queue.messages[0].text = "First line\nSecond line".into();
    assert_eq!(queue.message_count(), 4);
    assert!(queue.can_expand());
    let short = queue.messages(false);
    assert_eq!(
        short
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(short[0].text, "First line\nSecond line");
    assert_eq!(queue.messages(true).len(), 4);
    assert_eq!(queue.labels()["more"], "Show all 4 queued");
    for count in [3, 2, 1, 0] {
        queue.messages.truncate(count);
        assert!(!queue.can_expand());
        assert_eq!(queue.messages(false).len(), count);
        assert_eq!(queue.messages(true).len(), count);
    }
}
