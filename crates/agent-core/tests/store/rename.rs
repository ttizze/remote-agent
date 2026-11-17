//! Native dispatch's public JSON contract reaches the real Store/transport
//! command boundary. The scripted Host controls acceptance and failure.
use super::*;

fn initial() -> (Snapshot, SessionRef) {
    let session = SessionRef::new(ProviderKind::Codex, "rename-original".into()).unwrap();
    let mut snapshot = Snapshot::default();
    snapshot.catalog.ui_language = agent_core::localization::UiLanguage::English;
    let navigation = Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation);
    navigation.thread_id = Some(session.clone());
    navigation.draft_key = session.clone().into();
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        session.clone(),
        Arc::new(Thread {
            id: Some(session.clone()),
            name: Some("Original name".into()),
            ..Default::default()
        }),
    );
    (snapshot, session)
}

fn rename(session: &SessionRef, name: &str) -> Intent {
    serde_json::from_value(json!({"RenameSession":{"threadId": session, "name": name}})).unwrap()
}

#[tokio::test]
async fn rename_sends_the_canonical_original_target_and_survives_navigation_until_acknowledged() {
    let (snapshot, session) = initial();
    let (store, mut reader, writer) = setup(snapshot).await;
    let result = store.dispatch(rename(&session, " \u{feff}新しい会話名 \u{3000}"));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/change");
    assert_eq!(
        request["params"]["session"],
        json!({"provider":"codex","id":"rename-original"})
    );
    assert_eq!(
        request["params"]["command"],
        json!({"Rename":"新しい会話名"})
    );
    assert_eq!(
        store.snapshot().sessions.conversations[&session]
            .name
            .as_deref(),
        Some("Original name"),
        "the editor does not invent a committed Host title"
    );
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    result.await.unwrap();
    assert!(
        store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .is_none()
    );
    assert!(
        !store
            .snapshot()
            .operation_running(op::OperationKey::Rename { session })
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn empty_and_unchanged_names_do_not_send_wire_commands() {
    let (snapshot, session) = initial();
    let (store, mut reader, _writer) = setup(snapshot).await;
    assert!(
        store
            .dispatch(rename(&session, " \u{feff}\u{3000} "))
            .await
            .is_err()
    );
    store
        .dispatch(rename(&session, " \u{feff}Original name "))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(40), reader.read_request())
            .await
            .is_err()
    );
    assert_eq!(
        store.snapshot().sessions.conversations[&session]
            .name
            .as_deref(),
        Some("Original name")
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn an_edit_from_the_previous_view_still_renames_its_original_conversation() {
    let (snapshot, session) = initial();
    let (store, mut reader, writer) = setup(snapshot).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    let new_draft = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    // A delayed edit belongs to the old conversation even after native Core
    // has moved on. Its no-op check must also read that original metadata.
    tokio::time::timeout(
        Duration::from_secs(1),
        store.dispatch(rename(&session, "Original name")),
    )
    .await
    .unwrap()
    .unwrap();
    let result = store.dispatch(rename(&session, "Changed from the old view"));
    let request = read(&mut reader).await;
    assert_eq!(request["method"], "host/conversation/change");
    assert_eq!(
        request["params"]["session"],
        json!({"provider":"codex","id":"rename-original"})
    );
    assert_eq!(
        request["params"]["command"],
        json!({"Rename":"Changed from the old view"})
    );
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    result.await.unwrap();
    assert_eq!(
        store.snapshot().panes.primary().navigation.draft_key,
        new_draft
    );
    assert!(
        store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .is_none()
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn rename_failure_stays_with_the_original_conversation_and_allows_a_new_attempt() {
    use agent_core::presentation::error::ErrorScope;
    let (snapshot, session) = initial();
    let (store, mut reader, writer) = setup(snapshot).await;
    let result = store.dispatch(rename(&session, "Rejected name"));
    let request = read(&mut reader).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    writer
        .reply(
            &request,
            json!({"error":{"code":"refused","message":"Denied by Host","delivery":"notSent"}}),
        )
        .await
        .unwrap();
    assert!(result.await.is_err());
    let snapshot = store.snapshot();
    let notices = snapshot.error_notices(
        "",
        ErrorScope::Conversation {
            draft_key: session.clone().into(),
        },
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Failed to rename thread");
    assert!(
        notices[0]
            .text
            .details
            .as_deref()
            .unwrap_or_default()
            .contains("Denied by Host")
    );
    assert!(
        snapshot
            .error_notices(
                "",
                ErrorScope::Conversation {
                    draft_key: snapshot.panes.primary().navigation.draft_key.clone()
                }
            )
            .is_empty()
    );
    assert_eq!(
        snapshot.sessions.conversations[&session].name.as_deref(),
        Some("Original name")
    );
    let retry = store.dispatch(rename(&session, "Accepted name"));
    let request = read(&mut reader).await;
    assert_eq!(
        request["params"]["command"],
        json!({"Rename":"Accepted name"})
    );
    writer.reply(&request, json!({"result":{}})).await.unwrap();
    retry.await.unwrap();
    assert!(
        store
            .snapshot()
            .error_notices(
                "",
                ErrorScope::Conversation {
                    draft_key: session.into()
                }
            )
            .is_empty()
    );
    store.close().await.unwrap();
}
