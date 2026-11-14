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
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: Uuid::new_v4(),
            command: Command::ResumeQueue,
        }),
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
