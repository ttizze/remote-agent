use super::*;
use crate::host_rpc::runtime::native_fixture::NativeAgent;
use agent_protocol::{
    execution::{ItemStatus, SessionStatus, TurnStatus},
    items::{Item, ItemBody},
    models,
};

#[rstest::rstest]
#[case(ProviderKind::Codex)]
#[case(ProviderKind::Claude)]
#[case(ProviderKind::DeepSeek)]
#[tokio::test]
async fn explicit_project_import_reads_an_unowned_native_transcript_once(
    #[case] provider: ProviderKind,
) {
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(provider, root.path().join("native"));
    let native = SessionRef::new(provider, "external".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        models::ThreadResponse {
            thread: models::Thread {
                id: Some(native.clone()),
                name: Some("Native conversation".into()),
                status: SessionStatus::Running,
                cwd: Some(root.path().to_str().unwrap().into()),
                updated_at: Some(100.0),
                turns: Some(vec![Arc::new(models::Turn {
                    id: "native-turn".into(),
                    status: TurnStatus::Completed,
                    started_at_ms: Some(10_000),
                    items: Some(vec![Arc::new(Item::new(
                        "native-answer".into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: "the native answer".into(),
                            phase: Default::default(),
                        },
                    ))]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            model: Some(models::ModelRef {
                provider,
                id: "native-model".into(),
            }),
            ..Default::default()
        },
    );
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let storage = &service.inner.conversation.storage;
    assert!(storage.shell_snapshot().unwrap().threads.is_empty());
    service.inner.projects.register(root.path()).await.unwrap();
    Runtime::import_project_threads(&service, &[root.path().to_str().unwrap().into()])
        .await
        .unwrap();
    let shell = storage.shell_snapshot().unwrap();
    assert_eq!(shell.threads.len(), 1);
    assert_eq!(
        shell.threads[0].native_status,
        Some(model::ProviderThreadStatus::Idle)
    );
    let header = &shell.threads[0];
    assert_eq!(
        model::shell::public_session_ref(&header.thread.id).unwrap(),
        native
    );
    assert_eq!(header.thread.title, "Native conversation");
    assert_eq!(header.thread.model_selection.model, "native-model");
    assert_eq!(
        header.thread.history_origin,
        Some(model::ThreadHistoryOrigin::V1Import)
    );
    assert_eq!(header.item_count, 1);
    assert!(
        storage
            .thread_state(&native.application_id(), &[])
            .unwrap()
            .runs
            .is_empty()
    );
    Runtime::ensure_imported(&service, &native).await.unwrap();
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert_eq!(
        state.thread.history_origin,
        Some(model::ThreadHistoryOrigin::V1Import)
    );
    assert_eq!(state.thread.created_at, "1970-01-01T00:00:10.000Z");
    assert!(state.runs.is_empty());
    assert!(state.run_attempts.is_empty());
    let stream = storage.thread_stream(&native, None).unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &stream[0] else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.projection.turn_items.len(), 1);
    assert!(
        matches!(&snapshot.projection.turn_items[0].body,model::TurnItemBody::AssistantMessage {text,..} if text=="the native answer")
    );
    assert!(snapshot.projection.turn_items[0].run_id.is_none());
    let _blocked = agent.open_gate.acquire().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        Runtime::ensure_imported(&service, &native),
    )
    .await
    .expect("the completed import must not read history again")
    .unwrap();
    assert_eq!(stream, storage.thread_stream(&native, None).unwrap());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[rstest::rstest]
#[case(SessionStatus::Idle)]
#[case(SessionStatus::Unknown)]
#[tokio::test]
async fn native_work_without_an_app_run_blocks_delivery_until_the_selected_source_allows_preparation(
    #[case] released: SessionStatus,
) {
    use agent_protocol::{
        conversation::{Command, InputContent, records},
        operations::Input,
        session::SessionChange,
    };
    let root = tempfile::tempdir().unwrap();
    let mut agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    Arc::get_mut(&mut agent).unwrap().submit_gate = Some(tokio::sync::Semaphore::new(0));
    let native = SessionRef::new(ProviderKind::Codex, "external-busy".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        models::ThreadResponse {
            thread: models::Thread {
                id: Some(native.clone()),
                status: SessionStatus::Running,
                cwd: Some(root.path().to_str().unwrap().into()),
                turns: Some(vec![]),
                ..Default::default()
            },
            model: Some(models::ModelRef {
                provider: ProviderKind::Codex,
                id: "native-model".into(),
            }),
            ..Default::default()
        },
    );
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let _client = service.open_session();
    Runtime::ensure_imported(&service, &native).await.unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Status {
                status: SessionStatus::Running,
            },
        },
        None,
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    storage
        .observe_thread(
            &native.application_id(),
            &[],
            "2026-11-14T00:00:00.000Z",
            |state| {
                let mut old = state.selected_native_thread().unwrap().clone();
                old.id = "old-provider-thread".into();
                old.native_thread_ref.as_mut().unwrap().native_id = Some("obsolete-source".into());
                old.status = model::ProviderThreadStatus::Idle;
                Ok(model::Decision {
                    changes: vec![model::Change::ProviderThread(old)],
                    effects: vec![],
                })
            },
        )
        .unwrap();
    let operation = Uuid::new_v4();
    let ack = Runtime::change(
        &service,
        &records::Change {
            session: native.clone(),
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: InputContent {
                    parts: vec![Input::Text {
                        text: "send only when the native conversation is idle".into(),
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
    assert_eq!(ack.error, None);
    let storage = &service.inner.conversation.storage;
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert_eq!(state.runs.len(), 1);
    let run = state.runs.values().next().unwrap();
    assert_eq!(
        run.status,
        model::RunStatus::Queued,
        "an application run must not overlap unobserved native work"
    );
    assert!(state.effects.values().all(|effect| !matches!(
        effect.request,
        model::EffectRequest::ProviderTurnStart { .. }
    )));
    assert!(agent.inputs.lock().unwrap().is_empty());
    let promoted = Runtime::change(
        &service,
        &records::Change {
            session: native.clone(),
            operation: Uuid::new_v4(),
            command: Command::SteerInput(operation.to_string().into()),
        },
    )
    .await
    .unwrap();
    assert!(
        promoted.error.is_some(),
        "send-now cannot bypass known external native work when there is no owned foreground turn to steer"
    );
    assert_eq!(
        storage
            .thread_state(&native.application_id(), &[])
            .unwrap()
            .runs[&run.id]
            .status,
        model::RunStatus::Queued
    );
    let before_obsolete = storage.thread_stream(&native, None).unwrap();
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: SessionRef::new(ProviderKind::Codex, "obsolete-source".into()).unwrap(),
            change: SessionChange::Status {
                status: SessionStatus::Idle,
            },
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        storage
            .thread_state(&native.application_id(), &[])
            .unwrap()
            .runs[&run.id]
            .status,
        model::RunStatus::Queued
    );
    assert_eq!(
        before_obsolete,
        storage.thread_stream(&native, None).unwrap(),
        "a historical binding must reach and be rejected by the selected-source fence"
    );
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&native.id)
        .unwrap()
        .thread
        .status = released;
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Status { status: released },
        },
        None,
    )
    .await
    .unwrap();
    let activated = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert_eq!(activated.runs[&run.id].status, model::RunStatus::Starting);
    assert_eq!(
        activated
            .effects
            .values()
            .filter(|effect| matches!(
                effect.request,
                model::EffectRequest::ProviderTurnStart { .. }
            ))
            .count(),
        1
    );
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Status { status: released },
        },
        None,
    )
    .await
    .unwrap();
    let repeated = storage.thread_state(&state.thread.id, &[]).unwrap();
    assert_eq!(
        repeated.effects.keys().collect::<Vec<_>>(),
        activated.effects.keys().collect::<Vec<_>>(),
        "duplicate idle observations must not enqueue delivery again"
    );
    let submitted = agent.submit_started.notified();
    tokio::time::timeout(std::time::Duration::from_secs(5), submitted)
        .await
        .expect("the queued input reaches the real effect worker after idle");
    let mut changed = storage.changes();
    agent.submit_gate.as_ref().unwrap().add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = storage.thread_state(&native.application_id(), &[]).unwrap();
            if state.runs[&run.id].status == model::RunStatus::Completed {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the resumed native input completes");
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].thread_id, native);
    assert!(
        matches!(&inputs[0].input[..],[Input::Text {text}] if text=="send only when the native conversation is idle")
    );
    assert!(
        agent.created_in.lock().unwrap().is_empty(),
        "the existing native conversation is reused"
    );
}

// Unknown catalog activity includes an unloaded native transcript. It keeps
// completion unconfirmed, but is not an uncertain *delivery* of an old input.
#[tokio::test]
async fn an_unknown_native_activity_can_resume_on_explicit_input_without_replaying_that_input() {
    use agent_protocol::{
        conversation::{Command, InputContent, records},
        operations::Input,
    };
    let root = tempfile::tempdir().unwrap();
    let mut agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    Arc::get_mut(&mut agent).unwrap().submit_gate = Some(tokio::sync::Semaphore::new(0));
    let native = SessionRef::new(ProviderKind::Codex, "unloaded-history".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        models::ThreadResponse {
            thread: models::Thread {
                id: Some(native.clone()),
                status: SessionStatus::Unknown,
                cwd: Some(root.path().to_str().unwrap().into()),
                turns: Some(vec![]),
                ..Default::default()
            },
            model: Some(models::ModelRef {
                provider: ProviderKind::Codex,
                id: "native-model".into(),
            }),
            ..Default::default()
        },
    );
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let _client = service.open_session();
    Runtime::ensure_imported(&service, &native).await.unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Status {
                status: SessionStatus::Unknown,
            },
        },
        None,
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    let activity = storage.activity().unwrap();
    assert!(activity.display.ongoing);
    assert_eq!(
        activity.statuses,
        [(native.clone(), SessionStatus::Unknown)]
    );
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert!(state.runs.is_empty());
    let operation = Uuid::new_v4();
    let request = records::Change {
        session: native.clone(),
        operation,
        command: Command::Submit {
            id: operation.to_string().into(),
            content: InputContent {
                parts: vec![Input::Text {
                    text: "continue this conversation".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        },
    };
    let submitted = agent.submit_started.notified();
    let ack = Runtime::change(&service, &request).await.unwrap();
    assert_eq!(ack.error, None);
    tokio::time::timeout(std::time::Duration::from_secs(5),submitted).await.expect("an unloaded history can resume without waiting for an idle event from an absent runtime");
    assert_eq!(Runtime::change(&service, &request).await.unwrap(), ack);
    let mut changed = storage.changes();
    agent.submit_gate.as_ref().unwrap().add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = storage.thread_state(&native.application_id(), &[]).unwrap();
            if state
                .runs
                .values()
                .any(|run| run.status == model::RunStatus::Completed)
            {
                assert_eq!(state.runs.len(), 1);
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the explicit input completes once");
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].thread_id, native);
    assert!(
        matches!(&inputs[0].input[..],[Input::Text {text}] if text=="continue this conversation")
    );
    assert!(agent.created_in.lock().unwrap().is_empty());
    assert!(!storage.activity().unwrap().display.ongoing);
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_native_turns_commit_real_provider_turns_without_application_runs_or_invented_input(
    #[case] terminal_body: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "external-observer".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        models::ThreadResponse {
            thread: models::Thread {
                id: Some(native.clone()),
                status: SessionStatus::Idle,
                cwd: Some(root.path().to_str().unwrap().into()),
                turns: Some(vec![]),
                ..Default::default()
            },
            model: Some(models::ModelRef {
                provider: ProviderKind::Codex,
                id: "native-model".into(),
            }),
            ..Default::default()
        },
    );
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    Runtime::ensure_imported(&service, &native).await.unwrap();
    let storage = &service.inner.conversation.storage;
    let execution = Uuid::new_v4();
    let turn_id: agent_protocol::ids::TurnId = "external-native-turn".into();
    let item = |text: &str, status| {
        Arc::new(Item::new(
            "external-answer".into(),
            status,
            ItemBody::AssistantText {
                text: text.into(),
                phase: Default::default(),
            },
        ))
    };
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Turn {
                turn: models::Turn {
                    id: turn_id.clone(),
                    status: TurnStatus::Running,
                    items: Some(vec![
                        Arc::new(Item::new(
                            "external-user".into(),
                            ItemStatus::Completed,
                            ItemBody::UserMessage {
                                text: Some("Actual native user input".into()),
                                content: vec![],
                            },
                        )),
                        item("Hello", ItemStatus::Running),
                    ]),
                    ..Default::default()
                },
                completed: false,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let active = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert!(active.runs.is_empty());
    assert!(active.run_attempts.is_empty());
    assert!(active.effects.is_empty());
    assert_eq!(active.provider_turns.len(), 1);
    let turn = active.provider_turns.values().next().unwrap();
    assert!(turn.run_attempt_id.is_none());
    assert_eq!(turn.status, model::ProviderTurnStatus::Running);
    assert_eq!(
        turn.native_turn_ref.as_ref().unwrap().native_id.as_deref(),
        Some("external-native-turn")
    );
    let root_node = &active.nodes[&turn.node_id];
    assert!(root_node.run_id.is_none());
    assert!(!root_node.counts_for_run);
    assert_eq!(
        storage.activity().unwrap().statuses,
        [(native.clone(), SessionStatus::Running)]
    );
    let model::projection::ThreadStreamItem::Snapshot(active_snapshot) =
        &storage.thread_stream(&native, None).unwrap()[0]
    else {
        panic!("snapshot")
    };
    let view = agent_core::orchestration::project(
        active_snapshot,
        &native,
        ProviderKind::Codex,
        Default::default(),
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        view.thread.turns.as_ref().unwrap()[0].status,
        TurnStatus::Running
    );
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Text {
                turn_id: turn_id.clone(),
                item_id: "external-answer".into(),
                field: agent_protocol::session::TextField::AssistantText,
                delta: " world".into(),
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let (held, ready) = tokio::sync::oneshot::channel();
    let (release, unblock) = std::sync::mpsc::channel();
    let owner = service.inner.clone();
    let blocker = tokio::task::spawn_blocking(move || {
        owner.conversation.storage.hold_connection(held, unblock)
    });
    ready.await.unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed = accepted.clone();
    // The releaser must not depend on Tokio: the faulty path can block every
    // async worker on the same synchronous database mutex.
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let immediate = completed.load(std::sync::atomic::Ordering::SeqCst);
        let _ = release.send(());
        immediate
    });
    let observed = Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Text {
                turn_id: turn_id.clone(),
                item_id: "external-answer".into(),
                field: agent_protocol::session::TextField::AssistantText,
                delta: "!".into(),
            },
        },
        Some(execution),
    )
    .await;
    accepted.store(true, std::sync::atomic::Ordering::SeqCst);
    let immediate = tokio::task::spawn_blocking(move || releaser.join().unwrap())
        .await
        .unwrap();
    blocker.await.unwrap();
    observed.unwrap();
    assert!(
        immediate,
        "a buffered text delta must be accepted while SQL is busy; only its later publication can need the writer"
    );
    let final_event = || crate::providers::AgentChange::Session {
        session: native.clone(),
        change: agent_protocol::session::SessionChange::Turn {
            turn: models::Turn {
                id: turn_id.clone(),
                status: TurnStatus::Completed,
                items: terminal_body.then(|| vec![item("Hello world!", ItemStatus::Completed)]),
                ..Default::default()
            },
            completed: true,
        },
    };
    Runtime::observe(&service, final_event(), Some(execution))
        .await
        .unwrap();
    let finished = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert!(finished.runs.is_empty());
    assert!(finished.run_attempts.is_empty());
    assert!(finished.effects.is_empty());
    assert_eq!(finished.provider_turns.len(), 1);
    assert_eq!(
        finished.provider_turns.values().next().unwrap().status,
        model::ProviderTurnStatus::Completed
    );
    assert_eq!(
        storage.activity().unwrap().statuses,
        [(native.clone(), SessionStatus::Idle)]
    );
    let before = storage.thread_stream(&native, None).unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &before[0] else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.projection.turn_items.len(), 2);
    assert!(snapshot.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::UserMessage {text,..} if text=="Actual native user input")));
    assert!(
        matches!(&snapshot.projection.turn_items.last().unwrap().body,model::TurnItemBody::AssistantMessage {text,streaming:false,..} if text=="Hello world!")
    );
    assert!(
        snapshot
            .projection
            .turn_items
            .last()
            .unwrap()
            .run_id
            .is_none()
    );
    Runtime::observe(&service, final_event(), Some(execution))
        .await
        .unwrap();
    assert_eq!(before, storage.thread_stream(&native, None).unwrap());
    let receipt: agent_protocol::ids::TurnId = finished
        .provider_turns
        .values()
        .next()
        .unwrap()
        .id
        .clone()
        .into();
    assert_eq!(
        storage.activity().unwrap().unread,
        [(native.clone(), receipt.clone())]
    );
    storage
        .mark_read(&native, &"external-native-turn".into())
        .unwrap();
    assert_eq!(before, storage.thread_stream(&native, None).unwrap());
    storage.mark_read(&native, &receipt).unwrap();
    assert!(storage.activity().unwrap().unread.is_empty());
    let read = storage.thread_stream(&native, None).unwrap();
    storage.mark_read(&native, &receipt).unwrap();
    assert_eq!(read, storage.thread_stream(&native, None).unwrap());
    assert!(agent.inputs.lock().unwrap().is_empty());
    assert!(agent.created_in.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_command_output_streams_and_settles_without_a_final_item_body() {
    use crate::host_rpc::runtime::native_fixture::{call, own};
    use agent_protocol::{
        protocol::Call,
        session::{SessionChange, TextField},
    };
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "live-command".into()).unwrap();
    let response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(native.clone()),
            cwd: Some(root.path().to_str().unwrap().into()),
            status: SessionStatus::Idle,
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
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let client = service.open_session();
    own(&service, &native, &response).await;
    let execution = Uuid::new_v4();
    let turn: agent_protocol::ids::TurnId = "native-command-turn".into();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Item {
                turn_id: turn.clone(),
                item: Arc::new(Item::new(
                    "native-command-item".into(),
                    ItemStatus::Running,
                    ItemBody::CommandExecution {
                        command: "a recorded native command".into(),
                        cwd: None,
                        output: String::new(),
                        output_omitted: false,
                        exit_code: None,
                    },
                )),
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    let events = storage.thread_stream(&native, None).unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) = &events[0] else {
        panic!("snapshot")
    };
    let item = snapshot
        .projection
        .turn_items
        .iter()
        .find(|item| {
            item.native_item_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("native-command-item")
        })
        .unwrap();
    let key = records::OutputKey {
        session: native.clone(),
        item: item.id.clone().into(),
    };
    for delta in ["progress 50%", "\rprogress 100%\x1b[K\n", "日\n"] {
        Runtime::observe(
            &service,
            AgentChange::Session {
                session: native.clone(),
                change: SessionChange::Text {
                    turn_id: turn.clone(),
                    item_id: "native-command-item".into(),
                    field: TextField::CommandOutput,
                    delta: delta.into(),
                },
            },
            Some(execution),
        )
        .await
        .unwrap();
    }
    let opened: records::OpenedOutput = call(
        &service,
        client.id(),
        Call::SubscribeCommandOutput(records::SubscribeOutput {
            key: key.clone(),
            subscription: Uuid::new_v4(),
        }),
    )
    .await
    .unwrap();
    assert!(opened.initial.sequence > 0);
    assert_eq!(opened.initial.text, "progress 100%\n日\n");
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: models::Turn {
                    id: turn,
                    status: TurnStatus::Completed,
                    items: None,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let saved = storage
        .thread_item(&native, key.item.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, model::TurnItemStatus::Completed);
    assert!(
        matches!(&saved.body,model::TurnItemBody::CommandExecution {output:Some(text),output_omitted:Some(false),..} if text=="progress 100%\n日\n")
    );
    let closed: records::OpenedOutput = call(
        &service,
        client.id(),
        Call::SubscribeCommandOutput(records::SubscribeOutput {
            key,
            subscription: Uuid::new_v4(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(closed.initial.text, "progress 100%\n日\n");
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert!(state.runs.is_empty());
    assert!(state.run_attempts.is_empty());
    assert!(state.effects.is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn project_import_cannot_replace_observed_native_work_without_an_application_run() {
    use crate::host_rpc::runtime::native_fixture::own;
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "observed-native-owner".into()).unwrap();
    let response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(native.clone()),
            status: SessionStatus::Idle,
            updated_at: Some(100.0),
            cwd: Some(root.path().to_str().unwrap().into()),
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
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    own(&service, &native, &response).await;
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: agent_protocol::session::SessionChange::Turn {
                turn: models::Turn {
                    id: "observed-native-turn".into(),
                    status: TurnStatus::Running,
                    items: None,
                    ..Default::default()
                },
                completed: false,
            },
        },
        Some(Uuid::new_v4()),
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    service.inner.projects.register(root.path()).await.unwrap();
    let before = storage.thread_header(&native).unwrap().unwrap();
    assert!(before.history_origin.is_none());
    for missing in [false, true, true] {
        if missing {
            agent.records.lock().unwrap().clear();
        }
        service.inner.projects.register(root.path()).await.unwrap();
        Runtime::import_project_threads(&service, &[root.path().to_str().unwrap().into()])
            .await
            .unwrap();
        let after = storage
            .thread_header(&native)
            .unwrap()
            .expect("actual native work owns its saved header independently of a discovery scan");
        assert_eq!(after.updated_at, before.updated_at);
        assert_eq!(
            storage.activity().unwrap().statuses,
            [(native.clone(), SessionStatus::Running)]
        );
        let state = storage.thread_state(&native.application_id(), &[]).unwrap();
        assert!(state.runs.is_empty());
        assert!(state.run_attempts.is_empty());
        assert!(state.effects.is_empty());
        assert_eq!(state.provider_turns.len(), 1);
        assert!(state.thread.deleted_at.is_none());
    }
}

#[tokio::test]
async fn project_import_preserves_a_pending_session_request_before_history_is_opened() {
    use crate::host_rpc::runtime::native_fixture::{AnswerResource, own};
    use agent_protocol::requests::{Request, RequestBody, RequestTarget};
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "session-only-owner".into()).unwrap();
    let response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(native.clone()),
            status: SessionStatus::Idle,
            updated_at: Some(100.0),
            cwd: Some(root.path().to_str().unwrap().into()),
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
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    own(&service, &native, &response).await;
    Runtime::observe(
        &service,
        AgentChange::Request {
            session: native.clone(),
            origin: RequestOrigin {
                instance: Uuid::new_v4(),
                native_id: serde_json::json!(1),
                provider: ProviderKind::Codex,
                source: Arc::new(AnswerResource),
            },
            request: Request {
                id: "native-session-request".into(),
                target: RequestTarget::Session,
                delivery: agent_protocol::session::RequestDelivery::Awaiting,
                body: RequestBody::ToolExecution {
                    tool: "inspect".into(),
                    namespace: None,
                    arguments: serde_json::json!({}),
                },
            },
        },
        None,
    )
    .await
    .unwrap();
    let storage = &service.inner.conversation.storage;
    service.inner.projects.register(root.path()).await.unwrap();
    let before = storage.thread_header(&native).unwrap().unwrap();
    let owner = storage.thread_state(&native.application_id(), &[]).unwrap();
    let session_id = owner
        .selected_native_thread()
        .unwrap()
        .provider_session_id
        .clone();
    assert!(session_id.is_some());
    assert!(before.history_origin.is_none());
    assert!(owner.provider_turns.is_empty());
    for absent in [false, true, true] {
        if absent {
            agent.records.lock().unwrap().clear();
        }
        service.inner.projects.register(root.path()).await.unwrap();
        Runtime::import_project_threads(&service, &[root.path().to_str().unwrap().into()])
            .await
            .unwrap();
        let after = storage.thread_header(&native).unwrap().expect("a real pending callback keeps its conversation discoverable even without an app or native turn");
        assert_eq!(after.updated_at, before.updated_at);
        let state = storage.thread_state(&native.application_id(), &[]).unwrap();
        assert!(state.thread.deleted_at.is_none());
        assert_eq!(
            state.selected_native_thread().unwrap().provider_session_id,
            session_id
        );
        assert!(state.runs.is_empty());
        assert!(state.run_attempts.is_empty());
        assert!(state.provider_turns.is_empty());
        assert!(state.effects.is_empty());
        assert_eq!(state.runtime_requests.len(), 1);
        assert!(
            state
                .runtime_requests
                .values()
                .all(|request| request.status == model::RequestStatus::Pending)
        );
    }
}
