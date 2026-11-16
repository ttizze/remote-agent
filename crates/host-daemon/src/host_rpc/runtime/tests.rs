use super::*;
use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
use agent_protocol::protocol::{self, Call, Response};
use agent_protocol::{conversation as decision, orchestration as model};

mod attribution;
mod fresh;

fn content() -> decision::InputContent {
    decision::InputContent {
        fresh_context: false,
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
        let agent = NativeAgent::new(ProviderKind::Codex);
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
        let before = crate::host_rpc::runtime::native_fixture::history(&service, &child).unwrap();
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
        let after = crate::host_rpc::runtime::native_fixture::history(&service, &child).unwrap();
        assert_eq!(after.thread.turns, before.thread.turns);
        assert_eq!(after.thread.parent_id, before.thread.parent_id);
    })
    .await
    .expect("child observation commits before following native work");
}

#[rstest::rstest]
#[tokio::test]
async fn returning_to_a_provider_reuses_its_context_and_injects_only_unseen_history(
    #[values(
        "reuse",
        "compacted",
        "import_incomplete",
        "busy",
        "state_error",
        "injection_unknown"
    )]
    mode: &str,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let compacted = mode == "compacted";
        let directory = tempfile::tempdir().unwrap();
        let mut codex = NativeAgent::new(ProviderKind::Codex);
        Arc::get_mut(&mut codex).unwrap().metadata_only_state = compacted;
        let claude = NativeAgent::new(ProviderKind::Claude);
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
            let _: records::Opened = call(
                &service,
                connection.id(),
                Call::SubscribeConversation(records::Subscribe {
                    session: target.clone(),
                    subscription: Uuid::new_v4(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
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
            assert_eq!(acknowledged.error, None);
            if provider == ProviderKind::Codex && matches!(mode, "busy" | "injection_unknown") {
                let storage = &service.inner.conversation.storage;
                let mut changes = storage.changes();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        let state = storage.thread_state(&target.application_id(), &[]).unwrap();
                        if let Some(run) = state
                            .runs
                            .values()
                            .find(|run| run.user_message_id == operation.to_string())
                            && run.status == agent_protocol::orchestration::RunStatus::Failed
                        {
                            let history =
                                super::native_fixture::history(&service, &target).unwrap();
                            let error = history
                                .thread
                                .turns
                                .as_ref()
                                .unwrap()
                                .iter()
                                .find(|turn| turn.id.as_str() == run.id)
                                .unwrap()
                                .error
                                .as_ref()
                                .unwrap();
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
                assert_eq!(retried.error, None);
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
        let context = serde_json::to_string(&injected.last().unwrap().1).unwrap();
        assert!(context.contains("second-provider-marker"));
        assert_eq!(context.contains("first-provider-marker"), rebuilt);
        assert!(!context.contains("third-provider-marker"));
        assert!(codex.released.lock().unwrap().contains(&original.id));
        assert!(!claude.released.lock().unwrap().is_empty());
    })
    .await
    .expect("provider round trip settles");
}

#[tokio::test]
async fn compact_keeps_inline_provider_handoff_until_the_next_ordinary_input() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let codex = NativeAgent::new(ProviderKind::Codex);
        let claude = NativeAgent::new(ProviderKind::Claude);
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
            assert_eq!(receipt.error, None);
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
async fn missing_providerless_conversations_are_rejected_without_creating_state_or_dispatching() {
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    assert!(
        call::<records::Opened>(
            &service,
            connection.id(),
            Call::SubscribeConversation(records::Subscribe {
                session: target.clone(),
                subscription: Uuid::new_v4(),
                after_sequence: None
            })
        )
        .await
        .is_err()
    );
    let operation = Uuid::new_v4();
    let request = records::Change {
        session: target.clone(),
        operation,
        command: Command::Rename("missing".into()),
    };
    for _ in 0..2 {
        assert!(
            call::<records::Acknowledged>(
                &service,
                connection.id(),
                Call::ChangeConversation(request.clone())
            )
            .await
            .is_err()
        );
    }
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
    assert!(receipts.is_empty());
    assert!(
        service
            .inner
            .conversation
            .storage
            .thread_headers()
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
async fn stopping_a_running_turn_saves_live_output_and_ends_its_subscription() {
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
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{Item, ItemBody},
        sync::OutputUpdate,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let (target, native, execution) =
        super::t3_contract::running(&service, &agent, directory.path().to_str().unwrap()).await;
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
    let short = matches!(case, "short" | "pretruncated");
    send(SessionChange::Item {
        turn_id: "observed-turn".into(),
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
    let storage = &service.inner.conversation.storage;
    let item=storage.thread_state(&target.application_id(),&[]).unwrap().turn_items.into_values().find(|item|matches!(&item.body,model::TurnItemBody::CommandExecution{input,..} if input=="echo output")).unwrap();
    let key = records::OutputKey {
        session: target.clone(),
        item: item.id.into(),
    };
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
        panic!("output subscription")
    };
    assert_eq!(opened.initial.text, "initial");
    let mut updates = reply.updates.unwrap();
    let before = storage.thread_stream(&target, None).unwrap();
    send(SessionChange::Text {
        turn_id: "observed-turn".into(),
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
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), updates.recv())
        .await
        .unwrap()
        .unwrap();
    let update: OutputUpdate = protocol::decode(&frame).unwrap();
    assert_eq!(matches!(update, OutputUpdate::Tail(_)), !short);
    let tail = opened.initial.apply(update);
    assert_eq!(tail.truncated, case != "short");
    assert!(tail.text.encode_utf16().count() <= 64 * 1024);
    assert_eq!(
        storage.thread_stream(&target, None).unwrap(),
        before,
        "live output bytes do not advance the durable timeline"
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
            turn_id: "observed-turn".into(),
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
        let _late = service
            .inner
            .conversation
            .outputs
            .subscribe(&key, Some((execution.to_string(), Default::default())))
            .unwrap();
    }
    if background {
        send(SessionChange::Background {
            tasks: vec![agent_protocol::execution::BackgroundTask {
                id: "background-command".into(),
                kind: agent_protocol::execution::BackgroundTaskKind::Command,
                description: Some("echo output".into()),
            }],
        })
        .await
        .unwrap();
    }
    if case == "stopped" {
        Runtime::observe(
            &service,
            AgentChange::Stopped {
                provider: ProviderKind::Codex,
                error: agent_protocol::execution::ExecutionError {
                    message: "provider stopped".into(),
                    ..Default::default()
                },
            },
            None,
        )
        .await
        .unwrap();
    } else if case == "fenced" || case == "foreground-stop" {
        let run = storage
            .thread_state(&target.application_id(), &[])
            .unwrap()
            .active_run()
            .unwrap()
            .id
            .clone();
        Runtime::change(
            &service,
            &records::Change {
                session: target.clone(),
                operation: Uuid::new_v4(),
                command: Command::Interrupt {
                    turn: Some(run.into()),
                },
            },
        )
        .await
        .unwrap()
        .accepted()
        .unwrap();
        // T3 records a Stop request before the provider reports interruption.
        // The native terminal event, rather than the RPC acknowledgment, is
        // the evidence that output can be durably finalized.
        send(SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status: TurnStatus::Interrupted,
                ..Default::default()
            },
            completed: true,
        })
        .await
        .unwrap();
    } else {
        let status = match case {
            "background-failed" => TurnStatus::Failed,
            "background-interrupted" => TurnStatus::Interrupted,
            _ => TurnStatus::Completed,
        };
        send(SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status,
                ..Default::default()
            },
            completed: true,
        })
        .await
        .unwrap();
        if background && matches!(case, "background" | "background-stop") {
            send(SessionChange::Background { tasks: vec![] })
                .await
                .unwrap();
        }
    }
    let stored = storage
        .thread_item(&target, key.item.as_str())
        .unwrap()
        .unwrap();
    let model::TurnItemBody::CommandExecution { output, .. } = &stored.body else {
        panic!("command missing")
    };
    assert!(
        output.as_deref() == Some(tail.text.as_str()),
        "the committed output must equal the last observed terminal tail"
    );
    assert_eq!(
        stored.status,
        match case {
            "stopped" | "background-failed" => model::TurnItemStatus::Failed,
            "background-interrupted" => model::TurnItemStatus::Interrupted,
            "foreground-stop" | "fenced" => model::TurnItemStatus::Interrupted,
            _ => model::TurnItemStatus::Completed,
        }
    );
    assert!(updates.recv().await.is_none());
}

#[rstest::rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[tokio::test]
async fn native_reply_before_submit_ack_commits_one_turn_and_replay_never_sends_again(
    #[case] without_folder: bool,
    #[case] unknown_ack: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    if unknown_ack {
        agent
            .fail_submit
            .store(3, std::sync::atomic::Ordering::SeqCst);
    }
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let request = records::Create {
        workspace: None,
        provider: agent_protocol::session::ProviderKind::Codex,
        subscription: Uuid::new_v4(),
        session: target.clone(),
        cwd: (!without_folder).then(|| directory.path().to_str().unwrap().into()),
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
    let mut replica = super::native_fixture::Transcript::opened(opened);
    let mut stream = reply.updates.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let projected = replica.project().unwrap();
            assert!(
                !projected
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Failed),
                "native execution failed: {projected:?}"
            );
            if projected.thread.turns.as_ref().is_some_and(|turns| {
                turns.len() == 1
                    && turns[0].status == agent_protocol::execution::TurnStatus::Completed
            }) {
                break;
            }
            let frame = stream.recv().await.unwrap();
            replica = replica.apply(protocol::decode(&frame).unwrap()).unwrap();
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
    assert_eq!(acknowledged.error, None);
    let opened: records::Opened = call(
        &service,
        connection.id(),
        Call::SubscribeConversation(records::Subscribe {
            session: target.clone(),
            subscription: Uuid::new_v4(),
            after_sequence: None,
        }),
    )
    .await
    .unwrap();
    let stored = super::native_fixture::Transcript::opened(opened)
        .project()
        .unwrap();
    let turns = stored.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.as_ref().unwrap().len(), 2);
    assert_eq!(
        turns[0].items.as_ref().unwrap()[0]
            .client_input_id
            .as_ref()
            .unwrap()
            .as_str(),
        operation.to_string().as_str()
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_eq!(*agent.created_in.lock().unwrap(), vec![true]);
    assert_eq!(stored.effort.as_deref(), Some("high"));
    assert_eq!(stored.service_tier.as_deref(), Some("priority"));
}

#[tokio::test]
async fn cancelling_unknown_delivery_releases_native_resources_and_allows_a_new_input_without_resending()
 {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    agent
        .fail_submit
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let reply = service
        .dispatch(
            connection.id(),
            &Call::CreateConversation(records::Create {
                workspace: None,
                session: target.clone(),
                provider: ProviderKind::Codex,
                subscription: Uuid::new_v4(),
                cwd: Some(directory.path().to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: Some((operation.to_string().into(), content())),
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: opened } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("create failed");
    };
    let mut transcript = super::native_fixture::Transcript::opened(opened);
    let mut updates = reply.updates.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !transcript
            .project()
            .unwrap()
            .thread
            .preparations
            .iter()
            .any(|preparation| {
                preparation.status == agent_protocol::session::PreparationStatus::Unknown
            })
        {
            transcript = transcript
                .clone()
                .apply(protocol::decode(&updates.recv().await.unwrap()).unwrap())
                .unwrap();
        }
    })
    .await
    .unwrap();
    assert!(agent.inputs.lock().unwrap().is_empty());
    let cancelled: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::ResolveUnknown {
                id: operation.to_string().into(),
                retry: false,
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(cancelled.error, None);
    let next = Uuid::new_v4();
    let sent: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: next,
            command: Command::Submit {
                id: next.to_string().into(),
                content: content(),
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(sent.error, None);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = transcript.project().unwrap();
            if view.thread.turns.iter().flatten().any(|turn| {
                turn.status == agent_protocol::execution::TurnStatus::Completed
                    && turn.items.iter().flatten().any(|item| {
                        item.client_input_id
                            .as_ref()
                            .is_some_and(|id| id.as_str() == next.to_string())
                    })
            }) {
                break;
            }
            transcript = transcript
                .clone()
                .apply(protocol::decode(&updates.recv().await.unwrap()).unwrap())
                .unwrap();
        }
    })
    .await
    .unwrap();
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].client_user_message_id.as_str(), next.to_string());
    assert!(!agent.released.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_known_unsent_start_retries_the_reserved_attempt_after_its_outbox_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    agent
        .fail_submit
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let operation = Uuid::new_v4();
    let reply = service
        .dispatch(
            connection.id(),
            &Call::CreateConversation(records::Create {
                workspace: None,
                session: target.clone(),
                provider: ProviderKind::Codex,
                subscription: Uuid::new_v4(),
                cwd: Some(directory.path().to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: Some((operation.to_string().into(), content())),
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: opened } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("create failed");
    };
    let mut transcript = super::native_fixture::Transcript::opened(opened);
    let mut updates = reply.updates.unwrap();
    let attempt = transcript.snapshot.projection.attempts[0].id.clone();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !transcript
            .project()
            .unwrap()
            .thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
        {
            transcript = transcript
                .clone()
                .apply(protocol::decode(&updates.recv().await.unwrap()).unwrap())
                .unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(transcript.snapshot.projection.runs.len(), 1);
    assert_eq!(transcript.snapshot.projection.attempts.len(), 1);
    assert_eq!(transcript.snapshot.projection.attempts[0].id, attempt);
    assert_eq!(agent.records.lock().unwrap().len(), 1);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_eq!(
        agent.fail_submit.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

async fn completed(
    service: &HostRpcService,
    target: &SessionRef,
    count: usize,
) -> agent_protocol::models::ThreadResponse {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let history = super::native_fixture::history(service, target).unwrap();
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

#[tokio::test]
async fn sibling_forks_continue_the_selected_native_prefix_in_independent_resources() {
    use agent_protocol::session::{HistoryReadKind, HistoryReadState};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
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
    // The native transcript may become incomplete after admission. Reopening
    // must still expose the authoritative canonical source, without reimporting
    // a shortened native copy over it.
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::SubscribeConversation(records::Subscribe {
            session: target.clone(),
            subscription: Uuid::new_v4(),
            after_sequence: None,
        }),
    )
    .await
    .unwrap();
    let original = super::native_fixture::history(&service, &target).unwrap();
    let through = original.thread.turns.as_ref().unwrap()[0].id.clone();
    let mut sources = std::collections::HashSet::new();
    for _ in 0..2 {
        let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let request = records::Fork {
            source: target.clone(),
            target: fork.clone(),
            through: records::ForkPoint::Run {
                run_id: through.to_string(),
            },
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
        assert_eq!(ack.error, None);
        let continued = completed(&service, &fork, 2).await;
        let can_rewind_prefix = |thread: &agent_protocol::models::Thread| {
            let rendered = agent_core::presentation::conversation::project_conversation(
                Default::default(),
                &Default::default(),
                &Default::default(),
                Arc::new(thread.clone()),
                &None,
            );
            rendered.turns[0]
                .rows
                .iter()
                .any(|row| row.edit_turn_id.is_some())
        };
        assert!(can_rewind_prefix(&original.thread));
        assert!(
            !can_rewind_prefix(&continued.thread),
            "inherited history is not a local rollback target"
        );
        let mut inherited =
            serde_json::to_value(&continued.thread.turns.as_ref().unwrap()[0]).unwrap();
        let mut original_prefix =
            serde_json::to_value(&original.thread.turns.as_ref().unwrap()[0]).unwrap();
        // Only local execution facts differ: rewind eligibility and, as T3's
        // resolveAttempt reads the displayed thread's own attempts, the source's
        // attempts. Every transcript field, provider attribution, fork action
        // and timing fact stays identical.
        for turn in [&mut inherited, &mut original_prefix] {
            let turn = turn.as_object_mut().unwrap();
            turn.remove("supportsRewind");
            turn.remove("attempts");
            for item in turn["items"].as_array_mut().unwrap() {
                item.as_object_mut().unwrap().remove("attempt");
            }
        }
        assert_eq!(inherited, original_prefix);
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
    // Two old turns already satisfy completed(count=2) before rollback. Wait
    // for the fifth actual prompt (two source + two sibling + this continuation)
    // so the old snapshot cannot make an unexecuted rewind appear successful.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if agent.inputs.lock().unwrap().len() == 5 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("post-rewind input reaches native execution");
    let resumed = completed(&service, &target, 2).await;
    assert_eq!(storage.active_source(&target).unwrap(), Some(source));
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
    let mut agent = NativeAgent::new(ProviderKind::Codex);
    Arc::get_mut(&mut agent).unwrap().submit_gate = Some(tokio::sync::Semaphore::new(0));
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
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.submit_started.notified(),
    )
    .await
    .unwrap();
    let (native, execution) = super::native_fixture::bound_execution(&service, &target);
    let native_turn_id: agent_protocol::ids::TurnId = "stream-turn".into();
    let native_answer_id: agent_protocol::ids::ItemId = "stream-answer".into();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: native_turn_id.clone(),
                    status: agent_protocol::execution::TurnStatus::Running,
                    items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                        native_answer_id.clone(),
                        agent_protocol::execution::ItemStatus::Running,
                        agent_protocol::items::ItemBody::AssistantText {
                            text: "initial".into(),
                            phase: agent_protocol::items::AssistantPhase::Final,
                        },
                    ))]),
                    ..Default::default()
                },
                completed: false,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let initial = super::native_fixture::history(&service, &target).unwrap();
    let text = |delta: &str| AgentChange::Session {
        session: native.clone(),
        change: SessionChange::Text {
            turn_id: native_turn_id.clone(),
            item_id: native_answer_id.clone(),
            field: agent_protocol::session::TextField::AssistantText,
            delta: delta.into(),
        },
    };
    for delta in [" 一", "文"] {
        Runtime::observe(&service, text(delta), Some(execution))
            .await
            .unwrap();
    }
    let unchanged = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
    assert_eq!(
        unchanged.thread.turns, initial.thread.turns,
        "partial chunks do not each rewrite the database"
    );
    Runtime::observe(&service, text("。\n\n"), Some(execution))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
            if matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with(" 一文。\n\n")) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let paragraph = crate::host_rpc::runtime::native_fixture::history(&service, &target)
        .unwrap()
        .thread
        .turns;
    Runtime::observe(&service, text("unfinished suffix"), Some(execution))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(799)).await;
    assert_eq!(
        crate::host_rpc::runtime::native_fixture::history(&service, &target)
            .unwrap()
            .thread
            .turns,
        paragraph
    );
    Runtime::observe(&service, text(" more"), Some(execution))
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert_eq!(
        crate::host_rpc::runtime::native_fixture::history(&service, &target)
            .unwrap()
            .thread
            .turns,
        paragraph,
        "new text resets the idle deadline"
    );
    tokio::time::advance(std::time::Duration::from_millis(799)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saved = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
            if matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with("unfinished suffix more")) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let partial = crate::host_rpc::runtime::native_fixture::history(&service, &target)
        .unwrap()
        .thread
        .turns;
    for delta in [" paragraph\n", "\n"] {
        Runtime::observe(&service, text(delta), Some(execution))
            .await
            .unwrap();
    }
    tokio::time::advance(std::time::Duration::from_millis(399)).await;
    assert_eq!(
        crate::host_rpc::runtime::native_fixture::history(&service, &target)
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
                turn_id: native_turn_id.clone(),
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
        Some(execution),
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::ReasoningPart {
                turn_id: native_turn_id.clone(),
                item_id: "reason-0".into(),
                field: ReasoningField::Summary,
                index: 0,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    for delta in ["", "A\n\nB"] {
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Text {
                    turn_id: native_turn_id.clone(),
                    item_id: "reason-0".into(),
                    field: TextField::ReasoningSummary { index: 0 },
                    delta: delta.into(),
                },
            },
            Some(execution),
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;
    }
    let mut changes = service.inner.conversation.storage.changes();
    tokio::time::advance(std::time::Duration::from_millis(800)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let id=super::native::text_item_id(ProviderKind::Codex,native_turn_id.as_str(),"reason-0",TextField::ReasoningSummary {index:0});
            let saved=service.inner.conversation.storage.thread_item(&target, &id).unwrap();
            if saved.is_some_and(|item|matches!(item.body, agent_protocol::orchestration::TurnItemBody::Reasoning {text,..} if text=="A\n\nB")) { break; }
            changes.changed().await.unwrap();
        }
    }).await.expect("reasoning must stream after an empty part, before completion");
    Runtime::observe(&service, text(" final pending"), Some(execution))
        .await
        .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: native_turn_id.clone(),
                    status: agent_protocol::execution::TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let saved = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
    assert!(
        matches!(saved.thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1].body(), agent_protocol::items::ItemBody::AssistantText { text, .. } if text.ends_with(" paragraph\n\n final pending")),
        "completion flushes the last partial body"
    );
}

#[tokio::test]
async fn explicit_project_import_never_backfills_an_owned_conversations_history() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
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
    let storage = &service.inner.conversation.storage;
    let before = storage.thread_stream(&target, None).unwrap();
    let native = storage.active_source(&target).unwrap().unwrap();
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
        .push(Arc::new(agent_protocol::models::Turn {
            id: "external".into(),
            status: agent_protocol::models::TurnStatus::Completed,
            items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                "external-text".into(),
                agent_protocol::models::ItemStatus::Completed,
                agent_protocol::items::ItemBody::AssistantText {
                    text: "must never be appended".into(),
                    phase: Default::default(),
                },
            ))]),
            ..Default::default()
        }));
    for _ in 0..2 {
        let _: agent_protocol::project_import::ProjectImportResult = call(
            &service,
            connection.id(),
            Call::ImportProjects(agent_protocol::project_import::ImportProjects {
                paths: vec![directory.path().to_str().unwrap().into()],
            }),
        )
        .await
        .unwrap();
    }
    let after = storage.thread_state(&target.application_id(), &[]).unwrap();
    assert_eq!(after.runs.len(), 1);
    assert!(!after.turn_items.values().any(|item|matches!(&item.body,model::TurnItemBody::AssistantMessage{text,..} if text=="must never be appended")));
    // Project registration may assign membership; the existing transcript is unchanged.
    let model::projection::ThreadStreamItem::Snapshot(before) = &before[0] else {
        panic!("snapshot")
    };
    let history = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
    assert_eq!(
        history.thread.turns.unwrap().len(),
        before.projection.runs.len()
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn registering_a_project_updates_open_conversations_and_the_shared_list_after_restart() {
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
    let mut thread = crate::host_rpc::storage::test_support::thread(&id.application_id());
    thread.title = "Already open".into();
    thread.worktree_path = Some(cwd.clone());
    service
        .inner
        .conversation
        .storage
        .thread_command(
            crate::host_rpc::storage::commands::Command {
                id: "create",
                thread_id: &id.application_id(),
                kind: "thread.create",
                now: crate::host_rpc::storage::test_support::NOW,
                item_ids: &[],
            },
            Some(&thread),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
    let connection = service.open_session();
    let reply = service
        .dispatch(
            connection.id(),
            &Call::SubscribeConversation(records::Subscribe {
                session: id.clone(),
                subscription: Uuid::new_v4(),
                after_sequence: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("open")
    };
    let mut replica = super::native_fixture::Transcript::opened(result);
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
            let message: agent_protocol::orchestration::projection::ThreadStreamItem =
                protocol::decode(&updates.recv().await.unwrap()).unwrap();
            replica = replica.clone().apply(message).unwrap();
            if replica.project().unwrap().thread.project_id.as_deref() == Some(project.id.as_str())
            {
                break;
            }
        }
    })
    .await
    .expect("registration updates the existing subscription");
    let shell = service.inner.conversation.storage.shell_snapshot().unwrap();
    let listed = shell
        .threads
        .iter()
        .find(|thread| thread.thread.id == id.application_id())
        .unwrap();
    assert_eq!(
        listed.thread.project_id.as_deref(),
        Some(project.id.as_str())
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
            .thread_header(&id)
            .unwrap()
            .unwrap()
            .project_id
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
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(ProviderKind::Codex, "unreadable-native".into()).unwrap();
    let storage = &service.inner.conversation.storage;
    assert!(
        call::<records::Opened>(
            &service,
            connection.id(),
            Call::SubscribeConversation(records::Subscribe {
                session: target.clone(),
                subscription: Uuid::new_v4(),
                after_sequence: None
            })
        )
        .await
        .is_err()
    );
    let id = Uuid::new_v4();
    let rejected = call::<records::Acknowledged>(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: id,
            command: Command::Submit {
                id: id.to_string().into(),
                content: content(),
            },
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.code, "history_unavailable");
    assert!(storage.thread_header(&target).unwrap().is_none());
    assert!(agent.records.lock().unwrap().is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opening_native_history_waits_for_atomic_import_before_publishing_its_canonical_snapshot() {
    use agent_protocol::models::{Thread, ThreadResponse, Turn};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
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
                                        text: Some(format!("question {index}")),
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
    let request = Call::SubscribeConversation(records::Subscribe {
        session: target.clone(),
        subscription: Uuid::new_v4(),
        after_sequence: None,
    });
    let mut opening = Box::pin(service.dispatch(connection.id(), &request));
    assert!(futures_util::poll!(&mut opening).is_pending());
    assert!(
        service
            .inner
            .conversation
            .storage
            .thread_header(&target)
            .unwrap()
            .is_none()
    );
    drop(held);
    let reply = opening.await.unwrap();
    let Response::Success { result: opened } =
        protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("open")
    };
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &opened.initial[0] else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.projection.thread.history_origin,
        Some(model::ThreadHistoryOrigin::V1Import)
    );
    assert!(snapshot.projection.runs.is_empty());
    assert!(snapshot.projection.provider_turns.is_empty());
    // Pinned ProjectionStore's userTurnLimit=10 window keeps the latest ten
    // user turns. Earlier messages remain available through the real cursor.
    assert_eq!(snapshot.projection.turn_items.len(), 10);
    assert!(snapshot.has_more_history);
    let earlier = service
        .inner
        .conversation
        .storage
        .thread_history(&target, snapshot.history_cursor.as_deref().unwrap())
        .unwrap();
    assert!(!earlier.items.is_empty());
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .context_history(&agent_protocol::handoff::ReadHistory {
                session: target.clone(),
                item_id: None,
                offset: 0,
                limit: 20,
                text_offset: 0
            })
            .unwrap()
            .items
            .len(),
        20
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn completed_resources_retire_and_late_fenced_callbacks_cannot_create_another_turn() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
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
        .thread_state(&target.application_id(), &[])
        .unwrap()
        .run_attempts
        .into_values()
        .next()
        .unwrap();
    let native = SessionRef::new(
        ProviderKind::from_instance_id(&first.provider_instance_id).unwrap(),
        first
            .native_thread_ref
            .as_ref()
            .unwrap()
            .native_id
            .clone()
            .unwrap(),
    )
    .unwrap();
    let execution = Uuid::parse_str(&first.id).unwrap();
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
        assert_eq!(receipt.error, None);
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
    let before = crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap();
    let state = service
        .inner
        .conversation
        .storage
        .thread_state(&target.application_id(), &[])
        .unwrap();
    let native_turn: agent_protocol::ids::TurnId = state.provider_turns
        [first.provider_turn_id.as_ref().unwrap()]
    .native_turn_ref
    .as_ref()
    .unwrap()
    .native_id
    .clone()
    .unwrap()
    .into();
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
        Some(execution),
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
        Some(execution),
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
        Some(execution),
    )
    .await
    .unwrap();
    assert!(
        !service
            .inner
            .conversation
            .storage
            .thread_state(&target.application_id(), &[])
            .unwrap()
            .turn_items
            .values()
            .any(|item| item
                .native_item_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("late-command"))
    );
    assert_eq!(
        crate::host_rpc::runtime::native_fixture::history(&service, &target).unwrap(),
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
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex);
        let held = agent.create_response_gate.acquire().await.unwrap();
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        service
            .inner
            .projects
            .register(directory.path())
            .await
            .unwrap();
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
        let roots = [directory.path().to_str().unwrap().to_owned()];
        let mut catalog = Box::pin(Runtime::import_project_threads(&service, &roots));
        assert!(
            futures_util::poll!(&mut catalog).is_pending(),
            "catalog discovery waits until private ownership is recorded"
        );
        drop(held);
        catalog.await.unwrap();
        completed(&service, &target, 1).await;
        assert_eq!(
            service
                .inner
                .conversation
                .storage
                .shell_snapshot()
                .unwrap()
                .threads
                .into_iter()
                .map(|thread| thread.thread.id.clone())
                .collect::<Vec<_>>(),
            vec![target.application_id()]
        );
    })
    .await
    .expect("native creation and discovery settle");
}

#[tokio::test]
async fn ending_a_turn_releases_its_answer_resource_without_a_resolved_notification() {
    use super::native_fixture::AnswerResource;
    use agent_protocol::requests::{Request, RequestBody, RequestTarget};
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex);
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let (target, native, execution) =
            super::t3_contract::running(&service, &agent, directory.path().to_str().unwrap()).await;
        let storage = &service.inner.conversation.storage;
        let request =
            |instance, source: Arc<dyn crate::providers::AnswerSource>| AgentChange::Request {
                session: native.clone(),
                origin: RequestOrigin {
                    instance,
                    native_id: serde_json::json!(1),
                    provider: ProviderKind::Codex,
                    source,
                },
                request: Request {
                    id: "native-request".into(),
                    target: RequestTarget::Turn {
                        turn_id: "observed-turn".into(),
                        item_id: None,
                    },
                    delivery: agent_protocol::session::RequestDelivery::Awaiting,
                    body: RequestBody::ToolExecution {
                        tool: "inspect".into(),
                        namespace: None,
                        arguments: serde_json::Value::Null,
                    },
                },
            };
        let resource = Arc::new(AnswerResource);
        let retained = Arc::downgrade(&resource);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        Runtime::observe(&service, request(first, resource), Some(execution))
            .await
            .unwrap();
        Runtime::observe(
            &service,
            request(second, Arc::new(AnswerResource)),
            Some(execution),
        )
        .await
        .unwrap();
        let pending = || {
            storage
                .thread_state(&target.application_id(), &[])
                .unwrap()
                .runtime_requests
                .into_values()
                .filter(|request| request.status == model::RequestStatus::Pending)
                .collect::<Vec<_>>()
        };
        assert_eq!(pending().len(), 2);
        Runtime::observe(
            &service,
            AgentChange::Resolved {
                instance: second,
                native_id: serde_json::json!(1),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(pending().len(), 1);
        let first_id: agent_protocol::ids::RequestId = pending()[0].id.clone().into();
        assert!(
            retained.upgrade().is_some(),
            "resolving another SDK instance must retain the first handle"
        );
        let answer = records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Answer {
                id: first_id,
                answer: agent_protocol::requests::Answer::ToolExecution {
                    success: true,
                    content: vec![],
                },
            },
        };
        let updating = service.inner.worktree_access.write().await;
        let mut answering = Box::pin(Runtime::change(&service, &answer));
        assert!(futures_util::poll!(&mut answering).is_pending());
        drop(answering);
        drop(updating);
        service.set_history_scope(directory.path().into(), true);
        service.prepare_update().await.unwrap();
        assert!(Runtime::change(&service, &answer).await.is_err());
        assert_eq!(pending().len(), 1);
        assert!(retained.upgrade().is_some());
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "observed-turn".into(),
                        status: agent_protocol::models::TurnStatus::Completed,
                        ..Default::default()
                    },
                    completed: true,
                },
            },
            Some(execution),
        )
        .await
        .unwrap();
        assert!(pending().is_empty());
        assert!(retained.upgrade().is_none());
        assert!(
            service
                .inner
                .conversation
                .requests
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(
            Runtime::observe(
                &service,
                request(Uuid::new_v4(), Arc::new(AnswerResource)),
                Some(execution)
            )
            .await
            .is_err()
        );
    })
    .await
    .expect("request lifecycle settles");
}

#[tokio::test]
async fn unavailable_project_does_not_block_listing_or_opening_saved_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
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
            after_sequence: None,
            refresh: true,
        }),
    )
    .await
    .unwrap();
    let mut shell = None;
    for item in listed.initial {
        shell = agent_core::orchestration::shell::receive(shell.as_ref(), item, true).unwrap();
    }
    assert!(
        shell
            .unwrap()
            .threads
            .iter()
            .any(|thread| thread.thread.id == target.application_id())
    );
    let opened: records::Opened = call(
        &service,
        connection.id(),
        Call::SubscribeConversation(records::Subscribe {
            session: target.clone(),
            subscription: Uuid::new_v4(),
            after_sequence: None,
        }),
    )
    .await
    .unwrap();
    let replica = super::native_fixture::Transcript::opened(opened);
    let history = replica.project().unwrap();
    assert_eq!(history.thread.id.as_ref(), Some(&target));
    assert!(history.thread.turns.as_ref().unwrap().iter().flat_map(|turn| turn.items.iter().flatten()).any(|item| {
        matches!(item.body(), agent_protocol::items::ItemBody::UserMessage { text, content }
            if text.as_deref()==Some("run once") || content.iter().any(|part| matches!(part, agent_protocol::items::MessagePart::Text { text } if text == "run once")))
    }));
}

#[tokio::test]
async fn stopping_root_provider_creation_keeps_one_native_session_for_retry() {
    for retry_before_reply in [false, true] {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let agent = NativeAgent::new(ProviderKind::Codex);
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
            assert_eq!(stopped.error, None);
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
                assert_eq!(retried.error, None);
            }
            drop(held);
            if !retry_before_reply {
                let mut changed = service.inner.conversation.storage.changes();
                loop {
                    let state = service
                        .inner
                        .conversation
                        .storage
                        .thread_state(&target.application_id(), &[])
                        .unwrap();
                    if state.provider_threads.values().any(|thread| {
                        thread.native_thread_ref.is_some()
                            && thread.status
                                == agent_protocol::orchestration::ProviderThreadStatus::Idle
                            && state.provider_session(&thread.id).is_some()
                    }) {
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
                assert_eq!(retried.error, None);
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
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let held = agent.create_response_gate.acquire().await.unwrap();
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    service.set_history_scope(directory.path().into(), true);
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
    let mut updating = Box::pin(service.prepare_update());
    assert!(
        futures_util::poll!(&mut updating).is_pending(),
        "a native write already owns the update gate"
    );
    drop(held);
    updating.await.unwrap();
    let count = agent.inputs.lock().unwrap().len();
    let rejected = Runtime::change(
        &service,
        &records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: content(),
            },
        },
    )
    .await;
    assert!(rejected.is_err());
    assert_eq!(agent.inputs.lock().unwrap().len(), count);
}
