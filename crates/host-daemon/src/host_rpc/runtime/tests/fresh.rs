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

async fn reader(
    service: &HostRpcService,
    connection: crate::host_rpc::routing::SessionId,
    session: &SessionRef,
) -> String {
    let page: agent_protocol::handoff::Page = call(
        service,
        connection,
        Call::ReadHandoffHistory(agent_protocol::handoff::ReadHistory {
            session: session.clone(),
            item_id: None,
            offset: 0,
            limit: 20,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    serde_json::to_string(&page).unwrap()
}

#[tokio::test]
async fn fresh_context_keeps_the_displayed_transcript_but_never_hands_old_messages_back_to_any_provider()
 {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
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
    assert_eq!(codex.records.lock().unwrap().len(), 1);
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
    // The fresh start runs in a new native conversation of the same provider.
    assert_eq!(codex.records.lock().unwrap().len(), 2);
    assert!(
        !reader(&service, connection.id(), &target)
            .await
            .contains("before reset")
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
    }
    // Claude declines native injection, so its history arrives inline.
    let delivered = |agent: &NativeAgent| {
        serde_json::to_string(&(
            &*agent.injected.lock().unwrap(),
            &*agent.inputs.lock().unwrap(),
        ))
        .unwrap()
    };
    let claude_context = delivered(&claude);
    assert!(claude_context.contains("after reset"), "{claude_context}");
    // Only Codex's own first prompt ever carried "before reset".
    for (agent, own_prompts) in [(&codex, 1), (&claude, 0)] {
        let context = delivered(agent);
        assert_eq!(
            context.matches("before reset").count(),
            own_prompts,
            "{context}"
        );
    }
    assert!(
        serde_json::to_string(&codex.inputs.lock().unwrap()[0])
            .unwrap()
            .contains("before reset")
    );
    let page = reader(&service, connection.id(), &target).await;
    assert!(page.contains("after reset") && !page.contains("before reset"));
    // A fork inherits the boundary of its visible prefix.
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
            through: records::ForkPoint::Run {
                run_id: latest.to_string(),
            },
        }),
    )
    .await
    .unwrap();
    let forked = reader(&service, connection.id(), &fork).await;
    assert!(forked.contains("after reset") && !forked.contains("before reset"));
    // As in T3 CheckpointRollbackService, a rewind cannot cross into another
    // provider thread: the fresh start is not offered and a request is refused.
    let turns = completed(&service, &target, 4).await.thread.turns.unwrap();
    assert_eq!(turns[1].id, reset_turn);
    assert_eq!(turns[1].supports_rewind, Some(false));
    let refused: records::Acknowledged = call(
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
    assert!(refused.error.is_some());
    assert!(
        !reader(&service, connection.id(), &target)
            .await
            .contains("before reset")
    );
    service.shutdown_owned_processes().await;
}
