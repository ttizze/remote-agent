//! The same observable conversation contract against both native subprocess fixtures.
use agent_protocol::{
    execution::{ErrorCategory, TurnStatus},
    ids::{ClientInputId, TurnId},
    items::ItemBody,
    models::{Empty, Turn, WorktreeSettings},
    operations as op,
    protocol::Call,
    requests::{Answer, QuestionAnswer, RequestBody},
    session::{ProviderKind, SessionRef},
};
use agent_transport::client::Client;
// Each transport test binary uses a different subset of these shared helpers.
#[allow(dead_code)]
#[path = "support/conversations.rs"]
mod conversations;
use agent_protocol::conversation::Command;
use conversations::{Conversation, submit};
use host_fixture::test_support::{HostFixture, Memory};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

async fn open(
    client: &Client,
    session: &SessionRef,
) -> (agent_protocol::models::ThreadResponse, Conversation) {
    let stream = Conversation::open(client, session).await.unwrap();
    (stream.view(), stream)
}
/// A queued input as the user sent it. Its run carries the resolved T3 model
/// selection rather than the request's.
fn queued_inputs(
    thread: &agent_protocol::models::Thread,
) -> Vec<(&agent_protocol::ids::ClientInputId, &[op::Input])> {
    thread
        .queue
        .inputs
        .iter()
        .map(|queued| (&queued.client_user_message_id, queued.input.as_slice()))
        .collect()
}
fn sent(input: &op::Submission) -> (&agent_protocol::ids::ClientInputId, &[op::Input]) {
    (&input.client_user_message_id, input.input.as_slice())
}
async fn finished(events: &mut Conversation) -> Turn {
    events.finished().await
}
async fn finished_with_input(events: &mut Conversation, input: &ClientInputId) -> TurnId {
    let turn = events.input_turn(input, true).await;
    assert_eq!(turn.status, TurnStatus::Completed, "{turn:#?}");
    turn.id
}

fn submission(session: &SessionRef, text: &str) -> op::Submission {
    op::Submission {
        fresh_context: false,
        thread_id: session.clone(),
        client_user_message_id: ClientInputId::from(uuid::Uuid::new_v4().to_string()),
        input: vec![op::Input::Text { text: text.into() }],
        model: None,
        effort: None,
        service_tier: None,
        follow_up: op::FollowUp::Queue,
    }
}
fn prompt(provider: ProviderKind, scenario: &str) -> &str {
    match (provider, scenario) {
        (ProviderKind::Codex, "wait") => "[delayed-input]",
        (ProviderKind::Codex, "approval") => "[approval]",
        (ProviderKind::Codex, "question") => "[request]",
        (ProviderKind::Codex, "crash") => "[crash]",
        (ProviderKind::Claude, "approval") => "permission",
        (_, other) => other,
    }
}
async fn create(client: &Client, provider: ProviderKind, root: &Path) -> SessionRef {
    conversations::created(
        client,
        conversations::create_request(provider, Some(root.to_string_lossy().into_owned()), None),
    )
    .await
    .unwrap()
    .thread
    .id
    .unwrap()
}

async fn start(root: &Path, memory: Arc<Memory>) -> HostFixture {
    let claude = Path::new(env!("CARGO_BIN_EXE_agent-claude-fixture"));
    #[cfg(windows)]
    let executable = claude.with_extension("");
    #[cfg(windows)]
    let claude = executable.as_path();
    let program = host_fixture::fixture::Config {
        trace: true,
        stream_delay_ms: 5,
        ..Default::default()
    }
    .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), root)
    .unwrap();
    HostFixture::start(
        root,
        codex_app_server::AppServerConfig {
            program,
            ..Default::default()
        },
        memory,
        "adapter conformance",
        false,
        Some(claude),
        None,
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_import_discovers_native_projects_and_continues_the_original_session() {
    use agent_core::{
        state::{Intent, Snapshot, operations as core_op},
        store::Store,
    };
    use agent_transport::transport::{Endpoint, Relays};
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let original_directory = tempfile::tempdir().unwrap();
    let original_rollout = original_directory.path().join("original-session.jsonl");
    std::fs::write(&original_rollout, "original session history\n").unwrap();
    let repository = root.join("existing-project");
    let other = root.join("unselected-project");
    for path in [&repository, &other] {
        std::fs::create_dir_all(path).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let threads: Vec<_> = (0..106).map(|index| serde_json::json!({
        "id":format!("external-{index}"), "cwd":if index == 105 { &other } else { &repository },
        "path":original_rollout,
        "name":format!("Existing conversation {index}"), "updatedAt":now, "model":"fixture-model",
        "historyMode":"paginated", "status":{"type":"notLoaded"},
        "turns":[{"id":"original-turn","status":"completed","items":[
            {"id":"original-user","type":"userMessage","content":[{"type":"text","text":"original input"}]},
            {"id":"original-answer","type":"agentMessage","text":"original answer","phase":"final_answer"}
        ]}]
    })).collect();
    std::fs::write(
        root.join("list-fixture.json"),
        serde_json::to_vec(&threads).unwrap(),
    )
    .unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let endpoint = Endpoint::bind(host.credentials.local_identity().await, Relays::Disabled)
        .await
        .unwrap();
    let store = Store::connect(
        &endpoint,
        &host.ticket,
        Snapshot::default(),
        None,
        "Fixture device",
        None,
    )
    .await
    .unwrap();
    store
        .dispatch(Intent::ScanProjects(core_op::ScanProjects {}))
        .await
        .unwrap();
    let scan = store.snapshot();
    let candidates = &scan.project_import.scan.as_ref().unwrap().projects;
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate.name == "existing-project")
            .unwrap()
            .conversation_count,
        105,
        "scan must follow native pagination"
    );
    let selected = dunce::simplified(&repository).to_str().unwrap().to_owned();
    assert_eq!(
        scan.project_import.selected.iter().collect::<Vec<_>>(),
        [&selected]
    );
    assert!(
        !root.join("bex-projects.json").exists(),
        "scanning and skipping cannot register anything"
    );
    store
        .dispatch(Intent::ImportProjects(core_op::ImportProjects {}))
        .await
        .unwrap();
    let result = store.snapshot();
    assert_eq!(
        result
            .project_import
            .result
            .as_ref()
            .unwrap()
            .imported
            .as_slice(),
        std::slice::from_ref(&selected)
    );
    assert!(
        result
            .project_import
            .result
            .as_ref()
            .unwrap()
            .errors
            .is_empty()
    );
    let catalog = conversations::Catalog::open(&host.local().await.unwrap().peer).await;
    let registered = catalog
        .snapshot
        .projects
        .iter()
        .map(|project| agent_protocol::models::Project {
            id: project.id.clone(),
            name: project.title.clone(),
            roots: vec![agent_protocol::models::ProjectRoot {
                path: project.workspace_root.clone(),
            }],
            icon: None,
        })
        .collect::<Vec<_>>();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].roots[0].path, selected);
    let local = host.local().await.unwrap();
    let id = SessionRef::new(ProviderKind::Codex, "external-0".into()).unwrap();
    let (opened, mut events) = open(&local.peer, &id).await;
    assert_eq!(opened.thread.id.as_ref(), Some(&id));
    assert_eq!(
        opened.thread.project_id,
        agent_protocol::models::ProjectMembership::Assigned(registered[0].id.clone())
    );
    // T3 imports native history as items of the conversation, not native turn IDs.
    let imported: Vec<_> = opened
        .thread
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter_map(|item| match item.body() {
            ItemBody::UserMessage { text, .. } => text.as_deref(),
            ItemBody::AssistantText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(imported, ["original input", "original answer"]);
    submit(
        &local.peer,
        &submission(&id, "continue imported conversation"),
    )
    .await
    .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(&original_rollout).unwrap(),
        "original session history\n",
        "continuing an imported fixture must preserve the original native rollout"
    );
    drop(events);
    local.close().await;
    store.close().await.unwrap();
    endpoint.close().await;
    host.close().await.unwrap();
}

#[rstest::rstest]
#[case::codex_stalls(true, false)]
#[case::claude_stalls(false, true)]
#[case::both_stall(true, true)]
#[tokio::test]
async fn setup_catalog_isolates_stalled_native_providers(
    #[case] codex_stalls: bool,
    #[case] claude_stalls: bool,
) {
    use agent_core::{presentation::connections::AgentAvailability, state::operations::Operation};
    use agent_protocol::protocol::Response;
    use host_daemon::HostRpcService;
    use serde_json::json;

    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let codex_home = root.join("codex");
    let claude_home = root.join("claude-native");
    std::fs::create_dir_all(&claude_home).unwrap();
    let program = host_fixture::fixture::Config::default()
        .install(
            Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")),
            &codex_home,
        )
        .unwrap();
    std::fs::write(
        codex_home.join("account-fixture.json"),
        r#"{"type":"apiKey"}"#,
    )
    .unwrap();
    std::fs::write(
        claude_home.join("fixture-auth.json"),
        json!({"tokenSource":"oauth","apiKeySource":"none","email":"claude@example.invalid","subscriptionType":"max"}).to_string(),
    ).unwrap();
    let config = codex_app_server::AppServerConfig {
        program,
        ..Default::default()
    };
    let server = Arc::new(
        codex_app_server::CodexAppServer::spawn(config.clone())
            .await
            .unwrap(),
    );
    let codex = host_daemon::providers::codex::Codex::new(Ok(server.clone()));
    codex
        .enable_accounts(root.join("accounts"), config)
        .await
        .unwrap();
    let claude = host_daemon::providers::Claude::load(
        Path::new(env!("CARGO_BIN_EXE_agent-claude-fixture")).to_owned(),
        root.join("claude"),
        Some(claude_home.clone()),
    )
    .await
    .unwrap();
    let service = HostRpcService::new([codex.into(), claude.into()], root.join("worktrees.json"));
    let session = service.open_session();
    let accounts = service
        .dispatch(session.id(), &Call::ListAccounts(Empty {}))
        .await
        .unwrap();
    let Response::Success { result: accounts } =
        agent_protocol::protocol::decode::<Response<op::Accounts>>(&accounts.initial).unwrap()
    else {
        panic!("native accounts must remain readable before discovery");
    };
    let mut snapshot = agent_core::state::Snapshot {
        connection: agent_core::state::Connection {
            connected: true,
            ..Default::default()
        },
        ..Default::default()
    };
    agent_core::state::operations::ListAccounts {}.apply(&mut snapshot, "", accounts);
    // Account checks also use SDK initialization. Stall only the model discovery
    // phase whose timeout and recovery this contract verifies.
    for (home, stalls) in [(&codex_home, codex_stalls), (&claude_home, claude_stalls)] {
        std::fs::write(
            home.join("model-list-mode"),
            if stalls { "silent" } else { "ready" },
        )
        .unwrap();
    }
    let mut discoveries = tokio::task::JoinSet::new();
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        let service = service.clone();
        let session_id = session.id();
        discoveries.spawn(async move {
            let response = service
                .dispatch(
                    session_id,
                    &Call::ListModels(op::ListModels {
                        provider,
                        limit: 100,
                        cursor: None,
                    }),
                )
                .await
                .unwrap();
            (provider, response)
        });
    }
    let healthy = usize::from(!codex_stalls) + usize::from(!claude_stalls);
    for index in 0..2 {
        let deadline = if index < healthy { 10 } else { 25 };
        let (provider, response) =
            tokio::time::timeout(Duration::from_secs(deadline), discoveries.join_next())
                .await
                .expect("a healthy provider must finish without waiting for stalled discovery")
                .unwrap()
                .unwrap();
        let Response::Success { result: catalog } =
            agent_protocol::protocol::decode::<Response<op::ModelPage>>(&response.initial).unwrap()
        else {
            panic!("discovery must return its provider's catalog or error");
        };
        agent_core::state::operations::LoadModels { provider }.apply(&mut snapshot, "", catalog);
        if index < healthy {
            assert!(
                snapshot.connection_setup().can_start,
                "healthy models must be usable before the stalled provider finishes"
            );
        }
    }
    let setup = snapshot.connection_setup();
    assert_eq!(setup.can_start, !codex_stalls || !claude_stalls);
    for (provider, stalls) in [
        (ProviderKind::Codex, codex_stalls),
        (ProviderKind::Claude, claude_stalls),
    ] {
        assert_eq!(
            snapshot
                .catalog
                .models
                .iter()
                .any(|model| model.model.provider == provider),
            !stalls
        );
        let error = snapshot.catalog.model_errors.get(provider.key());
        if stalls {
            assert_eq!(error.unwrap()["code"], "provider_timeout");
        } else {
            assert!(error.is_none());
        }
        let agent = setup
            .agents
            .iter()
            .find(|agent| agent.provider == provider)
            .unwrap();
        assert_eq!(
            agent.availability,
            if stalls {
                AgentAvailability::Unavailable
            } else {
                AgentAvailability::Ready
            }
        );
    }
    for home in [&codex_home, &claude_home] {
        std::fs::write(home.join("model-list-mode"), "ready").unwrap();
    }
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        let retried = tokio::time::timeout(
            Duration::from_secs(10),
            service.dispatch(
                session.id(),
                &Call::ListModels(op::ListModels {
                    provider,
                    limit: 100,
                    cursor: None,
                }),
            ),
        )
        .await
        .expect("discovery must recover after its native provider responds again")
        .unwrap();
        let Response::Success { result: catalog } =
            agent_protocol::protocol::decode::<Response<op::ModelPage>>(&retried.initial).unwrap()
        else {
            panic!("retry must return the recovered provider catalog");
        };
        agent_core::state::operations::LoadModels { provider }.apply(&mut snapshot, "", catalog);
    }
    assert!(snapshot.catalog.model_errors.is_empty());
    let setup = snapshot.connection_setup();
    assert!(setup.can_start);
    assert!(
        setup
            .agents
            .iter()
            .filter(|agent| agent.provider != ProviderKind::DeepSeek)
            .all(|agent| agent.availability == AgentAvailability::Ready)
    );
    drop(session);
    server.shutdown().await.unwrap();
}

async fn scenarios(provider: ProviderKind) {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let memory = Arc::new(Memory::default());
    let host = start(&root, memory.clone()).await;
    let local = host.local().await.unwrap();
    let catalog = local
        .peer
        .call(&op::LoadComposerCatalog {
            cwd: root.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    assert!(
        catalog
            .candidates
            .iter()
            .any(|candidate| candidate.invocation.provider == ProviderKind::Codex)
    );
    assert!(
        catalog
            .candidates
            .iter()
            .any(|candidate| candidate.invocation.provider == ProviderKind::Claude)
    );
    // Creation, sending, typed streamed output, history, and client reconnection.
    let session = create(&local.peer, provider, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    let first = submission(&session, "first input");
    submit(&local.peer, &first).await.unwrap();
    let turn = finished(&mut events).await;
    assert_eq!(turn.status, TurnStatus::Completed);
    assert!(
        turn.items.iter().flatten().any(
            |item| matches!(item.body(),ItemBody::AssistantText {text,..} if !text.is_empty())
        )
    );
    assert!(
        turn.items
            .iter()
            .flatten()
            .any(|item| item.client_input_id.as_ref() == Some(&first.client_user_message_id))
    );
    let (history, _) = open(&local.peer, &session).await;
    assert!(history.thread.turns.iter().flatten().any(|saved| {
        saved
            .items
            .iter()
            .flatten()
            .any(|item| item.client_input_id.as_ref() == Some(&first.client_user_message_id))
    }));
    local.close().await;
    let local = host.local().await.unwrap();
    let (_, mut events) = open(&local.peer, &session).await;
    let receipt = submit(&local.peer, &submission(&session, "resumed input"))
        .await
        .unwrap();
    receipt.accepted().unwrap();
    let resumed = finished(&mut events).await;
    assert_ne!(resumed.id, turn.id);
    assert_eq!(resumed.status, TurnStatus::Completed);
    let skill = catalog
        .candidates
        .iter()
        .find(|candidate| {
            candidate.invocation.provider == provider
                && candidate.invocation.kind == agent_protocol::composer::InvocationKind::Skill
        })
        .unwrap()
        .invocation
        .clone();
    let mut invoke = submission(&session, &skill.token());
    invoke.input.push(skill.input());
    submit(&local.peer, &invoke).await.unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    if provider == ProviderKind::Claude {
        let received = session_inputs(root.join(format!(
            "claude-session-{}.jsonl",
            conversations::native_source(&root, &session, provider)
        )));
        let text = received.as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text.matches(&format!("/{}", skill.name)).count(), 1);
        assert!(!text.contains(&skill.token()));
    }

    // Resume after the Host and native subprocesses have actually exited.
    let native = conversations::native_source(&root, &session, provider);
    local.close().await;
    host.close().await.unwrap();
    if provider == ProviderKind::Codex {
        // Codex's fixture loads archived native state from this file;
        // Claude's fixture writes its native transcript during the first run.
        std::fs::write(root.join("list-fixture.json"), serde_json::json!([{
            "id":native,"cwd":root,"model":"fixture-model","historyMode":"paginated","status":{"type":"notLoaded"},
            "turns":[{"id":"archived-turn","status":"completed","items":[
                {"id":"archived-user","type":"userMessage","content":[{"type":"text","text":"first input"}]},
                {"id":"archived-answer","type":"agentMessage","text":"saved reply","phase":"final_answer"}
            ]}]
        }]).to_string()).unwrap();
    }
    let host = start(&root, memory.clone()).await;
    let local = host.local().await.unwrap();
    let _catalog = conversations::Catalog::open(&local.peer).await;
    if provider == ProviderKind::Codex {
        // The archived native state reaches the Host transcript on re-import.
        local
            .peer
            .request::<agent_protocol::project_import::ProjectImportResult>(&Call::ImportProjects(
                agent_protocol::project_import::ImportProjects {
                    paths: vec![root.to_str().unwrap().into()],
                },
            ))
            .await
            .unwrap();
        for _ in 0..500 {
            if open(&local.peer, &session).await.0.model.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    let (history, mut events) = open(&local.peer, &session).await;
    if provider == ProviderKind::Codex {
        assert_eq!(
            history.model,
            Some(agent_protocol::models::ModelRef {
                provider,
                id: "fixture-model".into(),
            })
        );
    }
    assert!(history.thread.turns.iter().flatten().any(|turn| {
        turn.items.iter().flatten().any(|item| {
        matches!(item.body(), ItemBody::UserMessage { text: Some(text), .. } if text == "first input")
    })
    }));
    let trace_offset = if provider == ProviderKind::Codex {
        std::fs::read_to_string(root.join("rpc-trace.jsonl"))
            .unwrap()
            .lines()
            .count()
    } else {
        0
    };
    let after_restart = submission(&session, "after process restart");
    submit(&local.peer, &after_restart).await.unwrap();
    let restarted = events
        .input_turn(&after_restart.client_user_message_id, true)
        .await;
    let debug_trace = std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap_or_default();
    let debug_tail: Vec<_> = debug_trace.lines().skip(trace_offset).collect();
    assert_eq!(
        restarted.status,
        TurnStatus::Completed,
        "{:?} {debug_tail:#?}",
        restarted.error
    );
    if provider == ProviderKind::Codex {
        let trace = std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap();
        let operations: Vec<_> = trace
            .lines()
            .skip(trace_offset)
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect();
        for (method, expected) in [("thread/read", 0), ("thread/resume", 1)] {
            assert_eq!(
                operations
                    .iter()
                    .filter(|event| event["method"] == method)
                    .count(),
                expected,
                "submission uses the saved conversation and resumes the same native source: {operations:?}"
            );
        }
    }

    // Input sent while the initial turn is live runs once: a steer joins that
    // turn, and queued input waits in the Host and runs as the next turn. A
    // stop holds the queue until it is resumed. The routing matrix is tested
    // with the Host's submission decisions.
    for case in ["steer", "queue", "stop"] {
        let session = create(&local.peer, provider, &root).await;
        let (_, mut events) = open(&local.peer, &session).await;
        let initial = submission(&session, prompt(provider, "wait"));
        submit(&local.peer, &initial).await.unwrap();
        let turn = events
            .input_turn(&initial.client_user_message_id, false)
            .await
            .id;
        // Follow-ups target a live turn: wait until the provider started it.
        while !events
            .view()
            .thread
            .turns
            .iter()
            .flatten()
            .any(|live| live.id == turn && live.started_at_ms.is_some())
        {
            events.next().await;
        }
        let follow_up = op::Submission {
            follow_up: if case == "steer" {
                op::FollowUp::Steer
            } else {
                op::FollowUp::Queue
            },
            ..submission(&session, &format!("{case} follow-up"))
        };
        let receipt = submit(&local.peer, &follow_up).await.unwrap();
        let id = &follow_up.client_user_message_id;
        receipt.accepted().unwrap();
        if case != "steer" {
            let (opened, _) = open(&local.peer, &session).await;
            assert_eq!(queued_inputs(&opened.thread), [sent(&follow_up)]);
        }
        if case == "stop" {
            conversations::command(
                &local.peer,
                &session,
                uuid::Uuid::new_v4(),
                Command::Interrupt { turn: None },
            )
            .await
            .unwrap();
            assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
            let (opened, _) = open(&local.peer, &session).await;
            assert!(opened.thread.queue.held);
            assert_eq!(queued_inputs(&opened.thread), [sent(&follow_up)]);
            conversations::command(
                &local.peer,
                &session,
                uuid::Uuid::new_v4(),
                Command::ResumeQueue,
            )
            .await
            .unwrap();
        } else {
            std::fs::write(root.join("release-inputs"), "").unwrap();
        }
        let turn_id = finished_with_input(&mut events, id).await;
        assert_eq!(turn_id == turn, case == "steer");
        let (history, _) = open(&local.peer, &session).await;
        assert_eq!(history.thread.queue, Default::default());
        let saved = history
            .thread
            .turns
            .iter()
            .flatten()
            .find(|saved| saved.id == turn_id)
            .unwrap();
        assert_eq!(
            saved
                .items
                .iter()
                .flatten()
                .filter(|item| item.client_input_id.as_ref() == Some(id))
                .count(),
            1
        );
    }
    // Approval and questions use only the choices supplied by the adapter.
    for scenario in ["approval", "question"] {
        let session = create(&local.peer, provider, &root).await;
        let (_, mut events) = open(&local.peer, &session).await;
        submit(
            &local.peer,
            &submission(&session, prompt(provider, scenario)),
        )
        .await
        .unwrap();
        let request = loop {
            if let Some(request) = events.view().thread.requests.values().next() {
                break request.clone();
            }
            events.next().await;
        };
        let answer = match &request.body {
            RequestBody::Approval { choices, .. } => Answer::Approval {
                choice_id: choices
                    .iter()
                    .find(|c| c.label == "承認")
                    .unwrap()
                    .id
                    .clone(),
            },
            RequestBody::Question { questions } => Answer::Questions {
                answers: questions
                    .iter()
                    .map(|q| {
                        (
                            q.id.clone(),
                            QuestionAnswer::SingleChoice {
                                choice_id: q.choices[0].id.clone(),
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            },
            other => panic!("unexpected conformance request: {other:?}"),
        };
        conversations::command(
            &local.peer,
            &session,
            uuid::Uuid::new_v4(),
            Command::Answer {
                id: request.id.clone(),
                answer: answer.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
        assert!(
            conversations::command(
                &local.peer,
                &session,
                uuid::Uuid::new_v4(),
                Command::Answer {
                    id: request.id.clone(),
                    answer
                }
            )
            .await
            .is_err()
        );
    }
    // Abnormal provider exit terminates the owned execution with neutral evidence.
    let session = create(&local.peer, provider, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    let _ = submit(
        &local.peer,
        &submission(&session, prompt(provider, "crash")),
    )
    .await;
    let failed = finished(&mut events).await;
    assert_eq!(failed.status, TurnStatus::Failed);
    assert_eq!(failed.error.unwrap().category, ErrorCategory::Network);
    let other = if provider == ProviderKind::Codex {
        ProviderKind::Claude
    } else {
        ProviderKind::Codex
    };
    let healthy = create(&local.peer, other, &root).await;
    let (_, mut healthy_events) = open(&local.peer, &healthy).await;
    submit(
        &local.peer,
        &submission(&healthy, "other provider is healthy"),
    )
    .await
    .unwrap();
    assert_eq!(
        finished(&mut healthy_events).await.status,
        TurnStatus::Completed
    );
    // PC operations remain available after one provider fails.
    let _catalog = conversations::Catalog::open(&local.peer).await;
    local
        .peer
        .request::<WorktreeSettings>(&Call::ReadWorktreeSettings(Empty {}))
        .await
        .unwrap();
    local.close().await;
    host.close().await.unwrap();
}

/// Read the inputs the Claude fixture appended to its workspace trace.
fn session_inputs(path: impl AsRef<Path>) -> serde_json::Value {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_conforms_to_host_conversations() {
    scenarios(ProviderKind::Codex).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_conforms_to_host_conversations() {
    scenarios(ProviderKind::Claude).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adapter_initialization_failure_preserves_the_other_provider_and_host() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(
        root.join("bex-worktrees.json"),
        serde_json::json!({
            "settings":{"createOnNewSession":true,"worktreeDirectory":root.join("worktrees")}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("claude/accounts")).unwrap();
    std::fs::write(
        root.join("claude/accounts/accounts.json"),
        "invalid registry",
    )
    .unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    local
        .peer
        .call(&op::AddProject {
            cwd: root.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let models = local
        .peer
        .call(&op::ListModels {
            provider: agent_protocol::session::ProviderKind::Codex,
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(!models.data.is_empty());
    assert!(models.provider_errors.is_none());
    let unavailable = local
        .peer
        .call(&op::ListModels {
            provider: ProviderKind::Claude,
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(unavailable.data.is_empty());
    assert_eq!(
        unavailable.provider_errors.unwrap()["claude"]["code"],
        "provider_unavailable"
    );
    let session = create(&local.peer, ProviderKind::Codex, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    submit(&local.peer, &submission(&session, "healthy provider"))
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let worktrees = local
        .peer
        .request::<Vec<agent_protocol::models::Worktree>>(&Call::ListWorktrees(Empty {}))
        .await
        .unwrap();
    assert_eq!(worktrees.len(), 1);
    assert!(
        worktrees[0]
            .blocked_reason
            .as_deref()
            .unwrap()
            .contains("稼働状況を確認できない")
    );
    assert!(
        local
            .peer
            .call(&op::RemoveWorktree {
                path: worktrees[0].path.clone()
            })
            .await
            .is_err()
    );
    assert!(Path::new(&worktrees[0].path).is_dir());
    assert!(
        conversations::created(
            &local.peer,
            conversations::create_request(
                ProviderKind::Claude,
                Some(root.to_string_lossy().into_owned()),
                None
            )
        )
        .await
        .is_err()
    );
    local
        .peer
        .request::<WorktreeSettings>(&Call::ReadWorktreeSettings(Empty {}))
        .await
        .unwrap();
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_native_rewind_resumes_the_kept_assistant_through_rpc_and_the_real_sdk() {
    // T3 retains the first native assistant, removes the second app run, then
    // resumes the same native session at that assistant. CLI history must also
    // exclude the discarded input; a canonical-only cut cannot satisfy this.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let memory = Arc::new(Memory::default());
    let host = start(&root, memory.clone()).await;
    let local = host.local().await.unwrap();
    let session = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    let first = submission(&session, "remember-prefix");
    submit(&local.peer, &first).await.unwrap();
    let first_run = events
        .input_turn(&first.client_user_message_id, true)
        .await
        .id;
    let snapshot = &events.projection.snapshot.projection;
    let attempt = snapshot
        .runs
        .iter()
        .find(|run| run.id == first_run.as_str())
        .unwrap()
        .active_attempt_id
        .as_ref()
        .unwrap();
    let kept_cursor = snapshot
        .provider_turns
        .iter()
        .filter(|turn| turn.run_attempt_id.as_ref() == Some(attempt))
        .max_by_key(|turn| turn.ordinal)
        .unwrap()
        .native_turn_ref
        .as_ref()
        .unwrap()
        .native_id
        .clone()
        .unwrap();
    assert!(
        !kept_cursor.starts_with("turn:"),
        "SDK wrote a real assistant cursor"
    );
    let tail = submission(&session, "discard-tail");
    submit(&local.peer, &tail).await.unwrap();
    let discarded = events
        .input_turn(&tail.client_user_message_id, true)
        .await
        .id;
    let native = conversations::native_source(&root, &session, ProviderKind::Claude);
    conversations::command(
        &local.peer,
        &session,
        uuid::Uuid::new_v4(),
        Command::Rewind(discarded.clone()),
    )
    .await
    .expect("Claude exposes native conversation rewind");
    while events
        .view()
        .thread
        .turns
        .iter()
        .flatten()
        .any(|turn| turn.id == discarded)
    {
        events.next().await;
    }
    let snapshot = &events.projection.snapshot.projection;
    let selected = snapshot
        .provider_threads
        .iter()
        .find(|provider| Some(&provider.id) == snapshot.thread.active_provider_thread_id.as_ref())
        .unwrap();
    assert_eq!(
        selected
            .native_thread_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_deref(),
        Some(native.as_str())
    );
    assert_eq!(
        selected
            .native_conversation_head_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_deref(),
        Some(kept_cursor.as_str())
    );
    assert!(
        events
            .view()
            .thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.id == first_run)
    );
    let next = submission(&session, "after-rewind");
    submit(&local.peer, &next).await.unwrap();
    let answer = events.input_turn(&next.client_user_message_id, true).await;
    let text = answer
        .items
        .iter()
        .flatten()
        .filter_map(|item| match item.body() {
            ItemBody::AssistantText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(
        text, "reply 2: after-rewind",
        "the private CLI actually resumed the kept native prefix, excluding discard-tail"
    );
    assert_eq!(
        conversations::native_source(&root, &session, ProviderKind::Claude),
        native
    );
    let rows = claude_native_rows(&root, &native);
    assert!(
        rows.iter().any(|row| row["message"]["content"]
            .to_string()
            .contains("discard-tail")),
        "rewind retains the native audit tail"
    );
    let resumed_user = rows
        .iter()
        .find(|row| {
            row["message"]["content"]
                .to_string()
                .contains("after-rewind")
                && row["type"] == "user"
        })
        .unwrap();
    assert_eq!(
        resumed_user["parentUuid"], kept_cursor,
        "the new branch links to the retained assistant"
    );
    local.close().await;
    host.close().await.unwrap();
    let host = start(&root, memory).await;
    let local = host.local().await.unwrap();
    let (_, mut events) = open(&local.peer, &session).await;
    let next = submission(&session, "after-restart");
    submit(&local.peer, &next).await.unwrap();
    let answer = events.input_turn(&next.client_user_message_id, true).await;
    assert_eq!(
        claude_answer(&answer),
        "reply 3: after-restart",
        "restart without a resume pin follows the selected native branch"
    );
    assert_eq!(
        conversations::native_source(&root, &session, ProviderKind::Claude),
        native
    );
    local.close().await;
    host.close().await.unwrap();
}

fn claude_answer(turn: &Turn) -> String {
    turn.items
        .iter()
        .flatten()
        .filter_map(|item| match item.body() {
            ItemBody::AssistantText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn claude_native_rows(root: &Path, native: &str) -> Vec<serde_json::Value> {
    let path = std::fs::read_dir(root.join("claude-native/projects"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join(format!("{native}.jsonl")))
        .find(|path| path.is_file())
        .expect("actual CLI transcript exists");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[rstest::rstest]
#[case::empty(false)]
#[case::fork(true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_native_baseline_rewind_preserves_only_the_inherited_prefix(#[case] fork: bool) {
    // Pinned ClaudeAdapterV2 uses forkHeadMessageId at a fork baseline, while
    // a genuinely empty baseline allocates a fresh native/logical owner.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let source = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut source_events) = open(&local.peer, &source).await;
    let input = submission(&source, "inherited-prefix");
    submit(&local.peer, &input).await.unwrap();
    let first = source_events
        .input_turn(&input.client_user_message_id, true)
        .await;
    assert_eq!(claude_answer(&first), "reply 1: inherited-prefix");
    let session = if fork {
        let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let returned: SessionRef = local
            .peer
            .request(&Call::ForkConversation(
                agent_protocol::conversation::records::Fork {
                    source: source.clone(),
                    target: target.clone(),
                    through: agent_protocol::conversation::records::ForkPoint::Run {
                        run_id: first.id.to_string(),
                    },
                },
            ))
            .await
            .unwrap();
        assert_eq!(returned, target);
        target
    } else {
        source.clone()
    };
    let (_, mut events) = open(&local.peer, &session).await;
    let discarded = if fork {
        let input = submission(&session, "discard-child");
        submit(&local.peer, &input).await.unwrap();
        let turn = events.input_turn(&input.client_user_message_id, true).await;
        assert_eq!(
            claude_answer(&turn),
            "reply 2: discard-child",
            "native fork inherits its selected prefix"
        );
        turn.id
    } else {
        first.id.clone()
    };
    let before = conversations::native_source(&root, &session, ProviderKind::Claude);
    let fork_head = events
        .projection
        .snapshot
        .projection
        .provider_threads
        .iter()
        .find(|thread| {
            Some(&thread.id)
                == events
                    .projection
                    .snapshot
                    .projection
                    .thread
                    .active_provider_thread_id
                    .as_ref()
        })
        .unwrap()
        .native_metadata
        .as_ref()
        .and_then(|meta| meta.fork_head_message_id.clone());
    if fork {
        assert!(
            fork_head.is_some(),
            "the actual SDK fork supplies its remapped baseline head"
        );
    }
    conversations::command(
        &local.peer,
        &session,
        uuid::Uuid::new_v4(),
        Command::Rewind(discarded.clone()),
    )
    .await
    .unwrap();
    while events
        .view()
        .thread
        .turns
        .iter()
        .flatten()
        .any(|turn| turn.id == discarded)
    {
        events.next().await;
    }
    let after = conversations::native_source(&root, &session, ProviderKind::Claude);
    if fork {
        assert_eq!(
            after, before,
            "fork baseline must keep the native inherited prefix"
        );
        let snapshot = &events.projection.snapshot.projection;
        let selected = snapshot
            .provider_threads
            .iter()
            .find(|thread| Some(&thread.id) == snapshot.thread.active_provider_thread_id.as_ref())
            .unwrap();
        assert_eq!(
            selected
                .native_conversation_head_ref
                .as_ref()
                .and_then(|head| head.native_id.as_ref()),
            fork_head.as_ref()
        );
        assert!(
            events
                .view()
                .thread
                .turns
                .iter()
                .flatten()
                .any(|turn| turn.id == first.id),
            "inherited display survives the child's rewind"
        );
    } else {
        assert_ne!(after, before, "an empty baseline gets a new native owner");
    }
    let input = submission(&session, "after-baseline");
    submit(&local.peer, &input).await.unwrap();
    let answer = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(
        claude_answer(&answer),
        if fork {
            "reply 2: after-baseline"
        } else {
            "reply 1: after-baseline"
        }
    );
    assert!(
        claude_native_rows(&root, &before)
            .iter()
            .any(|row| row["type"] == "user"),
        "old native audit remains readable"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_native_fork_retires_settled_background_queries_without_rewriting_the_answer() {
    // Pinned ClaudeAdapterV2.forkThread closes a settled live query before
    // native copy. A completed answer remains completed; its background work ends.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::write(root.join("background-shell-paused"), []).unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let source = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &source).await;
    let input = submission(&source, "background-shell");
    submit(&local.peer, &input).await.unwrap();
    let first = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(first.status, TurnStatus::Completed);
    assert_eq!(claude_answer(&first), "あとで結果を報告します");
    assert!(
        events
            .projection
            .snapshot
            .projection
            .provider_threads
            .iter()
            .any(|thread| !thread.pending_background_tasks.is_empty()),
        "the source really has pending native background work"
    );
    let source_native = conversations::native_source(&root, &source, ProviderKind::Claude);
    let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let returned: SessionRef = local
        .peer
        .request(&Call::ForkConversation(
            agent_protocol::conversation::records::Fork {
                source: source.clone(),
                target: target.clone(),
                through: agent_protocol::conversation::records::ForkPoint::Run {
                    run_id: first.id.to_string(),
                },
            },
        ))
        .await
        .unwrap();
    assert_eq!(returned, target);
    let (_, mut child) = open(&local.peer, &target).await;
    let input = submission(&target, "after-background-fork");
    submit(&local.peer, &input).await.unwrap();
    let answer = child.input_turn(&input.client_user_message_id, true).await;
    let snapshot = &child.projection.snapshot.projection;
    assert!(
        snapshot.provider_threads.iter().any(|thread| thread
            .native_metadata
            .as_ref()
            .is_some_and(|meta| meta.fork_head_message_id.is_some())),
        "the actual SDK fork succeeds after query closure; portable replay cannot stand in for this boundary"
    );
    assert_eq!(claude_answer(&answer), "reply 2: after-background-fork");
    assert_ne!(
        conversations::native_source(&root, &target, ProviderKind::Claude),
        source_native
    );
    let (after, fresh) = open(&local.peer, &source).await;
    assert_eq!(
        after
            .thread
            .turns
            .iter()
            .flatten()
            .find(|turn| turn.id == first.id)
            .unwrap()
            .status,
        TurnStatus::Completed,
        "closing background work must preserve the settled answer"
    );
    assert!(
        fresh
            .projection
            .snapshot
            .projection
            .provider_threads
            .iter()
            .all(|thread| thread.pending_background_tasks.is_empty()),
        "the original query and its native background work are closed"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[rstest::rstest]
#[case::latest_fork("latest-fork")]
#[case::compact_rewind_fork("compact-rewind-fork")]
#[case::plain_retained_prompt("plain-retained-prompt")]
#[case::missing_previous_cursor("missing-previous-cursor")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_no_assistant_boundaries_preserve_retained_user_prompts(#[case] mode: &str) {
    // Pinned ClaudeAdapterV2 resolveForkUpToMessageId / resolveRollbackResumeSessionAt:
    // latest missing cursor uses current head/whole-copy; compaction alone may
    // fall back to its immediately preceding assistant, never past a user prompt.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let source = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &source).await;
    let first = submission(&source, "keep-this-user-prompt");
    submit(&local.peer, &first).await.unwrap();
    let first_turn = events.input_turn(&first.client_user_message_id, true).await;
    assert_eq!(claude_answer(&first_turn), "reply 1: keep-this-user-prompt");
    let first_cursor = events.projection.snapshot.projection.provider_turns[0]
        .native_turn_ref
        .as_ref()
        .unwrap()
        .native_id
        .clone()
        .unwrap();
    let mut inputs = 1;
    if mode == "missing-previous-cursor" {
        let user = submission(&source, "no-assistant");
        submit(&local.peer, &user).await.unwrap();
        let turn = events.input_turn(&user.client_user_message_id, true).await;
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(claude_answer(&turn).is_empty());
        inputs += 1;
    }
    let text = if mode == "plain-retained-prompt" {
        "no-assistant"
    } else {
        "/compact"
    };
    let boundary = submission(&source, text);
    submit(&local.peer, &boundary).await.unwrap();
    let boundary_turn = events
        .input_turn(&boundary.client_user_message_id, true)
        .await;
    assert_eq!(boundary_turn.status, TurnStatus::Completed);
    assert!(
        claude_answer(&boundary_turn).is_empty(),
        "the real SDK boundary emitted no assistant envelope"
    );
    inputs += 1;
    let projection = &events.projection.snapshot.projection;
    let attempt = projection
        .runs
        .iter()
        .find(|run| run.id == boundary_turn.id.as_str())
        .unwrap()
        .active_attempt_id
        .as_ref()
        .unwrap();
    let native_turn = projection
        .provider_turns
        .iter()
        .find(|turn| turn.run_attempt_id.as_ref() == Some(attempt))
        .unwrap();
    assert!(
        native_turn
            .native_turn_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_deref()
            .unwrap()
            .starts_with("turn:"),
        "no fabricated assistant cursor may stand in for this boundary"
    );
    let native = conversations::native_source(&root, &source, ProviderKind::Claude);
    if mode != "latest-fork" {
        let tail = submission(&source, "discard-tail");
        submit(&local.peer, &tail).await.unwrap();
        let removed = events.input_turn(&tail.client_user_message_id, true).await;
        inputs += 1;
        assert_eq!(
            claude_answer(&removed),
            format!("reply {inputs}: discard-tail")
        );
        let operation = uuid::Uuid::new_v4();
        let outcome = conversations::command(
            &local.peer,
            &source,
            operation,
            Command::Rewind(removed.id.clone()),
        )
        .await;
        let refused = matches!(mode, "plain-retained-prompt" | "missing-previous-cursor");
        if refused {
            if let Err(error) = outcome {
                assert!(error.to_string().contains("cursor"), "{error}");
            } else {
                loop {
                    assert!(
                        events
                            .view()
                            .thread
                            .turns
                            .iter()
                            .flatten()
                            .any(|turn| turn.id == removed.id),
                        "rewind cannot drop a retained user prompt with no assistant cursor"
                    );
                    if let Some(failure) = &events
                        .projection
                        .snapshot
                        .projection
                        .thread
                        .rollback_failure
                    {
                        assert_eq!(failure.request_id, operation.to_string());
                        assert!(failure.message.contains("cursor"), "{}", failure.message);
                        break;
                    }
                    events.next().await;
                }
            }
            assert!(
                events
                    .view()
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.id == boundary_turn.id)
            );
            assert_eq!(
                conversations::native_source(&root, &source, ProviderKind::Claude),
                native
            );
            let after = submission(&source, "after-rejection");
            submit(&local.peer, &after).await.unwrap();
            let answer = events.input_turn(&after.client_user_message_id, true).await;
            assert_eq!(
                claude_answer(&answer),
                format!("reply {}: after-rejection", inputs + 1),
                "native context also remains intact after refusing the cut"
            );
            local.close().await;
            host.close().await.unwrap();
            return;
        }
        outcome.expect("compaction can retain its immediately preceding real assistant");
        while events
            .view()
            .thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.id == removed.id)
        {
            assert!(
                events
                    .projection
                    .snapshot
                    .projection
                    .thread
                    .rollback_failure
                    .is_none(),
                "{:?}",
                events
                    .projection
                    .snapshot
                    .projection
                    .thread
                    .rollback_failure
            );
            events.next().await;
        }
        let projection = &events.projection.snapshot.projection;
        let thread = projection
            .provider_threads
            .iter()
            .find(|thread| Some(&thread.id) == projection.thread.active_provider_thread_id.as_ref())
            .unwrap();
        assert_eq!(
            thread
                .native_conversation_head_ref
                .as_ref()
                .unwrap()
                .native_id
                .as_deref(),
            Some(first_cursor.as_str())
        );
        assert!(
            events
                .view()
                .thread
                .turns
                .iter()
                .flatten()
                .any(|turn| turn.id == boundary_turn.id),
            "the retained compaction remains in the canonical history"
        );
    }
    let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let returned: SessionRef = local
        .peer
        .request(&Call::ForkConversation(
            agent_protocol::conversation::records::Fork {
                source: source.clone(),
                target: target.clone(),
                through: agent_protocol::conversation::records::ForkPoint::Run {
                    run_id: boundary_turn.id.to_string(),
                },
            },
        ))
        .await
        .unwrap();
    assert_eq!(returned, target);
    let (_, mut child) = open(&local.peer, &target).await;
    let after = submission(&target, "after-no-cursor-fork");
    submit(&local.peer, &after).await.unwrap();
    let answer = child.input_turn(&after.client_user_message_id, true).await;
    assert!(
        child
            .projection
            .snapshot
            .projection
            .provider_threads
            .iter()
            .any(|thread| thread
                .native_metadata
                .as_ref()
                .is_some_and(|meta| meta.fork_head_message_id.is_some())),
        "the SDK copied the actual native boundary, rather than replaying a different context"
    );
    let expected = if mode == "latest-fork" {
        "reply 3: after-no-cursor-fork"
    } else {
        "reply 2: after-no-cursor-fork"
    };
    assert_eq!(claude_answer(&answer), expected);
    let child_native = conversations::native_source(&root, &target, ProviderKind::Claude);
    let rows = claude_native_rows(&root, &child_native);
    assert!(rows.iter().any(|row| {
        row["message"]["content"]
            .to_string()
            .contains("keep-this-user-prompt")
    }));
    assert!(!rows.iter().any(|row| {
        row["message"]["content"]
            .to_string()
            .contains("discard-tail")
    }));
    assert_eq!(
        rows.iter().any(|row| row["type"] == "user"
            && row["message"]["content"].to_string().contains("/compact")),
        mode == "latest-fork",
        "a pinned fork omits only the compaction command"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[rstest::rstest]
#[case::same_host(false)]
#[case::restart_before_first_query(true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn imported_claude_history_reaches_a_fresh_native_baseline_without_reimport(
    #[case] restart: bool,
) {
    // T3 shouldPrepareLegacyImportHandoff keeps imported, run-less messages
    // available when no completed app run owns a native prefix. After a fresh
    // baseline the actual SDK input must contain that prefix, not the cut run.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let native = uuid::Uuid::new_v4().to_string();
    let user = uuid::Uuid::new_v4().to_string();
    let assistant = uuid::Uuid::new_v4().to_string();
    let key: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let project = root.join("claude-native/projects").join(key);
    std::fs::create_dir_all(&project).unwrap();
    let original = project.join(format!("{native}.jsonl"));
    let rows = [
        serde_json::json!({"type":"user","uuid":user,"parentUuid":null,"sessionId":native,"cwd":root,"version":"2.1.266","timestamp":"2026-11-16T00:00:00.000Z","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"imported-prefix-must-survive"}]}}),
        serde_json::json!({"type":"assistant","uuid":assistant,"parentUuid":user,"sessionId":native,"cwd":root,"version":"2.1.266","timestamp":"2026-11-16T00:00:01.000Z","isSidechain":false,"message":{"id":"msg_imported_answer","model":"default","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"provider-saved-answer"}]}}),
    ];
    std::fs::write(
        &original,
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let memory = Arc::new(Memory::default());
    let host = start(&root, memory.clone()).await;
    let local = host.local().await.unwrap();
    let imported: agent_protocol::project_import::ProjectImportResult = local
        .peer
        .request(&Call::ImportProjects(
            agent_protocol::project_import::ImportProjects {
                paths: vec![root.to_str().unwrap().into()],
            },
        ))
        .await
        .unwrap();
    assert!(imported.errors.is_empty(), "{:?}", imported.errors);
    let session = SessionRef::new(ProviderKind::Claude, native.clone()).unwrap();
    let (_, mut events) = open(&local.peer, &session).await;
    assert_eq!(
        events.projection.snapshot.projection.thread.history_origin,
        Some(agent_protocol::orchestration::ThreadHistoryOrigin::V1Import)
    );
    assert!(
        events.projection.snapshot.projection.runs.is_empty(),
        "provider history is not fabricated as app runs"
    );
    assert!(events.view().thread.turns.iter().flatten().flat_map(|turn|turn.items.iter().flatten()).any(|item|matches!(item.body(),ItemBody::AssistantText {text,..} if text=="provider-saved-answer")));
    let input = submission(&session, "discard-imported-continuation");
    submit(&local.peer, &input).await.unwrap();
    let discarded = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(discarded.status, TurnStatus::Completed, "{discarded:?}");
    assert_eq!(
        conversations::native_source(&root, &session, ProviderKind::Claude),
        native,
        "initial import continues the original native resource"
    );
    conversations::command(
        &local.peer,
        &session,
        uuid::Uuid::new_v4(),
        Command::Rewind(discarded.id.clone()),
    )
    .await
    .unwrap();
    while events
        .view()
        .thread
        .turns
        .iter()
        .flatten()
        .any(|turn| turn.id == discarded.id)
    {
        assert!(
            events
                .projection
                .snapshot
                .projection
                .thread
                .rollback_failure
                .is_none(),
            "{:?}",
            events
                .projection
                .snapshot
                .projection
                .thread
                .rollback_failure
        );
        events.next().await;
    }
    let replacement = conversations::native_source(&root, &session, ProviderKind::Claude);
    assert_ne!(
        replacement, native,
        "a baseline without a native fork head allocates a fresh physical resource"
    );
    if restart {
        local.close().await;
        host.close().await.unwrap();
        let host = start(&root, memory).await;
        let local = host.local().await.unwrap();
        let (_, events) = open(&local.peer, &session).await;
        verify_imported_reset_input(&root, &local.peer, &session, events, &replacement).await;
        local.close().await;
        host.close().await.unwrap();
    } else {
        verify_imported_reset_input(&root, &local.peer, &session, events, &replacement).await;
        local.close().await;
        host.close().await.unwrap();
    }
    assert!(
        std::fs::read_to_string(original)
            .unwrap()
            .contains("imported-prefix-must-survive"),
        "the provider's original transcript stays intact"
    );
}

async fn verify_imported_reset_input(
    root: &Path,
    client: &Client,
    session: &SessionRef,
    mut events: Conversation,
    native: &str,
) {
    let input = submission(session, "after-native-reset");
    submit(client, &input).await.unwrap();
    let answer = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(answer.status, TurnStatus::Completed);
    assert_eq!(
        conversations::native_source(root, session, ProviderKind::Claude),
        native
    );
    let rows = claude_native_rows(root, native);
    let texts = rows
        .iter()
        .filter(|row| row["type"] == "user")
        .flat_map(|row| row["message"]["content"].as_array().unwrap())
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        texts.contains("imported-prefix-must-survive"),
        "the actual SDK input to the fresh native resource must carry retained imported history"
    );
    assert!(texts.contains("provider-saved-answer"));
    assert!(texts.contains("after-native-reset"));
    assert!(
        !texts.contains("discard-imported-continuation"),
        "the discarded app run cannot leak back through context recovery"
    );
    assert_eq!(
        events.projection.snapshot.projection.thread.history_origin,
        Some(agent_protocol::orchestration::ThreadHistoryOrigin::V1Import)
    );
    assert_eq!(
        events
            .projection
            .snapshot
            .projection
            .runs
            .iter()
            .filter(|run| run.status != agent_protocol::orchestration::RunStatus::RolledBack)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_native_rewind_retires_settled_background_work_before_resuming_the_kept_head() {
    // Pinned ClaudeAdapterV2.rollbackThread rejects an active answer, then
    // closes the live query before rewinding. Settled background work alone
    // must not permanently lock a completed conversation out of rewind.
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::write(root.join("background-shell-paused"), []).unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let session = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    let input = submission(&session, "retained-answer");
    submit(&local.peer, &input).await.unwrap();
    let kept = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(kept.status, TurnStatus::Completed);
    let input = submission(&session, "background-shell");
    submit(&local.peer, &input).await.unwrap();
    let discarded = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(discarded.status, TurnStatus::Completed);
    assert!(
        events
            .projection
            .snapshot
            .projection
            .provider_threads
            .iter()
            .any(|thread| !thread.pending_background_tasks.is_empty())
    );
    let native = conversations::native_source(&root, &session, ProviderKind::Claude);
    conversations::command(
        &local.peer,
        &session,
        uuid::Uuid::new_v4(),
        Command::Rewind(discarded.id.clone()),
    )
    .await
    .unwrap();
    while events
        .view()
        .thread
        .turns
        .iter()
        .flatten()
        .any(|turn| turn.id == discarded.id)
    {
        assert!(
            events
                .projection
                .snapshot
                .projection
                .thread
                .rollback_failure
                .is_none(),
            "{:?}",
            events
                .projection
                .snapshot
                .projection
                .thread
                .rollback_failure
        );
        events.next().await;
    }
    let retained = events
        .view()
        .thread
        .turns
        .unwrap()
        .into_iter()
        .find(|turn| turn.id == kept.id)
        .unwrap();
    assert_eq!(retained.status, TurnStatus::Completed);
    assert_eq!(claude_answer(&retained), claude_answer(&kept));
    let input = submission(&session, "after-background-rewind");
    submit(&local.peer, &input).await.unwrap();
    let answer = events.input_turn(&input.client_user_message_id, true).await;
    assert_eq!(answer.status, TurnStatus::Completed, "{answer:?}");
    assert_eq!(claude_answer(&answer), "reply 2: after-background-rewind");
    assert_eq!(
        conversations::native_source(&root, &session, ProviderKind::Claude),
        native
    );
    assert!(
        events
            .projection
            .snapshot
            .projection
            .provider_threads
            .iter()
            .all(|thread| thread.pending_background_tasks.is_empty())
    );
    assert!(
        claude_native_rows(&root, &native)
            .iter()
            .any(|row| row["fixtureInput"]["content"]
                .to_string()
                .contains("background-shell")),
        "native rollback retains the discarded audit tail"
    );
    local.close().await;
    host.close().await.unwrap();
}
