use super::*;
use crate::presentation::queue::{QueueControl, QueuePresentation};
use crate::state::Intent;

#[test]
fn follow_up_controls_own_row_admission_and_adjacent_move_actions() {
    let message = |id: &str| QueuedMessage {
        id: id.into(),
        fresh_context: false,
        text: id.into(),
        attachments: 0,
        pending: false,
    };
    let controls = QueuePresentation {
        messages: vec![message("one"), message("two"), message("three")],
        held: false,
        resumable: false,
        can_steer: true,
        editing: None,
        busy: false,
        skips_compaction: false,
        can_change: true,
        edit_blocked: false,
        session: Some(
            crate::session::SessionRef::new(ProviderKind::Codex, "queue".into()).unwrap(),
        ),
        language: crate::localization::UiLanguage::English,
    };
    let one = agent_protocol::ids::ClientInputId::from("one");
    let two = agent_protocol::ids::ClientInputId::from("two");
    let three = agent_protocol::ids::ClientInputId::from("three");
    assert!(controls.action(one.clone(), QueueControl::Edit).is_some());
    assert!(controls.action(one.clone(), QueueControl::Remove).is_some());
    assert!(controls.action(one.clone(), QueueControl::Steer).is_some());
    assert!(controls.resume().is_none());
    assert!(controls.action(one.clone(), QueueControl::MoveUp).is_none());
    assert!(controls.action(two.clone(), QueueControl::MoveUp).is_some());
    assert!(
        controls
            .action(two.clone(), QueueControl::MoveDown)
            .is_some()
    );
    assert!(
        controls
            .action(three.clone(), QueueControl::MoveDown)
            .is_none()
    );
    assert_eq!(
        controls
            .messages(true)
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![one.clone(), two.clone(), three.clone()]
    );
    assert!(matches!(
        controls.action(two.clone(), QueueControl::MoveUp),
        Some(Intent::Queue { action: crate::state::follow_up::QueueAction::Move { before: Some(before), .. }, .. }) if before == one
    ));
    assert!(matches!(
        controls.action(two.clone(), QueueControl::MoveDown),
        Some(Intent::Queue {
            action: crate::state::follow_up::QueueAction::Move { before: None, .. },
            ..
        })
    ));

    let mut editing = controls.clone();
    editing.editing = Some(two.clone());
    assert!(editing.action(two.clone(), QueueControl::Edit).is_none());
    assert!(editing.action(two.clone(), QueueControl::Remove).is_none());
    assert!(editing.action(two.clone(), QueueControl::Steer).is_none());
    assert!(
        editing
            .action(two.clone(), QueueControl::CancelEdit)
            .is_some()
    );

    let mut held = controls.clone();
    held.held = true;
    assert!(held.resume().is_some());
    held.busy = true;
    assert!(held.resume().is_none());
}

fn queue_edit_state() -> (Snapshot, crate::session::SessionRef) {
    use crate::state::{Attachment, DraftKey};
    let session =
        crate::session::SessionRef::new(ProviderKind::Codex, "attachment-edit".into()).unwrap();
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    snapshot.connection.storage_scope = "first-host".into();
    Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(crate::state::Navigation {
        thread_id: Some(session.clone()),
        draft_key: DraftKey::from(&session),
        cwd: "/project".into(),
    });
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        session.clone().into(),
        Arc::new(Draft {
            text: "My own draft".into(),
            attachments: vec![Attachment {
                path: "/host/own.txt".into(),
                name: "own.txt".into(),
                is_image: false,
            }],
            ..Default::default()
        }),
    );
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        session.clone(),
        Arc::new(crate::models::Thread {
            id: Some(session.clone()),
            active_provider: Some(ProviderKind::Codex),
            queue: agent_protocol::operations::InputQueue {
                held: true,
                inputs: vec![agent_protocol::operations::Submission {
                    fresh_context: false,
                    thread_id: session.clone(),
                    client_user_message_id: "queued".into(),
                    input: vec![
                        agent_protocol::operations::Input::Text {
                            text: "Queued content".into(),
                        },
                        agent_protocol::operations::Input::LocalImage {
                            path: "/host/queued.png".into(),
                        },
                    ],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: FollowUp::Queue,
                }],
            },
            ..Default::default()
        }),
    );
    (snapshot, session)
}

fn edit_queue(
    snapshot: &Snapshot,
    session: &crate::session::SessionRef,
    action: crate::state::follow_up::QueueAction,
) -> Snapshot {
    crate::state::reduce(
        snapshot,
        "",
        crate::state::Event::Intent(Box::new(crate::state::Intent::Queue {
            thread_id: session.clone(),
            action,
        })),
    )
    .0
}

#[test]
fn queued_edit_owns_its_attachments_and_cancel_restores_the_entire_original_draft() {
    use crate::state::{Event, Intent, follow_up::QueueAction};
    let (snapshot, session) = queue_edit_state();
    let original = snapshot.input.drafts[&session.clone().into()].clone();
    let editing = edit_queue(
        &snapshot,
        &session,
        QueueAction::Edit {
            client_user_message_id: "queued".into(),
        },
    );
    assert_eq!(
        ComposerPresentation::project(&editing, editing.panes.primary(), None)
            .draft
            .attachments[0]
            .path,
        "/host/queued.png"
    );
    assert_eq!(
        editing.input.drafts[&session.clone().into()],
        original,
        "the queued editor must not replace the original draft's identity"
    );
    let (removed, _) = crate::state::reduce(
        &editing,
        "",
        Event::Intent(Box::new(Intent::RemoveAttachment {
            draft_key: ComposerPresentation::project(&editing, editing.panes.primary(), None)
                .controls
                .navigation
                .draft_key
                .clone(),
            index: 0,
        })),
    );
    let cancelled = edit_queue(&removed, &session, QueueAction::CancelEdit);
    assert_eq!(cancelled.input.drafts[&session.clone().into()], original);
    assert_eq!(
        cancelled.sessions.conversations[&session].queue.inputs[0]
            .input
            .len(),
        2,
        "removal stays local until save"
    );
}

#[test]
fn queued_edit_and_original_draft_both_survive_device_storage_and_host_scope_changes() {
    use crate::state::{Event, follow_up::QueueAction};
    let (snapshot, session) = queue_edit_state();
    let original = snapshot.input.drafts[&session.clone().into()].clone();
    let editing = edit_queue(
        &snapshot,
        &session,
        QueueAction::Edit {
            client_user_message_id: "queued".into(),
        },
    );
    let restored =
        crate::persistence::decode(&crate::persistence::encode(&editing).unwrap()).unwrap();
    assert!(restored.input.queued_edits.contains_key(&session));
    assert_eq!(
        edit_queue(&restored, &session, QueueAction::CancelEdit)
            .input
            .drafts[&session.clone().into()],
        original
    );
    let away = crate::state::reduce(&editing, "", Event::StorageScope("other-host".into())).0;
    assert!(
        away.input.queued_edits.is_empty(),
        "edit identity belongs to its Host storage scope"
    );
    let returned = crate::state::reduce(&away, "", Event::StorageScope("first-host".into())).0;
    assert!(returned.input.queued_edits.contains_key(&session));
    assert_eq!(
        edit_queue(&returned, &session, QueueAction::CancelEdit)
            .input
            .drafts[&session.into()],
        original
    );
}

#[test]
fn queued_editor_failures_are_visible_in_its_conversation_and_leave_with_the_editor() {
    use crate::state::{
        follow_up::QueueAction,
        operations::{OperationKey, OperationPhase, OperationState},
    };
    let (snapshot, session) = queue_edit_state();
    let mut editing = edit_queue(
        &snapshot,
        &session,
        QueueAction::Edit {
            client_user_message_id: "queued".into(),
        },
    );
    let key = ComposerPresentation::project(&editing, editing.panes.primary(), None)
        .controls
        .navigation
        .draft_key
        .clone();
    let failure = OperationKey::Attachment { draft_key: key };
    Arc::make_mut(&mut editing.operations).insert(
        failure.clone(),
        OperationState {
            generation: 1,
            phase: OperationPhase::Failed {
                message: "Fixture upload failed".into(),
            },
        },
    );
    let context = crate::presentation::context::ConversationContext::project(
        &editing,
        editing.panes.primary(),
        "",
        None,
    );
    assert!(
        context
            .notices()
            .iter()
            .any(|notice| notice.text.to_string().contains("Fixture upload failed"))
    );
    let cancelled = edit_queue(&editing, &session, QueueAction::CancelEdit);
    let context = crate::presentation::context::ConversationContext::project(
        &cancelled,
        cancelled.panes.primary(),
        "",
        None,
    );
    assert!(
        !context
            .notices()
            .iter()
            .any(|notice| notice.text.to_string().contains("Fixture upload failed"))
    );
    assert_eq!(cancelled.input.drafts, snapshot.input.drafts);
}

#[test]
fn queued_editor_settings_and_model_discovery_read_its_retained_submission_model() {
    use crate::state::{
        DraftKey, Event, Intent, follow_up::QueueAction, operations::ReadModelSettings,
    };
    let (mut snapshot, session) = queue_edit_state();
    let first = ModelRef {
        provider: ProviderKind::Codex,
        id: "first".into(),
    };
    let second = ModelRef {
        provider: ProviderKind::Codex,
        id: "second".into(),
    };
    snapshot.catalog.models=Arc::new([(&first,"First"),(&second,"Second")].into_iter().map(|(model,label)|{
        serde_json::from_value::<crate::models::Model>(serde_json::json!({"id":model.id,"model":model,"displayName":label,"settingsDiscovery":true})).unwrap()
    }).collect());
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.input.drafts)
            .get_mut(&session.clone().into())
            .unwrap(),
    )
    .model = Some(first.clone());
    // Queue edits retain the Host's submission settings. The original draft can
    // select another model, so settings projection and discovery must read the editor.
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session)
            .unwrap(),
    )
    .queue
    .inputs[0]
        .model = Some(second.clone());
    let before =
        crate::presentation::view::Presentation::project(Arc::new(snapshot.clone()), "", None)
            .unwrap();
    let (selected, effects) = crate::state::reduce(
        &snapshot,
        "",
        Event::from(Intent::Queue {
            thread_id: session.clone(),
            action: QueueAction::Edit {
                client_user_message_id: "queued".into(),
            },
        }),
    );
    let after = crate::presentation::view::Presentation::project(
        Arc::new(selected.clone()),
        "",
        Some(&before),
    )
    .unwrap();
    let input = after.composer.controls.navigation.draft_key.clone();
    assert_eq!(after.settings.draft_key(), input);
    assert_eq!(
        after.settings.selected_model_name().as_deref(),
        Some("Second")
    );
    assert_eq!(after.pane().navigation.draft_key, DraftKey::from(&session));
    assert_eq!(
        selected.input.drafts[&session.clone().into()].model,
        Some(first)
    );
    let expected = format!("{:?}", ReadModelSettings { model: second });
    assert!(
        format!("{effects:?}").contains(&expected),
        "model discovery must use the selected editor model"
    );
}

#[test]
fn merged_conversation_and_editor_notices_keep_newer_failures_first() {
    use crate::state::{
        follow_up::QueueAction,
        operations::{ErrorId, OperationKey, OperationPhase, OperationState},
    };
    for (own_generation, editor_generation, expected) in [(1, 2, vec![2, 1]), (5, 3, vec![5, 3])] {
        let (snapshot, session) = queue_edit_state();
        let mut editing = edit_queue(
            &snapshot,
            &session,
            QueueAction::Edit {
                client_user_message_id: "queued".into(),
            },
        );
        let key = ComposerPresentation::project(&editing, editing.panes.primary(), None)
            .controls
            .navigation
            .draft_key
            .clone();
        for (key, generation, message) in [
            (
                OperationKey::Attachment {
                    draft_key: session.clone().into(),
                },
                own_generation,
                "Original draft upload failed",
            ),
            (
                OperationKey::Dictation { draft_key: key },
                editor_generation,
                "Queued editor dictation failed",
            ),
        ] {
            Arc::make_mut(&mut editing.operations).insert(
                key,
                OperationState {
                    generation,
                    phase: OperationPhase::Failed {
                        message: message.into(),
                    },
                },
            );
        }
        let context = crate::presentation::context::ConversationContext::project(
            &editing,
            editing.panes.primary(),
            "",
            None,
        );
        let generations = context
            .notices()
            .iter()
            .map(|notice| match notice.id {
                ErrorId::Operation { generation, .. } => generation,
                _ => panic!("fixture contains only operation failures"),
            })
            .collect::<Vec<_>>();
        assert_eq!(generations, expected);
    }
}
