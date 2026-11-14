use super::*;
use agent_protocol::{
    conversation::{Action, Command, Context, Event, InputContent, OperationId},
    models::{Thread, ThreadResponse, Turn, TurnStatus},
    session::{ProviderKind, SessionRef},
};

#[tokio::test]
async fn update_without_an_installed_codex_records_no_interrupted_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let codex = crate::providers::codex::Codex::new(Err("not installed".into()));
    let service = HostRpcService::new([codex.into()], directory.path().join("projects.json"));
    service.set_history_scope(directory.path().to_owned(), true);
    service.prepare_update().await.unwrap();
    let saved: Recovery =
        serde_json::from_slice(&std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap())
            .unwrap();
    assert!(saved.tasks.is_empty());
}

#[tokio::test]
async fn an_uncertain_update_continuation_is_reported_without_resending() {
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("bex-worktrees.json"));
    let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let path = directory.path().join(RECOVERY_FILE);
    agent_local_host::save_private_json(
        &path,
        &Recovery {
            tasks: vec![InterruptedTask {
                native_session: SessionRef::new(ProviderKind::Codex, "native".into()).unwrap(),
                cwd: None,
                submission: op::Submission {
                    thread_id: target.clone(),
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    input: vec![],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                last_turn: None,
                delivery: ContinuationDelivery::Sending,
            }],
        },
    )
    .unwrap();
    service.resume_after_update(directory.path()).await.unwrap();
    let saved = std::fs::read(&path).unwrap();
    let report: Recovery = serde_json::from_slice(&saved).unwrap();
    assert!(
        matches!(&report.tasks[0].delivery, ContinuationDelivery::Failed(message)
        if message == "継続の配送結果を確認できません。会話を開いて確認してください。")
    );
    assert!(
        service
            .inner
            .conversation
            .storage
            .lookup(&target)
            .unwrap()
            .is_none()
    );
    service.resume_after_update(directory.path()).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), saved);
}

#[tokio::test]
async fn update_handover_continues_running_work_once_and_settles_stopped_or_retrying_work() {
    for mode in ["stopped", "retrying", "running"] {
        use crate::host_rpc::runtime::{Runtime, native_fixture::NativeAgent};
        use agent_protocol::conversation::{Observation, TurnState};
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        service.set_history_scope(directory.path().into(), true);
        let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let storage = &service.inner.conversation.storage;
        storage
            .import(
                &target,
                &ThreadResponse {
                    thread: Thread {
                        id: Some(target.clone()),
                        active_provider: Some(ProviderKind::Codex),
                        cwd: Some(directory.path().to_str().unwrap().into()),
                        turns: Some(vec![]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(&native),
                100,
            )
            .unwrap();
        let context = Context {
            preparation: None,
            rewind_binding: None,
            imported_turns: &[],
            now: 100,
            seed: uuid::Uuid::new_v4(),
            steers: true,
            steer_ready: true,
            external_busy: false,
        };
        let input = uuid::Uuid::new_v4();
        let admitted = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(input),
                    receipt: None,
                    command: Command::Submit {
                        id: input.to_string().into(),
                        content: InputContent {
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
        } = &admitted.step.actions[0]
        else {
            panic!("delivery")
        };
        storage
            .decide(
                &target,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: Some("native-turn".into()),
                },
                context,
                None,
            )
            .unwrap();
        if mode == "retrying" {
            storage
                .decide(
                    &target,
                    Event::Provider {
                        attempt: attempt.id,
                        native_turn: None,
                        native_items: vec![],
                        observation: Observation::Failed(Some(
                            agent_protocol::execution::ExecutionError {
                                message: "overloaded".into(),
                                provider_code: Some("serverOverloaded".into()),
                                ..Default::default()
                            },
                        )),
                    },
                    context,
                    None,
                )
                .unwrap();
        } else if mode == "stopped" {
            storage
                .decide(
                    &target,
                    Event::Command {
                        id: OperationId::Client(uuid::Uuid::new_v4()),
                        receipt: None,
                        command: Command::Interrupt {
                            turn: Some(attempt.turn.clone()),
                        },
                    },
                    context,
                    None,
                )
                .unwrap();
        }
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    status: agent_protocol::models::SessionStatus::Idle,
                    turns: Some(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let turn = attempt.turn.clone();
        service.prepare_update().await.unwrap();
        let operation = uuid::Uuid::new_v4();
        let rejected = Runtime::change(
            &service,
            &agent_protocol::conversation::records::Change {
                session: target.clone(),
                operation,
                command: Command::Submit {
                    id: operation.to_string().into(),
                    content: InputContent {
                        parts: vec![op::Input::Text {
                            text: "must not start during update".into(),
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
        .unwrap_err();
        assert_eq!(rejected.code, "host_updating");
        assert!(storage.receipts(&target, &[operation]).unwrap().is_empty());
        drop(service);
        let service = HostRpcService::new(
            [agent.restarted().backend()],
            directory.path().join("projects.json"),
        );
        service.set_history_scope(directory.path().into(), true);
        let _connection = service.open_session();
        let storage = &service.inner.conversation.storage;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.resume_after_update(directory.path()),
        )
        .await
        .unwrap()
        .unwrap();
        let report: Recovery = serde_json::from_slice(
            &std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(report.tasks.len(), 1);
        if mode == "running" {
            assert!(matches!(
                report.tasks[0].delivery,
                ContinuationDelivery::Accepted(_)
            ));
            service.resume_after_update(directory.path()).await.unwrap();
            assert_eq!(
                agent.inputs.lock().unwrap().len(),
                1,
                "the durable continuation runs once"
            );
        } else {
            assert_eq!(report.tasks[0].delivery, ContinuationDelivery::Completed);
            assert!(agent.inputs.lock().unwrap().is_empty());
        }
        assert!(storage.continuations(&target).unwrap().is_empty());
        if mode == "stopped" {
            assert_eq!(
                storage.state(&target).unwrap().turns[&turn].state,
                TurnState::Interrupted
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_original_turn_does_not_confirm_a_pending_update_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let storage = &service.inner.conversation.storage;
    storage
        .import(
            &target,
            &ThreadResponse {
                thread: Thread {
                    id: Some(target.clone()),
                    active_provider: Some(ProviderKind::Codex),
                    cwd: Some("/project".into()),
                    turns: Some(vec![std::sync::Arc::new(Turn {
                        id: "original".into(),
                        status: TurnStatus::Interrupted,
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(&native),
            100,
        )
        .unwrap();
    let input = uuid::Uuid::new_v4();
    let path = directory.path().join(RECOVERY_FILE);
    agent_local_host::save_private_json(
        &path,
        &Recovery {
            tasks: vec![InterruptedTask {
                native_session: target.clone(),
                cwd: None,
                submission: op::Submission {
                    thread_id: target.clone(),
                    client_user_message_id: input.to_string().into(),
                    input: vec![],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                last_turn: Some("original".into()),
                delivery: ContinuationDelivery::Pending,
            }],
        },
    )
    .unwrap();
    let mut resumed = Box::pin(service.resume_after_update(directory.path()));
    assert!(futures_util::poll!(&mut resumed).is_pending());
    let read = || serde_json::from_slice::<Recovery>(&std::fs::read(&path).unwrap()).unwrap();
    assert!(matches!(
        read().tasks[0].delivery,
        ContinuationDelivery::Pending
    ));
    let context = Context {
        preparation: None,
        rewind_binding: None,
        imported_turns: &[],
        now: 200,
        seed: uuid::Uuid::new_v4(),
        steers: true,
        steer_ready: true,
        external_busy: false,
    };
    let admitted = storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(input),
                receipt: None,
                command: Command::Submit {
                    id: input.to_string().into(),
                    content: InputContent {
                        parts: vec![op::Input::Text {
                            text: "Continue".into(),
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
    } = &admitted.step.actions[0]
    else {
        panic!("continuation delivery")
    };
    storage
        .decide(
            &target,
            Event::Delivered {
                id: id.clone(),
                action: *action,
                native_turn: Some("continued".into()),
            },
            context,
            None,
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(50)).await;
    resumed.await.unwrap();
    assert!(
        matches!(&read().tasks[0].delivery,ContinuationDelivery::Accepted(turn) if turn==&attempt.turn)
    );
    service.resume_after_update(directory.path()).await.unwrap();
    assert_eq!(
        storage
            .history(&target, 0)
            .unwrap()
            .thread
            .turns
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test(start_paused = true)]
async fn a_new_update_serializes_its_report_and_only_a_committed_update_stops_the_prior_reader()
{
    for failed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        service.set_history_scope(directory.path().into(), true);
        let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        service
            .inner
            .conversation
            .storage
            .create(
                &target,
                &agent_protocol::conversation::records::Header {
                    state: agent_protocol::conversation::Thread {
                        name: "prior update".into(),
                        workspace: agent_protocol::conversation::Workspace::Missing,
                        provider: ProviderKind::Codex,
                        queue_held: false,
                    },
                    details: Default::default(),
                },
            )
            .unwrap();
        if failed {
            rusqlite::Connection::open(directory.path().join("bex-conversations.sqlite")).unwrap().execute_batch("CREATE TRIGGER reject_new_update BEFORE UPDATE OF updating ON store WHEN NEW.updating=1 BEGIN SELECT RAISE(ABORT,'update storage unavailable'); END;").unwrap();
        }
        let path = directory.path().join(RECOVERY_FILE);
        agent_local_host::save_private_json(
            &path,
            &Recovery {
                tasks: vec![InterruptedTask {
                    native_session: target.clone(),
                    cwd: None,
                    submission: op::Submission {
                        thread_id: target.clone(),
                        client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                        input: vec![],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                    last_turn: None,
                    delivery: ContinuationDelivery::Pending,
                }],
            },
        )
        .unwrap();
        let previous = std::fs::read(&path).unwrap();
        let report = service.inner.conversation.update_report.lock().await;
        let mut resumed = Box::pin(service.resume_after_update(directory.path()));
        assert!(futures_util::poll!(&mut resumed).is_pending());
        let mut preparing = Box::pin(service.prepare_update());
        assert!(futures_util::poll!(&mut preparing).is_pending());
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        drop(report);
        // Preparation is admitted but hasn't replaced the old report yet.
        assert!(futures_util::poll!(&mut resumed).is_pending());
        let prepared = preparing.await;
        if failed {
            assert!(prepared.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), previous);
            assert!(
                !service
                    .inner
                    .updating
                    .load(std::sync::atomic::Ordering::Acquire)
            );
            service.update_recovery_committed(&target).await.unwrap();
            tokio::time::advance(std::time::Duration::from_millis(50)).await;
            resumed.await.unwrap();
            let report: Recovery =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(report.tasks.len(), 1);
            assert!(matches!(
                report.tasks[0].delivery,
                ContinuationDelivery::Completed
            ));
        } else {
            prepared.unwrap();
            tokio::time::advance(std::time::Duration::from_millis(50)).await;
            resumed.await.unwrap();
            let report: Recovery =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(report.tasks.is_empty());
        }
    }
}
#[tokio::test]
async fn a_report_write_failure_rolls_back_both_update_gates_and_preserves_existing_contents() {
    let directory = tempfile::tempdir().unwrap();
    let reports = directory.path().join("reports");
    let path = reports.join(RECOVERY_FILE);
    std::fs::create_dir_all(&path).unwrap();
    let marker = path.join("keep");
    std::fs::write(&marker, b"existing contents").unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    service.set_history_scope(reports, true);
    assert!(service.prepare_update().await.is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), b"existing contents");
    assert!(
        !service
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(!service.inner.conversation.storage.updating().unwrap());
}
