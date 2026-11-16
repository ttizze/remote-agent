use super::super::native_fixture::{NativeAgent, bound_execution, call};
use super::*;
use agent_protocol::conversation as decision;
use agent_protocol::{execution::ItemStatus, items::ItemBody, protocol::Call};

async fn launch(service: &HostRpcService, cwd: &str, text: &str) -> SessionRef {
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        service,
        connection.id(),
        Call::CreateConversation(records::Create {
            session: target.clone(),
            subscription: Uuid::new_v4(),
            provider: ProviderKind::DeepSeek,
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            cwd: Some(cwd.into()),
            selection: Default::default(),
            initial: Some((
                Uuid::new_v4().to_string().into(),
                decision::InputContent {
                    fresh_context: false,
                    parts: vec![op::Input::Text { text: text.into() }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            )),
        }),
    )
    .await
    .unwrap();
    target
}

async fn plan(
    service: &HostRpcService,
    native: &SessionRef,
    execution: Uuid,
    text: &str,
) -> Result<(), String> {
    // The real DSH adapter emits this same ID for every plan update. Its turn
    // IDs are Host UUIDs; weak native item IDs are only meaningful in a turn.
    Runtime::observe(
        service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Item {
                turn_id: format!("turn-{execution}").into(),
                item: Arc::new(agent_protocol::items::Item::new(
                    "deepseek-plan".into(),
                    ItemStatus::Completed,
                    ItemBody::Plan { text: text.into() },
                )),
            },
        },
        Some(execution),
    )
    .await
}

async fn assistant(
    service: &HostRpcService,
    native: &SessionRef,
    execution: Uuid,
    leading: &str,
    delta: &str,
) -> Result<(), String> {
    let turn_id = format!("turn-{execution}");
    Runtime::observe(
        service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Item {
                turn_id: turn_id.clone().into(),
                item: Arc::new(agent_protocol::items::Item::new(
                    "reused-message".into(),
                    ItemStatus::Running,
                    ItemBody::AssistantText {
                        text: leading.into(),
                        phase: Default::default(),
                    },
                )),
            },
        },
        Some(execution),
    )
    .await?;
    for character in delta.chars() {
        Runtime::observe(
            service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Text {
                    turn_id: turn_id.clone().into(),
                    item_id: "reused-message".into(),
                    field: agent_protocol::session::TextField::AssistantText,
                    delta: character.to_string(),
                },
            },
            Some(execution),
        )
        .await?;
    }
    Ok(())
}

fn messages(service: &HostRpcService, target: &SessionRef) -> Vec<(String, String, String)> {
    let stream = service
        .inner
        .conversation
        .storage
        .thread_stream(target, None)
        .unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &stream[0] else {
        panic!("snapshot")
    };
    snapshot
        .projection
        .turn_items
        .iter()
        .filter_map(|item| {
            if item
                .native_item_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                != Some("reused-message")
            {
                return None;
            }
            let model::TurnItemBody::AssistantMessage {
                message_id, text, ..
            } = &item.body
            else {
                panic!("assistant body")
            };
            Some((item.id.clone(), message_id.clone(), text.clone()))
        })
        .collect()
}

async fn completed(service: &HostRpcService, targets: &[SessionRef]) {
    let storage = &service.inner.conversation.storage;
    let mut changed = storage.changes();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if targets.iter().all(|target| {
                let state = storage.thread_state(&target.id, &[]).unwrap();
                !state.runs.is_empty()
                    && state
                        .runs
                        .values()
                        .all(|run| run.status == model::RunStatus::Completed)
            }) {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the gated native writes complete");
}

fn plans(service: &HostRpcService, target: &SessionRef) -> Vec<(String, String, String)> {
    let stream = service
        .inner
        .conversation
        .storage
        .thread_stream(target, None)
        .unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &stream[0] else {
        panic!("snapshot")
    };
    snapshot
        .projection
        .turn_items
        .iter()
        .filter_map(|item| {
            let model::TurnItemBody::ProposedPlan {
                plan_id, markdown, ..
            } = &item.body
            else {
                return None;
            };
            let reference = item.native_item_ref.as_ref().unwrap();
            assert_eq!(reference.native_id.as_deref(), Some("deepseek-plan"));
            assert_eq!(reference.strength, model::NativeRefStrength::Weak);
            Some((item.id.clone(), plan_id.clone(), markdown.clone()))
        })
        .collect()
}

#[tokio::test]
async fn weak_native_plan_ids_are_scoped_across_conversations_and_successive_turns_without_changing_raw_references()
 {
    let root = tempfile::tempdir().unwrap();
    let mut agent = NativeAgent::new(ProviderKind::DeepSeek);
    Arc::get_mut(&mut agent).unwrap().submit_gate = Some(tokio::sync::Semaphore::new(0));
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let cwd = root.path().to_str().unwrap();
    let submitted = agent.submit_started.notified();
    let a = launch(&service, cwd, "first conversation").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), submitted)
        .await
        .unwrap();
    let (native_a, execution_a) = bound_execution(&service, &a);
    let first = plan(&service, &native_a, execution_a, "first plan A").await;
    let text_a = assistant(&service, &native_a, execution_a, "A: ", "日本語").await;
    let submitted = agent.submit_started.notified();
    let b = launch(&service, cwd, "second conversation").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), submitted)
        .await
        .unwrap();
    let (native_b, execution_b) = bound_execution(&service, &b);
    let second = plan(&service, &native_b, execution_b, "plan B").await;
    let text_b = assistant(&service, &native_b, execution_b, "B: ", "答え").await;
    agent.submit_gate.as_ref().unwrap().add_permits(2);
    completed(&service, &[a.clone(), b.clone()]).await;
    // Drain returned permits after SDK receipts, before the next gated turn.
    agent
        .submit_gate
        .as_ref()
        .unwrap()
        .acquire_many(2)
        .await
        .unwrap()
        .forget();
    assert!(first.is_ok(), "first native plan failed: {first:?}");
    assert!(
        second.is_ok(),
        "a weak ID reused by another conversation must persist independently: {second:?}"
    );
    assert!(text_a.is_ok());
    assert!(text_b.is_ok());
    let messages_a = messages(&service, &a);
    let messages_b = messages(&service, &b);
    assert_eq!(messages_a.len(), 1);
    assert_eq!(messages_b.len(), 1);
    assert_ne!(messages_a[0].0, messages_b[0].0);
    assert_ne!(messages_a[0].1, messages_b[0].1);
    assert_eq!(messages_a[0].2, "A: 日本語");
    assert_eq!(messages_b[0].2, "B: 答え");
    let first_a = plans(&service, &a);
    let first_b = plans(&service, &b);
    assert_eq!(first_a.len(), 1);
    assert_eq!(first_b.len(), 1);
    assert_ne!(first_a[0].0, first_b[0].0);
    assert_ne!(first_a[0].1, first_b[0].1);
    assert_eq!(first_a[0].2, "first plan A");
    assert_eq!(first_b[0].2, "plan B");
    let submitted = agent.submit_started.notified();
    let operation = Uuid::new_v4();
    Runtime::change(
        &service,
        &records::Change {
            session: a.clone(),
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: decision::InputContent {
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: "next turn in A".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), submitted)
        .await
        .unwrap();
    let (same_native, next) = bound_execution(&service, &a);
    assert_eq!(same_native, native_a);
    assert_ne!(next, execution_a);
    let third = plan(&service, &same_native, next, "second plan A").await;
    let text_next = assistant(&service, &same_native, next, "A next: ", "done").await;
    let late = plan(&service, &native_a, execution_a, "stale first plan").await;
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native_a.clone(),
            change: SessionChange::Text {
                turn_id: format!("turn-{execution_a}").into(),
                item_id: "reused-message".into(),
                field: agent_protocol::session::TextField::AssistantText,
                delta: "STALE".into(),
            },
        },
        Some(execution_a),
    )
    .await
    .unwrap();

    agent.submit_gate.as_ref().unwrap().add_permits(1);
    completed(&service, std::slice::from_ref(&a)).await;
    assert!(
        third.is_ok(),
        "the next native turn reuses IDs without replacing history: {third:?}"
    );
    assert!(late.is_ok());
    assert!(text_next.is_ok());
    let final_messages = messages(&service, &a);
    assert_eq!(
        final_messages
            .iter()
            .map(|message| message.2.as_str())
            .collect::<Vec<_>>(),
        ["A: 日本語", "A next: done"]
    );
    assert_ne!(final_messages[0].0, final_messages[1].0);
    assert_ne!(final_messages[0].1, final_messages[1].1);
    let final_a = plans(&service, &a);
    assert_eq!(final_a.len(), 2);
    assert_eq!(
        final_a
            .iter()
            .map(|plan| plan.2.as_str())
            .collect::<Vec<_>>(),
        ["first plan A", "second plan A"]
    );
    assert_ne!(final_a[0].0, final_a[1].0);
    assert_ne!(final_a[0].1, final_a[1].1);
    assert_eq!(first_b, plans(&service, &b));
    assert_eq!(agent.inputs.lock().unwrap().len(), 3);
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .thread_state(&a.id, &[])
            .unwrap()
            .runs
            .len(),
        2
    );
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .thread_state(&b.id, &[])
            .unwrap()
            .runs
            .len(),
        1
    );
}

#[test]
fn strong_native_item_ids_keep_the_pinned_global_identity_and_match_text_updates() {
    for (provider, driver) in [
        (ProviderKind::Codex, "codex"),
        (ProviderKind::Claude, "claudeAgent"),
    ] {
        let context = Context {
            thread_id: "app".into(),
            run_id: Some("run".into()),
            root_node_id: "root".into(),
            provider_thread_id: "provider-thread".into(),
            provider_turn_id: "provider-turn".into(),
            native_turn_id: "native-turn".into(),
            native_thread_id: "native-thread".into(),
            provider,
            prompt_actor: model::Actor::User,
            now: "2026-11-14T00:00:00.000Z".into(),
            completed: false,
        };
        let item = agent_protocol::items::Item::new(
            "item".into(),
            ItemStatus::Running,
            ItemBody::AssistantText {
                text: "body".into(),
                phase: Default::default(),
            },
        );
        let artifact = context.item(&item).unwrap().remove(0);
        assert_eq!(
            artifact.node.id,
            format!("node:provider:{driver}:native-item:item")
        );
        assert_eq!(
            artifact.item.id,
            format!("turn-item:provider:{driver}:native-item:item")
        );
        let model::TurnItemBody::AssistantMessage { message_id, .. } = &artifact.item.body else {
            panic!("assistant")
        };
        assert_eq!(
            message_id,
            &format!("message:provider:{driver}:native-item:item")
        );
        assert_eq!(
            text_item_id(
                provider,
                "different-turn",
                "item",
                agent_protocol::session::TextField::AssistantText
            ),
            artifact.item.id
        );
        let reference = artifact.item.native_item_ref.unwrap();
        assert_eq!(reference.native_id.as_deref(), Some("item"));
        assert_eq!(reference.strength, model::NativeRefStrength::Strong);
    }
}

#[test]
fn native_tool_outputs_preserve_provider_names_and_canonical_content() {
    let context = Context {
        thread_id: "app".into(),
        run_id: Some("run".into()),
        root_node_id: "root".into(),
        provider_thread_id: "provider-thread".into(),
        provider_turn_id: "provider-turn".into(),
        native_turn_id: "native-turn".into(),
        native_thread_id: "native".into(),
        provider: ProviderKind::Codex,
        prompt_actor: model::Actor::User,
        now: "2026-11-14T00:00:00.000Z".into(),
        completed: true,
    };
    // codexDynamicToolOutput/projectCodexDynamicToolItem and
    // mcpToolResultOutput at pinned T3 1a12c4a define these values.
    let cases = [
        (
            json!({"type":"dynamicToolCall","tool":"exec","namespace":" plugin ","contentItems":[{"type":"inputImage","imageUrl":"data:IMAGE/PNG;base64,aW1hZ2U="}]}),
            "plugin.exec",
            json!([{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]),
        ),
        (
            json!({"type":"dynamicToolCall","tool":"exec","contentItems":[{"type":"inputText","text":"answer"},{"type":"future","data":7}]}),
            "exec",
            json!([{"type":"inputText","text":"answer"},{"type":"future","data":7}]),
        ),
        (
            json!({"type":"dynamicToolCall","tool":"exec","contentItems":[{"type":"inputImage","imageUrl":"https://example.invalid/image.png"}]}),
            "exec",
            json!([{"type":"inputImage","imageUrl":"https://example.invalid/image.png"}]),
        ),
        (
            json!({"type":"dynamicToolCall","tool":"exec","success":false}),
            "exec",
            json!({"success":false}),
        ),
        (
            json!({"type":"mcpToolCall","server":"fixture","tool":"lookup","result":{"content":[{"type":"text","text":"answer"}],"structuredContent":{}}}),
            "fixture.lookup",
            json!([{"type":"text","text":"answer"}]),
        ),
        (
            json!({"type":"mcpToolCall","server":"fixture","tool":"lookup","result":{"content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}],"structuredContent":{"value":7}}}),
            "fixture.lookup",
            json!({"content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}],"structuredContent":{"value":7}}),
        ),
        (
            json!({"type":"mcpToolCall","server":"fixture","tool":"lookup","result":{"content":[],"structuredContent":{"value":7}},"error":{"message":"failed"}}),
            "fixture.lookup",
            json!({"error":"failed","result":{"value":7}}),
        ),
    ];
    for (mut native, name, expected) in cases {
        native["id"] = json!("tool");
        native["status"] = json!("completed");
        let item = crate::providers::codex::native::parse_item(native).unwrap();
        let artifacts = context.item(&item).unwrap();
        assert!(
            matches!(&artifacts[0].item.body, model::TurnItemBody::DynamicTool {tool_name:Some(actual),output:Some(output),..} if actual==name && output==&expected),
            "expected {name}: {expected}; got {:?}",
            artifacts[0].item.body
        );
    }
    let metadata_image = serde_json::json!({"type":"mcpToolCall","id":"metadata-image","status":"completed","server":"fixture","tool":"lookup","result":{"content":[{"type":"image","mimeType":"image/png"}],"structuredContent":{"value":7}}});
    let item = crate::providers::codex::native::parse_item(metadata_image).unwrap();
    let artifacts = context.item(&item).unwrap();
    assert!(
        matches!(&artifacts[0].item.body,model::TurnItemBody::DynamicTool {output:Some(value),..} if value==&json!({"content":[{"type":"image","mimeType":"image/png"}],"structuredContent":{"value":7}})),
        "image metadata must survive alongside structured results before its bytes are hydrated"
    );
    let mut other = context;
    for (provider, kind, server, expected) in [
        (ProviderKind::Claude, "mcp", Some("a"), "mcp__a__b__tool"),
        (
            ProviderKind::Claude,
            "local",
            Some("Claude Code"),
            "b__tool",
        ),
        (ProviderKind::DeepSeek, "local", None, "b__tool"),
    ] {
        other.provider = provider;
        let item=serde_json::from_value(json!({"id":"other","status":"completed","body":{"inline":{"body":{"toolCall":{"kind":kind,"tool":"b__tool","server":server,"arguments":{},"result":7,"content":[]}}}}})).unwrap();
        let artifacts = other.item(&item).unwrap();
        assert!(
            matches!(&artifacts[0].item.body,model::TurnItemBody::DynamicTool {tool_name:Some(name),..} if name==expected),
            "each provider keeps its native name convention"
        );
    }
}
