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
    let injected = claude.injected.lock().unwrap();
    assert_eq!(injected.len(), 1);
    assert!(
        serde_json::to_string(&injected[0].1)
            .unwrap()
            .contains("prior-history-marker")
    );
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
    assert_eq!(snapshot(&service, &source), first);
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
    let inputs = agent.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_ne!(inputs[0].thread_id, inputs[1].thread_id);
    assert_eq!(snapshot(&service, &source), first);
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
