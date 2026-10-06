use super::*;
use crate::commands::build::{LifecycleAction, dispatch, lifecycle_command};
use crate::sync::fixtures::*;
use agent_domain::{PendingRequestSummary, RuntimeRequestId, SendMessage, ThreadShell};
use agent_protocol::conversation::{ConversationError, LaunchMessage, WorkspaceStrategy};
use proptest::prelude::*;

fn now() -> Timestamp {
    Timestamp::parse("2026-09-12T10:00:00Z").unwrap()
}
fn future() -> Timestamp {
    Timestamp::parse("2099-01-01T00:00:00Z").unwrap()
}
fn row() -> ThreadShell {
    let mut row = agent_domain::shell(&thread_state("Remote thread")).unwrap();
    row.auto_settle = true;
    row
}
fn shell(sequence: u64, row: ThreadShell) -> ShellSnapshot {
    ShellSnapshot {
        snapshot_sequence: sequence,
        projects: vec![],
        threads: vec![row],
    }
}
fn entry(id: &str, action: LifecycleAction) -> PendingCommand {
    let overlay = LifecycleOverlay::of(&action);
    let mut entry = PendingCommand::new(
        thread_id(),
        Request::Dispatch(Box::new(dispatch(
            thread_id(),
            CommandId::new(id).unwrap(),
            lifecycle_command(action),
        ))),
        now(),
    );
    entry.overlay = overlay;
    entry
}
fn committed(sequence: u64) -> Delivered {
    Delivered::Committed(Committed {
        reply: Reply::Accepted,
        thread_sequence: sequence,
        sequence,
        replayed: false,
    })
}
fn rejected() -> Delivered {
    Delivered::Committed(Committed {
        reply: Reply::Rejected {
            reason: "Remote rejected the action".into(),
        },
        thread_sequence: 1,
        sequence: 1,
        replayed: false,
    })
}
fn id(value: &str) -> CommandId {
    CommandId::new(value).unwrap()
}
fn visible(outbox: &Outbox, shell: &ShellSnapshot) -> ThreadShell {
    outbox.overlay_shell(shell).threads[0].clone()
}

#[test]
fn shows_each_action_before_a_delayed_reply_and_rolls_back_a_rejection() {
    let settled = |row: &mut ThreadShell| {
        row.settled = Some(true);
        row.settled_at = Some(now());
    };
    let snoozed = |row: &mut ThreadShell| {
        row.snoozed_until = Some(future());
        row.snoozed_at = Some(now());
    };
    let pinned = |row: &mut ThreadShell| {
        row.pinned_at = Some(now());
        row.pin_order = Some("a".into());
    };
    type Check = fn(&ThreadShell) -> bool;
    type Setup = Vec<fn(&mut ThreadShell)>;
    let cases: Vec<(LifecycleAction, Setup, Check)> = vec![
        (LifecycleAction::Settle, vec![snoozed, pinned], |r| {
            r.settled == Some(true) && r.pinned_at.is_none() && r.snoozed_until.is_none()
        }),
        (LifecycleAction::Unsettle, vec![settled], |r| {
            r.settled == Some(false) && r.settled_at.is_none()
        }),
        (LifecycleAction::Snooze { until: future() }, vec![], |r| {
            r.snoozed_until == Some(future())
        }),
        (LifecycleAction::Unsnooze, vec![snoozed], |r| {
            r.snoozed_until.is_none() && r.snoozed_at.is_none()
        }),
        (
            LifecycleAction::Pin {
                order: Some("a".into()),
            },
            vec![settled, snoozed],
            |r| r.pinned_at.is_some() && r.pin_order.as_deref() == Some("a"),
        ),
        (LifecycleAction::Unpin, vec![pinned], |r| {
            r.pinned_at.is_none() && r.pin_order.is_none()
        }),
        (
            LifecycleAction::AutoSettle { enabled: false },
            vec![],
            |r| !r.auto_settle,
        ),
        (
            LifecycleAction::ReorderPinned { order: "b".into() },
            vec![],
            |r| r.pin_order.as_deref() == Some("b"),
        ),
        (
            LifecycleAction::ReorderActive { order: "b".into() },
            vec![],
            |r| r.active_order.as_deref() == Some("b"),
        ),
    ];
    for (action, setup, expected) in cases {
        let mut initial = row();
        for change in setup {
            change(&mut initial);
        }
        let source = shell(1, initial);
        let mut outbox = Outbox::default();
        outbox.enqueue(entry("action", action.clone())).unwrap();
        assert!(expected(&visible(&outbox, &source)), "{action:?}");
        outbox.sending(&id("action"));
        assert!(matches!(
            outbox.resolve(&id("action"), rejected()),
            Resolution::Failed { .. }
        ));
        assert_eq!(outbox.overlay_shell(&source).as_ref(), &source);
    }
}

#[test]
fn keeps_the_preview_after_acknowledgement_until_the_matching_shell_update() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(entry("settle", LifecycleAction::Settle))
        .unwrap();
    outbox.sending(&id("settle"));
    assert_eq!(
        outbox.resolve(&id("settle"), committed(3)),
        Resolution::Waiting
    );
    let mut renamed = row();
    renamed.title = "Renamed remotely".into();
    let changed = shell(2, renamed.clone());
    let shown = visible(&outbox, &changed);
    assert_eq!(shown.title, "Renamed remotely");
    assert_eq!(shown.settled, Some(true));
    assert!(outbox.complete(Some(2), |_| Some(3)).is_empty());
    let mut confirmed_row = renamed;
    confirmed_row.settled = Some(true);
    confirmed_row.settled_at = Some(Timestamp::parse("2026-09-12T12:00:00Z").unwrap());
    let confirmed = shell(3, confirmed_row);
    assert!(matches!(outbox.overlay_shell(&confirmed), Cow::Borrowed(_)));
    assert_eq!(outbox.complete(Some(3), |_| None).len(), 1);
    assert_eq!(visible(&outbox, &shell(4, row())).settled, None);
}

#[test]
fn shows_a_queued_reverse_action_and_keeps_it_when_the_earlier_one_fails() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(entry("settle", LifecycleAction::Settle))
        .unwrap();
    outbox.sending(&id("settle"));
    outbox
        .enqueue(entry("unsettle", LifecycleAction::Unsettle))
        .unwrap();
    let source = shell(1, row());
    assert_eq!(visible(&outbox, &source).settled, Some(false));
    assert_eq!(outbox.next(&thread_id()), None);
    outbox.resolve(&id("settle"), rejected());
    assert_eq!(visible(&outbox, &source).settled, Some(false));
    assert_eq!(outbox.next(&thread_id()).unwrap().id, id("unsettle"));
    let mut active = row();
    active.settled = Some(false);
    let confirmed = shell(2, active);
    outbox.sending(&id("unsettle"));
    outbox.resolve(&id("unsettle"), committed(2));
    assert_eq!(outbox.overlay_shell(&confirmed).as_ref(), &confirmed);
}

#[test]
fn does_not_restore_a_remotely_removed_thread() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(entry("settle", LifecycleAction::Settle))
        .unwrap();
    let removed = ShellSnapshot {
        threads: vec![],
        ..shell(2, row())
    };
    assert!(outbox.overlay_shell(&removed).threads.is_empty());
    outbox.sending(&id("settle"));
    outbox.resolve(
        &id("settle"),
        Delivered::NotSent(ConversationError::ThreadNotFound(thread_id()).into()),
    );
    assert_eq!(outbox.overlay_shell(&removed).as_ref(), &removed);
}

fn request(kind: &str) -> PendingRequestSummary {
    PendingRequestSummary {
        id: RuntimeRequestId::new(kind).unwrap(),
        kind: kind.into(),
        created_at: now(),
    }
}

#[test]
fn keeps_pending_approvals_visible_while_a_lifecycle_request_is_pending() {
    let mut blocked = row();
    blocked.pending_request = Some(request("command"));
    let source = shell(1, blocked.clone());
    let mut outbox = Outbox::default();
    outbox
        .enqueue(entry("settle", LifecycleAction::Settle))
        .unwrap();
    assert_eq!(visible(&outbox, &source), blocked);
}

#[test]
fn restores_a_confirmed_settle_or_snooze_when_a_queued_undo_fails() {
    for (park, undo) in [
        (LifecycleAction::Settle, LifecycleAction::Unsettle),
        (
            LifecycleAction::Snooze { until: future() },
            LifecycleAction::Unsnooze,
        ),
    ] {
        let awake = |row: &ThreadShell| row.settled != Some(true) && row.snoozed_until.is_none();
        let mut outbox = Outbox::default();
        outbox.enqueue(entry("park", park.clone())).unwrap();
        outbox.sending(&id("park"));
        outbox.enqueue(entry("undo", undo)).unwrap();
        assert!(awake(&visible(&outbox, &shell(1, row()))));
        outbox.resolve(&id("park"), committed(2));
        let mut parked = row();
        match park {
            LifecycleAction::Settle => parked.settled = Some(true),
            _ => parked.snoozed_until = Some(future()),
        }
        let confirmed = shell(2, parked);
        outbox.complete(Some(2), |_| None);
        assert!(awake(&visible(&outbox, &confirmed)));
        outbox.sending(&id("undo"));
        outbox.resolve(&id("undo"), rejected());
        assert_eq!(outbox.overlay_shell(&confirmed).as_ref(), &confirmed);
    }
}

#[test]
fn preserves_a_newer_approval_when_the_reply_arrives_after_the_shell() {
    for action in [
        LifecycleAction::Settle,
        LifecycleAction::Snooze { until: future() },
    ] {
        let mut outbox = Outbox::default();
        outbox.enqueue(entry("action", action)).unwrap();
        let mut waiting = row();
        waiting.pending_request = Some(request("command"));
        let newer = shell(3, waiting.clone());
        assert_eq!(visible(&outbox, &newer), waiting);
        outbox.sending(&id("action"));
        outbox.resolve(&id("action"), committed(2));
        assert_eq!(outbox.overlay_shell(&newer).as_ref(), &newer);
    }
}

#[test]
fn shows_an_accepted_settle_or_snooze_over_an_old_input_request() {
    for action in [
        LifecycleAction::Settle,
        LifecycleAction::Snooze { until: future() },
    ] {
        let mut stale = row();
        stale.pending_request = Some(request("user_input"));
        let source = shell(1, stale.clone());
        let mut outbox = Outbox::default();
        outbox.enqueue(entry("action", action.clone())).unwrap();
        assert_eq!(visible(&outbox, &source), stale);
        outbox.sending(&id("action"));
        outbox.resolve(&id("action"), committed(2));
        let shown = visible(&outbox, &source);
        match action {
            LifecycleAction::Settle => assert_eq!(shown.settled, Some(true)),
            _ => assert_eq!(shown.snoozed_until, Some(future())),
        }
        assert_eq!(shown.pending_request, None);
    }
}

fn send(message_id: &str, text: &str, created_at: &str) -> PendingCommand {
    let command = Command::Send(SendMessage {
        created_by: agent_domain::MessageAuthor::User,
        creation_source: "mobile".into(),
        id: MessageId::new(message_id).unwrap(),
        text: text.into(),
        attachments: vec![],
        selection: None,
        mode: agent_domain::DispatchMode::StartImmediately,
        intent: Some(agent_domain::DeliveryIntent::Auto),
        source_plan: None,
        resolved_plan: None,
        continuation: None,
        title_seed: None,
        context: None,
    });
    PendingCommand::new(
        thread_id(),
        Request::Dispatch(Box::new(dispatch(thread_id(), id(message_id), command))),
        Timestamp::parse(created_at).unwrap(),
    )
}

#[test]
fn pending_messages_keep_their_context_while_waiting() {
    let mut entry = send(
        "context",
        "[Checkout.tsx](context://v1/mention/setup-file)",
        "2026-09-06T10:00:00Z",
    );
    let context = MessageContext {
        version: 1,
        records: vec![agent_domain::Json(serde_json::json!({
            "version": 1, "kind": "mention", "contextId": "setup-file",
            "label": "Checkout.tsx", "path": "src/Checkout.tsx"
        }))],
    };
    let Request::Dispatch(dispatch) = &mut entry.request else {
        unreachable!()
    };
    let Command::Send(send) = &mut dispatch.command else {
        unreachable!()
    };
    send.context = Some(context.clone());
    let mut outbox = Outbox::default();
    outbox.enqueue(entry).unwrap();
    let pending = outbox.undelivered_messages(&thread_id(), None);
    assert_eq!(
        pending[0].text,
        "[Checkout.tsx](context://v1/mention/setup-file)"
    );
    assert_eq!(pending[0].context, Some(context));
}

#[test]
fn pending_messages_follow_send_order_until_the_thread_folds_them() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(send("first", "first", "2026-09-06T10:00:00Z"))
        .unwrap();
    outbox
        .enqueue(send("second", "second", "2026-09-06T10:00:00Z"))
        .unwrap();
    let ids = |outbox: &Outbox, state: Option<&State>| -> Vec<String> {
        outbox
            .undelivered_messages(&thread_id(), state)
            .iter()
            .map(|m| m.id.to_string())
            .collect()
    };
    assert_eq!(ids(&outbox, None), ["first", "second"]);
    let mut state = thread_state("Thread");
    let mut delivered = state
        .messages
        .first()
        .cloned()
        .unwrap_or_else(|| agent_domain::Message {
            notification: None,
            id: MessageId::new("first").unwrap(),
            run: None,
            role: agent_domain::Role::User,
            text: "first".into(),
            attachments: vec![],
            intent: agent_domain::InputIntent::TurnStart,
            streaming: false,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "mobile".into(),
            created_at: at(),
            updated_at: at(),
            context: None,
        });
    delivered.id = MessageId::new("first").unwrap();
    state.messages.push(delivered);
    assert_eq!(ids(&outbox, Some(&state)), ["second"]);
}

#[test]
fn a_launch_carries_its_first_message_as_a_pending_row() {
    let launch = Launch {
        command_id: id("launch"),
        thread_id: Some(thread_id()),
        project_id: "project".into(),
        title: "Task".into(),
        selection: selection("codex"),
        runtime_mode: agent_domain::RuntimeMode::FullAccess,
        interaction_mode: agent_domain::InteractionMode::Default,
        workspace: WorkspaceStrategy::Root { branch: None },
        message: Some(LaunchMessage {
            id: Some(MessageId::new("first").unwrap()),
            text: "Task".into(),
            attachments: vec![],
            creation_source: "mobile".into(),
            title_seed: Some("Task".into()),
            context: None,
        }),
    };
    let mut outbox = Outbox::default();
    outbox
        .enqueue(PendingCommand::new(
            thread_id(),
            Request::Launch(Box::new(launch)),
            now(),
        ))
        .unwrap();
    assert!(outbox.creates(&thread_id()));
    assert_eq!(outbox.pending_launches().count(), 1);
    assert_eq!(
        outbox.undelivered_messages(&thread_id(), None)[0].text,
        "Task"
    );
}

#[test]
fn backs_off_retries_and_caps_them_at_sixteen_seconds() {
    let delays: Vec<_> = [1, 2, 3, 4, 5, 6].map(retry_delay_ms).to_vec();
    assert_eq!(delays, [1_000, 2_000, 4_000, 8_000, 16_000, 16_000]);
}

#[test]
fn only_removes_a_missing_thread_message_after_the_shell_is_live() {
    assert_eq!(
        delivery_action(false, false, ShellStatus::Synchronizing, true),
        DeliveryAction::Wait
    );
    assert_eq!(
        delivery_action(false, false, ShellStatus::Live, true),
        DeliveryAction::Remove
    );
    assert_eq!(
        delivery_action(false, true, ShellStatus::Live, true),
        DeliveryAction::Send
    );
}

#[test]
fn sends_existing_thread_messages_whenever_connected() {
    assert_eq!(
        delivery_action(false, true, ShellStatus::Live, true),
        DeliveryAction::Send
    );
    assert_eq!(
        delivery_action(false, true, ShellStatus::Live, false),
        DeliveryAction::Wait
    );
}

#[test]
fn sends_creations_once_connected_and_live_and_removes_created_ones() {
    assert_eq!(
        delivery_action(true, false, ShellStatus::Cached, false),
        DeliveryAction::Wait
    );
    assert_eq!(
        delivery_action(true, false, ShellStatus::Synchronizing, true),
        DeliveryAction::Wait
    );
    assert_eq!(
        delivery_action(true, false, ShellStatus::Live, true),
        DeliveryAction::Send
    );
    assert_eq!(
        delivery_action(true, true, ShellStatus::Live, true),
        DeliveryAction::Remove
    );
}

#[test]
fn retries_transport_failures_but_drops_host_decided_refusals() {
    assert!(should_retry(&Delivered::Unknown(
        "Socket is not connected".into()
    )));
    let unknown: RpcFailure = ConversationError::Unavailable("busy".into()).into();
    assert!(should_retry(&Delivered::NotSent(unknown.clone())));
    let refused: RpcFailure = ConversationError::ThreadNotFound(thread_id()).into();
    assert!(!should_retry(&Delivered::NotSent(refused.clone())));
    let mut outbox = Outbox::default();
    outbox
        .enqueue(send("message", "text", "2026-09-06T10:00:00Z"))
        .unwrap();
    outbox.sending(&id("message"));
    assert_eq!(
        outbox.resolve(&id("message"), Delivered::NotSent(unknown)),
        Resolution::Uncertain
    );
    outbox.retry(&id("message"));
    outbox.sending(&id("message"));
    assert!(matches!(
        outbox.resolve(&id("message"), Delivered::NotSent(refused)),
        Resolution::Failed { .. }
    ));
    assert!(outbox.is_empty());
}

#[test]
fn replaces_an_entry_when_a_retry_uses_the_same_id() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(send("message", "first", "2026-09-06T10:00:00Z"))
        .unwrap();
    outbox
        .enqueue(send("message", "edited", "2026-09-06T10:00:00Z"))
        .unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox.undelivered_messages(&thread_id(), None)[0].text,
        "edited"
    );
}

#[test]
fn persisted_entries_round_trip_and_resend_requests_in_flight() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(send("message", "text", "2026-09-06T10:00:00Z"))
        .unwrap();
    outbox.sending(&id("message"));
    let stored = serde_json::to_vec(&outbox.persisted()).unwrap();
    let restored: Outbox = serde_json::from_slice(&stored).unwrap();
    assert_eq!(restored.entries[0].phase, Phase::Queued);
    assert_eq!(restored.next(&thread_id()).unwrap().id, id("message"));
}

#[test]
fn stop_retrying_discards_only_requests_not_in_flight() {
    let mut outbox = Outbox::default();
    outbox
        .enqueue(send("message", "text", "2026-09-06T10:00:00Z"))
        .unwrap();
    outbox.sending(&id("message"));
    assert_eq!(outbox.discard(&id("message")), None);
    outbox.resolve(&id("message"), Delivered::Unknown("lost".into()));
    assert!(outbox.discard(&id("message")).is_some());
}

#[derive(Debug, Clone)]
enum Op {
    Enqueue(bool),
    Send(bool),
    Settle(bool, bool),
    Reconnect,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<bool>().prop_map(Op::Enqueue),
        any::<bool>().prop_map(Op::Send),
        (any::<bool>(), any::<bool>()).prop_map(|(a, b)| Op::Settle(a, b)),
        Just(Op::Reconnect),
    ]
}

proptest! {
    #[test]
    fn each_thread_delivers_in_send_order_one_request_at_a_time(ops in prop::collection::vec(op(), 1..80)) {
        let threads = [ThreadId::new("a").unwrap(), ThreadId::new("b").unwrap()];
        let mut outbox = Outbox::default();
        let mut enqueued: Vec<Vec<CommandId>> = vec![vec![], vec![]];
        let mut first_sent: Vec<Vec<CommandId>> = vec![vec![], vec![]];
        let mut counter = 0;
        let mut sequence = 0;
        for op in ops {
            match op {
                Op::Enqueue(second) => {
                    let index = usize::from(second);
                    counter += 1;
                    let command = id(&format!("command-{counter}"));
                    let request = Request::Dispatch(Box::new(dispatch(
                        threads[index].clone(),
                        command.clone(),
                        Command::MarkUnread,
                    )));
                    outbox.enqueue(PendingCommand::new(threads[index].clone(), request, now())).unwrap();
                    enqueued[index].push(command);
                }
                Op::Send(second) => {
                    let index = usize::from(second);
                    if let Some(next) = outbox.next(&threads[index]).map(|entry| entry.id.clone()) {
                        if !first_sent[index].contains(&next) {
                            first_sent[index].push(next.clone());
                        }
                        outbox.sending(&next);
                    }
                }
                Op::Settle(second, commit) => {
                    let index = usize::from(second);
                    let in_flight: Vec<_> = outbox
                        .entries
                        .iter()
                        .filter(|entry| entry.thread == threads[index] && entry.phase == Phase::InFlight)
                        .map(|entry| entry.id.clone())
                        .collect();
                    prop_assert!(in_flight.len() <= 1);
                    if let Some(id) = in_flight.first() {
                        sequence += 1;
                        let result = if commit { committed(sequence) } else { Delivered::Unknown("lost".into()) };
                        outbox.resolve(id, result);
                    }
                    outbox.complete(None, |_| Some(sequence));
                }
                Op::Reconnect => outbox.reconnected(),
            }
        }
        for index in 0..2 {
            prop_assert_eq!(&first_sent[index][..], &enqueued[index][..first_sent[index].len()]);
        }
    }
}
