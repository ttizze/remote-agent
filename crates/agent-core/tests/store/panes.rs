use super::*;

#[rstest::rstest]
#[case::default_provider(None, "Needle\nsecond", "codex", "> Needle\n> second\n\n")]
#[case::original_provider(Some(ProviderKind::Claude), "Needle", "claude", "> Needle\n\n")]
#[case::empty_side_chat(None, "", "codex", "")]
#[tokio::test]
async fn side_chat_retries_creation_and_initializes_its_quote_once(
    #[case] provider: Option<ProviderKind>,
    #[case] selection: &str,
    #[case] expected_provider: &str,
    #[case] expected_draft: &str,
    #[values(false, true)] abandon_waiter: bool,
) {
    let mut initial = authenticated_snapshot();
    let original = initial.pane.navigation.draft_key.clone();
    Arc::make_mut(&mut initial.input.drafts).insert(
        original.clone(),
        Arc::new(Draft {
            text: "Keep the original draft".into(),
            ..Default::default()
        }),
    );
    let (store, mut reader, mut writer) = setup(initial).await;
    let original_draft = store.snapshot().input.drafts[&original].clone();
    let pane = store.open_pane("/fixture".into()).unwrap();
    let failed = tokio::spawn({
        let pane = pane.clone();
        let selection = selection.to_owned();
        async move { pane.prepare_side_chat(provider, selection).await }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/create");
    assert_eq!(request["params"]["provider"], expected_provider);
    assert_eq!(request["params"]["cwd"], "/fixture");
    assert!(request["params"]["initialInput"].is_null());
    writer.reply(&request, json!({"error":{"code":"request_failed","message":"fixture failure","delivery":"notSent"}})).await.unwrap();
    assert!(failed.await.unwrap().is_err());
    assert!(pane.snapshot().unwrap().pane.error.is_none());
    assert!(pane.snapshot().unwrap().pane.navigation.thread_id.is_none());
    assert_eq!(store.snapshot().input.drafts[&original], original_draft);

    let retry = tokio::spawn({
        let pane = pane.clone();
        let selection = selection.to_owned();
        async move { pane.prepare_side_chat(provider, selection).await }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/create");
    if abandon_waiter {
        retry.abort();
    }
    let concurrent = pane.prepare_side_chat(provider, "Must not replace the first quote".into());
    tokio::pin!(concurrent);
    assert!(futures_util::poll!(concurrent.as_mut()).is_pending());
    writer.reply(&request, json!({"result":{"thread":{
        "id":{"provider":expected_provider,"id":"side"},"cwd":"/fixture","turns":[],"status":"idle"
    }}})).await.unwrap();
    if abandon_waiter {
        assert!(retry.await.unwrap_err().is_cancelled());
    } else {
        retry.await.unwrap().unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), concurrent)
        .await
        .unwrap()
        .unwrap();
    let key = pane.snapshot().unwrap().pane.navigation.draft_key.clone();
    assert!(pane.snapshot().unwrap().pane.error.is_none());
    assert_eq!(
        pane.snapshot().unwrap().input.drafts[&key].text,
        expected_draft
    );

    pane.dispatch(Intent::SetDraftText {
        thread_id: key.clone(),
        text: "Keep the side draft".into(),
    })
    .await
    .unwrap();
    pane.prepare_side_chat(provider, selection.into())
        .await
        .unwrap();
    assert_eq!(
        pane.snapshot().unwrap().input.drafts[&key].text,
        "Keep the side draft"
    );
    assert_eq!(store.snapshot().input.drafts[&original], original_draft);
    store.close().await.unwrap();
}

#[tokio::test]
async fn side_chat_without_an_agent_or_after_close_cannot_change_input() {
    let store = Arc::new(Store::offline(Snapshot::default()));
    let pane = store.open_pane("/fixture".into()).unwrap();
    let before = pane.snapshot().unwrap();
    let error = pane
        .prepare_side_chat(None, "Needle".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "利用可能なエージェントを選択してください。"
    );
    assert_eq!(pane.snapshot().unwrap(), before);
    pane.close();
    assert!(
        pane.prepare_side_chat(Some(ProviderKind::Codex), "Needle".into())
            .await
            .is_err()
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn abandoning_side_chat_creation_does_not_keep_its_pane_alive() {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    let pane = store.open_pane("/fixture".into()).unwrap();
    let weak = Arc::downgrade(&pane);
    let id = pane.id().to_owned();
    let preparing = tokio::spawn({
        let pane = pane.clone();
        async move {
            pane.prepare_side_chat(None, "Abandoned selection".into())
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/create");
    preparing.abort();
    assert!(preparing.await.unwrap_err().is_cancelled());
    drop(pane);
    assert!(weak.upgrade().is_none());
    assert!(store.snapshot().for_pane(&id).is_none());
    writer.reply(&request, json!({"result":{"thread":{
        "id":{"provider":"codex","id":"abandoned-side"},"cwd":"/fixture","turns":[],"status":"idle"
    }}})).await.unwrap();
    store.close().await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
async fn closing_a_side_chat_during_creation_cannot_apply_its_quote_to_another_pane(
    #[values("", "Closed selection")] selection: &str,
) {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    let original = store.snapshot().pane.navigation.draft_key.clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: original.clone(),
            text: "Keep the original draft".into(),
        })
        .await
        .unwrap();
    let pane = store.open_pane("/fixture".into()).unwrap();
    let preparing = tokio::spawn({
        let pane = pane.clone();
        let selection = selection.to_owned();
        async move { pane.prepare_side_chat(None, selection).await }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/session/create");
    pane.close();
    writer.reply(&request, json!({"result":{"thread":{
        "id":{"provider":"codex","id":"closed-side"},"cwd":"/fixture","turns":[],"status":"idle"
    }}})).await.unwrap();
    assert!(preparing.await.unwrap().is_err());
    let snapshot = store.snapshot();
    assert_eq!(
        snapshot.input.drafts[&original].text,
        "Keep the original draft"
    );
    let closed = SessionRef::new(ProviderKind::Codex, "closed-side".into()).unwrap();
    assert!(
        snapshot
            .input
            .drafts
            .get(&closed.into())
            .is_none_or(|draft| draft.text.is_empty())
    );
    assert!(pane.snapshot().is_err());
    store.close().await.unwrap();
}

#[tokio::test]
async fn closing_the_primary_view_does_not_close_the_host_or_accept_more_view_actions() {
    let store = Arc::new(Store::offline(Snapshot::default()));
    let primary = store.primary_pane();
    let side = store.open_pane("/side".into()).unwrap();
    primary.close();
    assert!(primary.dispatch(Intent::ShowThreadList).await.is_err());
    side.dispatch(Intent::NewChat {
        cwd: "/still-open".into(),
    })
    .await
    .unwrap();
    assert_eq!(side.snapshot().unwrap().pane.navigation.cwd, "/still-open");
    let replacement = store.primary_pane();
    replacement
        .dispatch(Intent::NewChat {
            cwd: "/primary".into(),
        })
        .await
        .unwrap();
    assert_eq!(store.snapshot().pane.navigation.cwd, "/primary");
    store.close().await.unwrap();
}

#[tokio::test]
async fn local_validation_errors_and_dismissals_belong_to_the_originating_pane() {
    let store = Arc::new(Store::offline(Snapshot::default()));
    let pane = store.open_pane("/side".into()).unwrap();
    pane.dispatch(Intent::EditProject {
        project_id: "project".into(),
        name: String::new(),
        section: String::new(),
    })
    .await
    .unwrap();
    let message = pane
        .snapshot()
        .unwrap()
        .pane
        .error
        .clone()
        .expect("validation failure");
    assert!(store.snapshot().pane.error.is_none());
    store
        .dispatch(Intent::DismissError {
            id: op::ErrorId::Application {
                message: message.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(
        pane.snapshot().unwrap().pane.error.as_deref(),
        Some(message.as_str())
    );
    pane.dispatch(Intent::DismissError {
        id: op::ErrorId::Application { message },
    })
    .await
    .unwrap();
    assert!(pane.snapshot().unwrap().pane.error.is_none());
    store.close().await.unwrap();
}

#[tokio::test]
async fn panes_keep_distinct_local_drafts_and_closing_one_keeps_the_store_available() {
    let store = Arc::new(Store::offline(Snapshot::default()));
    let first = store.open_pane("/same".into()).unwrap();
    let second = store.open_pane("/same".into()).unwrap();
    store
        .dispatch(Intent::NewChat {
            cwd: "/same".into(),
        })
        .await
        .unwrap();
    let first_key = first.snapshot().unwrap().pane.navigation.draft_key.clone();
    let second_key = second.snapshot().unwrap().pane.navigation.draft_key.clone();
    let primary_key = store.snapshot().pane.navigation.draft_key.clone();
    assert_ne!(first_key, second_key);
    assert_ne!(first_key, primary_key);
    assert_ne!(second_key, primary_key);
    first
        .dispatch(Intent::SetDraftText {
            thread_id: first_key.clone(),
            text: "first".into(),
        })
        .await
        .unwrap();
    let held = second.snapshot().unwrap();
    first.close();
    assert!(first.snapshot().is_err());
    second
        .dispatch(Intent::SetDraftText {
            thread_id: second_key.clone(),
            text: "second".into(),
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&held.pane, &second.snapshot().unwrap().pane));
    assert_eq!(store.snapshot().input.drafts[&first_key].text, "first");
    assert_eq!(store.snapshot().input.drafts[&second_key].text, "second");
    assert_eq!(store.snapshot().pane.navigation.draft_key, primary_key);
    let restored = agent_core::persistence::decode(
        &agent_core::persistence::encode(&store.snapshot()).unwrap(),
    )
    .unwrap();
    assert_eq!(restored.input.drafts[&first_key].text, "first");
    store.close().await.unwrap();
    assert!(second.snapshot().is_err());
    assert!(store.open_pane("/closed".into()).is_err());
}

#[tokio::test]
async fn reopening_and_restoring_panes_recovers_each_unsent_draft_without_reusing_operation_identity()
 {
    let store = Arc::new(Store::offline(Snapshot::default()));
    let first = store.open_pane("/same".into()).unwrap();
    let second = store.open_pane("/same".into()).unwrap();
    let first_id = first.id().to_owned();
    let keys = [
        first.snapshot().unwrap().pane.navigation.draft_key.clone(),
        second.snapshot().unwrap().pane.navigation.draft_key.clone(),
    ];
    for (pane, key, text) in [
        (&first, &keys[0], "first unsent"),
        (&second, &keys[1], "second unsent"),
    ] {
        pane.dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: text.into(),
        })
        .await
        .unwrap();
    }
    first.close();
    let reopened = store.open_pane("/same".into()).unwrap();
    assert_ne!(reopened.id(), first_id);
    assert_eq!(
        reopened.snapshot().unwrap().pane.navigation.draft_key,
        keys[0]
    );
    assert_eq!(
        reopened.snapshot().unwrap().input.drafts[&keys[0]].text,
        "first unsent"
    );
    reopened
        .dispatch(Intent::NewChat {
            cwd: "/elsewhere".into(),
        })
        .await
        .unwrap();
    reopened
        .dispatch(Intent::NewChat {
            cwd: "/same".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().pane.navigation.draft_key,
        keys[0]
    );
    assert_eq!(
        second.snapshot().unwrap().input.drafts[&keys[1]].text,
        "second unsent"
    );
    let saved = agent_core::persistence::encode(&store.snapshot()).unwrap();
    store.close().await.unwrap();
    let restored = Arc::new(Store::offline(
        agent_core::persistence::decode(&saved).unwrap(),
    ));
    let mut panes = Vec::new();
    for (key, text) in keys.iter().zip(["first unsent", "second unsent"]) {
        let pane = restored.open_pane("/same".into()).unwrap();
        assert_eq!(&pane.snapshot().unwrap().pane.navigation.draft_key, key);
        assert_eq!(pane.snapshot().unwrap().input.drafts[key].text, text);
        panes.push(pane);
    }
    restored.close().await.unwrap();
}

#[tokio::test]
async fn changing_host_storage_keeps_secondary_new_chat_drafts_separate() {
    let store = Arc::new(Store::offline(Snapshot {
        connection: agent_core::state::Connection {
            storage_scope: "original-host".into(),
            ..Default::default()
        },
        ..Default::default()
    }));
    let panes = [
        store.open_pane("/same".into()).unwrap(),
        store.open_pane("/same".into()).unwrap(),
    ];
    let before = store.snapshot();
    let changed = agent_core::state::reduce(
        &before,
        agent_core::state::Event::StorageScope("another-host".into()),
    )
    .0;
    for pane in &panes {
        let view = changed.for_pane(pane.id()).unwrap();
        let expected = pane.snapshot().unwrap().pane.navigation.draft_key.clone();
        assert_eq!(view.pane.navigation.draft_key, expected);
        let navigated = agent_core::state::reduce(
            &view,
            agent_core::state::Event::Intent(Intent::NewChat {
                cwd: "/same".into(),
            }),
        )
        .0;
        assert_eq!(
            navigated.pane.navigation.draft_key, expected,
            "changing Host storage must not merge secondary input with the main pane"
        );
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn file_replies_belong_to_their_panes_across_navigation_in_another_pane() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let first = store.open_pane("/one".into()).unwrap();
    let second = store.open_pane("/two".into()).unwrap();
    let one = first.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/one/file".into(),
        discard_draft: false,
    }));
    assert!(
        first
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    assert!(!store.snapshot().operation_running(op::OperationKey::File));
    assert!(
        !second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    let request_one = read(&mut reader).await;
    let two = second.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/two/file".into(),
        discard_draft: false,
    }));
    assert!(
        first
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    assert!(
        second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    let request_two = read(&mut reader).await;
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    writer
        .reply(
            &request_two,
            json!({"result":file("/two/file", "two", "second")}),
        )
        .await
        .unwrap();
    two.await.unwrap();
    assert!(
        first
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    assert!(
        !second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File)
    );
    let second_selection = second.snapshot().unwrap().pane.workspace.clone();
    writer
        .reply(
            &request_one,
            json!({"result":file("/one/file", "one", "first")}),
        )
        .await
        .unwrap();
    one.await.unwrap();
    assert_eq!(
        first
            .snapshot()
            .unwrap()
            .pane
            .workspace
            .file
            .as_ref()
            .unwrap()
            .text,
        "first"
    );
    assert_eq!(
        second
            .snapshot()
            .unwrap()
            .pane
            .workspace
            .file
            .as_ref()
            .unwrap()
            .text,
        "second"
    );
    assert!(Arc::ptr_eq(
        &second_selection,
        &second.snapshot().unwrap().pane.workspace
    ));
    assert!(store.snapshot().pane.workspace.file.is_none());
    store.close().await.unwrap();
}

async fn answer_open(
    reader: &mut host_fixture::Reader,
    writer: &host_fixture::Writer,
    source: &Thread,
) {
    let request = read(reader).await;
    assert_eq!(request["method"], "host/session/open");
    writer
        .reply(&request, json!({"result":{"thread":source}}))
        .await
        .unwrap();
    let request = read(reader).await;
    assert_eq!(request["method"], "host/workspace/review");
    writer
        .reply(&request, json!({"result":review()}))
        .await
        .unwrap();
}

#[tokio::test]
async fn another_pane_keeps_a_shared_conversation_subscribed_after_primary_navigation() {
    let mut source = thread("shared");
    Arc::make_mut(&mut source.turns.as_mut().unwrap()[0]).status =
        agent_protocol::execution::TurnStatus::Completed;
    let id = source.id.clone().unwrap();
    let initial = Snapshot {
        sessions: agent_core::state::Sessions {
            conversations: Arc::new([(id.clone(), Arc::new(source.clone()))].into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let (store, mut reader, writer) = setup(initial).await;
    let pane = store.open_pane("/fixture".into()).unwrap();
    let opening = pane.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())));
    answer_open(&mut reader, &writer, &source).await;
    opening.await.unwrap();
    let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())));
    answer_open(&mut reader, &writer, &source).await;
    opening.await.unwrap();
    assert_eq!(store.snapshot().sessions.subscriptions.len(), 1);
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    assert!(store.snapshot().sessions.subscriptions.contains_key(&id));
    for status in [
        agent_protocol::models::SessionStatus::Running,
        agent_protocol::models::SessionStatus::Idle,
    ] {
        writer
            .notify(json!({"method":"fixture/session/change", "session":id,
            "change":agent_protocol::session::SessionChange::Status { status }}))
            .await
            .unwrap();
        wait_for(&store, |snapshot| {
            snapshot.sessions.conversations[&id].status == status
        })
        .await;
        assert_eq!(
            pane.snapshot().unwrap().sessions.conversations[&id].status,
            status
        );
    }
    pane.close();
    assert!(!store.snapshot().sessions.subscriptions.contains_key(&id));
    store.close().await.unwrap();
}

#[tokio::test]
async fn host_settings_follow_the_resource_after_navigation() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let loading = store.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
    let pane = store.open_pane("/side".into()).unwrap();
    assert!(
        pane.snapshot()
            .unwrap()
            .operation_running(op::OperationKey::WorktreeSettings)
    );
    let request = read(&mut reader).await;
    store.dispatch(Intent::ShowThreadList).await.unwrap();
    let selected = store.snapshot().pane.clone();
    writer
        .reply(
            &request,
            json!({"result":{"createOnNewSession":false,"copyOnCreate":false,
        "copyPaths":[],"worktreeDirectory":"/shared"}}),
        )
        .await
        .unwrap();
    loading.await.unwrap();
    assert!(Arc::ptr_eq(&selected, &store.snapshot().pane));
    assert_eq!(
        store
            .snapshot()
            .files
            .workspace
            .settings
            .as_ref()
            .unwrap()
            .worktree_directory,
        "/shared"
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn connection_reopens_every_pane_in_one_shared_subscription_map() {
    use agent_core::state::{Navigation, PaneState, PaneStates};
    let mut initial = Snapshot::default();
    let mut sources = BTreeMap::new();
    for name in ["primary", "side"] {
        let id = SessionRef {
            provider: ProviderKind::Codex,
            id: name.into(),
        };
        let mut source = thread(name);
        source.id = Some(id.clone());
        Arc::make_mut(&mut source.turns.as_mut().unwrap()[0]).status =
            agent_protocol::execution::TurnStatus::Completed;
        let pane = Arc::new(PaneState {
            draft_slot: u64::from(name != "primary"),
            navigation: Arc::new(Navigation {
                thread_id: Some(id.clone()),
                cwd: "/fixture".into(),
                draft_key: id.clone().into(),
            }),
            ..Default::default()
        });
        if name == "primary" {
            initial.pane = pane;
        } else {
            initial.panes = PaneStates(Arc::new([("side".into(), pane)].into()));
        }
        sources.insert(id, Arc::new(source));
    }
    initial.sessions.conversations = Arc::new(sources.clone());
    let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
    let store = Store::new(peer, initial);
    let mut opened = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/session/list" => {
                json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5})
            }
            "host/model/list" => json!({"data":[]}),
            "host/account/list" => json!({"accounts":[],"selected":{}}),
            "host/taskActivity/read" => {
                json!({"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
            }
            "host/workspace/review" => review(),
            "host/session/open" => {
                let id: SessionRef =
                    serde_json::from_value(request["params"]["session"].clone()).unwrap();
                opened.insert(id.clone());
                json!({"thread":sources[&id]})
            }
            method => panic!("unexpected reconnect request {method}"),
        };
        writer
            .reply(&request, json!({"result":result}))
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        snapshot.sessions.subscriptions.len() == 2
            && snapshot.pane.workspace.review.is_some()
            && snapshot
                .for_pane("side")
                .unwrap()
                .pane
                .workspace
                .review
                .is_some()
    })
    .await;
    assert_eq!(opened, sources.keys().cloned().collect());
    assert_eq!(
        store
            .snapshot()
            .pane
            .navigation
            .thread_id
            .as_ref()
            .unwrap()
            .id,
        "primary"
    );
    assert_eq!(
        store
            .snapshot()
            .for_pane("side")
            .unwrap()
            .pane
            .navigation
            .thread_id
            .as_ref()
            .unwrap()
            .id,
        "side"
    );
    store.close().await.unwrap();
}
