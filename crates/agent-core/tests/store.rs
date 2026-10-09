use agent_core::state::DraftKey;
use agent_protocol::session::{ProviderKind, SessionRef};
#[path = "support/host.rs"]
mod host_fixture;
async fn scoped_incoming(
    host: &agent_transport::transport::Endpoint,
    trust: &agent_transport::transport::Trust,
) -> (
    host_fixture::Session,
    host_fixture::Reader,
    host_fixture::Writer,
) {
    let session = host
        .accept()
        .await
        .unwrap()
        .unwrap()
        .authorize(trust)
        .unwrap();
    let (session, mut reader, writer) = host_fixture::accept(session).await;
    let request = reader.read_request().await.unwrap().unwrap();
    assert_eq!(request["method"], "host/session/scope");
    writer
        .reply(&request, serde_json::json!({"result":"fixture-storage"}))
        .await
        .unwrap();
    (session, reader, writer)
}
use agent_core::{
    state::{Draft, Intent, Snapshot, operations as op},
    store::{Outcome, Store},
};
use agent_protocol::{models::Thread, requests::Answer};
use agent_transport::peer::PeerError;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[tokio::test]
async fn dictation_preparation_keeps_audio_until_stop_and_orders_cancel_after_start() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let preparation = store.prepare_dictation();
    let id = preparation.id();
    let before = store.snapshot();
    let start = read(&mut reader).await;
    assert_eq!(start["method"], "host/dictation/prepare");
    assert_eq!(start["params"], json!({"id":id}));
    drop(preparation);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), reader.read_request())
            .await
            .is_err(),
        "cancellation must wait for the preparation request to finish"
    );
    writer.reply(&start, json!({"result":{}})).await.unwrap();
    let cancel = read(&mut reader).await;
    assert_eq!(cancel["method"], "host/dictation/cancel");
    assert_eq!(cancel["params"], json!({"id":id}));
    writer.reply(&cancel, json!({"result":{}})).await.unwrap();
    assert_eq!(store.snapshot().drafts, before.drafts);
    assert_eq!(store.snapshot().epoch, before.epoch);
    assert!(store.snapshot().error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn dictation_preparation_failure_does_not_block_complete_recording_transcription() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let preparation = store.prepare_dictation();
    let start = read(&mut reader).await;
    writer.reply(&start, json!({"error":{"code":"unavailable","message":"preparation failed","delivery":"notSent"}})).await.unwrap();
    let cancel = read(&mut reader).await;
    assert_eq!(cancel["method"], "host/dictation/cancel");
    writer.reply(&cancel, json!({"result":{}})).await.unwrap();
    assert!(store.snapshot().error.is_none());
    let operation = store.dispatch(Intent::Transcribe(op::Dictate {
        draft_key: DraftKey::from("/fixture"),
        preparation: Some(preparation.id()),
        audio: vec![1, 0, 255, 127],
        send: false,
        client_user_message_id: "spoken".into(),
    }));
    let transcribe = read(&mut reader).await;
    assert_eq!(transcribe["method"], "host/dictation/transcribe");
    assert_eq!(
        transcribe["params"],
        json!({"preparation":preparation.id(),"audio":"AQD/fw=="})
    );
    writer
        .reply(&transcribe, json!({"result":{"text":"spoken"}}))
        .await
        .unwrap();
    operation.await.unwrap();
    assert_eq!(
        store.snapshot().drafts[&DraftKey::from("/fixture")].text,
        "spoken"
    );
    drop(preparation);
    store.close().await.unwrap();
}

#[tokio::test]
async fn saturated_dictation_preparations_leave_complete_recording_transcription_available() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let preparations: Vec<_> = (0..32).map(|_| store.prepare_dictation()).collect();
    for _ in &preparations {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/prepare");
        writer.reply(&request, json!({"result":{}})).await.unwrap();
    }
    let overflow = store.prepare_dictation();
    let transcription = store.dispatch(Intent::Transcribe(op::Dictate {
        draft_key: DraftKey::from("/fixture"),
        preparation: Some(overflow.id()),
        audio: vec![1, 0, 255, 127],
        send: false,
        client_user_message_id: "spoken".into(),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/dictation/transcribe");
    assert_eq!(
        request["params"],
        json!({"preparation":overflow.id(),"audio":"AQD/fw=="})
    );
    writer
        .reply(&request, json!({"result":{"text":"spoken"}}))
        .await
        .unwrap();
    transcription.await.unwrap();
    assert_eq!(
        store.snapshot().drafts[&DraftKey::from("/fixture")].text,
        "spoken"
    );
    assert!(store.snapshot().error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn list_refresh_bursts_keep_only_the_latest_refresh_without_blocking_navigation() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let first = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        Default::default(),
    )));
    let first_request = read(&mut reader).await;
    assert_eq!(first_request["method"], "host/session/list");
    let navigation_epoch = store.snapshot().epoch;
    let mut receipts = Vec::new();
    for _ in 0..40 {
        receipts.push(store.dispatch(Intent::ListSessions(op::ListSessions::new(
            (*store.snapshot().list_query).clone(),
        ))));
    }
    assert_eq!(
        store.snapshot().epoch,
        navigation_epoch,
        "list queries must not invalidate conversation reads"
    );
    let id = SessionRef::new(ProviderKind::Codex, "selected".to_owned()).unwrap();
    let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())));
    assert_eq!(store.snapshot().navigation.thread_id.as_ref(), Some(&id));
    let open_request = read_after_status(&mut reader, &writer).await;
    assert_eq!(
        open_request["method"], "host/session/open",
        "refreshes must not fan out while the first list is pending"
    );
    receipts.push(store.dispatch(Intent::ListSessions(op::ListSessions::new(
        (*store.snapshot().list_query).clone(),
    ))));
    writer
        .reply(
            &open_request,
            json!({"result":{"thread":{"id":id,"status":"idle","turns":[]}}}),
        )
        .await
        .unwrap();
    opening.await.unwrap();
    assert!(
        store.snapshot().subscriptions.contains_key(&id),
        "expanding projects must retain the in-flight conversation subscription"
    );
    writer.reply(&first_request, json!({"result":{"data":[],"projects":[{"id":"obsolete","name":"Old project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}})).await.unwrap();
    first.await.unwrap();
    assert!(
        store
            .snapshot()
            .threads
            .as_ref()
            .unwrap()
            .projects
            .is_empty(),
        "an older query must not replace the expanded list"
    );
    let latest = read_after_status(&mut reader, &writer).await;
    assert_eq!(latest["method"], "host/session/list");
    assert_eq!(latest["params"]["limit"], 5);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            read_after_status(&mut reader, &writer)
        )
        .await
        .is_err()
    );
    writer.reply(&latest, json!({"result":{"data":[],"projects":[{"id":"latest","name":"Expanded project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}})).await.unwrap();
    for receipt in receipts {
        receipt.await.unwrap();
    }
    assert_eq!(
        store.snapshot().threads.as_ref().unwrap().projects[0].id,
        "latest"
    );
    assert_eq!(store.snapshot().navigation.thread_id.as_ref(), Some(&id));

    // A list result remains useful after a navigation-only epoch change.
    let refresh = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        (*store.snapshot().list_query).clone(),
    )));
    let request = read(&mut reader).await;
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    writer.reply(&request, json!({"result":{"data":[],"projects":[{"id":"after-navigation","name":"Current project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}})).await.unwrap();
    refresh.await.unwrap();
    assert_eq!(
        store.snapshot().threads.as_ref().unwrap().projects[0].id,
        "after-navigation"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn workspace_refresh_bursts_keep_the_latest_directory_and_leave_lists_available() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let first = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        cwd: "/old".into(),
    }));
    let first_request = read(&mut reader).await;
    assert_eq!(first_request["method"], "host/workspace/review");
    let mut receipts = Vec::new();
    for index in 0..40 {
        receipts.push(store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
            cwd: format!("/latest/{index}"),
        })));
    }
    let listing = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        Default::default(),
    )));
    let list_request = read(&mut reader).await;
    assert_eq!(
        list_request["method"], "host/session/list",
        "workspace refreshes must not fan out or block independent reads"
    );
    writer
        .reply(
            &list_request,
            json!({"result":{"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}}),
        )
        .await
        .unwrap();
    listing.await.unwrap();
    writer
        .reply(&first_request, json!({"result":review()}))
        .await
        .unwrap();
    first.await.unwrap();
    assert!(
        store.snapshot().workspace.review.is_none(),
        "an obsolete directory must not publish its diff"
    );
    let latest = read_after_status(&mut reader, &writer).await;
    assert_eq!(latest["method"], "host/workspace/review");
    assert_eq!(latest["params"], json!({"cwd":"/latest/39"}));
    let mut current_review = review();
    current_review["branch"] = "latest".into();
    writer
        .reply(&latest, json!({"result":current_review}))
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        futures_util::future::try_join_all(receipts),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        store.snapshot().workspace.review_cwd.as_deref(),
        Some("/latest/39")
    );
    assert_eq!(
        store.snapshot().workspace.review.as_ref().unwrap().branch,
        "latest"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn failed_list_refresh_releases_the_next_request_and_close_releases_its_waiters() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let refresh = || {
        store.dispatch(Intent::ListSessions(op::ListSessions::new(
            Default::default(),
        )))
    };
    let first = refresh();
    let request = read(&mut reader).await;
    let second = refresh();
    let joined = refresh();
    // This independent RPC is also a barrier: the queued refreshes were admitted.
    let models = store.dispatch(Intent::LoadModels(op::LoadModels {}));
    let model_request = read_after_status(&mut reader, &writer).await;
    assert_eq!(model_request["method"], "host/model/list");
    writer
        .reply(
            &model_request,
            json!({"result":{"data":[],"nextCursor":null}}),
        )
        .await
        .unwrap();
    models.await.unwrap();
    writer.reply(&request, json!({"error":{"code":"provider_failed","message":"list failed","delivery":"notSent"}})).await.unwrap();
    assert!(first.await.is_err());
    let second_request = read_after_status(&mut reader, &writer).await;
    assert_eq!(second_request["method"], "host/session/list");
    let pending = refresh();
    store.close().await.unwrap();
    let superseded = second.await;
    assert!(matches!(superseded, Ok(Outcome::Applied) | Err(_)));
    assert!(joined.await.is_err());
    assert!(pending.await.is_err());
}

#[tokio::test]
async fn browser_frames_are_ephemeral_and_pending_reads_end_with_the_connection() {
    use agent_protocol::browser::{BrowserAction, BrowserFrame, BrowserRequest};
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let snapshot = store.snapshot();
    let request = BrowserRequest {
        thread_id: SessionRef {
            provider: ProviderKind::Codex,
            id: "browser-thread".into(),
        },
        tab_id: String::new(),
        image_id: String::new(),
        action: BrowserAction::Read,
    };
    let received = tokio::spawn({
        let store = store.clone();
        let request = request.clone();
        async move { store.browser(request).await }
    });
    let rpc = read(&mut reader).await;
    assert_eq!(rpc["method"], "host/browser");
    let frame = BrowserFrame {
        image: vec![1, 2, 3],
        ..Default::default()
    };
    writer.reply(&rpc, json!({"result":frame})).await.unwrap();
    assert_eq!(received.await.unwrap().unwrap(), frame);
    assert!(
        Arc::ptr_eq(&snapshot, &store.snapshot()),
        "browser frames must not update or persist conversation state"
    );
    assert!(
        store
            .browser(BrowserRequest {
                action: BrowserAction::Navigate {
                    url: "file:///private/data".into()
                },
                ..request.clone()
            })
            .await
            .is_err()
    );
    assert!(
        Arc::ptr_eq(&snapshot, &store.snapshot()),
        "input validation must not contaminate conversation errors"
    );
    let pending = tokio::spawn({
        let store = store.clone();
        async move { store.browser(request).await }
    });
    assert_eq!(read(&mut reader).await["method"], "host/browser");
    store.close().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

fn authenticated_snapshot() -> Snapshot {
    Snapshot {
        models: Arc::new(
            serde_json::from_value(json!([{
                "id":"fixture","model":{"provider":"codex","id":"fixture"},"displayName":"Fixture",
                "isDefault":true,"defaultReasoningEffort":"","supportedReasoningEfforts":[]
            }]))
            .unwrap(),
        ),
        account: Arc::new(agent_core::state::AccountState {
            accounts: Some(Arc::new(
                serde_json::from_value(json!({
                    "accounts":[{"id":"account","provider":"codex"}],"selected":{"codex":"account"}
                }))
                .unwrap(),
            )),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn connected(snapshot: Snapshot) -> (Arc<Store>, host_fixture::Reader, host_fixture::Writer) {
    let (peer, reader, writer) = host_fixture::connect(&snapshot).await;
    (Arc::new(Store::new(peer, snapshot)), reader, writer)
}
async fn setup(snapshot: Snapshot) -> (Arc<Store>, host_fixture::Reader, host_fixture::Writer) {
    let (store, mut reader, writer) = connected(snapshot.clone()).await;
    let selected = snapshot
        .navigation
        .thread_id
        .as_ref()
        .map(|id| &snapshot.conversations[id]);
    let cwd = selected.map_or(snapshot.navigation.cwd.as_str(), |thread| {
        thread.cwd.as_deref().unwrap_or_default()
    });
    for _ in 0..1
        + usize::from(snapshot.threads.is_none())
        + usize::from(snapshot.models.is_empty())
        + usize::from(snapshot.account.accounts.is_none())
        + usize::from(selected.is_some())
        + usize::from(!cwd.is_empty())
    {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/session/list" => snapshot
                .threads
                .as_ref()
                .map(|threads| json!(threads))
                .unwrap_or_else(
                    || json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
                ),
            "host/account/list" => snapshot
                .account
                .accounts
                .as_ref()
                .map(|accounts| json!(accounts))
                .unwrap_or_else(|| json!({"accounts":[],"selected":{}})),
            "host/account/usage" => json!({"windows":[],"fetchedAt":1,"error":null}),
            "host/taskActivity/read" => {
                json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
            }
            "host/model/list" => json!({"data":snapshot.models,"nextCursor":null}),
            "host/session/open" => {
                json!({"thread": snapshot.conversations[snapshot.navigation.thread_id.as_ref().unwrap()]})
            }
            "host/workspace/review" => {
                assert_eq!(request["params"]["cwd"], cwd);
                review()
            }
            method => panic!("unexpected connection request: {method}"),
        };
        writer
            .reply(&request, json!({ "result":result}))
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
            .reply(&request, json!({"result":{"thread":thread}}))
            .await
            .unwrap();
        opening.await.unwrap();
    }
    wait_for(&store, |current| {
        current.threads.is_some()
            && current.task_activity.is_some()
            && (cwd.is_empty() || current.workspace.review.is_some())
    })
    .await;
    (store, reader, writer)
}
async fn read(reader: &mut host_fixture::Reader) -> host_fixture::Request {
    tokio::time::timeout(Duration::from_secs(2), reader.read_request())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn read_after_status(
    reader: &mut host_fixture::Reader,
    writer: &host_fixture::Writer,
) -> host_fixture::Request {
    loop {
        let request = read(reader).await;
        if request["method"] != "host/taskActivity/read" {
            return request;
        }
        writer.reply(&request, json!({"result":{"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
    }
}
fn review() -> Value {
    json!({"branch":"main","additions":0,"deletions":0,"files":[],"diff":""})
}
async fn new_chat(
    store: &Store,
    reader: &mut host_fixture::Reader,
    writer: &mut host_fixture::Writer,
    cwd: &str,
) {
    let navigation = store.dispatch(Intent::NewChat { cwd: cwd.into() });
    let request = read(reader).await;
    assert_eq!(request["method"], "host/workspace/review");
    assert_eq!(request["params"], json!({"cwd":cwd}));
    writer
        .reply(&request, json!({"result":review()}))
        .await
        .unwrap();
    navigation.await.unwrap();
}
async fn read_after_reviews(
    reader: &mut host_fixture::Reader,
    writer: &mut host_fixture::Writer,
) -> host_fixture::Request {
    loop {
        let request = read(reader).await;
        if request["method"] == "host/session/open" {
            let id = serde_json::from_value(request["params"]["session"].clone()).unwrap();
            let response = writer.current(&id);
            writer
                .reply(&request, json!({ "result":response}))
                .await
                .unwrap();
            continue;
        }
        if request["method"] == "host/session/list" {
            writer
                .reply(
                    &request,
                    json!({"result":{"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}}),
                )
                .await
                .unwrap();
            continue;
        }
        if request["method"] != "host/workspace/review" {
            return request;
        }
        assert_eq!(request["params"], json!({"cwd":"/fixture"}));
        writer
            .reply(&request, json!({"result":review()}))
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
    serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle","turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":text,"phase":"unknown"}}}}}]}]})).unwrap()
}
fn snapshot() -> Snapshot {
    Snapshot {
        conversations: Arc::new(BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            Arc::new(thread("old")),
        )])),
        ..Default::default()
    }
}
fn loaded_text(snapshot: &Snapshot) -> Option<&str> {
    item_text(
        snapshot
            .conversations
            .get(&SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            })?
            .turns
            .as_ref()?
            .first()?
            .items
            .as_ref()?
            .first()?,
    )
}

#[tokio::test]
async fn draft_field_edits_preserve_interleaved_attachments_and_settings() {
    use agent_core::state::Attachment;
    for attachment_first in [false, true] {
        let previous = Snapshot {
            models: Arc::new(serde_json::from_value(json!([{
                "id":"model", "model":{"provider": "codex", "id": "model"}, "displayName":"Model",
                "defaultReasoningEffort":"medium", "defaultServiceTier":"priority",
                "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"max"}],
                "serviceTiers":[{"id":"priority","fast":true}]
            }])).unwrap()),
            drafts: Arc::new(BTreeMap::from([(SessionRef {provider: ProviderKind::Codex, id: "thread".into()}.into(), Arc::new(Draft {
                text: "old".into(), model: Some(agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Codex, id: "model".into() }), effort: Some("medium".into()),
                service_tier: Some("priority".into()), ..Default::default()
            }))])),
            ..Default::default()
        };
        let (store, _reader, _writer) = setup(previous.clone()).await;
        let attachment = Intent::AddAttachment {
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            attachment: Attachment {
                path: "/fixture/image.png".into(),
                name: "image.png".into(),
                is_image: true,
            },
        };
        let text = Intent::SetDraftText {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
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
        drop(
            store.dispatch(Intent::SelectEffort {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                effort: "max".into(),
            }),
        );
        store
            .dispatch(Intent::SelectServiceTier {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                service_tier: "default".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        let draft = &current.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })];
        assert_eq!(draft.text, "new");
        assert_eq!(draft.attachments[0].path, "/fixture/image.png");
        assert_eq!(
            draft.model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "model".into()
            })
        );
        assert_eq!(draft.effort.as_deref(), Some("max"));
        assert_eq!(draft.service_tier.as_deref(), Some("default"));
        assert_eq!(
            previous.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            "old"
        );
        assert!(
            previous.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .attachments
                .is_empty()
        );
        store
            .dispatch(Intent::SelectEffort {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                effort: "invalid".into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectServiceTier {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                service_tier: "invalid".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        assert_eq!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .effort
                .as_deref(),
            Some("medium")
        );
        assert_eq!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .service_tier
                .as_deref(),
            Some("priority")
        );
        assert_eq!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            "new"
        );
        assert_eq!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .attachments
                .len(),
            1
        );
        store.close().await.unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_response_precedes_following_delta_until_subscription_ends() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"text":{"turnId":"turn","itemId":"item","delta":" obsolete","field":"assistantText"}}})).await.unwrap();
        writer
            .reply(&request, json!({"result":{"thread":thread("base")}}))
            .await
            .unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"text":{"turnId":"turn","itemId":"item","delta":" tail","field":"assistantText"}}})).await.unwrap();
        writer.finish_updates().await;
        (writer, reader)
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        store.dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }))),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap(), Outcome::Applied);
    let _connection = server.await.unwrap();
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("base tail")
    })
    .await;
}
#[tokio::test]
async fn approval_can_complete_while_another_request_is_waiting() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let pending = read(&mut reader).await;
        writer.notify(json!({"method": "fixture/session/request", "session": {"provider": "codex", "id": "thread"}, "request": {"id": "approval", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": ""}, {"id": "choice-1", "label": "このセッションで承認", "description": ""}, {"id": "choice-2", "label": "拒否", "description": ""}, {"id": "choice-3", "label": "キャンセル", "description": ""}]}}}})).await.unwrap();
        let answer = read(&mut reader).await;
        assert_eq!(answer["method"], "host/session/answer");
        assert_eq!(
            answer["params"],
            json!({"requestId":"approval","answer":{"approval":{"choiceId":"choice-2"}}})
        );
        writer.reply(&answer, json!({"result":{}})).await.unwrap();
        writer
            .reply(
                &pending,
                json!({"result":{"thread":thread("approved path")}}),
            )
            .await
            .unwrap();
        writer
    });
    let loading = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                })))
                .await
        }
    });
    wait_for(&store, |snapshot| snapshot.request("approval").is_some()).await;
    store
        .dispatch(Intent::Respond(op::Respond {
            request_id: "approval".into(),
            answer: Answer::Approval {
                choice_id: "choice-2".into(),
            },
        }))
        .await
        .unwrap();
    assert_eq!(loading.await.unwrap().unwrap(), Outcome::Applied);
    let _writer = server.await.unwrap();
    assert!(store.snapshot().requests().next().is_none());
    assert_eq!(loaded_text(&store.snapshot()), Some("approved path"));
}
#[tokio::test]
async fn approval_reserve_is_bounded_when_ordinary_work_is_full() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    for index in 0..17 {
        writer
            .notify(json!({
                "method": "fixture/session/request",
                "session": {"provider": "codex", "id": "thread"},
                "request": {
                    "id": format!("approval-reserve-{index}"),
                    "target": "session",
                    "delivery": "awaiting",
                    "body": {"approval": {
                        "kind": "command", "description": "", "details": "",
                        "choices": [{"id": "deny", "label": "拒否", "description": ""}]
                    }}
                }
            }))
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        snapshot.request("approval-reserve-16").is_some()
    })
    .await;

    let mut requests = Vec::new();
    let mut receipts = Vec::new();
    for index in 0..32 {
        receipts.push(store.dispatch(Intent::ReadFile(op::ReadFile {
            path: format!("/pending/{index}"),
            discard_draft: false,
        })));
        if index < 16 {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/file/read");
            requests.push(request);
        }
    }
    for index in 0..16 {
        receipts.push(store.dispatch(Intent::Respond(op::Respond {
            request_id: format!("approval-reserve-{index}").into(),
            answer: Answer::Approval {
                choice_id: "deny".into(),
            },
        })));
    }
    let overflow = store
        .dispatch(Intent::Respond(op::Respond {
            request_id: "approval-reserve-16".into(),
            answer: Answer::Approval {
                choice_id: "deny".into(),
            },
        }))
        .await
        .unwrap_err();
    assert!(matches!(overflow, PeerError::InvalidMessage(message) if message.contains("混み合")));
    for index in 0..16 {
        assert!(
            store
                .snapshot()
                .operation_running(op::OperationKey::Request {
                    id: format!("approval-reserve-{index}").into(),
                })
        );
    }

    for request in requests {
        writer
            .reply(&request, json!({"result":file(request["params"]["path"].as_str().unwrap(), "r1", "content")}))
            .await
            .unwrap();
    }
    let mut answers = 0;
    for _ in 0..32 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/file/read" => file(request["params"]["path"].as_str().unwrap(), "r1", "content"),
            "host/session/answer" => {
                answers += 1;
                json!({})
            }
            method => panic!("unexpected request: {method}"),
        };
        writer
            .reply(&request, json!({"result":result}))
            .await
            .unwrap();
    }
    assert_eq!(answers, 16);
    for receipt in receipts {
        receipt.await.unwrap();
    }
    assert!(store.snapshot().request("approval-reserve-16").is_some());
    store.close().await.unwrap();
}

#[tokio::test]
async fn invalid_typed_reply_does_not_block_later_wire_events() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer
            .reply(&request, json!({"result":{"thread":{"cwd":"/fixture"}}}))
            .await
            .unwrap();
        let request = read(&mut reader).await;
        writer
            .reply(&request, json!({"result":{"thread":thread("recovered")}}))
            .await
            .unwrap();
        writer
    });
    let error = store
        .dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PeerError::InvalidResponse { sequence: None, .. }
    ));
    store
        .dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })))
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
async fn new_conversation_moves_draft_to_pending_before_creation_reply() {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
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
    assert_eq!(request["method"], "host/session/create");
    let pending = store.snapshot();
    assert!(pending.drafts[&key].text.is_empty());
    let source = pending.conversation_thread().unwrap();
    let rendered =
        agent_core::presentation::conversation::project_conversation(&pending, source, &None);
    assert_eq!(rendered.queued.len(), 1);
    assert_eq!(
        rendered.queued[0].data.body.as_deref(),
        Some("first message")
    );
    assert_eq!(rendered.queued[0].data.title.as_deref(), Some("送信中…"));
    writer.reply(&request, json!({ "result": {"thread": {"id":{"provider":"codex","id":"created"}, "cwd":"/fixture", "status":"idle", "turns":[]}}})).await.unwrap();
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/submit");
    assert_eq!(
        request["params"]["input"][0]["text"]["text"],
        "first message"
    );
    writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"created"},"change":{"item":{"turnId":"turn","item":{"id":"native","status":"unknown","clientInputId":"client","body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":"first message"}}]}}}}}}}})).await.unwrap();
    writer
        .reply(&request, json!({ "result":{"turn":{"id":"turn"}}}))
        .await
        .unwrap();
    sending.await.unwrap();
    let current = store.snapshot();
    assert_eq!(
        current.navigation.draft_key,
        DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "created".into()
        })
    );
    assert!(
        current.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "created".into()
        })]
            .text
            .is_empty()
    );
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
                SessionRef { provider: ProviderKind::Codex, id: "thread".into() },
                Arc::new(
                    serde_json::from_value(
                        json!({"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle","turns":[]}),
                    )
                    .unwrap(),
                ),
            )])),
            ..Default::default()
        };
        let (store, mut reader, mut writer) = setup(initial).await;
        store
            .dispatch(Intent::SetDraft {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: sent,
            })
            .await
            .unwrap();
        let sending = store.dispatch(Intent::Submit {
            thread_id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }),
            client_user_message_id: "fixture-message".into(),
        });
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(request["method"], "host/session/submit");
        assert_eq!(request["params"]["input"][0]["text"]["text"], "sent");
        assert_eq!(request["params"]["serviceTierForTurn"], "priority");
        assert!(
            store.snapshot().drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text
                .is_empty()
        );
        assert!(
            store.snapshot().drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .attachments
                .is_empty()
        );
        store
            .dispatch(Intent::SetDraft {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: serde_json::from_value(case["current"].clone()).unwrap(),
            })
            .await
            .unwrap();
        writer
            .reply(&request, json!({"result":{"turnId":"turn-new"}}))
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
            *store.snapshot().drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })],
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
        let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
        new_chat(&store, &mut reader, &mut writer, "/fixture").await;
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SetDraft {
                thread_id: key.clone(),
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
        assert_eq!(create["method"], "host/session/create");
        assert_eq!(create["params"]["cwd"], "/fixture");
        store
            .dispatch(Intent::SetDraft {
                thread_id: key.clone(),
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
        writer.reply(&create, json!({"result":{"thread":{"id":{"provider":"codex","id":"created"},"cwd":"/fixture","turns":[],"status":"idle"}}})).await.unwrap();
        let submit = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(submit["method"], "host/session/submit");
        assert_eq!(submit["params"]["threadId"]["id"], "created");
        assert_eq!(submit["params"]["input"][0]["text"]["text"], "sent");
        assert!(
            !sending.is_finished(),
            "dispatch must wait for submission, not just thread creation"
        );
        writer
            .reply(&submit, json!({"result":{"turnId":"turn"}}))
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
            assert_eq!(state.drafts[&key].text, "newer");
            assert!(
                state.drafts[&DraftKey::from(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "created".into()
                })]
                    .text
                    .is_empty()
            );
        } else {
            assert_eq!(
                state
                    .navigation
                    .thread_id
                    .as_ref()
                    .map(|session| session.id.as_str()),
                Some("created")
            );
            assert_eq!(
                state.drafts[&DraftKey::from(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "created".into()
                })]
                    .text,
                "newer"
            );
            assert!(!state.drafts.contains_key(&key));
        }
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_new_submission_keeps_retry_at_the_last_successful_step() {
    for fail_creation in [false, true] {
        let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
        new_chat(&store, &mut reader, &mut writer, "/fixture").await;
        store
            .dispatch(Intent::SetDraft {
                thread_id: store.snapshot().navigation.draft_key.clone(),
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
        assert_eq!(create["method"], "host/session/create");
        let failed = if fail_creation {
            create
        } else {
            writer.reply(&create, json!({"result":{"thread":{"id":{"provider":"codex","id":"created"},"cwd":"/fixture","turns":[],"status":"idle"}}})).await.unwrap();
            let submit = read_after_reviews(&mut reader, &mut writer).await;
            assert_eq!(submit["method"], "host/session/submit");
            submit
        };
        writer
            .reply(
                &failed,
                json!({"error":{"code":"request_failed","message":"fixture failure","delivery":"notSent"}}),
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
                "host/session/create"
            } else {
                "host/session/submit"
            }
        );
        if !fail_creation {
            assert_eq!(request["params"]["threadId"]["id"], "created");
        }
        writer
            .reply(
                &request,
                json!({"error":{"code":"request_failed","message":"end fixture"}}),
            )
            .await
            .unwrap();
        assert!(retry.await.unwrap().is_err());
        store.close().await.unwrap();
    }
}

fn file(path: &str, revision: &str, text: &str) -> Value {
    json!({"path":path,"revision":revision,"text":text,"size":text.len()})
}

#[tokio::test]
async fn transcription_preserves_newer_input_and_restores_audio_text_on_send_failure() {
    for fail_send in [false, true] {
        let mut initial = snapshot();
        Arc::make_mut(
            &mut Arc::make_mut(
                Arc::make_mut(&mut initial.conversations)
                    .get_mut(&SessionRef {
                        provider: ProviderKind::Codex,
                        id: "thread".into(),
                    })
                    .unwrap(),
            )
            .turns
            .as_mut()
            .unwrap()[0],
        )
        .status = agent_protocol::execution::TurnStatus::Completed;
        Arc::make_mut(&mut initial.navigation).thread_id = Some(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        });
        Arc::make_mut(&mut initial.navigation).draft_key = SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into();
        Arc::make_mut(&mut initial.activity).active.insert(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            false,
        );
        Arc::make_mut(&mut initial.drafts).insert(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
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
                        draft_key: SessionRef {
                            provider: ProviderKind::Codex,
                            id: "thread".into(),
                        }
                        .into(),
                        preparation: None,
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
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: Draft {
                    text: "newer".into(),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        writer
            .reply(&request, json!({"result":{"text":"spoken"}}))
            .await
            .unwrap();
        let submit = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(submit["method"], "host/session/submit");
        assert_eq!(
            submit["params"]["input"][0]["text"]["text"],
            "original\nspoken"
        );
        assert!(!transcribing.is_finished());
        let reply = if fail_send {
            json!({"error":{"code":"request_failed","message":"send failed","delivery":"notSent"}})
        } else {
            json!({"result":{"turnId":"next"}})
        };
        writer.reply(&submit, reply).await.unwrap();
        assert_eq!(transcribing.await.unwrap().is_err(), fail_send);
        assert_eq!(
            store.snapshot().drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            if fail_send {
                "original\nspoken\nnewer"
            } else {
                "newer"
            }
        );
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn new_chat_dictation_preserves_text_and_images_for_draft_and_direct_send() {
    for direct in [false, true] {
        let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
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
            preparation: None,
            audio: vec![0, 0],
            send: direct,
            client_user_message_id: "dictation".into(),
        }));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/transcribe");
        writer
            .reply(&request, json!({"result":{"text":"spoken"}}))
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
        assert_eq!(start["method"], "host/session/create");
        assert!(start["params"]["cwd"].is_null());
        writer.reply(&start, json!({"result":{"thread":{"id":{"provider":"codex","id":"created"},"cwd":"/fixture","projectId":null,"status":"idle","turns":[]}}})).await.unwrap();
        let submit = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(submit["method"], "host/session/submit");
        let input =
            json!([{"text":{"text":"typed\nspoken"}},{"localImage":{"path":"/fixture/photo.png"}}]);
        assert_eq!(submit["params"]["input"], input);
        writer
            .reply(&submit, json!({"result":{"turnId":"turn"}}))
            .await
            .unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"created"},"change":{"turn":{"turn":{"id":"turn","status":"completed","items":[{"id":"native","status":"unknown","clientInputId":"dictation","body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"done","phase":"final"}}}}}]},"completed":true}}})).await.unwrap();
        assert!(matches!(sending.await.unwrap(), Outcome::Submitted { .. }));
        wait_for(&store, |snapshot| {
            snapshot.conversations[&SessionRef {
                provider: ProviderKind::Codex,
                id: "created".into(),
            }]
                .turns
                .as_ref()
                .is_some_and(|turns| {
                    turns[0].status == agent_protocol::execution::TurnStatus::Completed
                })
        })
        .await;
        let current = store.snapshot();
        assert_eq!(
            current
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("created")
        );
        assert!(current.selected_directory().is_empty());
        assert_eq!(current.navigation.cwd, "/fixture");
        assert!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "created".into()
            })]
                .text
                .is_empty()
        );
        assert!(
            current.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "created".into()
            })]
                .attachments
                .is_empty()
        );
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
                    draft_key: "/fixture".into(),
                    preparation: None,
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
        .reply(&request, json!({"result":{"text":"spoken"}}))
        .await
        .unwrap();
    assert_eq!(transcribing.await.unwrap().unwrap(), Outcome::Applied);
    assert_eq!(
        store.snapshot().drafts[&DraftKey::from("/fixture")].text,
        "spoken"
    );
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
        let key: DraftKey = if existing {
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into()
        } else {
            "/fixture".into()
        };
        let navigation = Arc::make_mut(&mut initial.navigation);
        navigation.thread_id = existing.then(|| SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        });
        navigation.cwd = "/fixture".into();
        navigation.draft_key = key.clone();
        Arc::make_mut(&mut initial.drafts).insert(
            key.clone(),
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
            draft_key: key.clone(),
            preparation: None,
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
            .reply(&request, json!({ "result":{"text":transcript}}))
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
            tokio::time::timeout(Duration::from_millis(50), reader.read_request())
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
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
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
        .reply(
            &second_request,
            json!({"result":file("/second", "2", "second")}),
        )
        .await
        .unwrap();
    second.await.unwrap().unwrap();
    writer
        .reply(
            &first_request,
            json!({"result":file("/first", "1", "first")}),
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
    let (store, mut reader, writer) = setup(initial).await;
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
        .reply(
            &request,
            json!({"result":file("/file", "saved", "submitted")}),
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
        .reply(
            &request,
            json!({"result":file("/file", "saved-again", "newer")}),
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
                "data":[{"id":{"provider":"codex","id":"thread"},"cwd":"/listed","name":"Selected task"}],
                "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
            .unwrap(),
        ));
        Arc::make_mut(&mut initial.drafts).insert(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            Arc::new(Draft {
                text: "Keep this draft".into(),
                ..Default::default()
            }),
        );
        if restored {
            initial = serde_json::from_slice(&serde_json::to_vec(&initial).unwrap()).unwrap();
        }
        let (store, mut reader, writer) = setup(initial).await;
        let refresh = store.dispatch(Intent::ListSessions(op::ListSessions::new(
            Default::default(),
        )));
        let list_request = read(&mut reader).await;
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })));
        let selected = store.snapshot();
        assert_eq!(
            selected
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("thread"),
            "selection must not wait for either RPC (cached={cached}, restored={restored})"
        );
        assert_eq!(
            selected.navigation.draft_key,
            DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })
        );
        assert_eq!(
            selected.navigation.cwd,
            if cached { "/fixture" } else { "/listed" }
        );
        assert_eq!(
            selected.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            "Keep this draft"
        );
        assert_eq!(loaded_text(&selected), cached.then_some("old"));
        let request = read_after_status(&mut reader, &writer).await;
        assert_eq!(request["method"], "host/session/open");
        writer
            .reply(&request, json!({"result":{"thread":thread("latest")}}))
            .await
            .unwrap();
        opening.await.unwrap();
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(store.snapshot().navigation.cwd, "/fixture");
        writer
            .reply(
                &list_request,
                json!({"result":{
                "data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}}),
            )
            .await
            .unwrap();
        refresh.await.unwrap();
        assert_eq!(
            store
                .snapshot()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("thread")
        );
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(
            store.snapshot().drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            "Keep this draft"
        );
        assert!(store.snapshot().error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_failed_open_keeps_selection_and_draft_and_can_retry() {
    for cached in [false, true] {
        let (store, mut reader, writer) = setup(if cached {
            snapshot()
        } else {
            Snapshot::default()
        })
        .await;
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })));
        store
            .dispatch(Intent::SetDraftText {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                text: "Written while loading".into(),
            })
            .await
            .unwrap();
        let request = read(&mut reader).await;
        writer
            .reply(
                &request,
                json!({"error":{"code":"request_failed","message":"history unavailable"}}),
            )
            .await
            .unwrap();
        assert!(opening.await.is_err());
        assert_eq!(
            store
                .snapshot()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
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
        let retry = store.dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })));
        assert!(store.snapshot().error.is_none());
        let request = read(&mut reader).await;
        writer
            .reply(&request, json!({"result":{"thread":thread("recovered")}}))
            .await
            .unwrap();
        retry.await.unwrap();
        let recovered = store.snapshot();
        assert_eq!(loaded_text(&recovered), Some("recovered"));
        assert_eq!(
            recovered.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })]
                .text,
            "Written while loading"
        );
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
                .dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                })))
                .await
        }
    });
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/new-project").await;
    writer
        .reply(&request, json!({"result":{"thread":thread("loaded")}}))
        .await
        .unwrap();
    opening.await.unwrap().unwrap();
    let snapshot = store.snapshot();
    assert!(snapshot.navigation.thread_id.is_none());
    assert_eq!(snapshot.navigation.cwd, "/new-project");
    assert_eq!(
        snapshot.navigation.draft_key,
        DraftKey::from("new:/new-project")
    );
    assert_eq!(loaded_text(&snapshot), Some("loaded"));
}

#[tokio::test]
async fn a_stale_catalogue_does_not_queue_a_completed_thread() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.conversations).insert(
        SessionRef { provider: ProviderKind::Codex, id: "thread".into() },
        Arc::new(
            serde_json::from_value(
                json!({"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle","turns":[]}),
            )
            .unwrap(),
        ),
    );
    initial.threads = Some(Arc::new(serde_json::from_value(json!({"data":[{"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"running"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,})).unwrap()));
    let (store, mut reader, mut writer) = setup(initial).await;
    writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"turn":{"turn":{"id":"completed","status":"completed"},"completed":true}}})).await.unwrap();
    wait_for(&store, |snapshot| {
        snapshot.activity.active.get(&SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }) == Some(&false)
    })
    .await;
    let refresh = read(&mut reader).await;
    assert_eq!(refresh["method"], "host/session/decorations/read");
    // Sending must not wait for the decoration refresh to complete.
    store
        .dispatch(Intent::SetDraft {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
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
                    thread_id: Some(SessionRef {
                        provider: ProviderKind::Codex,
                        id: "thread".into(),
                    }),
                    client_user_message_id: "next-message".into(),
                })
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/submit");
    writer
        .reply(&request, json!({"result":{"turnId":"new-turn"}}))
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
    use agent_protocol::models::ListQuery;
    for failure in [false, true] {
        let (store, mut reader, writer) = setup(Snapshot::default()).await;
        let old = store.dispatch(Intent::ListSessions(op::ListSessions::new(ListQuery {
            search_term: "old".into(),
            ..Default::default()
        })));
        let old_request = read(&mut reader).await;
        let new = store.dispatch(Intent::ListSessions(op::ListSessions::new(ListQuery {
            search_term: "new".into(),
            ..Default::default()
        })));
        let requested = store.snapshot();
        let result = |id| json!({"data":[{"id":{"provider":"codex","id":id}}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,});
        writer
        .reply(&old_request, if failure {
            json!({"error":{"code":"provider_failed","message":"old search failed","delivery":"notSent"}})
        } else {
            json!({"result":result("old")})
        })
        .await
        .unwrap();
        assert_eq!(old.await.is_err(), failure);
        assert_eq!(
            store.snapshot(),
            requested,
            "the old query must not publish while the new search is pending"
        );
        let new_request = read(&mut reader).await;
        writer
            .reply(&new_request, json!({"result":result("new")}))
            .await
            .unwrap();
        new.await.unwrap();
        assert_eq!(
            store.snapshot().threads.as_ref().unwrap().data[0]
                .id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("new")
        );
        assert_eq!(store.snapshot().list_query.search_term, "new");
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn gallery_history_reads_do_not_block_conversation_notifications() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/open");
        writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"gallery"},"turns":[{"id":"image-turn","items":[{"id":"image","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}],"status":"unknown"}]}}})).await.unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"text":{"turnId":"turn","itemId":"item","delta":" continued","field":"assistantText"}}})).await.unwrap();
        (reader, writer)
    });
    let result = store
        .dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "gallery".into(),
            },
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
    use agent_core::state::TerminalPhase;
    use agent_protocol::operations::TerminalSize;
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let server = tokio::spawn(async move {
        let start = read(&mut reader).await;
        assert_eq!(start["method"], "host/terminal/start");
        assert_eq!(start["params"]["cwd"], "/fixture");
        for data in ["YQ==", "Yg=="] {
            writer.notify(json!({"method":"host/terminal/output","params":{"processHandle":"terminal","stream":"stdout","deltaBase64":data,"capReached":false}})).await.unwrap();
        }
        writer.reply(&start, json!({"result":{}})).await.unwrap();
        for data in ["Zmlyc3Q=", "c2Vjb25k"] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/terminal/write");
            assert_eq!(request["params"]["deltaBase64"], data);
            writer.reply(&request, json!({"result":{}})).await.unwrap();
        }
        // Keep the transport alive until Store closes it.
        assert!(reader.read_request().await.unwrap().is_none());
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
            .map(|chunk| chunk.data.as_slice())
            .collect::<Vec<_>>(),
        [b"a".as_slice(), b"b".as_slice()]
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
async fn disconnect_does_not_wait_for_a_terminal_start_reply() {
    let (store, mut reader, _writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartTerminal(op::StartTerminal {
        handle: "starting".into(),
        cwd: "/fixture".into(),
        size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
    }));
    assert_eq!(read(&mut reader).await["method"], "host/terminal/start");
    // Keep the response pending: Host cleanup is triggered by connection EOF.
    tokio::time::timeout(Duration::from_secs(1), store.close())
        .await
        .expect("disconnect waited for terminal startup")
        .unwrap();
    assert!(reader.read_request().await.unwrap().is_none());
    assert!(starting.await.is_err());
}

#[tokio::test]
async fn terminal_exit_before_spawn_reply_is_not_replaced_by_running() {
    use agent_core::state::TerminalPhase;
    use agent_protocol::operations::TerminalSize;
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer.notify(json!({"method":"host/terminal/exited","params":{"processHandle":"terminal","exitCode":17,"stdout":"","stderr":"","stdoutCapReached":false,"stderrCapReached":false}})).await.unwrap();
        writer.reply(&request, json!({"result":{}})).await.unwrap();
        assert!(reader.read_request().await.unwrap().is_none());
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
async fn creating_a_chat_updates_matching_search_results_without_reading_the_list() {
    let (store, mut reader, writer) = setup(Snapshot {
        threads: Some(Arc::new(
            serde_json::from_value(
                json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
            )
            .unwrap(),
        )),
        list_query: Arc::new(agent_protocol::models::ListQuery {
            search_term: "created".into(),
            ..Default::default()
        }),
        ..authenticated_snapshot()
    })
    .await;
    let server = tokio::spawn(async move {
        while let Some(request) =
            tokio::time::timeout(Duration::from_secs(2), reader.read_request())
                .await
                .unwrap()
                .unwrap()
        {
            let result = match request["method"].as_str().unwrap() {
                "host/session/create" => {
                    writer.notify(json!({"method":"host/session/updated", "params":{"thread":{"id":{"provider":"codex","id":"created"},"name":"created chat","cwd":"/fixture","listRevision":1},"project":null}})).await.unwrap();
                    json!({"thread":{"id":{"provider":"codex","id":"created"},"name":"created chat","cwd":"/fixture","turns":[],"status":"idle"}})
                }
                "host/session/open" => writer.current(
                    &serde_json::from_value(request["params"]["session"].clone()).unwrap(),
                ),
                "host/session/submit" => json!({"turnId":"turn"}),
                "host/workspace/review" => review(),
                other => panic!("unexpected method {other}"),
            };
            writer
                .reply(&request, json!({"result":result}))
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
            thread_id: store.snapshot().navigation.draft_key.clone(),
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
            page.data.iter().any(|thread| {
                thread.id.as_ref().map(|session| session.id.as_str()) == Some("created")
            })
        })
    })
    .await;
    assert_eq!(store.snapshot().list_query.search_term, "created");
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn expanded_history_failure_preserves_cache_and_retry_adopts_complete_window() {
    let initial: Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"historyLimit":5,"historyHasMore":true,"turns":[{"id":"latest","items":[],"status":"unknown"}]})).unwrap();
    let (store, mut reader, writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            Arc::new(initial),
        )])),
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
                json!({"session":{"provider":"codex","id":"thread"},"limit":10,"includeActivity":false})
            );
            let response = if failed {
                json!({"error":{"code":"request_failed","message":"temporary history failure"}})
            } else {
                json!({"result":{"thread":{"id":{"provider":"codex","id":"thread"},"historyLimit":10,"historyHasMore":false,"turns":[{"id":"old","items":[],"status":"unknown"},{"id":"missing","items":[{"id":"question","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":"comparison"}}]}}}}}],"status":"unknown"},{"id":"latest","items":[],"status":"unknown"}]}}})
            };
            writer.reply(&request, response).await.unwrap();
        }
        (reader, writer)
    });
    assert!(
        store
            .dispatch(Intent::ReadOlder {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into()
                }
            })
            .await
            .is_err()
    );
    assert_eq!(store.snapshot().conversations, cached);
    store
        .dispatch(Intent::ReadOlder {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
        })
        .await
        .unwrap();
    let recovered = store.snapshot();
    let thread = &recovered.conversations[&SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    }];
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
        "question".into()
    );
    assert_eq!(thread.history_has_more, Some(false));
    assert_eq!(recovered.error, None);
    let _server = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn expanded_history_replaces_the_window_preserving_native_item_ids() {
    let initial: Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"historyLimit":5,"historyHasMore":true,"turns":[{"id":"new","status":"completed","items":[{"id":"new-item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"unknown"}}}}}]}]})).unwrap();
    let (store, mut reader, writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            Arc::new(initial),
        )])),
        ..Default::default()
    })
    .await;
    let server = tokio::spawn(async move {
        for (limit, complete) in [(10, false), (15, true)] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/session/open");
            assert_eq!(request["params"]["limit"], limit);
            let items = if complete {
                json!([{"id":"first-old","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"head","phase":"unknown"}}}}},{"id":"last-old","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"tail","phase":"unknown"}}}}}])
            } else {
                json!([{"id":"last-old","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"tail","phase":"unknown"}}}}}])
            };
            writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"thread"},"historyLimit":limit,"historyHasMore":!complete,"turns":[{"id":"old","items":items,"status":"unknown"},{"id":"new","status":"completed","items":[{"id":"new-item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"unknown"}}}}}]}]}}})).await.unwrap();
        }
        (reader, writer)
    });
    for _ in 0..2 {
        store
            .dispatch(Intent::ReadOlder {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                },
            })
            .await
            .unwrap();
    }
    let snapshot = store.snapshot();
    let thread = &snapshot.conversations[&SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    }];
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
        item_text(&(turns[1].items.as_ref().unwrap()[0])),
        Some("new")
    );
    assert_eq!(thread.history_has_more, Some(false));
    let _server = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn fork_opens_the_returned_thread_and_keeps_later_deltas() {
    for navigate in [false, true] {
        let (store, mut reader, mut writer) = setup(snapshot()).await;
        let fork = store.dispatch(Intent::ForkSession(op::ForkSession::new(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            "turn".into(),
        )));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/fork");
        assert_eq!(request["params"]["lastTurnId"], "turn");
        if navigate {
            new_chat(&store, &mut reader, &mut writer, "/new").await;
        }
        writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"forked"},"cwd":"/fixture","turns":[{"id":"copy","items":[{"id":"reply","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"copied","phase":"unknown"}}}}}],"status":"unknown"}]}}})).await.unwrap();
        if !navigate {
            loop {
                let opening = read(&mut reader).await;
                if opening["method"] == "host/session/open" {
                    writer
                        .reply(&opening, json!({"result":writer.current(&SessionRef { provider: ProviderKind::Codex, id: "forked".into() })}))
                        .await
                        .unwrap();
                    break;
                }
                let result = if opening["method"] == "host/workspace/review" {
                    review()
                } else {
                    json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,})
                };
                writer
                    .reply(&opening, json!({"result":result}))
                    .await
                    .unwrap();
            }
            writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"forked"},"change":{"text":{"turnId":"copy","itemId":"reply","delta":" later","field":"assistantText"}}})).await.unwrap();
        }
        assert_eq!(
            fork.await.unwrap(),
            Outcome::StartedThread {
                id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "forked".into()
                }
            }
        );
        wait_for(&store, |s| {
            s.conversations
                .get(&SessionRef {
                    provider: ProviderKind::Codex,
                    id: "forked".into(),
                })
                .and_then(|t| t.turns.as_ref())
                .is_some_and(|turns| {
                    item_text(&(turns[0].items.as_ref().unwrap()[0]))
                        == Some(if navigate { "copied" } else { "copied later" })
                })
        })
        .await;
        assert_eq!(
            store
                .snapshot()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
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
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let request = read(&mut reader).await;
    writer.reply(&request, json!({"result":{"accounts":[{"provider":"codex","id":"a"},{"provider":"claude","id":"claude:c"}],"selected":{"codex":"a","claude":"claude:c"},"error":null}})).await.unwrap();
    listing.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected
            .get(&agent_protocol::session::ProviderKind::Codex)
            .map(String::as_str),
        Some("a")
    );
    // The listing receipt resolves while both usage reads are still pending.
    let usage_a = read(&mut reader).await;
    let usage_c = read(&mut reader).await;
    assert_eq!(usage_a["method"], "host/account/usage");
    assert_eq!(usage_c["method"], "host/account/usage");
    let selecting = store.dispatch(Intent::SelectAccount(op::SelectAccount {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "b".into(),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["accountId"], "b");
    writer.reply(&request, json!({"result":{"provider":"codex","selectedId":"b","persistenceError":"store unavailable"}})).await.unwrap();
    selecting.await.unwrap();
    for request in [usage_a, usage_c] {
        writer
            .reply(
                &request,
                json!({"result":{"windows":[],"fetchedAt":1,"error":"unavailable"}}),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        store
            .snapshot()
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected
            .get(&agent_protocol::session::ProviderKind::Codex)
            .map(String::as_str),
        Some("b")
    );
    assert_eq!(store.snapshot().error.as_deref(), Some("store unavailable"));
    assert_eq!(
        store
            .snapshot()
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected
            .get(&agent_protocol::session::ProviderKind::Claude)
            .map(String::as_str),
        Some("claude:c")
    );
    // Selecting a newly logged-in account invalidates the login completion's
    // pending list request, so selection must fetch the new entry itself.
    for _ in 0..2 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/account/list" => {
                json!({"accounts":[{"provider":"codex","id":"a"},{"provider":"codex","id":"b"},{"provider":"claude","id":"claude:c"}],"selected":{"codex":"b","claude":"claude:c"},"error":null})
            }
            "host/model/list" => json!({"data":[]}),
            method => panic!("unexpected account refresh: {method}"),
        };
        writer
            .reply(&request, json!({"result":result}))
            .await
            .unwrap();
    }
    wait_for(&store, |s| {
        s.account.accounts.as_ref().is_some_and(|accounts| {
            accounts.accounts.iter().any(|account| account.id == "b")
                && accounts
                    .selected
                    .get(&agent_protocol::session::ProviderKind::Codex)
                    .map(String::as_str)
                    == Some("b")
        })
    })
    .await;
    store.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_account_listing_preserves_login_and_cancellation_ignores_late_status() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
        provider: ProviderKind::Codex,
    }));
    let request = read(&mut reader).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let list = read(&mut reader).await;
    writer
        .reply(
            &list,
            json!({"result":{"accounts":[],"selected":{},"error":null}}),
        )
        .await
        .unwrap();
    listing.await.unwrap();
    writer.reply(&request, json!({"result":{"provider":"codex","loginId":"login","userCode":"fixture-only","requiresCodeSubmission":false,"verificationUrl":"https://example.invalid"}})).await.unwrap();
    starting.await.unwrap();
    assert_eq!(
        store.snapshot().account.login.as_ref().unwrap().login_id,
        "login"
    );

    let polling = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "login".into(),
        thread_id: None,
    }));
    let poll = read(&mut reader).await;
    let cancelling = store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "login".into(),
    }));
    let cancel = read(&mut reader).await;
    writer.reply(&cancel, json!({"result":{}})).await.unwrap();
    cancelling.await.unwrap();
    writer
        .reply(
            &poll,
            json!({"result":{"completed":true,"accountId":"obsolete"}}),
        )
        .await
        .unwrap();
    polling.await.unwrap();
    let state = store.snapshot();
    assert!(state.account.login.is_none());
    assert!(state.account.accounts.as_ref().unwrap().accounts.is_empty());
    store.close().await.unwrap();
}

#[tokio::test]
async fn disconnected_store_keeps_editing_and_persisting_drafts() {
    let (store, reader, writer) = setup(authenticated_snapshot()).await;
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
            thread_id: store.snapshot().navigation.draft_key.clone(),
            text: "切断中の下書き".into(),
        })
        .await
        .unwrap();
    let serialized = serde_json::to_vec(&store.snapshot()).unwrap();
    let restored: Snapshot = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(
        restored.drafts[&restored.navigation.draft_key].text,
        "切断中の下書き"
    );
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
        store.snapshot().drafts[&store.snapshot().navigation.draft_key].text,
        "切断中の下書き"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn initial_titles_wait_for_scope_verification_and_use_the_current_query() {
    use agent_protocol::models::ListQuery;
    use agent_transport::transport::Endpoint;
    use agent_transport::transport::Identity;
    use agent_transport::transport::Relays;
    use agent_transport::transport::Trust;
    for mode in ["ready", "zero-limits", "changed-query", "rejected"] {
        tokio::time::timeout(Duration::from_secs(5), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let trust = Trust { allowed: [client.node_id()].into(), ..Default::default() };
            let store = Arc::new(Store::offline(Snapshot {
                list_query: Arc::new(if mode == "zero-limits" {
                    ListQuery { limit: 0, ..Default::default() }
                } else { ListQuery::default() }),
                ..Default::default()
            }));
            let connecting = tokio::spawn({
                let store = store.clone();
                let client = client.clone();
                let ticket = host.ticket();
                async move { store.reconnect(&client, &ticket, None).await }
            });
            let incoming = host.accept().await.unwrap().unwrap().authorize(&trust).unwrap();
            let (session, mut reader, writer) = host_fixture::accept(incoming).await;
            let scope = read(&mut reader).await;
            assert_eq!(scope["method"], "host/session/scope");
            assert!(tokio::time::timeout(Duration::from_millis(40), reader.read_request()).await.is_err(),
                "an unverified candidate must not load titles");
            assert!(!store.snapshot().connected);
            assert!(store.snapshot().threads.is_none());
            let titles = |id| json!({"result":{"data":[{"id":{"provider":"codex","id":id},"name":"title"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}});
            if mode == "changed-query" {
                let _ = store.dispatch(Intent::ListSessions(op::ListSessions::new(ListQuery { search_term: "new query".into(), ..Default::default() }))).await;
            }
            writer.reply(&scope, json!({"result":if mode == "rejected" { "" } else { "verified-storage" }})).await.unwrap();
            let connected = connecting.await.unwrap();
            if mode == "rejected" {
                assert!(connected.is_err());
                assert!(!store.snapshot().connected);
                assert!(store.snapshot().threads.is_none());
                assert!(!matches!(reader.read_request().await, Ok(Some(_))));
            } else {
                connected.unwrap();
                assert!(store.snapshot().connected);
                assert!(store.snapshot().threads.is_none());
                for _ in 0..4 {
                    let request = read(&mut reader).await;
                    let result = match request["method"].as_str().unwrap() {
                        "host/session/list" => {
                            assert_eq!(request["params"]["limit"], 5);
                            assert_eq!(request["params"]["searchTerm"], if mode == "changed-query" { "new query" } else { "" });
                            titles("fresh")["result"].clone()
                        }
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/taskActivity/read" => json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                        method => panic!("unexpected bootstrap: {method}"),
                    };
                    writer.reply(&request, json!({"result": result})).await.unwrap();
                }
                wait_for(&store, |state| state.threads.is_some()).await;
                assert_eq!(store.snapshot().threads.as_ref().unwrap().data[0].id.as_ref().map(|session| session.id.as_str()), Some("fresh"));
            }
            session.close();
            store.close().await.unwrap();
            client.close().await;
            host.close().await;
        }).await.unwrap_or_else(|_| panic!("pipeline timed out: {mode}"));
    }
}

#[tokio::test]
async fn reconnect_preserves_edits_made_during_pairing() {
    use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
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
                thread_id: store.snapshot().navigation.draft_key.clone(),
                text: "接続待ち中の編集".into(),
            })
            .await
            .unwrap();
        let drafts = store.snapshot().drafts.clone();
        let (session, _peer) = pairing.authorize(&trust).await.unwrap();
        let agent_transport::transport::IncomingRequest::Call(mut call) =
            session.accept_request().await.unwrap()
        else {
            panic!("scope request expected")
        };
        assert!(matches!(
            call.call,
            agent_protocol::protocol::Call::SessionScope(_)
        ));
        agent_transport::framing::write(
            &mut call.send,
            agent_protocol::protocol::Response::Success {
                result: Box::new("fixture-storage".to_owned()),
            },
        )
        .await
        .unwrap();
        connecting.await.unwrap().unwrap();
        wait_for(&store, |state| state.connected).await;
        assert!(Arc::ptr_eq(&drafts, &store.snapshot().drafts));
        assert_eq!(
            store.snapshot().drafts[&store.snapshot().navigation.draft_key].text,
            "接続待ち中の編集"
        );
        assert_eq!(store.snapshot().navigation.cwd, "/pairing");
        store.close().await.unwrap();
        session.close();
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reconnect_cancels_obsolete_pairing_and_retains_local_state() {
    use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
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
            assert!(old_reader.read_request().await.unwrap().is_none(), "core must release the old connection before pairing");
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
            if let Ok((session, _peer)) = pending.authorize(&trust).await {
                session.close();
            }
            if let Some((session, mut reader, writer)) = replacement {
                for _ in 0..2 {
                    let request = reader.read_request().await.unwrap().unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "host/session/list" => json!({"data":[{"id":{"provider":"codex","id":"replacement"},"name":"fresh"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/taskActivity/read" => json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        other => panic!("unexpected bootstrap: {other}"),
                    };
                    writer.reply(&request, json!({"result":result})).await.unwrap();
                }
                wait_for(&store, |state| state.task_activity.is_some()).await;
                assert!(store.snapshot().threads.as_ref().unwrap().data.is_empty());
                assert!(store.snapshot().connected);
                assert!(store.snapshot().error.is_none());
                store.close().await.unwrap();
                session.close();
            } else {
                assert!(!store.snapshot().connected, "cancelled setup must remain offline: {action}");
                store.close().await.unwrap();
            }
            assert!(Arc::ptr_eq(&drafts, &store.snapshot().drafts));
            assert_eq!(store.snapshot().drafts[&DraftKey::from("local")].text, "接続待ち中の編集");
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
    let key = store.snapshot().navigation.draft_key.clone();
    assert_ne!(key, Snapshot::default().navigation.draft_key);
    let edit = store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "入力を戻さない".into(),
    });
    assert_eq!(store.snapshot().drafts[&key].text, "入力を戻さない");
    let second = store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "second".into(),
    });
    second.await.unwrap();
    edit.await.unwrap();
    navigation.await.unwrap();
    assert_eq!(store.snapshot().drafts[&key].text, "second");
    drop(store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "last".into(),
    }));
    assert_eq!(store.snapshot().drafts[&key].text, "last");
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
    use agent_core::state::{Activity, Navigation, PendingSubmission};
    let draft = Arc::new(Draft {
        text: "typed\nspoken".into(),
        ..Default::default()
    });
    let mut saved = snapshot();
    saved.connected = true;
    saved.navigation = Arc::new(Navigation {
        thread_id: Some(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }),
        draft_key: SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        ..Default::default()
    });
    saved.activity = Arc::new(Activity {
        active: BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            },
            true,
        )]),
        ..Default::default()
    });
    saved.drafts = Arc::new(BTreeMap::from([(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        Arc::default(),
    )]));
    saved.pending_submissions = Arc::new(BTreeMap::from([(
        "unsent".into(),
        Arc::new(PendingSubmission {
            sequence: 0,
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            draft,
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
        }),
    )]));
    Arc::make_mut(
        Arc::make_mut(&mut saved.conversations)
            .get_mut(saved.navigation.thread_id.as_ref().unwrap())
            .unwrap(),
    ).requests = BTreeMap::from([("1".into(), Arc::new(serde_json::from_value(json!({"id": "1", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": ""}, {"id": "choice-1", "label": "このセッションで承認", "description": ""}, {"id": "choice-2", "label": "拒否", "description": ""}, {"id": "choice-3", "label": "キャンセル", "description": ""}]}}})).unwrap()))]);
    let bytes = serde_json::to_vec(&saved).unwrap();
    let (store, _reader, _writer) = connected(serde_json::from_slice(&bytes).unwrap()).await;
    wait_for(&store, |snapshot| snapshot.connected).await;
    let current = store.snapshot();
    assert!(current.requests().next().is_none());
    assert!(current.activity.active.is_empty());
    assert!(current.subscriptions.is_empty());
    assert!(current.pending_submissions["unsent"].delivery_unknown);
    assert_eq!(
        current.pending_submissions["unsent"].draft.text,
        "typed\nspoken"
    );
    assert!(
        current.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text
            .is_empty()
    );
    assert!(
        current.conversations[&SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        }]
            .status
            != agent_protocol::models::SessionStatus::Running
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn close_keeps_the_last_enqueued_draft_and_unconfirmed_send() {
    let (store, mut reader, _writer) = setup(snapshot()).await;
    store
        .dispatch(Intent::SetDraftText {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            text: "unsent".into(),
        })
        .await
        .unwrap();
    drop(store.dispatch(Intent::Submit {
        thread_id: Some(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }),
        client_user_message_id: "unsent".into(),
    }));
    read(&mut reader).await;
    assert!(!store.snapshot().pending_submissions.is_empty());
    drop(
        store.dispatch(Intent::SetDraftText {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            text: "newest edit".into(),
        }),
    );
    store.close().await.unwrap();
    let current = store.snapshot();
    assert!(current.pending_submissions["unsent"].delivery_unknown);
    assert_eq!(current.pending_submissions["unsent"].draft.text, "unsent");
    assert_eq!(
        current.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text,
        "newest edit"
    );
}

#[tokio::test]
async fn stores_share_an_endpoint_without_closing_each_others_transport() {
    use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
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
        let (incoming, _reader, _writer) = incoming;
        assert!(stores[1].snapshot().connected);
        stores[1].close().await.unwrap();
        third.close().await.unwrap();
        for (session, _reader, _writer) in sessions {
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
    let directory = std::env::temp_dir()
        .join("old")
        .to_string_lossy()
        .into_owned();
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
                path: directory.clone(),
            }),
            json!({"path":directory,"entries":[],"truncated":false}),
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
            json!({"accounts":[],"selected":{},"error":null}),
        ),
    ] {
        let (store, mut reader, writer) = setup(Snapshot::default()).await;
        let loading = store.dispatch(intent);
        let request = read(&mut reader).await;
        let navigation = store.dispatch(Intent::NewChat { cwd: "/new".into() });
        let navigated = store.snapshot();
        writer
            .reply(&request, json!({"result":output}))
            .await
            .unwrap();
        loading.await.unwrap();
        assert_eq!(
            store.snapshot(),
            navigated,
            "old {} updated the new view",
            request["method"]
        );
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/workspace/review");
        assert_eq!(request["params"], json!({"cwd":"/new"}));
        writer
            .reply(&request, json!({"result":review()}))
            .await
            .unwrap();
        navigation.await.unwrap();
        store.close().await.unwrap();
    }
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let loading = store.dispatch(Intent::ListFiles(op::ListFiles {
        path: directory.clone(),
    }));
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/new").await;
    writer
        .reply(
            &request,
            json!({"error":{"code":"request_failed","message":"old failure"}}),
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
        .reply(
            &request,
            json!({"result":file("/old/file", "saved", "submitted")}),
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
async fn opening_a_draft_during_initial_catalog_reads_retries_and_selects_a_model() {
    let (store, mut reader, writer) = connected(Snapshot::default()).await;
    let mut pending = BTreeMap::new();
    for _ in 0..4 {
        let request = read(&mut reader).await;
        pending.insert(request["method"].as_str().unwrap().to_owned(), request);
    }
    writer.reply(&pending["host/taskActivity/read"], json!({"result":{"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
    writer
        .reply(
            &pending["host/session/list"],
            json!({"result":{
        "data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| state.threads.is_some()).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    writer
        .reply(&pending["host/model/list"], json!({"result":{"data":[]}}))
        .await
        .unwrap();
    writer
        .reply(
            &pending["host/account/list"],
            json!({"result":{"accounts":[],"selected":{}}}),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/model/list" => json!({"data":[{
                "id":"fresh","model":{"provider": "codex", "id": "fresh"},"displayName":"Fresh model",
                "isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]
            }]}),
            "host/account/list" => {
                json!(authenticated_snapshot().account.accounts.as_ref().unwrap())
            }
            method => panic!("unexpected retry: {method}"),
        };
        writer
            .reply(&request, json!({"result":result}))
            .await
            .unwrap();
    }
    wait_for(&store, |state| {
        state.drafts[&state.navigation.draft_key].model.as_ref()
            == Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "fresh".into(),
            })
            && state.account.accounts.is_some()
    })
    .await;
    assert_eq!(store.snapshot().models[0].display_name, "Fresh model");
    store.close().await.unwrap();
}

#[tokio::test]
async fn connection_loads_workspace_and_lists_in_one_epoch() {
    let initial = Snapshot {
        navigation: Arc::new(agent_core::state::Navigation {
            cwd: "/fixture".into(),
            draft_key: "/fixture".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (store, mut reader, writer) = connected(initial).await;
    let epoch = store.snapshot().epoch;
    let mut requests = BTreeMap::new();
    for _ in 0..5 {
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
        requests["host/session/list"]["params"],
        serde_json::to_value(agent_protocol::models::ListQuery::default()).unwrap()
    );
    // Review completes first; the other automatic reads must remain current.
    for (method, result) in [
        (
            "host/workspace/review",
            json!({"branch":"main","additions":2,"deletions":1,"files":[],"diff":"fixture diff"}),
        ),
        ("host/account/list", json!({"accounts":[],"selected":{}})),
        (
            "host/taskActivity/read",
            json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
        ),
        (
            "host/model/list",
            json!({"data":[{"id":"fresh","model":{"provider": "codex", "id": "fresh"},"displayName":"Fresh","defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}],"nextCursor":null}),
        ),
        (
            "host/session/list",
            json!({"data":[{"id":{"provider":"codex","id":"listed"}}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
        ),
    ] {
        writer
            .reply(&requests[method], json!({"result":result}))
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        snapshot.account.accounts.is_some()
            && snapshot.workspace.review.is_some()
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
    assert_eq!(
        snapshot.models[0].model,
        agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "fresh".into()
        }
    );
    assert_eq!(
        snapshot.threads.as_ref().unwrap().data[0]
            .id
            .as_ref()
            .map(|session| session.id.as_str()),
        Some("listed")
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn item_transfer_releases_wire_order_and_preserves_newer_items() {
    use agent_protocol::session::SessionChange;
    use agent_protocol::session::TextField;
    use agent_transport::transport::Endpoint;
    use agent_transport::transport::Identity;
    use agent_transport::transport::Relays;
    use agent_transport::transport::Trust;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // QUIC uses real network time; keep its timers separate from the Store's
    // virtual transfer deadline so advancing that deadline cannot expire QUIC.
    let network = scopeguard::guard(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
        tokio::runtime::Runtime::shutdown_background,
    );
    tokio::time::timeout(Duration::from_secs(180), async {
        let (host, client) = network.spawn(async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            (host, client)
        }).await.unwrap();
        let trust = Trust { allowed: [client.node_id()].into(), ..Default::default() };
        let ticket = host.ticket();
        let store = Arc::new(Store::offline(Snapshot::default()));
        let connecting = network.spawn({
            let store = store.clone();
            let client = client.clone();
            async move { store.reconnect(&client, &ticket, None).await }
        });
        let accepting = network.spawn({
            let host = host.clone();
            async move { scoped_incoming(&host, &trust).await }
        });
        let (connected, incoming) = tokio::join!(connecting, accepting);
        connected.unwrap().unwrap();
        let (session, mut reader, writer) = incoming.unwrap();
        let output = Arc::new(tokio::sync::Mutex::new(writer));
        let (send, mut requests) = tokio::sync::mpsc::channel(16);
        let subscription_a = uuid::Uuid::new_v4();
        let subscription_b = uuid::Uuid::new_v4();
        let server = tokio::spawn({
            let output = output.clone();
            async move {
                while let Some(request) = reader.read_request().await.unwrap() {
                    let result = match request["method"].as_str().unwrap() {
                        "host/session/list" => json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        "host/taskActivity/read" => json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                        "host/session/open" => {
                            let a = request["params"]["session"]["id"] == "A";
                            json!({"session":request["params"]["session"],"subscriptionId":if a {subscription_a} else {subscription_b},"response":{"thread":{"id":{"provider":"codex","id":if a {"A"} else {"B"}},"turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":if a {json!({"deferred":{"summary":{"commandExecution":{"command":"pwd","cwd":null,"output":"","exitCode":null}}}})} else {json!({"inline":{"body":{"assistantText":{"text":"B prefix","phase":"unknown"}}}})}}]}]}}})
                        }
                        _ => { send.send(request).await.unwrap(); continue; }
                    };
                    output.lock().await.reply(&request, json!({"result":result})).await.unwrap();
                }
            }
        });
        for id in ["A", "B"] {
            store.dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {provider:ProviderKind::Codex,id:id.into()}))).await.unwrap();
        }
        let read_item = op::ReadItem {thread_id:SessionRef { provider: ProviderKind::Codex, id: "A".into() },turn_id:"turn".into(),item_id:"item".into()};
        let mut reading = Box::pin(store.dispatch(Intent::ReadItem(read_item.clone())));
        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "host/session/item/read");
        let body = agent_protocol::protocol::encode(serde_json::from_value::<agent_protocol::models::Item>(json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"old body","exitCode":null}}}}})).unwrap()).unwrap();
        let grant = json!({"token":([1u8;32]),"sha256":ring::digest::digest(&ring::digest::SHA256,&body).as_ref(),"size":body.len()});
        output.lock().await.reply(&request, json!({"result":{"item":{"id":"item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},"transfer":grant}})).await.unwrap();
        let mut transfer = session.accept_stream().await.unwrap();
        let mut token = [0; 32];
        transfer.read_exact(&mut token).await.unwrap();
        // The binary stream is now an explicit barrier: no body bytes are sent
        // until all the following control operations have completed.
        assert!(futures_util::poll!(&mut reading).is_pending());
        let mut duplicate = Box::pin(store.dispatch(Intent::ReadItem(read_item.clone())));
        let changes = [
            (subscription_b, SessionChange::Text {turn_id:"turn".into(),item_id:"item".into(),field:TextField::AssistantText,delta:" + delta".into()}),
            (subscription_a, SessionChange::Request {request:serde_json::from_value(json!({"id": "approval", "target": {"turn": {"turnId": "turn", "itemId": "item"}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": ""}, {"id": "choice-1", "label": "このセッションで承認", "description": ""}, {"id": "choice-2", "label": "拒否", "description": ""}, {"id": "choice-3", "label": "キャンセル", "description": ""}]}}})).unwrap()}),
        ];
        for (subscription, change) in changes {
            output.lock().await.notify(json!({"method":"host/session/update","params":{"subscriptionId":subscription,"change":change}})).await.unwrap();
        }
        wait_for(&store, |s| s.request("approval").is_some() && item_text(&s.conversations[&SessionRef { provider: ProviderKind::Codex, id: "B".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0]) == Some("B prefix + delta")).await;
        for intent in [
            Intent::Respond(op::Respond {request_id:"approval".into(),answer:Answer::Approval {choice_id: "choice-2".into()}}),
            Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: ProviderKind::Codex, id: "A".into() },turn_id:"turn".into()}),
        ] {
            let completion = store.dispatch(intent);
            let request = requests.recv().await.unwrap();
            assert!(matches!(request["method"].as_str(), Some("host/session/answer" | "host/session/interrupt")));
            output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
            completion.await.unwrap();
        }
        let subscription = store.snapshot().subscriptions[&read_item.thread_id];
        let source = store.snapshot().conversations[&read_item.thread_id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].clone();
        output.lock().await.notify(json!({"method":"host/session/update","params":{"subscriptionId":subscription_a,"change":SessionChange::Text {turn_id:"turn".into(),item_id:"item".into(),field:TextField::CommandOutput,delta:"live suffix".into()}}})).await.unwrap();
        wait_for(&store, |s| !Arc::ptr_eq(&source, &s.conversations[&read_item.thread_id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0])).await;
        assert_eq!(store.snapshot().subscriptions[&read_item.thread_id],subscription);
        assert!(store.snapshot().error.is_none());
        // New deferred metadata invalidates the in-flight source; a complete read must retry.
        output.lock().await.notify(json!({"method":"host/session/update","params":{"subscriptionId":subscription_a,"change":SessionChange::Item {turn_id:"turn".into(),item: serde_json::from_value(json!({"id":"item","status":"running","clientInputId":null,"body":{"deferred":{"summary":{"commandExecution":{"command":"pwd","cwd":null,"output":"new suffix","exitCode":null}}}}})).unwrap()}}})).await.unwrap();
        wait_for(&store, |s| matches!(s.conversations[&SessionRef { provider: ProviderKind::Codex, id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::CommandExecution { output, .. } if output == "new suffix")).await;
        assert!(store.snapshot().conversations[&SessionRef { provider: ProviderKind::Codex, id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].is_deferred());
        assert!(futures_util::poll!(&mut reading).is_pending());
        assert!(futures_util::poll!(&mut duplicate).is_pending());
        transfer.write_all(&body).await.unwrap();
        transfer.shutdown().await.unwrap();
        let retry = requests.recv().await.unwrap();
        assert_eq!(retry["method"], "host/session/item/read");
        output.lock().await.reply(&retry, json!({"result":{"item":{"id":"item","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"complete prefix + new suffix","exitCode":null}}}}}}})).await.unwrap();
        reading.await.unwrap();
        duplicate.await.unwrap();
        let snapshot = store.snapshot();
        let turn = &snapshot.conversations[&SessionRef { provider: ProviderKind::Codex, id: "A".into() }].turns.as_ref().unwrap()[0];
        assert!(matches!(turn.items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::CommandExecution {output, ..} if output == "complete prefix + new suffix"));
        assert!(!turn.items.as_ref().unwrap()[0].is_deferred());
        assert!(requests.try_recv().is_err(), "same-item requests must be coalesced");
        for failure in ["digest", "id", "timeout"] {
            let wrong_id = failure == "id";
            let before = store.snapshot().conversations.clone();
            let mut failed_read = Box::pin(store.dispatch(Intent::ReadItem(read_item.clone())));
            let request = requests.recv().await.unwrap();
            let invalid = agent_protocol::protocol::encode(serde_json::from_value::<agent_protocol::models::Item>(json!({"id":if wrong_id {"different"} else {"item"},"status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"invalid body","exitCode":null}}}}})).unwrap()).unwrap();
            let digest = if wrong_id { ring::digest::digest(&ring::digest::SHA256, &invalid).as_ref().to_vec() } else { vec![0;32] };
            output.lock().await.reply(&request, json!({"result":{"item":{"id":"item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},"transfer":{"token":([2u8;32]),"sha256":digest,"size":invalid.len()}}})).await.unwrap();
            let mut transfer = session.accept_stream().await.unwrap();
            let mut token = [0; 32];
            transfer.read_exact(&mut token).await.unwrap();
            if failure == "timeout" {
                // The real stream has delivered its token and remains stalled.
                // Advance the production deadline without waiting two minutes.
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(60)).await;
                assert!(futures_util::poll!(&mut failed_read).is_pending());
                tokio::time::resume();
                let interrupt = store.dispatch(Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: ProviderKind::Codex, id: "A".into() },turn_id:"turn".into()}));
                let request = requests.recv().await.unwrap();
                assert_eq!(request["method"], "host/session/interrupt");
                output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
                interrupt.await.unwrap();
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::time::resume();
                let result = failed_read.await;
                assert!(matches!(result, Err(PeerError::RequestTimeout { .. })), "{result:?}");
            } else {
                transfer.write_all(&invalid).await.unwrap();
                transfer.shutdown().await.unwrap();
                assert!(matches!(failed_read.await, Err(PeerError::InvalidMessage(_))));
            }
            assert!(Arc::ptr_eq(&before, &store.snapshot().conversations));
            let interrupt = store.dispatch(Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: ProviderKind::Codex, id: "A".into() },turn_id:"turn".into()}));
            let request = requests.recv().await.unwrap();
            assert_eq!(request["method"], "host/session/interrupt");
            output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
            interrupt.await.unwrap();
            assert!(store.snapshot().connected);
        }
        store.close().await.unwrap();
        server.abort();
        session.close();
        client.close().await;
        host.close().await;
    }).await.unwrap();
}

#[rstest::rstest]
#[case::restored(true)]
#[case::read_failed(false)]
#[tokio::test]
async fn session_update_gap_preserves_history_and_only_reports_failed_recovery(
    #[case] restored: bool,
) {
    use agent_protocol::session::{SessionChange, TextField};
    let id = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
    let mut conversation = thread("cached");
    conversation.cwd = None;
    conversation.history_limit = Some(24);
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.conversations).insert(id.clone(), Arc::new(conversation.clone()));
    Arc::make_mut(&mut initial.drafts).insert(
        id.clone().into(),
        Arc::new(Draft {
            text: "unsent draft".into(),
            ..Default::default()
        }),
    );
    let (store, mut reader, writer) = setup(initial).await;
    let old_subscription = store.snapshot().subscriptions[&id];
    writer
        .notify(
            json!({"method":"fixture/session/change", "session":id, "change":SessionChange::Text {
                turn_id: "turn".into(), item_id: "missing".into(),
                field: TextField::AssistantText, delta: "unapplied".into(),
            }}),
        )
        .await
        .unwrap();
    let recovery = read(&mut reader).await;
    assert_eq!(recovery["method"], "host/session/open");
    assert_eq!(recovery["params"]["limit"], 5);
    assert!(!store.snapshot().subscriptions.contains_key(&id));
    assert!(store.snapshot().error.is_none());
    assert_eq!(
        store.snapshot().conversations[&id].turns,
        conversation.turns
    );
    assert_eq!(store.snapshot().conversations[&id].history_limit, Some(24));

    if restored {
        let mut recovered = thread("authoritative");
        recovered.cwd = None;
        recovered.history_limit = Some(24);
        writer
            .reply(&recovery, json!({"result":{"thread":recovered}}))
            .await
            .unwrap();
        wait_for(&store, |state| state.subscriptions.contains_key(&id)).await;
        assert_ne!(store.snapshot().subscriptions[&id], old_subscription);
        writer.notify(json!({"method":"fixture/session/change", "session":id, "change":SessionChange::Text {
            turn_id: "turn".into(), item_id: "item".into(),
            field: TextField::AssistantText, delta: " updated".into(),
        }})).await.unwrap();
        wait_for(&store, |state| matches!(state.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::AssistantText {text, ..} if text == "authoritative updated")).await;
        assert!(store.snapshot().error.is_none());
        assert_eq!(store.snapshot().conversations[&id].history_limit, Some(24));
    } else {
        writer
            .reply(
                &recovery,
                json!({"error":{"code":"history_unavailable","message":"history read failed"}}),
            )
            .await
            .unwrap();
        wait_for(&store, |state| state.error.is_some()).await;
        assert!(
            store
                .snapshot()
                .error
                .as_ref()
                .unwrap()
                .contains("history read failed")
        );
        assert!(!store.snapshot().subscriptions.contains_key(&id));
        assert_eq!(
            store.snapshot().conversations[&id].turns,
            conversation.turns
        );
    }
    assert_eq!(
        store.snapshot().drafts[&DraftKey::from(id)].text,
        "unsent draft"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn completed_login_selects_its_account_before_refreshing_without_client_logic() {
    for (provider, id) in [("codex", "added"), ("claude", "claude:added")] {
        for thread_id in [None, Some("draft")] {
            let model_id = if provider == "claude" {
                "sonnet"
            } else {
                "gpt"
            };
            let (store, mut reader, writer) = setup(Snapshot::default()).await;
            let polling = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
                provider: serde_json::from_value(json!(provider)).unwrap(),
                id: "login".into(),
                thread_id: thread_id.map(DraftKey::from),
            }));
            let request = read(&mut reader).await;
            assert_eq!(
                request["params"],
                json!({"provider":provider,"loginId":"login"})
            );
            writer
                .reply(
                    &request,
                    json!({"result":{"completed":true,"accountId":id}}),
                )
                .await
                .unwrap();
            polling.await.unwrap();
            let select = read(&mut reader).await;
            assert_eq!(select["method"], "host/account/select");
            assert_eq!(
                select["params"],
                json!({"provider":provider,"accountId":id})
            );
            writer
                .reply(
                    &select,
                    json!({"result":{"provider":provider,"selectedId":id}}),
                )
                .await
                .unwrap();
            for _ in 0..2 {
                let request = read(&mut reader).await;
                let result = match request["method"].as_str().unwrap() {
                    "host/account/list" => {
                        json!({"accounts":[{"provider":provider,"id":id}],"selected":{(provider):id},"error":null})
                    }
                    "host/model/list" => {
                        json!({"data":[{"id":model_id,"model":{"provider":provider,"id":model_id},"displayName":model_id,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}]})
                    }
                    method => panic!("unexpected login effect: {method}"),
                };
                writer
                    .reply(&request, json!({"result":result}))
                    .await
                    .unwrap();
            }
            wait_for(&store, |snapshot| {
                thread_id.is_none_or(|key| {
                    snapshot
                        .drafts
                        .get(&DraftKey::from(key))
                        .is_some_and(|draft| {
                            draft.model.as_ref().is_some_and(|model| {
                                model.provider
                                    == if provider == "claude" {
                                        ProviderKind::Claude
                                    } else {
                                        ProviderKind::Codex
                                    }
                                    && model.id == model_id
                            })
                        })
                }) && snapshot.account.accounts.as_ref().is_some_and(|accounts| {
                    accounts
                        .accounts
                        .iter()
                        .any(|account| account.id == id && accounts.is_selected(account))
                })
            })
            .await;
            assert!(store.snapshot().account.login.is_none());
            store.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn overloaded_catalog_prefetch_finishes_loading_and_can_be_retried() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    reader.script_composer_catalog();
    let mut requests = Vec::new();
    let mut receipts = Vec::new();
    for index in 0..32 {
        receipts.push(store.dispatch(Intent::ReadFile(op::ReadFile {
            path: format!("/pending/{index}"),
            discard_draft: false,
        })));
        // The fixture permits 16 wire requests; the other 16 occupy Store jobs
        // while waiting for a transport permit.
        if index < 16 {
            requests.push(read(&mut reader).await);
        }
    }
    assert!(
        store
            .dispatch(Intent::NewChat {
                cwd: "/project".into(),
            })
            .await
            .is_err()
    );
    wait_for(&store, |snapshot| {
        snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.loading && !catalog.errors.is_empty())
    })
    .await;
    for request in &requests {
        writer
            .reply(request, json!({"result":file(request["params"]["path"].as_str().unwrap(), "r1", "content")}))
            .await
            .unwrap();
    }
    for _ in 0..16 {
        let request = read(&mut reader).await;
        writer
            .reply(&request, json!({"result":file(request["params"]["path"].as_str().unwrap(), "r1", "content")}))
            .await
            .unwrap();
    }
    for receipt in receipts {
        receipt.await.unwrap();
    }
    let retry = store.dispatch(Intent::EditComposer {
        thread_id: store.snapshot().navigation.draft_key.clone(),
        text: "/".into(),
        cursor: 1,
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/composer/catalog");
    writer
        .reply(
            &request,
            json!({"result":{"cwd":"/project","loading":false,"candidates":[],"errors":{}}}),
        )
        .await
        .unwrap();
    retry.await.unwrap();
    wait_for(&store, |snapshot| {
        snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.loading && catalog.errors.is_empty())
    })
    .await;
    store.close().await.unwrap();
}

#[tokio::test]
async fn composer_catalog_prefetch_and_refresh_keep_candidates_available() {
    let (store, mut reader, writer) = setup(authenticated_snapshot()).await;
    reader.script_composer_catalog();
    let navigation = store.dispatch(Intent::NewChat {
        cwd: "/project".into(),
    });
    let mut prefetch = None;
    for _ in 0..2 {
        let request = read(&mut reader).await;
        match request["method"].as_str().unwrap() {
            "host/composer/catalog" => {
                assert_eq!(request["params"]["cwd"], "/project");
                prefetch = Some(request);
            }
            "host/workspace/review" => writer
                .reply(&request, json!({"result":review()}))
                .await
                .unwrap(),
            method => panic!("unexpected prefetch request: {method}"),
        }
    }
    navigation.await.unwrap();
    let key = store.snapshot().navigation.draft_key.clone();
    store
        .dispatch(Intent::EditComposer {
            thread_id: key.clone(),
            text: "/".into(),
            cursor: 1,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), reader.read_request())
            .await
            .is_err(),
        "opening the picker must share the pending prefetch"
    );
    let catalog = json!({"cwd":"/project","loading":false,"candidates":[{
        "invocation":{"provider":"codex","kind":"Skill","name":"review","path":"/project/review/SKILL.md"},
        "description":"Review changes"
    }],"errors":{}});
    writer
        .reply(&prefetch.unwrap(), json!({"result":catalog}))
        .await
        .unwrap();
    wait_for(&store, |snapshot| {
        snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|c| !c.loading)
    })
    .await;
    let suggestions = store
        .snapshot()
        .composer_suggestions("/".into(), 1)
        .unwrap();
    assert_eq!(suggestions.candidates[0].invocation.name, "review");
    assert!(suggestions.status.is_none());

    store
        .dispatch(Intent::EditComposer {
            thread_id: key.clone(),
            text: String::new(),
            cursor: 0,
        })
        .await
        .unwrap();
    let refresh = store.dispatch(Intent::EditComposer {
        thread_id: key.clone(),
        text: "/".into(),
        cursor: 1,
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/composer/catalog");
    tokio::time::timeout(Duration::from_secs(2), refresh)
        .await
        .unwrap()
        .unwrap();
    assert!(store.snapshot().composer_catalog.as_ref().unwrap().loading);
    let suggestions = store
        .snapshot()
        .composer_suggestions("/".into(), 1)
        .unwrap();
    assert_eq!(suggestions.candidates[0].invocation.name, "review");
    assert!(
        suggestions.status.is_none(),
        "cached candidates should display immediately during refresh"
    );
    store
        .dispatch(Intent::EditComposer {
            thread_id: key,
            text: "/rev".into(),
            cursor: 4,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), reader.read_request())
            .await
            .is_err()
    );
    writer
        .reply(
            &request,
            json!({"error":{"code":"unavailable","message":"offline"}}),
        )
        .await
        .unwrap();
    wait_for(&store, |snapshot| {
        snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|c| !c.loading)
    })
    .await;
    let suggestions = store
        .snapshot()
        .composer_suggestions("/rev".into(), 4)
        .unwrap();
    assert_eq!(suggestions.candidates[0].invocation.name, "review");
    assert!(
        suggestions
            .status
            .unwrap()
            .contains("候補を取得できませんでした")
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn composer_catalog_ignores_replies_from_previous_directories_and_accounts() {
    let (store, mut reader, writer) = setup(authenticated_snapshot()).await;
    reader.script_composer_catalog();
    let mut pending = Vec::new();
    for cwd in ["/first", "/second"] {
        let navigation = store.dispatch(Intent::NewChat { cwd: cwd.into() });
        for _ in 0..2 {
            let request = read(&mut reader).await;
            match request["method"].as_str().unwrap() {
                "host/composer/catalog" => {
                    assert_eq!(request["params"]["cwd"], cwd);
                    pending.push(request);
                }
                "host/workspace/review" => writer
                    .reply(&request, json!({"result":review()}))
                    .await
                    .unwrap(),
                method => panic!("unexpected navigation request: {method}"),
            }
        }
        navigation.await.unwrap();
    }
    writer
        .reply(
            &pending[0],
            json!({"result":{"cwd":"/first","loading":false,"candidates":[],"errors":{}}}),
        )
        .await
        .unwrap();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: store.snapshot().navigation.draft_key.clone(),
            text: "draft".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        store.snapshot().composer_catalog.as_ref().unwrap().cwd,
        "/second"
    );
    assert!(store.snapshot().composer_catalog.as_ref().unwrap().loading);

    let selection = store.dispatch(Intent::SelectAccount(op::SelectAccount {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "new".into(),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/account/select");
    writer
        .reply(
            &request,
            json!({"result":{"provider":"codex","selectedId":"new","persistenceError":null}}),
        )
        .await
        .unwrap();
    selection.await.unwrap();
    let mut refreshed = None;
    for _ in 0..3 {
        let request = read(&mut reader).await;
        match request["method"].as_str().unwrap() {
            "host/account/list" => writer
                .reply(&request, json!({"result":{"accounts":[],"selected":{}}}))
                .await
                .unwrap(),
            "host/model/list" => writer
                .reply(&request, json!({"result":{"data":[]}}))
                .await
                .unwrap(),
            "host/composer/catalog" => refreshed = Some(request),
            method => panic!("unexpected account refresh: {method}"),
        }
    }
    writer.reply(&pending[1], json!({"result":{"cwd":"/second","loading":false,"candidates":[{
        "invocation":{"provider":"codex","kind":"Skill","name":"old-account","path":"/old/SKILL.md"},"description":"Old"
    }],"errors":{}}})).await.unwrap();
    let catalog = json!({"cwd":"/second","loading":false,"candidates":[{
        "invocation":{"provider":"codex","kind":"Skill","name":"new-account","path":"/new/SKILL.md"},"description":"New"
    }],"errors":{}});
    writer
        .reply(&refreshed.unwrap(), json!({"result":catalog}))
        .await
        .unwrap();
    wait_for(&store, |snapshot| {
        snapshot
            .composer_catalog
            .as_ref()
            .is_some_and(|c| !c.loading)
    })
    .await;
    assert_eq!(
        store
            .snapshot()
            .composer_suggestions("/".into(), 1)
            .unwrap()
            .candidates[0]
            .invocation
            .name,
        "new-account"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn selected_invocations_reach_submission_and_return_after_failure() {
    use agent_core::composer::insert_invocation;
    use agent_protocol::composer::Invocation;
    use agent_protocol::composer::InvocationKind;
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    new_chat(&store, &mut reader, &mut writer, "/fixture").await;
    let key = store.snapshot().navigation.draft_key.clone();
    let invocation = Invocation {
        provider: agent_protocol::session::ProviderKind::Codex,
        kind: InvocationKind::Skill,
        name: "review".into(),
        path: "/fixture/review/SKILL.md".into(),
    };
    let insertion = insert_invocation(
        "/review".into(),
        7,
        invocation.kind,
        invocation.name.clone(),
    )
    .unwrap();
    store
        .dispatch(Intent::InsertInvocation {
            thread_id: key.clone(),
            text: insertion.text,
            invocation: invocation.clone(),
        })
        .await
        .unwrap();
    let sending = store.dispatch(Intent::Submit {
        thread_id: None,
        client_user_message_id: "invocation".into(),
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/create");
    assert!(store.snapshot().drafts[&key].invocations.is_empty());
    assert_eq!(
        store.snapshot().pending_submissions["invocation"]
            .draft
            .invocations,
        vec![invocation.clone()]
    );
    writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"created"},"cwd":"/fixture","status":"idle","turns":[]}}})).await.unwrap();
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/submit");
    assert_eq!(
        request["params"]["input"][1],
        json!({"skill":{"name":"review","path":"/fixture/review/SKILL.md"}})
    );
    store
        .dispatch(Intent::SetDraftText {
            thread_id: SessionRef {
                provider: ProviderKind::Codex,
                id: "created".into(),
            }
            .into(),
            text: "newer input".into(),
        })
        .await
        .unwrap();
    writer
        .reply(
            &request,
            json!({"error":{"code":"request_failed","message":"definite failure","delivery":"notSent"}}),
        )
        .await
        .unwrap();
    assert!(sending.await.is_err());
    let snapshot = store.snapshot();
    assert_eq!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "created".into()
        })]
            .invocations,
        vec![invocation]
    );
    assert!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "created".into()
        })]
            .text
            .contains("newer input")
    );
    assert!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "created".into()
        })]
            .text
            .contains("$review")
    );
    store.close().await.unwrap();
}

fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
    match item.body() {
        agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
        agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
        agent_protocol::items::ItemBody::Reasoning { content, .. } => {
            content.first().map(String::as_str)
        }
        _ => None,
    }
}

#[tokio::test]
async fn model_catalog_pages_keep_provider_identity_and_distinct_alias_entries() {
    let (store, mut reader, writer) = connected(Snapshot::default()).await;
    let mut pending = BTreeMap::new();
    for _ in 0..4 {
        let request = read(&mut reader).await;
        pending.insert(request["method"].as_str().unwrap().to_owned(), request);
    }
    writer.reply(&pending["host/taskActivity/read"], json!({"result":{"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
    writer
        .reply(
            &pending["host/session/list"],
            json!({"result":{"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}}),
        )
        .await
        .unwrap();
    writer
        .reply(
            &pending["host/account/list"],
            json!({"result":{"accounts":[],"selected":{}}}),
        )
        .await
        .unwrap();
    let model = |provider: &str, id: &str, title: &str| json!({"id":id,"model":{"provider":provider,"id":"same-native-model"},"displayName":title,"defaultReasoningEffort":"","supportedReasoningEfforts":[],"isDefault":false});
    writer.reply(&pending["host/model/list"],json!({"result":{"data":[model("codex","shared-entry","Original Codex"),model("claude","shared-entry","Claude")],"nextCursor":"next"}})).await.unwrap();
    let next = read(&mut reader).await;
    assert_eq!(next["method"], "host/model/list");
    assert_eq!(next["params"]["cursor"], "next");
    writer.reply(&next,json!({"result":{"data":[model("codex","shared-entry","Updated Codex"),model("codex","alias-entry","Codex alias")],"nextCursor":null}})).await.unwrap();
    wait_for(&store, |state| {
        state
            .models
            .iter()
            .any(|model| model.display_name == "Updated Codex")
    })
    .await;
    let state = store.snapshot();
    assert_eq!(state.models.len(), 3);
    assert_eq!(
        state
            .models
            .iter()
            .map(|model| (
                model.model.provider,
                model.id.as_str(),
                model.display_name.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (ProviderKind::Codex, "shared-entry", "Updated Codex"),
            (ProviderKind::Claude, "shared-entry", "Claude"),
            (ProviderKind::Codex, "alias-entry", "Codex alias"),
        ]
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn cursor_history_retry_preserves_the_window_and_late_pages_cannot_replace_a_refresh() {
    let id = SessionRef::new(ProviderKind::Codex, "history".into()).unwrap();
    let history = agent_core::state::operations::OperationKey::History {
        session: id.clone(),
    };
    let item = |id: &str| {
        json!({"id":id,"status":"unknown","clientInputId":null,
        "body":{"inline":{"body":{"assistantText":{"text":id,"phase":"unknown"}}}}})
    };
    let initial: Thread = serde_json::from_value(
        json!({"id":id,"historyCursor":"oldest-A","historyHasMore":true,
        "turns":[{"id":"turn","status":"completed","items":[item("tail")]}]}),
    )
    .unwrap();
    let (store, mut reader, writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([(id.clone(), Arc::new(initial))])),
        ..Default::default()
    })
    .await;
    assert!(store.snapshot().subscriptions.contains_key(&id));
    let cached = store.snapshot().conversations.clone();
    let failed = store.dispatch(Intent::ReadOlder {
        thread_id: id.clone(),
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/session/history/read");
    assert_eq!(
        request["params"],
        json!({"session":id,"cursor":"oldest-A","includeActivity":false})
    );
    assert!(store.snapshot().operation_running(history.clone()));
    writer
        .reply(
            &request,
            json!({"error":{"code":"request_failed","message":"temporary history failure"}}),
        )
        .await
        .unwrap();
    assert!(failed.await.is_err());
    assert_eq!(store.snapshot().conversations, cached);
    assert!(!store.snapshot().operation_running(history.clone()));
    assert!(
        store
            .snapshot()
            .operation_error(history.clone())
            .unwrap()
            .contains("temporary history failure")
    );

    let retry = store.dispatch(Intent::ReadOlder {
        thread_id: id.clone(),
    });
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["cursor"], "oldest-A");
    assert!(store.snapshot().operation_running(history.clone()));
    assert!(store.snapshot().operation_error(history.clone()).is_none());
    writer
        .reply(
            &request,
            json!({"result":{"nextCursor":"oldest-B",
        "turns":[{"id":"older-turn","status":"completed","items":[item("head")]}]}}),
        )
        .await
        .unwrap();
    retry.await.unwrap();
    let joined = store.snapshot();
    assert!(!joined.operation_running(history.clone()));
    let thread = &joined.conversations[&id];
    assert_eq!(thread.history_cursor.as_deref(), Some("oldest-B"));
    let turn = &thread.turns.as_ref().unwrap()[0];
    assert_eq!(
        turn.status,
        agent_protocol::execution::TurnStatus::Completed
    );
    assert_eq!(
        turn.items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["head"]
    );
    assert_eq!(
        thread.turns.as_ref().unwrap()[1].items.as_ref().unwrap()[0]
            .id
            .as_str(),
        "tail"
    );

    let pending = store.dispatch(Intent::ReadOlder {
        thread_id: id.clone(),
    });
    let old_request = read(&mut reader).await;
    assert_eq!(old_request["params"]["cursor"], "oldest-B");
    let refresh = store.dispatch(Intent::ReadThread(op::ReadThread::new(id.clone())));
    let request = read(&mut reader).await;
    writer
        .reply(
            &request,
            json!({"result":{"thread":{"id":id,"historyCursor":"fresh-C","historyHasMore":true,
        "turns":[{"id":"fresh-turn","status":"completed","items":[item("fresh-tail")]}]}}}),
        )
        .await
        .unwrap();
    refresh.await.unwrap();
    let refreshed = store.snapshot().conversations.clone();
    writer
        .reply(
            &old_request,
            json!({"result":{"nextCursor":null,
        "turns":[{"id":"stale-turn","status":"completed","items":[item("stale")]}]}}),
        )
        .await
        .unwrap();
    pending.await.unwrap();
    assert!(!store.snapshot().operation_running(history.clone()));
    assert!(store.snapshot().operation_error(history).is_none());
    assert_eq!(
        store.snapshot().conversations,
        refreshed,
        "an older-page reply belongs to its original cursor and subscription"
    );
    assert_eq!(
        store.snapshot().conversations[&id]
            .history_cursor
            .as_deref(),
        Some("fresh-C")
    );
    store.close().await.unwrap();
}
#[tokio::test]
async fn independent_view_operations_complete_without_invalidating_each_other() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let epoch = store.snapshot().epoch;
    let reading = store.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/selected/file".into(),
        discard_draft: false,
    }));
    let file_request = read(&mut reader).await;
    let reviewing = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        cwd: "/selected".into(),
    }));
    let review_request = read(&mut reader).await;
    assert_eq!(store.snapshot().epoch, epoch);
    assert!(store.snapshot().operation_running(op::OperationKey::File));
    assert!(
        store
            .snapshot()
            .operation_running(op::OperationKey::WorkspaceReview)
    );
    writer.reply(&review_request, json!({"result":{"branch":"main","additions":0,"deletions":0,"files":[],"diff":"review"}})).await.unwrap();
    reviewing.await.unwrap();
    assert!(
        !store
            .snapshot()
            .operation_running(op::OperationKey::WorkspaceReview)
    );
    assert!(store.snapshot().operation_running(op::OperationKey::File));
    writer
        .reply(
            &file_request,
            json!({"result":file("/selected/file", "r1", "selected content")}),
        )
        .await
        .unwrap();
    reading.await.unwrap();
    assert_eq!(
        store.snapshot().workspace.file.as_ref().unwrap().text,
        "selected content"
    );
    assert!(!store.snapshot().operation_running(op::OperationKey::File));
    store.close().await.unwrap();
}

#[tokio::test]
async fn terminal_queue_overload_preserves_live_terminal_and_finishes_rejected_start() {
    use agent_core::state::TerminalPhase;
    use agent_protocol::operations::TerminalSize;
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let start = |handle: &str| {
        store.dispatch(Intent::StartTerminal(op::StartTerminal {
            handle: handle.into(),
            cwd: "/fixture".into(),
            size: TerminalSize { cols: 80, rows: 24 },
        }))
    };
    let starting = start("live");
    let request = read(&mut reader).await;
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    starting.await.unwrap();
    let write = || {
        store.dispatch(Intent::WriteTerminal(op::WriteTerminal {
            handle: "live".into(),
            data: b"input".to_vec(),
        }))
    };
    drop(write());
    let _pending = read(&mut reader).await;
    for _ in 0..128 {
        drop(write());
    }
    assert!(matches!(write().await, Err(PeerError::InvalidMessage(_))));
    assert_eq!(
        store.snapshot().terminals["live"].phase,
        TerminalPhase::Running
    );
    assert!(matches!(
        start("rejected").await,
        Err(PeerError::InvalidMessage(_))
    ));
    assert!(matches!(
        store.snapshot().terminals["rejected"].phase,
        TerminalPhase::Failed(_)
    ));
    assert_eq!(
        store.snapshot().terminals["live"].phase,
        TerminalPhase::Running
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn terminal_handles_progress_independently_while_each_keeps_input_order() {
    use agent_protocol::operations::TerminalSize;
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let start = |handle: &str| {
        store.dispatch(Intent::StartTerminal(op::StartTerminal {
            handle: handle.into(),
            cwd: "/fixture".into(),
            size: TerminalSize { cols: 80, rows: 24 },
        }))
    };
    let a = start("A");
    let request_a = read(&mut reader).await;
    let writing_a = store.dispatch(Intent::WriteTerminal(op::WriteTerminal {
        handle: "A".into(),
        data: b"queued".to_vec(),
    }));
    let b = start("B");
    let request_b = tokio::time::timeout(Duration::from_secs(5), read(&mut reader))
        .await
        .unwrap();
    assert_eq!(request_b["method"], "host/terminal/start");
    writer
        .reply(&request_b, json!({"result":{}}))
        .await
        .unwrap();
    b.await.unwrap();
    let writing_b = store.dispatch(Intent::WriteTerminal(op::WriteTerminal {
        handle: "B".into(),
        data: b"independent".to_vec(),
    }));
    let input_b = read(&mut reader).await;
    assert_eq!(input_b["params"]["processHandle"], "B");
    writer.reply(&input_b, json!({"result":{}})).await.unwrap();
    writing_b.await.unwrap();
    writer
        .reply(&request_a, json!({"result":{}}))
        .await
        .unwrap();
    a.await.unwrap();
    let input_a = read(&mut reader).await;
    assert_eq!(input_a["params"]["processHandle"], "A");
    writer.reply(&input_a, json!({"result":{}})).await.unwrap();
    writing_a.await.unwrap();
    store.close().await.unwrap();
}
