use super::*;
use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
use agent_protocol::{
    protocol::{self, Call, Response},
    sync::Scope,
};

fn content() -> decision::InputContent {
    decision::InputContent {
        parts: vec![op::Input::Text {
            text: "run once".into(),
        }],
        model: None,
        effort: Some("high".into()),
        service_tier: Some("priority".into()),
        follow_up: Default::default(),
    }
}

#[tokio::test]
async fn child_discovery_uses_the_public_parent_and_preserves_already_received_work() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        let parent = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let _: records::Opened = call(
            &service,
            connection.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                provider: ProviderKind::Codex,
                session: parent.clone(),
                subscription: Uuid::new_v4(),
                cwd: Some(directory.path().to_str().unwrap().into()),
                selection: Default::default(),
                initial: Some((Uuid::new_v4().to_string().into(), content())),
            }),
        )
        .await
        .unwrap();
        completed(&service, &parent, 1).await;
        let native_parent = service
            .inner
            .conversation
            .storage
            .active_source(&parent)
            .unwrap()
            .unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child-native".into()).unwrap();
        let discovered = || AgentChange::TaskParent {
            session: child.clone(),
            parent: native_parent.clone(),
        };
        Runtime::observe(&service, discovered(), None)
            .await
            .unwrap();
        let execution = Uuid::new_v4();
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: child.clone(),
                change: SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "child-turn".into(),
                        status: agent_protocol::models::TurnStatus::Running,
                        ..Default::default()
                    },
                    completed: false,
                },
            },
            Some(execution),
        )
        .await
        .unwrap();
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: child.clone(),
                change: SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "child-turn".into(),
                        status: agent_protocol::models::TurnStatus::Completed,
                        items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                            "child-answer".into(),
                            agent_protocol::models::ItemStatus::Completed,
                            agent_protocol::items::ItemBody::AssistantText {
                                text: "Child result".into(),
                                phase: agent_protocol::items::AssistantPhase::Final,
                            },
                        ))]),
                        ..Default::default()
                    },
                    completed: true,
                },
            },
            Some(execution),
        )
        .await
        .unwrap();
        let before = service
            .inner
            .conversation
            .storage
            .history(&child, 0)
            .unwrap();
        assert_eq!(before.thread.parent_id, Some(parent));
        assert_eq!(before.thread.can_accept_direct_input, Some(false));
        assert_eq!(before.thread.turns.as_ref().unwrap().len(), 1);
        assert_eq!(
            before.thread.turns.as_ref().unwrap()[0]
                .items
                .as_ref()
                .unwrap()
                .len(),
            1
        );
        Runtime::observe(&service, discovered(), None)
            .await
            .unwrap();
        let after = service
            .inner
            .conversation
            .storage
            .history(&child, 0)
            .unwrap();
        assert_eq!(after.thread.turns, before.thread.turns);
        assert_eq!(after.thread.parent_id, before.thread.parent_id);
    })
    .await
    .expect("child observation commits before following native work");
}

#[tokio::test]
async fn returning_to_a_provider_reuses_its_context_and_injects_only_unseen_history() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for mode in [
            "reuse",
            "compacted",
            "import_incomplete",
            "busy",
            "state_error",
            "injection_unknown",
        ] {
            let compacted = mode == "compacted";
            let directory = tempfile::tempdir().unwrap();
            let mut codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
            Arc::get_mut(&mut codex).unwrap().metadata_only_state = compacted;
            let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
            let service = HostRpcService::new(
                [codex.backend(), claude.backend()],
                directory.path().join("projects.json"),
            );
            let connection = service.open_session();
            let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
            let input = |provider, text: &str| decision::InputContent {
                parts: vec![op::Input::Text { text: text.into() }],
                model: Some(agent_protocol::models::ModelRef {
                    provider,
                    id: "fixture".into(),
                }),
                ..content()
            };
            let _: records::Opened = call(
                &service,
                connection.id(),
                Call::CreateConversation(records::Create {
                    workspace: None,
                    provider: ProviderKind::Codex,
                    session: target.clone(),
                    subscription: Uuid::new_v4(),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    selection: Default::default(),
                    initial: Some((
                        Uuid::new_v4().to_string().into(),
                        input(ProviderKind::Codex, "first-provider-marker"),
                    )),
                }),
            )
            .await
            .unwrap();
            completed(&service, &target, 1).await;
            let original = service
                .inner
                .conversation
                .storage
                .active_source(&target)
                .unwrap()
                .unwrap();
            if mode == "import_incomplete" {
                codex
                    .records
                    .lock()
                    .unwrap()
                    .get_mut(&original.id)
                    .unwrap()
                    .thread
                    .history_read_state = Some(agent_protocol::session::HistoryReadState::new(
                    agent_protocol::session::HistoryReadKind::Incomplete,
                    vec!["compaction boundary".into()],
                ));
                reimported(
                    &service,
                    &target,
                    agent_protocol::session::HistoryReadKind::Incomplete,
                )
                .await;
            }
            for (provider, text, count) in [
                (ProviderKind::Claude, "second-provider-marker", 2),
                (ProviderKind::Codex, "third-provider-marker", 3),
            ] {
                let before = codex.next.load(std::sync::atomic::Ordering::SeqCst);
                if provider == ProviderKind::Codex {
                    match mode {
                        "busy" => {
                            codex
                                .records
                                .lock()
                                .unwrap()
                                .get_mut(&original.id)
                                .unwrap()
                                .thread
                                .status = agent_protocol::models::SessionStatus::Running
                        }
                        "state_error" => {
                            *codex.state_error.lock().unwrap() =
                                Some(Failure::unknown("resume_unavailable", "lost native state"))
                        }
                        "injection_unknown" => codex
                            .fail_inject
                            .store(1, std::sync::atomic::Ordering::SeqCst),
                        _ => {}
                    }
                }
                let operation = Uuid::new_v4();
                let acknowledged: records::Acknowledged = call(
                    &service,
                    connection.id(),
                    Call::ChangeConversation(records::Change {
                        session: target.clone(),
                        operation,
                        command: Command::Submit {
                            id: operation.to_string().into(),
                            content: input(provider, text),
                        },
                    }),
                )
                .await
                .unwrap();
                assert_eq!(acknowledged.outcome, Outcome::Accepted);
                if provider == ProviderKind::Codex && matches!(mode, "busy" | "injection_unknown") {
                    let storage = &service.inner.conversation.storage;
                    let mut changes = storage.changes();
                    tokio::time::timeout(std::time::Duration::from_secs(2), async {
                        loop {
                            if let Some(input) = storage
                                .input(&target, &operation.to_string().into())
                                .unwrap()
                                && let decision::InputState::Failed {
                                    error: Some(error), ..
                                } = input.state
                            {
                                assert_eq!(
                                    error.message,
                                    if mode == "busy" {
                                        "切り替え先のAIがまだ実行中です。完了を待ってください。"
                                    } else {
                                        "connection lost after writing history"
                                    }
                                );
                                break;
                            }
                            changes.changed().await.unwrap();
                        }
                    })
                    .await
                    .unwrap();
                    assert_eq!(codex.next.load(std::sync::atomic::Ordering::SeqCst), before);
                    assert_eq!(
                        codex.inputs.lock().unwrap().len(),
                        1,
                        "failed preparation must not send the input"
                    );
                    codex
                        .records
                        .lock()
                        .unwrap()
                        .get_mut(&original.id)
                        .unwrap()
                        .thread
                        .status = agent_protocol::models::SessionStatus::Idle;
                    let retried: records::Acknowledged = call(
                        &service,
                        connection.id(),
                        Call::ChangeConversation(records::Change {
                            session: target.clone(),
                            operation: Uuid::new_v4(),
                            command: Command::RetryInput {
                                id: operation.to_string().into(),
                                work_locally: false,
                            },
                        }),
                    )
                    .await
                    .unwrap();
                    assert_eq!(retried.outcome, Outcome::Accepted);
                }
                completed(&service, &target, count).await;
            }
            let active = service
                .inner
                .conversation
                .storage
                .active_source(&target)
                .unwrap()
                .unwrap();
            let rebuilt = matches!(
                mode,
                "state_error" | "injection_unknown" | "import_incomplete"
            );
            assert_eq!(active == original, !rebuilt, "{mode}");
            assert_eq!(
                codex.records.lock().unwrap().len(),
                if rebuilt { 2 } else { 1 }
            );
            assert_eq!(
                codex.inputs.lock().unwrap().len(),
                2,
                "each user input is delivered exactly once: {mode}"
            );
            let injected = codex.injected.lock().unwrap();
            assert_eq!(
                injected.len(),
                if mode == "injection_unknown" { 2 } else { 1 }
            );
            if mode == "injection_unknown" {
                assert_ne!(injected[0].0, injected[1].0);
            }
            assert_eq!(injected.last().unwrap().0, active.id);
            let context = injected.last().unwrap().1.render();
            assert!(context.contains("second-provider-marker"));
            assert_eq!(context.contains("first-provider-marker"), rebuilt);
            assert!(!context.contains("third-provider-marker"));
            assert!(codex.released.lock().unwrap().contains(&original.id));
            assert!(!claude.released.lock().unwrap().is_empty());
        }
    })
    .await
    .expect("provider round trip settles");
}

#[tokio::test]
async fn compact_keeps_inline_provider_handoff_until_the_next_ordinary_input() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        let service = HostRpcService::new([codex.backend(), claude.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let text = |text: &str| decision::InputContent { parts: vec![op::Input::Text { text: text.into() }], ..content() };
        let _: records::Opened = call(&service, connection.id(), Call::CreateConversation(records::Create {
            workspace: None, provider: ProviderKind::Codex, session: target.clone(), subscription: Uuid::new_v4(), cwd: Some(directory.path().to_str().unwrap().into()), selection: Default::default(), initial: Some((Uuid::new_v4().to_string().into(), text("first-provider-marker"))),
        })).await.unwrap();
        completed(&service, &target, 1).await;
        for (count, message) in [(2, "/compact"), (3, "ordinary-input"), (4, "later-input")] {
            let operation = Uuid::new_v4();
            let mut input = text(message);
            input.model = Some(agent_protocol::models::ModelRef { provider: ProviderKind::Claude, id: "fixture".into() });
            let receipt: records::Acknowledged = call(&service, connection.id(), Call::ChangeConversation(records::Change { session: target.clone(), operation, command: Command::Submit { id: operation.to_string().into(), content: input } })).await.unwrap();
            assert_eq!(receipt.outcome, Outcome::Accepted);
            completed(&service, &target, count).await;
        }
        let inputs = claude.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].input, vec![op::Input::Text { text: "/compact".into() }]);
        assert!(matches!(&inputs[1].input[0], op::Input::Text { text } if text.contains("first-provider-marker")));
        assert!(matches!(&inputs[1].input[1], op::Input::Text { text } if text == "ordinary-input"));
        assert_eq!(inputs[2].input, vec![op::Input::Text { text: "later-input".into() }]);
    }).await.expect("inline handoff settles after compact");
}

#[tokio::test]
async fn missing_providerless_conversations_keep_durable_rejections_without_a_provider() {
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let request = records::Change {
        session: target.clone(),
        operation,
        command: Command::Rename("missing".into()),
    };
    let first: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(request.clone()),
    )
    .await
    .unwrap();
    assert!(matches!(first.outcome, Outcome::Rejected(_)));
    let replay: records::Acknowledged =
        call(&service, connection.id(), Call::ChangeConversation(request))
            .await
            .unwrap();
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(replay.cursor, first.cursor);
    let receipts: records::Receipts = call(
        &service,
        connection.id(),
        Call::CheckConversationCommands(records::CheckCommands {
            session: target,
            operations: vec![operation],
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].1.outcome, first.outcome);
    assert!(
        service
            .inner
            .conversation
            .storage
            .identities()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn expanded_commands_stream_bounded_tails_until_completion() {
    command_output_case("completed").await;
}
#[tokio::test]
async fn expanded_commands_stream_bounded_tails_until_provider_stop() {
    command_output_case("stopped").await;
}

#[tokio::test]
async fn background_command_completion_and_completion_while_stop_is_pending_save_the_tail() {
    for case in [
        "background",
        "background-stop",
        "background-failed",
        "background-interrupted",
        "foreground-stop",
    ] {
        command_output_case(case).await;
    }
}

#[tokio::test]
async fn resolving_unknown_delivery_saves_live_output_and_ends_its_subscription() {
    command_output_case("fenced").await;
}
#[tokio::test]
async fn a_short_pretruncated_command_keeps_its_omission_notice_when_settled() {
    command_output_case("pretruncated").await;
}

#[tokio::test]
async fn a_complete_short_command_does_not_claim_earlier_output_was_omitted() {
    command_output_case("short").await;
}
#[tokio::test]
async fn a_late_output_subscription_cannot_erase_a_completed_items_output() {
    command_output_case("orphan").await;
}

async fn command_output_case(case: &str) {
    let stopped = case == "stopped";
    let short = matches!(case, "short" | "pretruncated");
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{Item, ItemBody},
        sync::OutputUpdate,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((Uuid::new_v4().to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    let native = service
        .inner
        .conversation
        .storage
        .active_source(&target)
        .unwrap()
        .unwrap();
    let execution = Uuid::new_v4();
    let send = |change| {
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change,
            },
            Some(execution),
        )
    };
    send(SessionChange::Turn {
        turn: agent_protocol::models::Turn {
            id: "tools".into(),
            status: TurnStatus::Running,
            ..Default::default()
        },
        completed: false,
    })
    .await
    .unwrap();
    send(SessionChange::Item {
        turn_id: "tools".into(),
        item: Arc::new(Item::new(
            "command".into(),
            ItemStatus::Running,
            ItemBody::CommandExecution {
                command: "echo output".into(),
                cwd: None,
                output: "initial".into(),
                output_omitted: case == "pretruncated",
                exit_code: None,
            },
        )),
    })
    .await
    .unwrap();
    let key = records::OutputKey {
        session: target.clone(),
        item: decision::provider_item_id(execution, "command"),
    };
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    let reply = service
        .dispatch(
            connection.id(),
            &Call::SubscribeCommandOutput(records::SubscribeOutput {
                key: key.clone(),
                subscription: Uuid::new_v4(),
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: opened } =
        protocol::decode::<Response<records::OpenedOutput>>(&reply.initial).unwrap()
    else {
        panic!("output subscription failed")
    };
    assert_eq!(opened.initial.text, "initial");
    let mut updates = reply.updates.unwrap();
    let before = service
        .inner
        .conversation
        .storage
        .snapshot(&target, None, None)
        .unwrap();
    send(SessionChange::Text {
        turn_id: "tools".into(),
        item_id: "command".into(),
        field: agent_protocol::session::TextField::CommandOutput,
        delta: if short {
            "short tail".into()
        } else {
            "日".repeat(100_000)
        },
    })
    .await
    .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(99)).await;
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, updates.recv())
            .await
            .is_err()
    );
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    let frame = updates.recv().await.unwrap();
    let update: OutputUpdate = protocol::decode(&frame).unwrap();
    assert_eq!(
        matches!(update, OutputUpdate::Tail(_)),
        !short,
        "only an overflowing pending append requires a full tail"
    );
    let tail = opened.initial.apply(update);
    assert_eq!(tail.truncated, case != "short");
    assert!(tail.text.encode_utf16().count() <= 64 * 1024);
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .snapshot(&target, None, None)
            .unwrap(),
        before,
        "live bytes never advance row versions or the resumable cursor"
    );
    let late: records::OpenedOutput = call(
        &service,
        connection.id(),
        Call::SubscribeCommandOutput(records::SubscribeOutput {
            key: key.clone(),
            subscription: Uuid::new_v4(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(late.initial, tail);
    let background = case.starts_with("background");
    if case == "orphan" {
        send(SessionChange::Item {
            turn_id: "tools".into(),
            item: Arc::new(Item::new(
                "command".into(),
                ItemStatus::Completed,
                ItemBody::CommandExecution {
                    command: "echo output".into(),
                    cwd: None,
                    output: tail.text.clone(),
                    output_omitted: tail.truncated,
                    exit_code: Some(0),
                },
            )),
        })
        .await
        .unwrap();
        // A subscriber that read Running before the item committed can still
        // seed this empty stream after native_item removed the previous tail.
        let _late = service
            .inner
            .conversation
            .outputs
            .subscribe(&key, Some((execution, Default::default())))
            .unwrap();
    }
    if background || case == "foreground-stop" {
        send(SessionChange::Background {
            tasks: vec![agent_protocol::execution::BackgroundTask {
                id: "background-command".into(),
                kind: agent_protocol::execution::BackgroundTaskKind::Command,
                description: Some("echo output".into()),
            }],
        })
        .await
        .unwrap();
        if case == "foreground-stop" {
            let runtime = &service.inner.conversation;
            let operation = Uuid::new_v4();
            let queued = operation.to_string().into();
            let admitted = runtime
                .storage
                .decide(
                    &target,
                    Event::Command {
                        id: decision::OperationId::Client(operation),
                        receipt: None,
                        command: Command::Submit {
                            id: queued,
                            content: content(),
                        },
                    },
                    Runtime::context(&service, Some(ProviderKind::Codex)),
                    None,
                )
                .unwrap();
            assert_eq!(admitted.step.outcome, Outcome::Accepted);
            let state = runtime.storage.state(&target).unwrap();
            runtime
                .storage
                .decide(
                    &target,
                    Event::Command {
                        id: decision::OperationId::Client(Uuid::new_v4()),
                        receipt: None,
                        command: Command::Interrupt {
                            turn: state.active_turn().map(|turn| turn.id.clone()),
                        },
                    },
                    Runtime::context(&service, Some(ProviderKind::Codex)),
                    None,
                )
                .unwrap();
            send(SessionChange::Background { tasks: Vec::new() })
                .await
                .unwrap();
            let state = runtime.storage.state(&target).unwrap();
            assert!(matches!(
                state.active_turn().map(|turn| &turn.state),
                Some(decision::TurnState::Running { interrupt: Some(_) })
            ));
            assert_eq!(
                state
                    .inputs
                    .values()
                    .filter(|input| matches!(input.state, decision::InputState::Queued { .. }))
                    .count(),
                1
            );
        }
        let status = match case {
            "background-failed" => TurnStatus::Failed,
            "background-interrupted" | "foreground-stop" => TurnStatus::Interrupted,
            _ => TurnStatus::Completed,
        };
        send(SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "tools".into(),
                status,
                ..Default::default()
            },
            completed: true,
        })
        .await
        .unwrap();
        if case == "background-stop" {
            let state = service.inner.conversation.storage.state(&target).unwrap();
            service
                .inner
                .conversation
                .storage
                .decide(
                    &target,
                    Event::Command {
                        id: decision::OperationId::Client(Uuid::new_v4()),
                        receipt: None,
                        command: Command::Interrupt {
                            turn: state.active_turn().map(|turn| turn.id.clone()),
                        },
                    },
                    Runtime::context(&service, Some(ProviderKind::Codex)),
                    None,
                )
                .unwrap();
        }
        if matches!(case, "background" | "background-stop") {
            send(SessionChange::Background { tasks: Vec::new() })
                .await
                .unwrap();
        }
        let state = service.inner.conversation.storage.state(&target).unwrap();
        assert!(state.active_turn().is_none());
        if case == "foreground-stop" {
            assert!(state.thread.queue_held);
        }
    } else if case == "fenced" {
        let storage = &service.inner.conversation.storage;
        let context = Runtime::context(&service, Some(ProviderKind::Codex));
        let input = Uuid::new_v4();
        let mut content = content();
        content.follow_up = op::FollowUp::Steer;
        let pending = storage
            .decide(
                &target,
                Event::Command {
                    id: decision::OperationId::Client(input),
                    receipt: None,
                    command: Command::Submit {
                        id: input.to_string().into(),
                        content,
                    },
                },
                context,
                None,
            )
            .unwrap();
        let Action::Deliver { id, action, .. } = &pending.step.actions[0] else {
            panic!("steer delivery");
        };
        storage
            .decide(
                &target,
                Event::DeliveryFailed {
                    id: id.clone(),
                    action: *action,
                    unknown: true,
                    error: Default::default(),
                },
                context,
                None,
            )
            .unwrap();
        send(SessionChange::Item {
            turn_id: "tools".into(),
            item: Arc::new(Item::new(
                "last-text".into(),
                ItemStatus::Running,
                ItemBody::AssistantText {
                    text: String::new(),
                    phase: Default::default(),
                },
            )),
        })
        .await
        .unwrap();
        send(SessionChange::Text {
            turn_id: "tools".into(),
            item_id: "last-text".into(),
            field: agent_protocol::session::TextField::AssistantText,
            delta: "last buffered text".into(),
        })
        .await
        .unwrap();
        let resolved = storage
            .decide(
                &target,
                Event::Command {
                    id: decision::OperationId::Client(Uuid::new_v4()),
                    receipt: None,
                    command: Command::ResolveUnknown {
                        id: id.clone(),
                        retry: false,
                    },
                },
                context,
                None,
            )
            .unwrap();
        for action in resolved.step.actions {
            Runtime::execute(
                &service,
                Effect {
                    target: target.clone(),
                    action,
                },
            )
            .await
            .unwrap();
        }
        let text = storage
            .item(&target, &decision::provider_item_id(execution, "last-text"))
            .unwrap()
            .unwrap();
        assert!(
            matches!(text.body, ItemBody::AssistantText { text, .. } if text == "last buffered text")
        );
    } else if stopped {
        Runtime::observe(
            &service,
            AgentChange::Stopped {
                provider: ProviderKind::Codex,
                error: agent_protocol::execution::ExecutionError {
                    message: "provider stopped".into(),
                    category: agent_protocol::execution::ErrorCategory::Network,
                    ..Default::default()
                },
            },
            None,
        )
        .await
        .unwrap();
    } else {
        send(SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "tools".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        })
        .await
        .unwrap();
    }
    let stored = service
        .inner
        .conversation
        .storage
        .item(&target, &key.item)
        .unwrap()
        .unwrap();
    let ItemBody::CommandExecution {
        output,
        output_omitted,
        ..
    } = stored.body
    else {
        panic!("command missing")
    };
    assert_eq!(output, tail.text);
    assert_eq!(output_omitted, case != "short");
    assert_eq!(
        stored.status,
        match case {
            "stopped" | "background-failed" => ItemStatus::Failed,
            "background-interrupted" | "foreground-stop" | "fenced" => ItemStatus::Interrupted,
            _ => ItemStatus::Completed,
        }
    );
    assert!(updates.recv().await.is_none());
    tokio::time::resume();
}

#[tokio::test]
async fn native_reply_before_submit_ack_commits_one_turn_and_replay_never_sends_again() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let request = records::Create {
        workspace: None,
        provider: agent_protocol::session::ProviderKind::Codex,
        subscription: Uuid::new_v4(),
        session: target.clone(),
        cwd: Some(directory.path().to_str().unwrap().into()),
        selection: Default::default(),
        initial: Some((operation.to_string().into(), content())),
    };
    let reply = service
        .dispatch(connection.id(), &Call::CreateConversation(request))
        .await
        .unwrap();
    let Response::Success { result: opened } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!(
            "create failed: {:?}",
            protocol::decode::<serde_json::Value>(&reply.initial)
        );
    };
    let mut replica = records::Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, opened.initial)
        .unwrap();
    let mut stream = reply.updates.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let projected = records::project(&replica).unwrap();
            if projected.thread.turns.as_ref().is_some_and(|turns| {
                turns.len() == 1
                    && turns[0].status == agent_protocol::execution::TurnStatus::Completed
            }) {
                break;
            }
            let frame = stream.recv().await.unwrap();
            replica = replica.apply(1, protocol::decode(&frame).unwrap()).unwrap();
        }
    })
    .await
    .unwrap();
    let acknowledged: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: content(),
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(acknowledged.outcome, Outcome::Accepted);
    let stored = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap();
    let turns = stored.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.as_ref().unwrap().len(), 2);
    assert_eq!(
        turns[0].items.as_ref().unwrap()[0].id.as_str(),
        operation.to_string()
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_eq!(stored.effort.as_deref(), Some("high"));
    assert_eq!(stored.service_tier.as_deref(), Some("priority"));
}

async fn completed(
    service: &HostRpcService,
    target: &SessionRef,
    count: usize,
) -> agent_protocol::models::ThreadResponse {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let history = service
                .inner
                .conversation
                .storage
                .history(target, 0)
                .unwrap();
            if history.thread.turns.as_ref().is_some_and(|turns| {
                turns.len() == count
                    && turns.last().unwrap().status
                        == agent_protocol::execution::TurnStatus::Completed
            }) {
                return history;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn reimported(
    service: &HostRpcService,
    target: &SessionRef,
    kind: agent_protocol::session::HistoryReadKind,
) {
    let storage = &service.inner.conversation.storage;
    let source = storage.active_source(target).unwrap().unwrap();
    let mut changed = storage.changes();
    storage.queue_import(target, &source, now()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while storage
            .header(target)
            .unwrap()
            .details
            .history_read_state
            .as_ref()
            .is_none_or(|read| read.kind != kind)
        {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn sibling_forks_continue_the_selected_native_prefix_in_independent_resources() {
    use agent_protocol::session::{HistoryReadKind, HistoryReadState};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let submit = |session: SessionRef| {
        let operation = Uuid::new_v4();
        Call::ChangeConversation(records::Change {
            session,
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: content(),
            },
        })
    };
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let first = Uuid::new_v4();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((first.to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    let ack: records::Acknowledged = call(&service, connection.id(), submit(target.clone()))
        .await
        .unwrap();
    ack.accepted().unwrap();
    completed(&service, &target, 2).await;
    let storage = &service.inner.conversation.storage;
    let source = storage.active_source(&target).unwrap().unwrap();
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&source.id)
        .unwrap()
        .thread
        .history_read_state = Some(HistoryReadState::new(
        HistoryReadKind::Incomplete,
        vec!["compaction boundary".into()],
    ));
    reimported(&service, &target, HistoryReadKind::Incomplete).await;
    let original = storage.history(&target, 0).unwrap();
    let through = original.thread.turns.as_ref().unwrap()[0].id.clone();
    let mut sources = std::collections::HashSet::new();
    for _ in 0..2 {
        let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let request = records::Fork {
            source: target.clone(),
            target: fork.clone(),
            through: through.clone(),
        };
        for _ in 0..2 {
            let accepted: SessionRef = call(
                &service,
                connection.id(),
                Call::ForkConversation(request.clone()),
            )
            .await
            .unwrap();
            assert_eq!(accepted, fork);
        }
        let ack: records::Acknowledged = call(&service, connection.id(), submit(fork.clone()))
            .await
            .unwrap();
        assert_eq!(ack.outcome, Outcome::Accepted);
        let continued = completed(&service, &fork, 2).await;
        assert_eq!(
            continued.thread.turns.as_ref().unwrap()[0],
            original.thread.turns.as_ref().unwrap()[0]
        );
        let native = service
            .inner
            .conversation
            .storage
            .active_source(&fork)
            .unwrap()
            .unwrap();
        assert!(sources.insert(native.clone()));
        let saved = agent.records.lock().unwrap()[&native.id].clone();
        assert_eq!(
            saved.thread.turns.unwrap().len(),
            2,
            "native fork retains the selected tool context before continuing"
        );
    }
    assert_eq!(agent.inputs.lock().unwrap().len(), 4);
    assert_eq!(
        completed(&service, &target, 2).await.thread.turns,
        original.thread.turns
    );
    let through = original.thread.turns.as_ref().unwrap()[1].id.clone();
    let rewound: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Rewind(through),
        }),
    )
    .await
    .unwrap();
    rewound.accepted().unwrap();
    let ack: records::Acknowledged = call(&service, connection.id(), submit(target.clone()))
        .await
        .unwrap();
    ack.accepted().unwrap();
    let resumed = completed(&service, &target, 2).await;
    assert_eq!(
        resumed.thread.turns.as_ref().unwrap()[0],
        original.thread.turns.as_ref().unwrap()[0]
    );
    assert!(
        agent.injected.lock().unwrap().is_empty(),
        "native forks preserve their context without portable replay"
    );
}

#[tokio::test(start_paused = true)]
async fn text_streams_commit_at_paragraphs_and_within_the_streaming_interval() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((operation.to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    let initial = completed(&service, &target, 1).await;
    let native = service
        .inner
        .conversation
        .storage
        .active_source(&target)
        .unwrap()
        .unwrap();
    let attempt = service
        .inner
        .conversation
        .storage
        .state(&target)
        .unwrap()
        .attempts
        .into_values()
        .next()
        .unwrap();
    let text = |delta: &str| AgentChange::Session {
        session: native.clone(),
        change: SessionChange::Text {
            turn_id: "turn-0".into(),
            item_id: "answer-0".into(),
            field: agent_protocol::session::TextField::AssistantText,
            delta: delta.into(),
        },
    };
    for delta in [" 一", "文"] {
        Runtime::observe(&service, text(delta), Some(attempt.id))
            .await
            .unwrap();
    }
    let unchanged = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap();
    assert_eq!(
        unchanged.thread.turns, initial.thread.turns,
        "partial chunks do not each rewrite the database"
    );
    Runtime::observe(&service, text("。\n\n"), Some(attempt.id))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = service.inner.conversation.storage.history(&target, 0).unwrap();
            if matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with(" 一文。\n\n")) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let paragraph = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap()
        .thread
        .turns;
    Runtime::observe(&service, text("unfinished suffix"), Some(attempt.id))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(799)).await;
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .history(&target, 0)
            .unwrap()
            .thread
            .turns,
        paragraph
    );
    Runtime::observe(&service, text(" more"), Some(attempt.id))
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .history(&target, 0)
            .unwrap()
            .thread
            .turns,
        paragraph,
        "new text resets the idle deadline"
    );
    tokio::time::advance(std::time::Duration::from_millis(799)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = service.inner.conversation.storage.history(&target, 0).unwrap();
            if matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with("unfinished suffix more")) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let partial = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap()
        .thread
        .turns;
    for delta in [" paragraph\n", "\n"] {
        Runtime::observe(&service, text(delta), Some(attempt.id))
            .await
            .unwrap();
    }
    tokio::time::advance(std::time::Duration::from_millis(399)).await;
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .history(&target, 0)
            .unwrap()
            .thread
            .turns,
        partial,
        "a paragraph does not bypass the minimum commit interval"
    );
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    use agent_protocol::{
        execution::ItemStatus,
        items::ItemBody,
        models::Item,
        session::{ReasoningField, TextField},
    };
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Item {
                turn_id: "turn-0".into(),
                item: Arc::new(Item::new(
                    "reason-0".into(),
                    ItemStatus::Running,
                    ItemBody::Reasoning {
                        content: Vec::new(),
                        summary: Vec::new(),
                    },
                )),
            },
        },
        Some(attempt.id),
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::ReasoningPart {
                turn_id: "turn-0".into(),
                item_id: "reason-0".into(),
                field: ReasoningField::Summary,
                index: 0,
            },
        },
        Some(attempt.id),
    )
    .await
    .unwrap();
    for delta in ["", "A\n\nB"] {
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Text {
                    turn_id: "turn-0".into(),
                    item_id: "reason-0".into(),
                    field: TextField::ReasoningSummary { index: 0 },
                    delta: delta.into(),
                },
            },
            Some(attempt.id),
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;
    }
    let mut changes = service.inner.conversation.storage.changes();
    tokio::time::advance(std::time::Duration::from_millis(800)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = service.inner.conversation.storage.history(&target, 0).unwrap();
            if saved.thread.turns.iter().flatten().flat_map(|turn| turn.items.iter().flatten())
                .any(|item| matches!(item.body(), ItemBody::Reasoning { summary, .. } if summary == &["A\n\nB"])) { break; }
            changes.changed().await.unwrap();
        }
    }).await.expect("reasoning must stream after an empty part, before completion");
    Runtime::observe(&service, text(" final pending"), Some(attempt.id))
        .await
        .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "turn-0".into(),
                    status: agent_protocol::execution::TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(attempt.id),
    )
    .await
    .unwrap();
    let saved = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap();
    assert!(
        matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with(" paragraph\n\n final pending")),
        "completion flushes the last partial body"
    );
}

#[tokio::test]
async fn an_update_restart_continues_each_interrupted_root_once_in_its_original_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let projects = directory.path().join("projects.json");
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], projects.clone());
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((operation.to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    let native = service
        .inner
        .conversation
        .storage
        .active_source(&target)
        .unwrap()
        .unwrap();
    let execution = Uuid::new_v4();
    let ongoing = agent_protocol::models::Turn {
        id: "ongoing".into(),
        status: agent_protocol::execution::TurnStatus::Running,
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&native.id)
        .unwrap()
        .thread
        .turns
        .as_mut()
        .unwrap()
        .push(Arc::new(ongoing.clone()));
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native,
            change: SessionChange::Turn {
                turn: ongoing,
                completed: false,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    service
        .inner
        .conversation
        .storage
        .set_updating(true)
        .unwrap();
    let owner = Arc::downgrade(&service.inner);
    drop(connection);
    drop(service);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while owner.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let restarted = agent.restarted();
    let service = HostRpcService::new([restarted.backend()], projects);
    service.start().unwrap();
    let history = completed(&service, &target, 3).await;
    assert_eq!(
        service.inner.conversation.storage.identities().unwrap(),
        vec![target.clone()]
    );
    assert_eq!(history.effort.as_deref(), Some("high"));
    assert_eq!(history.service_tier.as_deref(), Some("priority"));
    let turns = history.thread.turns.unwrap();
    assert_eq!(
        turns[1].status,
        agent_protocol::execution::TurnStatus::Interrupted
    );
    assert_eq!(restarted.inputs.lock().unwrap().len(), 2);
    let text = restarted.inputs.lock().unwrap()[1].input.clone();
    assert!(matches!(&text[..], [op::Input::Text { text }] if text == super::recovery::CONTINUE));
    for _ in 0..3 {
        Runtime::recover(&service, &target, ProviderKind::Codex)
            .await
            .unwrap();
    }
    assert_eq!(
        restarted.inputs.lock().unwrap().len(),
        2,
        "replayed recovery never repeats the user request or creates another conversation"
    );
}

#[tokio::test]
async fn explicit_native_imports_append_missing_text_and_keep_settled_messages() {
    use agent_protocol::{
        execution::ItemStatus,
        items::{Item, ItemBody},
        models::{Turn, TurnStatus},
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((Uuid::new_v4().to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    let original = completed(&service, &target, 1).await;
    let native = service
        .inner
        .conversation
        .storage
        .active_source(&target)
        .unwrap()
        .unwrap();
    let external = |text: &str| Turn {
        id: "external".into(),
        status: TurnStatus::Completed,
        items: Some(vec![
            Arc::new(Item::new(
                "external-answer".into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: text.into(),
                    phase: Default::default(),
                },
            )),
            Arc::new(Item::new(
                "external-tool".into(),
                ItemStatus::Completed,
                ItemBody::CommandExecution {
                    output_omitted: false,
                    command: "private tool details".into(),
                    cwd: None,
                    output: "not imported".into(),
                    exit_code: Some(0),
                },
            )),
        ]),
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&native.id)
        .unwrap()
        .thread
        .turns
        .as_mut()
        .unwrap()
        .push(Arc::new(external("external answer")));
    let storage = &service.inner.conversation.storage;
    for _ in 0..2 {
        let imported: agent_protocol::project_import::ProjectImportResult = call(
            &service,
            connection.id(),
            Call::ImportProjects(agent_protocol::project_import::ImportProjects {
                paths: vec![directory.path().to_str().unwrap().into()],
            }),
        )
        .await
        .unwrap();
        assert_eq!(imported.imported.len(), 1);
    }
    let imported = completed(&service, &target, 2).await;
    let turns = imported.thread.turns.as_ref().unwrap();
    assert_eq!(turns[0], original.thread.turns.as_ref().unwrap()[0]);
    assert_eq!(
        turns[1].items.as_ref().unwrap().len(),
        1,
        "explicit imports copy conversation text only"
    );
    let id = turns[1].id.clone();
    let item = turns[1].items.as_ref().unwrap()[0].id.clone();
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&native.id)
        .unwrap()
        .thread
        .turns
        .as_mut()
        .unwrap()[1] = Arc::new(external("updated answer"));
    let version = |message: records::Message| {
        let agent_protocol::sync::Message::Snapshot { cursor, .. } = message else {
            panic!("snapshot")
        };
        cursor.sequence
    };
    let before = version(storage.snapshot(&target, None, None).unwrap());
    storage.queue_import(&target, &native, now()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let history = storage.history(&target, 0).unwrap();
            let turns = history.thread.turns.unwrap();
            // Observe the committed import event before asserting immutability.
            if version(storage.snapshot(&target, None, None).unwrap()) > before {
                assert!(matches!(turns.last().unwrap().items.as_ref().unwrap()[0].body(), ItemBody::AssistantText { text, .. } if text == "external answer"));
                assert_eq!(turns.len(), 2);
                assert_eq!(turns[1].id, id);
                assert_eq!(turns[1].items.as_ref().unwrap()[0].id, item);
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(
        agent.inputs.lock().unwrap().len(),
        1,
        "importing never sends an input"
    );
}

#[tokio::test]
async fn registering_a_project_updates_open_conversations_and_the_shared_list_after_restart() {
    use agent_protocol::sync::{Content, Key};
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("projects.json");
    let root = directory.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let cwd = dunce::canonicalize(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let service = HostRpcService::new([], &state_file);
    let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    service
        .inner
        .conversation
        .storage
        .create(
            &id,
            &records::Header {
                state: decision::Thread {
                    name: "Already open".into(),
                    workspace: decision::Workspace::Ready(cwd.clone()),
                    provider: ProviderKind::Codex,
                    queue_held: false,
                },
                details: Default::default(),
            },
        )
        .unwrap();
    let connection = service.open_session();
    let reply = service
        .dispatch(
            connection.id(),
            &Call::SubscribeConversation(records::Subscribe {
                session: id.clone(),
                subscription: Uuid::new_v4(),
                cached: None,
                window: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("open")
    };
    let mut replica = records::Replica::new(Scope::Conversation(id.clone()), 1)
        .apply(1, result.initial)
        .unwrap();
    let mut updates = reply.updates.unwrap();
    service.inner.projects.register(&root).await.unwrap();
    let project = service
        .inner
        .conversation
        .storage
        .projects()
        .unwrap()
        .remove(0);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let message: records::Message =
                protocol::decode(&updates.recv().await.unwrap()).unwrap();
            replica = replica.clone().apply(1, message).unwrap();
            if records::project(&replica)
                .unwrap()
                .thread
                .project_id
                .as_deref()
                == Some(project.id.as_str())
            {
                break;
            }
        }
    })
    .await
    .expect("registration updates the existing subscription");
    let records::Message::Snapshot { rows, .. } =
        service.inner.conversation.storage.list(None).unwrap()
    else {
        panic!("list")
    };
    let listed = rows
        .iter()
        .find(|row| row.key == Key::Thread(id.clone()))
        .unwrap();
    assert_eq!(
        listed.version,
        replica.rows()[&Key::Thread(id.clone())].version
    );
    assert!(
        matches!(&listed.content,Content::Full(record) | Content::Summary(record) if matches!(record.as_ref(),records::Record::Thread(header) if header.details.project.as_deref()==Some(project.id.as_str())))
    );
    drop(updates);
    drop(connection);
    drop(service);
    let restarted = HostRpcService::new([], &state_file);
    assert_eq!(
        restarted
            .inner
            .conversation
            .storage
            .header(&id)
            .unwrap()
            .details
            .project
            .as_deref(),
        Some(project.id.as_str())
    );
    assert_eq!(
        restarted.inner.conversation.storage.projects().unwrap()[0].id,
        project.id
    );
}

#[tokio::test]
async fn a_failed_initial_import_cannot_start_an_empty_replacement_conversation() {
    use agent_protocol::session::HistoryReadKind;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        let target = SessionRef::new(ProviderKind::Codex, "unreadable-native".into()).unwrap();
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        let _: records::Opened = call(
            &service,
            connection.id(),
            Call::SubscribeConversation(records::Subscribe {
                session: target.clone(),
                subscription: Uuid::new_v4(),
                cached: None,
                window: None,
            }),
        )
        .await
        .unwrap();
        while storage
            .header(&target)
            .unwrap()
            .details
            .history_read_state
            .as_ref()
            .is_none_or(|read| read.kind != HistoryReadKind::Unavailable)
        {
            changed.changed().await.unwrap();
        }
        assert!(!storage.loaded(&target).unwrap());
        let operation = Uuid::new_v4();
        let input: ClientInputId = operation.to_string().into();
        let rejected = call::<records::Acknowledged>(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation,
                command: Command::Submit {
                    id: input.clone(),
                    content: content(),
                },
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.code, "history_unavailable");
        assert!(storage.input(&target, &input).unwrap().is_none());
        assert!(agent.records.lock().unwrap().is_empty());
        assert!(agent.inputs.lock().unwrap().is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn opening_native_history_subscribes_before_import_and_publishes_the_latest_window() {
    use agent_protocol::{
        models::{Thread, ThreadResponse, Turn},
        session::HistoryReadKind,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
    let target = SessionRef::new(ProviderKind::Codex, "slow-history".into()).unwrap();
    agent.records.lock().unwrap().insert(
        target.id.clone(),
        ThreadResponse {
            thread: Thread {
                id: Some(target.clone()),
                turns: Some(
                    (0..25)
                        .map(|index| {
                            Arc::new(Turn {
                                id: format!("native-{index}").into(),
                                status: agent_protocol::models::TurnStatus::Completed,
                                items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                                    format!("user-{index}").into(),
                                    agent_protocol::execution::ItemStatus::Completed,
                                    agent_protocol::items::ItemBody::UserMessage {
                                        text: Some("question".into()),
                                        content: vec![],
                                    },
                                ))]),
                                ..Default::default()
                            })
                        })
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let held = agent.open_gate.acquire().await.unwrap();
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.dispatch(
            connection.id(),
            &Call::SubscribeConversation(records::Subscribe {
                session: target.clone(),
                subscription: Uuid::new_v4(),
                cached: None,
                window: None,
            }),
        ),
    )
    .await
    .expect("opening does not wait for the provider")
    .unwrap();
    let Response::Success { result } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("subscription")
    };
    let mut replica = records::Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, result.initial)
        .unwrap();
    assert_eq!(
        records::project(&replica)
            .unwrap()
            .thread
            .history_read_state
            .unwrap()
            .kind,
        HistoryReadKind::Loading
    );
    let mut updates = reply.updates.unwrap();
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            replica = replica
                .clone()
                .apply(1, protocol::decode(&updates.recv().await.unwrap()).unwrap())
                .unwrap();
            let thread = records::project(&replica).unwrap().thread;
            if thread
                .history_read_state
                .as_ref()
                .is_some_and(|state| state.kind == HistoryReadKind::Loading)
            {
                continue;
            }
            assert_eq!(
                thread
                    .turns
                    .unwrap()
                    .iter()
                    .map(|turn| turn.id.as_str())
                    .collect::<Vec<_>>(),
                (15..25)
                    .map(|index| format!("native-{index}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(thread.history_has_more, Some(true));
            break;
        }
    })
    .await
    .unwrap();
    assert!(service.inner.conversation.storage.loaded(&target).unwrap());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn completed_resources_retire_and_late_fenced_callbacks_cannot_create_another_turn() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: ProviderKind::Codex,
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((Uuid::new_v4().to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    let first = service
        .inner
        .conversation
        .storage
        .state(&target)
        .unwrap()
        .attempts
        .into_values()
        .next()
        .unwrap();
    let native = SessionRef::new(first.provider, first.source.clone()).unwrap();
    for count in 2..=20 {
        let operation = Uuid::new_v4();
        let receipt: records::Acknowledged = call(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation,
                command: Command::Submit {
                    id: operation.to_string().into(),
                    content: content(),
                },
            }),
        )
        .await
        .unwrap();
        assert_eq!(receipt.outcome, Outcome::Accepted);
        completed(&service, &target, count).await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !service
            .inner
            .conversation
            .resources
            .lock()
            .unwrap()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed native I/O resources retire");
    let before = service
        .inner
        .conversation
        .storage
        .history(&target, 0)
        .unwrap();
    let native_turn = service
        .inner
        .conversation
        .storage
        .execution(&target, first.id)
        .unwrap()
        .unwrap()
        .native_turn
        .unwrap();
    service
        .inner
        .conversation
        .storage
        .fence_execution(&target, first.id)
        .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: native_turn.clone(),
                    status: agent_protocol::models::TurnStatus::Running,
                    ..Default::default()
                },
                completed: false,
            },
        },
        Some(first.id),
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Item {
                turn_id: native_turn.clone(),
                item: Arc::new(agent_protocol::items::Item::new(
                    "late-after-fence".into(),
                    agent_protocol::execution::ItemStatus::Completed,
                    agent_protocol::items::ItemBody::AssistantText {
                        text: "must stay fenced".into(),
                        phase: Default::default(),
                    },
                )),
            },
        },
        Some(first.id),
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native,
            change: SessionChange::Item {
                turn_id: native_turn,
                item: Arc::new(agent_protocol::items::Item::new(
                    "late-command".into(),
                    agent_protocol::execution::ItemStatus::Running,
                    agent_protocol::items::ItemBody::CommandExecution {
                        output_omitted: false,
                        command: "ignored".into(),
                        cwd: None,
                        output: "fenced bytes".into(),
                        exit_code: None,
                    },
                )),
            },
        },
        Some(first.id),
    )
    .await
    .unwrap();
    assert!(
        service
            .inner
            .conversation
            .outputs
            .subscribe(
                &records::OutputKey {
                    session: target.clone(),
                    item: decision::provider_item_id(first.id, "late-command")
                },
                None
            )
            .is_none(),
        "a fenced callback cannot open a command output resource"
    );
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .history(&target, 0)
            .unwrap(),
        before
    );
    assert!(
        service
            .inner
            .conversation
            .resources
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn discovery_cannot_publish_a_private_source_before_its_creation_receipt() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        use crate::host_rpc::storage::jobs::Work;
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
        let held = agent.create_response_gate.acquire().await.unwrap();
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        service
            .inner
            .conversation
            .storage
            .queue_catalog(ProviderKind::Codex, now())
            .unwrap();
        let claim = service
            .inner
            .conversation
            .storage
            .claim(now(), 1)
            .unwrap()
            .pop()
            .unwrap();
        assert!(matches!(claim.work, Work::Catalog { .. }));
        let connection = service.open_session();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let _: records::Opened = call(
            &service,
            connection.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                provider: ProviderKind::Codex,
                session: target.clone(),
                subscription: Uuid::new_v4(),
                cwd: Some(directory.path().to_str().unwrap().into()),
                selection: Default::default(),
                initial: Some((Uuid::new_v4().to_string().into(), content())),
            }),
        )
        .await
        .unwrap();
        agent.created.notified().await;
        assert_eq!(
            agent.records.lock().unwrap().len(),
            1,
            "the native identity already exists before its creation reply"
        );
        let mut catalog = Box::pin(Runtime::run_job(&service, claim));
        assert!(
            futures_util::poll!(&mut catalog).is_pending(),
            "catalog discovery waits until private ownership is recorded"
        );
        drop(held);
        catalog.await;
        completed(&service, &target, 1).await;
        assert_eq!(
            service
                .inner
                .conversation
                .storage
                .summaries("", false)
                .unwrap()
                .into_iter()
                .filter_map(|thread| thread.id)
                .collect::<Vec<_>>(),
            vec![target]
        );
    })
    .await
    .expect("native creation and discovery settle");
}

#[tokio::test]
async fn ending_a_turn_releases_its_answer_resource_without_a_resolved_notification() {
    use super::native_fixture::AnswerResource;
    use crate::providers::RequestOrigin;
    use agent_protocol::requests::{Request, RequestBody, RequestTarget};
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex,directory.path().into());
        let service = HostRpcService::new([agent.backend()],directory.path().join("projects.json"));
        let connection = service.open_session();
        let target = SessionRef::new(None,Uuid::new_v4().to_string()).unwrap();
        let _: records::Opened = call(&service,connection.id(),Call::CreateConversation(records::Create {
                workspace: None,
            provider:ProviderKind::Codex,session:target.clone(),subscription:Uuid::new_v4(),cwd:Some(directory.path().to_str().unwrap().into()),selection:Default::default(),initial:Some((Uuid::new_v4().to_string().into(),content())),
        })).await.unwrap();
        completed(&service,&target,1).await;
        let native = service.inner.conversation.storage.active_source(&target).unwrap().unwrap();
        let execution = Uuid::new_v4();
        let turn = |completed| AgentChange::Session {session:native.clone(),change:SessionChange::Turn {
            turn:agent_protocol::models::Turn {id:"request-turn".into(),status:if completed {agent_protocol::models::TurnStatus::Completed} else {agent_protocol::models::TurnStatus::Running},..Default::default()},completed,
        }};
        Runtime::observe(&service,turn(false),Some(execution)).await.unwrap();
        let other=SessionRef::new(ProviderKind::Codex,"superseded-source".into()).unwrap();
        let _other=service.inner.conversation.register_native(&target,&other);
        Runtime::observe(&service,AgentChange::Session {session:other.clone(),change:SessionChange::Item {
            turn_id:"request-turn".into(),item:Arc::new(agent_protocol::items::Item::new("wrong-source".into(),agent_protocol::execution::ItemStatus::Completed,
                agent_protocol::items::ItemBody::AssistantText {text:"must not apply".into(),phase:Default::default()})),
        }},Some(execution)).await.unwrap();
        assert!(!service.inner.conversation.storage.state(&target).unwrap().items.contains_key(&decision::provider_item_id(execution,"wrong-source")));
        assert!(Runtime::observe(&service,AgentChange::Request {
            session:other,origin:RequestOrigin {instance:Uuid::new_v4(),native_id:serde_json::json!(3),provider:ProviderKind::Codex,source:Arc::new(AnswerResource)},
            request:Request {id:"wrong-source-request".into(),target:RequestTarget::Turn {turn_id:"request-turn".into(),item_id:None},delivery:agent_protocol::session::RequestDelivery::Awaiting,
                body:RequestBody::ToolExecution {tool:"wrong source".into(),namespace:None,arguments:serde_json::Value::Null}},
        },Some(execution)).await.is_err());
        assert!(service.inner.conversation.storage.state(&target).unwrap().requests.is_empty());

        let resource = Arc::new(AnswerResource);
        let retained = Arc::downgrade(&resource);
        let first_instance = Uuid::new_v4();
        Runtime::observe(&service,AgentChange::Request {
            session:native.clone(),origin:RequestOrigin {instance:first_instance,native_id:serde_json::json!(1),provider:ProviderKind::Codex,source:resource},
            request:Request {id:"native-request".into(),target:RequestTarget::Turn {turn_id:"request-turn".into(),item_id:None},delivery:agent_protocol::session::RequestDelivery::Awaiting,
                body:RequestBody::ToolExecution {tool:"inspect".into(),namespace:None,arguments:serde_json::Value::Null}},
        },Some(execution)).await.unwrap();
        assert!(retained.upgrade().is_some(),"the pending native answer remains addressable");
        assert_eq!(service.inner.conversation.storage.state(&target).unwrap().requests.len(),1);
        let second_instance = Uuid::new_v4();
        Runtime::observe(&service,AgentChange::Request {
            session:native.clone(),origin:RequestOrigin {instance:second_instance,native_id:serde_json::json!(1),provider:ProviderKind::Codex,source:Arc::new(AnswerResource)},
            request:Request {id:"second-native-request".into(),target:RequestTarget::Turn {turn_id:"request-turn".into(),item_id:None},delivery:agent_protocol::session::RequestDelivery::Awaiting,
                body:RequestBody::ToolExecution {tool:"second inspect".into(),namespace:None,arguments:serde_json::Value::Null}},
        },Some(execution)).await.unwrap();
        assert_eq!(service.inner.conversation.storage.state(&target).unwrap().requests.len(),2);
        Runtime::observe(&service,AgentChange::Resolved {instance:second_instance,native_id:serde_json::json!(1)},None).await.unwrap();
        let first_id = decision::provider_request_id(execution, &format!("{first_instance}:1"));
        assert_eq!(service.inner.conversation.storage.state(&target).unwrap().requests.keys().collect::<Vec<_>>(),vec![&first_id]);
        assert_eq!(service.inner.conversation.requests.lock().unwrap().keys().collect::<Vec<_>>(),vec![&first_id]);
        assert!(retained.upgrade().is_some(),"resolving another instance's identical native ID retains the first request");
        let admitted = service.inner.conversation.storage.decide(&target, Event::Command {
            id: decision::OperationId::Client(Uuid::new_v4()), receipt: None,
            command: Command::Answer { id: first_id.clone(), answer: agent_protocol::requests::Answer::ToolExecution { success: true, content: vec![] } },
        }, Runtime::context(&service, Some(ProviderKind::Codex)), None).unwrap();
        let [action] = admitted.step.actions.as_slice() else { panic!("answer action") };
        let updating = service.inner.worktree_access.write().await;
        let mut answering = Box::pin(Runtime::execute(&service, Effect { target: target.clone(), action: action.clone() }));
        assert!(futures_util::poll!(&mut answering).is_pending(), "answer I/O shares the update gate");
        drop(answering);
        drop(updating);
        service.set_history_scope(directory.path().to_owned(), true);
        service.prepare_update().await.unwrap();
        Runtime::execute(&service, Effect { target: target.clone(), action: action.clone() }).await.unwrap();
        assert_eq!(service.inner.conversation.storage.state(&target).unwrap().requests[&first_id].state, decision::RequestState::Awaiting);
        assert!(retained.upgrade().is_some(), "an unsent answer remains addressable after the update gate rejects it");
        Runtime::observe(&service,turn(true),Some(execution)).await.unwrap();
        assert!(service.inner.conversation.storage.state(&target).unwrap().requests.is_empty());
        assert!(retained.upgrade().is_none(),"terminal canonical state releases the private answer handle even without a native Resolved frame");
        let rejected = Runtime::observe(&service,AgentChange::Request {
            session:native.clone(),origin:RequestOrigin {instance:Uuid::new_v4(),native_id:serde_json::json!(2),provider:ProviderKind::Codex,source:Arc::new(AnswerResource)},
            request:Request {id:"late-request".into(),target:RequestTarget::Turn {turn_id:"request-turn".into(),item_id:None},delivery:agent_protocol::session::RequestDelivery::Awaiting,
                body:RequestBody::ToolExecution {tool:"late".into(),namespace:None,arguments:serde_json::Value::Null}},
        },Some(execution)).await;
        assert!(rejected.is_err(),"the adapter must receive an error instead of leaving its permission prompt unanswered");
        assert!(service.inner.conversation.requests.lock().unwrap().is_empty());

    }).await.expect("request lifecycle settles");
}

#[tokio::test]
async fn unavailable_project_does_not_block_listing_or_opening_saved_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            session: target.clone(),
            provider: ProviderKind::Codex,
            workspace: None,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            selection: Default::default(),
            initial: Some((Uuid::new_v4().to_string().into(), content())),
        }),
    )
    .await
    .unwrap();
    completed(&service, &target, 1).await;
    #[cfg(unix)]
    let unavailable = {
        let path = directory.path().join("unavailable");
        std::os::unix::fs::symlink(&path, &path).unwrap();
        path
    };
    #[cfg(windows)]
    let unavailable = std::path::PathBuf::from(r"\\?\UNC\wsl.localhost\Ubuntu\home\user\project");
    let projects: Vec<agent_protocol::models::Project> =
        serde_json::from_value(serde_json::json!([
            {"id":"offline","name":"Offline","roots":[{"path":unavailable}]}
        ]))
        .unwrap();
    service
        .inner
        .conversation
        .storage
        .replace_projects(&projects)
        .unwrap();
    let listed: records::OpenedList = call(
        &service,
        connection.id(),
        Call::SubscribeList(records::SubscribeList {
            subscription: Uuid::new_v4(),
            cached: None,
            refresh: true,
        }),
    )
    .await
    .unwrap();
    let list = records::Replica::new(Scope::List, 1)
        .apply(1, listed.initial)
        .unwrap();
    assert!(
        list.rows()
            .contains_key(&agent_protocol::sync::Key::Thread(target.clone()))
    );
    let opened: records::Opened = call(
        &service,
        connection.id(),
        Call::SubscribeConversation(records::Subscribe {
            session: target.clone(),
            subscription: Uuid::new_v4(),
            cached: None,
            window: None,
        }),
    )
    .await
    .unwrap();
    let replica = records::Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, opened.initial)
        .unwrap();
    let history = records::project(&replica).unwrap();
    assert_eq!(history.thread.id.as_ref(), Some(&target));
    assert!(history.thread.turns.as_ref().unwrap().iter().flat_map(|turn| turn.items.iter().flatten()).any(|item| {
        matches!(item.body(), agent_protocol::items::ItemBody::UserMessage { content, .. }
            if content.iter().any(|part| matches!(part, agent_protocol::items::MessagePart::Text { text } if text == "run once")))
    }));
}

#[tokio::test]
async fn cancellation_between_stages_discards_the_owned_checkout_without_an_active_worker() {
    use agent_protocol::{
        conversation::{self as decision, Action},
        workspace::CreatedWorktree,
    };
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "fixture"],
        &["config", "user.email", "fixture@example.test"],
        &["commit", "--allow-empty", "-m", "initial"],
    ] {
        crate::git::text(&repository, args).unwrap();
    }
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    let checkout = service
        .inner
        .worktrees
        .prepare_selected(
            repository.to_str().unwrap(),
            "main".into(),
            false,
            Default::default(),
        )
        .await
        .unwrap();
    let branch = crate::git::text(&checkout, &["symbolic-ref", "--short", "HEAD"])
        .unwrap()
        .trim()
        .to_owned();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let header = records::Header {
        state: decision::Thread {
            name: String::new(),
            workspace: decision::Workspace::Folder(repository.to_str().unwrap().into()),
            provider: ProviderKind::Codex,
            queue_held: false,
        },
        details: Default::default(),
    };
    service
        .inner
        .conversation
        .storage
        .create(&target, &header)
        .unwrap();
    let action = Action::CancelPreparation {
        action: Uuid::new_v4(),
        discard: Some((
            Uuid::new_v4(),
            CreatedWorktree {
                path: checkout.to_str().unwrap().into(),
                branch: branch.clone(),
            },
        )),
    };
    for _ in 0..2 {
        Runtime::execute(
            &service,
            crate::host_rpc::owners::Effect {
                target: target.clone(),
                action: action.clone(),
            },
        )
        .await
        .unwrap();
    }
    assert!(!checkout.exists());
    assert!(
        crate::git::text(&repository, &["branch", "--list", &branch])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .header(&target)
            .unwrap()
            .state
            .workspace
            .cwd(),
        repository.to_str()
    );
}

#[tokio::test]
async fn delivery_proof_before_a_waiting_fence_keeps_command_output_running() {
    use agent_protocol::{
        execution::ItemStatus,
        items::ItemBody,
        sync::{Output, OutputUpdate},
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for proven in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        agent
            .fail_submit
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let input: agent_protocol::ids::ClientInputId = Uuid::new_v4().to_string().into();
        let _: records::Opened = call(
            &service,
            connection.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                provider: ProviderKind::Codex,
                session: target.clone(),
                subscription: Uuid::new_v4(),
                cwd: Some(directory.path().to_str().unwrap().into()),
                selection: Default::default(),
                initial: Some((input.clone(), content())),
            }),
        )
        .await
        .unwrap();
        let runtime = &service.inner.conversation;
        let mut changed = runtime.storage.changes();
        let delivery = loop {
            let state = runtime.storage.state(&target).unwrap();
            if let Some(decision::PendingInput {
                state: decision::InputState::Unknown { delivery, .. },
                ..
            }) = state.inputs.get(&input)
            {
                break delivery.clone();
            }
            changed.changed().await.unwrap();
        };
        let resolved = runtime
            .storage
            .decide(
                &target,
                Event::Command {
                    id: decision::OperationId::Client(Uuid::new_v4()),
                    receipt: None,
                    command: Command::ResolveUnknown {
                        id: input.clone(),
                        retry: false,
                    },
                },
                Runtime::context(&service, Some(ProviderKind::Codex)),
                None,
            )
            .unwrap();
        let [action] = resolved.step.actions.as_slice() else {
            panic!("fence action")
        };
        let source = runtime.source(&target, &delivery.attempt);
        let held = source.updates.lock().await;
        let mut fencing = Box::pin(Runtime::execute(
            &service,
            Effect {
                target: target.clone(),
                action: action.clone(),
            },
        ));
        assert!(futures_util::poll!(&mut fencing).is_pending());
        if !proven {
            let unloading = agent.unload_gate.acquire().await.unwrap();
            assert!(agent.released.lock().unwrap().is_empty());
            drop(held);
            tokio::select! {
                result = &mut fencing => panic!("unload must still be awaiting its reply: {result:?}"),
                _ = async {
                    while agent.released.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
                } => {}
            }
            let native = SessionRef::new(ProviderKind::Codex, delivery.attempt.source.clone()).unwrap();
            assert!(Arc::ptr_eq(&source, &runtime.resources.lock().unwrap()[&native]));
            Runtime::observe(&service, AgentChange::Session {
                session: native,
                change: SessionChange::Item {
                    turn_id: "native-turn".into(),
                    item: Arc::new(agent_protocol::models::Item::new(
                        "late-during-unload".into(), ItemStatus::Completed,
                        ItemBody::AssistantText { text: "already fenced".into(), phase: Default::default() },
                    )),
                },
            }, Some(delivery.attempt.id)).await.unwrap();
            assert!(!runtime.storage.state(&target).unwrap().items.contains_key(
                &decision::provider_item_id(delivery.attempt.id, "late-during-unload")));
            drop(unloading);
            fencing.await.unwrap();
            assert!(runtime.storage.input(&target, &input).unwrap().is_none());
            continue;
        }
        // This lock is the provider callback's commit boundary. Delivery proof
        // wins while the fence is waiting, before any output can be settled.
        Runtime::event(
            &service,
            &target,
            Event::Delivered {
                id: input.clone(),
                action: delivery.action,
                native_turn: Some("native-turn".into()),
            },
        )
        .await
        .unwrap();
        Runtime::event(
            &service,
            &target,
            Event::Provider {
                attempt: delivery.attempt.id,
                native_turn: Some("native-turn".into()),
                native_items: vec![],
                observation: decision::Observation::Item {
                    id: "command".into(),
                    deferred: false,
                    status: ItemStatus::Running,
                    body: ItemBody::CommandExecution {
                        command: "work".into(),
                        cwd: None,
                        output: String::new(),
                        output_omitted: true,
                        exit_code: None,
                    },
                },
            },
        )
        .await
        .unwrap();
        let key = records::OutputKey {
            session: target.clone(),
            item: "command".into(),
        };
        runtime
            .outputs
            .write(
                delivery.attempt.id,
                key.clone(),
                output::Change::Append("running"),
            )
            .unwrap();
        let (initial, mut stream) = runtime.outputs.subscribe(&key, None).unwrap();
        assert_eq!(initial.text, "running");
        drop(held);
        fencing.await.unwrap();
        assert_eq!(
            runtime
                .storage
                .item(&target, &key.item)
                .unwrap()
                .unwrap()
                .status,
            ItemStatus::Running
        );
        assert!(
            !runtime
                .storage
                .state(&target)
                .unwrap()
                .inputs
                .contains_key(&input)
        );
        runtime
            .outputs
            .write(delivery.attempt.id, key, output::Change::Append(" more"))
            .unwrap();
        let update: OutputUpdate = protocol::decode(
            &stream
                .recv()
                .await
                .expect("delivery proof keeps the output stream alive"),
        )
        .unwrap();
        let final_tail: Output = initial.apply(update);
        assert_eq!(final_tail.text, "running more");
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stopping_root_provider_creation_keeps_one_native_session_for_retry() {
    for retry_before_reply in [false, true] {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
            let held = agent.create_gate.acquire().await.unwrap();
            let service =
                HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
            let connection = service.open_session();
            let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
            let input: agent_protocol::ids::ClientInputId = Uuid::new_v4().to_string().into();
            let _: records::Opened = call(
                &service,
                connection.id(),
                Call::CreateConversation(records::Create {
                    workspace: None,
                    provider: ProviderKind::Codex,
                    session: target.clone(),
                    subscription: Uuid::new_v4(),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    selection: Default::default(),
                    initial: Some((input.clone(), content())),
                }),
            )
            .await
            .unwrap();
            agent.create_started.notified().await;
            let stopped: records::Acknowledged = call(
                &service,
                connection.id(),
                Call::ChangeConversation(records::Change {
                    session: target.clone(),
                    operation: Uuid::new_v4(),
                    command: Command::Interrupt {
                        turn: Some(input.to_string().into()),
                    },
                }),
            )
            .await
            .unwrap();
            assert_eq!(stopped.outcome, Outcome::Accepted);
            let retry = records::Change {
                session: target.clone(),
                operation: Uuid::new_v4(),
                command: Command::RetryInput {
                    id: input.clone(),
                    work_locally: false,
                },
            };
            if retry_before_reply {
                let retried: records::Acknowledged = call(
                    &service,
                    connection.id(),
                    Call::ChangeConversation(retry.clone()),
                )
                .await
                .unwrap();
                assert_eq!(retried.outcome, Outcome::Accepted);
            }
            drop(held);
            if !retry_before_reply {
                let mut changed = service.inner.conversation.storage.changes();
                loop {
                    if matches!(
                        service
                            .inner
                            .conversation
                            .storage
                            .state(&target)
                            .unwrap()
                            .bindings
                            .get(&ProviderKind::Codex),
                        Some(decision::Binding::Ready { .. })
                    ) {
                        break;
                    }
                    changed.changed().await.unwrap();
                }
                assert!(
                    agent.inputs.lock().unwrap().is_empty(),
                    "Stop never sends the input"
                );
                let retried: records::Acknowledged =
                    call(&service, connection.id(), Call::ChangeConversation(retry))
                        .await
                        .unwrap();
                assert_eq!(retried.outcome, Outcome::Accepted);
            }
            completed(&service, &target, 1).await;
            assert_eq!(
                agent.next.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "retry_before_reply={retry_before_reply}"
            );
            assert_eq!(agent.inputs.lock().unwrap().len(), 1);
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn a_delivery_admitted_before_update_cannot_write_after_the_update_gate_closes() {
    use agent_protocol::conversation::{InputContent, InputState, OperationId};
    use native_fixture::{NativeAgent, own};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    service.set_history_scope(directory.path().to_owned(), true);
    let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    own(
        &service,
        &target,
        &agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(native),
                cwd: Some(directory.path().to_str().unwrap().into()),
                turns: Some(Vec::new()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    Runtime::recover(&service, &target, ProviderKind::Codex)
        .await
        .unwrap();
    let operation = Uuid::new_v4();
    let input: ClientInputId = operation.to_string().into();
    let admitted = service
        .inner
        .conversation
        .storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(operation),
                receipt: None,
                command: Command::Submit {
                    id: input.clone(),
                    content: InputContent {
                        parts: vec![op::Input::Text {
                            text: "keep the admitted input".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            },
            decision::Context {
                preparation: None,
                rewind_binding: None,
                imported_turns: &[],
                external_busy: false,
                now: now(),
                seed: Uuid::new_v4(),
                steers: false,
                steer_ready: true,
            },
            None,
        )
        .unwrap();
    let action = admitted.step.actions.into_iter().next().expect("delivery");
    assert!(matches!(action, Action::Deliver { .. }));
    // Admission has committed; its resource worker has not acquired the lock.
    service.prepare_update().await.unwrap();
    Runtime::execute(
        &service,
        super::super::owners::Effect {
            target: target.clone(),
            action,
        },
    )
    .await
    .unwrap();
    assert!(agent.inputs.lock().unwrap().is_empty());
    let pending = service
        .inner
        .conversation
        .storage
        .input(&target, &input)
        .unwrap()
        .unwrap();
    assert!(
        matches!(pending.state, InputState::Failed { .. }),
        "a known unsent input must remain retryable"
    );
    assert_eq!(
        pending.content.parts,
        vec![op::Input::Text {
            text: "keep the admitted input".into()
        }]
    );
}
