#[tokio::test]
async fn fork_refuses_only_the_turn_still_answering() {
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
        .create(root.to_str().unwrap(), "default")
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let project = native.join("projects/project");
    std::fs::create_dir_all(&project).unwrap();
    let row = |kind: &str, uuid: &str, parent: Option<&str>, content: Value| {
        json!({"sessionId":session.id,"cwd":root,"type":kind,"uuid":uuid,"parentUuid":parent,
            "timestamp":"2026-10-28T01:00:00.000Z","message":{"id":uuid,"content":content}})
        .to_string()
    };
    let text = |text: &str| json!([{"type":"text","text":text}]);
    std::fs::write(
        project.join(format!("{}.jsonl", session.id)),
        [
            row("user", "first", None, json!("first")),
            row("assistant", "answer", Some("first"), text("done")),
            row("user", "second", Some("answer"), json!("second")),
            row("assistant", "partial", Some("second"), text("still")),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let (input, _receiver) = mpsc::channel(1);
    let (_interrupt, interrupt) = watch::channel(None);
    claude
        .record(&session.id)
        .await
        .unwrap()
        .lock()
        .await
        .running = Some(Running {
        turn_id: "second".into(),
        input,
        interrupt,
        effort: None,
        work: watch::channel(Work::default()).1,
    });
    let refused = Agent::fork(&claude, &session.id, "second", None)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "fork_failed");
    assert!(refused.message.contains("生成中"), "{}", refused.message);
    // A completed turn gets past the running turn to the account, which
    // this Host has not selected; forking it succeeds in host-fixture.
    let completed = Agent::fork(&claude, &session.id, "first", None)
        .await
        .unwrap_err();
    assert!(
        !completed.message.contains("生成中") && !completed.message.contains("running"),
        "{}",
        completed.message
    );
}

