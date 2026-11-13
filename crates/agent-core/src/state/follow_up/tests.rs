use super::*;
use crate::{
    models::{SessionStatus, Turn, TurnStatus},
    session::{Capabilities, ProviderKind, SessionRef},
    state::{Draft, Intent, op},
};

fn session() -> SessionRef {
    SessionRef::new(ProviderKind::Claude, "thread".into()).unwrap()
}

fn queued(id: &str, text: &str) -> rpc::Submission {
    rpc::Submission {
        thread_id: session(),
        client_user_message_id: id.into(),
        input: vec![
            rpc::Input::Text { text: text.into() },
            rpc::Input::LocalImage {
                path: "/shot.png".into(),
            },
        ],
        model: None,
        effort: None,
        service_tier: None,
        follow_up: FollowUp::Queue,
    }
}

/// A conversation whose turn runs, or that is idle, with input waiting.
fn thread(running: bool, steer: bool, waiting: Vec<rpc::Submission>) -> Thread {
    Thread {
        id: Some(session()),
        steerable: running && steer,
        status: if running {
            SessionStatus::Running
        } else {
            SessionStatus::Idle
        },
        capabilities: Some(Capabilities {
            steer,
            ..Default::default()
        }),
        turns: Some(vec![Arc::new(Turn {
            id: "turn".into(),
            status: if running {
                TurnStatus::Running
            } else {
                TurnStatus::Completed
            },
            ..Default::default()
        })]),
        queue: rpc::InputQueue {
            inputs: waiting,
            held: false,
        },
        ..Default::default()
    }
}

fn snapshot(thread: Thread, text: &str) -> Snapshot {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(session(), Arc::new(thread));
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        DraftKey::from(&session()),
        Arc::new(Draft {
            text: text.into(),
            ..Default::default()
        }),
    );
    snapshot
}

fn text(snapshot: &Snapshot) -> &str {
    &snapshot.input.drafts[&DraftKey::from(&session())].text
}

fn submit(snapshot: &Snapshot, id: &str, alternate: bool) -> (Snapshot, Vec<Effect>) {
    super::super::reduce_intent(
        snapshot,
        "",
        Intent::Submit {
            thread_id: Some(session()),
            client_user_message_id: id.into(),
            alternate,
        },
    )
}

#[test]
fn the_alternate_does_the_opposite_of_the_preference_for_one_message() {
    for (preference, alternate, expected) in [
        (FollowUp::Queue, false, FollowUp::Queue),
        (FollowUp::Queue, true, FollowUp::Steer),
        (FollowUp::Steer, false, FollowUp::Steer),
        (FollowUp::Steer, true, FollowUp::Queue),
    ] {
        assert_eq!(requested(preference, alternate), expected);
    }
}

#[test]
fn a_drop_moves_before_the_message_at_its_position() {
    let ids: Vec<ClientInputId> = ["a", "b", "c"].map(Into::into).into();
    let dropped = |id: &str, insert| match drop_at(&ids, &id.into(), insert) {
        Some(QueueAction::Move {
            client_user_message_id,
            before,
        }) => {
            assert_eq!(client_user_message_id.as_str(), id);
            Some(before.map(|before| before.to_string()))
        }
        None => None,
        Some(other) => panic!("unexpected {other:?}"),
    };
    // A drop beside itself, past the end or of a missing message moves nothing.
    for (id, insert) in [("b", 1), ("b", 2), ("a", 4), ("missing", 0)] {
        assert_eq!(dropped(id, insert), None, "{id} at {insert}");
    }
    assert_eq!(dropped("a", 2), Some(Some("c".into())));
    assert_eq!(dropped("a", 3), Some(None));
    assert_eq!(dropped("c", 1), Some(Some("b".into())));
    assert_eq!(dropped("c", 0), Some(Some("a".into())));
}

#[test]
fn enter_sends_as_the_send_shortcut_allows() {
    for (shortcut, one_line, multiline) in [
        (SendShortcut::Enter, true, true),
        (SendShortcut::ModEnterMultiline, true, false),
        (SendShortcut::ModEnter, false, false),
    ] {
        assert_eq!(shortcut.enter_sends("one line"), one_line);
        assert_eq!(shortcut.enter_sends("two\nlines"), multiline);
    }
}

#[test]
fn input_waits_unless_a_running_turn_can_take_its_steer() {
    for (running, steer, waiting, follow_up, waits_now) in [
        (true, true, false, FollowUp::Steer, false),
        (true, true, false, FollowUp::Queue, true),
        // A provider that cannot steer queues instead.
        (true, false, false, FollowUp::Steer, true),
        // A steer does not wait for input queued before it.
        (true, true, true, FollowUp::Steer, false),
        // Nothing overtakes input already waiting for the queue to start.
        (false, true, true, FollowUp::Steer, true),
        (false, true, false, FollowUp::Queue, false),
    ] {
        let waiting = if waiting {
            vec![queued("waiting", "earlier")]
        } else {
            Vec::new()
        };
        assert_eq!(
            waits(&thread(running, steer, waiting), follow_up),
            waits_now,
            "running {running}, steer {steer}, {follow_up:?}"
        );
    }
    // Input a stop held waits for the user; new input starts.
    let mut held = thread(false, true, vec![queued("waiting", "earlier")]);
    held.queue.held = true;
    assert!(!waits(&held, FollowUp::Queue));
    // An answered turn waiting on background work takes input as its next
    // turn, though its conversation still runs.
    let mut waiting_on_work = thread(false, true, Vec::new());
    waiting_on_work.status = SessionStatus::Running;
    waiting_on_work.background = vec![agent_protocol::execution::BackgroundTask {
        id: "shell".into(),
        kind: agent_protocol::execution::BackgroundTaskKind::Command,
        description: None,
    }];
    assert!(!working(&waiting_on_work));
    assert!(!waits(&waiting_on_work, FollowUp::Queue));
    waiting_on_work.background.clear();
    assert!(working(&waiting_on_work));
    assert!(waits(&waiting_on_work, FollowUp::Queue));
}

#[test]
fn the_preference_and_the_alternate_choose_what_a_send_asks_the_host() {
    for (preference, alternate, follow_up) in [
        (FollowUp::Queue, false, FollowUp::Queue),
        (FollowUp::Queue, true, FollowUp::Steer),
        (FollowUp::Steer, false, FollowUp::Steer),
        (FollowUp::Steer, true, FollowUp::Queue),
    ] {
        let mut state = snapshot(thread(true, true, Vec::new()), "follow-up");
        state.input.composer_preferences = ComposerPreferences {
            follow_up: preference,
            ..Default::default()
        };
        let (next, effects) = submit(&state, "input", alternate);
        let pending = &next.sessions.pending_submissions["input"];
        assert_eq!(pending.queued, follow_up == FollowUp::Queue);
        let expected = Effect::execute(op::SendSubmission {
            thread_id: session(),
            client_user_message_id: "input".into(),
            draft: pending.draft.clone(),
            follow_up,
        });
        assert_eq!(format!("{effects:?}"), format!("{:?}", [expected]));
    }
}

#[test]
fn pending_inputs_use_host_steering_admission() {
    for (capability, allowed, skips_compaction) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut conversation = thread(true, capability, Vec::new());
        conversation.steerable = allowed;
        conversation.skips_compaction = skips_compaction;
        let mut state = snapshot(conversation, "follow-up");
        state.input.composer_preferences.follow_up = FollowUp::Steer;
        let (next, _) = submit(&state, "input", false);
        assert_eq!(next.sessions.pending_submissions["input"].queued, !allowed);
    }
}

#[test]
fn editing_a_queued_message_saves_its_text_and_returns_the_composer_text() {
    let state = snapshot(
        thread(true, true, vec![queued("waiting", "old text")]),
        "my draft",
    );
    let edit = |state: &Snapshot, action| {
        super::super::reduce_intent(
            state,
            "",
            Intent::Queue {
                thread_id: session(),
                action,
            },
        )
    };
    let (editing, effects) = edit(
        &state,
        QueueAction::Edit {
            client_user_message_id: "waiting".into(),
        },
    );
    assert!(effects.is_empty());
    assert_eq!(text(&editing), "old text");
    let (cancelled, _) = edit(&editing, QueueAction::CancelEdit);
    assert_eq!(text(&cancelled), "my draft");
    assert!(cancelled.input.queued_edits.is_empty());

    let (edited, _) = super::super::reduce_intent(
        &editing,
        "",
        Intent::SetDraftText {
            thread_id: DraftKey::from(&session()),
            text: "new text".into(),
        },
    );
    let (saved, effects) = submit(&edited, "unused", false);
    assert!(saved.sessions.pending_submissions.is_empty());
    assert_eq!(text(&saved), "my draft");
    let expected = Effect::execute(rpc::ChangeQueue {
        thread_id: session(),
        change: QueueChange::Edit {
            client_user_message_id: "waiting".into(),
            input: vec![
                rpc::Input::Text {
                    text: "new text".into(),
                },
                rpc::Input::LocalImage {
                    path: "/shot.png".into(),
                },
            ],
        },
    });
    assert_eq!(format!("{effects:?}"), format!("{:?}", [expected]));
}

#[test]
fn an_edit_ends_when_its_message_leaves_the_queue() {
    for (saved, edited, kept) in [
        ("", "unsaved edit", "unsaved edit"),
        ("my draft", "unsaved edit", "my draft"),
        ("", "old text", ""),
    ] {
        let mut state = snapshot(
            thread(true, true, vec![queued("waiting", "old text")]),
            saved,
        );
        let effects = reduce(
            &mut state,
            session(),
            QueueAction::Edit {
                client_user_message_id: "waiting".into(),
            },
        );
        assert!(effects.is_empty());
        set_text(&mut state, &DraftKey::from(&session()), edited.into());
        Arc::make_mut(&mut state.sessions.conversations)
            .insert(session(), Arc::new(thread(false, true, Vec::new())));
        reconcile_edit(&mut state, &session());
        assert!(state.input.queued_edits.is_empty());
        assert_eq!(text(&state), kept);
    }
}

#[test]
fn removing_moving_steering_and_resuming_ask_the_host() {
    let mut state = snapshot(thread(true, true, vec![queued("waiting", "old text")]), "");
    for (action, change) in [
        (
            QueueAction::Steer {
                client_user_message_id: "waiting".into(),
            },
            QueueChange::Steer {
                client_user_message_id: "waiting".into(),
            },
        ),
        (QueueAction::Resume, QueueChange::Resume),
        (
            QueueAction::Move {
                client_user_message_id: "waiting".into(),
                before: None,
            },
            QueueChange::Move {
                client_user_message_id: "waiting".into(),
                before: None,
            },
        ),
        (
            QueueAction::Remove {
                client_user_message_id: "waiting".into(),
            },
            QueueChange::Remove {
                client_user_message_id: "waiting".into(),
            },
        ),
    ] {
        let effects = reduce(&mut state, session(), action);
        let expected = Effect::execute(rpc::ChangeQueue {
            thread_id: session(),
            change,
        });
        assert_eq!(format!("{effects:?}"), format!("{:?}", [expected]));
    }
}

#[test]
fn a_queued_message_removed_before_its_admission_frame_arrives_does_not_return_to_the_composer() {
    use agent_protocol::sync::{Cursor, Key, Message};
    let state = snapshot(thread(true, true, Vec::new()), "withdraw me");
    let input = uuid::Uuid::from_u128(10).to_string();
    let (mut state, _) = submit(&state, &input, false);
    let replica = crate::test_conversations::replica(
        crate::models::ThreadResponse {
            thread: thread(true, true, Vec::new()),
            ..Default::default()
        },
        1,
    );
    let from = replica.cursor().unwrap();
    // The Submit ACK precedes a later removal by another device. This device
    // resumes after both commits, so it sees a tombstone rather than the input.
    let accepted = Cursor {
        sequence: from.sequence + 1,
        ..from
    };
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.pending_submissions)
            .get_mut(input.as_str())
            .unwrap(),
    )
    .cursor = Some(accepted);
    Arc::make_mut(&mut state.sessions.replicas).insert(session(), Arc::new(replica));
    super::super::reconcile_pending(&mut state, &session());
    assert!(
        state
            .sessions
            .pending_submissions
            .contains_key(input.as_str())
    );
    let replica = &state.sessions.replicas[&session()];
    let next = replica
        .apply(
            replica.subscription(),
            Message::Events {
                through: Cursor {
                    sequence: accepted.sequence + 1,
                    ..accepted
                },
                events: vec![agent_protocol::sync::Event {
                    sequence: (Cursor {
                        sequence: accepted.sequence + 1,
                        ..accepted
                    })
                    .sequence,
                    rows: Vec::new(),
                    removed: vec![Key::Input {
                        thread: session(),
                        id: input.into(),
                    }],
                }],
            },
        )
        .unwrap();
    Arc::make_mut(&mut state.sessions.replicas).insert(session(), Arc::new(next));
    super::super::reconcile_pending(&mut state, &session());
    assert!(state.sessions.pending_submissions.is_empty());
    assert!(text(&state).is_empty());
}
