//! Admission runs at the real Store's publication/effect boundary, after any
//! caller's cached presentation. Scope identity comes from this fixture.
use super::*;
use agent_core::store::DispatchScope;

#[tokio::test(flavor = "current_thread")]
async fn navigation_does_not_turn_a_queued_download_failure_into_success() {
    let (store, _) = fixture().await;
    let download = || {
        Intent::DownloadFile(op::DownloadFile {
            source: "/uploads/snapshot.pdf".into(),
            destination: "/unused-offline-target".into(),
        })
    };
    let expected = store
        .dispatch(download())
        .await
        .expect_err("the fixture has no connected Host");
    // No await between admission and navigation: the executor has not run yet.
    let queued = store.dispatch(download());
    let navigation = store.dispatch(Intent::NewChat { cwd: String::new() });
    navigation.await.unwrap();
    let result = queued.await;
    assert!(
        result.is_err(),
        "navigation must not manufacture downloaded bytes: {result:?}"
    );
    assert_eq!(result.unwrap_err().to_string(), expected.to_string());
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_workspace_save_reaches_the_original_host_only_in_its_original_namespace() {
    let mut snapshot = Snapshot::default();
    snapshot.connection.storage_scope = "workspace-storage".into();
    let (store, mut reader, writer) = setup(snapshot).await;
    let write = || {
        Intent::WriteWorkspaceFile(op::WriteWorkspaceFile {
            cwd: "/original".into(),
            relative_path: "docs/plan.md".into(),
            text: "# Entire plan\n".into(),
        })
    };
    let rejected = store
        .dispatch_scoped(
            "",
            DispatchScope {
                storage: "retired-storage".into(),
                draft: None,
            },
            write(),
        )
        .await;
    assert!(
        matches!(rejected, Err(PeerError::InvalidMessage(message)) if message == agent_core::store::STALE_DISPATCH_SCOPE)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), reader.read_request())
            .await
            .is_err(),
        "a retired dialog must not write into another Host namespace"
    );
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    let saved = store.dispatch_scoped(
        "",
        DispatchScope {
            storage: "workspace-storage".into(),
            draft: None,
        },
        write(),
    );
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/file/writeWorkspace");
    assert_eq!(
        request["params"],
        json!({"cwd":"/original", "relativePath":"docs/plan.md", "text":"# Entire plan\n"})
    );
    writer
        .reply(&request, json!({"result":"docs/plan.md"}))
        .await
        .unwrap();
    saved.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn scoped_pane_input_uses_its_own_navigation_and_cannot_reopen_a_closed_pane() {
    let (store, primary) = fixture().await;
    let store = Arc::new(store);
    let pane = store.open_pane("/side".into()).unwrap();
    let draft = pane
        .presentation()
        .unwrap()
        .pane()
        .navigation
        .draft_key
        .clone();
    let scope = || DispatchScope {
        storage: "current-storage".into(),
        draft: Some(draft.clone()),
    };
    pane.dispatch_scoped(
        scope(),
        Intent::SetDraftText {
            thread_id: draft.clone(),
            text: "Side input".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(store.snapshot().input.drafts[&primary].text, "Before");
    assert_eq!(store.snapshot().input.drafts[&draft].text, "Side input");
    let id = pane.id().to_owned();
    pane.close();
    assert!(
        pane.dispatch_scoped(
            scope(),
            Intent::NewChat {
                cwd: "/must-not-open".into()
            }
        )
        .await
        .is_err()
    );
    assert!(store.snapshot().panes.get(&id).is_none());
    assert_eq!(store.snapshot().input.drafts[&primary].text, "Before");
    store.close().await.unwrap();
}

async fn fixture() -> (Store, DraftKey) {
    let mut snapshot = Snapshot::default();
    snapshot.connection.storage_scope = "current-storage".into();
    let store = Store::offline(snapshot, None);
    store
        .dispatch(Intent::NewChat {
            cwd: "/repo".into(),
        })
        .await
        .unwrap();
    let draft = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: draft.clone(),
            text: "Before".into(),
        })
        .await
        .unwrap();
    (store, draft)
}

#[tokio::test]
async fn stale_namespace_with_identical_draft_ids_cannot_mutate_the_store() {
    let (store, draft) = fixture().await;
    for expected_draft in [Some(draft.clone()), None] {
        let result = store
            .dispatch_scoped(
                "",
                DispatchScope {
                    storage: "retired-storage".into(),
                    draft: expected_draft,
                },
                Intent::SetDraftText {
                    thread_id: draft.clone(),
                    text: "Must not land in the replacement".into(),
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(store.snapshot().input.drafts[&draft].text, "Before");
    }
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_valid_view_still_publishes_native_input_synchronously() {
    let (store, draft) = fixture().await;
    let result = store.dispatch_scoped(
        "",
        DispatchScope {
            storage: "current-storage".into(),
            draft: Some(draft.clone()),
        },
        Intent::SetDraftText {
            thread_id: draft.clone(),
            text: "Typed".into(),
        },
    );
    assert_eq!(
        store.snapshot().input.drafts[&draft].text,
        "Typed",
        "native input observes its transition before dispatch returns"
    );
    result.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn view_events_retire_on_navigation_but_a_namespace_owned_picker_keeps_its_original_draft() {
    let (store, original) = fixture().await;
    store
        .dispatch(Intent::NewChat {
            cwd: "/other".into(),
        })
        .await
        .unwrap();
    let next = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    assert_ne!(original, next);
    let stale = store
        .dispatch_scoped(
            "",
            DispatchScope {
                storage: "current-storage".into(),
                draft: Some(original.clone()),
            },
            Intent::SetDraftText {
                thread_id: original.clone(),
                text: "Stale view".into(),
            },
        )
        .await;
    assert!(stale.is_err());
    assert_eq!(store.snapshot().input.drafts[&original].text, "Before");
    store
        .dispatch_scoped(
            "",
            DispatchScope {
                storage: "current-storage".into(),
                draft: None,
            },
            Intent::SetDraftText {
                thread_id: original.clone(),
                text: "Original picker destination".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.snapshot().input.drafts[&original].text,
        "Original picker destination"
    );
    assert!(store.snapshot().input.drafts[&next].text.is_empty());
    store.close().await.unwrap();
}
