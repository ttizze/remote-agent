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
        fresh_context: false,
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
    let key = snapshot.composer_key(Some(&session()), &session().into());
    &snapshot.input.drafts[&key].text
}

fn submit(snapshot: &Snapshot, id: &str, alternate: bool) -> (Snapshot, Vec<Effect>) {
    super::super::reduce_intent(
        snapshot,
        "",
        snapshot.input.queued_edits.get(&session()).map_or_else(
            || Intent::Submit {
                thread_id: Some(session()),
                client_user_message_id: id.into(),
                alternate,
            },
            |edit| Intent::SaveQueuedEdit {
                draft_key: edit.key.clone(),
            },
        ),
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
    for status in [
        agent_protocol::session::PreparationStatus::Preparing,
        agent_protocol::session::PreparationStatus::Sending,
    ] {
        waiting_on_work.preparations = vec![agent_protocol::session::SessionPreparation {
            submission: queued("next", "next question"),
            status: status.clone(),
            stage: agent_protocol::session::PreparationStage::Workspace,
        }];
        assert!(working(&waiting_on_work), "foreground input {status:?}");
        assert!(waits(&waiting_on_work, FollowUp::Queue));
    }
    waiting_on_work.preparations.clear();
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
    let mut state = snapshot(
        thread(true, true, vec![queued("waiting", "old text")]),
        "my draft",
    );
    state.connection.connected = true;
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
            thread_id: editing.input.queued_edits[&session()].key.clone(),
            text: "new text".into(),
        },
    );
    let (saved, effects) = submit(&edited, "unused", false);
    assert!(saved.sessions.pending_submissions.is_empty());
    assert_eq!(
        text(&saved),
        "new text",
        "keep the edit while the Host has not acknowledged it"
    );
    assert_eq!(effects.len(), 1);
    let operation = save_operation(
        &saved,
        vec![
            rpc::Input::Text {
                text: "new text".into(),
            },
            rpc::Input::LocalImage {
                path: "/shot.png".into(),
            },
        ],
    );
    use crate::state::operations::Operation;
    operation.capture(&saved, "").unwrap();
    let mut acknowledged = saved;
    operation.complete(&mut acknowledged, "", 2, true).unwrap();
    assert_eq!(text(&acknowledged), "my draft");
    assert!(acknowledged.input.queued_edits.is_empty());
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
        let key = state.input.queued_edits[&session()].key.clone();
        super::super::set_draft_text(&mut state, key, edited.into());
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
    let state = snapshot(thread(true, true, Vec::new()), "withdraw me");
    let input = uuid::Uuid::from_u128(10).to_string();
    let (mut state, _) = submit(&state, &input, false);
    let replica = crate::test_conversations::projection(
        crate::models::ThreadResponse {
            thread: thread(true, true, Vec::new()),
            ..Default::default()
        },
        1,
    );
    // The ACK precedes another device's removal. The replacement contains no
    // input, but its sequence covers the receipt, so the composer stays empty.
    let accepted = replica.snapshot_sequence + 1;
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.pending_submissions)
            .get_mut(input.as_str())
            .unwrap(),
    )
    .cursor = Some(accepted);
    Arc::make_mut(&mut state.sessions.projections).insert(session(), Arc::new(replica));
    super::super::reconcile_pending(&mut state, &session());
    assert!(
        state
            .sessions
            .pending_submissions
            .contains_key(input.as_str())
    );
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.projections)
            .get_mut(&session())
            .unwrap(),
    )
    .snapshot_sequence = accepted + 1;
    super::super::reconcile_pending(&mut state, &session());
    assert!(state.sessions.pending_submissions.is_empty());
    assert!(text(&state).is_empty());
}

fn queued_editor() -> Snapshot {
    let mut state = snapshot(
        thread(true, true, vec![queued("waiting", "old text")]),
        "my draft",
    );
    state.connection.connected = true;
    reduce(
        &mut state,
        session(),
        QueueAction::Edit {
            client_user_message_id: "waiting".into(),
        },
    );
    state
}

#[test]
fn pending_receipts_cannot_retire_a_restored_editor_before_its_queue_is_loaded() {
    let mut state = queued_editor();
    let key = state.input.queued_edits[&session()].key.clone();
    super::super::set_draft_text(&mut state, key, "Unsaved edit restored from disk".into());
    // Conversation copies intentionally clear live queue state while opening.
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    )
    .queue = Default::default();
    let drafts = state.input.drafts.clone();
    let edits = state.input.queued_edits.clone();
    super::super::reconcile_pending(&mut state, &session());
    assert_eq!(state.input.drafts, drafts);
    assert_eq!(state.input.queued_edits, edits);
}

#[test]
fn queued_content_edits_cannot_toggle_the_hidden_conversation_drafts_context() {
    let mut state = queued_editor();
    state.connection.storage_scope = "storage".into();
    assert!(!state.can_set_fresh_context(&session(), true));
    Arc::make_mut(
        Arc::make_mut(&mut state.input.drafts)
            .get_mut(&DraftKey::from(session()))
            .unwrap(),
    )
    .fresh_context = true;
    let before = state.input.drafts.clone();
    for enabled in [false, true] {
        let (unchanged, _) = super::super::reduce_intent(
            &state,
            "",
            Intent::SetFreshContext {
                storage_scope: "storage".into(),
                session: session(),
                enabled,
            },
        );
        assert_eq!(unchanged.input.drafts, before);
    }
    reduce(&mut state, session(), QueueAction::CancelEdit);
    assert!(state.can_set_fresh_context(&session(), true));
}

#[test]
fn queued_content_edits_cannot_change_submission_settings_that_the_host_retains() {
    let mut state = queued_editor();
    Arc::make_mut(state.panes.primary_mut()).navigation = Arc::new(crate::state::Navigation {
        thread_id: Some(session()),
        draft_key: session().into(),
        ..Default::default()
    });
    let key = state.input.queued_edits[&session()].key.clone();
    let composer = crate::presentation::composer::ComposerPresentation::project(
        &state,
        state.panes.primary(),
        None,
    );
    assert_eq!(composer.controls.navigation.draft_key, key);
    assert!(!composer.can_choose_model());
    for intent in [
        Intent::SelectModel {
            thread_id: key.clone(),
            model: crate::models::ModelRef {
                provider: crate::session::ProviderKind::Claude,
                id: "another".into(),
            },
        },
        Intent::SelectEffort {
            thread_id: key.clone(),
            effort: "high".into(),
        },
        Intent::SelectServiceTier {
            thread_id: key.clone(),
            service_tier: "fast".into(),
        },
    ] {
        let (unchanged, effects) = super::super::reduce_intent(&state, "", intent);
        assert!(effects.is_empty());
        assert_eq!(unchanged.input.drafts, state.input.drafts);
        assert_eq!(
            unchanged.catalog.model_preferences,
            state.catalog.model_preferences
        );
    }
    let mut cancelled = state;
    reduce(&mut cancelled, session(), QueueAction::CancelEdit);
    assert!(
        crate::presentation::composer::ComposerPresentation::project(
            &cancelled,
            cancelled.panes.primary(),
            None,
        )
        .can_choose_model()
    );
}

fn save_operation(state: &Snapshot, parts: Vec<rpc::Input>) -> QueuedSave {
    let edit = &state.input.queued_edits[&session()];
    QueuedSave {
        session: session(),
        key: edit.key.clone(),
        message: edit.client_user_message_id.clone(),
        submitted: state.input.drafts[&edit.key].clone(),
        parts,
    }
}

#[test]
fn queued_attachment_removal_is_saved_without_consuming_the_original_draft() {
    let mut state = queued_editor();
    let original = state.input.drafts[&session().into()].clone();
    let key = state.input.queued_edits[&session()].key.clone();
    state = super::super::reduce_intent(
        &state,
        "",
        Intent::RemoveAttachment {
            draft_key: key.clone(),
            index: 0,
        },
    )
    .0;
    super::super::set_draft_text(&mut state, key.clone(), "new text".into());
    let (pending, effects) = super::super::reduce_intent(
        &state,
        "",
        Intent::SaveQueuedEdit {
            draft_key: key.clone(),
        },
    );
    assert_eq!(pending.input.drafts[&session().into()], original);
    assert_eq!(text(&pending), "new text");
    assert_eq!(effects.len(), 1);
    // No success has been published: the edited attachment set and original
    // content both remain available, including after local persistence.
    let restored =
        crate::persistence::decode(&crate::persistence::encode(&pending).unwrap()).unwrap();
    assert_eq!(restored.input.drafts[&key], pending.input.drafts[&key]);
    assert_eq!(restored.input.drafts[&session().into()], original);
}

#[test]
fn a_save_reply_or_click_from_a_cancelled_editor_cannot_close_or_send_a_later_editor() {
    use crate::state::operations::Operation;
    let mut state = queued_editor();
    let old_key = state.input.queued_edits[&session()].key.clone();
    let operation = save_operation(
        &state,
        vec![
            rpc::Input::Text {
                text: "old text".into(),
            },
            rpc::Input::LocalImage {
                path: "/shot.png".into(),
            },
        ],
    );
    operation.capture(&state, "").unwrap();
    reduce(&mut state, session(), QueueAction::CancelEdit);
    reduce(
        &mut state,
        session(),
        QueueAction::Edit {
            client_user_message_id: "waiting".into(),
        },
    );
    let new_key = state.input.queued_edits[&session()].key.clone();
    assert_ne!(new_key, old_key);
    assert!(
        operation.capture(&state, "").is_err(),
        "cancellation before scheduler admission must prevent the old RPC"
    );
    let before = state.clone();
    let (after, effects) = super::super::reduce_intent(
        &state,
        "",
        Intent::SaveQueuedEdit {
            draft_key: old_key.clone(),
        },
    );
    assert!(effects.is_empty());
    assert_eq!(after.input.drafts, before.input.drafts);
    assert!(!after.input.drafts.contains_key(&old_key));
    operation.complete(&mut state, "", 2, true).unwrap();
    assert_eq!(state.input.queued_edits[&session()].key, new_key);
    assert_eq!(state.input.drafts, before.input.drafts);
    assert!(
        super::super::reduce_intent(
            &state,
            "",
            Intent::Submit {
                thread_id: Some(session()),
                client_user_message_id: "old-button".into(),
                alternate: false
            }
        )
        .1
        .is_empty()
    );
}

#[test]
fn uploads_remain_with_their_exact_original_or_queued_editor_owner() {
    use crate::state::{
        Attachment,
        operations::{Operation, UploadAttachment},
    };
    let mut state = queued_editor();
    let old_key = state.input.queued_edits[&session()].key.clone();
    let upload = |key: DraftKey| UploadAttachment {
        draft_key: key,
        attachment: Attachment {
            path: "/device/picked.txt".into(),
            name: "picked.txt".into(),
            is_image: false,
        },
        snapshot: None,
    };
    upload(session().into())
        .complete(&mut state, "", "/host/original-upload.txt".into(), false)
        .unwrap();
    assert_eq!(
        state.input.drafts[&session().into()].attachments[0].path,
        "/host/original-upload.txt"
    );
    assert_eq!(
        state.input.drafts[&old_key].attachments[0].path,
        "/shot.png"
    );
    let mut retired = upload(old_key.clone());
    retired.prepare(&mut state, "").unwrap();
    reduce(&mut state, session(), QueueAction::CancelEdit);
    reduce(
        &mut state,
        session(),
        QueueAction::Edit {
            client_user_message_id: "waiting".into(),
        },
    );
    let before = state.input.drafts.clone();
    retired
        .complete(&mut state, "", "/host/retired-upload.txt".into(), false)
        .unwrap();
    assert_eq!(state.input.drafts, before);
    assert!(!state.input.drafts.contains_key(&old_key));
    assert!(upload(old_key).prepare(&mut state, "").is_err());
}

#[test]
fn queued_saves_cannot_race_upload_transcription_submission_or_another_queue_request() {
    use crate::state::operations::{OperationKey, OperationPhase, OperationState};
    let state = queued_editor();
    let key = state.input.queued_edits[&session()].key.clone();
    for operation in [
        OperationKey::Queue { session: session() },
        OperationKey::Attachment {
            draft_key: key.clone(),
        },
        OperationKey::Dictation {
            draft_key: key.clone(),
        },
        OperationKey::Submission {
            draft_key: key.clone(),
        },
    ] {
        let mut busy = state.clone();
        Arc::make_mut(&mut busy.operations).insert(
            operation,
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        let (after, effects) = super::super::reduce_intent(
            &busy,
            "",
            Intent::SaveQueuedEdit {
                draft_key: key.clone(),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(after.input.drafts, state.input.drafts);
        assert_eq!(after.input.queued_edits, state.input.queued_edits);
    }
}

#[test]
fn completed_dictation_saves_the_latest_queued_editor_without_replacing_newer_text() {
    use crate::state::operations::{
        Dictate, Operation, OperationKey, OperationPhase, OperationState,
    };
    let mut state = queued_editor();
    let key = state.input.queued_edits[&session()].key.clone();
    let dictate = Dictate {
        draft_key: key.clone(),
        preparation: None,
        audio: vec![],
        send: true,
        client_user_message_id: "unused".into(),
    };
    let captured = dictate.capture(&state, "").unwrap();
    super::super::set_draft_text(&mut state, key.clone(), "Newer typed text".into());
    Arc::make_mut(&mut state.operations).insert(
        OperationKey::Dictation {
            draft_key: key.clone(),
        },
        OperationState {
            generation: 1,
            phase: OperationPhase::Running,
        },
    );
    let effects = dictate
        .complete(
            &mut state,
            "",
            (
                captured,
                rpc::Transcription {
                    text: "Spoken words".into(),
                },
            ),
            true,
        )
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(text(&state), "Newer typed text\nSpoken words");
    assert_eq!(state.input.drafts[&session().into()].text, "my draft");
}

#[test]
fn queued_save_captured_after_new_input_keeps_that_new_input_on_acknowledgement() {
    use crate::state::operations::Operation;
    let mut state = queued_editor();
    let key = state.input.queued_edits[&session()].key.clone();
    let operation = save_operation(
        &state,
        vec![
            rpc::Input::Text {
                text: "old text".into(),
            },
            rpc::Input::LocalImage {
                path: "/shot.png".into(),
            },
        ],
    );
    super::super::set_draft_text(
        &mut state,
        key.clone(),
        "Typed while waiting for the scheduler".into(),
    );
    operation.capture(&state, "").unwrap();
    operation.complete(&mut state, "", 2, true).unwrap();
    assert_eq!(text(&state), "Typed while waiting for the scheduler");
    assert_eq!(state.input.queued_edits[&session()].key, key);
    assert_eq!(state.input.drafts[&session().into()].text, "my draft");
}
