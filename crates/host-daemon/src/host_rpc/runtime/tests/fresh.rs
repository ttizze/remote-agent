use super::*;

async fn send(
    service: &HostRpcService,
    connection: crate::host_rpc::routing::SessionId,
    target: &SessionRef,
    prompt: &str,
    fresh_context: bool,
    provider: ProviderKind,
) {
    let mut input = content();
    input.parts = vec![op::Input::Text {
        text: prompt.into(),
    }];
    input.fresh_context = fresh_context;
    input.model = Some(agent_protocol::models::ModelRef {
        provider,
        id: "fixture".into(),
    });
    let operation = Uuid::new_v4();
    let acknowledged: records::Acknowledged = call(
        service,
        connection,
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: input,
            },
        }),
    )
    .await
    .unwrap();
    acknowledged.accepted().unwrap();
}

#[tokio::test]
async fn fresh_context_keeps_the_displayed_transcript_but_never_hands_old_messages_back_to_any_provider()
 {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            session: target.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: None,
        }),
    )
    .await
    .unwrap();
    send(
        &service,
        connection.id(),
        &target,
        "before reset",
        false,
        ProviderKind::Codex,
    )
    .await;
    completed(&service, &target, 1).await;
    let first_source = service
        .inner
        .conversation
        .storage
        .active_source(&target)
        .unwrap()
        .unwrap();
    send(
        &service,
        connection.id(),
        &target,
        "after reset",
        true,
        ProviderKind::Codex,
    )
    .await;
    let history = completed(&service, &target, 2).await;
    let turns = history.thread.turns.as_ref().unwrap();
    assert!(!turns[0].fresh_context);
    assert!(turns[1].fresh_context);
    let reset_turn = turns[1].id.clone();
    assert_ne!(
        service
            .inner
            .conversation
            .storage
            .active_source(&target)
            .unwrap()
            .unwrap(),
        first_source
    );
    for (count, provider) in [(3, ProviderKind::Claude), (4, ProviderKind::Codex)] {
        send(
            &service,
            connection.id(),
            &target,
            "continue",
            false,
            provider,
        )
        .await;
        completed(&service, &target, count).await;
        let (context, missing) = Runtime::handoff_history(&service, &target, 0)
            .await
            .unwrap();
        assert!(!missing);
        let text = serde_json::to_string(&context.thread.turns).unwrap();
        assert!(text.contains("after reset"));
        assert!(!text.contains("before reset"));
        let page: agent_protocol::handoff::Page = call(
            &service,
            connection.id(),
            Call::ReadHandoffHistory(agent_protocol::handoff::ReadHistory {
                session: target.clone(),
                offset: 0,
                limit: 20,
                text_offset: 0,
            }),
        )
        .await
        .unwrap();
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("before reset")
        );
    }
    for agent in [&codex, &claude] {
        let injected = serde_json::to_string(&*agent.injected.lock().unwrap()).unwrap();
        assert!(!injected.contains("before reset"));
    }
    let latest = completed(&service, &target, 4)
        .await
        .thread
        .turns
        .as_ref()
        .unwrap()
        .last()
        .unwrap()
        .id
        .clone();
    let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: target.clone(),
            target: fork.clone(),
            through: latest,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .header(&fork)
            .unwrap()
            .state
            .context_start,
        1
    );
    let (fork_history, _) = Runtime::handoff_history(&service, &fork, 0).await.unwrap();
    assert!(
        !serde_json::to_string(&fork_history.thread.turns)
            .unwrap()
            .contains("before reset")
    );
    let _: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Rewind(reset_turn),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .header(&target)
            .unwrap()
            .state
            .context_start,
        0
    );
    let (rewound, _) = Runtime::handoff_history(&service, &target, 0)
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&rewound)
            .unwrap()
            .contains("before reset")
    );
    assert_eq!(rewound.thread.turns.unwrap().len(), 1);
    service.shutdown_owned_processes().await;
}
