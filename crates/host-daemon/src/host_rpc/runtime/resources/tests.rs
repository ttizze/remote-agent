use super::*;
use agent_protocol::execution::{BackgroundTask, BackgroundTaskKind};

#[rstest::rstest]
#[tokio::test]
async fn capacity_resume_checks_native_safety_and_preserves_stop(
    #[values(
        "refused",
        "continued",
        "stop-after-ack",
        "running",
        "question",
        "child",
        "reload",
        "disabled",
        "unknown",
        "preparing",
        "stop",
        "stop-before-start",
        "updating"
    )]
    case: &str,
) {
    use super::super::native_fixture::NativeAgent;
    use agent_protocol::{
        conversation::{OperationId, TurnState},
        models::{Thread, ThreadResponse},
    };
    let directory = tempfile::tempdir().unwrap();
    let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
    Arc::get_mut(&mut agent).unwrap().needs_reload = case == "reload";
    let service =
        HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let response = ThreadResponse {
        thread: Thread {
            id: Some(native.clone()),
            cwd: Some(directory.path().to_str().unwrap().into()),
            status: agent_protocol::models::SessionStatus::Idle,
            turns: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .insert(native.id.clone(), response.clone());
    let storage = &service.inner.conversation.storage;
    super::super::native_fixture::own(&service, &target, &response);
    Runtime::recover(&service, &target, ProviderKind::Codex)
        .await
        .unwrap();
    let context = Runtime::context(&service, Some(ProviderKind::Codex));
    let input = Uuid::new_v4();
    let submitted = storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(input),
                receipt: None,
                command: Command::Submit {
                    id: input.to_string().into(),
                    content: decision::InputContent {
                        parts: vec![op::Input::Text {
                            text: "work".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            },
            context,
            None,
        )
        .unwrap();
    let Action::Deliver {
        id,
        action,
        attempt,
        ..
    } = &submitted.step.actions[0]
    else {
        panic!("delivery")
    };
    storage
        .decide(
            &target,
            Event::Delivered {
                id: id.clone(),
                action: *action,
                native_turn: Some("first".into()),
            },
            context,
            None,
        )
        .unwrap();
    storage
        .decide(
            &target,
            Event::Provider {
                attempt: attempt.id,
                native_turn: None,
                native_items: vec![],
                observation: Observation::Failed(Some(
                    agent_protocol::execution::ExecutionError {
                        message: "original overload".into(),
                        provider_code: Some("serverOverloaded".into()),
                        ..Default::default()
                    },
                )),
            },
            context,
            None,
        )
        .unwrap();
    let due = storage
        .decide(
            &target,
            Event::Wake,
            decision::Context {
                now: context.now + 2100,
                seed: Uuid::new_v4(),
                ..context
            },
            None,
        )
        .unwrap();
    let Action::Resume {
        attempt,
        cwd,
        at,
        error,
    } = &due.step.actions[0]
    else {
        panic!("resume")
    };
    {
        let mut records = agent.records.lock().unwrap();
        let thread = &mut records.get_mut(&native.id).unwrap().thread;
        match case {
            "running" => thread.status = agent_protocol::models::SessionStatus::Running,
            "unknown" => thread.status = agent_protocol::models::SessionStatus::Unknown,
            "preparing" => thread.status = agent_protocol::models::SessionStatus::Preparing,
            "child" => {
                thread.parent_id =
                    Some(SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap())
            }
            "question" => {
                thread.requests.insert(
                    "permission".into(),
                    Arc::new(agent_protocol::requests::Request {
                        id: "permission".into(),
                        target: agent_protocol::requests::RequestTarget::Session,
                        delivery: agent_protocol::session::RequestDelivery::Awaiting,
                        body: agent_protocol::requests::RequestBody::ToolExecution {
                            tool: "inspect".into(),
                            namespace: None,
                            arguments: serde_json::Value::Null,
                        },
                    }),
                );
            }
            _ => {}
        }
    }
    if case == "disabled" {
        let client = service.open_session();
        let _: agent_protocol::providers::ProviderSettings =
            super::super::native_fixture::call(
                &service,
                client.id(),
                agent_protocol::protocol::Call::SetProviderEnabled(
                    agent_protocol::providers::SetProviderEnabled {
                        provider: ProviderKind::Codex,
                        enabled: false,
                    },
                ),
            )
            .await
            .unwrap();
    }
    if case == "refused" {
        agent.continue_gate.close();
    }
    if case == "updating" {
        service.set_history_scope(directory.path().to_owned(), true);
        service.prepare_update().await.unwrap();
    }
    let effect = Effect {
        target: target.clone(),
        action: Action::Resume {
            attempt: attempt.clone(),
            cwd: cwd.clone(),
            at: *at,
            error: error.clone(),
        },
    };
    if case == "stop-after-ack" {
        let acknowledgement = agent.continue_gate.acquire().await.unwrap();
        let mut resuming = Box::pin(Runtime::execute(&service, effect));
        tokio::select! {
            () = agent.continue_started.notified() => {},
            result = &mut resuming => panic!("continuation must await acknowledgement: {result:?}"),
        }
        assert_eq!(agent.continuations.lock().unwrap().len(), 1);
        let stopped = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(Uuid::new_v4()),
                    receipt: None,
                    command: Command::Interrupt {
                        turn: Some(attempt.turn.clone()),
                    },
                },
                context,
                None,
            )
            .unwrap();
        let mut stopping = Box::pin(async {
            for action in stopped.step.actions {
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
        });
        assert!(futures_util::poll!(&mut stopping).is_pending());
        drop(acknowledgement);
        let (resumed, ()) = tokio::join!(resuming, stopping);
        resumed.unwrap();
        let native_turn = storage
            .execution(&target, attempt.id)
            .unwrap()
            .unwrap()
            .native_turn
            .unwrap();
        assert_eq!(
            *agent.interrupts.lock().unwrap(),
            [(native.id.clone(), native_turn.clone())]
        );
        assert!(matches!(
            storage.state(&target).unwrap().turns[&attempt.turn].state,
            TurnState::Running { interrupt: Some(_) }
        ));
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: native_turn,
                        status: agent_protocol::execution::TurnStatus::Interrupted,
                        ..Default::default()
                    },
                    completed: true,
                },
            },
            Some(attempt.id),
        )
        .await
        .unwrap();
    } else if case.starts_with("stop") {
        let workspace = service.inner.worktree_access.write().await;
        let mut resuming = Box::pin(Runtime::execute(&service, effect));
        if case == "stop" {
            assert!(futures_util::poll!(&mut resuming).is_pending());
        }
        let stopped = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(Uuid::new_v4()),
                    receipt: None,
                    command: Command::Interrupt {
                        turn: Some(attempt.turn.clone()),
                    },
                },
                context,
                None,
            )
            .unwrap();
        // Stop gets the source lock before Resume gets workspace access.
        for action in stopped.step.actions {
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
        drop(workspace);
        resuming.await.unwrap();
    } else {
        Runtime::execute(&service, effect).await.unwrap();
    }
    assert!(
        agent.inputs.lock().unwrap().is_empty(),
        "capacity continuation must never replay user input"
    );
    let state = storage.state(&target).unwrap();
    if case.starts_with("stop") {
        assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
    } else if case == "continued" {
        assert_eq!(
            state.turns[&attempt.turn].state,
            TurnState::Running { interrupt: None }
        );
        assert_eq!(
            *agent.continuations.lock().unwrap(),
            [(native.id.clone(), attempt.id)]
        );
        let native_turn = storage
            .execution(&target, attempt.id)
            .unwrap()
            .unwrap()
            .native_turn
            .unwrap();
        assert_ne!(native_turn.as_str(), "first");
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: native_turn,
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
        let completed = storage.state(&target).unwrap();
        assert_eq!(completed.turns.len(), 1);
        assert_eq!(completed.turns[&attempt.turn].state, TurnState::Completed);
    } else if case == "updating" {
        assert_eq!(
            state.turns[&attempt.turn].state,
            TurnState::Waiting(decision::Wait::Retry { at: *at })
        );
        assert_eq!(
            state.turns[&attempt.turn].error.as_ref().unwrap().message,
            "original overload"
        );
    } else {
        let TurnState::Failed(error) = &state.turns[&attempt.turn].state else {
            panic!("unsafe resume must end without scheduling another retry")
        };
        assert!(error.retry.is_none());
        assert_eq!(error.message, "original overload", "case {case}");
    }
}

#[test]
fn pending_answer_admission_enforces_size_and_per_conversation_and_host_limits() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(
        Arc::new(Storage::open(&directory.path().join("state.sqlite"))),
        [],
    );
    let answer = Arc::new(super::super::native_fixture::AnswerResource);
    let pending = |conversation: usize| PendingRequest {
        body: agent_protocol::requests::RequestBody::Question { questions: vec![] },
        target: SessionRef::new(None, Uuid::from_u128(conversation as u128 + 1).to_string())
            .unwrap(),
        attempt: Attempt {
            id: Uuid::new_v4(),
            turn: "turn".into(),
            provider: ProviderKind::Codex,
            source: "native".into(),
        },
        origin: RequestOrigin {
            instance: Uuid::new_v4(),
            native_id: serde_json::json!(1),
            provider: ProviderKind::Codex,
            source: answer.clone(),
        },
    };
    assert!(
        runtime
            .retain_request("oversized".into(), 64 * 1024 + 1, pending(0))
            .is_err()
    );
    for conversation in 0..8 {
        for request in 0..32 {
            assert!(
                runtime
                    .retain_request(
                        format!("{conversation}:{request}").into(),
                        64 * 1024,
                        pending(conversation)
                    )
                    .unwrap()
            );
        }
        if conversation == 0 {
            assert!(
                runtime
                    .retain_request("conversation-overflow".into(), 1, pending(0))
                    .is_err()
            );
        }
    }
    // A duplicate never consumes another slot, including at the host limit.
    let duplicate_resource = Arc::new(super::super::native_fixture::AnswerResource);
    let mut duplicate = pending(0);
    duplicate.origin.source = duplicate_resource.clone();
    assert!(
        !runtime
            .retain_request("0:0".into(), 64 * 1024, duplicate)
            .unwrap()
    );
    assert_eq!(
        Arc::strong_count(&duplicate_resource),
        1,
        "a duplicate cannot replace the original native answer handle"
    );
    assert!(
        runtime
            .retain_request("host-overflow".into(), 1, pending(8))
            .is_err()
    );
    drop(runtime);
    assert_eq!(
        Arc::strong_count(&answer),
        1,
        "all admitted native resources are released with their owner"
    );
}

#[test]
fn a_new_foreground_turn_cannot_inherit_or_hide_the_previous_turns_background_work() {
    let source = Source {
        target: SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap(),
        session_id: Uuid::new_v4().to_string(),
        io: Default::default(),
        updates: Default::default(),
        text: Default::default(),
        background: Default::default(),
        images: Default::default(),
    };
    let attempt = |turn: &str| Attempt {
        id: Uuid::new_v4(),
        turn: turn.into(),
        provider: ProviderKind::Codex,
        source: "native".into(),
    };
    let first = attempt("first");
    let second = attempt("second");
    let task = BackgroundTask {
        id: "monitor".into(),
        kind: BackgroundTaskKind::Command,
        description: None,
    };
    assert_eq!(
        source.roster(first.id, vec![task.clone()]),
        vec![(first.id, vec![task.clone()], false)]
    );
    assert_eq!(
        source.roster(second.id, vec![task.clone()]),
        vec![(first.id, vec![task.clone()], false)]
    );
    assert_eq!(source.background(first.id), vec![task]);
    assert!(source.background(second.id).is_empty());
    assert_eq!(
        source.roster(second.id, vec![]),
        vec![(first.id, vec![], false)]
    );
    assert!(source.background(first.id).is_empty());
}
