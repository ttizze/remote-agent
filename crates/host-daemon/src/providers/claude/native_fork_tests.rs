#[tokio::test]
async fn native_fork_uses_an_assistant_cursor_and_waits_for_an_idle_query() {
    // ClaudeAdapterV2.forkThread rejects an active query before any SDK fork.
    // The cursor is already an assistant ID; it needs no user-turn lookup.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::create_dir_all(root.join("adapter/accounts")).unwrap();
    std::fs::write(
        root.join("adapter/accounts/accounts.json"),
        r#"{"accounts":[],"selectedId":null}"#,
    )
    .unwrap();
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

#[rstest::rstest]
#[case("retained")]
#[case("baseline")]
#[case("no-cursor-latest")]
#[case("no-cursor-with-later-turn")]
#[case("busy")]
#[tokio::test]
async fn native_claude_rollback_keeps_real_cursors_and_allocates_only_empty_baselines(
    #[case] mode: &str,
) {
    // Pinned ClaudeAdapterV2.rollbackThread and resolveRollbackResumeSessionAt:
    // real assistant -> same native resource; empty baseline -> fresh UUID;
    // missing cursor is a no-op only when there is no later native turn.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let claude = Claude::load(
        root.join("unused"),
        root.join("adapter"),
        Some(root.join("native")),
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
    let (_receiver, _watch) = if mode == "busy" {
        let (input, receiver) = mpsc::channel(1);
        let (watch, interrupt) = watch::channel(None);
        record.lock().await.running = Some(Running {
            turn_id: "raw-control".into(),
            input,
            interrupt,
            effort: None,
            work: watch::channel(Work::default()).1,
        });
        (Some(receiver), Some(watch))
    } else {
        (None, None)
    };
    let assistant = "01234567-89ab-4cde-8fab-0123456789ab";
    let retained = match mode {
        "retained" => Some(assistant),
        "no-cursor-latest" | "no-cursor-with-later-turn" => Some("turn:raw-control"),
        _ => None,
    };
    let removed =
        matches!(mode, "retained" | "no-cursor-with-later-turn").then_some("later-assistant");
    let result = Agent::rollback(
        &claude,
        &session.id,
        retained,
        removed,
        root.to_str().unwrap(),
        None,
    )
    .await;
    if matches!(mode, "busy" | "no-cursor-with-later-turn") {
        let error = result.err().expect("unsafe native rollback is refused");
        assert_eq!(error.code, "rollback_failed");
        assert!(
            error
                .message
                .contains(if mode == "busy" { "running" } else { "cursor" }),
            "{}",
            error.message
        );
        assert_eq!(claude.records.lock().await.len(), 1);
        return;
    }
    let result = result.unwrap_or_else(|error| panic!("{mode}: {error}"));
    if mode == "baseline" {
        assert_ne!(result.native_id, session.id);
        Uuid::parse_str(&result.native_id).unwrap();
        let reset = claude.record(&result.native_id).await.unwrap();
        let reset = reset.lock().await;
        assert!(
            !reset.resumable,
            "allocated UUID has never run a native query"
        );
        assert!(reset.running.is_none());
        assert!(result.head_id.is_none());
        assert!(
            Arc::ptr_eq(&claude.record(&session.id).await.unwrap(), &record),
            "previous native resource is retained for old callback ownership"
        );
    } else {
        assert_eq!(result.native_id, session.id);
        assert_eq!(
            result.head_id.as_deref(),
            (mode == "retained").then_some(assistant)
        );
        assert_eq!(claude.records.lock().await.len(), 1);
    }
}
