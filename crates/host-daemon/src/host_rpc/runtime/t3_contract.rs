//! Behavioral migration checks against T3 Code 1a12c4afae998925b4a29417f2dc436c0ccd0969.
//! ShellStream.ts, AgentSessionImporter.ts, ThreadLaunchService.ts and
//! orchestrationV2Timeline define the expectations, independently of old Rows.
use super::native_fixture::{NativeAgent, call};
use super::*;
use agent_protocol::conversation as decision;
use agent_protocol::{orchestration as model, protocol::Call};

fn input(provider: ProviderKind, text: &str) -> decision::InputContent {
    decision::InputContent {
        fresh_context: false,
        parts: vec![op::Input::Text { text: text.into() }],
        model: Some(agent_protocol::models::ModelRef {
            provider,
            id: "fixture".into(),
        }),
        effort: None,
        service_tier: None,
        follow_up: Default::default(),
    }
}

async fn create(service: &HostRpcService, cwd: &str, text: Option<&str>) -> SessionRef {
    let connection = service.open_session();
    let session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            session: session.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(cwd.into()),
            selection: Default::default(),
            initial: text.map(|text| {
                (
                    Uuid::new_v4().to_string().into(),
                    input(ProviderKind::Codex, text),
                )
            }),
        }),
    )
    .await
    .unwrap();
    session
}

fn snapshot(service: &HostRpcService, session: &SessionRef) -> model::projection::ThreadSnapshot {
    let stream = service
        .inner
        .conversation
        .storage
        .thread_stream(session, None)
        .unwrap();
    let model::projection::ThreadStreamItem::Snapshot(snapshot) =
        stream.into_iter().next().unwrap()
    else {
        panic!("snapshot")
    };
    snapshot
}

fn assert_fork_preserves_source(
    current: model::projection::ThreadSnapshot,
    before: &model::projection::ThreadSnapshot,
    target: &SessionRef,
) {
    // ProjectionStore.ts4253-4270 exposes outgoing transfers on the source too.
    // The fork adds this link; shared provider-session metadata may also change
    // when another thread uses the same runtime. The source owns its thread,
    // execution, native thread and transcript, which must stay intact.
    let transfer = current
        .projection
        .context_transfers
        .iter()
        .find(|transfer| {
            transfer.kind == model::TransferType::Fork
                && transfer.source_thread_id == before.projection.thread.id
                && transfer.target_thread_id == target.application_id()
        })
        .expect("outgoing fork context transfer");
    assert_eq!(
        transfer.source_point.run_id.as_ref(),
        Some(&before.projection.runs[0].id)
    );
    assert_eq!(current.snapshot_sequence, before.snapshot_sequence);
    assert_eq!(current.projection.thread, before.projection.thread);
    assert_eq!(current.projection.runs, before.projection.runs);
    assert_eq!(current.projection.attempts, before.projection.attempts);
    assert_eq!(
        current.projection.provider_threads,
        before.projection.provider_threads
    );
    assert_eq!(
        current.projection.visible_turn_items,
        before.projection.visible_turn_items
    );
}

async fn settled(
    service: &HostRpcService,
    session: &SessionRef,
    count: usize,
) -> model::projection::ThreadSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = snapshot(service, session);
            if snapshot.projection.runs.len() == count
                && snapshot
                    .projection
                    .runs
                    .iter()
                    .all(|run| run.status == model::RunStatus::Completed)
            {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native run and checkpoint settle")
}

async fn shell(service: &HostRpcService) -> serde_json::Value {
    let connection = service.open_session();
    let opened: records::OpenedList = call(
        service,
        connection.id(),
        Call::SubscribeList(records::SubscribeList {
            refresh: false,
            subscription: Uuid::new_v4(),
            after_sequence: None,
        }),
    )
    .await
    .unwrap();
    let value = serde_json::to_value(opened).unwrap();
    value["initial"]
        .as_array()
        .expect("T3 shell stream items")
        .iter()
        .find(|item| item["kind"] == "snapshot")
        .expect("shell snapshot")["snapshot"]
        .clone()
}

#[tokio::test]
async fn native_source_queries_use_canonical_ownership_across_provider_switches() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(&service, directory.path().to_str().unwrap(), Some("first")).await;
    settled(&service, &session, 1).await;
    let codex_native = codex.inputs.lock().unwrap()[0].thread_id.clone();
    let storage = &service.inner.conversation.storage;
    assert_eq!(
        storage.active_source(&session).unwrap(),
        Some(codex_native.clone())
    );
    assert_eq!(
        storage.native_owner(&codex_native).unwrap(),
        Some(session.clone())
    );
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Claude, "second"),
            },
        }),
    )
    .await
    .unwrap();
    ack.accepted().unwrap();
    settled(&service, &session, 2).await;
    let claude_native = claude.inputs.lock().unwrap()[0].thread_id.clone();
    assert_eq!(
        storage.active_source(&session).unwrap(),
        Some(claude_native.clone())
    );
    assert_eq!(
        storage.native_owner(&codex_native).unwrap(),
        Some(session.clone())
    );
    assert_eq!(
        storage.native_owner(&claude_native).unwrap(),
        Some(session.clone())
    );
    let private = storage.active_sources().unwrap();
    assert!(private.contains(&codex_native));
    assert!(private.contains(&claude_native));
    let foreign = SessionRef::new(ProviderKind::Claude, codex_native.id.clone()).unwrap();
    assert_eq!(storage.native_owner(&foreign).unwrap(), None);
    let owners = storage
        .native_owners([&codex_native, &claude_native, &foreign])
        .unwrap();
    assert_eq!(owners.len(), 2);
    assert_eq!(owners[&codex_native], session);
    assert_eq!(owners[&claude_native], session);
    // Replacing the current native reference must not release the previous
    // identity: accepted attempts still own that provider conversation.
    let state = storage.thread_state(&session.id, &[]).unwrap();
    let original = state
        .provider_threads
        .values()
        .find(|thread| thread.provider_instance_id == ProviderKind::Codex.key())
        .unwrap();
    let mut rebound = original.clone();
    rebound.native_thread_ref.as_mut().unwrap().native_id = Some("codex-replacement".into());
    let now = commands::at();
    storage
        .observe_thread(&session.id, &[], &now, |_| {
            Ok(model::Decision {
                changes: vec![model::Change::ProviderThread(rebound.clone())],
                ..Default::default()
            })
        })
        .unwrap();
    assert_eq!(
        storage.native_owner(&codex_native).unwrap(),
        Some(session.clone())
    );
    assert!(storage.active_sources().unwrap().contains(&codex_native));
    let replacement = SessionRef::new(ProviderKind::Codex, "codex-replacement".into()).unwrap();
    assert!(storage.active_sources().unwrap().contains(&replacement));
    let other = create(&service, directory.path().to_str().unwrap(), None).await;
    let mut stolen = original.clone();
    stolen.id = "foreign-claim".into();
    stolen.app_thread_id = Some(other.application_id());
    let error = storage
        .observe_thread(&other.id, &[], &now, |_| {
            Ok(model::Decision {
                changes: vec![model::Change::ProviderThread(stolen)],
                ..Default::default()
            })
        })
        .unwrap_err();
    assert!(error.contains("belongs to another app thread"), "{error}");
    assert_eq!(storage.native_owner(&codex_native).unwrap(), Some(session));
}

#[tokio::test]
async fn created_and_renamed_threads_are_shared_as_t3_shells() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(&service, directory.path().to_str().unwrap(), None).await;
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::Rename("Shared title".into()),
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    assert_eq!(
        snapshot(&service, &session).projection.thread.title,
        "Shared title"
    );
    let shell = shell(&service).await;
    let thread = shell["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["id"] == session.application_id())
        .expect("created thread is listed");
    assert_eq!(thread["title"], "Shared title");
    assert_eq!(thread["itemCount"], 0);
    assert_eq!(thread["status"], "idle");
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn switching_providers_carries_prior_history_without_duplicate_input() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("prior-history-marker"),
    )
    .await;
    let first = settled(&service, &session, 1).await;
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Claude, "continue-marker"),
            },
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    let second = settled(&service, &session, 2).await;
    assert_eq!(codex.inputs.lock().unwrap().len(), 1);
    assert_eq!(claude.inputs.lock().unwrap().len(), 1);
    assert_eq!(second.projection.thread.id, first.projection.thread.id);
    assert!(!second.projection.context_handoffs.is_empty());
    // ContextHandoffDelivery uses inline history when the adapter explicitly
    // declines native injection (Claude's fixture, like the real adapter).
    let delivered = serde_json::to_string(&(
        &*claude.injected.lock().unwrap(),
        &*claude.inputs.lock().unwrap(),
    ))
    .unwrap();
    assert!(delivered.contains("prior-history-marker"));
}

#[tokio::test]
async fn a_fork_keeps_the_selected_prefix_and_independent_native_resources() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("prefix-marker"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let forked: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(forked, target);
    let fork = snapshot(&service, &target);
    assert_eq!(
        fork.projection.thread.lineage.parent_thread_id,
        Some(source.application_id())
    );
    assert!(fork.projection.visible_turn_items.iter().any(|item|item.visibility==model::projection::Visibility::Inherited && matches!(&item.item.body,model::TurnItemBody::UserMessage {text,..} if text=="prefix-marker")));
    assert!(fork.projection.runs.is_empty());
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_fork_preserves_source(snapshot(&service, &source), &first, &target);
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Codex, "fork-only-marker"),
            },
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    let continued = settled(&service, &target, 1).await;
    assert_eq!(continued.projection.runs[0].ordinal, 1);
    // Orchestrator.ts8300-8327 binds native fork provenance at dispatch.
    let provider_thread = continued
        .projection
        .provider_threads
        .iter()
        .find(|thread| Some(&thread.id) == continued.projection.runs[0].provider_thread_id.as_ref())
        .unwrap();
    let origin = provider_thread
        .forked_from
        .as_ref()
        .expect("native fork provenance");
    assert_eq!(
        Some(&origin.provider_thread_id),
        first.projection.runs[0].provider_thread_id.as_ref()
    );
    assert_eq!(
        origin.provider_turn_id,
        first.projection.attempts[0].provider_turn_id
    );
    assert!(
        continued
            .projection
            .context_transfers
            .iter()
            .any(
                |transfer| transfer.status == model::TransferStatus::Consumed
                    && matches!(
                        transfer.resolution,
                        Some(model::ContextTransferResolution::NativeFork { .. })
                    )
            )
    );
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_ne!(inputs[0].thread_id, inputs[1].thread_id);
    assert_fork_preserves_source(snapshot(&service, &source), &first, &target);
}

#[tokio::test]
async fn an_empty_launch_honors_the_selected_existing_worktree_without_sending_input() {
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("selected");
    std::fs::create_dir(&selected).unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: Some(
                agent_protocol::workspace::WorkspaceStrategy::ExistingWorktree {
                    path: selected.to_str().unwrap().into(),
                },
            ),
            session: session.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: None,
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &session);
            if current.projection.thread.worktree_path.as_deref() == selected.to_str() {
                assert!(current.projection.runs.is_empty());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("empty launch selects its worktree");
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[rstest::rstest]
#[case("inputs")]
#[case("turns")]
#[case("items")]
#[case("journal")]
#[case("bodies")]
#[case("jobs")]
#[case("executions")]
#[case("store")]
#[case("threads")]
#[case("requests")]
#[case("receipts")]
#[case("sources")]
#[case("bindings")]
#[case("item_natives")]
#[tokio::test]
async fn obsolete_storage_is_absent(#[case] table: &str) {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    create(&service, directory.path().to_str().unwrap(), None).await;
    let connection =
        rusqlite::Connection::open(directory.path().join(crate::CONVERSATION_DATABASE_FILE))
            .unwrap();
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists, "obsolete storage table remains: {table}");
}

pub(in crate::host_rpc) async fn running(
    service: &HostRpcService,
    agent: &NativeAgent,
    cwd: &str,
) -> (SessionRef, SessionRef, Uuid) {
    agent
        .fail_submit
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let session = create(service, cwd, Some("owned-input-marker")).await;
    let (native,execution)=tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let current=snapshot(service,&session);
            if current.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")) {
                let attempt=&current.projection.attempts[0];
                let native=current.projection.provider_threads.iter().find(|thread|thread.id==attempt.provider_thread_id)
                    .unwrap().native_thread_ref.as_ref().unwrap().native_id.clone().unwrap();
                break (SessionRef::new(ProviderKind::Codex,native).unwrap(),Uuid::parse_str(&attempt.id).unwrap());
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    observe(
        service,
        &native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status: agent_protocol::execution::TurnStatus::Running,
                ..Default::default()
            },
            completed: false,
        },
    )
    .await;
    assert_eq!(
        snapshot(service, &session).projection.runs[0].status,
        model::RunStatus::Running
    );
    (session, native, execution)
}

async fn observe(
    service: &HostRpcService,
    native: &SessionRef,
    execution: Uuid,
    change: SessionChange,
) {
    Runtime::observe(
        service,
        AgentChange::Session {
            session: native.clone(),
            change,
        },
        Some(execution),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn provider_stop_settles_the_owned_run_and_preserves_the_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, _, _) = running(&service, &agent, directory.path().to_str().unwrap()).await;
    Runtime::observe(
        &service,
        AgentChange::Stopped {
            provider: ProviderKind::Codex,
            error: agent_protocol::execution::ExecutionError {
                message: "provider-stopped-marker".into(),
                ..Default::default()
            },
        },
        None,
    )
    .await
    .unwrap();
    let stopped = snapshot(&service, &session);
    assert_eq!(stopped.projection.thread.id, session.application_id());
    assert_eq!(stopped.projection.runs[0].status, model::RunStatus::Failed);
    assert!(
        stopped
            .projection
            .provider_sessions
            .iter()
            .all(|session| session.status != model::ProviderSessionStatus::Ready)
    );
    assert!(stopped.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::UserMessage {text,..} if text=="owned-input-marker")));
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn a_late_creation_reply_cannot_publish_ready_after_provider_process_loss(
    #[case] cancelled_first: bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex);
        let held = agent.create_response_gate.acquire().await.unwrap();
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let session = create(
            &service,
            directory.path().to_str().unwrap(),
            Some("unsent-marker"),
        )
        .await;
        agent.created.notified().await;
        let physical = agent.records.lock().unwrap().keys().next().unwrap().clone();
        if cancelled_first {
            let connection = service.open_session();
            let run = snapshot(&service, &session).projection.runs[0].id.clone();
            let ack: records::Acknowledged = call(
                &service,
                connection.id(),
                Call::ChangeConversation(records::Change {
                    session: session.clone(),
                    operation: Uuid::new_v4(),
                    command: Command::Interrupt {
                        turn: Some(run.into()),
                    },
                }),
            )
            .await
            .unwrap();
            assert_eq!(ack.error, None);
        }
        Runtime::observe(
            &service,
            AgentChange::Stopped {
                provider: ProviderKind::Codex,
                error: agent_protocol::execution::ExecutionError {
                    message: "process was lost".into(),
                    ..Default::default()
                },
            },
            None,
        )
        .await
        .unwrap();
        let stopped = snapshot(&service, &session);
        assert_eq!(
            stopped.projection.runs[0].status,
            if cancelled_first {
                model::RunStatus::Cancelled
            } else {
                model::RunStatus::Failed
            }
        );
        drop(held);
        let mut changed = service.inner.conversation.storage.changes();
        loop {
            let current = snapshot(&service, &session);
            if current.projection.provider_threads.iter().any(|thread| {
                thread
                    .native_thread_ref
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_ref())
                    == Some(&physical)
            }) {
                assert_eq!(
                    current.projection.runs, stopped.projection.runs,
                    "late I/O cannot reopen app work"
                );
                assert_eq!(
                    current.projection.attempts, stopped.projection.attempts,
                    "no native input was accepted"
                );
                assert!(
                    current
                        .projection
                        .provider_sessions
                        .iter()
                        .all(|session| session.status != model::ProviderSessionStatus::Ready),
                    "a late reply cannot reverse the process-loss fact"
                );
                assert!(
                    current
                        .projection
                        .provider_threads
                        .iter()
                        .all(|thread| thread.status != model::ProviderThreadStatus::Idle),
                    "the physical identity is retained without claiming a live resource"
                );
                assert!(agent.inputs.lock().unwrap().is_empty());
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("late allocation settles without sending input");
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn provider_stop_saves_live_command_output_without_reopening_a_completed_run(
    #[case] background: bool,
) {
    use agent_protocol::{
        execution::{BackgroundTask, BackgroundTaskKind, ItemStatus, TurnStatus},
        items::{Item, ItemBody},
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    observe(
        &service,
        &native,
        execution,
        SessionChange::Item {
            turn_id: "observed-turn".into(),
            item: Arc::new(Item::new(
                "live-command".into(),
                ItemStatus::Running,
                ItemBody::CommandExecution {
                    command: "printf output".into(),
                    cwd: None,
                    output: "initial".into(),
                    output_omitted: false,
                    exit_code: None,
                },
            )),
        },
    )
    .await;
    let before = snapshot(&service, &session);
    let item=before.projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::CommandExecution{input,..} if input=="printf output")).unwrap().id.clone();
    observe(
        &service,
        &native,
        execution,
        SessionChange::Text {
            turn_id: "observed-turn".into(),
            item_id: "live-command".into(),
            field: agent_protocol::session::TextField::CommandOutput,
            delta: " tail".into(),
        },
    )
    .await;
    assert_eq!(
        snapshot(&service, &session).snapshot_sequence,
        before.snapshot_sequence,
        "live output does not write transcript revisions"
    );
    let connection = service.open_session();
    let mut reply = service
        .dispatch(
            connection.id(),
            &Call::SubscribeCommandOutput(records::SubscribeOutput {
                key: records::OutputKey {
                    session: session.clone(),
                    item: item.clone().into(),
                },
                subscription: Uuid::new_v4(),
            }),
        )
        .await
        .unwrap();
    let agent_protocol::protocol::Response::Success{result:opened}=agent_protocol::protocol::decode::<agent_protocol::protocol::Response<records::OpenedOutput>>(&reply.initial).unwrap() else{panic!("output subscription");};
    assert_eq!(opened.initial.text, "initial tail");
    if background {
        observe(
            &service,
            &native,
            execution,
            SessionChange::Background {
                tasks: vec![BackgroundTask {
                    id: "live-command".into(),
                    kind: BackgroundTaskKind::Command,
                    description: Some("printf output".into()),
                }],
            },
        )
        .await;
        observe(
            &service,
            &native,
            execution,
            SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "observed-turn".into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        )
        .await;
        settled(&service, &session, 1).await;
    }
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
    let stopped = snapshot(&service, &session);
    assert_eq!(stopped.projection.runs.len(), 1);
    assert_eq!(
        stopped.projection.runs[0].status,
        if background {
            model::RunStatus::Completed
        } else {
            model::RunStatus::Failed
        }
    );
    let saved = stopped
        .projection
        .turn_items
        .iter()
        .find(|saved| saved.id == item)
        .unwrap();
    // RunExecutionService.ts1733-1780 distinguishes a failed active run from
    // interrupted detached work; no native command exit code is fabricated.
    assert_eq!(
        saved.status,
        if background {
            model::TurnItemStatus::Interrupted
        } else {
            model::TurnItemStatus::Failed
        }
    );
    let updates = reply.updates.as_mut().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), updates.recv())
            .await
            .expect("stopped output closes its subscription")
            .is_none()
    );
    // Command bodies are intentionally omitted from T3 snapshots. Expand the
    // item through the actual public detail API, including after Host restart;
    // the fake native transcript has no tool body to recover on demand.
    let detail: records::Hydrated = call(
        &service,
        connection.id(),
        Call::HydrateConversation(records::Hydrate {
            session: session.clone(),
            item: item.clone().into(),
        }),
    )
    .await
    .unwrap();
    let detail = detail.item.unwrap();
    assert!(
        matches!(&detail.body,model::TurnItemBody::CommandExecution{output:Some(output),output_omitted:Some(false),exit_code:None,..} if output=="initial tail")
    );
    assert_eq!(
        snapshot(&service, &session).snapshot_sequence,
        stopped.snapshot_sequence,
        "reading detail never rewrites history"
    );
    assert!(
        agent.inputs.lock().unwrap().is_empty(),
        "process loss never resends accepted input"
    );
    drop(reply);
    drop(connection);
    let owner = Arc::downgrade(&service.inner);
    drop(service);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while owner.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let restarted = agent.restarted();
    let service = HostRpcService::new(
        [restarted.backend()],
        directory.path().join("projects.json"),
    );
    let connection = service.open_session();
    let reopened: records::Hydrated = call(
        &service,
        connection.id(),
        Call::HydrateConversation(records::Hydrate {
            session: session.clone(),
            item: item.into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(reopened.item.as_ref(), Some(&detail));
    assert!(restarted.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn update_restart_continues_the_same_conversation_once_without_repeating_user_input() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let projects = directory.path().join("projects.json");
    let service = HostRpcService::new([agent.backend()], projects.clone());
    let (session, native, _) = running(&service, &agent, directory.path().to_str().unwrap()).await;
    service.set_history_scope(directory.path().into(), true);
    service.prepare_update().await.unwrap();
    let owner = Arc::downgrade(&service.inner);
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
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &session);
            if current.projection.runs.len() == 2
                && current
                    .projection
                    .runs
                    .iter()
                    .any(|run| run.status == model::RunStatus::Completed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("update continuation completes");
    for _ in 0..3 {
        service.start().unwrap();
    }
    service.resume_after_update(directory.path()).await.unwrap();
    let report: agent_local_host::update::Recovery = serde_json::from_slice(
        &std::fs::read(
            directory
                .path()
                .join(agent_local_host::update::RECOVERY_FILE),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(report.restart_marker.is_none());
    assert!(matches!(
        &report.tasks[0].delivery,
        agent_local_host::update::ContinuationDelivery::Accepted(_)
    ));
    let inputs = restarted.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].thread_id, native);
    assert!(
        matches!(&inputs[0].input[..],[op::Input::Text {text}] if text==super::recovery::CONTINUE)
    );
}

#[tokio::test]
async fn a_native_child_has_canonical_subagent_lineage_and_an_execution_node() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    let child = SessionRef::new(ProviderKind::Codex, "native-child".into()).unwrap();
    Runtime::observe(
        &service,
        AgentChange::TaskParent {
            session: child.clone(),
            parent: native,
        },
        Some(execution),
    )
    .await
    .unwrap();
    let parent = snapshot(&service, &session);
    assert!(
        parent
            .projection
            .subagents
            .iter()
            .any(|subagent| subagent.child_thread_id.as_ref() == Some(&child.application_id()))
    );
    let child = snapshot(&service, &child);
    assert_eq!(
        child.projection.thread.lineage.parent_thread_id,
        Some(session.application_id())
    );
    assert_eq!(
        child
            .projection
            .thread
            .lineage
            .relationship_to_parent
            .as_deref(),
        Some("subagent")
    );
    assert!(
        parent
            .projection
            .nodes
            .iter()
            .any(|node| node.kind == model::NodeKind::Subagent)
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
}

// Codex's recorded spawn completes before either child does. Each real child
// turn owns its work; its transport may still carry the parent's execution token.
#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn native_children_with_the_parent_execution_token_keep_their_own_turns_and_results(
    #[case] parent_finishes_first: bool,
) {
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{Item, ItemBody},
        models::Turn,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    let children = [
        SessionRef::new(ProviderKind::Codex, "child-one".into()).unwrap(),
        SessionRef::new(ProviderKind::Codex, "child-two".into()).unwrap(),
    ];
    // An accepted turn keeps its model while a later default selection changes.
    service
        .inner
        .conversation
        .storage
        .observe_thread(&session.id, &[], &commands::at(), |state| {
            Ok(model::Decision {
                changes: vec![model::Change::Thread(model::Thread {
                    model_selection: model::ModelSelection {
                        model: "next-input-model".into(),
                        ..state.thread.model_selection.clone()
                    },
                    ..state.thread.clone()
                })],
                ..Default::default()
            })
        })
        .unwrap();
    for child in &children {
        Runtime::observe(
            &service,
            AgentChange::TaskParent {
                session: child.clone(),
                parent: native.clone(),
            },
            Some(execution),
        )
        .await
        .unwrap();
        assert_eq!(
            snapshot(&service, child)
                .projection
                .thread
                .model_selection
                .model,
            "fixture"
        );
    }
    let spawn=crate::providers::codex::native::parse_item(serde_json::json!({
        "type":"collabAgentToolCall","id":"spawn-real-children","tool":"spawnAgent","status":"completed",
        "prompt":"review the requested behavior","model":"child-model","receiverThreadIds":["child-one","child-two"],
        "agentsStates":{"child-one":{"status":"pendingInit","message":null},"child-two":{"status":"pendingInit","message":null}}
    })).unwrap();
    observe(
        &service,
        &native,
        execution,
        SessionChange::Item {
            turn_id: "observed-turn".into(),
            item: Arc::new(spawn),
        },
    )
    .await;
    let before = snapshot(&service, &session);
    assert_eq!(before.projection.subagents.len(), 2);
    for child in &children {
        assert_eq!(
            snapshot(&service, child)
                .projection
                .thread
                .model_selection
                .model,
            "child-model"
        );
        // Metadata-only parent repeats cannot replace the concrete spawn model.
        Runtime::observe(
            &service,
            AgentChange::TaskParent {
                session: child.clone(),
                parent: native.clone(),
            },
            Some(execution),
        )
        .await
        .unwrap();
        assert_eq!(
            snapshot(&service, child)
                .projection
                .thread
                .model_selection
                .model,
            "child-model"
        );
    }
    assert!(
        before
            .projection
            .subagents
            .iter()
            .all(|task| task.status == model::SubagentStatus::Pending)
    );
    let references = before
        .projection
        .subagents
        .iter()
        .map(|task| (task.id.clone(), task.native_task_ref.clone()))
        .collect::<HashMap<_, _>>();
    if parent_finishes_first {
        observe(
            &service,
            &native,
            execution,
            SessionChange::Turn {
                turn: Turn {
                    id: "observed-turn".into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        )
        .await;
    }
    for (index, child) in children.iter().enumerate() {
        let turn_id: agent_protocol::ids::TurnId = format!("actual-child-turn-{index}").into();
        observe(
            &service,
            child,
            execution,
            SessionChange::Turn {
                turn: Turn {
                    id: turn_id.clone(),
                    status: TurnStatus::Running,
                    items: Some(vec![Arc::new(Item::new(
                        format!("actual-child-input-{index}").into(),
                        ItemStatus::Completed,
                        ItemBody::UserMessage {
                            text: Some("actual SDK child input".into()),
                            content: vec![],
                        },
                    ))]),
                    ..Default::default()
                },
                completed: false,
            },
        )
        .await;
        let active = snapshot(&service, &session);
        assert_eq!(
            active.projection.provider_turns.len(),
            before.projection.provider_turns.len()
        );
        assert_eq!(active.projection.runs.len(), 1);
        let task = active
            .projection
            .subagents
            .iter()
            .find(|task| task.child_thread_id.as_deref() == Some(&child.application_id()))
            .unwrap();
        assert_eq!(task.status, model::SubagentStatus::Running);
        assert_eq!(task.native_task_ref, references[&task.id]);
        observe(
            &service,
            child,
            execution,
            SessionChange::Turn {
                turn: Turn {
                    id: turn_id,
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(Item::new(
                        format!("actual-child-answer-{index}").into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: format!("child result {index}"),
                            phase: Default::default(),
                        },
                    ))]),
                    ..Default::default()
                },
                completed: true,
            },
        )
        .await;
        let current = snapshot(&service, &session);
        let task = current
            .projection
            .subagents
            .iter()
            .find(|task| task.child_thread_id.as_deref() == Some(&child.application_id()))
            .unwrap();
        assert_eq!(task.status, model::SubagentStatus::Completed);
        let item = current
            .projection
            .turn_items
            .iter()
            .find(|item| item.node_id.as_ref() == Some(&task.id))
            .unwrap();
        assert_eq!(item.status, model::TurnItemStatus::Completed);
        assert!(
            matches!(&item.body,model::TurnItemBody::Subagent {result:Some(result),..} if result==&format!("child result {index}"))
        );
        assert!(
            current
                .projection
                .nodes
                .iter()
                .any(|node| node.id == task.id
                    && node.status == model::NodeStatus::Completed
                    && !node.counts_for_run)
        );
        let child = snapshot(&service, child);
        assert!(child.projection.runs.is_empty());
        assert!(child.projection.attempts.is_empty());
        assert_eq!(child.projection.provider_turns.len(), 1);
        assert_eq!(child.projection.thread.created_by, model::Actor::Agent);
        assert!(child.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::UserMessage {created_by:model::Actor::Agent,text,..} if text=="actual SDK child input")));
    }
    let stale=crate::providers::codex::native::parse_item(serde_json::json!({
        "type":"collabAgentToolCall","id":"late-wait-snapshot","tool":"wait","status":"completed",
        "receiverThreadIds":["child-one"],"agentsStates":{"child-one":{"status":"pendingInit","message":null}}
    })).unwrap();
    observe(
        &service,
        &native,
        execution,
        SessionChange::Item {
            turn_id: "observed-turn".into(),
            item: Arc::new(stale),
        },
    )
    .await;
    assert!(
        snapshot(&service, &session)
            .projection
            .subagents
            .iter()
            .all(|task| task.status == model::SubagentStatus::Completed)
    );
    // A genuine new child turn, unlike that snapshot, is evidence of new work.
    observe(
        &service,
        &children[0],
        execution,
        SessionChange::Turn {
            turn: Turn {
                id: "actual-child-next-turn".into(),
                status: TurnStatus::Running,
                ..Default::default()
            },
            completed: false,
        },
    )
    .await;
    let current = snapshot(&service, &session);
    let first = current
        .projection
        .subagents
        .iter()
        .find(|task| task.child_thread_id.as_deref() == Some(&children[0].application_id()))
        .unwrap();
    assert_eq!(first.status, model::SubagentStatus::Running);
    assert!(first.completed_at.is_none());
    assert_eq!(first.native_task_ref, references[&first.id]);
    // A terminal snapshot from the previous child turn cannot settle this one.
    observe(
        &service,
        &children[0],
        execution,
        SessionChange::Turn {
            turn: Turn {
                id: "actual-child-turn-0".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    )
    .await;
    let current = snapshot(&service, &session);
    let first = current
        .projection
        .subagents
        .iter()
        .find(|task| task.child_thread_id.as_deref() == Some(&children[0].application_id()))
        .unwrap();
    assert_eq!(first.status, model::SubagentStatus::Running);
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn generated_images_keep_t3_tool_results_and_markdown_embeds() {
    use agent_protocol::items::{Item, ItemBody};
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("generated image.png");
    std::fs::write(&image,base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a+X0AAAAASUVORK5CYII=").unwrap()).unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    observe(
        &service,
        &native,
        execution,
        SessionChange::Item {
            turn_id: "observed-turn".into(),
            item: Arc::new(Item::new(
                "native-image".into(),
                agent_protocol::execution::ItemStatus::Completed,
                ItemBody::ImageGeneration {
                    saved_path: Some(image.to_str().unwrap().into()),
                    data: None,
                    revised_prompt: Some("fixture image".into()),
                },
            )),
        },
    )
    .await;
    observe(
        &service,
        &native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status: agent_protocol::execution::TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    )
    .await;
    let current = settled(&service, &session, 1).await;
    // CodexAdapterV2.ts4143-4190 and6531-6564; generatedImageMarkdown.ts16.
    let tool = stored_image_tool(&service, &session, &current);
    let model::TurnItemBody::DynamicTool {
        output: Some(ref tool),
        ..
    } = tool.body
    else {
        panic!("stored image result")
    };
    assert_eq!(tool["savedPath"], image.to_str().unwrap());
    let embed = format!("![](<{}>)", image.to_str().unwrap());
    let (text, attachments) = current
        .projection
        .turn_items
        .iter()
        .find_map(|item| match &item.body {
            model::TurnItemBody::AssistantMessage {
                text, attachments, ..
            } => Some((text, attachments)),
            _ => None,
        })
        .expect("fallback final answer embeds the generated image");
    assert_eq!(text, &embed);
    assert!(attachments.is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

fn stored_image_tool(
    service: &HostRpcService,
    session: &SessionRef,
    current: &model::projection::ThreadSnapshot,
) -> model::TurnItem {
    let wire = current.projection.turn_items.iter().find(|item| matches!(&item.body, model::TurnItemBody::DynamicTool {tool_name: Some(name), ..} if name=="image_gen.imagegen")).expect("image generation metadata");
    assert!(
        matches!(
            &wire.body,
            model::TurnItemBody::DynamicTool {
                output: None,
                output_omitted: Some(_),
                ..
            }
        ),
        "snapshots keep tool outputs deferred"
    );
    service
        .inner
        .conversation
        .storage
        .thread_item(session, &wire.id)
        .unwrap()
        .unwrap()
}

async fn complete_image_turn(service: &HostRpcService, native: &SessionRef, execution: Uuid) {
    observe(
        service,
        native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status: agent_protocol::execution::TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    )
    .await;
}

fn generated_image(
    id: &str,
    status: agent_protocol::execution::ItemStatus,
    path: Option<String>,
    data: Option<String>,
) -> SessionChange {
    SessionChange::Item {
        turn_id: "observed-turn".into(),
        item: Arc::new(agent_protocol::items::Item::new(
            id.into(),
            status,
            agent_protocol::items::ItemBody::ImageGeneration {
                saved_path: path,
                data,
                revised_prompt: None,
            },
        )),
    }
}

#[rstest::rstest]
#[case::deduplicate(0)]
#[case::existing_markdown_and_references(1)]
#[case::links_code_and_escaped_embeds(2)]
#[case::unclosed_fence(3)]
#[case::unclosed_comment(4)]
#[case::unclosed_raw_html(5)]
#[case::windows_unc_and_reserved_characters(6)]
#[tokio::test]
async fn generated_image_answers_match_the_pinned_markdown_cases(#[case] case: usize) {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../scripts/t3-port-check/generated-image-fixtures.json"
    ))
    .unwrap();
    let fixture = &fixtures["cases"][case];
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    for (index, path) in fixture["saved"].as_array().unwrap().iter().enumerate() {
        observe(
            &service,
            &native,
            execution,
            generated_image(
                &format!("image-{index}"),
                agent_protocol::execution::ItemStatus::Completed,
                Some(path.as_str().unwrap().into()),
                None,
            ),
        )
        .await;
    }
    let text = fixture["text"].as_str().unwrap();
    if !text.is_empty() {
        observe(
            &service,
            &native,
            execution,
            SessionChange::Item {
                turn_id: "observed-turn".into(),
                item: Arc::new(agent_protocol::items::Item::new(
                    "final-answer".into(),
                    agent_protocol::execution::ItemStatus::Completed,
                    agent_protocol::items::ItemBody::AssistantText {
                        text: text.into(),
                        phase: agent_protocol::items::AssistantPhase::Final,
                    },
                )),
            },
        )
        .await;
    }
    complete_image_turn(&service, &native, execution).await;
    let current = settled(&service, &session, 1).await;
    let messages: Vec<_> = current
        .projection
        .turn_items
        .iter()
        .filter_map(|item| {
            if let model::TurnItemBody::AssistantMessage {
                text, attachments, ..
            } = &item.body
            {
                Some((text, attachments))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(messages.len(), 1, "{}", fixture["name"]);
    assert_eq!(
        messages[0].0,
        fixture["expected"].as_str().unwrap(),
        "{}",
        fixture["name"]
    );
    assert!(messages[0].1.is_empty());
    complete_image_turn(&service, &native, execution).await;
    assert_eq!(
        snapshot(&service, &session).projection.turn_items,
        current.projection.turn_items,
        "duplicate terminal events must not duplicate image messages"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn late_generated_images_finish_the_same_answer_after_foreground_completion(
    #[values(false, true)] existing_answer: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    observe(
        &service,
        &native,
        execution,
        generated_image(
            "late-image",
            agent_protocol::execution::ItemStatus::Running,
            None,
            None,
        ),
    )
    .await;
    if existing_answer {
        observe(
            &service,
            &native,
            execution,
            SessionChange::Item {
                turn_id: "observed-turn".into(),
                item: Arc::new(agent_protocol::items::Item::new(
                    "answer".into(),
                    agent_protocol::execution::ItemStatus::Completed,
                    agent_protocol::items::ItemBody::AssistantText {
                        text: "Created.".into(),
                        phase: agent_protocol::items::AssistantPhase::Final,
                    },
                )),
            },
        )
        .await;
    }
    complete_image_turn(&service, &native, execution).await;
    let current = settled(&service, &session, 1).await;
    let image=current.projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::DynamicTool {tool_name:Some(name),..} if name=="image_gen.imagegen")).unwrap();
    assert_eq!(image.status, model::TurnItemStatus::Running);
    observe(
        &service,
        &native,
        execution,
        generated_image(
            "late-image",
            agent_protocol::execution::ItemStatus::Completed,
            Some("/late.png".into()),
            None,
        ),
    )
    .await;
    let done = snapshot(&service, &session);
    let answers: Vec<_> = done
        .projection
        .turn_items
        .iter()
        .filter_map(|item| {
            if let model::TurnItemBody::AssistantMessage { text, .. } = &item.body {
                Some((item.id.clone(), text.clone()))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].1,
        if existing_answer {
            "Created.\n\n![](</late.png>)"
        } else {
            "![](</late.png>)"
        }
    );
    observe(
        &service,
        &native,
        execution,
        generated_image(
            "late-image",
            agent_protocol::execution::ItemStatus::Completed,
            Some("/late.png".into()),
            None,
        ),
    )
    .await;
    assert_eq!(
        snapshot(&service, &session).projection.turn_items,
        done.projection.turn_items
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn late_images_keep_their_original_turn_while_a_continued_turn_is_running() {
    use agent_protocol::execution::{ItemStatus, TurnStatus};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    observe(
        &service,
        &native,
        execution,
        generated_image("late-image", ItemStatus::Running, None, None),
    )
    .await;
    let before = snapshot(&service, &session);
    let original = before.projection.turn_items.iter().find(|item| matches!(&item.body,model::TurnItemBody::DynamicTool {tool_name:Some(name),..} if name=="image_gen.imagegen")).unwrap().provider_turn_id.clone();
    observe(
        &service,
        &native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "continued-turn".into(),
                status: TurnStatus::Running,
                ..Default::default()
            },
            completed: false,
        },
    )
    .await;
    complete_image_turn(&service, &native, execution).await;
    assert_eq!(
        snapshot(&service, &session).projection.runs[0].status,
        model::RunStatus::Running
    );
    observe(
        &service,
        &native,
        execution,
        generated_image(
            "late-image",
            ItemStatus::Completed,
            Some("/original.png".into()),
            None,
        ),
    )
    .await;
    let current = snapshot(&service, &session);
    assert_eq!(current.projection.runs[0].status, model::RunStatus::Running);
    let answer=current.projection.turn_items.iter().find(|item| matches!(&item.body,model::TurnItemBody::AssistantMessage {text,..} if text=="![](</original.png>)")).expect("original turn image answer");
    assert_eq!(answer.provider_turn_id, original);
    observe(
        &service,
        &native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "continued-turn".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    )
    .await;
    settled(&service, &session, 1).await;
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[rstest::rstest]
#[case::inline("inline", true)]
#[case::data_url("data_url", true)]
#[case::inclusive_limit("limit", true)]
#[case::oversize("oversize", false)]
#[case::invalid("invalid", false)]
#[tokio::test]
async fn generated_image_bytes_are_bounded_files_instead_of_database_payloads(
    #[case] kind: &str,
    #[case] success: bool,
) {
    use base64::Engine;
    let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a+X0AAAAASUVORK5CYII=").unwrap();
    let mut bytes = png;
    if kind == "limit" {
        bytes.resize(16 * 1024 * 1024, 0);
    }
    if kind == "oversize" {
        bytes.resize(16 * 1024 * 1024 + 1, 0);
    }
    let data = if kind == "invalid" {
        "invalid base64".into()
    } else {
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    };
    let data = if kind == "data_url" {
        format!("data:image/png;base64,{data}")
    } else {
        data
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    observe(
        &service,
        &native,
        execution,
        generated_image(
            "raw-image",
            agent_protocol::execution::ItemStatus::Completed,
            None,
            Some(data.clone()),
        ),
    )
    .await;
    complete_image_turn(&service, &native, execution).await;
    let current = settled(&service, &session, 1).await;
    let stored = stored_image_tool(&service, &session, &current);
    let status = stored.status;
    let model::TurnItemBody::DynamicTool {
        output: Some(ref output),
        ..
    } = stored.body
    else {
        panic!("stored image result")
    };
    if success {
        assert_eq!(status, model::TurnItemStatus::Completed);
        let path = output["savedPath"].as_str().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(current.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::AssistantMessage {text,..} if text.contains(path))));
    } else {
        assert_eq!(status, model::TurnItemStatus::Failed);
        assert!(output.get("savedPath").is_none());
        assert!(output.get("failure").is_some());
    }
    assert!(
        !serde_json::to_string(&current.projection)
            .unwrap()
            .contains(&data),
        "raw image data must not enter the event projection"
    );
    assert!(
        !serde_json::to_string(&stored).unwrap().contains(&data),
        "raw image data must not enter the stored item"
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_project_import_is_visible_as_a_canonical_thread_without_inventing_a_run() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let native = SessionRef::new(ProviderKind::Codex, "catalog-native".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(native.clone()),
                name: Some("Native catalog marker".into()),
                cwd: Some(directory.path().to_str().unwrap().into()),
                updated_at: Some(1.0),
                // T3 imports transcripts with a human message, not empty catalog headers.
                turns: Some(vec![Arc::new(agent_protocol::models::Turn {
                    id: "imported-turn".into(),
                    status: agent_protocol::models::TurnStatus::Completed,
                    items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                        "imported-user".into(),
                        agent_protocol::execution::ItemStatus::Completed,
                        agent_protocol::items::ItemBody::UserMessage {
                            text: Some("native input".into()),
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
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    service.start().unwrap();
    assert!(
        shell(&service).await["threads"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    service
        .inner
        .projects
        .register(directory.path())
        .await
        .unwrap();
    Runtime::import_project_threads(&service, &[directory.path().to_str().unwrap().into()])
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let listed = shell(&service).await;
            if let Some(thread) = listed["threads"]
                .as_array()
                .unwrap()
                .iter()
                .find(|thread| thread["id"] == native.application_id())
            {
                assert_eq!(thread["title"], "Native catalog marker");
                assert_eq!(thread["itemCount"], 1);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native catalog appears in shell stream");
    assert!(snapshot(&service, &native).projection.runs.is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn codex_rewind_continues_from_the_retained_head_in_the_same_native_thread() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("retained-marker"),
    )
    .await;
    settled(&service, &session, 1).await;
    let connection = service.open_session();
    for text in ["removed-marker", "new-suffix-marker"] {
        if text == "new-suffix-marker" {
            let current = snapshot(&service, &session);
            let run = current
                .projection
                .runs
                .iter()
                .max_by_key(|run| run.ordinal)
                .unwrap();
            let ack: records::Acknowledged = call(
                &service,
                connection.id(),
                Call::ChangeConversation(records::Change {
                    session: session.clone(),
                    operation: Uuid::new_v4(),
                    command: Command::Rewind(run.id.clone().into()),
                }),
            )
            .await
            .unwrap();
            assert!(ack.error.is_none());
        }
        let ack: records::Acknowledged = call(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: session.clone(),
                operation: Uuid::new_v4(),
                command: Command::Submit {
                    id: Uuid::new_v4().to_string().into(),
                    content: input(ProviderKind::Codex, text),
                },
            }),
        )
        .await
        .unwrap();
        assert!(ack.error.is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = snapshot(&service, &session);
                if current
                    .projection
                    .runs
                    .iter()
                    .max_by_key(|run| run.ordinal)
                    .unwrap()
                    .status
                    == model::RunStatus::Completed
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 3);
    // CodexAdapterV2.canRollbackThread and CheckpointRollbackService use
    // provider-thread.rollback on this same native resource.
    assert_eq!(inputs[1].thread_id, inputs[2].thread_id);
    let records = agent.records.lock().unwrap();
    let native = records.get(&inputs[2].thread_id.id).unwrap();
    assert_eq!(
        native.thread.turns.as_ref().unwrap().len(),
        2,
        "retained prefix plus new input"
    );
    assert_eq!(snapshot(&service,&session).projection.visible_turn_items.iter().filter(|item|matches!(&item.item.body,model::TurnItemBody::UserMessage {text,..} if text=="removed-marker")).count(),0);
}

#[tokio::test]
async fn an_empty_worktree_launch_prepares_a_checkout_without_creating_a_run() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(directory.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let _: String = call(
        &service,
        connection.id(),
        Call::AddProject(op::AddProject {
            cwd: directory.path().to_str().unwrap().into(),
        }),
    )
    .await
    .unwrap();
    let session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            session: session.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Worktree {
                base_ref: "main".into(),
                start_from_origin: false,
            }),
            selection: Default::default(),
            initial: None,
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &session);
            if let Some(path) = &current.projection.thread.worktree_path
                && std::path::Path::new(path).join(".git").is_file()
            {
                assert!(current.projection.runs.is_empty());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("empty launch prepares its worktree");
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn foreground_completion_preserves_a_background_command_and_accepts_its_late_result() {
    assert_background_late_result(false).await;
}

#[tokio::test]
async fn replacing_the_selected_native_source_keeps_background_results_owned_by_their_actual_attempt()
 {
    assert_background_late_result(true).await;
}

async fn assert_background_late_result(replace_source: bool) {
    use agent_protocol::{
        execution::{BackgroundTask, BackgroundTaskKind, ItemStatus, TurnStatus},
        items::{Item, ItemBody},
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (session, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    let command = |status, output: &str| SessionChange::Item {
        turn_id: "observed-turn".into(),
        item: Arc::new(Item::new(
            "server".into(),
            status,
            ItemBody::CommandExecution {
                output_omitted: false,
                command: "dev-server".into(),
                cwd: None,
                output: output.into(),
                exit_code: (status == ItemStatus::Completed).then_some(0),
            },
        )),
    };
    observe(
        &service,
        &native,
        execution,
        command(ItemStatus::Running, "listening"),
    )
    .await;
    observe(
        &service,
        &native,
        execution,
        SessionChange::Background {
            tasks: vec![BackgroundTask {
                id: "server".into(),
                kind: BackgroundTaskKind::Command,
                description: Some("dev-server".into()),
            }],
        },
    )
    .await;
    observe(
        &service,
        &native,
        execution,
        SessionChange::Turn {
            turn: agent_protocol::models::Turn {
                id: "observed-turn".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    )
    .await;
    let current = settled(&service, &session, 1).await;
    assert!(
        current
            .projection
            .provider_threads
            .iter()
            .any(|thread| !thread.pending_background_tasks.is_empty())
    );
    let item=current.projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::CommandExecution {input,..} if input=="dev-server")).unwrap();
    assert_eq!(item.status, model::TurnItemStatus::Running);
    let id = item.id.clone();
    observe(
        &service,
        &native,
        execution,
        SessionChange::Item {
            turn_id: "observed-turn".into(),
            item: Arc::new(Item::new(
                "unknown-late-command".into(),
                ItemStatus::Running,
                ItemBody::CommandExecution {
                    command: "unknown late command".into(),
                    cwd: None,
                    output: "ignored".into(),
                    exit_code: None,
                    output_omitted: false,
                },
            )),
        },
    )
    .await;
    let ignored = snapshot(&service, &session);
    assert_eq!(
        ignored.projection.nodes, current.projection.nodes,
        "rejected late items must not create orphan execution nodes"
    );
    assert_eq!(ignored.projection.turn_items, current.projection.turn_items);
    if replace_source {
        let selected = SessionRef::new(ProviderKind::Codex, "replacement-source".into()).unwrap();
        let now = commands::at();
        service
            .inner
            .conversation
            .storage
            .observe_thread(&session.id, &[], &now, |state| {
                let thread = state.selected_native_thread().unwrap();
                let mut reference = thread.native_thread_ref.clone().unwrap();
                reference.native_id = Some(selected.id.clone());
                Ok(model::Decision {
                    changes: vec![model::Change::ProviderThread(model::ProviderThread {
                        native_thread_ref: Some(reference),
                        updated_at: now.clone(),
                        ..thread.clone()
                    })],
                    ..Default::default()
                })
            })
            .unwrap();
        let before_foreign = snapshot(&service, &session);
        observe(
            &service,
            &selected,
            execution,
            command(ItemStatus::Completed, "foreign-source-output"),
        )
        .await;
        let after_foreign = snapshot(&service, &session);
        assert_eq!(
            after_foreign.projection.turn_items, before_foreign.projection.turn_items,
            "a different source cannot borrow an old attempt's pending item identity"
        );
        assert_eq!(
            after_foreign.projection.nodes,
            before_foreign.projection.nodes
        );
    }
    observe(
        &service,
        &native,
        execution,
        command(ItemStatus::Completed, "server-stopped-marker"),
    )
    .await;
    let final_item = service
        .inner
        .conversation
        .storage
        .thread_item(&session, &id)
        .unwrap()
        .unwrap();
    assert_eq!(final_item.status, model::TurnItemStatus::Completed);
    assert!(
        matches!(&final_item.body,model::TurnItemBody::CommandExecution {output:Some(output),..} if output=="server-stopped-marker")
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn importing_a_discovered_project_lists_its_existing_native_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let native = SessionRef::new(ProviderKind::Codex, "project-native".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(native.clone()),
                name: Some("project import marker".into()),
                cwd: Some(directory.path().to_str().unwrap().into()),
                updated_at: Some(1.0),
                turns: Some(vec![Arc::new(agent_protocol::models::Turn {
                    id: "imported-turn".into(),
                    status: agent_protocol::execution::TurnStatus::Completed,
                    items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                        "imported-question".into(),
                        agent_protocol::execution::ItemStatus::Completed,
                        agent_protocol::items::ItemBody::UserMessage {
                            text: Some("project import question".into()),
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
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let result: agent_protocol::project_import::ProjectImportResult = call(
        &service,
        connection.id(),
        Call::ImportProjects(agent_protocol::project_import::ImportProjects {
            paths: vec![directory.path().to_str().unwrap().into()],
        }),
    )
    .await
    .unwrap();
    assert!(result.errors.is_empty());
    assert_eq!(result.imported.len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if shell(&service).await["threads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|thread| thread["id"] == native.application_id())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("project scan publishes its native conversations");
    assert!(snapshot(&service, &native).projection.runs.is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fork_retries_its_unconsumed_source_after_a_failed_start_and_host_restart() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let projects = directory.path().join("projects.json");
    let service = HostRpcService::new([agent.backend()], projects.clone());
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("prefix-to-preserve"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source,
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    // All native creation is unavailable, including a portable-fork fallback.
    // A restarted adapter restores it without changing persisted native history.
    agent.create_gate.close();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Codex, "failed-before-write"),
            },
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if snapshot(&service, &target)
                .projection
                .runs
                .iter()
                .any(|run| run.status == model::RunStatus::Failed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native creation failure settles the first run");
    let failed = snapshot(&service, &target);
    assert!(
        failed.projection.context_handoffs.is_empty(),
        "failed fallback allocation cannot publish a context handoff"
    );
    assert!(
        failed
            .projection
            .context_transfers
            .iter()
            .all(|transfer| transfer.status == model::TransferStatus::Pending
                && transfer.resolution.is_none())
    );
    assert!(failed.projection.provider_threads[0].forked_from.is_some());
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
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
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Codex, "retry-with-context"),
            },
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    let admitted = snapshot(&service, &target);
    let next = admitted
        .projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .unwrap();
    assert!(
        admitted
            .projection
            .context_transfers
            .iter()
            .any(|transfer| transfer.target_run_id.as_ref() == Some(&next.id)),
        "the failed run cannot retain the unconsumed fork source"
    );
    let current = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &target);
            if current
                .projection
                .runs
                .iter()
                .any(|run| run.status == model::RunStatus::Completed)
            {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second input completes with the preserved fork prefix");
    let completed = current
        .projection
        .runs
        .iter()
        .find(|run| run.status == model::RunStatus::Completed)
        .unwrap();
    assert!(current.projection.context_transfers.iter().any(|transfer| {
        transfer.target_run_id.as_ref() == Some(&completed.id)
            && transfer.status == model::TransferStatus::Consumed
    }));
    let inputs = restarted.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    let records = restarted.records.lock().unwrap();
    let native = records.get(&inputs[1].thread_id.id).unwrap();
    assert_eq!(
        native.thread.turns.as_ref().unwrap().len(),
        2,
        "the retry retains the source turn before its new input"
    );
    assert_ne!(inputs[0].thread_id, inputs[1].thread_id);
}

#[rstest::rstest]
#[case(ProviderKind::Codex)]
#[case(ProviderKind::Claude)]
#[tokio::test]
async fn queued_fork_input_preserves_context_after_a_failed_start(#[case] provider: ProviderKind) {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let projects = directory.path().join("projects.json");
    let service = HostRpcService::new([codex.backend(), claude.backend()], projects.clone());
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("queued-prefix-marker"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source,
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let held = codex.create_gate.acquire().await.unwrap();
    for (kind, text) in [
        (ProviderKind::Codex, "failed-before-write"),
        (provider, "queued-with-context"),
    ] {
        let ack: records::Acknowledged = call(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation: Uuid::new_v4(),
                command: Command::Submit {
                    id: Uuid::new_v4().to_string().into(),
                    content: input(kind, text),
                },
            }),
        )
        .await
        .unwrap();
        assert!(ack.error.is_none());
    }
    let queued = snapshot(&service, &target)
        .projection
        .runs
        .iter()
        .find(|run| run.status == model::RunStatus::Queued)
        .unwrap()
        .id
        .clone();
    codex.create_gate.close();
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if snapshot(&service, &target)
                .projection
                .runs
                .iter()
                .any(|run| run.status == model::RunStatus::Failed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
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
    let codex = codex.restarted();
    let claude = claude.restarted();
    let service = HostRpcService::new([codex.backend(), claude.backend()], projects);
    let _connection = service.open_session();
    let ack = Runtime::change(
        &service,
        &records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::ResumeQueue,
        },
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    assert!(
        snapshot(&service, &target)
            .projection
            .context_transfers
            .iter()
            .any(|transfer| transfer.target_run_id.as_ref() == Some(&queued)),
        "queued activation claims the undelivered fork source"
    );
    if provider == ProviderKind::Claude {
        let current = snapshot(&service, &target);
        let transfer = current
            .projection
            .context_transfers
            .iter()
            .find(|transfer| transfer.target_run_id.as_ref() == Some(&queued))
            .unwrap();
        assert_eq!(
            transfer.status,
            model::TransferStatus::ResolvedPortable,
            "T3 prepares queued portable context before acknowledging resume"
        );
        assert!(
            current
                .projection
                .runs
                .iter()
                .find(|run| run.id == queued)
                .unwrap()
                .context_handoff_id
                .is_some()
        );
    }
    let current = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &target);
            if current
                .projection
                .runs
                .iter()
                .any(|run| run.id == queued && run.status == model::RunStatus::Completed)
            {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued input completes with its fork history");
    let transfer = current
        .projection
        .context_transfers
        .iter()
        .find(|transfer| transfer.target_run_id.as_ref() == Some(&queued))
        .unwrap();
    if provider == ProviderKind::Codex {
        assert_eq!(transfer.status, model::TransferStatus::Consumed);
        let inputs = codex.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2);
        let records = codex.records.lock().unwrap();
        assert_eq!(
            records[&inputs[1].thread_id.id]
                .thread
                .turns
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        assert!(claude.inputs.lock().unwrap().is_empty());
    } else {
        assert_eq!(transfer.status, model::TransferStatus::ResolvedPortable);
        assert!(!current.projection.context_handoffs.is_empty());
        assert_eq!(codex.inputs.lock().unwrap().len(), 1);
        assert_eq!(claude.inputs.lock().unwrap().len(), 1);
        let delivered = serde_json::to_string(&(
            &*claude.injected.lock().unwrap(),
            &*claude.inputs.lock().unwrap(),
        ))
        .unwrap();
        assert!(delivered.contains("queued-prefix-marker"));
    }
}

async fn send(service: &HostRpcService, session: &SessionRef, provider: ProviderKind, text: &str) {
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(provider, text),
            },
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none(), "{:?}", ack.error);
}

#[rstest::rstest]
#[case("")]
#[case("injection_unknown")]
#[case("not_sent")]
#[case("import_incomplete")]
#[case("state_error")]
#[case("metadata_only")]
#[tokio::test]
async fn switching_back_reuses_native_history_or_recovers_uncertain_injection(#[case] fault: &str) {
    let directory = tempfile::tempdir().unwrap();
    let mut codex = NativeAgent::new(ProviderKind::Codex);
    Arc::get_mut(&mut codex).unwrap().metadata_only_state = fault == "metadata_only";
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("already-native-marker"),
    )
    .await;
    settled(&service, &session, 1).await;
    let original = codex.inputs.lock().unwrap()[0].thread_id.id.clone();
    send(
        &service,
        &session,
        ProviderKind::Claude,
        "foreign-history-marker",
    )
    .await;
    settled(&service, &session, 2).await;
    if fault == "injection_unknown" {
        codex
            .fail_inject
            .store(1, std::sync::atomic::Ordering::SeqCst);
    }
    if fault == "not_sent" {
        codex
            .fail_submit
            .store(1, std::sync::atomic::Ordering::SeqCst);
    }
    if fault == "import_incomplete" {
        codex
            .records
            .lock()
            .unwrap()
            .get_mut(&original)
            .unwrap()
            .thread
            .history_read_state = Some(agent_protocol::session::HistoryReadState::new(
            agent_protocol::session::HistoryReadKind::Incomplete,
            vec!["native history was compacted".into()],
        ));
    }
    if fault == "state_error" {
        *codex.state_error.lock().unwrap() = Some(Failure::unknown(
            "resume_unavailable",
            "native source is unavailable",
        ));
    }
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "current-user-marker",
    )
    .await;
    if fault == "injection_unknown" {
        assert_eq!(codex.inputs.lock().unwrap().len(), 1);
        retry_failed_start(&service, &session, 3).await;
    }
    let current = settled(&service, &session, 3).await;
    let inputs = codex.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2, "one native prompt per accepted input");
    assert_eq!(claude.inputs.lock().unwrap().len(), 1);
    let injected = codex.injected.lock().unwrap();
    let recovering = matches!(
        fault,
        "injection_unknown" | "import_incomplete" | "state_error"
    );
    assert_eq!(
        injected.len(),
        if fault == "injection_unknown" { 2 } else { 1 }
    );
    assert_eq!(inputs[1].thread_id.id == original, !recovering);
    assert_eq!(injected.last().unwrap().0, inputs[1].thread_id.id);
    let context = serde_json::to_string(&injected.last().unwrap().1).unwrap();
    assert!(context.contains("foreign-history-marker"));
    assert_eq!(context.contains("already-native-marker"), recovering);
    assert!(!context.contains("current-user-marker"));
    assert_eq!(
        current
            .projection
            .context_transfers
            .iter()
            .filter(
                |transfer| transfer.kind == model::TransferType::ProviderHandoff
                    && transfer.status == model::TransferStatus::Consumed
            )
            .count(),
        2,
        "recovery must preserve both provider-switch records"
    );
    assert_eq!(
        current
            .projection
            .context_transfers
            .iter()
            .filter(
                |transfer| transfer.kind == model::TransferType::ProviderHandoff
                    && transfer.status == model::TransferStatus::ResolvedPortable
                    && transfer.created_by == model::Actor::System
            )
            .count(),
        usize::from(recovering)
    );
    let latest_run = current
        .projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .unwrap();
    let target_handoffs: Vec<_> = current
        .projection
        .context_handoffs
        .iter()
        .filter(|handoff| handoff.target_run_id == latest_run.id)
        .collect();
    assert_eq!(target_handoffs.len(), if recovering { 2 } else { 1 });
    let delta = current
        .projection
        .context_transfers
        .iter()
        .find(|transfer| {
            transfer.target_run_id.as_ref() == Some(&latest_run.id)
                && transfer.status == model::TransferStatus::Consumed
        })
        .unwrap();
    let base = delta
        .base_point
        .as_ref()
        .expect("T3 provider-switch delta records the prior target boundary");
    assert_eq!(
        base.run_id.as_deref(),
        Some(
            current
                .projection
                .runs
                .iter()
                .min_by_key(|run| run.ordinal)
                .unwrap()
                .id
                .as_str()
        )
    );
    assert!(base.provider_thread_ref.is_some() && base.provider_turn_ref.is_some());
    let model::ContextTransferResolution::DeltaContext { context_handoff_id } =
        delta.resolution.as_ref().unwrap()
    else {
        panic!("switch delta resolution");
    };
    assert_eq!(
        latest_run.context_handoff_id.as_ref(),
        Some(context_handoff_id)
    );
    for handoff in &target_handoffs {
        let owner = current
            .projection
            .provider_threads
            .iter()
            .find(|thread| thread.id == handoff.to_provider_thread_id)
            .unwrap();
        assert!(
            owner.handoff_ids.contains(&handoff.id),
            "the provider thread retains each prepared handoff link"
        );
        if handoff.strategy == model::HandoffStrategy::FullThreadSummary {
            assert_eq!(
                handoff.from_provider_thread_ids,
                std::slice::from_ref(&owner.id)
            );
        }
    }

    assert!(
        target_handoffs
            .iter()
            .any(|handoff| handoff.strategy == model::HandoffStrategy::DeltaSinceTargetLastSeen),
        "the original switch delta remains in the audit records"
    );
    assert_eq!(
        target_handoffs
            .iter()
            .any(|handoff| handoff.strategy == model::HandoffStrategy::FullThreadSummary),
        recovering,
        "recovery adds a full-context record alongside the original delta"
    );
}

#[tokio::test]
async fn stopping_during_provider_release_preserves_the_actual_unload_result_and_other_conversations()
 {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let root = tempfile::tempdir().unwrap();
        let codex = NativeAgent::new(ProviderKind::Codex);
        let claude = NativeAgent::new(ProviderKind::Claude);
        let service = HostRpcService::new(
            [codex.backend(), claude.backend()],
            root.path().join("projects.json"),
        );
        let source = create(
            &service,
            root.path().to_str().unwrap(),
            Some("source input"),
        )
        .await;
        let before = settled(&service, &source, 1).await;
        let old_thread = before.projection.provider_threads[0].id.clone();
        let peer = create(&service, root.path().to_str().unwrap(), Some("peer input")).await;
        let peer_before = settled(&service, &peer, 1).await;
        let held = codex.unload_gate.acquire().await.unwrap();
        send(
            &service,
            &source,
            ProviderKind::Claude,
            "unsent after switch",
        )
        .await;
        loop {
            if !codex.released.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let connection = service.open_session();
        let current = snapshot(&service, &source);
        let run = current
            .projection
            .runs
            .iter()
            .max_by_key(|run| run.ordinal)
            .unwrap();
        let ack: records::Acknowledged = call(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: source.clone(),
                operation: Uuid::new_v4(),
                command: Command::Interrupt {
                    turn: Some(run.id.clone().into()),
                },
            }),
        )
        .await
        .unwrap();
        assert_eq!(ack.error, None);
        let stopped = snapshot(&service, &source);
        assert_eq!(
            stopped
                .projection
                .runs
                .iter()
                .find(|saved| saved.id == run.id)
                .unwrap()
                .status,
            model::RunStatus::Cancelled
        );
        drop(held);
        let mut changed = service.inner.conversation.storage.changes();
        loop {
            let state = service
                .inner
                .conversation
                .storage
                .thread_state(&source.id, &[])
                .unwrap();
            if state.provider_threads[&old_thread].status == model::ProviderThreadStatus::NotLoaded
            {
                assert!(state.provider_session(&old_thread).is_none());
                assert_eq!(state.runs[&run.id].status, model::RunStatus::Cancelled);
                break;
            }
            changed.changed().await.unwrap();
        }
        assert!(
            claude.inputs.lock().unwrap().is_empty(),
            "cancelling startup never sends the waiting input"
        );
        let peer_after = snapshot(&service, &peer);
        assert_eq!(peer_after.projection.thread, peer_before.projection.thread);
        assert_eq!(peer_after.projection.runs, peer_before.projection.runs);
        assert_eq!(
            peer_after.projection.provider_threads,
            peer_before.projection.provider_threads
        );
        assert!(
            peer_after
                .projection
                .provider_sessions
                .iter()
                .any(|session| session.status == model::ProviderSessionStatus::Ready)
        );
        assert_eq!(codex.released.lock().unwrap().len(), 1);
    })
    .await
    .expect("an in-flight unload commits its resource result after cancellation");
}

#[rstest::rstest]
#[case("peer")]
#[case("native_roster")]
#[case("child")]
#[case("unreadable")]
#[tokio::test]
async fn provider_release_checks_real_background_work_and_preserves_other_conversations(
    #[case] condition: &str,
) {
    use agent_protocol::{execution, models};
    let root = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        root.path().join("projects.json"),
    );
    let source = create(
        &service,
        root.path().to_str().unwrap(),
        Some("source-input"),
    )
    .await;
    let before = settled(&service, &source, 1).await;
    let (native, execution) = super::native_fixture::bound_execution(&service, &source);
    let peer = create(&service, root.path().to_str().unwrap(), Some("peer-input")).await;
    let peer_before = settled(&service, &peer, 1).await;
    let peer_native = codex.inputs.lock().unwrap()[1].thread_id.clone();
    codex
        .records
        .lock()
        .unwrap()
        .get_mut(&peer_native.id)
        .unwrap()
        .thread
        .status = execution::SessionStatus::Running;
    match condition {
        "native_roster" => {
            codex
                .records
                .lock()
                .unwrap()
                .get_mut(&native.id)
                .unwrap()
                .thread
                .background = vec![execution::BackgroundTask {
                id: "native-only-child".into(),
                kind: execution::BackgroundTaskKind::Subagent,
                description: Some("actual SDK work absent from the projection".into()),
            }]
        }
        "unreadable" => {
            codex.records.lock().unwrap().remove(&native.id);
        }
        "child" => {
            let child =
                SessionRef::new(ProviderKind::Codex, "actual-pending-child".into()).unwrap();
            Runtime::observe(
                &service,
                AgentChange::TaskParent {
                    session: child.clone(),
                    parent: native.clone(),
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
                        turn: models::Turn {
                            id: "actual-child-turn".into(),
                            status: execution::TurnStatus::Running,
                            ..Default::default()
                        },
                        completed: false,
                    },
                },
                Some(execution),
            )
            .await
            .unwrap();
            let first = snapshot(&service, &source);
            assert_eq!(
                first.projection.subagents[0].status,
                model::SubagentStatus::Running
            );
            assert!(
                !first
                    .projection
                    .visible_turn_items
                    .iter()
                    .any(|row| matches!(row.item.body, model::TurnItemBody::Subagent { .. }))
            );
            Runtime::observe(
                &service,
                AgentChange::Session {
                    session: child,
                    change: SessionChange::Turn {
                        turn: models::Turn {
                            id: "actual-child-turn".into(),
                            status: execution::TurnStatus::Running,
                            ..Default::default()
                        },
                        completed: false,
                    },
                },
                Some(execution),
            )
            .await
            .unwrap();
            assert_eq!(
                snapshot(&service, &source),
                first,
                "repeated child state does not publish another parent event"
            );
        }
        _ => {}
    }
    send(&service, &source, ProviderKind::Claude, "switch-input").await;
    if condition == "peer" {
        let after = settled(&service, &source, 2).await;
        assert!(
            !after
                .projection
                .provider_sessions
                .iter()
                .any(|session| session.provider_instance_id == "codex")
        );
        assert_eq!(codex.released.lock().unwrap().as_slice(), [native.id]);
        assert_eq!(claude.inputs.lock().unwrap().len(), 1);
    } else {
        let after = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = snapshot(&service, &source);
                if current
                    .projection
                    .runs
                    .iter()
                    .any(|run| run.ordinal == 2 && run.status == model::RunStatus::Failed)
                {
                    break current;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            let current=snapshot(&service,&source);
            panic!("unsafe release fails only the new input: {error}; runs={:?}; tasks={:?}; resources={:?}",
                current.projection.runs.iter().map(|run|(&run.id,run.ordinal,run.status)).collect::<Vec<_>>(),
                current.projection.subagents.iter().map(|task|(&task.id,task.status,&task.parent_node_id)).collect::<Vec<_>>(),
                current.projection.provider_threads.iter().map(|thread|(&thread.id,thread.status,&thread.provider_session_id)).collect::<Vec<_>>()
            );
        });
        assert!(codex.released.lock().unwrap().is_empty());
        assert!(claude.inputs.lock().unwrap().is_empty());
        assert_eq!(
            after.projection.runs.iter().find(|run| run.ordinal == 1),
            before.projection.runs.first()
        );
        assert!(
            after
                .projection
                .provider_sessions
                .iter()
                .any(|session| session.provider_instance_id == "codex")
        );
        if condition == "child" {
            assert_eq!(
                after.projection.subagents[0].status,
                model::SubagentStatus::Running
            );
            Runtime::observe(
                &service,
                AgentChange::Session {
                    session: SessionRef::new(ProviderKind::Codex, "actual-pending-child".into())
                        .unwrap(),
                    change: SessionChange::Turn {
                        turn: models::Turn {
                            id: "actual-child-turn".into(),
                            status: execution::TurnStatus::Completed,
                            ..Default::default()
                        },
                        completed: true,
                    },
                },
                Some(execution),
            )
            .await
            .unwrap();
            let finished = snapshot(&service, &source);
            assert_eq!(
                finished.projection.subagents[0].status,
                model::SubagentStatus::Completed
            );
            assert!(
                !finished
                    .projection
                    .visible_turn_items
                    .iter()
                    .any(|row| matches!(row.item.body, model::TurnItemBody::Subagent { .. }))
            );
        }
    }
    assert!(!codex.released.lock().unwrap().contains(&peer_native.id));
    assert_eq!(snapshot(&service, &peer), peer_before);
    assert_eq!(
        codex.records.lock().unwrap()[&peer_native.id].thread.status,
        execution::SessionStatus::Running
    );
}

#[tokio::test]
async fn portable_forks_include_only_the_selected_source_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("selected-source-marker"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    send(
        &service,
        &source,
        ProviderKind::Codex,
        "later-source-marker",
    )
    .await;
    settled(&service, &source, 2).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: agent_protocol::models::Empty = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    send(
        &service,
        &target,
        ProviderKind::Claude,
        "fork-current-marker",
    )
    .await;
    let current = settled(&service, &target, 1).await;
    let inputs = claude.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1);
    let delivered = serde_json::to_string(&inputs[0].input).unwrap();
    assert!(delivered.contains("selected-source-marker"));
    assert!(!delivered.contains("later-source-marker"));
    assert_eq!(delivered.matches("fork-current-marker").count(), 1);
    assert!(
        current
            .projection
            .context_transfers
            .iter()
            .any(|transfer| transfer.kind == model::TransferType::Fork
                && transfer.status == model::TransferStatus::ResolvedPortable)
    );
    assert!(
        current.projection.runs[0]
            .context_handoff_id
            .as_ref()
            .is_some_and(|id| current
                .projection
                .context_handoffs
                .iter()
                .any(|handoff| &handoff.id == id)),
        "portable fork records its planned handoff on the run"
    );
}

#[tokio::test]
async fn accepted_attempts_record_the_native_identity_used_for_context_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("native-identity"),
    )
    .await;
    let current = settled(&service, &session, 1).await;
    let attempt = &current.projection.attempts[0];
    let provider_thread = current
        .projection
        .provider_threads
        .iter()
        .find(|thread| thread.id == attempt.provider_thread_id)
        .unwrap();
    assert_eq!(attempt.native_thread_ref, provider_thread.native_thread_ref);
}

#[tokio::test]
async fn attached_provider_sessions_publish_the_native_model_switch_capability() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("capability fixture"),
    )
    .await;
    let current = settled(&service, &session, 1).await;
    assert!(current.projection.provider_sessions.iter().any(|session| {
        session
            .capabilities
            .sessions
            .supports_model_switch_in_session
    }));
}

#[tokio::test]
async fn switching_back_replaces_a_full_native_context_before_appending_history() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some(&"a".repeat(400_000)),
    )
    .await;
    settled(&service, &session, 1).await;
    let original = codex.inputs.lock().unwrap()[0].thread_id.id.clone();
    send(
        &service,
        &session,
        ProviderKind::Claude,
        "foreign-history-marker",
    )
    .await;
    settled(&service, &session, 2).await;
    send(&service, &session, ProviderKind::Codex, "near-full-current").await;
    let current = settled(&service, &session, 3).await;
    let inputs = codex.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_ne!(
        inputs[1].thread_id.id, original,
        "T3 replaces a native context when the delta metadata cannot fit beside accepted history"
    );
    let injected = codex.injected.lock().unwrap();
    let delivered = serde_json::to_string(&injected.last().unwrap().1).unwrap();
    assert!(delivered.contains("foreign-history-marker"));
    assert!(!delivered.contains("near-full-current"));
    assert!(
        current
            .projection
            .context_transfers
            .iter()
            .any(
                |transfer| transfer.kind == model::TransferType::ProviderHandoff
                    && transfer.status == model::TransferStatus::ResolvedPortable
                    && transfer.created_by == model::Actor::System
            )
    );
    for handoff in &current.projection.context_handoffs {
        let owner = current
            .projection
            .provider_threads
            .iter()
            .find(|thread| thread.id == handoff.to_provider_thread_id)
            .unwrap();
        assert!(
            owner.handoff_ids.contains(&handoff.id),
            "budget recovery preserves the immediately preceding switch handoff link"
        );
    }
}

#[tokio::test]
async fn provider_handoff_is_committed_before_the_dispatch_acknowledgement() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("source-history"),
    )
    .await;
    settled(&service, &session, 1).await;
    // On this current-thread runtime the owner returns the acknowledgement
    // without yielding after commit. The native worker has not started it.
    let ack = Runtime::change(
        &service,
        &records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: input(ProviderKind::Claude, "next-input"),
            },
        },
    )
    .await
    .unwrap();
    assert!(ack.error.is_none());
    let current = snapshot(&service, &session);
    let run = current
        .projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .unwrap();
    let id = run
        .context_handoff_id
        .as_ref()
        .expect("T3 commits the context plan with its acknowledgement");
    assert!(
        current
            .projection
            .context_handoffs
            .iter()
            .any(|handoff| &handoff.id == id)
    );
    assert!(claude.inputs.lock().unwrap().is_empty());
    settled(&service, &session, 2).await;
}

#[tokio::test]
async fn provider_handoff_uses_the_message_actor_for_agent_created_input() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("source-history"),
    )
    .await;
    settled(&service, &session, 1).await;
    let operation = Uuid::new_v4().to_string();
    let at = commands::at();
    let ack = service
        .inner
        .conversation
        .storage
        .thread_command(
            super::super::storage::commands::Command {
                id: &operation,
                thread_id: &session.id,
                kind: "message.dispatch",
                now: &at,
                item_ids: &[],
            },
            None,
            |state| {
                model::admit(
                    state,
                    model::Input {
                        fresh_context: false,
                        scheduled_task_id: None,
                        message_id: operation.clone(),
                        created_by: model::Actor::Agent,
                        creation_source: model::CreationSource::Server,
                        text: "agent continuation".into(),
                        attachments: vec![],
                        context: None,
                        model_selection: model::ModelSelection {
                            instance_id: "claude".into(),
                            model: "fixture".into(),
                            options: vec![],
                        },
                        driver: ProviderKind::Claude.driver().into(),
                        workspace_preparation: None,
                        fork_origin: None,
                    },
                    model::Admission::Queue,
                    super::executor::context(state, &operation, &at),
                )
            },
        )
        .unwrap();
    assert!(ack.receipt.error.is_none());
    let current = settled(&service, &session, 2).await;
    assert!(
        current
            .projection
            .context_transfers
            .iter()
            .any(
                |transfer| transfer.kind == model::TransferType::ProviderHandoff
                    && transfer.status == model::TransferStatus::Consumed
                    && transfer.created_by == model::Actor::Agent
            )
    );
}

#[tokio::test]
async fn a_provider_thread_without_a_native_reference_receives_full_history() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("old-native-history-marker"),
    )
    .await;
    let first = settled(&service, &session, 1).await;
    let old = first.projection.thread.active_provider_thread_id.unwrap();
    send(
        &service,
        &session,
        ProviderKind::Claude,
        "foreign-history-marker",
    )
    .await;
    settled(&service, &session, 2).await;
    // A failed replacement may leave this canonical row without a native ID.
    // Prior accepted runs remain historical facts, not occupancy of a new ID.
    service
        .inner
        .conversation
        .storage
        .observe_thread(&session.id, &[], &commands::at(), |state| {
            Ok(model::Decision {
                changes: vec![model::Change::ProviderThread(model::ProviderThread {
                    native_thread_ref: None,
                    status: model::ProviderThreadStatus::NotLoaded,
                    ..state.provider_threads[&old].clone()
                })],
                effects: vec![],
            })
        })
        .unwrap();
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "fresh-current-marker",
    )
    .await;
    settled(&service, &session, 3).await;
    let inputs = codex.inputs.lock().unwrap();
    assert_ne!(inputs[0].thread_id, inputs[1].thread_id);
    let context = serde_json::to_string(&*codex.injected.lock().unwrap()).unwrap();
    assert!(context.contains("old-native-history-marker"));
    assert!(context.contains("foreign-history-marker"));
    assert!(!context.contains("fresh-current-marker"));
}

#[tokio::test]
async fn native_fork_provenance_survives_provider_switches() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("fork-source"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: agent_protocol::models::Empty = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source,
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    send(&service, &target, ProviderKind::Codex, "fork-first").await;
    let first = settled(&service, &target, 1).await;
    let owner = first.projection.thread.active_provider_thread_id.unwrap();
    let provenance = first
        .projection
        .provider_threads
        .iter()
        .find(|thread| thread.id == owner)
        .unwrap()
        .forked_from
        .clone();
    assert!(provenance.is_some());
    send(
        &service,
        &target,
        ProviderKind::Claude,
        "fork-other-provider",
    )
    .await;
    settled(&service, &target, 2).await;
    codex
        .fail_inject
        .store(1, std::sync::atomic::Ordering::SeqCst);
    send(&service, &target, ProviderKind::Codex, "fork-return").await;
    assert_eq!(codex.inputs.lock().unwrap().len(), 2);
    retry_failed_start(&service, &target, 3).await;
    let current = settled(&service, &target, 3).await;
    assert_eq!(
        current
            .projection
            .provider_threads
            .iter()
            .find(|thread| thread.id == owner)
            .unwrap()
            .forked_from,
        provenance
    );
    let inputs = codex.inputs.lock().unwrap();
    let fresh = &inputs.last().unwrap().thread_id.id;
    let native = codex.records.lock().unwrap();
    assert_eq!(
        native[fresh].thread.turns.as_ref().unwrap().len(),
        1,
        "full portable recovery creates a fresh native context rather than copying the parent prefix again"
    );
    let context = serde_json::to_string(&*codex.injected.lock().unwrap()).unwrap();
    assert!(
        context.contains("fork-source")
            && context.contains("fork-first")
            && context.contains("fork-other-provider")
    );
}

#[rstest::rstest]
#[case::native(false, ProviderKind::Codex)]
#[case::other_provider(false, ProviderKind::Claude)]
#[case::imported(true, ProviderKind::Codex)]
#[tokio::test]
async fn runless_forks_continue_only_the_selected_native_or_imported_prefix(
    #[case] imported: bool,
    #[case] destination: ProviderKind,
) {
    use agent_protocol::{execution, items, models};
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let source = SessionRef::new(ProviderKind::Codex, "real-external-source".into()).unwrap();
    let turns: Vec<_> = ["retained-native-marker", "excluded-native-marker"]
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            Arc::new(models::Turn {
                id: format!("real-native-turn-{index}").into(),
                status: execution::TurnStatus::Completed,
                items: Some(vec![Arc::new(items::Item::new(
                    format!("real-native-answer-{index}").into(),
                    execution::ItemStatus::Completed,
                    items::ItemBody::AssistantText {
                        text: text.into(),
                        phase: Default::default(),
                    },
                ))]),
                ..Default::default()
            })
        })
        .collect();
    let mut response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(source.clone()),
            status: execution::SessionStatus::Idle,
            cwd: Some(directory.path().to_str().unwrap().into()),
            turns: Some(if imported { turns.clone() } else { vec![] }),
            ..Default::default()
        },
        ..Default::default()
    };
    codex
        .records
        .lock()
        .unwrap()
        .insert(source.id.clone(), response.clone());
    Runtime::ensure_imported(&service, &source).await.unwrap();
    if !imported {
        for turn in &turns {
            for completed in [false, true] {
                Runtime::observe(
                    &service,
                    AgentChange::Session {
                        session: source.clone(),
                        change: SessionChange::Turn {
                            turn: models::Turn {
                                status: if completed {
                                    execution::TurnStatus::Completed
                                } else {
                                    execution::TurnStatus::Running
                                },
                                ..turn.as_ref().clone()
                            },
                            completed,
                        },
                    },
                    None,
                )
                .await
                .unwrap();
            }
        }
    }
    response.thread.turns = Some(turns);
    codex
        .records
        .lock()
        .unwrap()
        .insert(source.id.clone(), response.clone());
    let before = snapshot(&service, &source);
    assert!(before.projection.runs.is_empty());
    assert!(before.projection.attempts.is_empty());
    let through = if imported {
        records::ForkPoint::TurnItem {
            turn_item_id: before.projection.visible_turn_items[0]
                .source_item_id
                .clone(),
        }
    } else {
        records::ForkPoint::ProviderTurn {
            provider_turn_id: before
                .projection
                .provider_turns
                .iter()
                .min_by_key(|turn| turn.ordinal)
                .unwrap()
                .id
                .clone(),
        }
    };
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    for _ in 0..2 {
        let returned: SessionRef = call(
            &service,
            connection.id(),
            Call::ForkConversation(records::Fork {
                source: source.clone(),
                target: target.clone(),
                through: through.clone(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(returned, target);
    }
    let fork = snapshot(&service, &target);
    assert!(fork.projection.runs.is_empty());
    assert!(fork.projection.attempts.is_empty());
    assert_eq!(
        fork.projection
            .visible_turn_items
            .iter()
            .filter(|row| row.visibility == model::projection::Visibility::Inherited)
            .count(),
        1
    );
    send(&service, &target, destination, "new-fork-input").await;
    let continued = settled(&service, &target, 1).await;
    assert_eq!(continued.projection.attempts.len(), 1);
    let backend = if destination == ProviderKind::Codex {
        &codex
    } else {
        &claude
    };
    let inputs = backend.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 1, "the new input is sent exactly once");
    let native_target = &inputs[0].thread_id;
    assert_ne!(native_target, &source);
    let records = backend.records.lock().unwrap();
    let native_history = records[&native_target.id].thread.turns.as_ref().unwrap();
    let native_copy = !imported && destination == ProviderKind::Codex;
    assert_eq!(native_history.len(), if native_copy { 2 } else { 1 });
    let context = serde_json::to_string(&*backend.injected.lock().unwrap()).unwrap();
    if native_copy {
        assert_eq!(
            native_history[0],
            response.thread.turns.as_ref().unwrap()[0]
        );
        assert_eq!(context, "[]");
    } else {
        let delivered = if destination == ProviderKind::Claude {
            assert_eq!(context, "[]", "Claude receives portable history inline");
            let op::Input::Text { text } = &inputs[0].input[0] else {
                panic!("text input")
            };
            assert_eq!(
                inputs[0].input.len(),
                2,
                "history and the new input remain separate SDK parts"
            );
            assert_eq!(
                inputs[0].input[1],
                op::Input::Text {
                    text: "new-fork-input".into()
                }
            );
            text.strip_suffix("\n\nUser message:\n").unwrap()
        } else {
            context.as_str()
        };
        assert!(delivered.contains("retained-native-marker"));
        assert!(!delivered.contains("excluded-native-marker"));
        assert!(!delivered.contains("new-fork-input"));
        let handoff = &continued.projection.context_handoffs[0];
        assert_eq!(
            handoff.from_provider_thread_ids,
            [before.projection.provider_threads[0].id.clone()]
        );
        assert_eq!(
            continued.projection.context_transfers[0].source_point,
            fork.projection.context_transfers[0].source_point
        );
    }
    drop(records);
    drop(inputs);
    assert_eq!(
        snapshot(&service, &source).projection.visible_turn_items,
        before.projection.visible_turn_items
    );
    assert_eq!(codex.records.lock().unwrap()[&source.id], response);
}

#[tokio::test]
async fn first_injection_recovery_keeps_its_provider_without_inventing_a_source_model() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let connection = service.open_session();
    let session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            session: session.clone(),
            provider: ProviderKind::Claude,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((
                Uuid::new_v4().to_string().into(),
                input(ProviderKind::Claude, "source-history"),
            )),
        }),
    )
    .await
    .unwrap();
    settled(&service, &session, 1).await;
    codex
        .fail_inject
        .store(1, std::sync::atomic::Ordering::SeqCst);
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "new-provider-current",
    )
    .await;
    assert!(codex.inputs.lock().unwrap().is_empty());
    retry_failed_start(&service, &session, 2).await;
    let current = settled(&service, &session, 2).await;
    let item = current
        .projection
        .visible_turn_items
        .iter()
        .find(|entry| entry.item.title.as_deref() == Some("Recovered context"))
        .expect("recovery item");
    let model::TurnItemBody::Handoff {
        from_provider_instance_ids,
        from_model_selections,
        to_model,
        ..
    } = &item.item.body
    else {
        panic!("handoff");
    };
    assert_eq!(from_provider_instance_ids, &["codex".to_string()]);
    assert!(from_model_selections.as_ref().unwrap().is_empty());
    assert_eq!(to_model.as_deref(), Some("fixture"));
}

#[tokio::test]
async fn recovery_context_precedes_the_run_input_and_uses_a_stable_run_identity() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("original-history"),
    )
    .await;
    settled(&service, &session, 1).await;
    send(&service, &session, ProviderKind::Claude, "foreign-history").await;
    settled(&service, &session, 2).await;
    codex
        .fail_inject
        .store(1, std::sync::atomic::Ordering::SeqCst);
    send(&service, &session, ProviderKind::Codex, "current-input").await;
    assert_eq!(codex.inputs.lock().unwrap().len(), 1);
    retry_failed_start(&service, &session, 3).await;
    let current = settled(&service, &session, 3).await;
    let run = current
        .projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .unwrap();
    let recovery = current
        .projection
        .turn_items
        .iter()
        .find(|item| {
            item.run_id.as_ref() == Some(&run.id)
                && item.title.as_deref() == Some("Recovered context")
        })
        .expect("a recovered native context has a visible handoff");
    assert_eq!(
        recovery.id,
        model::derived_id(
            "turn-item",
            &["run", &run.id, "signal", "resume-fallback-handoff"]
        )
    );
    assert_eq!(
        recovery.ordinal,
        run.ordinal * 1_000_000,
        "T3 reserves the first run position for recovery"
    );
    assert!(
        current
            .projection
            .turn_items
            .iter()
            .filter(|item| { item.run_id.as_ref() == Some(&run.id) && item.id != recovery.id })
            .all(|item| item.ordinal > recovery.ordinal),
        "recovery precedes the provider-switch handoff and current input"
    );
}

#[tokio::test]
async fn context_history_reads_canonical_full_bodies_with_exact_unicode_continuations() {
    use agent_protocol::handoff;
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let text = "日".repeat(4205);
    let session = create(&service, directory.path().to_str().unwrap(), Some(&text)).await;
    let saved = settled(&service, &session, 1).await;
    let assistant = saved
        .projection
        .visible_turn_items
        .iter()
        .find(|row| matches!(row.item.body, model::TurnItemBody::AssistantMessage { .. }))
        .unwrap();
    let storage = &service.inner.conversation.storage;
    let mut full = storage
        .thread_item(&session, &assistant.source_item_id)
        .unwrap()
        .unwrap();
    let model::TurnItemBody::AssistantMessage { text: body, .. } = &mut full.body else {
        panic!("assistant")
    };
    *body = "🦀".repeat(10001);
    storage
        .observe_thread(&session.application_id(), &[], &commands::at(), |_| {
            Ok(model::Decision {
                changes: vec![model::Change::TurnItem(full)],
                effects: vec![],
            })
        })
        .unwrap();
    let connection = service.open_session();
    let read = |offset, text_offset| {
        Call::ReadHandoffHistory(handoff::ReadHistory {
            item_id: None,
            session: session.clone(),
            offset,
            limit: 1,
            text_offset,
        })
    };
    let first: handoff::Page = call(&service, connection.id(), read(0, 0)).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].message.role, handoff::Role::User);
    assert_eq!(first.items[0].message.text, "日".repeat(4000));
    assert_eq!(first.items[0].next_text_offset, Some(4000));
    assert_eq!(first.next_offset, Some(1));
    assert!(
        first.items[0]
            .message
            .source
            .contains(&session.application_id())
    );
    let tail: handoff::Page = call(&service, connection.id(), read(0, 4000))
        .await
        .unwrap();
    assert_eq!(tail.items[0].message.text, "日".repeat(205));
    assert_eq!(tail.items[0].next_text_offset, None);
    let answer: handoff::Page = call(&service, connection.id(), read(1, 8000))
        .await
        .unwrap();
    assert_eq!(answer.items[0].position, 1);
    assert_eq!(answer.items[0].message.text, "🦀".repeat(2001));
    assert_eq!(answer.items[0].next_text_offset, None);
    assert_eq!(answer.next_offset, None);
    assert!(
        answer.items[0]
            .message
            .source
            .contains(&assistant.source_item_id)
    );
    let end: handoff::Page = call(&service, connection.id(), read(usize::MAX, 0))
        .await
        .unwrap();
    assert!(end.items.is_empty() && end.next_offset.is_none());
    assert_eq!(
        agent.inputs.lock().unwrap().len(),
        1,
        "history never submits native input"
    );
}

#[tokio::test]
async fn context_history_inherits_only_the_selected_fork_prefix_with_source_identity() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("selected-context"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    send(&service, &source, ProviderKind::Codex, "later-context").await;
    settled(&service, &source, 2).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: agent_protocol::models::Empty = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let history: agent_protocol::handoff::Page = call(
        &service,
        connection.id(),
        Call::ReadHandoffHistory(agent_protocol::handoff::ReadHistory {
            item_id: None,
            session: target.clone(),
            offset: 0,
            limit: 20,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(history.items.len(), 2);
    assert_eq!(history.items[0].message.text, "selected-context");
    assert!(
        history
            .items
            .iter()
            .all(|item| item.message.source.contains(&source.application_id()))
    );
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("later-context")
    );
    assert!(snapshot(&service, &target).projection.runs.is_empty());
    assert_eq!(agent.inputs.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn context_history_finds_an_omitted_item_by_its_canonical_id() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("request"),
    )
    .await;
    let saved = settled(&service, &session, 1).await;
    let assistant = saved
        .projection
        .visible_turn_items
        .iter()
        .find(|row| matches!(row.item.body, model::TurnItemBody::AssistantMessage { .. }))
        .unwrap();
    let connection = service.open_session();
    let params = serde_json::from_value(serde_json::json!({"session":session,"offset":0,"limit":20,"text_offset":0,"item_id":assistant.source_item_id})).unwrap();
    let page: agent_protocol::handoff::Page =
        call(&service, connection.id(), Call::ReadHandoffHistory(params))
            .await
            .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "an exact item read does not include other activity"
    );
    assert_eq!(page.items[0].position, 1);
    assert_eq!(
        page.items[0].message.role,
        agent_protocol::handoff::Role::Assistant
    );
    assert!(
        page.items[0]
            .message
            .source
            .contains(&assistant.source_item_id)
    );
    assert_eq!(page.next_offset, None);
    let other = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("private-other"),
    )
    .await;
    let private = settled(&service, &other, 1).await;
    let foreign_id = &private.projection.visible_turn_items[0].source_item_id;
    let params = serde_json::from_value(serde_json::json!({"session":session,"offset":0,"limit":20,"text_offset":0,"item_id":foreign_id})).unwrap();
    let hidden: agent_protocol::handoff::Page =
        call(&service, connection.id(), Call::ReadHandoffHistory(params))
            .await
            .unwrap();
    assert!(
        hidden.items.is_empty() && hidden.next_offset.is_none(),
        "an item outside this visible timeline cannot be read"
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 2);
}

async fn finished_run(
    service: &HostRpcService,
    session: &SessionRef,
    ordinal: u64,
) -> model::projection::ThreadSnapshot {
    let mut changed = service.inner.conversation.storage.changes();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let saved = snapshot(service, session);
            if saved.projection.runs.iter().any(|run| {
                run.ordinal == ordinal
                    && matches!(
                        run.status,
                        model::RunStatus::Completed
                            | model::RunStatus::Failed
                            | model::RunStatus::Interrupted
                            | model::RunStatus::Cancelled
                    )
            }) {
                return saved;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("execution reaches its terminal state")
}

// Pinned ProviderSwitch.integration.test.ts's injection case first fails the
// run, then explicitly retries in a fresh resource. Bex retains RetryInput for
// that user action; the failed prompt must not be sent by an outbox retry.
async fn retry_failed_start(service: &HostRpcService, session: &SessionRef, ordinal: u64) {
    let failed = finished_run(service, session, ordinal).await;
    let run = failed
        .projection
        .runs
        .iter()
        .find(|run| run.ordinal == ordinal)
        .unwrap();
    assert_eq!(run.status, model::RunStatus::Failed);
    assert!(
        !failed
            .projection
            .provider_turns
            .iter()
            .any(|turn| turn.run_attempt_id.as_ref() == run.active_attempt_id.as_ref())
    );
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command: Command::RetryInput {
                id: run.user_message_id.clone().into(),
                work_locally: false,
            },
        }),
    )
    .await
    .unwrap();
    ack.accepted().unwrap();
}

#[tokio::test]
async fn later_input_carries_unsent_failed_requests_once_without_resending_their_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let projects = directory.path().join("projects.json");
    let agent = NativeAgent::new(ProviderKind::Codex);
    agent.create_gate.close();
    let service = HostRpcService::new([agent.backend()], projects.clone());
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("first-unsent-request"),
    )
    .await;
    let first = finished_run(&service, &session, 1).await;
    assert_eq!(first.projection.runs[0].status, model::RunStatus::Failed);
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "second-unsent-request",
    )
    .await;
    let second = finished_run(&service, &session, 2).await;
    assert!(
        second
            .projection
            .runs
            .iter()
            .all(|run| run.status == model::RunStatus::Failed)
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
    drop(service);
    let resumed = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([resumed.backend()], projects);
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "new-current-request",
    )
    .await;
    let current = finished_run(&service, &session, 3).await;
    let run = current
        .projection
        .runs
        .iter()
        .find(|run| run.ordinal == 3)
        .unwrap();
    assert_eq!(run.status, model::RunStatus::Completed);
    assert_eq!(run.context_handoff_id, None);
    let handoff = current
        .projection
        .context_handoffs
        .iter()
        .find(|handoff| handoff.target_run_id == run.id)
        .expect("T3 prepares missed native requests as retry history");
    assert_eq!(handoff.transfer_id, None);
    assert_eq!(
        handoff.strategy,
        model::HandoffStrategy::DeltaSinceTargetLastSeen
    );
    assert_eq!(
        (
            handoff.covered_run_ordinals.from,
            handoff.covered_run_ordinals.to
        ),
        (1, 2)
    );
    assert!(current.projection.context_transfers.is_empty());
    assert!(
        !current
            .projection
            .turn_items
            .iter()
            .any(|item| matches!(item.body, model::TurnItemBody::Handoff { .. }))
    );
    let history = serde_json::to_string(&*resumed.injected.lock().unwrap()).unwrap();
    assert_eq!(history.matches("first-unsent-request").count(), 1);
    assert_eq!(history.matches("second-unsent-request").count(), 1);
    assert!(!history.contains("new-current-request"));
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "next-current-request",
    )
    .await;
    let next = finished_run(&service, &session, 4).await;
    assert_eq!(
        next.projection
            .runs
            .iter()
            .find(|run| run.ordinal == 4)
            .unwrap()
            .status,
        model::RunStatus::Completed
    );
    assert_eq!(
        resumed.injected.lock().unwrap().len(),
        1,
        "settled retry history must not be injected again"
    );
    let inputs = resumed.inputs.lock().unwrap();
    assert_eq!(
        inputs.len(),
        2,
        "only the two new inputs are submitted natively"
    );
    assert!(matches!(&inputs[0].input[0], op::Input::Text {text} if text == "new-current-request"));
    assert!(
        matches!(&inputs[1].input[0], op::Input::Text {text} if text == "next-current-request")
    );
}

#[tokio::test]
async fn a_spent_native_fork_uses_fresh_portable_context_without_an_extra_handoff_card() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("retained-native-prefix"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    send(
        &service,
        &source,
        ProviderKind::Codex,
        "outside-native-prefix",
    )
    .await;
    settled(&service, &source, 2).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let original = agent.inputs.lock().unwrap()[0].thread_id.id.clone();
    agent.records.lock().unwrap().remove(&original);
    send(&service, &target, ProviderKind::Codex, "fresh-fork-input").await;
    let completed = finished_run(&service, &target, 1).await;
    assert_eq!(
        completed.projection.runs[0].status,
        model::RunStatus::Completed
    );
    assert_eq!(
        completed.projection.runs[0].context_handoff_id, None,
        "runtime fallback is not a dispatch-planned handoff"
    );
    assert_eq!(completed.projection.context_handoffs.len(), 1);
    assert!(
        completed
            .projection
            .context_transfers
            .iter()
            .any(|t| t.status == model::TransferStatus::ResolvedPortable)
    );
    assert!(
        !completed
            .projection
            .turn_items
            .iter()
            .any(|item| matches!(item.body, model::TurnItemBody::Handoff { .. })),
        "T3 runtime fork fallback does not emit a handoff turn item"
    );
    assert!(completed.projection.turn_items.iter().any(|item|item.title.as_deref()==Some("Native fork failed") && matches!(&item.body,model::TurnItemBody::SystemNotice {message} if message.contains("Started a fresh session with its history."))));
    let injected = serde_json::to_string(&*agent.injected.lock().unwrap()).unwrap();
    assert!(injected.contains("retained-native-prefix"));
    assert!(!injected.contains("outside-native-prefix"));
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 3);
    let records = agent.records.lock().unwrap();
    assert_eq!(
        records[&inputs[2].thread_id.id]
            .thread
            .turns
            .as_ref()
            .unwrap()
            .len(),
        1
    );
}

fn admit_before_worker(
    storage: &super::super::storage::Storage,
    session: &SessionRef,
    text: &str,
    fork_origin: Option<model::ProviderThreadFork>,
) {
    let operation = Uuid::new_v4().to_string();
    let now = commands::at();
    let ack = storage
        .thread_command(
            super::super::storage::commands::Command {
                id: &operation,
                thread_id: &session.application_id(),
                kind: "message.dispatch",
                now: &now,
                item_ids: &[],
            },
            None,
            |state| {
                model::admit(
                    state,
                    model::Input {
                        fresh_context: false,
                        scheduled_task_id: None,
                        message_id: operation.clone(),
                        created_by: model::Actor::User,
                        creation_source: model::CreationSource::Mobile,
                        text: text.into(),
                        attachments: vec![],
                        context: None,
                        model_selection: state.thread.model_selection.clone(),
                        driver: "codex".into(),
                        workspace_preparation: None,
                        fork_origin,
                    },
                    model::Admission::Queue,
                    super::executor::context(state, &operation, &now),
                )
            },
        )
        .unwrap();
    assert!(ack.receipt.error.is_none());
}

#[tokio::test]
async fn a_source_busy_after_admission_keeps_its_native_plan_until_fresh_allocation_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let source = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("busy-source-prefix"),
    )
    .await;
    let first = settled(&service, &source, 1).await;
    agent.create_started.notified().await; // Consume the source's creation notification.
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let connection = service.open_session();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let workspace = service.inner.worktree_access.write().await;
    let creation = agent.create_gate.acquire().await.unwrap();
    let storage = &service.inner.conversation.storage;
    let origin = storage
        .fork_origin(&target)
        .unwrap()
        .expect("eligible source at admission");
    admit_before_worker(storage, &target, "busy-fork-input", Some(origin));
    agent
        .fail_submit
        .store(2, std::sync::atomic::Ordering::SeqCst);
    admit_before_worker(storage, &source, "later-busy-source", None);
    drop(workspace);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.create_started.notified(),
    )
    .await
    .expect("portable fallback waits for fresh allocation");
    let allocating = snapshot(&service, &target);
    assert!(
        allocating.projection.context_handoffs.is_empty(),
        "fallback metadata is published only after successful native allocation"
    );
    assert_eq!(
        allocating.projection.context_transfers[0].status,
        model::TransferStatus::Pending
    );
    assert!(
        allocating.projection.provider_threads[0]
            .forked_from
            .is_some()
    );
    // The one-shot native failure belongs to the parent. Observe it before
    // releasing the child's allocation, so the child cannot consume it.
    let mut changed = storage.changes();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if snapshot(&service, &source).projection.turn_items.iter().any(|item| matches!(&item.body, model::TurnItemBody::Error {failure,..} if failure.class == "delivery_uncertain")) {
                break;
            }
            changed.changed().await.unwrap();
        }
    }).await.expect("parent owns the uncertain native write");
    drop(creation);
    let completed = finished_run(&service, &target, 1).await;
    assert_eq!(
        completed.projection.runs[0].status,
        model::RunStatus::Completed
    );
    assert_eq!(completed.projection.runs[0].context_handoff_id, None);
    assert!(
        completed
            .projection
            .context_transfers
            .iter()
            .any(|t| t.status == model::TransferStatus::ResolvedPortable)
    );
    assert!(
        !completed.projection.turn_items.iter().any(|item| matches!(
            item.body,
            model::TurnItemBody::Handoff { .. }
        ) || item.title.as_deref()
            == Some("Native fork failed")),
        "a busy source is not a failed fork"
    );
    let injected = serde_json::to_string(&*agent.injected.lock().unwrap()).unwrap();
    assert!(injected.contains("busy-source-prefix"));
    assert!(!injected.contains("later-busy-source"));
}

#[tokio::test]
async fn failed_recovery_allocation_keeps_the_existing_native_reference_and_uncommitted_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex);
    let claude = NativeAgent::new(ProviderKind::Claude);
    let service = HostRpcService::new(
        [codex.backend(), claude.backend()],
        directory.path().join("projects.json"),
    );
    let session = create(
        &service,
        directory.path().to_str().unwrap(),
        Some("native-before-recovery"),
    )
    .await;
    settled(&service, &session, 1).await;
    let original = codex.inputs.lock().unwrap()[0].thread_id.id.clone();
    send(&service, &session, ProviderKind::Claude, "foreign-context").await;
    settled(&service, &session, 2).await;
    codex.create_gate.close();
    codex
        .fail_inject
        .store(1, std::sync::atomic::Ordering::SeqCst);
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "unsent-recovery-input",
    )
    .await;
    retry_failed_start(&service, &session, 3).await;
    let failed = finished_run(&service, &session, 3).await;
    let run = failed
        .projection
        .runs
        .iter()
        .max_by_key(|r| r.ordinal)
        .unwrap();
    assert_eq!(run.status, model::RunStatus::Failed);
    let provider = failed
        .projection
        .provider_threads
        .iter()
        .find(|thread| Some(&thread.id) == run.provider_thread_id.as_ref())
        .unwrap();
    assert_eq!(
        provider
            .native_thread_ref
            .as_ref()
            .and_then(|r| r.native_id.as_deref()),
        Some(original.as_str()),
        "failed fresh allocation cannot erase the previous native identity"
    );
    assert_eq!(
        failed
            .projection
            .context_handoffs
            .iter()
            .filter(|h| h.target_run_id == run.id)
            .count(),
        1,
        "only the dispatch-planned switch exists before a fresh recovery is allocated"
    );
    assert_eq!(
        codex.inputs.lock().unwrap().len(),
        1,
        "the failed input never reached a native thread"
    );
}

#[cfg(unix)]
mod codex_rollback {
    include!("codex_rollback_tests.rs");
}

#[tokio::test]
async fn a_native_rollback_refusal_is_public_and_does_not_cut_visible_or_native_history() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(&service, directory.path().to_str().unwrap(), Some("kept")).await;
    settled(&service, &session, 1).await;
    send(&service, &session, ProviderKind::Codex, "still visible").await;
    let before = settled(&service, &session, 2).await;
    let run = before
        .projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .unwrap();
    *agent.rollback_error.lock().unwrap() = Some("native rollback was refused".into());
    let request = Uuid::new_v4();
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: request,
            command: Command::Rewind(run.id.clone().into()),
        }),
    )
    .await
    .unwrap();
    assert!(
        ack.error.is_none(),
        "the command is accepted before native completion"
    );
    let failed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = snapshot(&service, &session);
            let thread = serde_json::to_value(&current.projection.thread).unwrap();
            if thread["rollbackFailure"]["requestId"] == request.to_string() {
                break (current, thread);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the final native error must reach the canonical client projection");
    assert_eq!(
        failed.1["rollbackFailure"]["message"],
        "native rollback was refused"
    );
    assert_eq!(failed.0.projection.runs, before.projection.runs);
    assert_eq!(
        failed.0.projection.visible_turn_items,
        before.projection.visible_turn_items
    );
    assert_eq!(
        agent
            .rollback_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        5,
        "T3 bounds native retries to five attempts"
    );
    assert_eq!(agent.inputs.lock().unwrap().len(), 2);
    let source = service
        .inner
        .conversation
        .storage
        .active_source(&session)
        .unwrap()
        .unwrap();
    assert_eq!(
        agent.records.lock().unwrap()[&source.id]
            .thread
            .turns
            .as_ref()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn claude_native_cursor_updates_keep_controls_callback_ownership_and_one_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let mut agent = NativeAgent::new(ProviderKind::Claude);
    Arc::get_mut(&mut agent).unwrap().submit_gate = Some(tokio::sync::Semaphore::new(0));
    let _release = scopeguard::guard(agent.clone(), |agent| {
        agent.submit_gate.as_ref().unwrap().add_permits(1)
    });
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            session: public.clone(),
            provider: ProviderKind::Claude,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((
                Uuid::new_v4().to_string().into(),
                input(ProviderKind::Claude, "one input"),
            )),
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
    let storage = &service.inner.conversation.storage;
    let state = storage.thread_state(&public.id, &[]).unwrap();
    let attempt = state
        .active_run()
        .unwrap()
        .active_attempt_id
        .clone()
        .unwrap();
    let execution = Uuid::parse_str(&attempt).unwrap();
    // The private provider's wire control handle is independent of the actual
    // assistant UUID and arrives before the held submission acknowledgement.
    let control: agent_protocol::ids::TurnId = format!("turn-{execution}").into();
    let native = storage.active_source(&public).unwrap().unwrap();
    let cursor = Uuid::new_v4().to_string();
    Runtime::observe(
        &service,
        AgentChange::Head {
            session: native.clone(),
            turn: control.clone(),
            head: cursor.clone(),
        },
        Some(execution),
    )
    .await
    .unwrap();
    let state = storage.thread_state(&public.id, &[]).unwrap();
    assert_eq!(
        state.provider_turns.len(),
        1,
        "actual native output proves the same accepted turn"
    );
    assert_eq!(
        state
            .provider_turns
            .values()
            .next()
            .unwrap()
            .native_turn_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_deref(),
        Some(cursor.as_str())
    );
    for (source, token) in [
        (native.clone(), Uuid::new_v4()),
        (
            SessionRef::new(ProviderKind::Claude, "foreign-physical-source".into()).unwrap(),
            execution,
        ),
    ] {
        Runtime::observe(
            &service,
            AgentChange::Head {
                session: source,
                turn: control.clone(),
                head: "foreign-cursor".into(),
            },
            Some(token),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        storage
            .thread_state(&public.id, &[])
            .unwrap()
            .provider_turns,
        state.provider_turns,
        "a foreign attempt or physical source cannot replace the cursor"
    );
    let receipt: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: public.clone(),
            operation: Uuid::new_v4(),
            command: Command::Interrupt {
                turn: Some(state.active_run().unwrap().id.clone().into()),
            },
        }),
    )
    .await
    .unwrap();
    assert!(receipt.error.is_none());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while agent.interrupts.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        agent.interrupts.lock().unwrap().as_slice(),
        &[(native.id.clone(), control.clone())],
        "SDK interrupt uses the echoed control handle, never the assistant resume cursor"
    );
    agent.submit_gate.as_ref().unwrap().add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = storage.thread_state(&public.id, &[]).unwrap();
            if state.active_run().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let completed = storage.thread_state(&public.id, &[]).unwrap();
    assert_eq!(
        completed.provider_turns.len(),
        1,
        "later SDK callbacks still address this turn after its history cursor changed"
    );
    assert_eq!(
        agent.inputs.lock().unwrap().len(),
        1,
        "native evidence cannot resend the admitted input"
    );
    Runtime::observe(
        &service,
        AgentChange::Head {
            session: native,
            turn: control,
            head: "late-old-query-cursor".into(),
        },
        Some(execution),
    )
    .await
    .unwrap();
    assert_eq!(
        storage
            .thread_state(&public.id, &[])
            .unwrap()
            .provider_turns,
        completed.provider_turns,
        "terminal callbacks cannot revive a closed query"
    );
}

#[tokio::test]
async fn saved_short_long_and_forked_conversations_open_through_rpc_and_core_without_native_history()
 {
    use super::native_fixture::Transcript;
    use agent_protocol::models::ThreadResponse;
    use agent_protocol::protocol::Response;
    use std::time::Instant;

    async fn open(
        service: &HostRpcService,
        connection: crate::host_rpc::SessionId,
        session: &SessionRef,
    ) -> (Transcript, usize, u128) {
        let started = Instant::now();
        let reply = service
            .dispatch(
                connection,
                &Call::SubscribeConversation(records::Subscribe {
                    session: session.clone(),
                    subscription: Uuid::new_v4(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
        let bytes = reply.initial.len();
        let Response::Success { result: opened } =
            agent_protocol::protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
        else {
            panic!("saved conversation must open")
        };
        let transcript = Transcript::opened(opened);
        transcript.project().unwrap(); // include the actual client projection in the timing
        (transcript, bytes, started.elapsed().as_micros())
    }

    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    for (name, count) in [("short", 2usize), ("long", 1000)] {
        let turns:Vec<_>=(0..count).map(|i|serde_json::json!({
            "id":format!("native-turn-{i}"),"status":"completed","items":[
                {"id":format!("input-{i}"),"status":"completed","body":{"inline":{"body":{"userMessage":{"content":[{"text":{"text":format!("input-{i}")}}]}}}}},
                {"id":format!("answer-{i}"),"status":"completed","body":{"inline":{"body":{"assistantText":{"text":format!("answer-{i} {}","x".repeat(512)),"phase":"final"}}}}}
            ]
        })).collect();
        let response: ThreadResponse = serde_json::from_value(serde_json::json!({"thread":{
            "id":{"provider":"codex","id":name},"cwd":directory.path(),"status":"idle","turns":turns
        }}))
        .unwrap();
        agent.records.lock().unwrap().insert(name.into(), response);
    }
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let short = SessionRef::new(ProviderKind::Codex, "short".into()).unwrap();
    let long = SessionRef::new(ProviderKind::Codex, "long".into()).unwrap();
    for session in [&short, &long] {
        let (_, bytes, micros) = open(&service, connection.id(), session).await;
        eprintln!(
            "normal_open_sample {}",
            serde_json::json!({"case":session.id,"phase":"first_import","bytes":bytes,"micros":micros})
        );
    }
    // Remove the actual discovery source after its one-time paginated import.
    // A normal open that accidentally reloads it must fail, not get a cheap stub.
    let imported_reads = agent
        .discovery_reads
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        imported_reads > 0,
        "the recorder must observe the actual paginated import"
    );
    agent.records.lock().unwrap().clear();
    let long_snapshot = snapshot(&service, &long);
    // The pinned importer stores run-less history without native executions.
    // Choose the actual displayed imported reply, as its UI fork action does.
    let boundary = long_snapshot.projection.turn_items.iter().find(|item| {
        matches!(&item.body, model::TurnItemBody::AssistantMessage {text,..} if text.starts_with("answer-994 "))
    }).unwrap();
    let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: long.clone(),
            target: fork.clone(),
            through: records::ForkPoint::TurnItem {
                turn_item_id: boundary.id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    for (name, session, total, last) in [
        ("short", &short, 2, 1),
        ("long", &long, 1000, 999),
        ("fork", &fork, 995, 994),
    ] {
        for _ in 0..3 {
            let (opened, bytes, micros) = open(&service, connection.id(), session).await;
            assert_eq!(
                agent
                    .discovery_reads
                    .load(std::sync::atomic::Ordering::SeqCst),
                imported_reads,
                "saved opens must not read provider discovery/history, even on a caught failure"
            );
            let view = opened.project().unwrap();
            let turns = view.thread.turns.as_ref().unwrap();
            // ThreadReader sends local records plus the rendered inherited
            // rows. Count their canonical identities across both wire fields.
            let projection = &opened.snapshot.projection;
            let bodies: std::collections::BTreeSet<_> = projection
                .turn_items
                .iter()
                .map(AsRef::as_ref)
                .chain(
                    projection
                        .visible_turn_items
                        .iter()
                        .map(|row| row.item.as_ref()),
                )
                .filter(|item| {
                    matches!(
                        item.body,
                        model::TurnItemBody::UserMessage { .. }
                            | model::TurnItemBody::AssistantMessage { .. }
                    )
                })
                .map(|item| (&item.thread_id, &item.id))
                .collect();
            assert_eq!(
                bodies.len(),
                2 * 10.min(total),
                "the actual RPC must not transfer old message bodies"
            );
            let texts: Vec<_> = turns
                .iter()
                .flat_map(|turn| turn.items.iter().flatten())
                .filter_map(|item| match item.body() {
                    agent_protocol::items::ItemBody::UserMessage { text, content } => {
                        Some(text.clone().unwrap_or_else(|| {
                            content
                                .iter()
                                .filter_map(|part| match part {
                                    agent_protocol::items::MessagePart::Text { text, .. } => {
                                        Some(text.as_str())
                                    }
                                    _ => None,
                                })
                                .collect::<String>()
                        }))
                    }
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = ((last + 1 - 10.min(total))..=last)
                .map(|i| format!("input-{i}"))
                .collect();
            assert_eq!(
                texts, expected,
                "RPC and core must select the provider's retained boundary"
            );
            // Report bytes/time as measurements, without inventing a machine-
            // dependent latency threshold. The ten-user-turn/body contract
            // and loss of native access above supply the regression criteria.
            eprintln!(
                "normal_open_sample {}",
                serde_json::json!({"case":name,"phase":"saved","bytes":bytes,"micros":micros})
            );
        }
    }
}
