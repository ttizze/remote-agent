#[tokio::test]
async fn native_fork_uses_an_assistant_cursor_and_waits_for_an_idle_query() {
    // ClaudeAdapterV2.forkThread rejects an active query before any SDK fork.
    // The cursor is already an assistant ID; it needs no user-turn lookup.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let claude = Claude::load(
        root.join("unused"),
        root.join("adapter"),
        Some(root.join("native-home")),
    )
    .await
    .unwrap();
    let session = claude
        .create(root.to_str().unwrap(), "default")
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let record = claude.record(&session.id).await.unwrap();
    let (input, _receiver) = mpsc::channel(1);
    let (_interrupt, interrupt) = watch::channel(None);
    record.lock().await.running = Some(Running {
        turn_id: "native-user-control".into(),
        input,
        interrupt,
        effort: None,
        work: watch::channel(Work::default()).1,
    });
    for cursor in ["earlier-assistant-cursor", "current-assistant-cursor"] {
        let error = Agent::fork(&claude, &session.id, cursor, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, "fork_failed");
        assert!(
            error.message.contains("生成中") || error.message.contains("running"),
            "{cursor}: {}",
            error.message
        );
    }
    record.lock().await.running = None;
    let error = Agent::fork(&claude, &session.id, "earlier-assistant-cursor", None)
        .await
        .unwrap_err();
    assert!(
        error.message.contains("アカウント"),
        "the real SDK cursor reaches authentication without reading an obsolete user-to-assistant map: {}",
        error.message
    );
}
