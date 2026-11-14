use super::*;

fn fresh(root: &SessionRef, provider: ProviderKind, id: &str) -> op::Submission {
    op::Submission {
        fresh_context: true,
        thread_id: root.clone(),
        client_user_message_id: id.into(),
        input: vec![op::Input::Text { text: id.into() }],
        model: Some(ModelRef {
            provider,
            id: "model".into(),
        }),
        effort: None,
        service_tier: None,
        follow_up: op::FollowUp::Steer,
    }
}

#[tokio::test]
async fn fresh_context_keeps_visible_history_and_excludes_it_from_later_provider_switches() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = codex
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &codex, &claude);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "before-a",
        "before-reset-a",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "before-b",
        "before-reset-b",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "before-c",
        "before-reset-c",
    )
    .await
    .unwrap();
    let old_claude = host
        .inner
        .conversations
        .get(&root)
        .unwrap()
        .unwrap()
        .binding(ProviderKind::Claude)
        .unwrap()
        .session
        .clone();
    let input = fresh(&root, ProviderKind::Claude, "after-reset");
    let receipt: op::SubmissionReceipt = call(&host, client.id(), Call::Submit(input.clone()))
        .await
        .unwrap();
    let row = host.inner.conversations.get(&root).unwrap().unwrap();
    assert_ne!(row.active, old_claude);
    assert_eq!(row.context_start(), 3);
    assert!(row.binding(ProviderKind::Codex).is_none());
    let delivered = claude.inputs.lock().unwrap().last().unwrap().clone();
    assert_eq!(delivered.input, input.input);
    assert!(!delivered.fresh_context);
    assert_eq!(
        claude.records.lock().unwrap()[&row.active.id]
            .thread
            .turns
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    let shown = history(&host, client.id(), &root).await;
    assert_eq!(shown.thread.id.as_ref(), Some(&root));
    let turns = shown.thread.turns.unwrap();
    assert_eq!(turns.len(), 4);
    assert!(turns[3].fresh_context);
    assert_eq!(Some(&turns[3].id), receipt.turn_id.as_ref());
    assert!(turns[..3].iter().all(|turn| !turn.fresh_context));
    let packet: handoff::Page = call(
        &host,
        client.id(),
        Call::ReadHandoffHistory(handoff::ReadHistory {
            session: root.clone(),
            offset: 0,
            limit: 20,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    let packet = serde_json::to_string(&packet).unwrap();
    assert!(packet.contains("after-reset"));
    assert!(!packet.contains("before-reset"));
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "new-a",
        "after-reset-a",
    )
    .await
    .unwrap();
    let (target, packet) = codex.injected.lock().unwrap().last().unwrap().clone();
    assert_ne!(target, root.id);
    assert!(packet.render().contains("after-reset"));
    assert!(!packet.render().contains("before-reset"));
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "new-b",
        "after-reset-b",
    )
    .await
    .unwrap();
    let delivered = claude.inputs.lock().unwrap().last().unwrap().clone();
    let payload = serde_json::to_string(&delivered.input).unwrap();
    assert!(payload.contains("after-reset-a"));
    assert!(!payload.contains("before-reset"));
    assert_eq!(delivered.thread_id, row.active);
    let shown = history(&host, client.id(), &root).await;
    assert_eq!(
        shown
            .thread
            .turns
            .as_ref()
            .unwrap()
            .iter()
            .filter(|turn| turn.fresh_context)
            .count(),
        1
    );

    // Forking keeps the reset boundary; rewinding past it deliberately removes it.
    let last = shown
        .thread
        .turns
        .as_ref()
        .unwrap()
        .iter()
        .rfind(|turn| turn.provider == Some(ProviderKind::Codex))
        .unwrap()
        .id
        .clone();
    let fork: ThreadResponse = call(
        &host,
        client.id(),
        Call::ForkSession(op::ForkSession {
            thread_id: root.clone(),
            last_turn_id: last,
        }),
    )
    .await
    .unwrap();
    let fork = fork.thread.id.unwrap();
    assert_eq!(
        host.inner
            .conversations
            .get(&fork)
            .unwrap()
            .unwrap()
            .context_start(),
        3
    );
    send(
        &host,
        client.id(),
        &fork,
        ProviderKind::Claude,
        "forked",
        "forked after reset",
    )
    .await
    .unwrap();
    let payload =
        serde_json::to_string(&claude.inputs.lock().unwrap().last().unwrap().input).unwrap();
    assert!(payload.contains("after-reset-a"));
    assert!(!payload.contains("before-reset"));
    let rewound: ThreadResponse = call(
        &host,
        client.id(),
        Call::RewindSession(op::RewindSession {
            thread_id: root.clone(),
            turn_id: receipt.turn_id.unwrap(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(rewound.thread.turns.as_ref().unwrap().len(), 3);
    assert_eq!(
        host.inner
            .conversations
            .get(&root)
            .unwrap()
            .unwrap()
            .context_start(),
        0
    );
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "before-again",
        "continuing before reset",
    )
    .await
    .unwrap();
    let payload =
        serde_json::to_string(&claude.inputs.lock().unwrap().last().unwrap().input).unwrap();
    assert!(payload.contains("before-reset-a"));
}

#[tokio::test]
async fn fresh_context_rejection_restores_the_old_binding_and_unknown_delivery_never_replays() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = codex
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &codex, &claude);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "original",
        "original context",
    )
    .await
    .unwrap();
    let input = fresh(&root, ProviderKind::Codex, "fresh-input");
    codex.fail_submit.store(1, Ordering::SeqCst);
    assert!(
        call::<op::SubmissionReceipt>(&host, client.id(), Call::Submit(input.clone()))
            .await
            .is_err()
    );
    let row = host.inner.conversations.get(&root).unwrap().unwrap();
    assert_eq!(row.active, root);
    assert_eq!(row.context_start(), 0);
    let receipt: op::SubmissionReceipt = call(&host, client.id(), Call::Submit(input.clone()))
        .await
        .unwrap();
    let created = codex.next.load(Ordering::SeqCst);
    let duplicate: op::SubmissionReceipt =
        call(&host, client.id(), Call::Submit(input)).await.unwrap();
    assert_eq!(receipt, duplicate);
    assert_eq!(codex.next.load(Ordering::SeqCst), created);
    assert_eq!(codex.inputs.lock().unwrap().len(), 2);
    let input = fresh(&root, ProviderKind::Codex, "uncertain-input");
    codex.fail_submit.store(2, Ordering::SeqCst);
    assert!(
        call::<op::SubmissionReceipt>(&host, client.id(), Call::Submit(input.clone()))
            .await
            .is_err()
    );
    let created = codex.next.load(Ordering::SeqCst);
    let codex = codex.restarted();
    let claude = claude.restarted();
    let reopened = service(directory.path(), &codex, &claude);
    let client = reopened.open_session();
    let failure = call::<op::SubmissionReceipt>(&reopened, client.id(), Call::Submit(input))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "submission_delivery_unknown");
    assert_eq!(codex.next.load(Ordering::SeqCst), created);
    assert_eq!(
        history(&reopened, client.id(), &root)
            .await
            .thread
            .turns
            .as_ref()
            .unwrap()
            .len(),
        2
    );
}
