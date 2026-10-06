use super::*;

fn later(seconds: i64) -> Timestamp {
    Timestamp::from_millis(at().millis() + seconds * 1000).unwrap()
}
fn command_at(s: &mut State, key: &str, at: Timestamp, command: Command) -> Step {
    let result = ThreadMachine::step(
        s,
        &InputEnvelope {
            at,
            key: key.into(),
            input: Input::Command {
                id: CommandId::new(key).unwrap(),
                command: Box::new(command),
                receipt: None,
            },
        },
    );
    *s = fold(s, &result.facts).unwrap();
    result
}
fn rejected(reason: &str) -> Reply {
    Reply::Rejected {
        reason: reason.into(),
    }
}

// Sidebar commands on an archived thread are rejected, while message.dispatch
// is still accepted.
#[test]
fn an_archived_thread_rejects_sidebar_changes_but_accepts_messages() {
    let mut s = state();
    command(&mut s, "archive", Command::Archive { archived: true });
    for (key, mutation) in [
        (
            "settle",
            Command::Settle {
                settled: true,
                at: None,
            },
        ),
        (
            "unsettle",
            Command::Settle {
                settled: false,
                at: None,
            },
        ),
        (
            "snooze",
            Command::Snooze {
                until: Some(later(60)),
            },
        ),
        ("unsnooze", Command::Snooze { until: None }),
        ("auto-settle", Command::AutoSettle { enabled: false }),
        (
            "pin",
            Command::Pin {
                pinned: true,
                order: None,
            },
        ),
        (
            "unpin",
            Command::Pin {
                pinned: false,
                order: None,
            },
        ),
        ("pin-reorder", Command::ReorderPinned { order: "a0".into() }),
        (
            "active-reorder",
            Command::ReorderActive { order: "a0".into() },
        ),
    ] {
        assert_eq!(
            command(&mut s, key, mutation).reply,
            rejected("thread-archived"),
            "{key}"
        );
    }
    let Reply::Run(run) = command(
        &mut s,
        "message",
        send_message("message", DispatchMode::StartImmediately),
    )
    .reply
    else {
        panic!()
    };
    assert_eq!(
        s.runs.iter().find(|r| r.id == run).unwrap().status,
        RunStatus::Starting
    );
}

#[test]
fn pins_keep_their_slot_and_reorders_need_the_right_list() {
    let mut s = state();
    assert_eq!(
        command(
            &mut s,
            "early-reorder",
            Command::ReorderPinned { order: "a0".into() }
        )
        .reply,
        rejected("thread-not-pinned")
    );
    command_at(
        &mut s,
        "pin",
        later(1),
        Command::Pin {
            pinned: true,
            order: Some("a0".into()),
        },
    );
    let pinned_at = s.thread.as_ref().unwrap().pinned_at.clone();
    command_at(
        &mut s,
        "repin",
        later(2),
        Command::Pin {
            pinned: true,
            order: Some("z9".into()),
        },
    );
    let thread = s.thread.as_ref().unwrap();
    assert_eq!(thread.pinned_at, pinned_at);
    assert_eq!(thread.pin_order.as_deref(), Some("a0"));
    assert_eq!(
        command(
            &mut s,
            "active",
            Command::ReorderActive { order: "b0".into() }
        )
        .reply,
        rejected("thread-not-active")
    );
    command(
        &mut s,
        "pin-reorder",
        Command::ReorderPinned { order: "b1".into() },
    );
    assert_eq!(s.thread.as_ref().unwrap().pin_order.as_deref(), Some("b1"));
    command(
        &mut s,
        "unpin",
        Command::Pin {
            pinned: false,
            order: None,
        },
    );
    let thread = s.thread.as_ref().unwrap();
    assert_eq!(
        (thread.pinned_at.clone(), thread.pin_order.clone()),
        (None, None)
    );

    let updated = s.thread.as_ref().unwrap().updated_at.clone();
    command_at(
        &mut s,
        "active-order",
        later(5),
        Command::ReorderActive { order: "c0".into() },
    );
    let thread = s.thread.as_ref().unwrap();
    assert_eq!(thread.active_order.as_deref(), Some("c0"));
    assert_eq!(thread.updated_at, updated);
    command(
        &mut s,
        "settle",
        Command::Settle {
            settled: true,
            at: None,
        },
    );
    assert_eq!(
        command(
            &mut s,
            "settled-reorder",
            Command::ReorderActive { order: "d0".into() }
        )
        .reply,
        rejected("thread-not-active")
    );
}

// Deleting a deleted thread repeats only its cleanup, and delegated children
// are left to their own threads.
#[test]
fn deleting_is_idempotent_and_leaves_delegated_children_running() {
    let mut s = state();
    running(&mut s, "parent");
    queue::delegate(&mut s, "child");
    let first = command(&mut s, "delete", Command::Delete);
    assert!(!first.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::SendToThread { command, .. } if matches!(command.as_ref(), Command::Stop)
    ) || matches!(
        &effect.body,
        EffectBody::Provider(ProviderCommand::Interrupt { .. })
    )));
    assert_eq!(s.runs[0].status, RunStatus::Cancelled);
    let deleted_at = s.thread.as_ref().unwrap().deleted_at.clone();
    let again = command_at(&mut s, "delete-again", later(9), Command::Delete);
    assert_eq!(again.reply, Reply::Accepted);
    assert!(again.facts.is_empty());
    assert_eq!(s.thread.as_ref().unwrap().deleted_at, deleted_at);
    assert_eq!(
        command(&mut s, "rename", Command::Rename { title: "x".into() }).reply,
        rejected("thread-deleted")
    );
}

// Queues defer_start behind an active run, and its message.dispatch schema
// has no text length or emptiness bound.
#[test]
fn deferred_starts_queue_behind_active_runs_and_text_is_not_bounded() {
    let mut s = state();
    running(&mut s, "first");
    let step = command(
        &mut s,
        "deferred",
        send_message("deferred", DispatchMode::DeferStart),
    );
    assert!(matches!(step.reply, Reply::Run(_)));
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    assert!(
        !step
            .effects
            .iter()
            .any(|effect| matches!(effect.body, EffectBody::PrepareWorkspace { .. }))
    );
    for (key, text) in [("empty", String::new()), ("long", "x".repeat(120_001))] {
        let Command::Send(mut message) = send_message(key, DispatchMode::QueueAfterActive) else {
            unreachable!()
        };
        message.text = text;
        assert!(matches!(
            command(&mut s, key, Command::Send(message)).reply,
            Reply::Run(_)
        ));
    }
}

fn attachment(kind: AttachmentKind, mime: &str, size: u64) -> Attachment {
    Attachment {
        kind,
        source: None,
        id: format!("chat:{mime}:{size}"),
        name: "file".into(),
        mime_type: mime.into(),
        path: "/tmp/file".into(),
        size,
    }
}

#[test]
fn attachments_follow_the_reference_schemas_and_image_budget() {
    let image = |size| attachment(AttachmentKind::Image, "image/png", size);
    let file = |mime: &str, size| attachment(AttachmentKind::File, mime, size);
    assert_eq!(
        validate_attachments(&[file("text/plain", 0)]),
        Err("invalid-attachment")
    );
    assert_eq!(
        validate_attachments(&[attachment(AttachmentKind::Image, "text/plain", 1)]),
        Err("invalid-attachment")
    );
    let mut long_name = file("text/plain", 1);
    long_name.name = "n".repeat(256);
    assert_eq!(
        validate_attachments(&[long_name]),
        Err("invalid-attachment")
    );
    let mut long_mime = file("text/plain", 1);
    long_mime.mime_type = format!("text/{}", "x".repeat(100));
    assert_eq!(
        validate_attachments(&[long_mime]),
        Err("invalid-attachment")
    );
    let mut long_id = file("text/plain", 1);
    long_id.id = "i".repeat(129);
    assert_eq!(validate_attachments(&[long_id]), Err("invalid-attachment"));
    // An image sent as a file still counts toward the 80 MiB image budget.
    let mut images = (0..8)
        .map(|index| {
            let mut image = image(10 * 1024 * 1024);
            image.id = format!("image-{index}");
            image
        })
        .collect::<Vec<_>>();
    assert_eq!(validate_attachments(&images), Ok(()));
    let mut extra = file("IMAGE/PNG", 1);
    extra.id = "extra".into();
    images.push(extra);
    assert_eq!(validate_attachments(&images), Err("total-images-too-large"));
}
