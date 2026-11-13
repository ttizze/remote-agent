use agent_core::state::DraftKey;
#[path = "store/panes.rs"]
mod panes;
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
    assert_eq!(request["method"], "host/device/identify");
    writer
        .reply(
            &request,
            serde_json::json!({"result":{"storageScope":"fixture-storage","ticket":""}}),
        )
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
    assert_eq!(store.snapshot().input.drafts, before.input.drafts);
    assert_eq!(store.snapshot().connection.epoch, before.connection.epoch);
    assert!(store.snapshot().panes.primary().error.is_none());
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
    assert!(store.snapshot().panes.primary().error.is_none());
    let operation = store.dispatch(Intent::Transcribe(op::Dictate {
        draft_key: DraftKey::from("/fixture"),
        preparation: Some(preparation.id()),
        audio: vec![1, 0, 255, 127],
        send: false,
        client_user_message_id: "376d54b2-4e94-5075-8039-b1bee46d386b".into(),
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
        store.snapshot().input.drafts[&DraftKey::from("/fixture")].text,
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
        client_user_message_id: "376d54b2-4e94-5075-8039-b1bee46d386b".into(),
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
        store.snapshot().input.drafts[&DraftKey::from("/fixture")].text,
        "spoken"
    );
    assert!(store.snapshot().panes.primary().error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn opening_a_deferred_screenshot_loads_its_image_without_expanding_work() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let id = SessionRef::new(ProviderKind::Codex, "screenshots".into()).unwrap();
    let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/subscribe");
    writer.reply(&request, json!({"result":{
        "thread":{"id":id,"turns":[{"id":"turn","status":"completed","items":[{
            "id":"screenshot","status":"completed","body":{"deferred":{"summary":{"imageView":{"path":"file:///tmp/blog.png"}}}}
        }]}]}
    }})).await.unwrap();
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/hydrate");
    assert_eq!(request["params"]["item"], "screenshot");
    writer.reply(&request, json!({"result":{"item":{
        "id":"screenshot","status":"completed","body":{"inline":{"body":{"imageView":{"path":"file:///tmp/blog.png"}}}}
    }}})).await.unwrap();
    opening.await.unwrap();
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let loaded = {
                let snapshot = updates.borrow_and_update();
                !snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0]
                    .items
                    .as_ref()
                    .unwrap()[0]
                    .is_deferred()
            };
            if loaded {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let snapshot = store.snapshot();
    let projected = agent_core::presentation::conversation::project_conversation(
        agent_core::localization::UiLanguage::Japanese,
        &snapshot.panes.primary().navigation.draft_key,
        &snapshot.sessions.pending_submissions,
        snapshot.sessions.conversations[&id].clone(),
        &None,
    );
    assert!(projected.turns[0].rows.iter().any(|row| matches!(&row.content,
        agent_core::presentation::conversation::ConversationRowContent::Response { item, .. }
            if item.data.image_sources == ["file:///tmp/blog.png"]
    )));
    store.close().await.unwrap();
}

#[tokio::test]
async fn list_refresh_bursts_keep_only_the_latest_refresh_without_blocking_navigation() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let first = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        Default::default(),
    )));
    let first_request = read(&mut reader).await;
    assert_eq!(first_request["method"], "host/list/subscribe");
    let navigation_epoch = store.snapshot().connection.epoch;
    let mut receipts = Vec::new();
    for _ in 0..40 {
        receipts.push(store.dispatch(Intent::ListSessions(op::ListSessions::new(
            (*store.snapshot().lists.list_query).clone(),
        ))));
    }
    assert_eq!(
        store.snapshot().connection.epoch,
        navigation_epoch,
        "list queries must not invalidate conversation reads"
    );
    let id = SessionRef::new(ProviderKind::Codex, "selected".to_owned()).unwrap();
    let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())));
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .as_ref(),
        Some(&id)
    );
    let open_request = read(&mut reader).await;
    assert_eq!(
        open_request["method"], "host/conversation/subscribe",
        "refreshes must not fan out while the first list is pending"
    );
    receipts.push(store.dispatch(Intent::ListSessions(op::ListSessions::new(
        (*store.snapshot().lists.list_query).clone(),
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
        store.snapshot().sessions.subscriptions.contains_key(&id),
        "expanding projects must retain the in-flight conversation subscription"
    );
    writer.reply(&first_request, json!({"result":{"data":[],"projects":[{"id":"obsolete","name":"Old project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"limit":5}})).await.unwrap();
    first.await.unwrap();
    assert!(
        store
            .snapshot()
            .lists
            .threads
            .as_ref()
            .unwrap()
            .projects
            .is_empty(),
        "an older query must not replace the expanded list"
    );
    let latest = read(&mut reader).await;
    assert_eq!(latest["method"], "host/list/subscribe");
    assert!(latest["params"].get("limit").is_none());
    assert_eq!(store.snapshot().lists.list_query.limit, 5);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), read(&mut reader))
            .await
            .is_err()
    );
    writer.reply(&latest, json!({"result":{"data":[],"projects":[{"id":"latest","name":"Expanded project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"limit":5}})).await.unwrap();
    for receipt in receipts {
        receipt.await.unwrap();
    }
    assert_eq!(
        store.snapshot().lists.threads.as_ref().unwrap().projects[0].id,
        "latest"
    );
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .as_ref(),
        Some(&id)
    );

    // A list result remains useful after a navigation-only epoch change.
    let refresh = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        (*store.snapshot().lists.list_query).clone(),
    )));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/list/subscribe");
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    writer.reply(&request, json!({"result":{"data":[],"projects":[{"id":"after-navigation","name":"Current project","roots":[]}],"hasMore":false,"hasMoreProjects":false,"limit":5}})).await.unwrap();
    refresh.await.unwrap();
    assert_eq!(
        store.snapshot().lists.threads.as_ref().unwrap().projects[0].id,
        "after-navigation"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn workspace_refresh_bursts_keep_the_latest_directory_and_leave_lists_available() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let first = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        base_ref: None,
        cwd: "/old".into(),
    }));
    let first_request = read(&mut reader).await;
    assert_eq!(first_request["method"], "host/workspace/review");
    let mut receipts = Vec::new();
    for index in 0..40 {
        receipts.push(store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
            base_ref: None,
            cwd: format!("/latest/{index}"),
        })));
    }
    let listing = store.dispatch(Intent::ListSessions(op::ListSessions::new(
        Default::default(),
    )));
    let list_request = read(&mut reader).await;
    assert_eq!(
        list_request["method"], "host/list/subscribe",
        "workspace refreshes must not fan out or block independent reads"
    );
    writer
        .reply(
            &list_request,
            json!({"result":{"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5}}),
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
        store.snapshot().panes.primary().workspace.review.is_none(),
        "an obsolete directory must not publish its diff"
    );
    let latest = read(&mut reader).await;
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
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .review_cwd
            .as_deref(),
        Some("/latest/39")
    );
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .review
            .as_ref()
            .unwrap()
            .branch,
        "latest"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn comparison_changes_discard_in_flight_reviews_and_refresh_the_selected_base() {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
        "/project".into();
    let (store, mut reader, writer) = setup(snapshot).await;
    let first = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        cwd: "/project".into(),
        base_ref: None,
    }));
    let old_request = read(&mut reader).await;
    assert_eq!(
        old_request["params"],
        json!({"cwd":"/project","includeOptions":true})
    );
    let selecting = store.dispatch(Intent::SetWorkspaceComparison {
        cwd: "/project".into(),
        base_ref: Some("HEAD".into()),
    });
    let mut obsolete = review();
    obsolete["additions"] = 999.into();
    writer
        .reply(&old_request, json!({"result":obsolete}))
        .await
        .unwrap();
    first.await.unwrap();
    assert_ne!(
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .review
            .as_ref()
            .unwrap()
            .additions,
        999
    );
    let selected_request = read(&mut reader).await;
    assert_eq!(
        selected_request["params"],
        json!({"cwd":"/project", "baseRef":"HEAD","includeOptions":true})
    );
    let mut selected = review();
    selected["base_ref"] = "HEAD".into();
    selected["additions"] = 7.into();
    selected["deletions"] = 2.into();
    selected["diff"] = "selected diff".into();
    writer
        .reply(&selected_request, json!({"result":selected}))
        .await
        .unwrap();
    selecting.await.unwrap();
    let snapshot = store.snapshot();
    let workspace = &snapshot.panes.primary().workspace;
    assert_eq!(workspace.comparison.as_deref(), Some("HEAD"));
    let review = workspace.review.as_ref().unwrap();
    assert_eq!(review.base_ref.as_deref(), Some("HEAD"));
    assert_eq!((review.additions, review.deletions), (7, 2));
    assert_eq!(review.diff, "selected diff");
    let refresh = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        cwd: "/project".into(),
        base_ref: None,
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["baseRef"], "HEAD");
    writer
        .reply(&request, json!({"result":selected}))
        .await
        .unwrap();
    refresh.await.unwrap();
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
    let models = store.dispatch(Intent::LoadModels(op::LoadModels {
        provider: ProviderKind::Codex,
    }));
    let model_request = read(&mut reader).await;
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
    let second_request = read(&mut reader).await;
    assert_eq!(second_request["method"], "host/list/subscribe");
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
            provider: Some(ProviderKind::Codex),
            id: "browser-thread".into(),
        },
        tab_id: String::new(),
        image_id: String::new(),
        renderer_id: String::new(),
        generation: 0,
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

#[tokio::test]
async fn a_browser_session_orders_actions_reads_while_shown_and_slows_after_failures() {
    use agent_core::store::{BrowserSession, BrowserUpdate};
    use agent_protocol::browser::{BrowserAction, BrowserDialog, BrowserFrame};
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let thread = SessionRef {
        provider: Some(ProviderKind::Codex),
        id: "browser-thread".into(),
    };
    let session = BrowserSession::new(store.clone(), thread.clone());
    let frame = |generation: u64, dialog: Option<BrowserDialog>| BrowserFrame {
        tab_id: "tab".into(),
        image_id: format!("image-{generation}"),
        generation,
        dialog,
        ..Default::default()
    };
    // A hidden view has nothing to send until the user acts.
    assert!(session.next().await.is_none());
    session.send(BrowserAction::Navigate {
        url: "https://example.com/".into(),
    });
    let next = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let navigate = read(&mut reader).await;
    assert_eq!(navigate["method"], "host/browser");
    assert!(navigate["params"]["action"].get("Navigate").is_some());
    assert_eq!(navigate["params"]["generation"], 0);
    writer
        .reply(&navigate, json!({"result":frame(1, None)}))
        .await
        .unwrap();
    assert!(matches!(
        next.await.unwrap(),
        Some(BrowserUpdate::Frame { frame, action: BrowserAction::Navigate { .. } }) if frame.generation == 1
    ));

    // A shown view reads the page it last saw; actions keep the page the user acted on.
    session.set_active(true);
    let next = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let reading = read(&mut reader).await;
    assert_eq!(reading["params"]["action"], "Read");
    assert_eq!(reading["params"]["image_id"], "image-1");
    session.send(BrowserAction::Dialog {
        accept: true,
        text: String::new(),
    });
    writer
        .reply(
            &reading,
            json!({"result":frame(2, Some(BrowserDialog { message: "Leave?".into(), prompt: false }))}),
        )
        .await
        .unwrap();
    let Some(BrowserUpdate::Frame {
        frame: read_frame, ..
    }) = next.await.unwrap()
    else {
        panic!("the page was read")
    };
    assert!(
        read_frame.dialog.is_none(),
        "a queued answer keeps an earlier read from reopening the dialog"
    );
    let next = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let answer = read(&mut reader).await;
    assert!(answer["params"]["action"].get("Dialog").is_some());
    assert_eq!(answer["params"]["generation"], 1);
    writer
        .reply(&answer, json!({"result":frame(3, None)}))
        .await
        .unwrap();
    next.await.unwrap();

    // A failed read is reported and retried after the slower interval.
    let next = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let reading = read(&mut reader).await;
    writer
        .reply(&reading, json!({"error":{"code":"request_failed","message":"browser unavailable","delivery":"notSent"}}))
        .await
        .unwrap();
    assert!(matches!(
        next.await.unwrap(),
        Some(BrowserUpdate::ReadFailed { .. })
    ));
    let started = std::time::Instant::now();
    let retry = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let reading = read(&mut reader).await;
    assert!(started.elapsed() >= Duration::from_millis(1400));
    writer
        .reply(&reading, json!({"result":frame(4, None)}))
        .await
        .unwrap();
    assert!(matches!(
        retry.await.unwrap(),
        Some(BrowserUpdate::Frame { .. })
    ));

    // One reader at a time: a reader the view abandoned finishes before the next starts.
    let first = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let second = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    let reading = read(&mut reader).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(400), reader.read_request())
            .await
            .is_err(),
        "the second reader waits for the first"
    );
    writer
        .reply(&reading, json!({"result":frame(5, None)}))
        .await
        .unwrap();
    let reading = read(&mut reader).await;
    writer
        .reply(&reading, json!({"result":frame(6, None)}))
        .await
        .unwrap();
    for reader in [first, second] {
        assert!(matches!(
            reader.await.unwrap(),
            Some(BrowserUpdate::Frame { .. })
        ));
    }

    // Hiding the view ends a waiting reader before it takes a queued action.
    session.send(BrowserAction::Reload);
    let waiting = tokio::spawn({
        let session = session.clone();
        async move { session.next().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    session.set_active(false);
    assert!(waiting.await.unwrap().is_none());
    assert_eq!(session.pending_actions(), 1);
    session.clear();
    assert!(
        session.next().await.is_none(),
        "a hidden view stops reading"
    );

    // The backlog is bounded and can be dropped.
    for _ in 0..70 {
        session.send(BrowserAction::Reload);
    }
    assert_eq!(session.pending_actions(), 64);
    session.clear();
    assert_eq!(session.pending_actions(), 0);
    store.close().await.unwrap();
}

fn authenticated_snapshot() -> Snapshot {
    Snapshot {
        catalog: agent_core::state::Catalog {
            models: Arc::new(
                serde_json::from_value(json!([{
                    "id":"fixture","model":{"provider":"codex","id":"fixture"},"displayName":"Fixture",
                    "isDefault":true, "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]
                }}]))
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
        },
        ..Default::default()
    }
}

async fn connected(snapshot: Snapshot) -> (Arc<Store>, host_fixture::Reader, host_fixture::Writer) {
    let (peer, reader, writer) = host_fixture::connect(&snapshot).await;
    (Arc::new(Store::new(peer, snapshot, None)), reader, writer)
}
async fn setup(snapshot: Snapshot) -> (Arc<Store>, host_fixture::Reader, host_fixture::Writer) {
    let (store, mut reader, writer) = connected(snapshot.clone()).await;
    let selected = snapshot
        .panes
        .primary()
        .navigation
        .thread_id
        .as_ref()
        .map(|id| &snapshot.sessions.conversations[id]);
    let cwd = selected.map_or(snapshot.panes.primary().navigation.cwd.as_str(), |thread| {
        thread.cwd.as_deref().unwrap_or_default()
    });
    let mut updates = store.subscribe();
    loop {
        // Finish connection reads and their background decoration requests
        // before handing the scripted Host to the individual test.
        let ready = {
            let current = updates.borrow_and_update();
            current.lists.threads.is_some()
                && current.lists.task_activity.is_some()
                && current.catalog.account.accounts.is_some()
                && current.operations.is_empty()
                && (cwd.is_empty() || current.panes.primary().workspace.review.is_some())
        };
        if ready {
            break;
        }
        let request = tokio::select! {
            request = read(&mut reader) => request,
            changed = updates.changed() => { changed.unwrap(); continue; }
        };
        let result = match request["method"].as_str().unwrap() {
            "host/list/subscribe" => snapshot
                .lists.threads
                .as_ref()
                .map(|threads| json!(threads))
                .unwrap_or_else(
                    || json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}),
                ),
            "host/list/search" => json!({"data":[],"projects":[],"limit":10000,"hasMore":false,"hasMoreProjects":false}),
            "host/account/list" => snapshot
                .catalog.account
                .accounts
                .as_ref()
                .map(|accounts| json!(accounts))
                .unwrap_or_else(|| json!({"accounts":[],"selected":{}})),
            "host/account/usage" => json!({"windows":[],"fetchedAt":1,"error":null}),
            "host/model/list" => json!({"data":snapshot.catalog.models,"nextCursor":null}),
            "host/conversation/subscribe" => {
                let id: SessionRef = serde_json::from_value(request["params"]["session"].clone()).unwrap();
                json!({"thread": snapshot.sessions.conversations[&id]})
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
    for (id, thread) in snapshot.sessions.conversations.iter() {
        if snapshot.panes.primary().navigation.thread_id.as_ref() == Some(id) {
            continue;
        }
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::new(id.clone())));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/subscribe");
        writer
            .reply(&request, json!({"result":{"thread":thread}}))
            .await
            .unwrap();
        opening.await.unwrap();
    }
    (store, reader, writer)
}
async fn read(reader: &mut host_fixture::Reader) -> host_fixture::Request {
    tokio::time::timeout(Duration::from_secs(2), reader.read_request())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
/// The Store waits its poll interval before confirming a pending login.
async fn read_login_poll(reader: &mut host_fixture::Reader) -> host_fixture::Request {
    tokio::time::timeout(Duration::from_secs(5), reader.read_request())
        .await
        .expect("the Store polls a pending login")
        .unwrap()
        .unwrap()
}
fn review() -> Value {
    json!({"controls":{"branches":[],"references":[],"remotes":[],"can_push":false,"scripts":[]},"branch":"main","base_ref":"origin/main","additions":0,"deletions":0,"files":[],"diff":""})
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
    assert_eq!(request["params"], json!({"cwd":cwd,"includeOptions":true}));
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
        if request["method"] == "host/conversation/subscribe" {
            let id = serde_json::from_value(request["params"]["session"].clone()).unwrap();
            let response = writer.current(&id);
            writer
                .reply(&request, json!({ "result":response}))
                .await
                .unwrap();
            continue;
        }
        if request["method"] == "host/list/subscribe" {
            writer
                .reply(
                    &request,
                    json!({"result":{"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}}),
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
        sessions: agent_core::state::Sessions {
            conversations: Arc::new(BTreeMap::from([(
                SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                },
                Arc::new(thread("old")),
            )])),
            ..Default::default()
        },
        ..Default::default()
    }
}
fn loaded_text(snapshot: &Snapshot) -> Option<&str> {
    item_text(
        snapshot
            .sessions
            .conversations
            .get(&SessionRef {
                provider: Some(ProviderKind::Codex),
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
            catalog: agent_core::state::Catalog {
                models: Arc::new(serde_json::from_value(json!([{
                    "id":"model", "model":{"provider": "codex", "id": "model"}, "displayName":"Model", "settings": {
                    "defaultReasoningEffort":"medium", "defaultServiceTier":"priority",
                    "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"max"}],
                    "serviceTiers":[{"id":"priority","fast":true}]
                }}])).unwrap()),
                ..Default::default()
            },
            input: agent_core::state::Input {
                drafts: Arc::new(BTreeMap::from([(SessionRef {provider: Some(ProviderKind::Codex), id: "thread".into()}.into(), Arc::new(Draft {
                    text: "old".into(), model: Some(agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Codex, id: "model".into() }), effort: Some("medium".into()),
                    attachments: ["removed.png", "image.png"].map(|name| Attachment {
                        path: format!("/fixture/{name}"), name: name.into(), is_image: true,
                    }).into(),
                    service_tier: Some("priority".into()), ..Default::default()
                }))])),
                ..Default::default()
            },
            ..Default::default()
        };
        let (store, _reader, _writer) = setup(previous.clone()).await;
        let attachment = Intent::RemoveAttachment {
            draft_key: SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into(),
            index: 0,
        };
        let text = Intent::SetDraftText {
            thread_id: SessionRef {
                provider: Some(ProviderKind::Codex),
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
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                effort: "max".into(),
            }),
        );
        store
            .dispatch(Intent::SelectServiceTier {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                service_tier: "default".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        let draft = &current.input.drafts[&DraftKey::from(SessionRef {
            provider: Some(ProviderKind::Codex),
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
            previous.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text,
            "old"
        );
        assert_eq!(
            previous.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .attachments
                .len(),
            2
        );
        store
            .dispatch(Intent::SelectEffort {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
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
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                service_tier: "invalid".into(),
            })
            .await
            .unwrap();
        let current = store.snapshot();
        assert_eq!(
            current.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .effort
                .as_deref(),
            Some("medium")
        );
        assert_eq!(
            current.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .service_tier
                .as_deref(),
            Some("priority")
        );
        assert_eq!(
            current.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text,
            "new"
        );
        assert_eq!(
            current.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
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
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"type":"text","turnId":"turn","itemId":"item","delta":" obsolete","field":"assistantText"}})).await.unwrap();
        writer
            .reply(&request, json!({"result":{"thread":thread("base")}}))
            .await
            .unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"type":"text","turnId":"turn","itemId":"item","delta":" tail","field":"assistantText"}})).await.unwrap();
        writer.finish_updates().await;
        (writer, reader)
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        store.dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {
            provider: Some(ProviderKind::Codex),
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
        writer.notify(json!({"method": "fixture/session/request", "session": {"provider": "codex", "id": "thread"}, "request": {"id": "approval", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}}})).await.unwrap();
        let answer = read(&mut reader).await;
        assert_eq!(answer["method"], "host/conversation/change");
        assert_eq!(
            answer["params"]["command"],
            json!({"Answer":{"id":"approval","answer":{"approval":{"choiceId":"choice-2"}}}})
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
                    provider: Some(ProviderKind::Codex),
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
                        "choices": [{"id": "deny", "label": "拒否", "description": "", "effect": "deny"}]
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
            "host/conversation/change" => {
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
            provider: Some(ProviderKind::Codex),
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
            provider: Some(ProviderKind::Codex),
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
                .send(self.store.snapshot().panes.primary().navigation.cwd.clone())
                .unwrap();
        }
    }
    let store = Arc::new(Store::offline(Snapshot::default(), None));
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
    let key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: "first message".into(),
        })
        .await
        .unwrap();
    let sending = store.dispatch(Intent::Submit {
        thread_id: None,
        client_user_message_id: "5b1986e4-742d-5d96-81d1-93be060b53a6".into(),
        alternate: false,
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    let created: SessionRef = serde_json::from_value(request["params"]["session"].clone()).unwrap();
    let pending = store.snapshot();
    assert!(pending.input.drafts[&key].text.is_empty());
    let source = pending
        .conversation_thread(pending.panes.primary())
        .unwrap();
    let rendered = agent_core::presentation::conversation::project_conversation(
        agent_core::localization::UiLanguage::Japanese,
        &pending.panes.primary().navigation.draft_key,
        &pending.sessions.pending_submissions,
        source,
        &None,
    );
    assert_eq!(rendered.queued.len(), 1);
    assert_eq!(
        rendered.queued[0].data.body.as_deref(),
        Some("first message")
    );
    assert!(rendered.queued[0].data.title.is_none());
    let submitted_at = rendered.queued[0].data.timestamp_ms.unwrap();
    assert!(submitted_at > 0);
    let saved =
        agent_core::persistence::decode(&agent_core::persistence::encode(&pending).unwrap())
            .unwrap();
    assert_eq!(
        saved.sessions.pending_submissions["5b1986e4-742d-5d96-81d1-93be060b53a6"].submitted_at_ms,
        Some(submitted_at)
    );
    assert_eq!(
        request["params"]["initial"][1]["parts"][0]["text"]["text"],
        "first message"
    );
    writer.reply(&request, json!({ "result": {"thread": {"activeProvider":"codex", "cwd":"/fixture", "status":"preparing", "turns":[]}}})).await.unwrap();
    assert!(matches!(
        sending.await.unwrap(),
        Outcome::StartedThread { .. }
    ));
    let current = store.snapshot();
    assert_eq!(
        current.sessions.pending_submissions["5b1986e4-742d-5d96-81d1-93be060b53a6"]
            .submitted_at_ms,
        Some(submitted_at)
    );
    assert_eq!(
        current.panes.primary().navigation.draft_key,
        DraftKey::from(created.clone())
    );
    assert!(
        current.input.drafts[&DraftKey::from(created.clone())]
            .text
            .is_empty()
    );
    assert_eq!(
        DraftKey::from(created),
        key,
        "the draft address stays fixed through admission"
    );
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
            sessions: agent_core::state::Sessions {
                conversations: Arc::new(BTreeMap::from([(
                    SessionRef { provider: Some(ProviderKind::Codex), id: "thread".into() },
                    Arc::new(
                        serde_json::from_value(
                            json!({"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle","turns":[]}),
                        )
                        .unwrap(),
                    ),
                )])),
                ..Default::default()
            },
            ..Default::default()
        };
        let (store, mut reader, mut writer) = setup(initial).await;
        store
            .dispatch(Intent::SetDraft {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                draft: sent,
            })
            .await
            .unwrap();
        let sending = store.dispatch(Intent::Submit {
            thread_id: Some(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }),
            client_user_message_id: "c0587726-bfb5-53d5-8e91-ed64c47aa4ee".into(),
            alternate: false,
        });
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(request["method"], "host/conversation/change");
        assert_eq!(
            request["params"]["command"]["Submit"]["content"]["parts"][0]["text"]["text"],
            "sent"
        );
        assert_eq!(
            request["params"]["command"]["Submit"]["content"]["service_tier"],
            "priority"
        );
        let submitted_at =
            store.snapshot().sessions.pending_submissions["c0587726-bfb5-53d5-8e91-ed64c47aa4ee"]
                .submitted_at_ms
                .unwrap();
        assert!(submitted_at > 0);
        assert!(
            store.snapshot().input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text
                .is_empty()
        );
        assert!(
            store.snapshot().input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .attachments
                .is_empty()
        );
        store
            .dispatch(Intent::SetDraft {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                draft: serde_json::from_value(case["current"].clone()).unwrap(),
            })
            .await
            .unwrap();
        writer.reply(&request, json!({"result":{}})).await.unwrap();
        assert_eq!(sending.await.unwrap(), Outcome::Submitted);
        assert_eq!(
            store.snapshot().sessions.pending_submissions["c0587726-bfb5-53d5-8e91-ed64c47aa4ee"]
                .submitted_at_ms,
            Some(submitted_at)
        );
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(
            *store.snapshot().input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })],
            expected,
            "{}",
            case["name"]
        );
        assert!(store.snapshot().panes.primary().error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn new_submission_keeps_edits_and_navigation_while_creation_is_pending() {
    for navigate_away in [false, true] {
        let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
        new_chat(&store, &mut reader, &mut writer, "/fixture").await;
        let key = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone();
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
                        client_user_message_id: "5b1986e4-742d-5d96-81d1-93be060b53a6".into(),
                        alternate: false,
                    })
                    .await
            }
        });
        let create = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(create["method"], "host/conversation/create");
        let created: SessionRef =
            serde_json::from_value(create["params"]["session"].clone()).unwrap();
        assert_eq!(create["params"]["cwd"], "/fixture");
        assert_eq!(
            create["params"]["initial"][1]["parts"][0]["text"]["text"],
            "sent"
        );
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
        writer.reply(&create, json!({"result":{"thread":{"activeProvider":"codex","cwd":"/fixture","turns":[],"status":"idle"}}})).await.unwrap();
        assert_eq!(
            sending.await.unwrap().unwrap(),
            Outcome::StartedThread {
                id: created.clone()
            }
        );
        let state = store.snapshot();
        if navigate_away {
            assert_eq!(state.panes.primary().navigation.cwd, "/other");
            assert_eq!(state.panes.primary().navigation.thread_id, None);
            assert_eq!(state.input.drafts[&key].text, "newer");
            assert_eq!(DraftKey::from(&created), key);
            assert_eq!(state.input.drafts[&DraftKey::from(&created)].text, "newer");
        } else {
            assert_eq!(
                state
                    .panes
                    .primary()
                    .navigation
                    .thread_id
                    .as_ref()
                    .map(|session| session.id.as_str()),
                Some(created.id.as_str())
            );
            assert_eq!(
                state.input.drafts[&DraftKey::from(created.clone())].text,
                "newer"
            );
            assert_eq!(DraftKey::from(&created), key);
        }
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_new_task_admission_keeps_retryable_input() {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    new_chat(&store, &mut reader, &mut writer, "/fixture").await;
    store
        .dispatch(Intent::SetDraft {
            thread_id: store
                .snapshot()
                .panes
                .primary()
                .navigation
                .draft_key
                .clone(),
            draft: Draft {
                text: "retry me".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let sending = store.dispatch(Intent::Submit {
        thread_id: None,
        client_user_message_id: "5b1986e4-742d-5d96-81d1-93be060b53a6".into(),
        alternate: false,
    });
    let create = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(create["method"], "host/conversation/create");
    writer.reply(&create, json!({"error":{"code":"request_failed","message":"fixture failure","delivery":"notSent"}})).await.unwrap();
    assert!(matches!(sending.await, Err(PeerError::Remote { .. })));
    let state = store.snapshot();
    assert!(state.sessions.pending_submissions.is_empty());
    assert_eq!(
        state.input.drafts[&state.panes.primary().navigation.draft_key].text,
        "retry me"
    );
    let retry = store.dispatch(Intent::Submit {
        thread_id: None,
        client_user_message_id: "5cda0dfe-a323-51e0-b776-3e1c6109e468".into(),
        alternate: false,
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert!(
        store.snapshot().panes.primary().error.is_none(),
        "retry must clear the previous submission error"
    );
    assert_eq!(request["method"], "host/conversation/create");
    assert_eq!(
        request["params"]["initial"][1]["parts"][0]["text"]["text"],
        "retry me"
    );
    writer
        .reply(
            &request,
            json!({"error":{"code":"request_failed","message":"end fixture"}}),
        )
        .await
        .unwrap();
    assert!(retry.await.is_err());
    store.close().await.unwrap();
}

fn file(path: &str, revision: &str, text: &str) -> Value {
    json!({"path":path,"revision":revision,"text":text,"size":text.len(),"truncated":false})
}

#[tokio::test]
async fn transcription_preserves_newer_input_and_restores_audio_text_on_send_failure() {
    for fail_send in [false, true] {
        let mut initial = snapshot();
        Arc::make_mut(
            &mut Arc::make_mut(
                Arc::make_mut(&mut initial.sessions.conversations)
                    .get_mut(&SessionRef {
                        provider: Some(ProviderKind::Codex),
                        id: "thread".into(),
                    })
                    .unwrap(),
            )
            .turns
            .as_mut()
            .unwrap()[0],
        )
        .status = agent_protocol::execution::TurnStatus::Completed;
        Arc::make_mut(&mut Arc::make_mut(initial.panes.primary_mut()).navigation).thread_id =
            Some(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            });
        Arc::make_mut(&mut Arc::make_mut(initial.panes.primary_mut()).navigation).draft_key =
            SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into();
        Arc::make_mut(&mut initial.lists.activity).active.insert(
            SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            },
            false,
        );
        Arc::make_mut(&mut initial.input.drafts).insert(
            SessionRef {
                provider: Some(ProviderKind::Codex),
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
                            provider: Some(ProviderKind::Codex),
                            id: "thread".into(),
                        }
                        .into(),
                        preparation: None,
                        audio: vec![0, 0],
                        send: true,
                        client_user_message_id: "3d1919da-a635-591b-89e3-32f49d9197fb".into(),
                    }))
                    .await
            }
        });
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/dictation/transcribe");
        store
            .dispatch(Intent::SetDraft {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
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
        assert_eq!(submit["method"], "host/conversation/change");
        assert_eq!(
            submit["params"]["command"]["Submit"]["content"]["parts"][0]["text"]["text"],
            "original\nspoken"
        );
        assert!(!transcribing.is_finished());
        let reply = if fail_send {
            json!({"error":{"code":"request_failed","message":"send failed","delivery":"notSent"}})
        } else {
            json!({"result":{}})
        };
        writer.reply(&submit, reply).await.unwrap();
        assert_eq!(transcribing.await.unwrap().is_err(), fail_send);
        assert_eq!(
            store.snapshot().input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
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
        let (store, mut reader, writer) = setup(authenticated_snapshot()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone();
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
            client_user_message_id: "3d1919da-a635-591b-89e3-32f49d9197fb".into(),
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
            let draft = store.snapshot().input.drafts[&key].clone();
            assert_eq!(draft.text, "typed\nspoken");
            assert_eq!(draft.attachments, vec![attachment]);
            assert!(
                store
                    .snapshot()
                    .panes
                    .primary()
                    .navigation
                    .thread_id
                    .is_none()
            );
            store.dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "3d1919da-a635-591b-89e3-32f49d9197fb".into(),
                alternate: false,
            })
        };
        let start = read(&mut reader).await;
        assert_eq!(start["method"], "host/conversation/create");
        let created: SessionRef =
            serde_json::from_value(start["params"]["session"].clone()).unwrap();
        assert!(start["params"]["cwd"].is_null());
        let input =
            json!([{"text":{"text":"typed\nspoken"}},{"localImage":{"path":"/fixture/photo.png"}}]);
        assert_eq!(start["params"]["initial"][1]["parts"], input);
        writer.reply(&start, json!({"result":{"thread":{"activeProvider":"codex","cwd":"/fixture","projectId":null,"status":"idle","turns":[]}}})).await.unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":created,"change":{"type":"turn","turn":{"id":"turn","status":"completed","items":[{"id":"native","status":"unknown","clientInputId":"3d1919da-a635-591b-89e3-32f49d9197fb","body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"done","phase":"final"}}}}}]},"completed":true}})).await.unwrap();
        assert!(matches!(
            sending.await.unwrap(),
            Outcome::StartedThread { .. }
        ));
        wait_for(&store, |snapshot| {
            snapshot.sessions.conversations[&created.clone()]
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
                .panes
                .primary()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some(created.id.as_str())
        );
        assert!(
            current
                .selected_directory(current.panes.primary())
                .is_empty()
        );
        assert_eq!(current.panes.primary().navigation.cwd, "/fixture");
        assert!(
            current.input.drafts[&DraftKey::from(created.clone())]
                .text
                .is_empty()
        );
        assert!(
            current.input.drafts[&DraftKey::from(created.clone())]
                .attachments
                .is_empty()
        );
        assert!(current.sessions.pending_submissions.is_empty());
        assert!(current.panes.primary().error.is_none());
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
                    client_user_message_id: "3d1919da-a635-591b-89e3-32f49d9197fb".into(),
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
        store.snapshot().input.drafts[&DraftKey::from("/fixture")].text,
        "spoken"
    );
    assert!(store.snapshot().sessions.pending_submissions.is_empty());
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
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into()
        } else {
            "/fixture".into()
        };
        let navigation = Arc::make_mut(&mut Arc::make_mut(initial.panes.primary_mut()).navigation);
        navigation.thread_id = existing.then(|| SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        });
        navigation.cwd = "/fixture".into();
        navigation.draft_key = key.clone();
        Arc::make_mut(&mut initial.input.drafts).insert(
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
            client_user_message_id: "5a74b0f6-928d-5e70-a598-f02bf99d411c".into(),
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
        assert_eq!(after.input.drafts, before.input.drafts);
        assert_eq!(after.sessions.conversations, before.sessions.conversations);
        assert_eq!(
            after.sessions.pending_submissions,
            before.sessions.pending_submissions
        );
        assert_eq!(
            after.panes.primary().navigation,
            before.panes.primary().navigation
        );
        assert!(after.panes.primary().error.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), reader.read_request())
                .await
                .is_err(),
            "silent dictation must not submit or create a conversation"
        );
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&after).unwrap()).unwrap();
        assert_eq!(restored.input.drafts, after.input.drafts);
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
    let Outcome::FileRead { file: second } = second.await.unwrap().unwrap() else {
        panic!("file read receipt")
    };
    assert_eq!(second.path, "/second");
    assert_eq!(second.text, "second");
    writer
        .reply(
            &first_request,
            json!({"result":file("/first", "1", "first")}),
        )
        .await
        .unwrap();
    let Outcome::FileRead { file: first } = first.await.unwrap().unwrap() else {
        panic!("file read receipt")
    };
    assert_eq!(first.path, "/first");
    assert_eq!(first.text, "first");
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .file
            .as_ref()
            .unwrap()
            .path,
        "/second"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn saving_keeps_newer_edits_and_advances_their_revision_for_the_next_save() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut Arc::make_mut(initial.panes.primary_mut()).workspace).file = Some(Arc::new(
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
    assert_eq!(store.snapshot().files.file_drafts["/file"].text, "newer");
    assert_eq!(
        store.snapshot().files.file_drafts["/file"].revision,
        "saved"
    );
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
    assert!(store.snapshot().files.file_drafts.is_empty());
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .file
            .as_ref()
            .unwrap()
            .text,
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
        initial.lists.threads = Some(Arc::new(
            serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"thread"},"cwd":"/listed","name":"Selected task"}],
                "projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}))
            .unwrap(),
        ));
        Arc::make_mut(&mut initial.input.drafts).insert(
            SessionRef {
                provider: Some(ProviderKind::Codex),
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
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        })));
        let selected = store.snapshot();
        assert_eq!(
            selected
                .panes
                .primary()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("thread"),
            "selection must not wait for either RPC (cached={cached}, restored={restored})"
        );
        assert_eq!(
            selected.panes.primary().navigation.draft_key,
            DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })
        );
        assert_eq!(
            selected.panes.primary().navigation.cwd,
            if cached { "/fixture" } else { "/listed" }
        );
        assert_eq!(
            selected.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text,
            "Keep this draft"
        );
        assert_eq!(loaded_text(&selected), cached.then_some("old"));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/subscribe");
        writer
            .reply(&request, json!({"result":{"thread":thread("latest")}}))
            .await
            .unwrap();
        opening.await.unwrap();
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(store.snapshot().panes.primary().navigation.cwd, "/fixture");
        writer
            .reply(
                &list_request,
                json!({"result":{
                "data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}}),
            )
            .await
            .unwrap();
        refresh.await.unwrap();
        assert_eq!(
            store
                .snapshot()
                .panes
                .primary()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("thread")
        );
        assert_eq!(loaded_text(&store.snapshot()), Some("latest"));
        assert_eq!(
            store.snapshot().input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text,
            "Keep this draft"
        );
        assert!(store.snapshot().panes.primary().error.is_none());
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_failed_open_keeps_selection_and_draft_and_can_retry() {
    for cached in [false, true] {
        let history = op::OperationKey::History {
            session: SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap(),
        };
        let (store, mut reader, writer) = setup(if cached {
            snapshot()
        } else {
            Snapshot::default()
        })
        .await;
        let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        })));
        store
            .dispatch(Intent::SetDraftText {
                thread_id: SessionRef {
                    provider: Some(ProviderKind::Codex),
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
                .panes
                .primary()
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
                .operation_error(history.clone())
                .unwrap()
                .to_string()
                .contains("history unavailable")
        );
        let retry = store.dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        })));
        assert!(store.snapshot().panes.primary().error.is_none());
        assert!(store.snapshot().operation_error(history.clone()).is_none());
        let request = read(&mut reader).await;
        writer
            .reply(&request, json!({"result":{"thread":thread("recovered")}}))
            .await
            .unwrap();
        retry.await.unwrap();
        let recovered = store.snapshot();
        assert_eq!(loaded_text(&recovered), Some("recovered"));
        assert_eq!(
            recovered.input.drafts[&DraftKey::from(SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into()
            })]
                .text,
            "Written while loading"
        );
        assert!(recovered.panes.primary().error.is_none());
        assert!(recovered.operation_error(history).is_none());
        store.close().await.unwrap();
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_late_open_reply_caches_the_thread_without_leaving_a_new_chat(
    #[values(false, true)] active: bool,
) {
    let (store, mut reader, mut writer) = setup(Snapshot::default()).await;
    let opening = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::ReadThread(op::ReadThread::open(SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                })))
                .await
        }
    });
    let request = read(&mut reader).await;
    new_chat(&store, &mut reader, &mut writer, "/new-project").await;
    let new_draft = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    let mut loaded = thread("loaded");
    if !active {
        Arc::make_mut(&mut loaded.turns.as_mut().unwrap()[0]).status =
            agent_protocol::models::TurnStatus::Completed;
    }
    writer
        .reply(&request, json!({"result":{"thread":loaded}}))
        .await
        .unwrap();
    opening.await.unwrap().unwrap();
    let snapshot = store.snapshot();
    assert!(snapshot.panes.primary().navigation.thread_id.is_none());
    assert_eq!(snapshot.panes.primary().navigation.cwd, "/new-project");
    assert_eq!(snapshot.panes.primary().navigation.draft_key, new_draft);
    assert_ne!(
        new_draft,
        DraftKey::from(SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap())
    );
    assert_eq!(
        snapshot
            .sessions
            .subscriptions
            .contains_key(&SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap()),
        active
    );
    assert_eq!(loaded_text(&snapshot), Some("loaded"));
}

#[tokio::test]
async fn a_stale_catalogue_does_not_queue_a_completed_thread() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.sessions.conversations).insert(
        SessionRef { provider: Some(ProviderKind::Codex), id: "thread".into() },
        Arc::new(
            serde_json::from_value(
                json!({"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle","turns":[]}),
            )
            .unwrap(),
        ),
    );
    initial.lists.threads = Some(Arc::new(serde_json::from_value(json!({"data":[{"id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"running"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,})).unwrap()));
    let (store, mut reader, mut writer) = setup(initial).await;
    reader.script_decorations();
    writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"type":"turn","turn":{"id":"completed","status":"completed"},"completed":true}})).await.unwrap();
    wait_for(&store, |snapshot| {
        snapshot.lists.activity.active.get(&SessionRef {
            provider: Some(ProviderKind::Codex),
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
                provider: Some(ProviderKind::Codex),
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
                        provider: Some(ProviderKind::Codex),
                        id: "thread".into(),
                    }),
                    client_user_message_id: "26cd410e-2f6d-5b40-aa3e-902997b896d4".into(),
                    alternate: false,
                })
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/change");
    assert!(
        !store.snapshot().sessions.pending_submissions["26cd410e-2f6d-5b40-aa3e-902997b896d4"]
            .queued
    );
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    assert_eq!(sending.await.unwrap().unwrap(), Outcome::Submitted);
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
        let result = |id| json!({"data":[{"id":{"provider":"codex","id":id}}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,});
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
            store.snapshot().lists.threads.as_ref().unwrap().data[0]
                .id
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("new")
        );
        assert_eq!(store.snapshot().lists.list_query.search_term, "new");
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn gallery_history_reads_do_not_block_conversation_notifications() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/subscribe");
        writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"gallery"},"turns":[{"id":"image-turn","items":[{"id":"image","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}],"status":"unknown"}]}}})).await.unwrap();
        writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"thread"},"change":{"type":"text","turnId":"turn","itemId":"item","delta":" continued","field":"assistantText"}})).await.unwrap();
        (reader, writer)
    });
    let result = store
        .dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
            thread_id: SessionRef {
                provider: Some(ProviderKind::Codex),
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
async fn gallery_restarts_paging_after_a_history_reset() {
    use agent_protocol::sync::{Message, Position, Window};
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let id = SessionRef::new(ProviderKind::Codex, "reset-gallery".into()).unwrap();
    let server_id = id.clone();
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        let subscription =
            serde_json::from_value(request["params"]["subscription"].clone()).unwrap();
        writer.reply(&request, json!({"result":{"thread":{"id":server_id,"historyHasMore":true,"turns":[{"id":"image-turn","status":"completed","items":[{"id":"image","status":"completed","body":{"inline":{"body":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}]}]}}})).await.unwrap();
        let extend = read(&mut reader).await;
        assert_eq!(extend["method"], "host/conversation/extend");
        let mut reset = writer.replica(&server_id).snapshot().unwrap();
        let Message::Snapshot {
            cursor,
            window,
            rows,
        } = &mut reset
        else {
            unreachable!()
        };
        cursor.sequence += 1;
        *window = Some(Window {
            turn: 2000,
            item: 0,
        });
        for row in rows {
            match &mut row.position {
                Position::Turn { ordinal } | Position::Item { ordinal, .. } => *ordinal = 2000,
                _ => {}
            }
            if let agent_protocol::sync::Content::Full(record) = &mut row.content
                && let agent_protocol::conversation::records::Record::Turn(turn) =
                    Arc::make_mut(record)
            {
                turn.ordinal = 2000;
            }
        }
        let cursor = *cursor;
        writer.send_rows(subscription, reset).await.unwrap();
        writer
            .send_rows(
                subscription,
                Message::Extend {
                    cursor,
                    window: Window {
                        turn: 1000,
                        item: 0,
                    },
                    rows: vec![],
                },
            )
            .await
            .unwrap();
        extend.acknowledge().await.unwrap();
        let extend = read(&mut reader).await;
        assert_eq!(extend["method"], "host/conversation/extend");
        writer
            .send_rows(
                subscription,
                Message::Extend {
                    cursor,
                    window: Window { turn: 0, item: 0 },
                    rows: vec![],
                },
            )
            .await
            .unwrap();
        extend.acknowledge().await.unwrap();
        (reader, writer)
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        store.dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
            thread_id: id,
        })),
    )
    .await
    .unwrap()
    .unwrap();
    let Outcome::SessionImages { images } = result else {
        panic!("expected images")
    };
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].source, "/image.png");
    let _host = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn gallery_drains_live_frames_while_image_hydration_waits_for_the_host() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let id = SessionRef::new(ProviderKind::Codex, "gallery".into()).unwrap();
    let server_id = id.clone();
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/subscribe");
        let subscription =
            serde_json::from_value(request["params"]["subscription"].clone()).unwrap();
        writer.reply(&request, json!({"result":{"thread":{"id":server_id,"turns":[{"id":"image-turn","status":"completed","items":[{"id":"image","status":"completed","body":{"deferred":{"summary":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}]}]}}})).await.unwrap();
        let hydrate = read(&mut reader).await;
        assert_eq!(hydrate["method"], "host/conversation/hydrate");
        // Enough incompressible live rows to exceed QUIC's stream receive window.
        // Hydration cannot finish until the client consumes the pending frames.
        let mut seed = 1u64;
        let text: String = (0..256 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (b'!' + (seed % 90) as u8) as char
            })
            .collect();
        let mut message = writer.replica(&server_id).snapshot().unwrap();
        let agent_protocol::sync::Message::Snapshot { cursor, rows, .. } = &mut message else {
            unreachable!()
        };
        cursor.sequence += 1;
        for row in rows {
            if let agent_protocol::sync::Content::Full(record) = &mut row.content
                && let agent_protocol::conversation::records::Record::Thread(header) =
                    Arc::make_mut(record)
            {
                header.state.name = text.clone();
                row.version = cursor.sequence;
            }
        }
        for _ in 0..64 {
            writer
                .send_rows(subscription, message.clone())
                .await
                .unwrap();
        }
        writer.reply(&hydrate, json!({"result":{"item":{"id":"image","status":"completed","body":{"inline":{"body":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}}})).await.unwrap();
        (reader, writer)
    });
    let operation = store.dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
        thread_id: id,
    }));
    let result = tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .unwrap()
        .unwrap();
    let Outcome::SessionImages { images } = result else {
        panic!("expected images")
    };
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].source, "/image.png");
    let _host = server.await.unwrap();
    store.close().await.unwrap();
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
        let close = read(&mut reader).await;
        assert_eq!(close["method"], "host/terminal/kill");
        assert_eq!(close["params"]["processHandle"], "terminal");
        writer.notify(json!({"method":"host/terminal/exited","params":{"processHandle":"terminal","exitCode":0,"stdout":"","stderr":"","stdoutCapReached":false,"stderrCapReached":false}})).await.unwrap();
        writer.reply(&close, json!({"result":{}})).await.unwrap();
        // Keep the transport alive until Store closes it.
        assert!(reader.read_request().await.unwrap().is_none());
    });
    let start = store.dispatch(Intent::StartTerminal(op::StartTerminal {
        command: None,
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
    store
        .dispatch(Intent::CloseTerminal {
            handle: "terminal".into(),
        })
        .await
        .unwrap();
    assert!(!store.snapshot().terminals.contains_key("terminal"));
    store
        .dispatch(Intent::SetDraftText {
            thread_id: "draft".into(),
            text: "the conversation remains usable".into(),
        })
        .await
        .unwrap();
    store.close().await.unwrap();
    server.await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
async fn disconnect_does_not_wait_for_a_terminal_start_reply(
    #[values(false, true)] close_store: bool,
) {
    let (store, mut reader, _writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartTerminal(op::StartTerminal {
        command: None,
        handle: "starting".into(),
        cwd: "/fixture".into(),
        size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/terminal/start");
    // Retain the unanswered request through shutdown. Allow transport cleanup,
    // but require cancellation rather than the terminal RPC reaching its deadline.
    tokio::time::timeout(Duration::from_secs(5), async {
        if close_store {
            store.close().await
        } else {
            store.disconnect().await
        }
    })
    .await
    .expect("disconnect waited for terminal startup")
    .unwrap();
    assert!(reader.read_request().await.unwrap().is_none());
    assert!(matches!(
        starting.await,
        Err(PeerError::ConnectionClosed(_))
    ));
    if !close_store {
        store.close().await.unwrap();
    }
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
            command: None,
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
        lists: agent_core::state::Lists {
        threads: Some(Arc::new(
            serde_json::from_value(
                json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}),
            )
            .unwrap(),
        )),
        list_query: Arc::new(agent_protocol::models::ListQuery {
            search_term: "created".into(),
            ..Default::default()
        }),
            ..Default::default()
        },
        ..authenticated_snapshot()
    })
    .await;
    let server = tokio::spawn(async move {
        while let Ok(Some(request)) =
            tokio::time::timeout(Duration::from_secs(2), reader.read_request())
                .await
                .unwrap()
        {
            let result = match request["method"].as_str().unwrap() {
                "host/conversation/create" => {
                    json!({"thread":{"activeProvider":"codex","name":"created chat","cwd":"/fixture","turns":[],"status":"idle"}})
                }
                "host/conversation/subscribe" => writer.current(
                    &serde_json::from_value(request["params"]["session"].clone()).unwrap(),
                ),
                "host/workspace/review" => review(),
                other => panic!("unexpected method {other}"),
            };
            // Close may cancel an automatic workspace read after creation.
            if writer
                .reply(&request, json!({"result":result}))
                .await
                .is_err()
            {
                break;
            }
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
            thread_id: store
                .snapshot()
                .panes
                .primary()
                .navigation
                .draft_key
                .clone(),
            text: "message".into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::Submit {
            thread_id: None,
            client_user_message_id: "5b1986e4-742d-5d96-81d1-93be060b53a6".into(),
            alternate: false,
        })
        .await
        .unwrap();
    wait_for(&store, |state| {
        state.lists.threads.as_ref().is_some_and(|page| {
            page.data.iter().any(|thread| {
                thread.name.as_deref() == Some("created chat")
                    && thread.id.as_ref().is_some_and(|id| id.provider.is_none())
            })
        })
    })
    .await;
    assert_eq!(store.snapshot().lists.list_query.search_term, "created");
    assert!(store.snapshot().connection.connected);
    store.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn expanded_history_failure_preserves_cache_and_retry_adopts_complete_window() {
    let initial: Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"historyLimit":10,"historyHasMore":true,"turns":[{"id":"latest","items":[],"status":"unknown"}]})).unwrap();
    let (store, mut reader, writer) = setup(Snapshot {
        panes: agent_core::state::PaneStates::with_primary(Arc::new(
            agent_core::state::PaneState {
                navigation: Arc::new(agent_core::state::Navigation {
                    thread_id: initial.id.clone(),
                    draft_key: initial.id.clone().unwrap().into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        sessions: agent_core::state::Sessions {
            conversations: Arc::new(BTreeMap::from([(
                SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                },
                Arc::new(initial),
            )])),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let cached = store.snapshot().sessions.conversations.clone();
    let server = tokio::spawn(async move {
        for failed in [true, false] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/conversation/extend");
            assert_eq!(request["params"]["session"]["id"], "thread");
            assert!(request["params"]["subscription"].is_string());
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
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into()
                }
            })
            .await
            .is_err()
    );
    assert_eq!(store.snapshot().sessions.conversations, cached);
    store
        .dispatch(Intent::ReadOlder {
            thread_id: SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            },
        })
        .await
        .unwrap();
    let recovered = store.snapshot();
    let thread = &recovered.sessions.conversations[&SessionRef {
        provider: Some(ProviderKind::Codex),
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
    assert_eq!(recovered.panes.primary().error, None);
    let _server = server.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn expanded_history_replaces_the_window_preserving_native_item_ids() {
    let initial: Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"historyLimit":10,"historyHasMore":true,"turns":[{"id":"new","status":"completed","items":[{"id":"new-item","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"unknown"}}}}}]}]})).unwrap();
    let (store, mut reader, writer) = setup(Snapshot {
        panes: agent_core::state::PaneStates::with_primary(Arc::new(
            agent_core::state::PaneState {
                navigation: Arc::new(agent_core::state::Navigation {
                    thread_id: initial.id.clone(),
                    draft_key: initial.id.clone().unwrap().into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        sessions: agent_core::state::Sessions {
            conversations: Arc::new(BTreeMap::from([(
                SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                },
                Arc::new(initial),
            )])),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let server = tokio::spawn(async move {
        for (limit, complete) in [(20, false), (30, true)] {
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/conversation/extend");
            assert!(request["params"]["subscription"].is_string());
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
                    provider: Some(ProviderKind::Codex),
                    id: "thread".into(),
                },
            })
            .await
            .unwrap();
    }
    let snapshot = store.snapshot();
    let thread = &snapshot.sessions.conversations[&SessionRef {
        provider: Some(ProviderKind::Codex),
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
        let source = SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        };
        let mut initial = snapshot();
        let draft = Arc::new(Draft {
            text: "unsent message".into(),
            ..Default::default()
        });
        Arc::make_mut(&mut initial.input.drafts).insert(source.clone().into(), draft.clone());
        let (store, mut reader, mut writer) = setup(initial).await;
        let fork = store.dispatch(Intent::ForkSession(op::ForkSession::new(
            SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            },
            "turn".into(),
        )));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/fork");
        assert_eq!(request["params"]["through"], "turn");
        let target: SessionRef =
            serde_json::from_value(request["params"]["target"].clone()).unwrap();
        let progress = op::OperationKey::Fork {
            session: source.clone(),
            turn: "turn".into(),
        };
        assert!(store.snapshot().operation_running(progress.clone()));
        if navigate {
            new_chat(&store, &mut reader, &mut writer, "/new").await;
        }
        writer.reply(&request, json!({"result":{"model":{"provider":"codex","id":"fork-model"},"thread":{"activeProvider":"codex","cwd":"/fixture","turns":[{"id":"copy","items":[{"id":"reply","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"copied","phase":"unknown"}}}}}],"status":"unknown"}]}}})).await.unwrap();
        if !navigate {
            loop {
                let opening = read(&mut reader).await;
                if opening["method"] == "host/conversation/subscribe" {
                    writer
                        .reply(&opening, json!({"result":writer.current(&target.clone())}))
                        .await
                        .unwrap();
                    break;
                }
                let result = if opening["method"] == "host/workspace/review" {
                    review()
                } else {
                    json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,})
                };
                writer
                    .reply(&opening, json!({"result":result}))
                    .await
                    .unwrap();
            }
            writer.notify(json!({"method":"fixture/session/change","session":target,"change":{"type":"text","turnId":"copy","itemId":"reply","delta":" later","field":"assistantText"}})).await.unwrap();
        }
        assert_eq!(
            fork.await.unwrap(),
            Outcome::StartedThread { id: target.clone() }
        );
        if !navigate {
            wait_for(&store, |s| {
                s.sessions
                    .conversations
                    .get(&target.clone())
                    .and_then(|t| t.turns.as_ref())
                    .is_some_and(|turns| {
                        item_text(&(turns[0].items.as_ref().unwrap()[0])) == Some("copied later")
                    })
            })
            .await;
        } else {
            assert!(
                !store
                    .snapshot()
                    .sessions
                    .subscriptions
                    .contains_key(&target)
            );
        }
        assert_eq!(
            store
                .snapshot()
                .panes
                .primary()
                .navigation
                .thread_id
                .as_ref()
                .map(|session| session.id.as_str()),
            if navigate {
                None
            } else {
                Some(target.id.as_str())
            }
        );
        assert_eq!(
            store.snapshot().panes.primary().navigation.cwd,
            if navigate { "/new" } else { "/fixture" }
        );
        assert!(!store.snapshot().operation_running(progress));
        assert_eq!(loaded_text(&store.snapshot()), Some("old"));
        assert_eq!(
            store.snapshot().input.drafts[&DraftKey::from(source)],
            draft
        );
        let fork_key = DraftKey::from(target.clone());
        if navigate {
            assert!(!store.snapshot().input.drafts.contains_key(&fork_key));
        } else {
            let mut expected = draft.as_ref().clone();
            expected.model = Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Codex,
                id: "fork-model".into(),
            });
            assert_eq!(store.snapshot().input.drafts[&fork_key].as_ref(), &expected);
        }
        store.close().await.unwrap();
    }
}

#[rstest::rstest]
#[tokio::test]
async fn rewinding_drops_the_later_turns_and_returns_the_message_to_the_composer(
    #[values(false, true)] navigate: bool,
    #[values(
        "/fixture/shot.png",
        r"C:\fixture\shot.png",
        r"\\server\share\shot.png"
    )]
    image: &str,
) {
    let source = SessionRef {
        provider: Some(ProviderKind::Codex),
        id: "thread".into(),
    };
    let mut initial = snapshot();
    let user = |id: &str, content: serde_json::Value| json!({"id":id,"status":"completed","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":content}}}}});
    let kept = json!({"id":"kept","status":"completed","items":[user("a",json!([{"text":{"text":"earlier"}}]))]});
    let thread: Thread = serde_json::from_value(json!({
        "id":{"provider":"codex","id":"thread"},"cwd":"/fixture","status":"idle",
        "turns":[kept.clone(),
            {"id":"edit","status":"completed","items":[user("b",json!([
                {"text":{"text":"send this again"}},
                {"image":{"source":image}},
                {"image":{"source":"data:image/png;base64,AAAA"}},
                {"attachment":{"name":"notes.md","path":"/fixture/notes.md"}}
            ]))]},
            {"id":"later","status":"completed","items":[user("c",json!([{"text":{"text":"removed"}}]))]}]
    })).unwrap();
    Arc::make_mut(&mut initial.sessions.conversations).insert(source.clone(), Arc::new(thread));
    Arc::make_mut(&mut initial.input.drafts).insert(
        source.clone().into(),
        Arc::new(Draft {
            text: "replaced".into(),
            ..Default::default()
        }),
    );
    Arc::make_mut(initial.panes.primary_mut()).navigation =
        Arc::new(agent_core::state::Navigation {
            thread_id: Some(source.clone()),
            draft_key: source.clone().into(),
            cwd: "/fixture".into(),
        });
    let (store, mut reader, writer) = setup(initial).await;
    let rewind = store.dispatch(Intent::RewindSession(op::RewindSession::new(
        source.clone(),
        "edit".into(),
    )));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/change");
    assert_eq!(request["params"]["command"]["Rewind"], "edit");
    if navigate {
        let navigation = store.dispatch(Intent::NewChat {
            cwd: "/else".into(),
        });
        let review_request = read(&mut reader).await;
        assert_eq!(review_request["method"], "host/workspace/review");
        writer
            .reply(&review_request, json!({"result":review()}))
            .await
            .unwrap();
        navigation.await.unwrap();
    }
    // An active subscription receives the authoritative replacement before
    // the command ACK. A conversation left in the meantime stays unsubscribed
    // until an explicit read catches up its cached cursor.
    let subscription = store
        .snapshot()
        .sessions
        .subscriptions
        .get(&source)
        .copied();
    assert_eq!(subscription.is_some(), !navigate);
    if let Some(subscription) = subscription {
        let current = store.snapshot();
        let response: agent_protocol::models::ThreadResponse = serde_json::from_value(
            json!({"thread": {"id":source,"cwd":"/fixture","turns":[kept.clone()]}}),
        )
        .unwrap();
        let sequence = current.sessions.replicas[&source]
            .cursor()
            .unwrap()
            .sequence
            + 1;
        let replacement = host_fixture::rows::replica_at(response, 1, sequence)
            .snapshot()
            .unwrap();
        writer.send_rows(subscription, replacement).await.unwrap();
    }
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    rewind.await.unwrap();
    if navigate {
        assert!(
            !store
                .snapshot()
                .sessions
                .subscriptions
                .contains_key(&source)
        );
        let reopening = store.dispatch(Intent::ReadThread(op::ReadThread::new(source.clone())));
        let refresh = loop {
            let request = read(&mut reader).await;
            if request["method"] == "host/conversation/subscribe" {
                break request;
            }
            assert_eq!(request["method"], "host/workspace/review");
            writer
                .reply(&request, json!({"result":review()}))
                .await
                .unwrap();
        };
        let current = store.snapshot();
        let response: agent_protocol::models::ThreadResponse =
            serde_json::from_value(json!({"thread":{"id":source,"cwd":"/fixture","turns":[kept]}}))
                .unwrap();
        let sequence = current.sessions.replicas[&source]
            .cursor()
            .unwrap()
            .sequence
            + 1;
        let opened = agent_protocol::conversation::records::Opened {
            session: source.clone(),
            subscription_id: uuid::Uuid::nil(),
            initial: host_fixture::rows::replica_at(response, 1, sequence)
                .snapshot()
                .unwrap(),
        };
        writer
            .reply(&refresh, json!({"result":opened}))
            .await
            .unwrap();
        reopening.await.unwrap();
        assert!(
            !store
                .snapshot()
                .sessions
                .subscriptions
                .contains_key(&source)
        );
    }
    let mut changes = store.subscribe();
    tokio::time::timeout(
        Duration::from_secs(2),
        changes.wait_for(|snapshot| {
            snapshot.sessions.conversations[&source]
                .turns
                .as_ref()
                .is_some_and(|turns| turns.len() == 1)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snapshot = store.snapshot();
    let turns = snapshot.sessions.conversations[&source]
        .turns
        .clone()
        .unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["kept"]
    );
    if navigate {
        assert_eq!(snapshot.panes.primary().navigation.cwd, "/else");
        assert_eq!(snapshot.panes.primary().navigation.thread_id, None);
    }
    let draft = &snapshot.input.drafts[&DraftKey::from(source)];
    assert_eq!(draft.text, "send this again");
    assert_eq!(
        draft
            .attachments
            .iter()
            .map(|file| (file.name.as_str(), file.path.as_str(), file.is_image))
            .collect::<Vec<_>>(),
        [
            ("shot.png", image, true),
            ("notes.md", "/fixture/notes.md", false)
        ]
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn fork_failure_clears_progress_and_allows_retry() {
    let (store, mut reader, writer) = setup(snapshot()).await;
    let selected_before = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .thread_id
        .clone();
    let source = SessionRef {
        provider: Some(ProviderKind::Codex),
        id: "thread".into(),
    };
    let progress = op::OperationKey::Fork {
        session: source.clone(),
        turn: "turn".into(),
    };
    for _ in 0..2 {
        let fork = store.dispatch(Intent::ForkSession(op::ForkSession::new(
            source.clone(),
            "turn".into(),
        )));
        assert!(store.snapshot().operation_running(progress.clone()));
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/conversation/fork");
        writer.reply(&request, json!({"error":{"code":"unavailable","message":"fork failed","delivery":"notSent"}})).await.unwrap();
        assert!(fork.await.is_err());
        assert!(!store.snapshot().operation_running(progress.clone()));
        assert!(store.snapshot().operation_error(progress.clone()).is_some());
        assert_eq!(
            store.snapshot().panes.primary().navigation.thread_id,
            selected_before
        );
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn account_selection_publishes_the_selected_account_and_persistence_warning() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let request = read(&mut reader).await;
    writer.reply(&request, json!({"result":{"accounts":[{"provider":"codex","id":"a"},{"provider":"claude","id":"claude:c"}],"selected":{"codex":"a","claude":"claude:c"},"errors":{}}})).await.unwrap();
    listing.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .catalog
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
            .catalog
            .account
            .accounts
            .as_ref()
            .unwrap()
            .selected
            .get(&agent_protocol::session::ProviderKind::Codex)
            .map(String::as_str),
        Some("b")
    );
    assert_eq!(
        store.snapshot().panes.primary().error.as_deref(),
        Some("store unavailable")
    );
    assert_eq!(
        store
            .snapshot()
            .catalog
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
                json!({"accounts":[{"provider":"codex","id":"a"},{"provider":"codex","id":"b"},{"provider":"claude","id":"claude:c"}],"selected":{"codex":"b","claude":"claude:c"},"errors":{}})
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
        s.catalog.account.accounts.as_ref().is_some_and(|accounts| {
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

async fn start_claude_login(
    store: &Store,
    reader: &mut host_fixture::Reader,
    writer: &host_fixture::Writer,
) {
    let starting = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
        provider: ProviderKind::Claude,
        thread_id: None,
    }));
    let request = read(reader).await;
    writer.reply(&request, json!({"result":{"provider":"claude","loginId":"login","userCode":"","requiresCodeSubmission":true,"verificationUrl":"https://example.invalid/authorize?state=fixture"}})).await.unwrap();
    starting.await.unwrap();
}

#[tokio::test]
async fn ended_account_login_clears_only_its_own_form_and_can_be_started_again() {
    for submitting in [false, true] {
        for (code, provider, id, cleared) in [
            ("account_login_ended", ProviderKind::Claude, "login", true),
            (
                "account_operation_failed",
                ProviderKind::Claude,
                "login",
                false,
            ),
            ("account_login_ended", ProviderKind::Claude, "other", false),
            ("account_login_ended", ProviderKind::Codex, "login", false),
        ] {
            let (store, mut reader, writer) = setup(Snapshot::default()).await;
            start_claude_login(&store, &mut reader, &writer).await;
            let intent = if submitting {
                Intent::SubmitAccountLogin(op::SubmitAccountLogin {
                    provider,
                    id: id.into(),
                    code: "fixture#fixture".into(),
                })
            } else {
                Intent::ReadAccountLogin(op::ReadAccountLogin {
                    provider,
                    id: id.into(),
                })
            };
            let receipt = store.dispatch(intent);
            let request = read(&mut reader).await;
            writer
                .reply(
                    &request,
                    json!({"error":{"code":code,"message":"sign-in ended","delivery":"notSent"}}),
                )
                .await
                .unwrap();
            assert!(receipt.await.is_err());
            assert_eq!(store.snapshot().catalog.account.login.is_none(), cleared);
            assert!(
                store
                    .snapshot()
                    .operation_error(agent_core::state::operations::OperationKey::AccountLogin {
                        provider
                    })
                    .is_some()
            );
            let retry = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
                provider,
                thread_id: None,
            }));
            let request = read(&mut reader).await;
            writer.reply(&request, json!({"result":{"provider":provider,"loginId":"retry","userCode":"","requiresCodeSubmission":true,"verificationUrl":"https://example.invalid/authorize?state=retry"}})).await.unwrap();
            retry.await.unwrap();
            assert_eq!(
                store
                    .snapshot()
                    .catalog
                    .account
                    .login
                    .as_ref()
                    .unwrap()
                    .login_id,
                "retry"
            );
            store.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn cancelling_an_obsolete_form_keeps_confirming_the_current_login() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    start_claude_login(&store, &mut reader, &writer).await;
    let cancelling = store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
        provider: ProviderKind::Claude,
        id: "obsolete".into(),
    }));
    let request = read(&mut reader).await;
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    cancelling.await.unwrap();
    assert!(store.snapshot().catalog.account.login.is_some());
    let status = read_login_poll(&mut reader).await;
    assert_eq!(status["method"], "host/account/login/status");
    assert_eq!(
        status["params"],
        json!({"provider":"claude","loginId":"login"})
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_account_listing_preserves_login_and_cancellation_ignores_late_status() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
        provider: ProviderKind::Codex,
        thread_id: None,
    }));
    let request = read(&mut reader).await;
    let listing = store.dispatch(Intent::ListAccounts(op::ListAccounts {}));
    let list = read(&mut reader).await;
    writer
        .reply(
            &list,
            json!({"result":{"accounts":[],"selected":{},"errors":{}}}),
        )
        .await
        .unwrap();
    listing.await.unwrap();
    writer.reply(&request, json!({"result":{"provider":"codex","loginId":"login","userCode":"fixture-only","requiresCodeSubmission":false,"verificationUrl":"https://example.invalid"}})).await.unwrap();
    starting.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .catalog
            .account
            .login
            .as_ref()
            .unwrap()
            .login_id,
        "login"
    );

    let polling = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "login".into(),
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
    assert!(state.catalog.account.login.is_none());
    assert!(
        state
            .catalog
            .account
            .accounts
            .as_ref()
            .unwrap()
            .accounts
            .is_empty()
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn the_store_confirms_a_pending_login_until_it_completes_and_stops_on_failure() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let starting = store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
        provider: ProviderKind::Codex,
        thread_id: None,
    }));
    let request = read(&mut reader).await;
    writer.reply(&request, json!({"result":{"provider":"codex","loginId":"login","userCode":"fixture-only","requiresCodeSubmission":false,"verificationUrl":"https://example.invalid"}})).await.unwrap();
    starting.await.unwrap();
    // Navigation does not end confirmation of a Host-owned login.
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    let status = read_login_poll(&mut reader).await;
    assert_eq!(status["method"], "host/account/login/status");
    assert_eq!(
        status["params"],
        json!({"provider":"codex","loginId":"login"})
    );
    writer
        .reply(&status, json!({"error":{"code":"request_failed","message":"login status unavailable","delivery":"notSent"}}))
        .await
        .unwrap();
    wait_for(&store, |snapshot| {
        snapshot
            .operation_error(op::OperationKey::AccountLogin {
                provider: ProviderKind::Codex,
            })
            .is_some()
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), reader.read_request())
            .await
            .is_err(),
        "a failed confirmation waits for the user to retry"
    );
    let retrying = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
        provider: ProviderKind::Codex,
        id: "login".into(),
    }));
    let retry = read(&mut reader).await;
    writer
        .reply(&retry, json!({"result":{"completed":false}}))
        .await
        .unwrap();
    retrying.await.unwrap();
    let status = read_login_poll(&mut reader).await;
    assert_eq!(status["method"], "host/account/login/status");
    writer
        .reply(
            &status,
            json!({"result":{"completed":true,"accountId":"added"}}),
        )
        .await
        .unwrap();
    let select = read_login_poll(&mut reader).await;
    assert_eq!(select["method"], "host/account/select");
    assert_eq!(
        select["params"],
        json!({"provider":"codex","accountId":"added"})
    );
    assert!(store.snapshot().catalog.account.login.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn notification_eof_publishes_disconnection_before_waiting_for_close_ack() {
    use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
    let host = Endpoint::bind(Identity::generate(), Relays::Loopback)
        .await
        .unwrap();
    let client = Endpoint::bind(Identity::generate(), Relays::Loopback)
        .await
        .unwrap();
    let ticket = host.ticket();
    let (remote, incoming) = tokio::join!(client.connect(&ticket), host.accept());
    let remote = remote.unwrap();
    let incoming = incoming
        .unwrap()
        .unwrap()
        .authorize(&Trust {
            allowed: [client.node_id()].into(),
            ..Default::default()
        })
        .unwrap();
    let (peer, events) = tokio::join!(
        remote.open_peer(Duration::from_secs(1), 16),
        incoming.accept_peer()
    );
    let store = Store::new(peer.unwrap(), Snapshot::default(), None);
    events.unwrap().finish().unwrap();
    // Keep the Host alive without acknowledging CLOSE. Native input must still
    // observe the lost event stream before transport cleanup finishes.
    wait_for(&store, |snapshot| !snapshot.connection.connected).await;
    incoming.close();
    store.close().await.unwrap();
}

#[tokio::test]
async fn disconnected_store_keeps_editing_and_persisting_drafts() {
    let (store, reader, writer) = setup(authenticated_snapshot()).await;
    wait_for(&store, |s| s.connection.connected).await;
    drop((reader, writer));
    wait_for(&store, |s| !s.connection.connected).await;
    store
        .dispatch(Intent::NewChat {
            cwd: "/offline".into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: store
                .snapshot()
                .panes
                .primary()
                .navigation
                .draft_key
                .clone(),
            text: "切断中の下書き".into(),
        })
        .await
        .unwrap();
    let serialized = serde_json::to_vec(&store.snapshot()).unwrap();
    let restored: Snapshot = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(
        restored.input.drafts[&restored.panes.primary().navigation.draft_key].text,
        "切断中の下書き"
    );
    assert!(
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "4e5b8505-e836-574a-8224-669e68611b25".into(),
                alternate: false,
            })
            .await
            .is_err()
    );
    assert!(store.snapshot().sessions.pending_submissions.is_empty());
    assert_eq!(
        store.snapshot().input.drafts[&store.snapshot().panes.primary().navigation.draft_key].text,
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
                lists: agent_core::state::Lists {
                    list_query: Arc::new(if mode == "zero-limits" {
                        ListQuery { limit: 0, ..Default::default() }
                    } else { ListQuery::default() }),
                    ..Default::default()
                },
                ..Default::default()
            }, None));
            let connecting = tokio::spawn({
                let store = store.clone();
                let client = client.clone();
                let ticket = host.ticket();
                async move { store.reconnect(&client, &ticket, None, "Fixture device").await }
            });
            let incoming = host.accept().await.unwrap().unwrap().authorize(&trust).unwrap();
            let (session, mut reader, writer) = host_fixture::accept(incoming).await;
            let scope = read(&mut reader).await;
            assert_eq!(scope["method"], "host/device/identify");
            assert!(tokio::time::timeout(Duration::from_millis(40), reader.read_request()).await.is_err(),
                "an unverified candidate must not load titles");
            assert!(!store.snapshot().connection.connected);
            assert!(store.snapshot().lists.threads.is_none());
            let titles = |id| json!({"result":{"data":[{"id":{"provider":"codex","id":id},"name":"title"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5}});
            if mode == "changed-query" {
                let _ = store.dispatch(Intent::ListSessions(op::ListSessions::new(ListQuery { search_term: "new query".into(), ..Default::default() }))).await;
            }
            writer.reply(&scope, json!({"result":{"storageScope":if mode == "rejected" { "" } else { "verified-storage" },"ticket":""}})).await.unwrap();
            let connected = connecting.await.unwrap();
            if mode == "rejected" {
                assert!(connected.is_err());
                assert!(!store.snapshot().connection.connected);
                assert!(store.snapshot().lists.threads.is_none());
                assert!(!matches!(reader.read_request().await, Ok(Some(_))));
            } else {
                connected.unwrap();
                assert!(store.snapshot().connection.connected);
                assert!(store.snapshot().lists.threads.is_none());
                let requests = if mode == "changed-query" { 4 } else { 3 };
                for _ in 0..requests {
                    let request = read(&mut reader).await;
                    let result = match request["method"].as_str().unwrap() {
                        "host/list/subscribe" => {
                            assert!(uuid::Uuid::parse_str(request["params"]["subscription"].as_str().unwrap()).is_ok());
                            assert!(request["params"]["cached"].is_null());
                            titles("fresh")["result"].clone()
                        }
                        "host/list/search" => {
                            assert_eq!(request["params"]["term"], "new query");
                            assert_eq!(request["params"]["limit"], 50);
                            titles("fresh")["result"].clone()
                        },
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        method => panic!("unexpected bootstrap: {method}"),
                    };
                    writer.reply(&request, json!({"result": result})).await.unwrap();
                }
                wait_for(&store, |state| state.lists.threads.is_some()).await;
                assert_eq!(store.snapshot().lists.threads.as_ref().unwrap().data[0].id.as_ref().map(|session| session.id.as_str()), Some("fresh"));
            }
            session.close();
            store.close().await.unwrap();
            client.close().await;
            host.close().await;
        }).await.unwrap_or_else(|_| panic!("pipeline timed out: {mode}"));
    }
}

#[tokio::test]
async fn cached_scope_starts_reads_before_the_host_confirms_it_and_holds_user_work() {
    use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
    for changed in [false, true] {
        tokio::time::timeout(Duration::from_secs(5), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let trust = Trust { allowed: [client.node_id()].into(), ..Default::default() };
            let cached = format!("{}:cached", host.node_id());
            let store = Arc::new(Store::offline(Snapshot {
                connection: agent_core::state::Connection {
                    storage_scope: cached.clone(),
                    ..Default::default()
                },
                ..Default::default()
            }, None));
            let connecting = tokio::spawn({
                let (store, client, ticket) = (store.clone(), client.clone(), host.ticket());
                async move { store.reconnect(&client, &ticket, None, "Fixture device").await }
            });
            let incoming = host.accept().await.unwrap().unwrap().authorize(&trust).unwrap();
            let (session, mut reader, writer) = host_fixture::accept(incoming).await;
            let identify = read(&mut reader).await;
            assert_eq!(identify["method"], "host/device/identify");
            assert_eq!(identify["params"]["name"], "Fixture device");
            connecting.await.unwrap().unwrap();
            assert!(store.snapshot().connection.connected);
            // The connection's own reads do not wait for the Host's answer.
            let refresh = read(&mut reader).await;
            assert_ne!(refresh["method"], "host/worktree/settings/read");
            let settings = store.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
            let mut held = true;
            while let Ok(request) = tokio::time::timeout(Duration::from_millis(100), reader.read_request()).await {
                held &= request.unwrap().unwrap()["method"] != "host/worktree/settings/read";
            }
            assert!(held, "user work must wait for the Host to confirm the cached storage");
            let ticket = if changed { Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap().ticket() } else { host.ticket() }.to_string();
            writer.reply(&identify, json!({"result":{"storageScope":if changed { "changed" } else { "cached" },"ticket":ticket,"homeDirectory":"/actual-host-home"}})).await.unwrap();
            if changed {
                assert!(settings.await.is_err(), "work started against the old storage must not reach the new one");
                wait_for(&store, |state| state.connection.storage_scope == format!("{}:changed", host.node_id())).await;
                assert_eq!(store.snapshot().connection.host_ticket, None, "another Host's addresses must not replace the ticket");
            } else {
                let request = loop {
                    let request = read(&mut reader).await;
                    if request["method"] == "host/worktree/settings/read" { break request; }
                };
                writer.reply(&request, json!({"result":{"createOnNewSession":false,"copyOnCreate":false,"copyPaths":[],"worktreeDirectory":"fixture"}})).await.unwrap();
                settings.await.unwrap();
                assert_eq!(store.snapshot().connection.storage_scope, cached);
                assert_eq!(store.snapshot().connection.host_ticket, Some(host.ticket().to_string()));
            }
            assert_eq!(store.snapshot().connection.host_home_directory.as_deref(), Some("/actual-host-home"));
            session.close();
            store.close().await.unwrap();
            client.close().await;
            host.close().await;
        })
        .await
        .unwrap_or_else(|_| panic!("cached scope connection timed out: changed={changed}"));
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
        let store = Arc::new(Store::offline(Snapshot::default(), None));
        let connecting = {
            let store = store.clone();
            let ticket = host.ticket();
            tokio::spawn(async move {
                store
                    .reconnect(&client, &ticket, Some(uuid::Uuid::new_v4()), "Fixture device")
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
                thread_id: store.snapshot().panes.primary().navigation.draft_key.clone(),
                text: "接続待ち中の編集".into(),
            })
            .await
            .unwrap();
        let drafts = store.snapshot().input.drafts.clone();
        let (session, _peer) = pairing.authorize(&trust).await.unwrap();
        // The device names itself in the request that also reads the storage scope.
        let agent_transport::transport::IncomingRequest::Call(mut identify) =
            session.accept_request().await.unwrap()
        else {
            panic!("device identification expected")
        };
        assert!(matches!(
            &identify.call,
            agent_protocol::protocol::Call::IdentifyDevice(params) if params.name == "Fixture device"
        ));
        agent_transport::framing::write(
            &mut identify.send,
            agent_protocol::protocol::Response::Success {
                result: agent_protocol::operations::HostIdentity {
                    storage_scope: "fixture-storage".into(),
                    ticket: String::new(),
                    home_directory: Some("/host/home".into()),
                },
            },
        )
        .await
        .unwrap();
        connecting.await.unwrap().unwrap();
        wait_for(&store, |state| state.connection.connected).await;
        assert!(Arc::ptr_eq(&drafts, &store.snapshot().input.drafts));
        assert_eq!(
            store.snapshot().input.drafts[&store.snapshot().panes.primary().navigation.draft_key].text,
            "接続待ち中の編集"
        );
        assert_eq!(store.snapshot().panes.primary().navigation.cwd, "/pairing");
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
                tokio::spawn(async move { store.reconnect(&client, &ticket, Some(uuid::Uuid::new_v4()), "Fixture device").await })
            };
            let pending = host.accept().await.unwrap().unwrap().pairing().await.unwrap();
            assert!(old_reader.read_request().await.unwrap().is_none(), "core must release the old connection before pairing");
            assert!(!store.snapshot().connection.connected);
            store.dispatch(Intent::SetDraftText { thread_id: "local".into(), text: "接続待ち中の編集".into() }).await.unwrap();
            let drafts = store.snapshot().input.drafts.clone();
            let replacement = match action {
                "reconnect" => {
                    let ticket = host.ticket();
                    let (result, incoming) = tokio::join!(store.reconnect(&client, &ticket, None, "Fixture device"), scoped_incoming(&host, &trust));
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
                let ready = wait_for(&store, |state| state.lists.task_activity.is_some() && state.lists.threads.as_ref().is_some_and(|list| list.data.iter().any(|thread| thread.id.as_ref().is_some_and(|id| id.id == "replacement"))));
                tokio::pin!(ready);
                loop {
                    let request = tokio::select! {
                        _ = &mut ready => break,
                        request = reader.read_request() => request.unwrap().unwrap(),
                    };
                    let result = match request["method"].as_str().unwrap() {
                        "host/list/subscribe" => json!({"data":[{"id":{"provider":"codex","id":"replacement"},"name":"fresh"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        other => panic!("unexpected bootstrap: {other}"),
                    };
                    writer.reply(&request, json!({"result":result})).await.unwrap();
                }
                assert_eq!(store.snapshot().lists.threads.as_ref().unwrap().data[0].id.as_ref().unwrap().id,"replacement");
                assert!(store.snapshot().connection.connected);
                assert!(store.snapshot().panes.primary().error.is_none());
                store.close().await.unwrap();
                session.close();
            } else {
                assert!(!store.snapshot().connection.connected, "cancelled setup must remain offline: {action}");
                store.close().await.unwrap();
            }
            assert!(Arc::ptr_eq(&drafts, &store.snapshot().input.drafts));
            assert_eq!(store.snapshot().input.drafts[&DraftKey::from("local")].text, "接続待ち中の編集");
            client.close().await;
            host.close().await;
        }).await.unwrap_or_else(|_| panic!("timed out: {action}"));
    }
}

#[tokio::test]
async fn dispatch_publishes_edits_before_returning_to_the_native_input_control() {
    let store = Store::offline(Snapshot::default(), None);
    let navigation = store.dispatch(Intent::NewChat {
        cwd: "/input".into(),
    });
    let key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    assert_ne!(
        key,
        Snapshot::default().panes.primary().navigation.draft_key
    );
    let edit = store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "入力を戻さない".into(),
    });
    assert_eq!(store.snapshot().input.drafts[&key].text, "入力を戻さない");
    let second = store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "second".into(),
    });
    second.await.unwrap();
    edit.await.unwrap();
    navigation.await.unwrap();
    assert_eq!(store.snapshot().input.drafts[&key].text, "second");
    drop(store.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "last".into(),
    }));
    assert_eq!(store.snapshot().input.drafts[&key].text, "last");
    store.close().await.unwrap();
}

#[tokio::test]
async fn close_ends_subscriptions_while_store_is_retained() {
    let (store, _reader, _writer) = setup(Snapshot::default()).await;
    wait_for(&store, |snapshot| snapshot.connection.connected).await;
    let mut updates = store.subscribe();
    store.close().await.unwrap();
    assert!(!store.snapshot().connection.connected);
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
    saved.connection.connected = true;
    Arc::make_mut(saved.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: Some(SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        }),
        draft_key: SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        }
        .into(),
        ..Default::default()
    });
    saved.lists.activity = Arc::new(Activity {
        active: BTreeMap::from([(
            SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            },
            true,
        )]),
        ..Default::default()
    });
    saved.input.drafts = Arc::new(BTreeMap::from([(
        SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        }
        .into(),
        Arc::default(),
    )]));
    saved.sessions.pending_submissions = Arc::new(BTreeMap::from([(
        "ba739aed-7c1b-5223-83c6-26f68997d8bf".into(),
        Arc::new(PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: None,
            draft_key: SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into(),
            draft,
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
            queued: false,
        }),
    )]));
    Arc::make_mut(
        Arc::make_mut(&mut saved.sessions.conversations)
            .get_mut(saved.panes.primary().navigation.thread_id.as_ref().unwrap())
            .unwrap(),
    ).requests = BTreeMap::from([("1".into(), Arc::new(serde_json::from_value(json!({"id": "1", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}})).unwrap()))]);
    let bytes = serde_json::to_vec(&saved).unwrap();
    let (store, _reader, _writer) = connected(serde_json::from_slice(&bytes).unwrap()).await;
    wait_for(&store, |snapshot| snapshot.connection.connected).await;
    let current = store.snapshot();
    assert!(current.requests().next().is_none());
    assert!(current.lists.activity.active.is_empty());
    assert!(current.sessions.subscriptions.is_empty());
    assert!(
        current.sessions.pending_submissions["ba739aed-7c1b-5223-83c6-26f68997d8bf"]
            .delivery_unknown
    );
    assert_eq!(
        current.sessions.pending_submissions["ba739aed-7c1b-5223-83c6-26f68997d8bf"]
            .draft
            .text,
        "typed\nspoken"
    );
    assert!(
        current.input.drafts[&DraftKey::from(SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into()
        })]
            .text
            .is_empty()
    );
    assert!(
        current.sessions.conversations[&SessionRef {
            provider: Some(ProviderKind::Codex),
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
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into(),
            text: "unsent".into(),
        })
        .await
        .unwrap();
    drop(store.dispatch(Intent::Submit {
        thread_id: Some(SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "thread".into(),
        }),
        client_user_message_id: "ba739aed-7c1b-5223-83c6-26f68997d8bf".into(),
        alternate: false,
    }));
    read(&mut reader).await;
    assert!(!store.snapshot().sessions.pending_submissions.is_empty());
    drop(
        store.dispatch(Intent::SetDraftText {
            thread_id: SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "thread".into(),
            }
            .into(),
            text: "newest edit".into(),
        }),
    );
    store.close().await.unwrap();
    let current = store.snapshot();
    assert!(
        current.sessions.pending_submissions["ba739aed-7c1b-5223-83c6-26f68997d8bf"]
            .delivery_unknown
    );
    assert_eq!(
        current.sessions.pending_submissions["ba739aed-7c1b-5223-83c6-26f68997d8bf"]
            .draft
            .text,
        "unsent"
    );
    assert_eq!(
        current.input.drafts[&DraftKey::from(SessionRef {
            provider: Some(ProviderKind::Codex),
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
                Store::connect(
                    &client,
                    &ticket,
                    Snapshot::default(),
                    None,
                    "Fixture device",
                    None
                ),
                scoped_incoming(&host, &trust)
            );
            stores.push(store.unwrap());
            sessions.push(incoming);
        }
        stores[0].close().await.unwrap();
        // A new session proves the shared endpoint survived closing the first view.
        let (third, incoming) = tokio::join!(
            Store::connect(
                &client,
                &ticket,
                Snapshot::default(),
                None,
                "Fixture device",
                None
            ),
            scoped_incoming(&host, &trust)
        );
        let third = third.unwrap();
        let (incoming, _reader, _writer) = incoming;
        assert!(stores[1].snapshot().connection.connected);
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
            Intent::ReviewWorkspace(op::ReviewWorkspace {
                base_ref: None,
                cwd: "/old".into(),
            }),
            json!({"controls":{"branches":[],"references":[],"remotes":[],"can_push":false,"scripts":[]},"branch":"main","base_ref":"origin/main","additions":0,"deletions":0,"files":[],"diff":"old"}),
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
        assert_eq!(
            request["params"],
            json!({"cwd":"/new","includeOptions":true})
        );
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
    assert!(store.snapshot().panes.primary().error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn saving_after_navigation_rebases_newer_edits_without_restoring_the_old_file() {
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut Arc::make_mut(initial.panes.primary_mut()).workspace).file = Some(Arc::new(
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
    assert_eq!(saved.files.file_drafts["/old/file"].text, "newer");
    assert_eq!(saved.files.file_drafts["/old/file"].revision, "saved");
    assert!(saved.panes.primary().workspace.file.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn host_catalogs_read_during_navigation_apply_without_another_request() {
    let (store, mut reader, writer) = connected(Snapshot::default()).await;
    let mut pending = BTreeMap::new();
    for _ in 0..3 {
        let request = read(&mut reader).await;
        pending.insert(request["method"].as_str().unwrap().to_owned(), request);
    }
    writer
        .reply(
            &pending["host/list/subscribe"],
            json!({"result":{
        "data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| state.lists.threads.is_some()).await;
    // Navigation does not change the Host's catalogs, so their reads stay valid.
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    writer
        .reply(
            &pending["host/model/list"],
            json!({"result":{"data":[{
                "id":"fresh","model":{"provider": "codex", "id": "fresh"},"displayName":"Fresh model",
                "isDefault":true, "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]
            }}]}}),
        )
        .await
        .unwrap();
    writer
        .reply(
            &pending["host/account/list"],
            json!({"result":authenticated_snapshot().catalog.account.accounts.as_ref().unwrap()}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| {
        state.input.drafts[&state.panes.primary().navigation.draft_key]
            .model
            .as_ref()
            == Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "fresh".into(),
            })
            && state.catalog.account.accounts.is_some()
    })
    .await;
    assert_eq!(
        store.snapshot().catalog.models[0].display_name,
        "Fresh model"
    );
    while let Ok(Some(request)) =
        tokio::time::timeout(Duration::from_millis(100), reader.read_request())
            .await
            .unwrap_or(Ok(None))
    {
        assert!(
            !matches!(
                request["method"].as_str(),
                Some("host/model/list" | "host/account/list")
            ),
            "a catalog read during navigation must not be requested again"
        );
        let _ = writer.reply(&request, json!({"result":{}})).await;
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn connection_loads_workspace_and_lists_in_one_epoch() {
    let initial = Snapshot {
        panes: agent_core::state::PaneStates::with_primary(std::sync::Arc::new(
            agent_core::state::PaneState {
                navigation: Arc::new(agent_core::state::Navigation {
                    cwd: "/fixture".into(),
                    draft_key: "/fixture".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        ..Default::default()
    };
    let (store, mut reader, writer) = connected(initial).await;
    let epoch = store.snapshot().connection.epoch;
    let mut requests = BTreeMap::new();
    for _ in 0..4 {
        let request = read(&mut reader).await;
        assert!(
            requests
                .insert(request["method"].as_str().unwrap().to_owned(), request)
                .is_none()
        );
    }
    assert_eq!(
        requests["host/workspace/review"]["params"],
        json!({"cwd":"/fixture","includeOptions":true})
    );
    assert!(requests["host/list/subscribe"]["params"]["cached"].is_null());
    assert!(
        uuid::Uuid::parse_str(
            requests["host/list/subscribe"]["params"]["subscription"]
                .as_str()
                .unwrap()
        )
        .is_ok()
    );
    // Review completes first; the other automatic reads must remain current.
    for (method, result) in [
        (
            "host/workspace/review",
            json!({"controls":{"branches":[],"references":[],"remotes":[],"can_push":false,"scripts":[]},"branch":"main","base_ref":"origin/main","additions":2,"deletions":1,"files":[],"diff":"fixture diff"}),
        ),
        ("host/account/list", json!({"accounts":[],"selected":{}})),
        (
            "host/model/list",
            json!({"data":[{"id":"fresh","model":{"provider": "codex", "id": "fresh"},"displayName":"Fresh", "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}}],"nextCursor":null}),
        ),
        (
            "host/list/subscribe",
            json!({"data":[{"id":{"provider":"codex","id":"listed"}}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}),
        ),
    ] {
        writer
            .reply(&requests[method], json!({"result":result}))
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        snapshot.catalog.account.accounts.is_some()
            && snapshot.panes.primary().workspace.review.is_some()
            && !snapshot.catalog.models.is_empty()
            && snapshot.lists.threads.is_some()
    })
    .await;
    let snapshot = store.snapshot();
    assert_eq!(snapshot.connection.epoch, epoch);
    assert_eq!(
        snapshot
            .panes
            .primary()
            .workspace
            .review
            .as_ref()
            .unwrap()
            .diff,
        "fixture diff"
    );
    assert_eq!(
        snapshot.catalog.models[0].model,
        agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "fresh".into()
        }
    );
    assert_eq!(
        snapshot.lists.threads.as_ref().unwrap().data[0]
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
        let store = Arc::new(Store::offline(Snapshot::default(), None));
        let connecting = network.spawn({
            let store = store.clone();
            let client = client.clone();
            async move { store.reconnect(&client, &ticket, None, "Fixture device").await }
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
        let server = tokio::spawn({
            let output = output.clone();
            async move {
                while let Some(request) = reader.read_request().await.unwrap() {
                    let result = match request["method"].as_str().unwrap() {
                        "host/list/subscribe" => json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,}),
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        "host/conversation/subscribe" => {
                            let a = request["params"]["session"]["id"] == "A";
                            json!({"thread":{"id":{"provider":"codex","id":if a {"A"} else {"B"}},"turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":if a {json!({"deferred":{"summary":{"commandExecution":{"command":"pwd","cwd":null,"output":"","exitCode":null}}}})} else {json!({"inline":{"body":{"assistantText":{"text":"B prefix","phase":"unknown"}}}})}}]}]}})
                        }
                        _ => { send.send(request).await.unwrap(); continue; }
                    };
                    output.lock().await.reply(&request, json!({"result":result})).await.unwrap();
                }
            }
        });
        for id in ["A", "B"] {
            store.dispatch(Intent::ReadThread(op::ReadThread::new(SessionRef {provider:Some(ProviderKind::Codex),id:id.into()}))).await.unwrap();
        }
        let subscription_a = store.snapshot().sessions.subscriptions[&SessionRef::new(ProviderKind::Codex, "A".into()).unwrap()];
        let subscription_b = store.snapshot().sessions.subscriptions[&SessionRef::new(ProviderKind::Codex, "B".into()).unwrap()];
        let read_item = op::ReadItem {thread_id:SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() },turn_id:"turn".into(),item_id:"item".into()};
        let mut reading = Box::pin(store.dispatch(Intent::ReadItem(read_item.clone())));
        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "host/conversation/hydrate");
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
            (subscription_a, SessionChange::Request {request:serde_json::from_value(json!({"id": "approval", "target": {"turn": {"turnId": "turn", "itemId": "item"}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}})).unwrap()}),
        ];
        for (subscription, change) in changes {
            output.lock().await.notify(json!({"method":"fixture/session/change","session":SessionRef::new(ProviderKind::Codex, if subscription == subscription_a { "A" } else { "B" }.into()).unwrap(),"change":change})).await.unwrap();
        }
        wait_for(&store, |s| s.request("approval").is_some() && item_text(&s.sessions.conversations[&SessionRef { provider: Some(ProviderKind::Codex), id: "B".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0]) == Some("B prefix + delta")).await;
        for intent in [
            Intent::Respond(op::Respond {request_id:"approval".into(),answer:Answer::Approval {choice_id: "choice-2".into()}}),
            Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() },turn_id:"turn".into()}),
        ] {
            let completion = store.dispatch(intent);
            let request = requests.recv().await.unwrap();
            assert!(matches!(request["method"].as_str(), Some("host/conversation/change")));
            output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
            completion.await.unwrap();
        }
        let subscription = store.snapshot().sessions.subscriptions[&read_item.thread_id];
        let cursor = store.snapshot().sessions.replicas[&read_item.thread_id].cursor().unwrap();
        output.lock().await.notify(json!({"method":"fixture/session/change","session":read_item.thread_id,"change":SessionChange::Text {turn_id:"turn".into(),item_id:"item".into(),field:TextField::CommandOutput,delta:"live suffix".into()}})).await.unwrap();
        wait_for(&store, |s| s.sessions.replicas[&read_item.thread_id].cursor().is_some_and(|current| current.sequence > cursor.sequence)).await;
        assert_eq!(store.snapshot().sessions.subscriptions[&read_item.thread_id],subscription);
        assert!(store.snapshot().panes.primary().error.is_none());
        // New deferred metadata invalidates the in-flight source; a complete read must retry.
        output.lock().await.notify(json!({"method":"fixture/session/change","session":read_item.thread_id,"change":SessionChange::Item {turn_id:"turn".into(),item: serde_json::from_value(json!({"id":"item","status":"running","clientInputId":null,"body":{"deferred":{"summary":{"commandExecution":{"command":"pwd","cwd":null,"output":"new suffix","exitCode":null}}}}})).unwrap()}})).await.unwrap();
        wait_for(&store, |s| matches!(s.sessions.conversations[&SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::CommandExecution { output, .. } if output == "new suffix")).await;
        assert!(store.snapshot().sessions.conversations[&SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].is_deferred());
        assert!(futures_util::poll!(&mut reading).is_pending());
        assert!(futures_util::poll!(&mut duplicate).is_pending());
        transfer.write_all(&body).await.unwrap();
        transfer.shutdown().await.unwrap();
        let retry = requests.recv().await.unwrap();
        assert_eq!(retry["method"], "host/conversation/hydrate");
        output.lock().await.reply(&retry, json!({"result":{"item":{"id":"item","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"complete prefix + new suffix","exitCode":null}}}}}}})).await.unwrap();
        reading.await.unwrap();
        duplicate.await.unwrap();
        let snapshot = store.snapshot();
        let turn = &snapshot.sessions.conversations[&SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0];
        assert!(matches!(turn.items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::CommandExecution {output, ..} if output == "complete prefix + new suffix"));
        assert!(!turn.items.as_ref().unwrap()[0].is_deferred());
        assert!(requests.try_recv().is_err(), "same-item requests must be coalesced");
        for failure in ["digest", "id", "timeout"] {
            let wrong_id = failure == "id";
            let before = store.snapshot().sessions.conversations.clone();
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
                let interrupt = store.dispatch(Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() },turn_id:"turn".into()}));
                let request = requests.recv().await.unwrap();
                assert_eq!(request["method"], "host/conversation/change");
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
                let result = failed_read.await;
                if wrong_id {
                    assert!(matches!(&result, Err(PeerError::InvalidMessage(reason)) if reason.contains("item ID")), "{failure}: {result:?}");
                } else {
                    assert!(matches!(&result, Err(PeerError::Transfer(reason)) if reason.contains("digest")), "{failure}: {result:?}");
                }
            }
            assert!(Arc::ptr_eq(&before, &store.snapshot().sessions.conversations));
            let interrupt = store.dispatch(Intent::Interrupt(op::Interrupt {thread_id:SessionRef { provider: Some(ProviderKind::Codex), id: "A".into() },turn_id:"turn".into()}));
            let request = requests.recv().await.unwrap();
            assert_eq!(request["method"], "host/conversation/change");
            output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
            interrupt.await.unwrap();
            assert!(store.snapshot().connection.connected);
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
    let mut initial = Snapshot::default();
    Arc::make_mut(&mut initial.sessions.conversations)
        .insert(id.clone(), Arc::new(conversation.clone()));
    Arc::make_mut(&mut initial.input.drafts).insert(
        id.clone().into(),
        Arc::new(Draft {
            text: "unsent draft".into(),
            ..Default::default()
        }),
    );
    let (store, mut reader, writer) = setup(initial).await;
    let cached_turns = store.snapshot().sessions.conversations[&id].turns.clone();
    let old_subscription = store.snapshot().sessions.subscriptions[&id];
    let cursor = store.snapshot().sessions.replicas[&id].cursor().unwrap();
    writer
        .send_rows(
            old_subscription,
            agent_protocol::sync::Message::Changes {
                from: agent_protocol::sync::Cursor {
                    sequence: cursor.sequence + 1,
                    ..cursor
                },
                through: agent_protocol::sync::Cursor {
                    sequence: cursor.sequence + 2,
                    ..cursor
                },
                rows: vec![],
                removed: vec![],
            },
        )
        .await
        .unwrap();
    let recovery = read(&mut reader).await;
    assert_eq!(recovery["method"], "host/conversation/subscribe");
    assert_eq!(recovery["params"]["cached"], json!(cursor));
    assert!(!store.snapshot().sessions.subscriptions.contains_key(&id));
    assert!(store.snapshot().panes.primary().error.is_none());
    assert_eq!(
        store.snapshot().sessions.conversations[&id].turns,
        cached_turns
    );

    if restored {
        let mut recovered = thread("authoritative");
        recovered.cwd = None;
        writer
            .reply(&recovery, json!({"result":{"thread":recovered}}))
            .await
            .unwrap();
        wait_for(&store, |state| {
            state.sessions.subscriptions.contains_key(&id)
        })
        .await;
        assert_ne!(
            store.snapshot().sessions.subscriptions[&id],
            old_subscription
        );
        writer.notify(json!({"method":"fixture/session/change", "session":id, "change":SessionChange::Text {
            turn_id: "turn".into(), item_id: "item".into(),
            field: TextField::AssistantText, delta: " updated".into(),
        }})).await.unwrap();
        wait_for(&store, |state| matches!(state.sessions.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::AssistantText {text, ..} if text == "authoritative updated")).await;
        assert!(store.snapshot().panes.primary().error.is_none());
    } else {
        writer
            .reply(
                &recovery,
                json!({"error":{"code":"history_unavailable","message":"history read failed"}}),
            )
            .await
            .unwrap();
        let history = op::OperationKey::History {
            session: id.clone(),
        };
        wait_for(&store, |state| {
            state.operation_error(history.clone()).is_some()
        })
        .await;
        assert!(store.snapshot().panes.primary().error.is_none());
        assert!(
            store
                .snapshot()
                .operation_error(history)
                .unwrap()
                .to_string()
                .contains("history read failed")
        );
        assert!(!store.snapshot().sessions.subscriptions.contains_key(&id));
        assert_eq!(
            store.snapshot().sessions.conversations[&id].turns,
            cached_turns
        );
    }
    assert_eq!(
        store.snapshot().input.drafts[&DraftKey::from(id)].text,
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
            // The draft is recorded when the login starts.
            let mut initial = Snapshot::default();
            Arc::make_mut(&mut initial.catalog.account).login_draft = thread_id.map(DraftKey::from);
            let (store, mut reader, writer) = setup(initial).await;
            reader.script_model_catalog();
            let polling = store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin {
                provider: serde_json::from_value(json!(provider)).unwrap(),
                id: "login".into(),
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
                        json!({"accounts":[{"provider":provider,"id":id}],"selected":{(provider):id},"errors":{}})
                    }
                    "host/model/list" => {
                        json!({"data":[{"id":model_id,"model":{"provider":provider,"id":model_id},"displayName":model_id, "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}}]})
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
                        .input
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
                }) && snapshot
                    .catalog
                    .account
                    .accounts
                    .as_ref()
                    .is_some_and(|accounts| {
                        accounts
                            .accounts
                            .iter()
                            .any(|account| account.id == id && accounts.is_selected(account))
                    })
            })
            .await;
            assert!(store.snapshot().catalog.account.login.is_none());
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
            .catalog
            .composer_catalogs
            .get(&snapshot.panes.primary().navigation.cwd)
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
        thread_id: store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone(),
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
            .catalog
            .composer_catalogs
            .get(&snapshot.panes.primary().navigation.cwd)
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
    let key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
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
            .catalog
            .composer_catalogs
            .get(&snapshot.panes.primary().navigation.cwd)
            .is_some_and(|c| !c.loading)
    })
    .await;
    let suggestions = store
        .presentation()
        .composer
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
    assert!(
        store
            .snapshot()
            .catalog
            .composer_catalogs
            .get("/project")
            .unwrap()
            .loading
    );
    let suggestions = store
        .presentation()
        .composer
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
            .catalog
            .composer_catalogs
            .get(&snapshot.panes.primary().navigation.cwd)
            .is_some_and(|c| !c.loading)
    })
    .await;
    let suggestions = store
        .presentation()
        .composer
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
            thread_id: store
                .snapshot()
                .panes
                .primary()
                .navigation
                .draft_key
                .clone(),
            text: "draft".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .snapshot()
            .catalog
            .composer_catalogs
            .get("/second")
            .unwrap()
            .cwd,
        "/second"
    );
    assert!(
        store
            .snapshot()
            .catalog
            .composer_catalogs
            .get("/second")
            .unwrap()
            .loading
    );

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
            .catalog
            .composer_catalogs
            .get(&snapshot.panes.primary().navigation.cwd)
            .is_some_and(|c| !c.loading)
    })
    .await;
    assert_eq!(
        store
            .presentation()
            .composer
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
    let key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
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
        client_user_message_id: "daae3c3c-84a4-557c-85b8-ee33bcc8df85".into(),
        alternate: false,
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    assert!(store.snapshot().input.drafts[&key].invocations.is_empty());
    assert_eq!(
        store.snapshot().sessions.pending_submissions["daae3c3c-84a4-557c-85b8-ee33bcc8df85"]
            .draft
            .invocations,
        vec![invocation.clone()]
    );
    assert_eq!(
        request["params"]["initial"][1]["parts"][1],
        json!({"skill":{"name":"review","path":"/fixture/review/SKILL.md"}})
    );
    store
        .dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
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
    assert_eq!(snapshot.input.drafts[&key].invocations, vec![invocation]);
    assert!(snapshot.input.drafts[&key].text.contains("newer input"));
    assert!(snapshot.input.drafts[&key].text.contains("$review"));
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
async fn model_catalogs_publish_and_recover_independently_while_another_provider_is_pending() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    reader.script_model_catalog();
    let refreshed = store.dispatch(Intent::RefreshModels);
    let mut pending = BTreeMap::new();
    for _ in 0..3 {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/model/list");
        let provider: ProviderKind =
            serde_json::from_value(request["params"]["provider"].clone()).unwrap();
        assert!(pending.insert(provider, request).is_none());
    }
    let model = |provider: ProviderKind, id: &str, title: &str| json!({"id":id,"model":{"provider":provider,"id":"same-native-model"},"displayName":title,"isDefault":false, "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}});
    writer.reply(&pending[&ProviderKind::Codex], json!({"result":{
        "data":[model(ProviderKind::Codex,"shared-entry","Original Codex")],"nextCursor":"next"
    }})).await.unwrap();
    let next = read(&mut reader).await;
    assert_eq!(next["params"]["provider"], "codex");
    assert_eq!(next["params"]["cursor"], "next");
    writer
        .reply(
            &next,
            json!({"result":{"data":[
        model(ProviderKind::Codex,"shared-entry","Updated Codex"),
        model(ProviderKind::Codex,"alias-entry","Codex alias")],"nextCursor":null}}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| state.catalog.models.len() == 2).await;
    assert!(!store.snapshot().models_loading(Some(ProviderKind::Codex)));
    assert!(store.snapshot().models_loading(Some(ProviderKind::Claude)));
    assert!(
        store
            .snapshot()
            .models_loading(Some(ProviderKind::DeepSeek))
    );
    writer
        .reply(
            &pending[&ProviderKind::Claude],
            json!({"result":{"data":[],"providerErrors":{
                "claude":{"code":"provider_timeout","message":"Claude unavailable"}
            }}}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| {
        state.catalog.model_errors.contains_key("claude")
    })
    .await;
    assert_eq!(store.snapshot().catalog.models.len(), 2);
    let retry = store.dispatch(Intent::LoadModels(op::LoadModels {
        provider: ProviderKind::Claude,
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["provider"], "claude");
    writer.reply(&request,json!({"result":{"data":[model(ProviderKind::Claude,"shared-entry","Claude")],"nextCursor":null}})).await.unwrap();
    retry.await.unwrap();
    let state = store.snapshot();
    assert!(state.catalog.model_errors.is_empty());
    assert!(state.models_loading(Some(ProviderKind::DeepSeek)));
    assert_eq!(
        state
            .catalog
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
            (ProviderKind::Codex, "alias-entry", "Codex alias"),
            (ProviderKind::Claude, "shared-entry", "Claude"),
        ]
    );
    writer
        .reply(
            &pending[&ProviderKind::DeepSeek],
            json!({"result":{"data":[]}}),
        )
        .await
        .unwrap();
    wait_for(&store, |state| !state.models_loading(None)).await;
    refreshed.await.unwrap();
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
        panes: agent_core::state::PaneStates::with_primary(Arc::new(
            agent_core::state::PaneState {
                navigation: Arc::new(agent_core::state::Navigation {
                    thread_id: Some(id.clone()),
                    draft_key: id.clone().into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        sessions: agent_core::state::Sessions {
            conversations: Arc::new(BTreeMap::from([(id.clone(), Arc::new(initial))])),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    assert!(store.snapshot().sessions.subscriptions.contains_key(&id));
    let cached = store.snapshot().sessions.conversations.clone();
    let failed = store.dispatch(Intent::ReadOlder {
        thread_id: id.clone(),
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/extend");
    assert_eq!(request["params"]["session"], json!(id));
    let captured = request["params"]["cached"].clone();
    assert!(store.snapshot().operation_running(history.clone()));
    writer
        .reply(
            &request,
            json!({"error":{"code":"request_failed","message":"temporary history failure"}}),
        )
        .await
        .unwrap();
    assert!(failed.await.is_err());
    assert_eq!(store.snapshot().sessions.conversations, cached);
    assert!(!store.snapshot().operation_running(history.clone()));
    assert!(
        store
            .snapshot()
            .operation_error(history.clone())
            .unwrap()
            .to_string()
            .contains("temporary history failure")
    );

    let retry = store.dispatch(Intent::ReadOlder {
        thread_id: id.clone(),
    });
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["cached"], captured);
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
    let thread = &joined.sessions.conversations[&id];
    assert_eq!(thread.history_has_more, Some(true));
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
    assert_eq!(old_request["params"]["cached"], captured);
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
    let refreshed = store.snapshot().sessions.conversations.clone();
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
        store.snapshot().sessions.conversations,
        refreshed,
        "an older-page reply belongs to its original cursor and subscription"
    );
    assert_eq!(
        store.snapshot().sessions.conversations[&id].history_has_more,
        Some(true)
    );
    store.close().await.unwrap();
}
#[tokio::test]
async fn independent_view_operations_complete_without_invalidating_each_other() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let epoch = store.snapshot().connection.epoch;
    let reading = store.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/selected/file".into(),
        discard_draft: false,
    }));
    let file_request = read(&mut reader).await;
    let reviewing = store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
        base_ref: None,
        cwd: "/selected".into(),
    }));
    let review_request = read(&mut reader).await;
    assert_eq!(store.snapshot().connection.epoch, epoch);
    assert!(store.snapshot().operation_running(op::OperationKey::File {
        pane: ("").to_owned()
    }));
    assert!(
        store
            .snapshot()
            .operation_running(op::OperationKey::WorkspaceReview {
                pane: ("").to_owned()
            })
    );
    writer.reply(&review_request, json!({"result":{"controls":{"branches":[],"references":[],"remotes":[],"can_push":false,"scripts":[]},"branch":"main","base_ref":"origin/main","additions":0,"deletions":0,"files":[],"diff":"review"}})).await.unwrap();
    reviewing.await.unwrap();
    assert!(
        !store
            .snapshot()
            .operation_running(op::OperationKey::WorkspaceReview {
                pane: ("").to_owned()
            })
    );
    assert!(store.snapshot().operation_running(op::OperationKey::File {
        pane: ("").to_owned()
    }));
    writer
        .reply(
            &file_request,
            json!({"result":file("/selected/file", "r1", "selected content")}),
        )
        .await
        .unwrap();
    reading.await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
            .workspace
            .file
            .as_ref()
            .unwrap()
            .text,
        "selected content"
    );
    assert!(!store.snapshot().operation_running(op::OperationKey::File {
        pane: ("").to_owned()
    }));
    store.close().await.unwrap();
}

#[tokio::test]
async fn terminal_queue_overload_preserves_live_terminal_and_finishes_rejected_start() {
    use agent_core::state::TerminalPhase;
    use agent_protocol::operations::TerminalSize;
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let start = |handle: &str| {
        store.dispatch(Intent::StartTerminal(op::StartTerminal {
            command: None,
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
            command: None,
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

#[rstest::rstest]
#[tokio::test]
async fn local_unknown_input_restore_and_discard_follow_the_user_intent_after_host_rejection(
    #[values(false, true)] restore: bool,
) {
    let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
    let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
    let mut initial = snapshot();
    Arc::make_mut(&mut initial.input.drafts).insert(
        session.clone().into(),
        Arc::new(Draft {
            text: "newer".into(),
            ..Default::default()
        }),
    );
    Arc::make_mut(&mut initial.sessions.pending_submissions).insert(
        input.clone(),
        Arc::new(agent_core::state::PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: None,
            draft_key: session.clone().into(),
            draft: Arc::new(Draft {
                text: "uncertain".into(),
                ..Default::default()
            }),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: true,
            queued: false,
        }),
    );
    let (store, mut reader, writer) = setup(initial).await;
    let receipt = store.dispatch(if restore {
        Intent::RestoreUnknownSubmission {
            client_user_message_id: input.clone(),
        }
    } else {
        Intent::DiscardUnknownSubmission {
            client_user_message_id: input.clone(),
        }
    });
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/change");
    assert_eq!(request["params"]["session"], json!(session));
    assert_eq!(request["params"]["operation"], json!(input));
    assert_eq!(
        request["params"]["command"]["ResolveUnknown"],
        json!({"id":input,"retry":false})
    );
    writer.reply(&request, json!({"result":{"outcome":{"Rejected":"original was never admitted"},"cursor":{"store":uuid::Uuid::from_u128(1),"sequence":8}}})).await.unwrap();
    receipt.await.unwrap();
    let state = store.snapshot();
    assert!(!state.sessions.pending_submissions.contains_key(&input));
    assert_eq!(
        state.input.drafts[&session.into()].text,
        if restore { "uncertain\nnewer" } else { "newer" }
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn gallery_finishes_with_an_error_when_the_hydrated_image_was_removed() {
    use agent_protocol::{
        conversation::records,
        sync::{Key, Message, Removal},
    };
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let id = SessionRef::new(ProviderKind::Codex, "removed-gallery".into()).unwrap();
    let target = id.clone();
    let server = tokio::spawn(async move {
        let subscribe = read(&mut reader).await;
        let subscription =
            serde_json::from_value(subscribe["params"]["subscription"].clone()).unwrap();
        writer.reply(&subscribe, json!({"result":{"thread":{"id":target,"turns":[{"id":"turn","status":"completed","items":[{"id":"image","status":"completed","body":{"deferred":{"summary":{"imageGeneration":{"savedPath":"/image.png","data":null,"revisedPrompt":null}}}}}]}]}}})).await.unwrap();
        let hydrate = read(&mut reader).await;
        assert_eq!(hydrate["method"], "host/conversation/hydrate");
        let from = writer.replica(&target).cursor().unwrap();
        let through = agent_protocol::sync::Cursor {
            sequence: from.sequence + 1,
            ..from
        };
        writer
            .send_rows(
                subscription,
                Message::Changes {
                    from,
                    through,
                    rows: vec![],
                    removed: vec![Removal {
                        key: Key::Item {
                            thread: target,
                            id: "image".into(),
                        },
                        version: through.sequence,
                    }],
                },
            )
            .await
            .unwrap();
        writer.reply(&hydrate, json!({"result":records::Hydrated { cursor: through, version: None, transfer: None }})).await.unwrap();
        (reader, writer)
    });
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        store.dispatch(Intent::LoadSessionImages(op::LoadSessionImages {
            thread_id: id,
        })),
    )
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("gallery item is no longer available"),
        "{error:?}"
    );
    let _host = server.await.unwrap();
    store.close().await.unwrap();
}
