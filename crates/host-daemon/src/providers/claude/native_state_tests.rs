#[tokio::test]
async fn queued_input_reads_only_metadata_and_keeps_owned_execution() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let native = root.join("native-home");
    let claude = Claude::load(
        root.join("unused"),
        root.join("adapter"),
        Some(native.clone()),
    )
    .await
    .unwrap();
    let session = claude
        .create(root.to_str().unwrap(), "owned-model")
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let project = native.join("projects/project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
            project.join(format!("{}.jsonl", session.id)),
            format!(
                "{}\n{}\nmalformed transcript body\n",
                json!({"sessionId":session.id,"cwd":root,"type":"user","message":{"role":"user","content":"Original input"}}),
                json!({"sessionId":session.id,"type":"custom-title","customTitle":"Preserved title"}),
            ),
        )
        .unwrap();
    let record = claude.record(&session.id).await.unwrap();
    let (input, _receiver) = mpsc::channel(1);
    let (_interrupt, interrupt) = watch::channel(None);
    {
        let mut record = record.lock().await;
        record.resumable = true;
        record.running = Some(Running {
            turn_id: "owned-turn".into(),
            input,
            interrupt,
            effort: None,
            work: watch::channel(Work::default()).1,
        });
    }
    let state = Agent::state(&claude, &session.id).await.unwrap();
    assert!(!state.needs_reload);
    assert_eq!(state.response.thread.id.as_ref(), Some(&session));
    assert_eq!(state.response.thread.cwd.as_deref(), root.to_str());
    assert_eq!(
        state.response.thread.name.as_deref(),
        Some("Preserved title")
    );
    assert_eq!(
        state.response.thread.preview.as_deref(),
        Some("Original input")
    );
    assert_eq!(state.response.thread.status, SessionStatus::Running);
    assert_eq!(
        state.response.thread.turns.unwrap()[0].id.as_str(),
        "owned-turn"
    );
    assert_eq!(state.response.model.unwrap().id, "owned-model");
    assert!(state.response.thread.history_read_state.is_none());

    record.lock().await.running = None;
    let state = Agent::state(&claude, &session.id).await.unwrap();
    assert!(
        state
            .response
            .thread
            .history_read_state
            .is_some_and(|state| {
                state.kind == agent_protocol::session::HistoryReadKind::Incomplete
            })
    );
}
