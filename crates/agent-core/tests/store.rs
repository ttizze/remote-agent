async fn scoped_incoming(
    host: &agent_core::transport::Endpoint,
    trust: &agent_core::transport::Trust,
) -> (
    agent_core::transport::Session,
    impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + use<>,
) {
    use tokio::io::AsyncWriteExt;
    let session = host
        .accept()
        .await
        .unwrap()
        .unwrap()
        .authorize(trust)
        .unwrap();
    let mut stream = session.accept_stream().await.unwrap();
    let request = {
        let mut reader = agent_core::peer::JsonlReader::new(&mut stream);
        assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(""));
        serde_json::from_str::<serde_json::Value>(&reader.read_line().await.unwrap().unwrap())
            .unwrap()
    };
    assert_eq!(request["method"], "host/session/scope");
    stream
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({"id":request["id"],"result":"fixture-storage"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    (session, stream)
}

#[path = "support/session_wire.rs"]
mod session_wire;
use agent_core::peer::JsonlReader;
use agent_core::state::operations as op;
use agent_core::{
    client::Answer,
    models::Thread,
    peer::{PeerError, RpcPeer},
    state::{Draft, Intent, Snapshot},
    store::{Outcome, Store},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn connected(snapshot: Snapshot) -> (Arc<Store>, session_wire::Reader, session_wire::Writer) {
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let peer = RpcPeer::open(
        JsonlReader::new(read),
        write,
        Some(Duration::from_secs(1)),
        16,
    )
    .unwrap();
    let (reader, writer) = session_wire::pair(server, &snapshot);
    (Arc::new(Store::new(peer, snapshot)), reader, writer)
}
async fn setup(snapshot: Snapshot) -> (Arc<Store>, session_wire::Reader, session_wire::Writer) {
    let (store, mut reader, mut writer) = connected(snapshot.clone());
    let selected = snapshot
        .navigation
        .thread_id
        .as_ref()
        .map(|id| &snapshot.conversations[id]);
    let cwd = selected.map_or(snapshot.navigation.cwd.as_str(), |thread| {
        thread.cwd.as_deref().unwrap_or_default()
    });
    for _ in 0..2 + usize::from(selected.is_some()) + usize::from(!cwd.is_empty()) {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/thread/list" => snapshot.threads.as_ref().map(|threads| json!(threads))
                .unwrap_or_else(|| json!({"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})),
            "model/list" => json!({"data":snapshot.models,"nextCursor":null}),
            "host/session/open" => json!({"thread": snapshot.conversations[snapshot.navigation.thread_id.as_ref().unwrap()]}),
            "host/workspace/review" => { assert_eq!(request["params"]["cwd"], cwd); review() },
            method => panic!("unexpected connection request: {method}"),
        };
        writer
            .write_line(&json!({"id":request["id"], "result":result}).to_string())
            .await
            .unwrap();
    }
    for (id, thread) in snapshot.conversations.iter() {
        if snapshot.navigation.thread_id.as_ref() == Some(id) {
            continue;
        }
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::new(id.clone())));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/open");
        writer
            .write_line(&json!({"id":request["id"],"result":{"thread":thread}}).to_string())
            .await
            .unwrap();
        opening.await.unwrap();
    }
    wait_for(&store, |current| {
        current.threads.is_some()
            && !Arc::ptr_eq(&current.models, &snapshot.models)
            && (cwd.is_empty() || current.workspace.review.is_some())
    })
    .await;
    (store, reader, writer)
}
async fn read(reader: &mut session_wire::Reader) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(2), reader.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
fn review() -> Value {
    json!({"branch":"main","additions":0,"deletions":0,"files":[],"diff":""})
}
async fn new_chat(
    store: &Store,
    reader: &mut session_wire::Reader,
    writer: &mut session_wire::Writer,
    cwd: &str,
) {
    let navigation = store.dispatch(Intent::NewChat { cwd: cwd.into() });
    let request = read(reader).await;
    assert_eq!(request["method"], "host/workspace/review");
    assert_eq!(request["params"], json!({"cwd":cwd}));
    writer
        .write_line(&json!({"id":request["id"],"result":review()}).to_string())
        .await
        .unwrap();
    navigation.await.unwrap();
}
async fn read_after_reviews(
    reader: &mut session_wire::Reader,
    writer: &mut session_wire::Writer,
) -> Value {
    loop {
        let request = read(reader).await;
        if request["method"] == "host/session/open" {
            let id = request["params"]["session"]["id"].as_str().unwrap();
            let response = writer.current(id);
            writer
                .write_line(&json!({"id":request["id"], "result":response}).to_string())
                .await
                .unwrap();
            continue;
        }
        if request["method"] == "host/thread/list" {
            writer.write_line(&json!({"id":request["id"],"result":{"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}}).to_string()).await.unwrap();
            continue;
        }
        if request["method"] != "host/workspace/review" {
            return request;
        }
        assert_eq!(request["params"], json!({"cwd":"/fixture"}));
        writer
            .write_line(&json!({"id":request["id"],"result":review()}).to_string())
            .await
            .unwrap();
    }
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
        let (store, _reader, _writer) = setup(previous.clone()).await;
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
    let (store, mut reader, mut writer) = setup(snapshot()).await;
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
        store.dispatch(Intent::ReadThread(op::ReadThread::new("thread".into()))),
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
    let (store, mut reader, mut writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let pending = read(&mut reader).await;
        writer.write_line(&json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"thread"}}).to_string()).await.unwrap();
        let answer = read(&mut reader).await;
        assert_eq!(answer["method"], "host/session/answer");
        assert_eq!(
            answer["params"],
            json!({"requestId":"approval","result":{"decision":"decline"}})
        );
        writer
            .write_line(&json!({"id":answer["id"],"result":{}}).to_string())
            .await
            .unwrap();
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
        async move {
            store
                .dispatch(Intent::ReadThread(op::ReadThread::new("thread".into())))
                .await
        }
    });
    wait_for(&store, |snapshot| {
        snapshot.requests.contains_key("\"approval\"")
    })
    .await;
    store
        .dispatch(Intent::Respond(op::Respond {
            request_id: json!("approval"),
            answer: Answer::Decision { index: 2 },
        }))
        .await
        .unwrap();
    assert_eq!(loading.await.unwrap().unwrap(), Outcome::Applied);
    let _writer = server.await.unwrap();
    assert!(store.snapshot().requests.is_empty());
    assert_eq!(loaded_text(&store.snapshot()), Some("approved path"));
}
#[tokio::test]
async fn invalid_typed_reply_does_not_block_later_wire_events() {
    let (store, mut reader, mut writer) = setup(snapshot()).await;
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
        .dispatch(Intent::ReadThread(op::ReadThread::new("thread".into())))
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
        .dispatch(Intent::ReadThread(op::ReadThread::new("thread".into())))
        .await
        .unwrap();
    let _writer = server.await.unwrap();
    assert_eq!(loaded_text(&store.snapshot()), Some("recovered"));
}
#[tokio::test]
async fn snapshot_notification_can_reenter_store_synchronously() {
    use std::{
        future::Future,
        task::{Context, Wake, Waker},
    };
    struct ReadOnWake {
        store: Arc<Store>,
        observed: std::sync::mpsc::Sender<String>,
    }
    impl Wake for ReadOnWake {
        fn wake(self: Arc<Self>) {
            self.observed
                .send(self.store.snapshot().navigation.cwd.clone())
                .unwrap();
        }
    }
    let store = Arc::new(Store::offline(Snapshot::default()));
    let mut updates = store.subscribe();
    let mut changed = Box::pin(updates.changed());
    let (observed, received) = std::sync::mpsc::channel();
    let waker = Waker::from(Arc::new(ReadOnWake {
        store: store.clone(),
        observed,
    }));
    assert!(
        changed
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let publishing = std::thread::spawn({
        let store = store.clone();
        move || {
            drop(store.dispatch(Intent::NewChat {
                cwd: "/fixture".into(),
            }))
        }
    });
    assert_eq!(
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("notification held Store's snapshot lock"),
        "/fixture"
    );
    publishing.join().unwrap();
    drop(changed);
    store.close().await.unwrap();
}

#[tokio::test]
async fn new_conversation_clears_sent_draft_after_native_echo() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    new_chat(&store, &mut reader, &mut writer, "/fixture").await;
    let key = store.snapshot().navigation.draft_key.clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: "first message".into(),
        })
        .await
        .unwrap();
    let sending = store.dispatch(Intent::Submit {
        thread_id: None,
        client_user_message_id: "client".into(),
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/thread/start");
    writer.write_line(&json!({"id": request["id"], "result": {"thread": {"id":"created", "cwd":"/fixture", "status":{"type":"idle"}, "turns":[]}}}).to_string()).await.unwrap();
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "turn/start");
    assert_eq!(request["params"]["input"][0]["text"], "first message");
    writer.write_line(&json!({"method":"item/completed", "params":{"threadId":"created", "turnId":"turn", "item":{"id":"native", "type":"userMessage", "clientId":"client", "content":[{"type":"text", "text":"first message"}]}}}).to_string()).await.unwrap();
    writer
        .write_line(&json!({"id":request["id"], "result":{"turn":{"id":"turn"}}}).to_string())
        .await
        .unwrap();
    sending.await.unwrap();
    let current = store.snapshot();
    assert_eq!(current.navigation.draft_key, "created");
    assert!(current.drafts["created"].text.is_empty());
    assert!(!current.drafts.contains_key(&key));
    store.close().await.unwrap();
}

#[tokio::test]
async fn successful_submission_does_not_erase_a_newer_draft() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/submission-drafts.json")).unwrap();
    for case in cases {
        let mut sent: Draft = serde_json::from_value(case["sent"].clone()).unwrap();
        sent.service_tier = Some("priority".into());
        let initial = Snapshot {
            conversations: Arc::new(BTreeMap::from([(
                "thread".into(),
                Arc::new(
                    serde_json::from_value(
                        json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]}),
                    )
                    .unwrap(),
                ),
            )])),
            ..Default::default()
        };
        let (store, mut reader, mut writer) = setup(initial).await;
        store
            .dispatch(Intent::SetDraft {
                thread_id: "thread".into(),
                draft: sent,
            })
            .await
            .unwrap();
        let sending = store.dispatch(Intent::Submit {
            thread_id: Some("thread".into()),
            client_user_message_id: "fixture-message".into(),
        });
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(request["method"], "turn/start");
        assert_eq!(request["params"]["input"][0]["text"], "sent");
        assert_eq!(request["params"]["serviceTierForTurn"], "priority");
        store
            .dispatch(Intent::SetDraft {
                thread_id: "thread".into(),
                draft: serde_json::from_value(case["current"].clone()).unwrap(),
            })
            .await
            .unwrap();
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"turn":{"id":"turn-new"}}}).to_string(),
            )
            .await
            .unwrap();
        assert_eq!(
            sending.await.unwrap(),
            Outcome::Submitted {
                turn_id: Some("turn-new".into())
            }
        );
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(
            *store.snapshot().drafts["thread"],
            expected,
            "{}",
            case["name"]
        );
        assert!(store.snapshot().error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn new_submission_keeps_edits_and_navigation_while_creation_is_pending() {
    for navigate_away in [false, true] {
        let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
        new_chat(&store, &mut reader, &mut writer, "/fixture").await;
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
        let create = read_after_reviews(&mut reader, &mut writer).await;
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
            new_chat(&store, &mut reader, &mut writer, "/other").await;
        }
        writer.write_line(&json!({"id":create["id"],"result":{"thread":{"id":"created","cwd":"/fixture","turns":[],"status":{"type":"idle"}}}}).to_string()).await.unwrap();
        let submit = read_after_reviews(&mut reader, &mut writer).await;
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
            Outcome::Submitted {
                turn_id: Some("turn".into())
            }
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
        let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
        new_chat(&store, &mut reader, &mut writer, "/fixture").await;
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
        let create = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(create["method"], "host/thread/start");
        let failed = if fail_creation {
            create
        } else {
            writer.write_line(&json!({"id":create["id"],"result":{"thread":{"id":"created","cwd":"/fixture","turns":[],"status":{"type":"idle"}}}}).to_string()).await.unwrap();
            let submit = read_after_reviews(&mut reader, &mut writer).await;
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
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert!(
            store.snapshot().error.is_none(),
            "retry must clear the previous submission error"
        );
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
        let (store, mut reader, mut writer) = setup(initial).await;
        let transcribing = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .dispatch(Intent::Transcribe(op::Dictate {
                        draft_key: "thread".into(),
                        audio: vec![0, 0],
                        send: true,
                        client_user_message_id: "dictation".into(),
                    }))
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
        let submit = read_after_reviews(&mut reader, &mut writer).await;
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
async fn new_chat_dictation_preserves_text_and_images_for_draft_and_direct_send() {
    for direct in [false, true] {
        let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        let attachment = agent_core::state::Attachment {
            path: "/fixture/photo.png".into(),
            name: "photo.png".into(),
            is_image: true,
        };
        store
            .dispatch(Intent::SetDraft {
                thread_id: key.clone(),
                draft: Draft {
                    text: "typed".into(),
                    attachments: vec![attachment.clone()],
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        let transcribing = store.dispatch(Intent::Transcribe(op::Dictate {
            draft_key: key.clone(),
            audio: vec![0, 0],
            send: direct,
            client_user_message_id: "dictation".into(),
        }));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/transcribe");
        writer
            .write_line(&json!({"id":request["id"],"result":{"text":"spoken"}}).to_string())
            .await
            .unwrap();
        let sending = if direct {
            transcribing
        } else {
            assert_eq!(transcribing.await.unwrap(), Outcome::Applied);
            let draft = store.snapshot().drafts[&key].clone();
            assert_eq!(draft.text, "typed\nspoken");
            assert_eq!(draft.attachments, vec![attachment]);
            assert!(store.snapshot().navigation.thread_id.is_none());
            store.dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "dictation".into(),
            })
        };
        let start = read(&mut reader).await;
        assert_eq!(start["method"], "host/thread/start");
        assert!(start["params"].get("cwd").is_none());
        writer.write_line(&json!({"id":start["id"],"result":{"thread":{"id":"created","cwd":"/fixture","projectId":null,"status":{"type":"idle"},"turns":[]}}}).to_string()).await.unwrap();
        let submit = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(submit["method"], "turn/start");
        let input = json!([{"type":"text","text":"typed\nspoken","text_elements":[]},{"type":"localImage","path":"/fixture/photo.png"}]);
        assert_eq!(submit["params"]["input"], input);
        writer
            .write_line(&json!({"id":submit["id"],"result":{"turn":{"id":"turn"}}}).to_string())
            .await
            .unwrap();
        writer.write_line(&json!({"method":"turn/completed","params":{"threadId":"created","turn":{"id":"turn","status":"completed","items":[
            {"id":"native","type":"userMessage","clientId":"dictation","content":input},
            {"id":"answer","type":"agentMessage","phase":"final_answer","text":"done"}
        ]}}}).to_string()).await.unwrap();
        assert!(matches!(sending.await.unwrap(), Outcome::Submitted { .. }));
        wait_for(&store, |snapshot| {
            snapshot.conversations["created"]
                .turns
                .as_ref()
                .is_some_and(|turns| turns[0].status.as_deref() == Some("completed"))
        })
        .await;
        let current = store.snapshot();
        assert_eq!(current.navigation.thread_id.as_deref(), Some("created"));
        assert!(current.selected_directory().is_empty());
        assert_eq!(current.navigation.cwd, "/fixture");
        assert!(current.drafts["created"].text.is_empty());
        assert!(current.drafts["created"].attachments.is_empty());
        assert!(current.pending_submissions.is_empty());
        assert!(current.error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn navigation_cancels_dictation_send_but_keeps_the_transcript_in_its_draft() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    new_chat(&store, &mut reader, &mut writer, "/fixture").await;
    let transcribing = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::Transcribe(op::Dictate {
                    draft_key: "new:/fixture".into(),
                    audio: vec![0, 0],
                    send: true,
                    client_user_message_id: "dictation".into(),
                }))
                .await
        }
    });
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/other").await;
    new_chat(&store, &mut reader, &mut writer, "/fixture").await;
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
async fn silent_dictation_preserves_drafts_and_navigation_without_sending() {
    for (existing, send, navigate, transcript) in [
        (false, true, false, " \n"),
        (true, true, false, ""),
        (false, true, true, ""),
        (true, false, false, " \n"),
    ] {
        let mut initial = snapshot();
        let key = if existing { "thread" } else { "new:/fixture" };
        let navigation = Arc::make_mut(&mut initial.navigation);
        navigation.thread_id = existing.then(|| "thread".into());
        navigation.cwd = "/fixture".into();
        navigation.draft_key = key.into();
        Arc::make_mut(&mut initial.drafts).insert(
            key.into(),
            Arc::new(Draft {
                text: "keep this draft".into(),
                attachments: vec![agent_core::state::Attachment {
                    path: "/fixture/photo.png".into(),
                    name: "photo.png".into(),
                    is_image: true,
                }],
                ..Default::default()
            }),
        );
        let (store, mut reader, mut writer) = setup(initial).await;
        let operation = store.dispatch(Intent::Transcribe(op::Dictate {
            draft_key: key.into(),
            audio: vec![0, 0],
            send,
            client_user_message_id: "silent".into(),
        }));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/transcribe");
        if navigate {
            new_chat(&store, &mut reader, &mut writer, "/other").await;
        }
        let before = store.snapshot();
        writer
            .write_line(&json!({"id":request["id"], "result":{"text":transcript}}).to_string())
            .await
            .unwrap();
        assert_eq!(operation.await.unwrap(), Outcome::Applied);
        let after = store.snapshot();
        assert_eq!(after.drafts, before.drafts);
        assert_eq!(after.conversations, before.conversations);
        assert_eq!(after.pending_submissions, before.pending_submissions);
        assert_eq!(after.navigation, before.navigation);
        assert!(after.error.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_line())
                .await
                .is_err(),
            "silent dictation must not submit or create a conversation"
        );
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&after).unwrap()).unwrap();
        assert_eq!(restored.drafts, after.drafts);
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn file_navigation_ignores_a_late_reply_from_the_previous_file() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let first = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadFile(op::ReadFile {
                    path: "/first".into(),
                    discard_draft: false,
                }))
                .await
        }
    });
    let first_request = read(&mut reader).await;
    let second = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadFile(op::ReadFile {
                    path: "/second".into(),
                    discard_draft: false,
                }))
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
    let (store, mut reader, mut writer) = setup(initial).await;
    store
        .dispatch(Intent::SetFileDraft {
            path: "/file".into(),
            text: "submitted".into(),
        })
        .await
        .unwrap();
    let saving = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::SaveFile(op::SaveFile {
                    path: "/file".into(),
                }))
                .await
        }
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
        async move {
            store
                .dispatch(Intent::SaveFile(op::SaveFile {
                    path: "/file".into(),
                }))
                .await
        }
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
async fn opening_selects_the_task_before_history_and_list_refresh_finish() {
    for (cached, restored) in [(false, false), (true, true)] {
        let mut initial = if cached {
            snapshot()
        } else {
            Snapshot::default()
        };
        initial.threads = Some(Arc::new(
            serde_json::from_value(json!({
                "data":[{"id":"thread","cwd":"/listed","name":"Selected task"}],
                "projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            }))
            .unwrap(),
        ));
        Arc::make_mut(&mut initial.drafts).insert(
            "thread".into(),
            Arc::new(Draft {
                text: "Keep this draft".into(),
                ..Default::default()
            }),
        );
        if restored {
            initial = serde_json::from_slice(&serde_json::to_vec(&initial).unwrap()).unwrap();
        }
        let (store, mut reader, mut writer) = setup(initial).await;
        let refresh = store.dispatch(Intent::ListThreads(
            op::ListThreads::new(Default::default()),
        ));
        let list_request = read(&mut reader).await;
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open("thread".into())));
        let selected = store.snapshot();
        assert_eq!(
            selected.navigation.thread_id.as_deref(),
            Some("thread"),
            "selection must not wait for either RPC (cached={cached}, restored={restored})"
        );
        assert_eq!(selected.navigation.draft_key, "thread");
        assert_eq!(
            selected.navigation.cwd,
            if cached { "/fixture" } else { "/listed" }
        );
        assert_eq!(selected.drafts["thread"].text, "Keep this draft");
        assert_eq!(loaded_text(&selected), cached.then_some("old"));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/open");
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":thread("latest")}}).to_string(),
            )
            .await
            .unwrap();
        opening.await.unwrap();
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(store.snapshot().navigation.cwd, "/fixture");
        writer.write_line(&json!({"id":list_request["id"],"result":{
                "data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            }}).to_string()).await.unwrap();
        refresh.await.unwrap();
        assert_eq!(
            store.snapshot().navigation.thread_id.as_deref(),
            Some("thread")
        );
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(store.snapshot().drafts["thread"].text, "Keep this draft");
        assert!(store.snapshot().error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_failed_open_keeps_selection_and_draft_and_can_retry() {
    for cached in [false, true] {
        let (store, mut reader, mut writer) = setup(if cached {
            snapshot()
        } else {
            Snapshot::default()
        })
        .await;
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open("thread".into())));
        store
            .dispatch(Intent::SetDraftText {
                thread_id: "thread".into(),
                text: "Written while loading".into(),
            })
            .await
            .unwrap();
        let request = read(&mut reader).await;
        writer.write_line(&json!({"id":request["id"],"error":{"code":-32000,"message":"history unavailable"}}).to_string()).await.unwrap();
        assert!(opening.await.is_err());
        assert_eq!(
            store.snapshot().navigation.thread_id.as_deref(),
            Some("thread")
        );
        assert_eq!(loaded_text(&store.snapshot()), cached.then_some("old"));
        assert!(
            store
                .snapshot()
                .error
                .as_ref()
                .unwrap()
                .contains("history unavailable")
        );
        let retry = store.dispatch(Intent::ReadThread(op::ReadThread::open("thread".into())));
        assert!(store.snapshot().error.is_none());
        let request = read(&mut reader).await;
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":thread("recovered")}}).to_string(),
            )
            .await
            .unwrap();
        retry.await.unwrap();
        let recovered = store.snapshot();
        assert_eq!(loaded_text(&recovered), Some("recovered"));
        assert_eq!(recovered.drafts["thread"].text, "Written while loading");
        assert!(recovered.error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_late_open_reply_caches_the_thread_without_leaving_a_new_chat() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let opening = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadThread(op::ReadThread::open("thread".into())))
                .await
        }
    });
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/new-project").await;
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
    let (store, mut reader, mut writer) = setup(initial).await;
    writer.write_line(&json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"completed","status":"completed"}}}).to_string()).await.unwrap();
    wait_for(&store, |snapshot| {
        snapshot.activity.active.get("thread") == Some(&false)
    })
    .await;
    let refresh = read(&mut reader).await;
    assert_eq!(refresh["method"], "host/thread/list");
    // Sending must not wait for the catalogue refresh to complete.
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
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "turn/start");
    writer
        .write_line(&json!({"id":request["id"],"result":{"turn":{"id":"new-turn"}}}).to_string())
        .await
        .unwrap();
    assert_eq!(
        sending.await.unwrap().unwrap(),
        Outcome::Submitted {
            turn_id: Some("new-turn".into())
        }
    );
}

#[tokio::test]
async fn a_late_list_reply_cannot_replace_a_new_search() {
    use agent_core::models::ListQuery;
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let old = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ListThreads(op::ListThreads::new(ListQuery {
                    search_term: "old".into(),
                    ..Default::default()
                })))
                .await
        }
    });
    let old_request = read(&mut reader).await;
    let new = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ListThreads(op::ListThreads::new(ListQuery {
                    search_term: "new".into(),
                    ..Default::default()
                })))
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
    let (store, mut reader, mut writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/open");
        writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"gallery","turns":[{"id":"image-turn","items":[{"id":"image","type":"imageGeneration","savedPath":"/image.png"}]}]}}}).to_string()).await.unwrap();
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" continued"}}).to_string()).await.unwrap();
        (reader, writer)
    });
    let result = store
        .dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
            thread_id: "gallery".into(),
        }))
        .await
        .unwrap();
    let Outcome::SessionImages { images } = result else {
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
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
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
        assert_eq!(close["params"]["processHandle"], "terminal");
        writer
            .write_line(&json!({"id":close["id"],"result":{}}).to_string())
            .await
            .unwrap();
        // Keep the transport alive until Store closes it.
        assert!(reader.read_line().await.unwrap().is_none());
    });
    let start = store.dispatch(Intent::StartTerminal(op::StartTerminal {
        handle: "terminal".into(),
        cwd: "/fixture".into(),
        size: TerminalSize { cols: 80, rows: 24 },
    }));
    let first = store.dispatch(Intent::WriteTerminal(op::WriteTerminal {
        handle: "terminal".into(),
        data: b"first".to_vec(),
    }));
    let second = store.dispatch(Intent::WriteTerminal(op::WriteTerminal {
        handle: "terminal".into(),
        data: b"second".to_vec(),
    }));
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
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn terminal_exit_before_spawn_reply_is_not_replaced_by_running() {
    use agent_core::{client::TerminalSize, state::TerminalPhase};
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
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
        .dispatch(Intent::StartTerminal(op::StartTerminal {
            handle: "terminal".into(),
            cwd: "/fixture".into(),
            size: TerminalSize { cols: 80, rows: 24 },
        }))
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
async fn creating_a_chat_refreshes_the_loaded_thread_list_with_its_query() {
    let (store, mut reader, mut writer) = setup(Snapshot {
        threads: Some(Arc::new(serde_json::from_value(json!({"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})).unwrap())),
        list_query: Arc::new(agent_core::models::ListQuery { search_term:"created".into(), ..Default::default() }),
        ..Default::default()
    }).await;
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
                "host/session/open" => {
                    writer.current(request["params"]["session"]["id"].as_str().unwrap())
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
                "host/workspace/review" => review(),
                other => panic!("unexpected method {other}"),
            };
            writer
                .write_line(&json!({"id":request["id"],"result":result}).to_string())
                .await
                .unwrap();
        }
    });
    store
        .dispatch(Intent::NewChat {
            cwd: "/fixture".into(),
        })
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

#[tokio::test]
async fn malformed_file_change_delta_does_not_poison_store() {
    let mut previous = snapshot();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut previous.conversations)
            .get_mut("thread")
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0])
        .items
        .as_mut()
        .unwrap()
        .push(Arc::new(
            serde_json::from_value(json!({"id":"file", "type":"fileChange", "changes":[7]}))
                .unwrap(),
        ));
    let (store, mut reader, mut writer) = setup(previous).await;
    for (method, item) in [
        ("item/fileChange/outputDelta", "file"),
        ("item/agentMessage/delta", "item"),
    ] {
        writer
            .write_line(
                &json!({"method":method,"params":{
                    "threadId":"thread","turnId":"turn","itemId":item,"delta":" continues"
                }})
                .to_string(),
            )
            .await
            .unwrap();
    }
    let reopening = read(&mut reader).await;
    assert_eq!(reopening["method"], "host/session/open");
    writer
        .write_line(&json!({"id":reopening["id"],"result":writer.current("thread")}).to_string())
        .await
        .unwrap();
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("old continues")
    })
    .await;
    assert_eq!(
        serde_json::to_value(
            &store.snapshot().conversations["thread"]
                .turns
                .as_ref()
                .unwrap()[0]
                .items
                .as_ref()
                .unwrap()[1]
                .changes
        )
        .unwrap(),
        json!([7])
    );
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: agent_core::state::Draft {
                text: "still usable".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(store.snapshot().drafts["thread"].text, "still usable");
    store.close().await.unwrap();
}

#[tokio::test]
async fn expanded_history_failure_preserves_cache_and_retry_adopts_complete_window() {
    let initial: Thread = serde_json::from_value(json!({"id":"thread","historyLimit":5,"historyHasMore":true,"turns":[{"id":"latest","items":[]}]})).unwrap();
    let (store, mut reader, mut writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([("thread".into(), Arc::new(initial))])),
        ..Default::default()
    })
    .await;
    let cached = store.snapshot().conversations.clone();
    let server = tokio::spawn(async move {
        for failed in [true, false] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/session/open");
            assert_eq!(
                request["params"],
                json!({"session":{"provider":"codex","id":"thread"},"limit":10})
            );
            let response = if failed {
                json!({"id":request["id"],"error":{"code":-32000,"message":"temporary history failure"}})
            } else {
                json!({"id":request["id"],"result":{"thread":{"id":"thread","historyLimit":10,"historyHasMore":false,
                    "turns":[{"id":"old","items":[]},{"id":"missing","items":[
                        {"id":"question","type":"userMessage","content":[{"type":"text","text":"comparison"}]}]},{"id":"latest","items":[]}]}}})
            };
            writer.write_line(&response.to_string()).await.unwrap();
        }
        (reader, writer)
    });
    assert!(
        store
            .dispatch(Intent::ReadOlder(op::ReadOlder::new("thread".into())))
            .await
            .is_err()
    );
    assert_eq!(store.snapshot().conversations, cached);
    store
        .dispatch(Intent::ReadOlder(op::ReadOlder::new("thread".into())))
        .await
        .unwrap();
    let recovered = store.snapshot();
    let thread = &recovered.conversations["thread"];
    assert_eq!(
        thread
            .turns
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["old", "missing", "latest"]
    );
    assert_eq!(
        thread.turns.as_ref().unwrap()[1].items.as_ref().unwrap()[0].id,
        "question"
    );
    assert_eq!(thread.extra["historyHasMore"], false);
    assert_eq!(recovered.error, None);
    let _server = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn expanded_history_replaces_the_window_preserving_native_item_ids() {
    let initial: Thread = serde_json::from_value(json!({"id":"thread","historyLimit":5,"historyHasMore":true,
        "turns":[{"id":"new","status":"completed","items":[{"id":"new-item","type":"agentMessage","text":"new"}]}]})).unwrap();
    let (store, mut reader, mut writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([("thread".into(), Arc::new(initial))])),
        ..Default::default()
    })
    .await;
    let server = tokio::spawn(async move {
        for (limit, complete) in [(10, false), (15, true)] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/session/open");
            assert_eq!(request["params"]["limit"], limit);
            let items = if complete {
                json!([{"id":"first-old","text":"head"},{"id":"last-old","text":"tail"}])
            } else {
                json!([{"id":"last-old","text":"tail"}])
            };
            writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"thread","historyLimit":limit,"historyHasMore":!complete,
                "turns":[{"id":"old","itemsHasMore":!complete,"items":items},
                {"id":"new","status":"completed","items":[{"id":"new-item","type":"agentMessage","text":"new"}]}]}}}).to_string()).await.unwrap();
        }
        (reader, writer)
    });
    for _ in 0..2 {
        store
            .dispatch(Intent::ReadOlder(op::ReadOlder::new("thread".into())))
            .await
            .unwrap();
    }
    let snapshot = store.snapshot();
    let thread = &snapshot.conversations["thread"];
    let turns = thread.turns.as_ref().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["old", "new"]
    );
    assert_eq!(
        turns[0]
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["first-old", "last-old"]
    );
    assert_eq!(
        turns[1].items.as_ref().unwrap()[0].text.as_deref(),
        Some("new")
    );
    assert_eq!(turns[0].items_has_more, Some(false));
    assert_eq!(thread.extra["historyHasMore"], false);
    let _server = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn fork_opens_the_returned_thread_and_keeps_later_deltas() {
    for navigate in [false, true] {
        let (store, mut reader, mut writer) = setup(snapshot()).await;
        let fork = store.dispatch(Intent::ForkThread(op::ForkThread::new(
            "thread".into(),
            "turn".into(),
        )));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "thread/fork");
        assert_eq!(request["params"]["lastTurnId"], "turn");
        if navigate {
            new_chat(&store, &mut reader, &mut writer, "/new").await;
        }
        writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"forked","cwd":"/fixture","turns":[{"id":"copy","items":[{"id":"reply","type":"agentMessage","text":"copied"}]}]}}}).to_string()).await.unwrap();
        if !navigate {
            loop {
                let opening = read(&mut reader).await;
                if opening["method"] == "host/session/open" {
                    writer
                        .write_line(
                            &json!({"id":opening["id"],"result":writer.current("forked")})
                                .to_string(),
                        )
                        .await
                        .unwrap();
                    break;
                }
                let result = if opening["method"] == "host/workspace/review" {
                    review()
                } else {
                    json!({"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})
                };
                writer
                    .write_line(&json!({"id":opening["id"],"result":result}).to_string())
                    .await
                    .unwrap();
            }
            writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"forked","turnId":"copy","itemId":"reply","delta":" later"}}).to_string()).await.unwrap();
        }
        assert_eq!(
            fork.await.unwrap(),
            Outcome::StartedThread {
                id: "forked".into()
            }
        );
        wait_for(&store, |s| {
            s.conversations
                .get("forked")
                .and_then(|t| t.turns.as_ref())
                .is_some_and(|turns| {
                    turns[0].items.as_ref().unwrap()[0].text.as_deref()
                        == Some(if navigate { "copied" } else { "copied later" })
                })
        })
        .await;
        assert_eq!(
            store.snapshot().navigation.thread_id.as_deref(),
            if navigate { None } else { Some("forked") }
        );
        assert_eq!(
            store.snapshot().navigation.cwd,
            if navigate { "/new" } else { "/fixture" }
        );
        assert_eq!(loaded_text(&store.snapshot()), Some("old"));
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn account_selection_publishes_the_selected_account_and_persistence_warning() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let request = read(&mut reader).await;
    writer.write_line(&json!({"id":request["id"],"result":{"accounts":[{"id":"a"}],"selectedId":"a","error":null}}).to_string()).await.unwrap();
    listing.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected_id
            .as_deref(),
        Some("a")
    );
    let selecting = store.dispatch(Intent::SelectAccount(op::SelectAccount { id: "b".into() }));
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["accountId"], "b");
    writer.write_line(&json!({"id":request["id"],"result":{"selectedId":"b","persistenceError":"store unavailable"}}).to_string()).await.unwrap();
    selecting.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected_id
            .as_deref(),
        Some("b")
    );
    assert_eq!(store.snapshot().error.as_deref(), Some("store unavailable"));
    // Selecting a newly logged-in account invalidates the login completion's
    // pending list request, so selection must fetch the new entry itself.
    for _ in 0..2 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/account/list" => {
                json!({"accounts":[{"id":"a"},{"id":"b"}],"selectedId":"b","error":null})
            }
            "model/list" => json!({"data":[]}),
            method => panic!("unexpected account refresh: {method}"),
        };
        writer
            .write_line(&json!({"id":request["id"],"result":result}).to_string())
            .await
            .unwrap();
    }
    wait_for(&store, |s| {
        s.account.accounts.as_ref().is_some_and(|accounts| {
            accounts.accounts.iter().any(|account| account.id == "b")
                && accounts.selected_id.as_deref() == Some("b")
        })
    })
    .await;
    store.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_account_listing_preserves_login_and_cancellation_ignores_late_status() {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {}));
    let request = read(&mut reader).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let list = read(&mut reader).await;
    writer
        .write_line(
            &json!({"id":list["id"],"result":{"accounts":[],"selectedId":null,"error":null}})
                .to_string(),
        )
        .await
        .unwrap();
    listing.await.unwrap();
    writer.write_line(&json!({"id":request["id"],"result":{"loginId":"login","userCode":"fixture-only","verificationUrl":"https://example.invalid"}}).to_string()).await.unwrap();
    starting.await.unwrap();
    assert_eq!(
        store.snapshot().account.login.as_ref().unwrap().login_id,
        "login"
    );

    let polling = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
        id: "login".into(),
    }));
    let poll = read(&mut reader).await;
    let cancelling = store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
        id: "login".into(),
    }));
    let cancel = read(&mut reader).await;
    writer
        .write_line(&json!({"id":cancel["id"],"result":{}}).to_string())
        .await
        .unwrap();
    cancelling.await.unwrap();
    writer
        .write_line(
            &json!({"id":poll["id"],"result":{"completed":true,"accountId":"obsolete"}})
                .to_string(),
        )
        .await
        .unwrap();
    polling.await.unwrap();
    let state = store.snapshot();
    assert!(state.account.login.is_none());
    assert!(state.account.login_status.is_none());
    assert!(state.account.accounts.as_ref().unwrap().accounts.is_empty());
    store.close().await.unwrap();
}

#[tokio::test]
async fn disconnected_store_keeps_editing_and_persisting_drafts() {
    let (store, reader, writer) = setup(Snapshot::default()).await;
    wait_for(&store, |s| s.connected).await;
    drop((reader, writer));
    wait_for(&store, |s| !s.connected).await;
    store
        .dispatch(Intent::NewChat {
            cwd: "/offline".into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: "new:/offline".into(),
            text: "切断中の下書き".into(),
        })
        .await
        .unwrap();
    let serialized = serde_json::to_vec(&store.snapshot()).unwrap();
    let restored: Snapshot = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(restored.drafts["new:/offline"].text, "切断中の下書き");
    assert!(
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "offline".into()
            })
            .await
            .is_err()
    );
    assert!(store.snapshot().pending_submissions.is_empty());
    assert_eq!(
        store.snapshot().drafts["new:/offline"].text,
        "切断中の下書き"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_preserves_edits_made_during_pairing() {
    use agent_core::transport::{Endpoint, Identity, Relays, Trust};
    use std::collections::BTreeSet;
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let trust = Trust {
            allowed: BTreeSet::from([client.node_id()]),
            ..Default::default()
        };
        let store = Arc::new(Store::offline(Snapshot::default()));
        let connecting = {
            let store = store.clone();
            let ticket = host.ticket();
            tokio::spawn(async move {
                store
                    .reconnect(&client, &ticket, Some(uuid::Uuid::new_v4()))
                    .await
            })
        };
        let pairing = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .pairing()
            .await
            .unwrap();
        store
            .dispatch(Intent::NewChat {
                cwd: "/pairing".into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SetDraftText {
                thread_id: "new:/pairing".into(),
                text: "接続待ち中の編集".into(),
            })
            .await
            .unwrap();
        let drafts = store.snapshot().drafts.clone();
        let (session, peer, mut events) = pairing.authorize(&trust).await.unwrap();
        loop {
            if let agent_core::peer::PeerEvent::Message(message) = events.recv().await.unwrap() {
                let request: Value = serde_json::from_str(&message.value).unwrap();
                if request["method"] == "host/session/scope" {
                    peer.respond_raw(&request["id"].to_string(), "result", "\"fixture-storage\"")
                        .await
                        .unwrap();
                    break;
                }
            }
        }
        connecting.await.unwrap().unwrap();
        wait_for(&store, |state| state.connected).await;
        assert!(Arc::ptr_eq(&drafts, &store.snapshot().drafts));
        assert_eq!(
            store.snapshot().drafts["new:/pairing"].text,
            "接続待ち中の編集"
        );
        assert_eq!(store.snapshot().navigation.cwd, "/pairing");
        store.close().await.unwrap();
        let _ = peer.close().await;
        session.close();
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reconnect_cancels_obsolete_pairing_and_retains_local_state() {
    use agent_core::transport::{Endpoint, Identity, Relays, Trust};
    for action in ["reconnect", "disconnect", "close", "drop"] {
        tokio::time::timeout(Duration::from_secs(15), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let client = Arc::new(Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap());
            let trust = Trust { allowed: [client.node_id()].into(), ..Default::default() };
            let (store, mut old_reader, _old_writer) = setup(Snapshot::default()).await;
            let connecting = {
                let store = store.clone();
                let client = client.clone();
                let ticket = host.ticket();
                tokio::spawn(async move { store.reconnect(&client, &ticket, Some(uuid::Uuid::new_v4())).await })
            };
            let pending = host.accept().await.unwrap().unwrap().pairing().await.unwrap();
            assert!(old_reader.read_line().await.unwrap().is_none(), "core must release the old connection before pairing");
            assert!(!store.snapshot().connected);
            store.dispatch(Intent::SetDraftText { thread_id: "local".into(), text: "接続待ち中の編集".into() }).await.unwrap();
            let drafts = store.snapshot().drafts.clone();
            let replacement = match action {
                "reconnect" => {
                    let ticket = host.ticket();
                    let (result, incoming) = tokio::join!(store.reconnect(&client, &ticket, None), scoped_incoming(&host, &trust));
                    result.unwrap();
                    Some(incoming)
                }
                "disconnect" => { store.disconnect().await.unwrap(); None }
                "close" => { store.close().await.unwrap(); None }
                "drop" => { connecting.abort(); None }
                _ => unreachable!(),
            };
            if action == "drop" {
                assert!(connecting.await.unwrap_err().is_cancelled());
            } else {
                assert!(connecting.await.unwrap().is_err(), "obsolete pairing must not succeed: {action}");
            }
            // A delayed authorization cannot revive the superseded connection.
            if let Ok((session, peer, _events)) = pending.authorize(&trust).await {
                let _ = peer.close().await;
                session.close();
            }
            if let Some((session, stream)) = replacement {
                let (read, write) = tokio::io::split(stream);
                let mut reader = JsonlReader::new(read);
                let mut writer = agent_core::peer::JsonlWriter::new(write);
                for _ in 0..2 {
                    let request = serde_json::from_str::<Value>(&reader.read_line().await.unwrap().unwrap()).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "host/thread/list" => json!({"data":[{"id":"replacement","name":"fresh"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
                        "model/list" => json!({"data":[],"nextCursor":null}),
                        other => panic!("unexpected bootstrap: {other}"),
                    };
                    writer.write_line(&json!({"id":request["id"],"result":result}).to_string()).await.unwrap();
                }
                wait_for(&store, |state| state.threads.as_ref().is_some_and(|threads| threads.data.iter().any(|thread| thread.id.as_deref() == Some("replacement")))).await;
                assert!(store.snapshot().connected);
                assert!(store.snapshot().error.is_none());
                store.close().await.unwrap();
                session.close();
            } else {
                assert!(!store.snapshot().connected, "cancelled setup must remain offline: {action}");
                store.close().await.unwrap();
            }
            assert!(Arc::ptr_eq(&drafts, &store.snapshot().drafts));
            assert_eq!(store.snapshot().drafts["local"].text, "接続待ち中の編集");
            client.close().await;
            host.close().await;
        }).await.unwrap_or_else(|_| panic!("timed out: {action}"));
    }
}

#[tokio::test]
async fn dispatch_publishes_edits_before_returning_to_the_native_input_control() {
    let store = Store::offline(Snapshot::default());
    let navigation = store.dispatch(Intent::NewChat {
        cwd: "/input".into(),
    });
    assert_eq!(store.snapshot().navigation.draft_key, "new:/input");
    let edit = store.dispatch(Intent::SetDraftText {
        thread_id: "new:/input".into(),
        text: "入力を戻さない".into(),
    });
    assert_eq!(store.snapshot().drafts["new:/input"].text, "入力を戻さない");
    let second = store.dispatch(Intent::SetDraftText {
        thread_id: "new:/input".into(),
        text: "second".into(),
    });
    second.await.unwrap();
    edit.await.unwrap();
    navigation.await.unwrap();
    assert_eq!(store.snapshot().drafts["new:/input"].text, "second");
    drop(store.dispatch(Intent::SetDraftText {
        thread_id: "new:/input".into(),
        text: "last".into(),
    }));
    assert_eq!(store.snapshot().drafts["new:/input"].text, "last");
    store.close().await.unwrap();
}

#[tokio::test]
async fn close_ends_subscriptions_while_store_is_retained() {
    let (store, _reader, _writer) = setup(Snapshot::default()).await;
    wait_for(&store, |snapshot| snapshot.connected).await;
    let mut updates = store.subscribe();
    store.close().await.unwrap();
    assert!(!store.snapshot().connected);
    updates.borrow_and_update();
    let result = tokio::time::timeout(Duration::from_millis(100), updates.changed()).await;
    assert!(
        matches!(result, Ok(Err(_))),
        "closed Store must end subscriptions"
    );
}

#[tokio::test]
async fn restored_snapshot_discards_session_authority_and_preserves_unknown_dictation() {
    use agent_core::client::{SubmissionTarget, submission_target};
    use agent_core::state::{Activity, Navigation, PendingSubmission};
    let draft = Arc::new(Draft {
        text: "typed".into(),
        ..Default::default()
    });
    let mut saved = snapshot();
    saved.connected = true;
    saved.navigation = Arc::new(Navigation {
        thread_id: Some("thread".into()),
        draft_key: "thread".into(),
        ..Default::default()
    });
    saved.activity = Arc::new(Activity {
        active: BTreeMap::from([("thread".into(), true)]),
        ..Default::default()
    });
    saved.drafts = Arc::new(BTreeMap::from([("thread".into(), draft.clone())]));
    saved.pending_submissions = Arc::new(BTreeMap::from([(
        "unsent".into(),
        Arc::new(PendingSubmission {
            draft_key: "thread".into(),
            draft,
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
            recovery_text: Some("spoken".into()),
            clear_draft: None,
        }),
    )]));
    saved.requests = Arc::new(BTreeMap::from([("1".into(), Arc::new(serde_json::from_value(json!({
        "id":1,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread"}
    })).unwrap()))]));
    let bytes = serde_json::to_vec(&saved).unwrap();
    let (store, _reader, _writer) = connected(serde_json::from_slice(&bytes).unwrap());
    wait_for(&store, |snapshot| snapshot.connected).await;
    let current = store.snapshot();
    assert!(current.requests.is_empty());
    assert!(current.activity.active.is_empty());
    assert!(current.subscriptions.is_empty());
    assert!(current.pending_submissions["unsent"].delivery_unknown);
    assert_eq!(
        current.pending_submissions["unsent"]
            .recovery_text
            .as_deref(),
        Some("spoken")
    );
    assert_eq!(current.drafts["thread"].text, "typed");
    assert!(matches!(
        submission_target(Some(&current.conversations["thread"]), None, None).unwrap(),
        SubmissionTarget::Start { .. }
    ));
    store.close().await.unwrap();
}

#[tokio::test]
async fn close_keeps_the_last_enqueued_draft_and_unconfirmed_send() {
    let (store, mut reader, _writer) = setup(snapshot()).await;
    store
        .dispatch(Intent::SetDraftText {
            thread_id: "thread".into(),
            text: "unsent".into(),
        })
        .await
        .unwrap();
    drop(store.dispatch(Intent::Submit {
        thread_id: Some("thread".into()),
        client_user_message_id: "unsent".into(),
    }));
    read(&mut reader).await;
    assert!(!store.snapshot().pending_submissions.is_empty());
    drop(store.dispatch(Intent::SetDraftText {
        thread_id: "thread".into(),
        text: "newest edit".into(),
    }));
    store.close().await.unwrap();
    let current = store.snapshot();
    assert!(current.pending_submissions["unsent"].delivery_unknown);
    assert_eq!(current.pending_submissions["unsent"].draft.text, "unsent");
    assert_eq!(current.drafts["thread"].text, "newest edit");
}

#[tokio::test]
async fn stores_share_an_endpoint_without_closing_each_others_transport() {
    use agent_core::transport::{Endpoint, Identity, Relays, Trust};
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let trust = Trust {
            allowed: [client.node_id()].into(),
            ..Default::default()
        };
        let ticket = host.ticket();
        let mut stores = Vec::new();
        let mut sessions = Vec::new();
        for _ in 0..2 {
            let (store, incoming) = tokio::join!(
                Store::connect(&client, &ticket, Snapshot::default(), None),
                scoped_incoming(&host, &trust)
            );
            stores.push(store.unwrap());
            sessions.push(incoming);
        }
        stores[0].close().await.unwrap();
        // A new session proves the shared endpoint survived closing the first view.
        let (third, incoming) = tokio::join!(
            Store::connect(&client, &ticket, Snapshot::default(), None),
            scoped_incoming(&host, &trust)
        );
        let third = third.unwrap();
        let (incoming, _stream) = incoming;
        assert!(stores[1].snapshot().connected);
        stores[1].close().await.unwrap();
        third.close().await.unwrap();
        for (session, _stream) in sessions {
            session.close();
        }
        incoming.close();
        client.close().await;
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn navigation_invalidates_all_view_reads_and_their_errors() {
    for (intent, output) in [
        (
            Intent::ReadFile(op::ReadFile {
                path: "/old/file".into(),
                discard_draft: false,
            }),
            file("/old/file", "r1", "old"),
        ),
        (
            Intent::ListFiles(op::ListFiles {
                path: "/old".into(),
            }),
            json!({"path":"/old","entries":[],"truncated":false}),
        ),
        (
            Intent::ReviewWorkspace(op::ReviewWorkspace { cwd: "/old".into() }),
            json!({"branch":"main","additions":0,"deletions":0,"files":[],"diff":"old"}),
        ),
        (
            Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}),
            json!({"createOnNewSession":false,"copyOnCreate":false,"copyPaths":[],"worktreeDirectory":"old"}),
        ),
        (
            Intent::ListAccounts(op::ListAccounts {}),
            json!({"accounts":[],"selectedId":null,"error":null}),
        ),
        (
            Intent::ListThreads(op::ListThreads::new(Default::default())),
            json!({"data":[{"id":"old"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
        ),
    ] {
        let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
        let loading = store.dispatch(intent);
        let request = read(&mut reader).await;
        new_chat(&store, &mut reader, &mut writer, "/new").await;
        let navigated = store.snapshot();
        writer
            .write_line(&json!({"id":request["id"],"result":output}).to_string())
            .await
            .unwrap();
        loading.await.unwrap();
        assert_eq!(
            store.snapshot(),
            navigated,
            "old {} updated the new view",
            request["method"]
        );
        store.close().await.unwrap();
    }
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let loading = store.dispatch(Intent::ListFiles(op::ListFiles {
        path: "/old".into(),
    }));
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/new").await;
    writer
        .write_line(
            &json!({"id":request["id"],"error":{"code":-32000,"message":"old failure"}})
                .to_string(),
        )
        .await
        .unwrap();
    assert!(loading.await.is_err());
    assert!(store.snapshot().error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn saving_after_navigation_rebases_newer_edits_without_restoring_the_old_file() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.workspace).file = Some(Arc::new(
        serde_json::from_value(file("/old/file", "base", "old")).unwrap(),
    ));
    let (store, mut reader, mut writer) = setup(initial).await;
    store
        .dispatch(Intent::SetFileDraft {
            path: "/old/file".into(),
            text: "submitted".into(),
        })
        .await
        .unwrap();
    let saving = store.dispatch(Intent::SaveFile(op::SaveFile {
        path: "/old/file".into(),
    }));
    let request = read(&mut reader).await;
    store
        .dispatch(Intent::SetFileDraft {
            path: "/old/file".into(),
            text: "newer".into(),
        })
        .await
        .unwrap();
    new_chat(&store, &mut reader, &mut writer, "/new").await;
    writer
        .write_line(
            &json!({"id":request["id"],"result":file("/old/file", "saved", "submitted")})
                .to_string(),
        )
        .await
        .unwrap();
    saving.await.unwrap();
    let saved = store.snapshot();
    assert_eq!(saved.file_drafts["/old/file"].text, "newer");
    assert_eq!(saved.file_drafts["/old/file"].revision, "saved");
    assert!(saved.workspace.file.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn connection_loads_workspace_and_lists_in_one_epoch() {
    let initial = Snapshot {
        navigation: Arc::new(agent_core::state::Navigation {
            cwd: "/fixture".into(),
            draft_key: "new:/fixture".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (store, mut reader, mut writer) = connected(initial);
    let epoch = store.snapshot().epoch;
    let mut requests = BTreeMap::new();
    for _ in 0..3 {
        let request = read(&mut reader).await;
        assert!(
            requests
                .insert(request["method"].as_str().unwrap().to_owned(), request)
                .is_none()
        );
    }
    assert_eq!(
        requests["host/workspace/review"]["params"],
        json!({"cwd":"/fixture"})
    );
    assert_eq!(
        requests["host/thread/list"]["params"],
        json!({"projectLimit":5,"chatLimit":5,"projectThreadLimits":{},"searchTerm":""})
    );
    // Review completes first; the other automatic reads must remain current.
    for (method, result) in [
        (
            "host/workspace/review",
            json!({"branch":"main","additions":2,"deletions":1,"files":[],"diff":"fixture diff"}),
        ),
        (
            "model/list",
            json!({"data":[{"id":"fresh","model":"fresh","displayName":"Fresh","defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}],"nextCursor":null}),
        ),
        (
            "host/thread/list",
            json!({"data":[{"id":"listed"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}),
        ),
    ] {
        writer
            .write_line(&json!({"id":requests[method]["id"],"result":result}).to_string())
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        snapshot.workspace.review.is_some()
            && !snapshot.models.is_empty()
            && snapshot.threads.is_some()
    })
    .await;
    let snapshot = store.snapshot();
    assert_eq!(snapshot.epoch, epoch);
    assert_eq!(
        snapshot.workspace.review.as_ref().unwrap().diff,
        "fixture diff"
    );
    assert_eq!(snapshot.models[0].model, "fresh");
    assert_eq!(
        snapshot.threads.as_ref().unwrap().data[0].id.as_deref(),
        Some("listed")
    );
    store.close().await.unwrap();
}
