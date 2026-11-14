use super::*;
use crate::host_rpc::runtime::native_fixture::{NativeAgent, call, own};
use agent_protocol::{
    execution::{ItemStatus, TurnStatus},
    items::{AssistantPhase, Item, ItemBody},
    models::{Thread, ThreadResponse, Turn},
    protocol::Call,
};

#[tokio::test]
async fn unreadable_details_keep_other_history_and_can_be_retried_individually() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        Arc::get_mut(&mut agent).unwrap().deferred_history = true;
        let native = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let body = "long native answer ".repeat(200);
        let items = vec![
            Item::new(
                "answer".into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: body.clone(),
                    phase: AssistantPhase::Final,
                },
            ),
            Item::new(
                "image".into(),
                ItemStatus::Completed,
                ItemBody::ImageView {
                    path: "/workspace/result.png".into(),
                },
            ),
            Item::new(
                "tool".into(),
                ItemStatus::Completed,
                ItemBody::ToolCall {
                    kind: agent_protocol::items::ToolKind::Local,
                    tool: "ordinary tool".into(),
                    server: None,
                    namespace: None,
                    resource_uri: None,
                    plugin_id: None,
                    arguments: serde_json::json!({}),
                    result: None,
                    error: None,
                    content: vec![],
                    success: Some(true),
                    duration_ms: None,
                },
            ),
        ];
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    turns: Some(vec![Arc::new(Turn {
                        id: "turn".into(),
                        status: TurnStatus::Completed,
                        items: Some(items.into_iter().map(Arc::new).collect()),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        agent.unreadable.lock().unwrap().insert("answer".into());
        let service = HostRpcService::new(
            [agent.backend()],
            directory.path().join("projects.json"),
        );
        let imported = Runtime::native_history(&service, &native).await.unwrap();
        let items = imported.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.as_str(), "answer");
        assert!(items[0].is_deferred());
        assert!(items[1].is_image_output());
        assert_eq!(
            imported.thread.history_read_state.as_ref().unwrap().kind,
            agent_protocol::session::HistoryReadKind::Partial
        );
        own(&service, &public, &imported);
        let connection = service.open_session();
        let subscription = Uuid::new_v4();
        let mut opened = service
            .inner
            .conversation
            .owners
            .subscribe(
                &public,
                connection.id(),
                subscription,
                None,
                None,
                service.inner.router.queue_budget(connection.id()).unwrap(),
            )
            .await
            .unwrap();
        let request = Call::HydrateConversation(records::Hydrate {
            session: public.clone(),
            subscription,
            item: "answer".into(),
        });
        assert!(
            call::<records::Hydrated>(&service, connection.id(), request.clone())
                .await
                .is_err()
        );
        assert!(
            service
                .inner
                .conversation
                .storage
                .item(&public, &"answer".into())
                .unwrap()
                .unwrap()
                .deferred
        );
        agent.unreadable.lock().unwrap().clear();
        let response: records::Hydrated = call(&service, connection.id(), request).await.unwrap();
        assert!(response.transfer.is_none());
        let mut found = false;
        while !found {
            let message: records::Message = agent_protocol::protocol::decode(&opened.subscription.recv().await.unwrap()).unwrap();
            if let records::Message::Hydrate { row, .. } = message {
                let (agent_protocol::sync::Content::Full(record) | agent_protocol::sync::Content::Summary(record)) = row.content;
                if let records::Record::Item(item) = record.as_ref() {
                    found = !item.content.is_deferred() && matches!(item.content.body(), ItemBody::AssistantText { text, .. } if text == &body);
                }
            }
        }
        // Once canonical, reading no longer needs the native body.
        agent.unreadable.lock().unwrap().insert("answer".into());
        Runtime::hydrate_native(&service, &public, &"answer".into())
            .await
            .unwrap();
        drop(opened.subscription);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn handoff_rpc_imports_unloaded_history_and_switch_recovers_messages_despite_unreadable_images()
 {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        use agent_protocol::{
            conversation::{Command, InputContent, InputState},
            handoff,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        Arc::get_mut(&mut codex).unwrap().deferred_history = true;
        let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        let native = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        let answer = "日".repeat(4_205);
        codex.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    status: agent_protocol::models::SessionStatus::Idle,
                    turns: Some(vec![Arc::new(Turn {
                        id: "history".into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![
                            Arc::new(Item::new(
                                "answer".into(),
                                ItemStatus::Completed,
                                ItemBody::AssistantText {
                                    text: answer.clone(),
                                    phase: AssistantPhase::Final,
                                },
                            )),
                            Arc::new(Item::new(
                                "second".into(),
                                ItemStatus::Completed,
                                ItemBody::AssistantText {
                                    text: "readable message".into(),
                                    phase: AssistantPhase::Final,
                                },
                            )),
                            Arc::new(Item::new(
                                "image".into(),
                                ItemStatus::Completed,
                                ItemBody::ImageView {
                                    path: "/missing.png".into(),
                                },
                            )),
                        ]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        codex
            .unreadable
            .lock()
            .unwrap()
            .extend(["answer".into(), "image".into()]);
        let service = HostRpcService::new(
            [codex.backend(), claude.backend()],
            directory.path().join("projects.json"),
        );
        let connection = service.open_session();
        let read = |text_offset| {
            Call::ReadHandoffHistory(handoff::ReadHistory {
                session: native.clone(),
                offset: 0,
                limit: 1,
                text_offset,
            })
        };
        let partial: handoff::Page = call(&service, connection.id(), read(0)).await.unwrap();
        assert_eq!(partial.items[0].message.text, "readable message");
        assert_eq!(
            partial.history_read_state.unwrap().kind,
            agent_protocol::session::HistoryReadKind::Partial
        );
        let input = Uuid::new_v4();
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        Runtime::change(
            &service,
            &records::Change {
                session: native.clone(),
                operation: input,
                command: Command::Submit {
                    id: input.to_string().into(),
                    content: InputContent {
                        fresh_context: false,
                        parts: vec![op::Input::Text {
                            text: "continue on Claude".into(),
                        }],
                        model: Some(agent_protocol::models::ModelRef {
                            provider: ProviderKind::Claude,
                            id: "model".into(),
                        }),
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            },
        )
        .await
        .unwrap();
        while !storage
            .state(&native)
            .unwrap()
            .inputs
            .values()
            .any(|input| matches!(input.state, InputState::Failed { .. }))
        {
            changed.changed().await.unwrap();
        }
        assert!(
            claude.inputs.lock().unwrap().is_empty(),
            "required messages must not be omitted on a provider switch"
        );
        codex.unreadable.lock().unwrap().remove("answer");
        let first: handoff::Page = call(&service, connection.id(), read(0)).await.unwrap();
        assert_eq!(first.items[0].message.text, "日".repeat(4_000));
        assert_eq!(first.items[0].next_text_offset, Some(4_000));
        assert_eq!(first.next_offset, Some(1));
        let rest: handoff::Page = call(&service, connection.id(), read(4_000)).await.unwrap();
        assert_eq!(rest.items[0].message.text, "日".repeat(205));
        assert_eq!(rest.items[0].next_text_offset, None);
        Runtime::change(
            &service,
            &records::Change {
                session: native.clone(),
                operation: Uuid::new_v4(),
                command: Command::RetryInput {
                    id: input.to_string().into(),
                    work_locally: false,
                },
            },
        )
        .await
        .unwrap();
        while !storage.state(&native).unwrap().inputs.is_empty() {
            changed.changed().await.unwrap();
        }
        let inputs = claude.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        let text = inputs[0]
            .input
            .iter()
            .filter_map(|part| match part {
                op::Input::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(&answer));
        assert!(text.contains("readable message"));
    })
    .await
    .unwrap();
}
