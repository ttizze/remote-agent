use super::*;
use agent_protocol::attribution::Author;

#[tokio::test]
async fn read_only_child_prompts_are_attributed_to_the_parent_but_editable_threads_are_not_guessed()
{
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("codex"));
    let host = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
    for writable in [false, true] {
        agent.records.lock().unwrap().insert(
            child.id.clone(),
            agent_protocol::models::ThreadResponse {
                thread: agent_protocol::models::Thread {
                    id: Some(child.clone()),
                    parent_id: Some(parent.clone()),
                    can_accept_direct_input: Some(writable),
                    turns: Some(vec![Arc::new(agent_protocol::models::Turn {
                        id: "turn".into(),
                        items: Some(vec![Arc::new(agent_protocol::models::Item::new(
                            "prompt".into(),
                            agent_protocol::execution::ItemStatus::Completed,
                            agent_protocol::items::ItemBody::UserMessage {
                                text: Some("Investigate".into()),
                                content: vec![],
                            },
                        ))]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let history = Runtime::native_history(&host, &child).await.unwrap();
        let author = &history.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0]
            .author;
        assert_eq!(
            author,
            &(!writable).then(|| Box::new(Author::Agent {
                session: parent.clone(),
                name: None
            }))
        );
    }
    host.shutdown_owned_processes().await;
}

#[tokio::test]
async fn authenticated_origin_overrides_claimed_authors_and_survives_history_and_fork() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_authenticated_session("paired-phone".into());
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let mut input = content();
    input.author = Some(Box::new(Author::System {
        label: "forged".into(),
    }));
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            session: target.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            selection: Default::default(),
            initial: Some((Uuid::new_v4().to_string().into(), input.clone())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    let operation = Uuid::new_v4();
    let acknowledged: records::Acknowledged = call(
        &service,
        connection.id(),
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
    let history = completed(&service, &target, 2).await;
    let turns = history.thread.turns.as_ref().unwrap();
    for turn in turns {
        let user = turn
            .items
            .as_ref()
            .unwrap()
            .iter()
            .find(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                )
            })
            .unwrap();
        assert_eq!(
            user.author,
            Some(Box::new(Author::Device {
                id: "paired-phone".into()
            }))
        );
    }
    let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: target,
            target: fork.clone(),
            through: turns[0].id.clone(),
        }),
    )
    .await
    .unwrap();
    let history = service
        .inner
        .conversation
        .storage
        .history(&fork, None)
        .unwrap();
    assert_eq!(
        history.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0]
            .author,
        Some(Box::new(Author::Device {
            id: "paired-phone".into()
        }))
    );
    service.shutdown_owned_processes().await;
}
