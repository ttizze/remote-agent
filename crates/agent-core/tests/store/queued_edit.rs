//! Real Core Store/RPC integration for queued input editing. The scripted peer
//! receives the actual durable command; it does not replace the save path.
use super::*;
use agent_core::state::{Attachment, Navigation, follow_up::QueueAction};
use agent_protocol::operations::{FollowUp, Input, InputQueue, Submission};

#[tokio::test]
async fn failed_queued_saves_keep_the_editor_for_retry_and_success_restores_the_original_draft() {
    let session = SessionRef::new(ProviderKind::Codex, "queued-save".into()).unwrap();
    let original = Arc::new(Draft {
        text: "My unsent draft".into(),
        // The connection fixture opens with this canonical model selection.
        model: Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Codex,
            id: "default".into(),
        }),
        attachments: vec![Attachment {
            path: "/host/original.txt".into(),
            name: "original.txt".into(),
            is_image: false,
        }],
        ..Default::default()
    });
    let mut initial = Snapshot::default();
    Arc::make_mut(initial.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: Some(session.clone()),
        draft_key: session.clone().into(),
        cwd: String::new(),
    });
    Arc::make_mut(&mut initial.input.drafts).insert(session.clone().into(), original.clone());
    Arc::make_mut(&mut initial.sessions.conversations).insert(
        session.clone(),
        Arc::new(Thread {
            id: Some(session.clone()),
            active_provider: Some(ProviderKind::Codex),
            queue: InputQueue {
                held: true,
                inputs: vec![Submission {
                    fresh_context: true,
                    thread_id: session.clone(),
                    client_user_message_id: "queued".into(),
                    input: vec![
                        Input::Text {
                            text: "Before edit".into(),
                        },
                        Input::LocalImage {
                            path: "/host/queued.png".into(),
                        },
                        Input::Skill {
                            name: "keep".into(),
                            path: "/skills/keep/SKILL.md".into(),
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
    let (store, mut reader, mut writer) = setup(initial).await;
    store
        .dispatch(Intent::Queue {
            thread_id: session.clone(),
            action: QueueAction::Edit {
                client_user_message_id: "queued".into(),
            },
        })
        .await
        .unwrap();
    let key = store
        .presentation()
        .composer
        .controls()
        .navigation
        .draft_key;
    assert_ne!(key, DraftKey::from(&session));
    assert!(store.presentation().composer.draft().fresh_context);
    store
        .dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: "Edited without the image".into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::RemoveAttachment {
            draft_key: key.clone(),
            index: 0,
        })
        .await
        .unwrap();
    for outcome in ["rpc failure", "rejected", "accepted"] {
        let saving = store.dispatch(
            store
                .presentation()
                .composer
                .submit_intent("unused".into(), false),
        );
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(request["method"], "host/conversation/change");
        assert_eq!(request["params"]["session"], json!(session));
        // Canonical skills retain their name; the Host resolves the current
        // catalog path rather than storing a device's former path.
        assert_eq!(
            request["params"]["command"],
            json!({"EditInput":{"id":"queued","parts":[{"text":{"text":"Edited without the image"}}, {"skill":{"name":"keep","path":""}}]}})
        );
        assert_eq!(
            store.snapshot().input.drafts[&session.clone().into()],
            original
        );
        assert_eq!(
            store.presentation().composer.draft().text,
            "Edited without the image"
        );
        assert!(!store.presentation().composer.can_send(true));
        let duplicate = tokio::time::timeout(
            Duration::from_secs(1),
            store.dispatch(Intent::SaveQueuedEdit {
                draft_key: key.clone(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(duplicate, Outcome::Applied);
        let reply = if outcome == "rpc failure" {
            json!({"error":{"code":"request_failed","message":"Try again","delivery":"notSent"}})
        } else {
            json!({"result":agent_protocol::conversation::records::Acknowledged {
                sequence:50,
                accepted_at:"2026-11-17T00:00:00Z".into(),
                error:(outcome!="accepted").then(|| "input is no longer queued".into()),
            }})
        };
        writer.reply(&request, reply).await.unwrap();
        if outcome == "accepted" {
            saving.await.unwrap();
        } else {
            assert!(saving.await.is_err());
            assert_eq!(
                store
                    .presentation()
                    .composer
                    .controls()
                    .navigation
                    .draft_key,
                key
            );
            assert_eq!(
                store.presentation().composer.draft().text,
                "Edited without the image"
            );
            assert!(store.presentation().composer.draft().attachments.is_empty());
            assert!(store.presentation().composer.draft().fresh_context);
        }
    }
    assert!(store.snapshot().input.queued_edits.is_empty());
    assert!(!store.snapshot().input.drafts.contains_key(&key));
    assert_eq!(store.presentation().composer.draft(), *original);
    assert!(store.snapshot().sessions.pending_submissions.is_empty());
    store
        .dispatch(Intent::SaveQueuedEdit { draft_key: key })
        .await
        .unwrap();
    assert_eq!(store.snapshot().input.drafts[&session.into()], original);
    store.close().await.unwrap();
}
