use super::*;
use agent_core::store::SideChatStart;

#[rstest::rstest]
#[case::default_provider(None, "Needle\nsecond", "codex", "> Needle\n> second\n\n")]
#[case::unicode_selection(
    None,
    "日本語 👋\n\nlast\n",
    "codex",
    "> 日本語 👋\n> \n> last\n> \n\n"
)]
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
    let original = initial.panes.primary().navigation.draft_key.clone();
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
        async move {
            pane.start_side_chat(provider, SideChatStart::Quote(selection))
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    assert_eq!(request["params"]["provider"], expected_provider);
    assert_eq!(request["params"]["cwd"], "/fixture");
    assert!(request["params"]["initial"].is_null());
    writer.reply(&request, json!({"error":{"code":"request_failed","message":"fixture failure","delivery":"notSent"}})).await.unwrap();
    assert!(failed.await.unwrap().is_err());
    assert!(
        pane.snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .error
            .is_none()
    );
    assert!(
        pane.snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .navigation
            .thread_id
            .is_none()
    );
    assert_eq!(store.snapshot().input.drafts[&original], original_draft);

    let retry = tokio::spawn({
        let pane = pane.clone();
        let selection = selection.to_owned();
        async move {
            pane.start_side_chat(provider, SideChatStart::Quote(selection))
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    if abandon_waiter {
        retry.abort();
    }
    let concurrent = pane.start_side_chat(
        provider,
        SideChatStart::Quote("Must not replace the first quote".into()),
    );
    tokio::pin!(concurrent);
    assert!(futures_util::poll!(concurrent.as_mut()).is_pending());
    writer
        .reply(
            &request,
            json!({"result":{"thread":{
                "cwd":"/fixture","turns":[],"status":"idle"
            }}}),
        )
        .await
        .unwrap();
    if abandon_waiter {
        assert!(retry.await.unwrap_err().is_cancelled());
    } else {
        retry.await.unwrap().unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), concurrent)
        .await
        .unwrap()
        .unwrap();
    let key = pane
        .snapshot()
        .unwrap()
        .panes
        .get(pane.id())
        .unwrap()
        .navigation
        .draft_key
        .clone();
    assert!(
        pane.snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .error
            .is_none()
    );
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
    pane.start_side_chat(provider, SideChatStart::Quote(selection.into()))
        .await
        .unwrap();
    assert_eq!(
        pane.snapshot().unwrap().input.drafts[&key].text,
        "Keep the side draft"
    );
    assert_eq!(store.snapshot().input.drafts[&original], original_draft);
    store.close().await.unwrap();
}

#[rstest::rstest]
#[case::fresh_side_chat(false)]
#[case::shown_side_chat(true)]
#[tokio::test]
async fn explaining_a_selection_starts_a_conversation_that_asks_at_once(#[case] shown: bool) {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    let pane = store.open_pane("/fixture".into()).unwrap();
    let mut created = Vec::new();
    for start in [
        SideChatStart::Quote("Earlier".into()),
        SideChatStart::Explain("日本語 Needle".into()),
    ]
    .into_iter()
    .skip(usize::from(!shown))
    {
        let starting = tokio::spawn({
            let pane = pane.clone();
            async move { pane.start_side_chat(None, start).await }
        });
        let request = read_after_reviews(&mut reader, &mut writer).await;
        assert_eq!(request["method"], "host/conversation/create");
        assert!(request["params"]["initial"].is_null());
        let target: SessionRef =
            serde_json::from_value(request["params"]["session"].clone()).unwrap();
        writer
            .reply(
                &request,
                json!({"result":{"thread":{
                    "cwd":"/fixture","turns":[],"status":"idle"
                }}}),
            )
            .await
            .unwrap();
        created.push((target, starting));
    }
    let (explained, explaining) = created.pop().unwrap();
    let submit = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(submit["method"], "host/conversation/change");
    assert_eq!(submit["params"]["session"]["id"], explained.id.as_str());
    assert_eq!(
        submit["params"]["command"]["Submit"]["content"]["parts"][0]["text"]["text"],
        "次の選択範囲について詳しく説明してください。\n\n日本語 Needle"
    );
    writer.reply(&submit, json!({"result":{}})).await.unwrap();
    explaining.await.unwrap().unwrap();
    let snapshot = pane.snapshot().unwrap();
    assert_eq!(
        snapshot.panes.get(pane.id()).unwrap().navigation.thread_id,
        Some(explained)
    );
    // The side chat shown before keeps its quoted draft.
    for (earlier, starting) in created {
        starting.await.unwrap().unwrap();
        assert_eq!(snapshot.input.drafts[&earlier.into()].text, "> Earlier\n\n");
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn side_chat_without_an_agent_or_after_close_cannot_change_input() {
    let store = Arc::new(Store::offline(Snapshot::default(), None));
    let pane = store.open_pane("/fixture".into()).unwrap();
    let before = pane.snapshot().unwrap();
    let error = pane
        .start_side_chat(None, SideChatStart::Quote("Needle".into()))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "利用可能なエージェントを選択してください。"
    );
    assert_eq!(pane.snapshot().unwrap(), before);
    pane.close();
    assert!(
        pane.start_side_chat(
            Some(ProviderKind::Codex),
            SideChatStart::Quote("Needle".into())
        )
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
            pane.start_side_chat(None, SideChatStart::Quote("Abandoned selection".into()))
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    preparing.abort();
    assert!(preparing.await.unwrap_err().is_cancelled());
    drop(pane);
    assert!(weak.upgrade().is_none());
    assert!(store.snapshot().panes.get(&id).is_none());
    writer
        .reply(
            &request,
            json!({"result":{"thread":{
                "cwd":"/fixture","turns":[],"status":"idle"
            }}}),
        )
        .await
        .unwrap();
    store.close().await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
async fn closing_a_side_chat_during_creation_cannot_apply_its_quote_to_another_pane(
    #[values("", "Closed selection")] selection: &str,
) {
    let (store, mut reader, mut writer) = setup(authenticated_snapshot()).await;
    let original = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: original.clone(),
            text: "Keep the original draft".into(),
        })
        .await
        .unwrap();
    let original = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    let pane = store.open_pane("/fixture".into()).unwrap();
    let preparing = tokio::spawn({
        let pane = pane.clone();
        let selection = selection.to_owned();
        async move {
            pane.start_side_chat(None, SideChatStart::Quote(selection))
                .await
        }
    });
    let request = read_after_reviews(&mut reader, &mut writer).await;
    assert_eq!(request["method"], "host/conversation/create");
    let primary = store.snapshot().panes.primary().clone();
    pane.close();
    writer
        .reply(
            &request,
            json!({"result":{"thread":{
                "cwd":"/fixture","turns":[],"status":"idle"
            }}}),
        )
        .await
        .unwrap();
    assert!(preparing.await.unwrap().is_err());
    let snapshot = store.snapshot();
    assert_eq!(
        snapshot.input.drafts[&original].text,
        "Keep the original draft"
    );
    let closed: SessionRef = serde_json::from_value(request["params"]["session"].clone()).unwrap();
    assert!(
        snapshot
            .input
            .drafts
            .get(&closed.clone().into())
            .is_none_or(|draft| draft.text.is_empty())
    );
    assert!(pane.snapshot().is_err());
    assert!(snapshot.sessions.conversations.contains_key(&closed));
    assert!(!snapshot.sessions.subscriptions.contains_key(&closed));
    assert!(snapshot.panes.get(pane.id()).is_none());
    assert!(Arc::ptr_eq(&primary, snapshot.panes.primary()));
    store.close().await.unwrap();
}

#[tokio::test]
async fn local_validation_errors_and_dismissals_belong_to_the_originating_pane() {
    let store = Arc::new(Store::offline(Snapshot::default(), None));
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
        .panes
        .get(pane.id())
        .unwrap()
        .error
        .clone()
        .expect("validation failure");
    assert!(store.snapshot().panes.primary().error.is_none());
    store
        .dispatch(Intent::DismissError {
            id: op::ErrorId::Application {
                message: message.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(
        pane.snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .error
            .as_deref(),
        Some(message.as_str())
    );
    pane.dispatch(Intent::DismissError {
        id: op::ErrorId::Application { message },
    })
    .await
    .unwrap();
    assert!(
        pane.snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .error
            .is_none()
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn panes_keep_distinct_local_drafts_and_closing_one_keeps_the_store_available() {
    let store = Arc::new(Store::offline(Snapshot::default(), None));
    let first = store.open_pane("/same".into()).unwrap();
    let second = store.open_pane("/same".into()).unwrap();
    store
        .dispatch(Intent::NewChat {
            cwd: "/same".into(),
        })
        .await
        .unwrap();
    let first_key = first
        .snapshot()
        .unwrap()
        .panes
        .get(first.id())
        .unwrap()
        .navigation
        .draft_key
        .clone();
    let second_key = second
        .snapshot()
        .unwrap()
        .panes
        .get(second.id())
        .unwrap()
        .navigation
        .draft_key
        .clone();
    let primary_key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
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
    assert!(Arc::ptr_eq(
        held.panes.get(second.id()).unwrap(),
        second.snapshot().unwrap().panes.get(second.id()).unwrap()
    ));
    assert_eq!(store.snapshot().input.drafts[&first_key].text, "first");
    assert_eq!(store.snapshot().input.drafts[&second_key].text, "second");
    assert_eq!(
        store.snapshot().panes.primary().navigation.draft_key,
        primary_key
    );
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
    let store = Arc::new(Store::offline(Snapshot::default(), None));
    let first = store.open_pane("/same".into()).unwrap();
    let second = store.open_pane("/same".into()).unwrap();
    let first_id = first.id().to_owned();
    let keys = [
        first
            .snapshot()
            .unwrap()
            .panes
            .get(first.id())
            .unwrap()
            .navigation
            .draft_key
            .clone(),
        second
            .snapshot()
            .unwrap()
            .panes
            .get(second.id())
            .unwrap()
            .navigation
            .draft_key
            .clone(),
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
        reopened
            .snapshot()
            .unwrap()
            .panes
            .get(reopened.id())
            .unwrap()
            .navigation
            .draft_key,
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
        reopened
            .snapshot()
            .unwrap()
            .panes
            .get(reopened.id())
            .unwrap()
            .navigation
            .draft_key,
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
        None,
    ));
    let mut panes = Vec::new();
    for (key, text) in keys.iter().zip(["first unsent", "second unsent"]) {
        let pane = restored.open_pane("/same".into()).unwrap();
        assert_eq!(
            &pane
                .snapshot()
                .unwrap()
                .panes
                .get(pane.id())
                .unwrap()
                .navigation
                .draft_key,
            key
        );
        assert_eq!(pane.snapshot().unwrap().input.drafts[key].text, text);
        panes.push(pane);
    }
    restored.close().await.unwrap();
}

#[tokio::test]
async fn changing_host_storage_keeps_secondary_new_chat_drafts_separate() {
    let store = Arc::new(Store::offline(
        Snapshot {
            connection: agent_core::state::Connection {
                storage_scope: "original-host".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
    ));
    let panes = [
        store.open_pane("/same".into()).unwrap(),
        store.open_pane("/same".into()).unwrap(),
    ];
    let mut before = store.snapshot().as_ref().clone();
    for (index, pane) in panes.iter().enumerate() {
        let key = before
            .panes
            .get(pane.id())
            .unwrap()
            .navigation
            .draft_key
            .clone();
        Arc::make_mut(&mut before.input.drafts).insert(
            key,
            Arc::new(Draft {
                text: format!("original draft {index}"),
                ..Default::default()
            }),
        );
    }
    let changed = agent_core::state::reduce(
        &before,
        "",
        agent_core::state::Event::StorageScope("another-host".into()),
    )
    .0;
    assert_ne!(
        changed
            .panes
            .get(panes[0].id())
            .unwrap()
            .navigation
            .draft_key,
        changed
            .panes
            .get(panes[1].id())
            .unwrap()
            .navigation
            .draft_key
    );
    for pane in &panes {
        let view = changed.panes.get(pane.id()).unwrap();
        let expected = pane
            .snapshot()
            .unwrap()
            .panes
            .get(pane.id())
            .unwrap()
            .navigation
            .draft_key
            .clone();
        assert_ne!(
            view.navigation.draft_key, expected,
            "each Host has its own unsubmitted address"
        );
        let expected = view.navigation.draft_key.clone();
        let navigated = agent_core::state::reduce(
            &changed,
            pane.id(),
            agent_core::state::Event::Intent(Intent::NewChat {
                cwd: "/same".into(),
            }),
        )
        .0;
        assert_eq!(
            navigated.panes.get(pane.id()).unwrap().navigation.draft_key,
            expected,
            "changing Host storage must not merge secondary input with the main pane"
        );
    }
    let restored = agent_core::state::reduce(
        &changed,
        "",
        agent_core::state::Event::StorageScope("original-host".into()),
    )
    .0;
    for (index, pane) in panes.iter().enumerate() {
        let key = &before.panes.get(pane.id()).unwrap().navigation.draft_key;
        assert_eq!(
            &restored.panes.get(pane.id()).unwrap().navigation.draft_key,
            key
        );
        assert_eq!(
            restored.input.drafts[key].text,
            format!("original draft {index}")
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
            .operation_running(op::OperationKey::File {
                pane: (first.id()).to_owned()
            })
    );
    assert!(!store.snapshot().operation_running(op::OperationKey::File {
        pane: ("").to_owned()
    }));
    assert!(
        !second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File {
                pane: (second.id()).to_owned()
            })
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
            .operation_running(op::OperationKey::File {
                pane: (first.id()).to_owned()
            })
    );
    assert!(
        second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File {
                pane: (second.id()).to_owned()
            })
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
            .operation_running(op::OperationKey::File {
                pane: (first.id()).to_owned()
            })
    );
    assert!(
        !second
            .snapshot()
            .unwrap()
            .operation_running(op::OperationKey::File {
                pane: (second.id()).to_owned()
            })
    );
    let second_selection = second
        .snapshot()
        .unwrap()
        .panes
        .get(second.id())
        .unwrap()
        .workspace
        .clone();
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
            .panes
            .get(first.id())
            .unwrap()
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
            .panes
            .get(second.id())
            .unwrap()
            .workspace
            .file
            .as_ref()
            .unwrap()
            .text,
        "second"
    );
    assert!(Arc::ptr_eq(
        &second_selection,
        &second
            .snapshot()
            .unwrap()
            .panes
            .get(second.id())
            .unwrap()
            .workspace
    ));
    assert!(store.snapshot().panes.primary().workspace.file.is_none());
    store.close().await.unwrap();
}

async fn answer_open(
    reader: &mut host_fixture::Reader,
    writer: &host_fixture::Writer,
    source: &Thread,
) {
    let request = read(reader).await;
    assert_eq!(request["method"], "host/conversation/subscribe");
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
    let selected = store.snapshot().panes.primary().clone();
    writer
        .reply(
            &request,
            json!({"result":{"createOnNewSession":false,"copyOnCreate":false,
        "copyPaths":[],"worktreeDirectory":"/shared"}}),
        )
        .await
        .unwrap();
    loading.await.unwrap();
    assert!(Arc::ptr_eq(&selected, store.snapshot().panes.primary()));
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
    use agent_core::state::{Navigation, PaneState};
    let offline = Arc::new(Store::offline(Snapshot::default(), None));
    let side = offline.open_pane("/fixture".into()).unwrap();
    let side_id = side.id().to_owned();
    let mut initial = offline.snapshot().as_ref().clone();
    let mut sources = BTreeMap::new();
    for (name, pane_id) in [("primary", ""), ("side", side_id.as_str())] {
        let id = SessionRef {
            provider: Some(ProviderKind::Codex),
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
        *initial.panes.get_mut(pane_id).unwrap() = pane;
        sources.insert(id, Arc::new(source));
    }
    offline.close().await.unwrap();
    initial.sessions.conversations = Arc::new(sources.clone());
    let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
    let store = Store::new(peer, initial, None);
    let mut opened = std::collections::BTreeSet::new();
    for _ in 0..7 {
        let request = read(&mut reader).await;
        let result = match request["method"].as_str().unwrap() {
            "host/list/subscribe" => {
                json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5})
            }
            "host/model/list" => json!({"data":[]}),
            "host/account/list" => json!({"accounts":[],"selected":{}}),
            "host/workspace/review" => review(),
            "host/conversation/subscribe" => {
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
            && snapshot.panes.primary().workspace.review.is_some()
            && snapshot
                .panes
                .get(&side_id)
                .unwrap()
                .workspace
                .review
                .is_some()
    })
    .await;
    assert_eq!(opened, sources.keys().cloned().collect());
    assert_eq!(
        store
            .snapshot()
            .panes
            .primary()
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
            .panes
            .get(&side_id)
            .unwrap()
            .navigation
            .thread_id
            .as_ref()
            .unwrap()
            .id,
        "side"
    );
    store.close().await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
async fn closed_file_reads_resolve_without_changing_surviving_panes(
    #[values(false, true)] failed: bool,
) {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let pane = store.open_pane("/closed".into()).unwrap();
    let survivor = store.open_pane("/survivor".into()).unwrap();
    let reading = pane.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/closed/file".into(),
        discard_draft: false,
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/file/read");
    pane.close();
    let before = store.snapshot();
    let reply = if failed {
        json!({"error":{"code":"request_failed","message":"read failed","delivery":"notSent"}})
    } else {
        json!({"result":file("/closed/file", "read", "closed file")})
    };
    writer.reply(&request, reply).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), reading)
        .await
        .expect("closing a pane must still resolve its operation");
    if failed {
        assert!(result.is_err());
    } else {
        let agent_core::store::Outcome::FileRead { file } = result.unwrap() else {
            panic!("the caller must receive the actual file result")
        };
        assert_eq!(file.text, "closed file");
    }
    let after = store.snapshot();
    assert!(after.panes.get(pane.id()).is_none());
    for id in ["", survivor.id()] {
        assert!(Arc::ptr_eq(
            before.panes.get(id).unwrap(),
            after.panes.get(id).unwrap()
        ));
        assert!(!after.operation_running(op::OperationKey::File {
            pane: (id).to_owned()
        }));
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn closing_the_saving_pane_keeps_newer_shared_edits_and_the_saved_revision() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let pane = store.open_pane("/closed".into()).unwrap();
    let survivor = store.open_pane("/survivor".into()).unwrap();
    let loading = pane.dispatch(Intent::ReadFile(op::ReadFile {
        path: "/closed/file".into(),
        discard_draft: false,
    }));
    let request = read(&mut reader).await;
    writer
        .reply(
            &request,
            json!({"result":file("/closed/file", "base", "old")}),
        )
        .await
        .unwrap();
    loading.await.unwrap();
    pane.dispatch(Intent::SetFileDraft {
        path: "/closed/file".into(),
        text: "submitted".into(),
    })
    .await
    .unwrap();
    let saving = pane.dispatch(Intent::SaveFile(op::SaveFile {
        path: "/closed/file".into(),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/file/write");
    assert_eq!(request["params"]["text"], "submitted");
    pane.close();
    survivor
        .dispatch(Intent::SetFileDraft {
            path: "/closed/file".into(),
            text: "newer shared edit".into(),
        })
        .await
        .unwrap();
    let before = store.snapshot();
    writer
        .reply(
            &request,
            json!({"result":file("/closed/file", "saved", "submitted")}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), saving)
        .await
        .unwrap()
        .unwrap();
    let after = store.snapshot();
    let draft = &after.files.file_drafts["/closed/file"];
    assert_eq!(draft.text, "newer shared edit");
    assert_eq!(draft.revision, "saved");
    assert!(after.panes.get(pane.id()).is_none());
    assert!(!after.operation_running(op::OperationKey::SaveFile {
        path: "/closed/file".into()
    }));
    for id in ["", survivor.id()] {
        assert!(Arc::ptr_eq(
            before.panes.get(id).unwrap(),
            after.panes.get(id).unwrap()
        ));
    }
    store.close().await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
async fn closed_dictation_cannot_append_to_a_reopened_panes_input(
    #[values(false, true)] send: bool,
) {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let pane = store.open_pane("/same".into()).unwrap();
    let key = pane
        .presentation()
        .unwrap()
        .pane()
        .navigation
        .draft_key
        .clone();
    let transcribing = pane.dispatch(Intent::Transcribe(op::Dictate {
        draft_key: key.clone(),
        preparation: None,
        audio: vec![1, 0],
        send,
        client_user_message_id: "2de8b590-9866-5461-8bc9-faa433565f09".into(),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/dictation/transcribe");
    pane.close();
    let replacement = store.open_pane("/same".into()).unwrap();
    assert_eq!(
        replacement
            .presentation()
            .unwrap()
            .pane()
            .navigation
            .draft_key,
        key
    );
    replacement
        .dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: "new pane input".into(),
        })
        .await
        .unwrap();
    let before = store.snapshot();
    writer
        .reply(&request, json!({"result":{"text":"old recording"}}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), transcribing)
        .await
        .unwrap()
        .unwrap();
    let after = store.snapshot();
    assert_eq!(after.input.drafts[&key].text, "new pane input");
    assert!(Arc::ptr_eq(&before.input.drafts, &after.input.drafts));
    assert!(Arc::ptr_eq(
        before.panes.get(replacement.id()).unwrap(),
        after.panes.get(replacement.id()).unwrap()
    ));
    assert!(!after.operation_running(op::OperationKey::Dictation { draft_key: key }));
    assert!(after.sessions.pending_submissions.is_empty());
    store.close().await.unwrap();
}

// Each pair of conditions occurs once; the result belongs to the provider in every case.
#[rstest::rstest]
#[case::read_after_navigation(false, false, false)]
#[case::update_after_closure(true, true, false)]
#[case::failed_read_after_closure(false, true, true)]
#[case::failed_update_after_navigation(true, false, true)]
#[tokio::test]
async fn permission_results_follow_the_provider_after_navigation_or_pane_closure(
    #[case] update: bool,
    #[case] retired: bool,
    #[case] failed: bool,
) {
    use agent_protocol::permissions::{PermissionMode, PermissionSettings};
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let pane = store.open_pane("/permissions".into()).unwrap();
    let intent = if update {
        Intent::UpdatePermissionSettings(op::UpdatePermissionSettings {
            provider: ProviderKind::Codex,
            mode: PermissionMode::Auto,
            version: "base".into(),
        })
    } else {
        Intent::ReadPermissionSettings(op::ReadPermissionSettings {
            provider: ProviderKind::Codex,
        })
    };
    let loading = pane.dispatch(intent);
    let request = read(&mut reader).await;
    assert_eq!(request["params"]["provider"], "codex");
    if retired {
        pane.close();
    } else {
        pane.dispatch(Intent::ShowThreadList).await.unwrap();
    }
    let before = store.snapshot();
    let expected = PermissionSettings {
        mode: Some(PermissionMode::Auto),
        version: "applied".into(),
    };
    let reply = if failed {
        json!({"error":{"code":"request_failed","message":"permissions failed","delivery":"notSent"}})
    } else {
        json!({"result":expected})
    };
    writer.reply(&request, reply).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), loading)
        .await
        .unwrap()
        .unwrap();
    let after = store.snapshot();
    let result = after.catalog.permission_settings[&ProviderKind::Codex]
        .result
        .as_ref()
        .expect("the provider response must leave the loading state");
    if failed {
        assert!(result.is_err());
    } else {
        assert_eq!(result.as_ref().unwrap(), &expected);
    }
    assert!(Arc::ptr_eq(before.panes.primary(), after.panes.primary()));
    if !retired {
        assert!(Arc::ptr_eq(
            before.panes.get(pane.id()).unwrap(),
            after.panes.get(pane.id()).unwrap()
        ));
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_login_started_in_a_closed_pane_still_selects_its_account_for_that_draft() {
    let (store, mut reader, writer) = setup(Snapshot::default()).await;
    let pane = store.open_pane("/fixture".into()).unwrap();
    let draft = pane
        .presentation()
        .unwrap()
        .pane()
        .navigation
        .draft_key
        .clone();
    let starting = pane.dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
        provider: ProviderKind::Codex,
        thread_id: Some(draft.clone()),
    }));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/account/login/start");
    writer.reply(&request, json!({"result":{"provider":"codex","loginId":"login","userCode":"fixture-only","requiresCodeSubmission":false,"verificationUrl":"https://example.invalid"}})).await.unwrap();
    starting.await.unwrap();
    // The login belongs to the Host, so closing its pane does not end it.
    pane.close();
    let status = tokio::time::timeout(Duration::from_secs(5), reader.read_request())
        .await
        .expect("the Store keeps confirming the login")
        .unwrap()
        .unwrap();
    assert_eq!(status["method"], "host/account/login/status");
    writer
        .reply(
            &status,
            json!({"result":{"completed":true,"accountId":"added"}}),
        )
        .await
        .unwrap();
    let select = read(&mut reader).await;
    assert_eq!(select["method"], "host/account/select");
    assert_eq!(
        select["params"],
        json!({"provider":"codex","accountId":"added"})
    );
    writer
        .reply(
            &select,
            json!({"result":{"provider":"codex","selectedId":"added"}}),
        )
        .await
        .unwrap();
    let models = read(&mut reader).await;
    assert_eq!(models["method"], "host/model/list");
    writer
        .reply(&models, json!({"result":{"data":[{"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT","settings":{"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}}]}}))
        .await
        .unwrap();
    // The draft recorded when the login started adopts the new account's models.
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut snapshots = store.subscribe();
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            if snapshot
                .input
                .drafts
                .get(&draft)
                .and_then(|draft| draft.model.as_ref())
                == Some(&agent_protocol::models::ModelRef {
                    provider: ProviderKind::Codex,
                    id: "gpt".into(),
                })
            {
                assert!(snapshot.catalog.account.login.is_none());
                return;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .expect("the draft adopts the new account");
    store.close().await.unwrap();
}
