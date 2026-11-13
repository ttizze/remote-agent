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
        .replica
        .rows()
        .values()
        .filter_map(|row| match &row.content {
            agent_protocol::sync::Content::Full(record)
            | agent_protocol::sync::Content::Summary(record) => match record.as_ref() {
                agent_protocol::conversation::records::Record::Project(project) => {
                    Some(project.clone())
                }
                _ => None,
            },
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
    assert!(
        opened
            .thread
            .turns
            .as_ref()
            .unwrap()
            .iter()
            .any(|turn| turn.id.as_str() == "original-turn")
    );
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
    use host_daemon::{HostRpcService, ProjectStore};
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
    let service = HostRpcService::new(
        [codex.into(), claude.into()],
        ProjectStore::new(root.join("worktrees.json")),
    );
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
    assert_eq!(
        receipt.outcome,
        agent_protocol::conversation::Outcome::Accepted
    );
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
        matches!(item.body(), ItemBody::UserMessage { content, .. } if content.iter().any(|part| {
            matches!(part, agent_protocol::items::MessagePart::Text {text} if text == "first input")
        }))
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
    assert_eq!(
        events
            .input_turn(&after_restart.client_user_message_id, true)
            .await
            .status,
        TurnStatus::Completed
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
                "submission uses the saved conversation and resumes the same native source"
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
        if case == "steer" {
            assert_eq!(
                receipt.outcome,
                agent_protocol::conversation::Outcome::Accepted
            );
        } else {
            assert_eq!(
                receipt.outcome,
                agent_protocol::conversation::Outcome::Accepted
            );
            let (opened, _) = open(&local.peer, &session).await;
            assert_eq!(opened.thread.queue.inputs, std::slice::from_ref(&follow_up));
        }
        if case == "stop" {
            conversations::command(&local.peer, &session, Command::Interrupt { turn: None })
                .await
                .unwrap();
            assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
            let (opened, _) = open(&local.peer, &session).await;
            assert!(opened.thread.queue.held);
            assert_eq!(opened.thread.queue.inputs, std::slice::from_ref(&follow_up));
            conversations::command(&local.peer, &session, Command::ResumeQueue)
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
async fn search_preserves_cached_catalog_rows_when_native_pagination_fails() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let threads: Vec<_> = (0..101)
        .map(|index| serde_json::json!({
            "id":format!("page-{index}"),"cwd":root,"name":format!("Conversation {index}"),"updatedAt":index,
        }))
        .collect();
    std::fs::write(
        root.join("list-fixture.json"),
        serde_json::to_vec(&threads).unwrap(),
    )
    .unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let chats = root.join("bex-chats");
    std::fs::create_dir(&chats).unwrap();
    let session = create(&local.peer, ProviderKind::Claude, &chats).await;
    let (_, mut events) = open(&local.peer, &session).await;
    submit(&local.peer, &submission(&session, "saved conversation"))
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let query = agent_protocol::conversation::records::SearchList {
        term: String::new(),
        limit: Some(200),
    };
    let listing = local.peer.call(&query).await.unwrap();
    assert_eq!(listing.data.len(), 102);
    assert!(listing.provider_errors.is_none());
    let known = listing
        .data
        .iter()
        .filter_map(|thread| thread.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    std::fs::write(root.join("repeat-list-cursor"), []).unwrap();
    let listing = local.peer.call(&query).await.unwrap();
    assert!(
        listing
            .data
            .iter()
            .any(|thread| thread.id.as_ref() == Some(&session))
    );
    assert_eq!(
        listing
            .data
            .iter()
            .filter_map(|thread| thread.id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        known,
        "a partial native failure preserves the already synchronized catalog"
    );
    assert_eq!(
        listing.provider_errors.unwrap()["codex"]["code"],
        "invalid_session_list"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_lists_read_each_native_page_once_and_refresh_activity() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let repository = root.join("repository");
    std::fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "--quiet",
        "-m",
        "fixture",
    ]);
    let paths = [
        root.join("first"),
        root.join("second"),
        root.join("deleted"),
    ];
    let mut mappings = serde_json::Map::new();
    for (index, path) in paths.iter().enumerate() {
        git(&[
            "worktree",
            "add",
            "-b",
            &format!("task-{index}"),
            path.to_str().unwrap(),
        ]);
        mappings.insert(
            path.to_string_lossy().into_owned(),
            serde_json::json!(repository),
        );
    }
    git(&["worktree", "remove", paths[2].to_str().unwrap()]);
    std::fs::write(
        root.join("bex-worktrees.json"),
        serde_json::json!({"workspaceRoots":mappings}).to_string(),
    )
    .unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let reads = || {
        std::fs::read_to_string(root.join("rpc-trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|entry| {
                entry["method"] == "thread/list"
                    && entry["sourceKinds"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "exec"))
            })
            .count()
    };
    for (subagent, running) in [(false, true), (false, false), (true, true), (true, false)] {
        let threads: Vec<_> = (0..101)
            .map(|index| {
                serde_json::json!({
                    "id":format!("page-{index}"),
                    "parentThreadId":(index == 0 && subagent).then_some("parent-outside-the-checkout"),
                    "cwd":if index == 0 { &paths[0] } else { &paths[1] },
                    "name":format!("Conversation {index}: {running}"),
                    "updatedAt":index,
                    "status":{"type":if index == 0 && running { "active" } else { "idle" }},
                    "loaded":index == 0,
                })
            })
            .collect();
        std::fs::write(
            root.join("list-fixture.json"),
            serde_json::to_vec(&threads).unwrap(),
        )
        .unwrap();
        let before = reads();
        let entries: Vec<agent_protocol::models::Worktree> = local
            .peer
            .request(&Call::ListWorktrees(Empty {}))
            .await
            .unwrap();
        assert_eq!(
            reads() - before,
            2,
            "worktree count must not multiply native history reads"
        );
        assert_eq!(entries.len(), 3);
        let first = entries
            .iter()
            .find(|entry| Path::new(&entry.path) == paths[0])
            .unwrap();
        assert_eq!(first.threads.len(), 1);
        assert_eq!(first.threads[0].active, running);
        assert_eq!(first.blocked_reason.is_some(), running);
        assert_eq!(first.threads[0].name, format!("Conversation 0: {running}"));
        let second = entries
            .iter()
            .find(|entry| Path::new(&entry.path) == paths[1])
            .unwrap();
        assert_eq!(second.threads.len(), 100);
        assert!(second.threads.iter().all(|thread| !thread.active));
        assert!(second.blocked_reason.is_none());
        let deleted = entries
            .iter()
            .find(|entry| Path::new(&entry.path) == paths[2])
            .unwrap();
        assert_eq!(deleted.branch, "削除済み");
        assert!(deleted.blocked_reason.is_none() && deleted.threads.is_empty());
        let counted = running && !subagent;
        let mut catalog = conversations::Catalog::open(&local.peer).await;
        let activity = loop {
            let activity = catalog.activity();
            let fresh=catalog.replica.rows().values().any(|row|match &row.content {
                agent_protocol::sync::Content::Full(record)|agent_protocol::sync::Content::Summary(record)=>matches!(record.as_ref(),agent_protocol::conversation::records::Record::Thread(header) if header.state.name==format!("Conversation 0: {running}") && header.details.parent.is_some()==subagent && (header.details.activity.status==agent_protocol::execution::SessionStatus::Running)==running)
            });
            if fresh && activity.display.current.total == u32::from(counted) {
                break activity;
            }
            catalog.next().await;
        };
        assert_eq!(activity.display.current.total, u32::from(counted));
        assert_eq!(activity.display.ongoing, counted);
        assert_eq!(
            activity.display.current.label,
            if counted {
                "実行中 1件"
            } else {
                "すべてのタスクが終了"
            },
            "task activity counts user conversations from every native page, excluding subagents"
        );
    }
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_lists_skip_descendant_reads_and_concurrent_fleets_share_native_admission() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let mut threads: Vec<_> = (0..200).map(|index| serde_json::json!({
            "id":format!("root-{index:03}"), "name":format!("Conversation {index:03}"),
            "updatedAt":index,
        })).collect();
        threads.extend([
            serde_json::json!({"id":"child", "parentThreadId":"root-199", "name":"Child", "updatedAt":-1}),
            serde_json::json!({"id":"grandchild", "parentThreadId":"child", "name":"Grandchild", "updatedAt":-2}),
            serde_json::json!({"id":"hidden-child", "parentThreadId":"root-000", "updatedAt":-3}),
        ]);
        std::fs::write(root.join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        let host = start(&root, Arc::new(Memory::default())).await;
        let first = host.local().await.unwrap();
        let second = host.local().await.unwrap();
        let descendant_reads = || {
            std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap_or_default()
                .lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|entry| entry["method"] == "thread/list" && entry["ancestorThreadId"].is_string()).count()
        };
        let (mut first_catalog,mut second_catalog)=tokio::join!(conversations::Catalog::open(&first.peer),conversations::Catalog::open(&second.peer));
        tokio::join!(first_catalog.wait_rows(203),second_catalog.wait_rows(203));
        for catalog in [&first_catalog,&second_catalog] {
            let page=catalog.page(agent_protocol::models::ListQuery {limit:150,..Default::default()});
            let page=page.lists.threads.as_ref().unwrap();
            assert!(page.provider_errors.is_none());
            assert!(page.has_more);
            assert_eq!(page.data.len(),150);
            assert!(page.data.iter().all(|thread|thread.parent_id.is_none()));
        }
        let complete=first_catalog.page(agent_protocol::models::ListQuery {limit:250,..Default::default()});
        let complete=complete.lists.threads.as_ref().unwrap();
        assert!(!complete.has_more);
        assert_eq!(complete.data.len(),200);
        let searched=first.peer.call(&agent_protocol::conversation::records::SearchList {term:"Conversation 199".into(),limit: Some(100)}).await.unwrap();
        assert_eq!(searched.data.len(),1);
        assert_eq!(descendant_reads(),0,"catalogs and search must not issue descendant reads");
        let gate = root.join("hold-list-reads");
        std::fs::write(&gate, []).unwrap();
        let agents = op::ListAgents {
            thread_id: agent_protocol::session::SessionRef { provider: Some(ProviderKind::Codex), id: "root-199".into() },
        };
        let [a, b, c, d, e, f, g, h] = [
            &first.peer, &first.peer, &first.peer, &first.peer,
            &second.peer, &second.peer, &second.peer, &second.peer,
        ].map(|peer| peer.call(&agents));
        let listing = async { tokio::join!(a, b, c, d, e, f, g, h) };
        let release = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while descendant_reads() < 4 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let pending = descendant_reads();
            std::fs::remove_file(&gate).unwrap();
            assert_eq!(pending, 4, "concurrent clients must share native list admission");
        };
        let ((a, b, c, d, e, f, g, h), ()) = tokio::join!(listing, release);
        for response in [a, b, c, d, e, f, g, h] {
            let agents = response.unwrap();
            assert_eq!(agents.iter().map(|agent| agent.id.id.as_str()).collect::<Vec<_>>(), ["child", "grandchild"]);
        }
        assert_eq!(descendant_reads(), 8);
        first.close().await;
        second.close().await;
        host.close().await.unwrap();
    }).await.expect("concurrent fleet list deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_pages_and_project_disclosure_use_the_same_rows_without_native_reads() {
    use agent_core::state::{Event, Intent, reduce};
    use agent_protocol::models::ListQuery;
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let projects=(0..9).map(|index| {
        let path=root.join(format!("project-{index}"));
        std::fs::create_dir(&path).unwrap();
        serde_json::json!({"id":format!("project-{index}"),"name":format!("Project {index}"),"roots":[{"path":path}]})
    }).collect::<Vec<_>>();
    // Only pre-upgrade fixtures write the old registry; normal registration is
    // already tested through AddProject and the durable project-change job.
    std::fs::write(
        root.join("bex-projects.json"),
        serde_json::to_vec(&projects).unwrap(),
    )
    .unwrap();
    let threads=(0..2000).map(|index|serde_json::json!({
        "id":format!("scoped-{index:04}"),"name":format!("Scoped conversation {index}"),"updatedAt":index,
        "cwd":if index%4==3 {root.clone()} else {root.join(format!("project-{}",index%4))}
    })).collect::<Vec<_>>();
    std::fs::write(
        root.join("list-fixture.json"),
        serde_json::to_vec(&threads).unwrap(),
    )
    .unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let mut catalog = conversations::Catalog::open(&local.peer).await;
    catalog.wait_rows(2000).await;
    let initial = catalog.page(ListQuery::default());
    let listing = initial.lists.threads.as_ref().unwrap();
    assert_eq!(
        listing
            .projects
            .iter()
            .take(3)
            .map(|project| project.id.as_str())
            .collect::<Vec<_>>(),
        ["project-2", "project-1", "project-0"]
    );
    assert_eq!(listing.projects.len(), 5);
    assert!(listing.has_more_projects);
    let displayed = initial.thread_list().unwrap();
    assert_eq!(
        displayed.threads.len()
            + displayed
                .projects
                .iter()
                .map(|project| project.threads.len())
                .sum::<usize>(),
        20
    );
    assert_eq!(
        listing
            .data
            .iter()
            .filter(|thread| thread.project_id.is_none())
            .count(),
        5
    );
    assert_eq!(
        initial.lists.project_threads["project-0"].data[0]
            .id
            .as_ref()
            .unwrap()
            .id,
        "scoped-1996"
    );
    let reads = || {
        std::fs::read_to_string(root.join("rpc-trace.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|entry| entry["method"] == "thread/list" || entry["method"] == "thread/read")
            .collect::<Vec<_>>()
    };
    let before = reads();
    assert!(
        before.iter().all(|entry| entry["method"] == "thread/list"),
        "catalog discovery must not import bodies"
    );
    let (expanded, _) = reduce(
        &initial,
        "",
        Event::Intent(Intent::ListProjectSessions(op::ListProjectSessions {
            project_id: "project-0".into(),
            limit: 150,
            search_term: String::new(),
        })),
    );
    let project = &expanded.lists.project_threads["project-0"];
    assert_eq!(project.data.len(), 150);
    assert_eq!(project.data[149].id.as_ref().unwrap().id, "scoped-1400");
    assert!(project.has_more);
    let (shown, _) = reduce(&expanded, "", Event::Intent(Intent::ExpandProjects));
    assert_eq!(shown.lists.threads.as_ref().unwrap().projects.len(), 9);
    assert!(!shown.lists.threads.as_ref().unwrap().has_more_projects);
    assert!(
        shown
            .lists
            .project_threads
            .get("project-8")
            .is_none_or(|page| page.data.is_empty())
    );
    let (collapsed, _) = reduce(
        &shown,
        "",
        Event::Intent(Intent::SetProjectExpanded {
            project_id: "project-0".into(),
            expanded: false,
        }),
    );
    assert_eq!(collapsed.lists.replica, initial.lists.replica);
    assert_eq!(
        reads(),
        before,
        "disclosure and pagination leave native discovery untouched"
    );
    let found = local
        .peer
        .call(&agent_protocol::conversation::records::SearchList {
            term: "Scoped conversation 1900".into(),
            limit: Some(100),
        })
        .await
        .unwrap();
    assert_eq!(found.data.len(), 1);
    assert_eq!(found.data[0].id.as_ref().unwrap().id, "scoped-1900");
    assert!(
        catalog.replica.rows().len() >= 2009,
        "search does not replace the replicated catalog"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_queues_input_for_a_running_turn_it_has_not_observed() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::write(
        root.join("list-fixture.json"),
        serde_json::to_vec(&serde_json::json!([{
            "id":"external","cwd":root,"historyMode":"paginated","status":{"type":"active","activeFlags":[]},
            "turns":[{"id":"native-turn","status":"inProgress","items":[]}]
        }]))
        .unwrap(),
    )
    .unwrap();
    let program = host_fixture::fixture::Config {
        trace: true,
        stream_delay_ms: 5,
        ..Default::default()
    }
    .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), &root)
    .unwrap();
    let host = HostFixture::start(
        &root,
        codex_app_server::AppServerConfig {
            program,
            ..Default::default()
        },
        Arc::new(Memory::default()),
        "queue conformance",
        false,
        None,
        None,
    )
    .await
    .unwrap();
    let local = host.local().await.unwrap();
    let _catalog = conversations::Catalog::open(&local.peer).await;
    let session = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
    // Even a steer waits for a running turn the Host has not observed.
    let input = op::Submission {
        follow_up: op::FollowUp::Steer,
        ..submission(&session, "queued for unobserved turn")
    };
    assert_eq!(
        submit(&local.peer, &input).await.unwrap().outcome,
        agent_protocol::conversation::Outcome::Accepted
    );
    let (opened, _) = open(&local.peer, &session).await;
    assert_eq!(opened.thread.queue.inputs, [input]);
    let trace = std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap();
    assert!(!trace.lines().any(|line| {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        event["method"] == "turn/steer" || event["method"] == "turn/start"
    }));
    local.close().await;
    host.close().await.unwrap();
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
