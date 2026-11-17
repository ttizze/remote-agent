//! A confirmation from another Host namespace must never reach the wire,
//! even when session and turn ids have been reused in the new namespace.
use super::*;

#[tokio::test]
async fn retired_namespace_rewind_is_rejected_before_rpc_and_preserves_the_current_draft() {
    let mut initial = snapshot();
    let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut initial.sessions.conversations)
            .get_mut(&session)
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]).status =
        agent_protocol::models::TurnStatus::Completed;
    Arc::make_mut(&mut initial.input.drafts).insert(
        session.clone().into(),
        Arc::new(Draft {
            text: "Keep the newer namespace draft".into(),
            ..Default::default()
        }),
    );
    let (store, mut reader, _writer) = setup(initial).await;
    let retired = "retired-fixture-namespace";
    assert_ne!(store.snapshot().connection.storage_scope, retired);
    let before = store.snapshot().input.drafts.clone();
    // External UI contract, deliberately independent of the request constructor.
    let request: Intent = serde_json::from_value(json!({"RewindSession": {
        "threadId": session, "turnId": "turn", "storageScope": retired,
    }}))
    .unwrap();
    let result = store.dispatch(request);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), reader.read_request())
            .await
            .is_err(),
        "a retired namespace must issue no command RPC"
    );
    assert!(result.await.is_err());
    assert_eq!(store.snapshot().input.drafts, before);
}

/// Pinned chatViewSend::rewindToMessage appends to the live draft after the
/// command, keeps duplicate attachment occurrences, and targets the editor
/// that requested it. This exercises the real Store/command/publication path.
#[tokio::test]
async fn recall_appends_to_the_live_original_editor_and_does_not_resurrect_a_retired_queued_editor()
{
    use agent_core::state::{Attachment, Navigation, follow_up::QueueAction};
    use agent_protocol::{
        models::{Item, ItemBody, ItemStatus, MessagePart, Turn, TurnStatus},
        operations::{FollowUp, Input, InputQueue, Submission},
    };
    for queued in [false, true] {
        for retire in [false, true] {
            if retire && !queued {
                continue;
            }
            let session = SessionRef::new(ProviderKind::Codex, "recall".into()).unwrap();
            let photo = Attachment {
                path: "/fixture/photo.png".into(),
                name: "photo.png".into(),
                is_image: true,
            };
            let normal = Arc::new(Draft {
                text: "Keep normal draft".into(),
                attachments: vec![photo.clone()],
                ..Default::default()
            });
            let mut initial = Snapshot::default();
            Arc::make_mut(initial.panes.primary_mut()).navigation = Arc::new(Navigation {
                thread_id: Some(session.clone()),
                draft_key: session.clone().into(),
                cwd: "/fixture".into(),
            });
            Arc::make_mut(&mut initial.input.drafts).insert(session.clone().into(), normal.clone());
            let queue = if queued {
                InputQueue {
                    held: true,
                    inputs: vec![Submission {
                        thread_id: session.clone(),
                        client_user_message_id: "queued".into(),
                        fresh_context: false,
                        input: vec![
                            Input::Text {
                                text: "Queued original".into(),
                            },
                            Input::LocalImage {
                                path: photo.path.clone(),
                            },
                        ],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: FollowUp::Queue,
                    }],
                }
            } else {
                InputQueue::default()
            };
            Arc::make_mut(&mut initial.sessions.conversations).insert(
                session.clone(),
                Arc::new(Thread {
                    id: Some(session.clone()),
                    cwd: Some("/fixture".into()),
                    active_provider: Some(ProviderKind::Codex),
                    queue,
                    turns: Some(vec![Arc::new(Turn {
                        id: "turn".into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![Arc::new(Item::new(
                            "sent".into(),
                            ItemStatus::Completed,
                            ItemBody::UserMessage {
                                text: Some("Historical message".into()),
                                content: vec![
                                    MessagePart::Image {
                                        source: photo.path.clone(),
                                    },
                                    MessagePart::Image {
                                        source: photo.path.clone(),
                                    },
                                ],
                            },
                        ))]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                }),
            );
            let (store, mut reader, writer) = setup(initial).await;
            if queued {
                store
                    .dispatch(Intent::Queue {
                        thread_id: session.clone(),
                        action: QueueAction::Edit {
                            client_user_message_id: "queued".into(),
                        },
                    })
                    .await
                    .unwrap();
            }
            let target = store
                .presentation()
                .composer
                .controls()
                .navigation
                .draft_key;
            assert_eq!(matches!(target, DraftKey::Queued { .. }), queued);
            let scope = store.snapshot().connection.storage_scope.clone();
            let command: Intent = serde_json::from_value(json!({"RewindSession": { "threadId": session, "turnId": "turn", "storageScope": scope }})).unwrap();
            let mut base = rewind_initial_projection(
                (*store.snapshot().sessions.projections[&session]).clone(),
            );
            base.projection.provider_threads[0].last_run_ordinal = Some(1);
            let result = store.dispatch(command);
            let (request, scoped) = rewind_request(&mut reader, &writer, &session, &base).await;
            assert_eq!(request["method"], "host/conversation/change");
            assert_eq!(request["params"]["session"], json!(session));
            assert_eq!(request["params"]["command"]["Rewind"], "turn");
            let live_target = if retire {
                store
                    .dispatch(Intent::Queue {
                        thread_id: session.clone(),
                        action: QueueAction::CancelEdit,
                    })
                    .await
                    .unwrap();
                assert!(!store.snapshot().input.drafts.contains_key(&target));
                store
                    .dispatch(Intent::Queue {
                        thread_id: session.clone(),
                        action: QueueAction::Edit {
                            client_user_message_id: "queued".into(),
                        },
                    })
                    .await
                    .unwrap();
                let next = store
                    .presentation()
                    .composer
                    .controls()
                    .navigation
                    .draft_key;
                assert_ne!(next, target);
                next
            } else {
                target.clone()
            };
            store
                .dispatch(Intent::SetDraftText {
                    thread_id: live_target.clone(),
                    text: "Typed while waiting ".into(),
                })
                .await
                .unwrap();
            let before = store.snapshot();
            let unchanged_live_attachments = before.input.drafts[&live_target].attachments.clone();
            // The fixture Host stores sent images by immutable attachment ID;
            // editor-local paths remain local until submitted.
            let recalled_photo = Attachment {
                path: "bex-attachment://fixture-f054e932-e991-532a-9a8b-b2e180a52d39-png.png"
                    .into(),
                ..photo.clone()
            };
            let unchanged_normal = before.input.drafts[&DraftKey::from(&session)].clone();
            let mut cut = base.clone();
            cut.snapshot_sequence += 2;
            cut.projection.thread.rollback_request_id =
                request["params"]["operation"].as_str().map(str::to_owned);
            cut.projection.provider_threads[0].last_run_ordinal = None;
            for run in &mut cut.projection.runs {
                if run.id == "turn" {
                    run.status = agent_protocol::orchestration::RunStatus::RolledBack;
                }
            }
            cut.projection
                .visible_turn_items
                .retain(|row| row.item.run_id.as_deref() != Some("turn"));
            let active = before.sessions.subscriptions.get(&session).copied();
            rewind_frame(&writer, active, scoped, cut).await;
            writer.reply(&request, json!({"result":{"sequence":base.snapshot_sequence+1,"accepted_at":"2026-11-14T00:00:00.000Z","error":null}})).await.unwrap();
            result.await.unwrap();
            let after = store.snapshot();
            if retire {
                assert!(
                    !after.input.drafts.contains_key(&target),
                    "a retired queued editor stays retired"
                );
                assert_eq!(
                    after.input.drafts[&live_target].text,
                    "Typed while waiting "
                );
                assert_eq!(
                    after.input.drafts[&live_target].attachments,
                    unchanged_live_attachments
                );
            } else {
                assert_eq!(
                    after.input.drafts[&target].text,
                    "Typed while waiting \n\nHistorical message"
                );
                let mut expected_attachments = unchanged_live_attachments;
                expected_attachments.extend([recalled_photo.clone(), recalled_photo]);
                assert_eq!(
                    after.input.drafts[&target].attachments,
                    expected_attachments
                );
            }
            if queued {
                assert_eq!(
                    after.input.drafts[&DraftKey::from(&session)],
                    unchanged_normal
                );
            }
            store.close().await.unwrap();
        }
    }
}
