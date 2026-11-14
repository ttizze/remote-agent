//! Behavioral migration checks against T3 Code 1a12c4afae998925b4a29417f2dc436c0ccd0969.
//! ShellStream.ts, AgentSessionImporter.ts, ThreadLaunchService.ts and
//! orchestrationV2Timeline define the expectations, independently of old Rows.
use super::native_fixture::{NativeAgent, call};
use super::*;
use agent_protocol::{orchestration as model, protocol::Call};

fn input(provider: ProviderKind, text: &str) -> decision::InputContent {
    decision::InputContent {
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
            cached: None,
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
async fn created_and_renamed_threads_are_shared_as_t3_shells() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    create(&service, directory.path().to_str().unwrap(), None).await;
    let connection =
        rusqlite::Connection::open(directory.path().join("bex-conversations.sqlite")).unwrap();
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!exists, "obsolete storage table remains: {table}");
}

async fn running(
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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

#[tokio::test]
async fn update_restart_continues_the_same_conversation_once_without_repeating_user_input() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let projects = directory.path().join("projects.json");
    let service = HostRpcService::new([agent.backend()], projects.clone());
    let (session, native, _) = running(&service, &agent, directory.path().to_str().unwrap()).await;
    service
        .inner
        .conversation
        .storage
        .set_updating(true)
        .unwrap();
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
        Runtime::recover(&service, &session, ProviderKind::Codex)
            .await
            .unwrap();
    }
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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

#[tokio::test]
async fn generated_images_keep_t3_tool_results_and_markdown_embeds() {
    use agent_protocol::items::{Item, ItemBody};
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("generated image.png");
    std::fs::write(&image,base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a+X0AAAAASUVORK5CYII=").unwrap()).unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    let tool = current
        .projection
        .turn_items
        .iter()
        .find_map(|item| match &item.body {
            model::TurnItemBody::DynamicTool {
                tool_name: Some(name),
                output: Some(output),
                ..
            } if name == "image_gen.imagegen" => Some(output),
            _ => None,
        })
        .expect("image generation tool result");
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

#[tokio::test]
async fn refreshed_native_catalog_is_visible_as_a_canonical_thread_without_inventing_a_run() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "catalog-native".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(native.clone()),
                name: Some("Native catalog marker".into()),
                cwd: Some(directory.path().to_str().unwrap().into()),
                updated_at: Some(1.0),
                turns: Some(vec![]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    service.start().unwrap();
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
                assert_eq!(thread["itemCount"], 0);
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    // provider-thread.rollback; native fork is the unsupported-provider fallback.
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    use agent_protocol::{
        execution::{BackgroundTask, BackgroundTaskKind, ItemStatus, TurnStatus},
        items::{Item, ItemBody},
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "project-native".into()).unwrap();
    agent.records.lock().unwrap().insert(
        native.id.clone(),
        agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(native.clone()),
                name: Some("project import marker".into()),
                cwd: Some(directory.path().to_str().unwrap().into()),
                updated_at: Some(1.0),
                turns: Some(vec![]),
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
            through: first.projection.runs[0].id.clone().into(),
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
#[tokio::test]
async fn switching_back_reuses_native_history_or_recovers_uncertain_injection(#[case] fault: &str) {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    send(
        &service,
        &session,
        ProviderKind::Codex,
        "current-user-marker",
    )
    .await;
    let current = settled(&service, &session, 3).await;
    let inputs = codex.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2, "one native prompt per accepted input");
    assert_eq!(claude.inputs.lock().unwrap().len(), 1);
    let injected = codex.injected.lock().unwrap();
    let recovering = fault == "injection_unknown";
    assert_eq!(injected.len(), if recovering { 2 } else { 1 });
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
            assert_eq!(handoff.from_provider_thread_ids, [owner.id.clone()]);
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
async fn portable_forks_include_only_the_selected_source_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    assert_eq!(
        attempt.native_thread_id,
        provider_thread
            .native_thread_ref
            .as_ref()
            .unwrap()
            .native_id
    );
}

#[tokio::test]
async fn switching_back_replaces_a_full_native_context_before_appending_history() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    assert!(ack.error.is_none());
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
            through: first.projection.runs[0].id.clone().into(),
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

#[tokio::test]
async fn first_injection_recovery_keeps_its_provider_without_inventing_a_source_model() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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

#[tokio::test]
async fn a_spent_native_fork_uses_fresh_portable_context_without_an_extra_handoff_card() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    assert!(ack.error.is_none());
}

#[tokio::test]
async fn a_source_busy_after_admission_keeps_its_native_plan_until_fresh_allocation_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
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
            through: first.projection.runs[0].id.clone().into(),
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
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
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
