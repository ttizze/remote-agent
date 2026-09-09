use agent_core::{
    client::Answer,
    models::Thread,
    peer::{PeerError, RpcPeer},
    state::{Draft, Intent, Snapshot},
    store::{Outcome, Store},
};
use host_protocol::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn setup(
    snapshot: Snapshot,
) -> (
    Arc<Store>,
    JsonlReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    JsonlWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
) {
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let peer = RpcPeer::open(JsonlReader::new(read), write, Duration::from_secs(1), 16).unwrap();
    let (read, write) = tokio::io::split(server);
    (
        Arc::new(Store::new(peer, snapshot)),
        JsonlReader::new(read),
        JsonlWriter::new(write),
    )
}
async fn read(reader: &mut JsonlReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(2), reader.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn wait_for(store: &Store, predicate: impl Fn(&Snapshot) -> bool) {
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if predicate(&updates.borrow_and_update()) {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
fn thread(text: &str) -> Thread {
    serde_json::from_value(json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[{"id":"turn","status":"inProgress","items":[{"id":"item","type":"agentMessage","text":text}]}]})).unwrap()
}
fn snapshot() -> Snapshot {
    Snapshot {
        conversations: Arc::new(BTreeMap::from([("thread".into(), Arc::new(thread("old")))])),
        ..Default::default()
    }
}
fn loaded_text(snapshot: &Snapshot) -> Option<&str> {
    snapshot
        .conversations
        .get("thread")?
        .turns
        .as_ref()?
        .first()?
        .items
        .as_ref()?
        .first()?
        .text
        .as_deref()
}

#[tokio::test]
async fn intent_order_is_independent_of_receipt_polling_order() {
    let (store, _, _writer) = setup(Snapshot::default());
    let first = store.dispatch(Intent::SetDraft {
        thread_id: "thread".into(),
        draft: Draft {
            text: "first".into(),
            ..Default::default()
        },
    });
    let second = store.dispatch(Intent::SetDraft {
        thread_id: "thread".into(),
        draft: Draft {
            text: "second".into(),
            ..Default::default()
        },
    });
    second.await.unwrap();
    first.await.unwrap();
    assert_eq!(store.snapshot().drafts["thread"].text, "second");
    drop(store.dispatch(Intent::SetDraft {
        thread_id: "thread".into(),
        draft: Draft {
            text: "last".into(),
            ..Default::default()
        },
    }));
    wait_for(&store, |state| state.drafts["thread"].text == "last").await;
    store.close().await.unwrap();
}
#[tokio::test]
async fn draft_field_edits_preserve_interleaved_attachments_and_settings() {
    use agent_core::state::Attachment;
    for attachment_first in [false, true] {
        let previous = Snapshot {
            models: Arc::new(serde_json::from_value(json!([{
                "id":"model", "model":"model", "displayName":"Model",
                "defaultReasoningEffort":"medium", "defaultServiceTier":"priority",
                "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"max"}],
                "serviceTiers":[{"id":"priority"}]
            }])).unwrap()),
            drafts: Arc::new(BTreeMap::from([("thread".into(), Arc::new(Draft {
                text: "old".into(), model: Some("model".into()), effort: Some("medium".into()),
                service_tier: Some("priority".into()), ..Default::default()
            }))])),
            ..Default::default()
        };
        let (store, _reader, _writer) = setup(previous.clone());
        let attachment = Intent::AddAttachment {
            draft_key: "thread".into(),
            attachment: Attachment {
                path: "/fixture/image.png".into(),
                name: "image.png".into(),
                is_image: true,
            },
        };
        let text = Intent::SetDraftText {
            thread_id: "thread".into(),
            text: "new".into(),
        };
        let intents = if attachment_first {
            [attachment, text]
        } else {
            [text, attachment]
        };
        for intent in intents {
            drop(store.dispatch(intent));
        }
        drop(store.dispatch(Intent::SelectEffort {
            thread_id: "thread".into(),
            effort: "max".into(),
        }));
        store
            .dispatch(Intent::SelectServiceTier {
                thread_id: "thread".into(),
                service_tier: "default".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        let draft = &current.drafts["thread"];
        assert_eq!(draft.text, "new");
        assert_eq!(draft.attachments[0].path, "/fixture/image.png");
        assert_eq!(draft.model.as_deref(), Some("model"));
        assert_eq!(draft.effort.as_deref(), Some("max"));
        assert_eq!(draft.service_tier.as_deref(), Some("default"));
        assert_eq!(previous.drafts["thread"].text, "old");
        assert!(previous.drafts["thread"].attachments.is_empty());
        store
            .dispatch(Intent::SelectEffort {
                thread_id: "thread".into(),
                effort: "invalid".into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectServiceTier {
                thread_id: "thread".into(),
                service_tier: "invalid".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        assert_eq!(current.drafts["thread"].effort.as_deref(), Some("medium"));
        assert_eq!(
            current.drafts["thread"].service_tier.as_deref(),
            Some("priority")
        );
        assert_eq!(current.drafts["thread"].text, "new");
        assert_eq!(current.drafts["thread"].attachments.len(), 1);
        store.close().await.unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_response_precedes_following_delta_even_when_server_closes() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" obsolete"}}).to_string()).await.unwrap();
        writer
            .write_line(&json!({"id":request["id"],"result":{"thread":thread("base")}}).to_string())
            .await
            .unwrap();
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" tail"}}).to_string()).await.unwrap();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        store.dispatch(Intent::ReadThread("thread".into())),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap(), Outcome::Applied);
    server.await.unwrap();
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("base tail")
    })
    .await;
}
#[tokio::test]
async fn approval_can_complete_while_another_request_is_waiting() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let pending = read(&mut reader).await;
        writer.write_line(&json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"thread"}}).to_string()).await.unwrap();
        let answer = read(&mut reader).await;
        assert_eq!(
            answer,
            json!({"id":"approval","result":{"decision":"decline"}})
        );
        writer
            .write_line(
                &json!({"id":pending["id"],"result":{"thread":thread("approved path")}})
                    .to_string(),
            )
            .await
            .unwrap();
        writer
    });
    let loading = tokio::spawn({
        let store = store.clone();
        async move { store.dispatch(Intent::ReadThread("thread".into())).await }
    });
    wait_for(&store, |snapshot| {
        snapshot.requests.contains_key("\"approval\"")
    })
    .await;
    store
        .dispatch(Intent::Respond {
            request_id: json!("approval"),
            answer: Answer::Decision(2),
        })
        .await
        .unwrap();
    assert_eq!(loading.await.unwrap().unwrap(), Outcome::Applied);
    let _writer = server.await.unwrap();
    assert!(store.snapshot().requests.is_empty());
    assert_eq!(loaded_text(&store.snapshot()), Some("approved path"));
}
#[tokio::test]
async fn invalid_typed_reply_does_not_block_later_wire_events() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":{"cwd":"/fixture"}}}).to_string(),
            )
            .await
            .unwrap();
        let request = read(&mut reader).await;
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":thread("recovered")}}).to_string(),
            )
            .await
            .unwrap();
        writer
    });
    let error = store
        .dispatch(Intent::ReadThread("thread".into()))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PeerError::InvalidResponse {
            sequence: Some(_),
            ..
        }
    ));
    store
        .dispatch(Intent::ReadThread("thread".into()))
        .await
        .unwrap();
    let _writer = server.await.unwrap();
    assert_eq!(loaded_text(&store.snapshot()), Some("recovered"));
}
#[tokio::test]
async fn successful_submission_does_not_erase_a_newer_draft() {
    let mut initial = snapshot();
    Arc::make_mut(&mut initial.conversations).insert(
        "thread".into(),
        Arc::new(
            serde_json::from_value(
                json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]}),
            )
            .unwrap(),
        ),
    );
    let (store, mut reader, mut writer) = setup(initial);
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: Draft {
                text: "first".into(),
                service_tier: Some("priority".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let sending = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::Submit {
                    thread_id: Some("thread".into()),
                    client_user_message_id: "fixture-message".into(),
                })
                .await
        }
    });
    let request = tokio::time::timeout(Duration::from_secs(2), read(&mut reader))
        .await
        .unwrap();
    assert_eq!(request["params"]["input"][0]["text"], "first");
    assert_eq!(request["method"], "turn/start");
    assert_eq!(request["params"]["serviceTierForTurn"], "priority");
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: Draft {
                text: "second".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    writer
        .write_line(&json!({"id":request["id"],"result":{"turn":{"id":"turn-new"}}}).to_string())
        .await
        .unwrap();
    assert_eq!(
        sending.await.unwrap().unwrap(),
        Outcome::Submitted(Some("turn-new".into()))
    );
    assert_eq!(store.snapshot().drafts["thread"].text, "second");
}

#[tokio::test]
async fn new_submission_keeps_edits_and_navigation_while_creation_is_pending() {
    for navigate_away in [false, true] {
        let (store, mut reader, mut writer) = setup(Snapshot::default());
        store
            .dispatch(Intent::NewChat("/fixture".into()))
            .await
            .unwrap();
        store
            .dispatch(Intent::SetDraft {
                thread_id: "new:/fixture".into(),
                draft: Draft {
                    text: "sent".into(),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        let sending = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .dispatch(Intent::Submit {
                        thread_id: None,
                        client_user_message_id: "client".into(),
                    })
                    .await
            }
        });
        let create = read(&mut reader).await;
        assert_eq!(create["method"], "host/thread/start");
        assert_eq!(create["params"]["cwd"], "/fixture");
        store
            .dispatch(Intent::SetDraft {
                thread_id: "new:/fixture".into(),
                draft: Draft {
                    text: "newer".into(),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        if navigate_away {
            store
                .dispatch(Intent::NewChat("/other".into()))
                .await
                .unwrap();
        }
        writer.write_line(&json!({"id":create["id"],"result":{"thread":{"id":"created","cwd":"/fixture","turns":[],"status":{"type":"idle"}}}}).to_string()).await.unwrap();
        let submit = read(&mut reader).await;
        assert_eq!(submit["method"], "turn/start");
        assert_eq!(submit["params"]["threadId"], "created");
        assert_eq!(submit["params"]["input"][0]["text"], "sent");
        assert!(
            !sending.is_finished(),
            "dispatch must wait for submission, not just thread creation"
        );
        writer
            .write_line(&json!({"id":submit["id"],"result":{"turn":{"id":"turn"}}}).to_string())
            .await
            .unwrap();
        assert_eq!(
            sending.await.unwrap().unwrap(),
            Outcome::Submitted(Some("turn".into()))
        );
        let state = store.snapshot();
        if navigate_away {
            assert_eq!(state.navigation.cwd, "/other");
            assert_eq!(state.navigation.thread_id, None);
            assert_eq!(state.drafts["new:/fixture"].text, "newer");
            assert!(state.drafts["created"].text.is_empty());
        } else {
            assert_eq!(state.navigation.thread_id.as_deref(), Some("created"));
            assert_eq!(state.drafts["created"].text, "newer");
            assert!(!state.drafts.contains_key("new:/fixture"));
        }
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_new_submission_keeps_retry_at_the_last_successful_step() {
    for fail_creation in [false, true] {
        let (store, mut reader, mut writer) = setup(Snapshot::default());
        store
            .dispatch(Intent::NewChat("/fixture".into()))
            .await
            .unwrap();
        store
            .dispatch(Intent::SetDraft {
                thread_id: "new:/fixture".into(),
                draft: Draft {
                    text: "retry me".into(),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        let sending = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .dispatch(Intent::Submit {
                        thread_id: None,
                        client_user_message_id: "client".into(),
                    })
                    .await
            }
        });
        let create = read(&mut reader).await;
        assert_eq!(create["method"], "host/thread/start");
        let failed = if fail_creation {
            create
        } else {
            writer.write_line(&json!({"id":create["id"],"result":{"thread":{"id":"created","cwd":"/fixture","turns":[],"status":{"type":"idle"}}}}).to_string()).await.unwrap();
            let submit = read(&mut reader).await;
            assert_eq!(submit["method"], "turn/start");
            submit
        };
        writer
            .write_line(
                &json!({"id":failed["id"],"error":{"code":-32000,"message":"fixture failure"}})
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(matches!(
            sending.await.unwrap(),
            Err(PeerError::Remote { .. })
        ));
        let state = store.snapshot();
        assert!(state.pending_submissions.is_empty());
        assert_eq!(state.drafts[&state.navigation.draft_key].text, "retry me");
        let retry = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .dispatch(Intent::Submit {
                        thread_id: None,
                        client_user_message_id: "retry".into(),
                    })
                    .await
            }
        });
        let request = read(&mut reader).await;
        assert_eq!(
            request["method"],
            if fail_creation {
                "host/thread/start"
            } else {
                "turn/start"
            }
        );
        if !fail_creation {
            assert_eq!(request["params"]["threadId"], "created");
        }
        writer
            .write_line(
                &json!({"id":request["id"],"error":{"code":-32000,"message":"end fixture"}})
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(retry.await.unwrap().is_err());
        store.close().await.unwrap();
    }
}

fn file(path: &str, revision: &str, text: &str) -> Value {
    json!({"path":path,"revision":revision,"text":text,"bom":false,"lineEnding":"lf","size":text.len()})
}

#[tokio::test]
async fn transcription_preserves_newer_input_and_restores_audio_text_on_send_failure() {
    for fail_send in [false, true] {
        let mut initial = snapshot();
        Arc::make_mut(
            &mut Arc::make_mut(
                Arc::make_mut(&mut initial.conversations)
                    .get_mut("thread")
                    .unwrap(),
            )
            .turns
            .as_mut()
            .unwrap()[0],
        )
        .status = Some("completed".into());
        Arc::make_mut(&mut initial.navigation).thread_id = Some("thread".into());
        Arc::make_mut(&mut initial.navigation).draft_key = "thread".into();
        Arc::make_mut(&mut initial.activity)
            .active
            .insert("thread".into(), false);
        Arc::make_mut(&mut initial.drafts).insert(
            "thread".into(),
            Arc::new(Draft {
                text: "original".into(),
                ..Default::default()
            }),
        );
        let (store, mut reader, mut writer) = setup(initial);
        let transcribing = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .dispatch(Intent::Transcribe {
                        draft_key: "thread".into(),
                        audio: "AAA=".into(),
                        send: true,
                        client_user_message_id: "dictation".into(),
                    })
                    .await
            }
        });
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/transcribe");
        store
            .dispatch(Intent::SetDraft {
                thread_id: "thread".into(),
                draft: Draft {
                    text: "newer".into(),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        writer
            .write_line(&json!({"id":request["id"],"result":{"text":"spoken"}}).to_string())
            .await
            .unwrap();
        let submit = read(&mut reader).await;
        assert_eq!(submit["method"], "turn/start");
        assert_eq!(submit["params"]["input"][0]["text"], "original\nspoken");
        assert!(!transcribing.is_finished());
        let reply = if fail_send {
            json!({"id":submit["id"],"error":{"code":-32000,"message":"send failed"}})
        } else {
            json!({"id":submit["id"],"result":{"turn":{"id":"next"}}})
        };
        writer.write_line(&reply.to_string()).await.unwrap();
        assert_eq!(transcribing.await.unwrap().is_err(), fail_send);
        assert_eq!(
            store.snapshot().drafts["thread"].text,
            if fail_send { "newer\nspoken" } else { "newer" }
        );
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn navigation_cancels_dictation_send_but_keeps_the_transcript_in_its_draft() {
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    store
        .dispatch(Intent::NewChat("/fixture".into()))
        .await
        .unwrap();
    let transcribing = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::Transcribe {
                    draft_key: "new:/fixture".into(),
                    audio: "AAA=".into(),
                    send: true,
                    client_user_message_id: "dictation".into(),
                })
                .await
        }
    });
    let request = read(&mut reader).await;
    store
        .dispatch(Intent::NewChat("/other".into()))
        .await
        .unwrap();
    store
        .dispatch(Intent::NewChat("/fixture".into()))
        .await
        .unwrap();
    writer
        .write_line(&json!({"id":request["id"],"result":{"text":"spoken"}}).to_string())
        .await
        .unwrap();
    assert_eq!(transcribing.await.unwrap().unwrap(), Outcome::Applied);
    assert_eq!(store.snapshot().drafts["new:/fixture"].text, "spoken");
    assert!(store.snapshot().pending_submissions.is_empty());
    store.close().await.unwrap();
}

#[tokio::test]
async fn file_navigation_ignores_a_late_reply_from_the_previous_file() {
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let first = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadFile {
                    path: "/first".into(),
                    discard_draft: false,
                })
                .await
        }
    });
    let first_request = read(&mut reader).await;
    let second = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadFile {
                    path: "/second".into(),
                    discard_draft: false,
                })
                .await
        }
    });
    let second_request = read(&mut reader).await;
    writer
        .write_line(
            &json!({"id":second_request["id"],"result":file("/second", "2", "second")}).to_string(),
        )
        .await
        .unwrap();
    second.await.unwrap().unwrap();
    writer
        .write_line(
            &json!({"id":first_request["id"],"result":file("/first", "1", "first")}).to_string(),
        )
        .await
        .unwrap();
    first.await.unwrap().unwrap();
    assert_eq!(
        store.snapshot().workspace.file.as_ref().unwrap().path,
        "/second"
    );
}

#[tokio::test]
async fn saving_keeps_newer_edits_and_advances_their_revision_for_the_next_save() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.workspace).file = Some(Arc::new(
        serde_json::from_value(file("/file", "base", "old")).unwrap(),
    ));
    let (store, mut reader, mut writer) = setup(initial);
    store
        .dispatch(Intent::SetFileDraft {
            path: "/file".into(),
            text: "submitted".into(),
        })
        .await
        .unwrap();
    let saving = tokio::spawn({
        let store = store.clone();
        async move { store.dispatch(Intent::SaveFile("/file".into())).await }
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/file/write");
    assert_eq!(
        request["params"],
        json!({"path":"/file","revision":"base","text":"submitted"})
    );
    store
        .dispatch(Intent::SetFileDraft {
            path: "/file".into(),
            text: "newer".into(),
        })
        .await
        .unwrap();
    writer
        .write_line(
            &json!({"id":request["id"],"result":file("/file", "saved", "submitted")}).to_string(),
        )
        .await
        .unwrap();
    saving.await.unwrap().unwrap();
    assert_eq!(store.snapshot().file_drafts["/file"].text, "newer");
    assert_eq!(store.snapshot().file_drafts["/file"].revision, "saved");
    let saving = tokio::spawn({
        let store = store.clone();
        async move { store.dispatch(Intent::SaveFile("/file".into())).await }
    });
    let request = read(&mut reader).await;
    assert_eq!(
        request["params"],
        json!({"path":"/file","revision":"saved","text":"newer"})
    );
    writer
        .write_line(
            &json!({"id":request["id"],"result":file("/file", "saved-again", "newer")}).to_string(),
        )
        .await
        .unwrap();
    saving.await.unwrap().unwrap();
    assert!(store.snapshot().file_drafts.is_empty());
    assert_eq!(
        store.snapshot().workspace.file.as_ref().unwrap().text,
        "newer"
    );
}

#[tokio::test]
async fn a_late_open_reply_caches_the_thread_without_leaving_a_new_chat() {
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let opening = tokio::spawn({
        let store = store.clone();
        async move { store.dispatch(Intent::OpenThread("thread".into())).await }
    });
    let request = read(&mut reader).await;
    store
        .dispatch(Intent::NewChat("/new-project".into()))
        .await
        .unwrap();
    writer
        .write_line(&json!({"id":request["id"],"result":{"thread":thread("loaded")}}).to_string())
        .await
        .unwrap();
    opening.await.unwrap().unwrap();
    let snapshot = store.snapshot();
    assert!(snapshot.navigation.thread_id.is_none());
    assert_eq!(snapshot.navigation.cwd, "/new-project");
    assert_eq!(snapshot.navigation.draft_key, "new:/new-project");
    assert_eq!(loaded_text(&snapshot), Some("loaded"));
}

#[tokio::test]
async fn a_stale_catalogue_does_not_queue_a_completed_thread() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.conversations).insert(
        "thread".into(),
        Arc::new(
            serde_json::from_value(
                json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]}),
            )
            .unwrap(),
        ),
    );
    initial.threads = Some(Arc::new(serde_json::from_value(json!({"data":[{"id":"thread","cwd":"/fixture","status":{"type":"active"}}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})).unwrap()));
    let (store, mut reader, mut writer) = setup(initial);
    writer.write_line(&json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"completed","status":"completed"}}}).to_string()).await.unwrap();
    wait_for(&store, |snapshot| {
        snapshot.activity.active.get("thread") == Some(&false)
    })
    .await;
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: Draft {
                text: "next turn".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let sending = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::Submit {
                    thread_id: Some("thread".into()),
                    client_user_message_id: "next-message".into(),
                })
                .await
        }
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "turn/start");
    writer
        .write_line(&json!({"id":request["id"],"result":{"turn":{"id":"new-turn"}}}).to_string())
        .await
        .unwrap();
    assert_eq!(
        sending.await.unwrap().unwrap(),
        Outcome::Submitted(Some("new-turn".into()))
    );
}

#[tokio::test]
async fn a_late_list_reply_cannot_replace_a_new_search() {
    use agent_core::models::ListQuery;
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let old = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ListThreads(ListQuery {
                    search_term: "old".into(),
                    ..Default::default()
                }))
                .await
        }
    });
    let old_request = read(&mut reader).await;
    let new = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ListThreads(ListQuery {
                    search_term: "new".into(),
                    ..Default::default()
                }))
                .await
        }
    });
    let new_request = read(&mut reader).await;
    let result = |id| json!({"data":[{"id":id}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false});
    writer
        .write_line(&json!({"id":new_request["id"],"result":result("new")}).to_string())
        .await
        .unwrap();
    new.await.unwrap().unwrap();
    writer
        .write_line(&json!({"id":old_request["id"],"result":result("old")}).to_string())
        .await
        .unwrap();
    old.await.unwrap().unwrap();
    assert_eq!(
        store.snapshot().threads.as_ref().unwrap().data[0]
            .id
            .as_deref(),
        Some("new")
    );
    assert_eq!(store.snapshot().list_query.search_term, "new");
}

#[tokio::test]
async fn gallery_history_reads_do_not_block_conversation_notifications() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/thread/read");
        writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"gallery","turns":[{"id":"image-turn","items":[{"id":"image","type":"imageGeneration","savedPath":"/image.png"}]}]}}}).to_string()).await.unwrap();
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" continued"}}).to_string()).await.unwrap();
        writer
    });
    let result = store
        .dispatch(Intent::LoadSessionImages("gallery".into()))
        .await
        .unwrap();
    let Outcome::SessionImages(images) = result else {
        panic!("expected images")
    };
    assert_eq!(images[0].source, "/image.png");
    let _writer = server.await.unwrap();
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("old continued")
    })
    .await;
}

#[tokio::test]
async fn terminal_preserves_output_until_acknowledged_and_serializes_input() {
    use agent_core::{client::TerminalSize, state::TerminalPhase};
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let server = tokio::spawn(async move {
        let start = read(&mut reader).await;
        assert_eq!(start["method"], "host/terminal/start");
        assert_eq!(start["params"]["cwd"], "/fixture");
        for data in ["YQ==", "Yg=="] {
            writer.write_line(&json!({"method":"process/outputDelta","params":{"processHandle":"terminal","stream":"stdout","deltaBase64":data,"capReached":false}}).to_string()).await.unwrap();
        }
        writer
            .write_line(&json!({"id":start["id"],"result":{}}).to_string())
            .await
            .unwrap();
        for data in ["Zmlyc3Q=", "c2Vjb25k"] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "process/writeStdin");
            assert_eq!(request["params"]["deltaBase64"], data);
            writer
                .write_line(&json!({"id":request["id"],"result":{}}).to_string())
                .await
                .unwrap();
        }
        let close = read(&mut reader).await;
        assert_eq!(close["method"], "process/kill");
        writer
            .write_line(&json!({"id":close["id"],"result":{}}).to_string())
            .await
            .unwrap();
        // Keep the transport alive until Store closes it.
        assert!(reader.read_line().await.unwrap().is_none());
    });
    let start = store.dispatch(Intent::StartTerminal {
        handle: "terminal".into(),
        cwd: "/fixture".into(),
        size: TerminalSize { cols: 80, rows: 24 },
    });
    let first = store.dispatch(Intent::WriteTerminal {
        handle: "terminal".into(),
        data: b"first".to_vec(),
    });
    let second = store.dispatch(Intent::WriteTerminal {
        handle: "terminal".into(),
        data: b"second".to_vec(),
    });
    second.await.unwrap();
    first.await.unwrap();
    start.await.unwrap();
    let retained = store.snapshot();
    assert_eq!(retained.terminals["terminal"].phase, TerminalPhase::Running);
    assert_eq!(
        retained.terminals["terminal"]
            .output
            .iter()
            .map(|chunk| chunk.data.as_str())
            .collect::<Vec<_>>(),
        ["YQ==", "Yg=="]
    );
    store
        .dispatch(Intent::AcknowledgeTerminal {
            handle: "terminal".into(),
            sequence: 1,
        })
        .await
        .unwrap();
    assert_eq!(store.snapshot().terminals["terminal"].output.len(), 1);
    assert_eq!(store.snapshot().terminals["terminal"].output[0].sequence, 2);
    assert_eq!(retained.terminals["terminal"].output.len(), 2);
    store
        .dispatch(Intent::AcknowledgeTerminal {
            handle: "terminal".into(),
            sequence: 2,
        })
        .await
        .unwrap();
    assert!(store.snapshot().terminals["terminal"].output.is_empty());
    store
        .dispatch(Intent::CloseTerminal("terminal".into()))
        .await
        .unwrap();
    assert_eq!(
        store.snapshot().terminals["terminal"].phase,
        TerminalPhase::Closed
    );
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn terminal_exit_before_spawn_reply_is_not_replaced_by_running() {
    use agent_core::{client::TerminalSize, state::TerminalPhase};
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer.write_line(&json!({"method":"process/exited","params":{"processHandle":"terminal","exitCode":17,"stdout":"","stderr":"","stdoutCapReached":false,"stderrCapReached":false}}).to_string()).await.unwrap();
        writer
            .write_line(&json!({"id":request["id"],"result":{}}).to_string())
            .await
            .unwrap();
        assert!(reader.read_line().await.unwrap().is_none());
    });
    store
        .dispatch(Intent::StartTerminal {
            handle: "terminal".into(),
            cwd: "/fixture".into(),
            size: TerminalSize { cols: 80, rows: 24 },
        })
        .await
        .unwrap();
    assert_eq!(
        store.snapshot().terminals["terminal"].phase,
        TerminalPhase::Exited(17)
    );
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn closing_store_terminates_its_running_terminal() {
    use agent_core::client::TerminalSize;
    let (store, mut reader, mut writer) = setup(Snapshot::default());
    let server = tokio::spawn(async move {
        let start = read(&mut reader).await;
        writer
            .write_line(&json!({"id":start["id"],"result":{}}).to_string())
            .await
            .unwrap();
        let kill = read(&mut reader).await;
        assert_eq!(kill["method"], "process/kill");
        assert_eq!(kill["params"]["processHandle"], "terminal");
        writer
            .write_line(&json!({"id":kill["id"],"result":{}}).to_string())
            .await
            .unwrap();
        assert!(reader.read_line().await.unwrap().is_none());
    });
    store
        .dispatch(Intent::StartTerminal {
            handle: "terminal".into(),
            cwd: "/fixture".into(),
            size: TerminalSize { cols: 80, rows: 24 },
        })
        .await
        .unwrap();
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn creating_a_chat_refreshes_the_loaded_thread_list_with_its_query() {
    let (store, mut reader, mut writer) = setup(Snapshot {
        threads: Some(Arc::new(serde_json::from_value(json!({"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})).unwrap())),
        list_query: Arc::new(agent_core::models::ListQuery { search_term:"created".into(), ..Default::default() }),
        ..Default::default()
    });
    let server = tokio::spawn(async move {
        while let Some(line) = tokio::time::timeout(Duration::from_secs(2), reader.read_line())
            .await
            .unwrap()
            .unwrap()
        {
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "host/thread/start" => {
                    json!({"thread":{"id":"created","name":"created chat","cwd":"/fixture","turns":[],"status":{"type":"idle"}}})
                }
                "turn/start" => json!({"turn":{"id":"turn"}}),
                "host/thread/list" => {
                    let data = if request["params"]["searchTerm"] == "created" {
                        json!([{"id":"created","name":"created chat"}])
                    } else {
                        json!([])
                    };
                    json!({"data":data,"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})
                }
                other => panic!("unexpected method {other}"),
            };
            writer
                .write_line(&json!({"id":request["id"],"result":result}).to_string())
                .await
                .unwrap();
        }
    });
    store
        .dispatch(Intent::NewChat("/fixture".into()))
        .await
        .unwrap();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: "new:/fixture".into(),
            text: "message".into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::Submit {
            thread_id: None,
            client_user_message_id: "client".into(),
        })
        .await
        .unwrap();
    wait_for(&store, |state| {
        state.threads.as_ref().is_some_and(|page| {
            page.data
                .iter()
                .any(|thread| thread.id.as_deref() == Some("created"))
        })
    })
    .await;
    store.close().await.unwrap();
    server.await.unwrap();
}
