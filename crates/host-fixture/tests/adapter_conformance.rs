//! The same observable conversation contract against both native subprocess fixtures.
use agent_protocol::{
    execution::{ErrorCategory, TurnStatus},
    ids::ClientInputId,
    items::ItemBody,
    models::{Empty, Turn, WorktreeSettings},
    operations as op,
    protocol::Call,
    requests::{Answer, QuestionAnswer, RequestBody},
    session::{OpenSession, OpenedSession, ProviderKind, SessionChange, SessionRef},
};
use agent_transport::{client::Client, framing::Reader};
use host_fixture::test_support::{HostFixture, Memory};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

async fn open(client: &Client, session: &SessionRef) -> (OpenedSession, Reader) {
    client
        .request_stream(&Call::OpenSession(OpenSession {
            include_activity: true,
            session: session.clone(),
            limit: 20,
        }))
        .await
        .unwrap()
}
async fn change(events: &mut Reader) -> SessionChange {
    tokio::time::timeout(Duration::from_secs(15), events.read())
        .await
        .expect("adapter event deadline")
        .unwrap()
        .expect("adapter stream closed")
}
async fn finished(events: &mut Reader) -> Turn {
    loop {
        if let SessionChange::Turn {
            turn,
            completed: true,
        } = change(events).await
        {
            return turn;
        }
    }
}
fn submission(session: &SessionRef, text: &str) -> op::Submission {
    op::Submission {
        thread_id: session.clone(),
        client_user_message_id: ClientInputId::from(format!("{}:{text}", session.id)),
        input: vec![op::Input::Text { text: text.into() }],
        model: None,
        effort: None,
        service_tier: None,
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
    client
        .call(&op::CreateSession {
            provider,
            cwd: Some(root.to_string_lossy().into_owned()),
            model: None,
        })
        .await
        .unwrap()
        .response
        .thread
        .id
        .unwrap()
}

async fn start(root: &Path, memory: Arc<Memory>) -> HostFixture {
    let claude = Path::new(env!("CARGO_BIN_EXE_bex-claude-fixture"));
    #[cfg(windows)]
    let executable = claude.with_extension("");
    #[cfg(windows)]
    let claude = executable.as_path();
    let program = host_fixture::fixture::Config {
        trace: true,
        stream_delay_ms: 5,
        ..Default::default()
    }
    .install(Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")), root)
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
    )
    .await
    .unwrap()
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
    assert!(local.peer.call(&first).await.unwrap().turn_id.is_some());
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
    assert!(history.response.thread.turns.iter().flatten().any(|saved| {
        saved
            .items
            .iter()
            .flatten()
            .any(|item| item.client_input_id.as_ref() == Some(&first.client_user_message_id))
    }));
    local.close().await;
    let local = host.local().await.unwrap();
    let (_, mut events) = open(&local.peer, &session).await;
    let receipt = local
        .peer
        .call(&submission(&session, "resumed input"))
        .await
        .unwrap();
    assert_ne!(receipt.turn_id, Some(turn.id));
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
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
    local.peer.call(&invoke).await.unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    if provider == ProviderKind::Claude {
        let received: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(format!("claude-session-{}.json", session.id))).unwrap(),
        )
        .unwrap();
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
    local.close().await;
    host.close().await.unwrap();
    if provider == ProviderKind::Codex {
        // Codex's fixture loads archived native state from this file;
        // Claude's fixture writes its native transcript during the first run.
        std::fs::write(root.join("list-fixture.json"), serde_json::json!([{
            "id":session.id,"cwd":root,"historyMode":"paginated","status":{"type":"notLoaded"},
            "turns":[{"id":"archived-turn","status":"completed","items":[
                {"id":"archived-user","type":"userMessage","content":[{"type":"text","text":"first input"}]},
                {"id":"archived-answer","type":"agentMessage","text":"saved reply","phase":"final_answer"}
            ]}]
        }]).to_string()).unwrap();
    }
    let host = start(&root, memory.clone()).await;
    let local = host.local().await.unwrap();
    local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    let (history, mut events) = open(&local.peer, &session).await;
    assert!(history.response.thread.turns.iter().flatten().any(|turn| {
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
    local
        .peer
        .call(&submission(&session, "after process restart"))
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    if provider == ProviderKind::Codex {
        let trace = std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap();
        let operations: Vec<_> = trace
            .lines()
            .skip(trace_offset)
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect();
        for method in ["thread/read", "thread/resume"] {
            assert_eq!(
                operations
                    .iter()
                    .filter(|event| event["method"] == method)
                    .count(),
                1,
                "submission must reuse its routing evidence and resume the archived session"
            );
        }
    }

    // Additional input is accepted once while the initial turn is still live.
    // Native adapters may steer or queue; the Host's routing matrix is tested separately.
    for interrupt in [false, true] {
        let session = create(&local.peer, provider, &root).await;
        let (_, mut events) = open(&local.peer, &session).await;
        let initial = submission(&session, prompt(provider, "wait"));
        let turn = local.peer.call(&initial).await.unwrap().turn_id.unwrap();
        loop {
            if matches!(
                change(&mut events).await,
                SessionChange::Turn {
                    completed: false,
                    ..
                }
            ) {
                break;
            }
        }
        if interrupt {
            local
                .peer
                .call(&op::Interrupt {
                    thread_id: session.clone(),
                    turn_id: turn,
                })
                .await
                .unwrap();
            assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
        } else {
            let additional = submission(&session, "queued follow-up");
            local.peer.call(&additional).await.unwrap();
            if provider == ProviderKind::Codex {
                std::fs::write(root.join("release-inputs"), "").unwrap();
            }
            // Native completion and a deferred input echo may arrive independently.
            // Both must be observed before checking the persisted conversation.
            let mut completed = None;
            let mut echoed = false;
            while completed.is_none() || !echoed {
                let items = match change(&mut events).await {
                    SessionChange::Turn {
                        turn,
                        completed: done,
                    } => {
                        if done {
                            assert_eq!(turn.status, TurnStatus::Completed);
                            completed = Some(turn.id);
                        }
                        turn.items.unwrap_or_default()
                    }
                    SessionChange::Item { item, .. } => vec![item],
                    _ => Vec::new(),
                };
                echoed |= items.iter().any(|item| {
                    item.client_input_id.as_ref() == Some(&additional.client_user_message_id)
                });
            }
            let turn_id = completed.unwrap();
            let (history, _) = open(&local.peer, &session).await;
            let turn = history
                .response
                .thread
                .turns
                .iter()
                .flatten()
                .find(|saved| saved.id == turn_id)
                .unwrap();
            assert_eq!(
                turn.items
                    .iter()
                    .flatten()
                    .filter(|item| item.client_input_id.as_ref()
                        == Some(&additional.client_user_message_id))
                    .count(),
                1
            );
        }
    }
    // Approval and questions use only the choices supplied by the adapter.
    for scenario in ["approval", "question"] {
        let session = create(&local.peer, provider, &root).await;
        let (_, mut events) = open(&local.peer, &session).await;
        local
            .peer
            .call(&submission(&session, prompt(provider, scenario)))
            .await
            .unwrap();
        let request = loop {
            if let SessionChange::Request { request } = change(&mut events).await {
                break request;
            }
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
        local
            .peer
            .request::<Empty>(&Call::AnswerSession(op::SessionAnswer {
                request_id: request.id.clone(),
                answer: answer.clone(),
            }))
            .await
            .unwrap();
        assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
        assert!(
            local
                .peer
                .request::<Empty>(&Call::AnswerSession(op::SessionAnswer {
                    request_id: request.id,
                    answer
                }))
                .await
                .is_err()
        );
    }
    // Abnormal provider exit terminates the owned execution with neutral evidence.
    let session = create(&local.peer, provider, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    let _ = local
        .peer
        .call(&submission(&session, prompt(provider, "crash")))
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
    local
        .peer
        .call(&submission(&healthy, "other provider is healthy"))
        .await
        .unwrap();
    assert_eq!(
        finished(&mut healthy_events).await.status,
        TurnStatus::Completed
    );
    // PC operations remain available after one provider fails.
    local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    local
        .peer
        .request::<WorktreeSettings>(&Call::ReadWorktreeSettings(Empty {}))
        .await
        .unwrap();
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_conforms_to_bex_conversations() {
    scenarios(ProviderKind::Codex).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_conforms_to_bex_conversations() {
    scenarios(ProviderKind::Claude).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_pages_preserve_healthy_listings_and_reject_repeated_native_cursors() {
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
    let session = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    local
        .peer
        .call(&submission(&session, "saved conversation"))
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let query = op::ListSessions::new(agent_protocol::models::ListQuery {
        chat_limit: 200,
        ..Default::default()
    });
    let listing = local.peer.call(&query).await.unwrap();
    assert_eq!(listing.data.len(), 102);
    assert!(listing.provider_errors.is_none());
    std::fs::write(root.join("repeat-list-cursor"), []).unwrap();
    let listing = local.peer.call(&query).await.unwrap();
    assert!(
        listing
            .data
            .iter()
            .any(|thread| thread.id.as_ref() == Some(&session))
    );
    assert_eq!(
        listing.data.len(),
        101,
        "keep the earlier healthy Codex page"
    );
    assert_eq!(
        listing.provider_errors.unwrap()["codex"]["code"],
        "invalid_session_list"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn title_lists_stop_after_visible_sections_and_merge_provider_pages_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let threads: Vec<_> = (0..2000)
        .map(|index| {
            serde_json::json!({
                "id":format!("page-{index:04}"),"cwd":root,
                "name":format!("Conversation {index:04}"),"updatedAt":index,
            })
        })
        .collect();
    let fixture = root.join("list-fixture.json");
    std::fs::write(&fixture, serde_json::to_vec(&threads).unwrap()).unwrap();
    let host = start(&root, Arc::new(Memory::default())).await;
    let local = host.local().await.unwrap();
    let session = create(&local.peer, ProviderKind::Claude, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    local
        .peer
        .call(&submission(&session, "saved conversation"))
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let page_reads = || {
        std::fs::read_to_string(root.join("rpc-trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|entry| entry["method"] == "thread/list")
            .count()
    };
    let listing = local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    assert_eq!(listing.data[0].id.as_ref(), Some(&session));
    assert_eq!(listing.data.len(), 5);
    assert_eq!(listing.data[1].id.as_ref().unwrap().id, "page-1999");
    assert_eq!(listing.data[4].id.as_ref().unwrap().id, "page-1996");
    assert!(listing.has_more_chats);
    assert!(listing.provider_errors.is_none());
    assert_eq!(page_reads(), 1, "initial list must not fetch all 20 pages");

    let expanded = local
        .peer
        .call(&op::ListSessions::new(agent_protocol::models::ListQuery {
            chat_limit: 150,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(expanded.data.len(), 150);
    assert_eq!(expanded.data[0].id.as_ref(), Some(&session));
    assert_eq!(expanded.data[149].id.as_ref().unwrap().id, "page-1851");
    assert!(expanded.has_more_chats);
    assert_eq!(
        page_reads(),
        3,
        "expansion needs only two additional page reads"
    );

    let found = local
        .peer
        .call(&op::ListSessions::new(agent_protocol::models::ListQuery {
            search_term: "Conversation 1900".into(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(found.data.len(), 1);
    assert_eq!(found.data[0].id.as_ref().unwrap().id, "page-1900");
    assert!(!found.has_more_chats);
    assert_eq!(page_reads(), 4);

    let tied: Vec<_> = (0..250).rev().map(|index| serde_json::json!({
        "id":format!("equal-{index:03}"),"cwd":root,"name":"Equal timestamps","updatedAt":100,
    })).collect();
    std::fs::write(&fixture, serde_json::to_vec(&tied).unwrap()).unwrap();
    let listing = local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    assert_eq!(listing.data[0].id.as_ref(), Some(&session));
    assert_eq!(
        listing
            .data
            .iter()
            .skip(1)
            .map(|thread| thread.id.as_ref().unwrap().id.as_str())
            .collect::<Vec<_>>(),
        ["equal-000", "equal-001", "equal-002", "equal-003"]
    );
    assert!(listing.has_more_chats);
    assert_eq!(
        page_reads(),
        7,
        "finish timestamp ties across native page boundaries"
    );

    let projects: Vec<_> = (0..3)
        .map(|index| {
            serde_json::json!({
                "id":format!("project-{index}"), "name":format!("Project {index}"),
                "roots":[{"path":root.join(format!("project-{index}"))}],
            })
        })
        .collect();
    std::fs::write(
        root.join("bex-projects.json"),
        serde_json::to_vec(&projects).unwrap(),
    )
    .unwrap();
    let scoped: Vec<_> = (0..2000).map(|index| serde_json::json!({
        "id":format!("scoped-{index:04}"), "name":"Scoped conversation", "updatedAt":index,
        "cwd":if index % 4 == 3 { root.clone() } else { root.join(format!("project-{}", index % 4)) },
    })).collect();
    std::fs::write(&fixture, serde_json::to_vec(&scoped).unwrap()).unwrap();
    let listing = local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    assert_eq!(
        listing
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<Vec<_>>(),
        ["project-2", "project-1", "project-0"]
    );
    assert_eq!(listing.data.len(), 20);
    assert_eq!(listing.more_project_ids.len(), 3);
    assert!(listing.has_more_chats);
    assert_eq!(
        page_reads(),
        8,
        "stop when all visible sections have their lookahead"
    );
    let expanded = local
        .peer
        .call(&op::ListSessions::new(agent_protocol::models::ListQuery {
            project_thread_limits: [("project-0".into(), 150)].into(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(expanded.data.len(), 165);
    assert_eq!(expanded.data[159].id.as_ref().unwrap().id, "scoped-1400");
    assert_eq!(expanded.data[160].id.as_ref(), Some(&session));
    assert_eq!(
        page_reads(),
        15,
        "expand only the requested project before stopping"
    );
    local.close().await;
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_queues_when_the_native_turn_is_not_observed() {
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
    .install(Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")), &root)
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
    )
    .await
    .unwrap();
    let local = host.local().await.unwrap();
    local
        .peer
        .call(&op::ListSessions::new(Default::default()))
        .await
        .unwrap();
    let session = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
    let input = submission(&session, "queued for unobserved turn");
    assert!(local.peer.call(&input).await.unwrap().turn_id.is_none());
    let (opened, _) = open(&local.peer, &session).await;
    assert!(opened.response.thread.turns.iter().flatten().any(|turn| {
        turn.items
            .iter()
            .flatten()
            .any(|item| item.client_input_id.as_ref() == Some(&input.client_user_message_id))
    }));
    let trace = std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap();
    assert!(trace.lines().any(|line| {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        event["method"] == "thread/queue/add" && event["hasExpectedTurnId"] == false
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
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(!models.data.is_empty());
    assert_eq!(
        models.provider_errors.unwrap()["claude"]["code"],
        "provider_unavailable"
    );
    let session = create(&local.peer, ProviderKind::Codex, &root).await;
    let (_, mut events) = open(&local.peer, &session).await;
    local
        .peer
        .call(&submission(&session, "healthy provider"))
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
        local
            .peer
            .call(&op::CreateSession {
                provider: ProviderKind::Claude,
                cwd: Some(root.to_string_lossy().into_owned()),
                model: None
            })
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
