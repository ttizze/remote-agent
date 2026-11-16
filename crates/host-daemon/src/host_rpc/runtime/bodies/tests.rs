use super::*;
use crate::host_rpc::runtime::native_fixture::{NativeAgent, call, own};
use agent_protocol::conversation as decision;
use agent_protocol::{
    execution::{ItemStatus, TurnStatus},
    items::{AssistantPhase, Item, ItemBody},
    models::{Thread, ThreadResponse, Turn},
    protocol::Call,
};

#[rstest::rstest]
#[case(ProviderKind::Codex, true)]
#[case(ProviderKind::Codex, false)]
#[case(ProviderKind::Claude, true)]
#[case(ProviderKind::Claude, false)]
#[case(ProviderKind::DeepSeek, true)]
#[case(ProviderKind::DeepSeek, false)]
#[tokio::test]
async fn saved_tool_details_do_not_depend_on_native_history_or_a_rebound_source(
    #[case] provider: ProviderKind,
    #[case] persisted: bool,
) {
    use agent_protocol::orchestration as model;
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(provider);
    let native = SessionRef::new(provider, "actual-native-source".into()).unwrap();
    let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let full_output = format!("先頭\n{}\n末尾", "日".repeat(5000));
    let full = Item::new(
        "actual-command".into(),
        ItemStatus::Completed,
        ItemBody::CommandExecution {
            command: "recorded command".into(),
            cwd: None,
            output: full_output.clone(),
            output_omitted: false,
            exit_code: Some(0),
        },
    );
    let response = ThreadResponse {
        thread: Thread {
            id: Some(native.clone()),
            cwd: Some(directory.path().to_str().unwrap().into()),
            turns: Some(vec![Arc::new(Turn {
                id: "actual-native-turn".into(),
                status: TurnStatus::Completed,
                items: Some(vec![Arc::new(full.clone())]),
                ..Default::default()
            })]),
            ..Default::default()
        },
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .insert(native.id.clone(), response.clone());
    agent.unreadable.lock().unwrap().insert(full.id.to_string());
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    own(&service, &public, &response).await;
    let mut deferred = full;
    if !persisted {
        deferred.defer();
    }
    assert_eq!(deferred.is_deferred(), !persisted);
    let native_change = |change| crate::providers::AgentChange::Session {
        session: native.clone(),
        change,
    };
    Runtime::observe(
        &service,
        native_change(SessionChange::Item {
            turn_id: "actual-native-turn".into(),
            item: Arc::new(deferred),
        }),
        None,
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        native_change(SessionChange::Item {
            turn_id: "actual-native-turn".into(),
            item: Arc::new(Item::new(
                "later-answer".into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: "later native work survived".into(),
                    phase: AssistantPhase::Final,
                },
            )),
        }),
        None,
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        native_change(SessionChange::Turn {
            turn: Turn {
                id: "actual-native-turn".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        }),
        None,
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    let before = storage.thread_stream(&public, None).unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &before[0] else {
        panic!("snapshot");
    };
    let command = snapshot
        .projection
        .turn_items
        .iter()
        .find(|item| matches!(item.body, model::TurnItemBody::CommandExecution { .. }))
        .unwrap();
    let saved = storage.thread_item(&public, &command.id).unwrap().unwrap();
    assert_eq!(saved.body.has_omitted_detail(), !persisted);
    assert_eq!(saved.status, model::TurnItemStatus::Completed);
    assert!(snapshot.projection.runs.is_empty());
    assert!(snapshot.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::AssistantMessage {text,..} if text=="later native work survived")));
    let connection = service.open_session();
    let request = || {
        Call::HydrateConversation(records::Hydrate {
            session: public.clone(),
            item: saved.id.clone().into(),
        })
    };
    // T3 getTurnItem reads the saved projection. Availability or subsequent
    // contents of a provider transcript cannot change a read-only detail query.
    let result: records::Hydrated = call(&service, connection.id(), request()).await.unwrap();
    let detail = result.item.unwrap();
    assert_eq!(detail.body, saved.body);
    assert_eq!(before, storage.thread_stream(&public, None).unwrap());
    agent.unreadable.lock().unwrap().clear();
    let result: records::Hydrated = call(&service, connection.id(), request()).await.unwrap();
    let detail = result.item.unwrap();
    assert_eq!(detail.body, saved.body);
    assert_eq!(detail.id, saved.id);
    assert_eq!(detail.updated_at, saved.updated_at);
    assert_eq!(detail.ordinal, saved.ordinal);
    if persisted {
        assert!(
            matches!(&detail.body,model::TurnItemBody::CommandExecution {output:Some(text),..} if text==&full_output)
        );
    } else {
        assert!(detail.body.has_omitted_detail());
    }
    let reopened = HostRpcService::new([], directory.path().join("projects.json"));
    let offline = reopened.open_session();
    let result: records::Hydrated = call(&reopened, offline.id(), request()).await.unwrap();
    assert_eq!(result.item.unwrap().body, saved.body);
    assert_eq!(before, storage.thread_stream(&public, None).unwrap());
    assert!(agent.inputs.lock().unwrap().is_empty());

    // Actual app recovery reuses this logical provider row while allocating a
    // different physical session. Its copied native IDs can overlap. Reading
    // the old command must still use the source observed for that old turn.
    *agent.state_error.lock().unwrap() = Some(crate::providers::Failure::new(
        "lost_resource",
        "old runtime was unloaded",
    ));
    let input = Uuid::new_v4();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: public.clone(),
            operation: input,
            command: Command::Submit {
                id: input.to_string().into(),
                content: decision::InputContent {
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: "explicit new input".into(),
                    }],
                    model: Some(agent_protocol::models::ModelRef {
                        provider,
                        id: "fixture".into(),
                    }),
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(ack.error, None);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut changed = storage.changes();
        loop {
            let state = storage.thread_state(&public.application_id(), &[]).unwrap();
            if state
                .runs
                .values()
                .any(|run| run.status == model::RunStatus::Completed)
            {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the explicitly submitted input completes");
    let rebound = storage.active_source(&public).unwrap().unwrap();
    assert_ne!(
        rebound, native,
        "the normal recovery path replaces the physical source"
    );
    {
        let mut records = agent.records.lock().unwrap();
        records
            .get_mut(&rebound.id)
            .unwrap()
            .thread
            .turns
            .as_mut()
            .unwrap()
            .push(Arc::new(Turn {
                id: "actual-native-turn".into(),
                status: TurnStatus::Completed,
                items: Some(vec![Arc::new(Item::new(
                    "actual-command".into(),
                    ItemStatus::Completed,
                    ItemBody::CommandExecution {
                        command: "recorded command".into(),
                        cwd: None,
                        output: "another native source's output".into(),
                        output_omitted: false,
                        exit_code: Some(0),
                    },
                ))]),
                ..Default::default()
            }));
    }
    let before_detail = storage.thread_stream(&public, None).unwrap();
    let reread: records::Hydrated = call(&service, connection.id(), request()).await.unwrap();
    assert_eq!(reread.item.unwrap().body, saved.body);
    assert_eq!(before_detail, storage.thread_stream(&public, None).unwrap());
    assert_eq!(
        storage.native_owner(&native).unwrap(),
        Some(public.clone()),
        "the historical physical source remains private after rebinding"
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unreadable_message_history_is_atomic_and_can_be_retried_without_importing_native_tool_cards()
 {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Codex);
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
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let imported = Runtime::native_history(&service, &native).await.unwrap();
        let items = imported.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), "answer");
        assert!(items[0].is_deferred());
        assert_eq!(
            imported.thread.history_read_state.as_ref().unwrap().kind,
            agent_protocol::session::HistoryReadKind::Partial
        );
        assert!(
            Runtime::import_native_response(&service, &public, &native, &imported)
                .await
                .is_err()
        );
        let storage = &service.inner.conversation.storage;
        assert!(storage.thread_header(&public).unwrap().is_none());
        agent.unreadable.lock().unwrap().clear();
        let imported = Runtime::native_history(&service, &native).await.unwrap();
        Runtime::import_native_response(&service, &public, &native, &imported)
            .await
            .unwrap();
        let state = storage.thread_state(&public.application_id(), &[]).unwrap();
        assert_eq!(
            state.thread.history_origin,
            Some(agent_protocol::orchestration::ThreadHistoryOrigin::V1Import)
        );
        assert!(state.runs.is_empty());
        assert!(state.provider_turns.is_empty());
        let history = super::super::native_fixture::history(&service, &public).unwrap();
        let text = history
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .find_map(|item| match item.body() {
                ItemBody::AssistantText { text, .. } => Some(text),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, &body);
        agent.unreadable.lock().unwrap().insert("answer".into());
        Runtime::ensure_imported(&service, &public).await.unwrap();
        assert_eq!(
            state,
            storage.thread_state(&public.application_id(), &[]).unwrap()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unreadable_native_messages_block_handoff_and_switch_until_atomic_import_succeeds() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        use agent_protocol::{
            conversation::{Command, InputContent},
            handoff,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut codex = NativeAgent::new(ProviderKind::Codex);
        Arc::get_mut(&mut codex).unwrap().deferred_history = true;
        let claude = NativeAgent::new(ProviderKind::Claude);
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
                item_id: None,
                session: native.clone(),
                offset: 0,
                limit: 1,
                text_offset,
            })
        };
        assert!(
            call::<handoff::Page>(&service, connection.id(), read(0))
                .await
                .is_err()
        );
        let input = Uuid::new_v4();
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        let rejected = Runtime::change(
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
        .await;
        assert!(rejected.is_err());
        assert!(storage.thread_header(&native).unwrap().is_none());
        assert!(
            claude.inputs.lock().unwrap().is_empty(),
            "an unreadable required message must not be silently omitted"
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
        while storage
            .thread_state(&native.application_id(), &[])
            .unwrap()
            .active_run()
            .is_some()
        {
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
