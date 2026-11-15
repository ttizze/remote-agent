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

#[rstest::rstest]
#[case("missing")]
#[case("empty")]
#[case("conversation")]
#[case("copies")]
#[case("fresh")]
#[tokio::test]
async fn native_claude_resume_probe_classifies_transcripts_without_returning_history(
    #[case] mode: &str,
) {
    // Pinned resumeThread: missing is left to the CLI, metadata-only is refused,
    // and any conversation copy wins. This is a native start probe, not a
    // second display/history reader after canonical import.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let home = root.join("native");
    let claude = Claude::load(
        root.join("unused"),
        root.join("adapter"),
        Some(home.clone()),
    )
    .await
    .unwrap();
    let id = if mode == "fresh" {
        claude
            .create(root.to_str().unwrap(), "default")
            .await
            .unwrap()
            .thread
            .id
            .unwrap()
            .id
    } else {
        Uuid::new_v4().to_string()
    };
    if matches!(mode, "empty" | "conversation" | "copies") {
        let project = home.join("projects/first");
        std::fs::create_dir_all(&project).unwrap();
        let metadata = json!({"type":"system","subtype":"init","sessionId":id,"cwd":root});
        std::fs::write(project.join(format!("{id}.jsonl")), format!("{metadata}\n")).unwrap();
    }
    if matches!(mode, "conversation" | "copies") {
        let project = home.join(if mode == "copies" {
            "projects/second"
        } else {
            "projects/first"
        });
        std::fs::create_dir_all(&project).unwrap();
        let user = json!({"type":"user","uuid":"native-user","parentUuid":null,"sessionId":id,"cwd":root,"version":"2.1.266","message":{"role":"user","content":[{"type":"text","text":"native content stays out of a metadata probe"}]}});
        let answer = json!({"type":"assistant","uuid":"assistant-cursor","parentUuid":"native-user","sessionId":id,"cwd":root,"version":"2.1.266","message":{"id":"api-message","role":"assistant","content":[{"type":"text","text":"answer"}]}});
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            format!("{user}\n{answer}\n"),
        )
        .unwrap();
    }
    let result = Agent::state(&claude, &id).await;
    if mode == "empty" {
        assert!(
            result.is_err(),
            "a readable metadata-only transcript cannot be resumed or overwritten"
        );
    } else {
        let state = result.unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_eq!(state.response.thread.id.as_ref().unwrap().id, id);
        assert_eq!(state.response.thread.status, SessionStatus::Idle);
        assert!(
            state.response.thread.turns.is_none(),
            "native state probes never return display bodies"
        );
        assert!(
            state.response.thread.history_read_state.is_none(),
            "a resume classification is not a second history import"
        );
    }
}

#[rstest::rstest]
#[case("same")]
#[case("moved")]
#[case("pinned")]
#[tokio::test]
async fn native_background_query_accepts_input_only_in_its_current_workspace_and_unpinned_head(
    #[case] mode: &str,
) {
    // T3 reuses a live background query only while its runtime policy still
    // matches. Sending through its existing SDK input queue cannot change cwd
    // or apply a historical resume position.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let other = root.join("other-workspace");
    std::fs::create_dir(&other).unwrap();
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
    let (input, mut received) = mpsc::channel::<Command>(1);
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = writes.clone();
    let acknowledgements = tokio::spawn(async move {
        if let Some(mut command) = received.recv().await {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(receipt) = command.delivered.take() {
                let _ = receipt.send(Ok(Some("accepted-native-turn".into())));
            }
        }
    });
    record.lock().await.running = Some(Running {
        turn_id: "old-control".into(),
        input,
        interrupt: watch::channel(None).1,
        effort: None,
        work: watch::channel(Work {
            settled: true,
            background: true,
        })
        .1,
    });
    let params = op::Submission {
        thread_id: session,
        client_user_message_id: Uuid::new_v4().to_string().into(),
        input: vec![op::Input::Text {
            text: "next input".into(),
        }],
        model: None,
        effort: None,
        service_tier: None,
        follow_up: op::FollowUp::Queue,
    };
    let cwd = if mode == "moved" { &other } else { &root };
    let result = Agent::submit(
        &claude,
        &params,
        crate::providers::SubmissionTarget::start(
            cwd.to_str().unwrap(),
            (mode == "pinned").then_some("retained-assistant"),
            true,
        ),
        false,
        None,
        Uuid::new_v4(),
    )
    .await;
    record.lock().await.running = None;
    acknowledgements.await.unwrap();
    if mode == "same" {
        assert!(
            result.is_ok(),
            "an unchanged background query keeps accepting the next turn"
        );
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);
    } else {
        let error =
            result.expect_err("replacing query scope must not send the input into the old process");
        assert_eq!(error.delivery, agent_protocol::error::Delivery::NotSent);
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(record.lock().await.cwd, root.to_str().unwrap());
    }
}
