#[tokio::test]
async fn search_defaults_to_the_t3_bound_and_accepts_an_explicit_limit() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
    use agent_protocol::{
        models::{Thread, ThreadResponse},
        session::SessionRef,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    for index in 0..55 {
        let id = format!("conversation-{index}");
        let public = SessionRef::new(ProviderKind::Codex, id.clone()).unwrap();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(public.clone()),
                cwd: Some(directory.path().to_string_lossy().into()),
                preview: Some("matches".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        own(&service, &public, &response).await;
        agent.records.lock().unwrap().insert(id, response);
    }
    let all = service
        .search_list(&agent_protocol::conversation::records::SearchList {
            term: "matches".into(),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(all.data.len(), 50);
    assert!(all.has_more);
    let first = service
        .search_list(&agent_protocol::conversation::records::SearchList {
            term: "matches".into(),
            limit: Some(3),
        })
        .await
        .unwrap();
    assert_eq!(first.data.len(), 3);
    assert!(first.has_more);
}

#[tokio::test]
async fn sdk_reviews_stay_out_of_titles_and_search_after_host_restart() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::own;
    use agent_protocol::session::SessionRef;
    let root = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(root.path()).unwrap();
    let native = root.path().join("native");
    let transcripts = native.join("projects/shared");
    std::fs::create_dir_all(&transcripts).unwrap();
    let session = |id| SessionRef {
        provider: Some(ProviderKind::Claude),
        id: uuid::Uuid::from_u128(id).to_string(),
    };
    let public = session(10);
    let active = session(1);
    for (id, entrypoint, source, title) in [
        (1, "sdk-ts", "sdk", "NSH user conversation"),
        (2, "sdk-ts", "sdk", "NSH message text sizing"),
        (3, "sdk-py", "sdk", "NSH automated analysis"),
        (4, "sdk-cli", "sdk", "NSH CLI review"),
        (5, "cli", "cli", "NSH terminal conversation"),
        (6, "t3-code", "sdk", "NSH custom entrypoint review"),
    ] {
        let id = session(id).id;
        let rows = [
            serde_json::json!({"type":"user","uuid":uuid::Uuid::new_v4(),"parentUuid":null,"sessionId":id,"cwd":cwd,"entrypoint":entrypoint,"promptSource":source,"message":{"content":title}}),
            serde_json::json!({"type":"ai-title","sessionId":id,"aiTitle":title}),
        ].map(|row| row.to_string() + "\n").concat();
        std::fs::write(transcripts.join(format!("{id}.jsonl")), rows).unwrap();
    }
    let mut side_chat = None;
    for restarting in [false, true] {
        let agent = crate::providers::Claude::load(
            root.path().join("missing-cli"),
            root.path().join("state"),
            Some(native.clone()),
        )
        .await
        .unwrap();
        let projects = ProjectStore::fixture(root.path().join("bex-worktrees.json"));
        projects.register(&cwd).await.unwrap();
        let service = HostRpcService::new([agent.into()], projects.path());
        if !restarting {
            let agent = service
                .inner
                .providers
                .discovery(ProviderKind::Claude)
                .unwrap();
            let response = agent.open(&active.id, 10, true).await.unwrap();
            own(&service, &public, &response).await;
            let created = service
                .inner
                .providers
                .get(ProviderKind::Claude)
                .unwrap()
                .create(&cwd.to_string_lossy(), None, None)
                .await
                .unwrap();
            let id = created.thread.id.unwrap();
            // A completed SDK turn must stay visible even without a loaded worker.
            let rows = [
                serde_json::json!({"type":"user","uuid":"question","sessionId":id.id,"cwd":cwd,"entrypoint":"sdk-ts","message":{"content":"NSH side chat"}}),
                serde_json::json!({"type":"assistant","uuid":"answer","parentUuid":"question","sessionId":id.id,"entrypoint":"sdk-ts","message":{"id":"reply","content":[{"type":"text","text":"Done"}]}}),
            ].map(|row| row.to_string() + "\n").concat();
            std::fs::write(transcripts.join(format!("{}.jsonl", id.id)), rows).unwrap();
            let response = agent.open(&id.id, 10, true).await.unwrap();
            own(&service, &id, &response).await;
            side_chat = Some(id);
        }
        // Project import owns native discovery. Ordinary search reads only the
        // resulting canonical conversations, also after the service reopens.
        crate::host_rpc::runtime::Runtime::import_project_threads(
            &service,
            &[cwd.to_string_lossy().into_owned()],
        )
        .await
        .unwrap();
        assert_eq!(
            service
                .inner
                .conversation
                .storage
                .shell_snapshot()
                .unwrap()
                .threads
                .len(),
            3,
            "the explicit import owns native discovery and excludes SDK reviews"
        );
        for search in [
            "",
            "NSH",
            "NSH message text sizing",
            "NSH custom entrypoint review",
        ] {
            let page = service
                .search_list(&agent_protocol::conversation::records::SearchList {
                    term: search.into(),
                    limit: Some(10),
                })
                .await
                .unwrap();
            let ids: std::collections::HashSet<_> = page
                .data
                .into_iter()
                .map(|thread| thread.id.unwrap())
                .collect();
            let expected = if matches!(
                search,
                "NSH message text sizing" | "NSH custom entrypoint review"
            ) {
                std::collections::HashSet::new()
            } else {
                [public.clone(), session(5), side_chat.clone().unwrap()]
                    .into_iter()
                    .collect()
            };
            assert_eq!(ids, expected, "search={search}, restarting={restarting}");
        }
    }
}

#[tokio::test]
async fn prefetched_project_titles_continue_past_the_first_native_page() {
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(root.path()).unwrap();
    let native = root.path().join("native");
    let transcripts = native.join("projects/shared");
    std::fs::create_dir_all(&transcripts).unwrap();
    let mut expected = std::collections::HashSet::new();
    for _ in 0..102 {
        let id = uuid::Uuid::new_v4();
        expected.insert(id.to_string());
        let user = serde_json::json!({"type":"user","uuid":uuid::Uuid::new_v4(),"parentUuid":null,"sessionId":id,"cwd":cwd,"message":{"content":"hello"}});
        std::fs::write(transcripts.join(format!("{id}.jsonl")), format!("{user}\n")).unwrap();
    }
    let agent = crate::providers::Claude::load(
        root.path().join("missing-cli"),
        root.path().join("state"),
        Some(native),
    )
    .await
    .unwrap();
    let projects = ProjectStore::fixture(root.path().join("bex-worktrees.json"));
    projects.register(&cwd).await.unwrap();
    let service = HostRpcService::new([agent.into()], projects.path());
    crate::host_rpc::runtime::Runtime::import_project_threads(
        &service,
        &[cwd.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .shell_snapshot()
            .unwrap()
            .threads
            .len(),
        expected.len(),
        "explicit import must commit the complete native page sequence before search"
    );
    let page = service
        .search_list(&agent_protocol::conversation::records::SearchList {
            term: String::new(),
            limit: Some(101),
        })
        .await
        .unwrap();
    assert_eq!(page.data.len(), 101, "{:?}", page.provider_errors);
    assert!(page.has_more);
    let ids: std::collections::HashSet<_> = page
        .data
        .into_iter()
        .map(|thread| thread.id.unwrap().id)
        .collect();
    assert_eq!(ids.len(), 101);
    assert!(ids.is_subset(&expected));
}

#[tokio::test]
async fn list_decorations_use_the_listed_roots_without_reloading_project_catalogs() {
    use super::*;
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::write(
        root.join("favicon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><path fill="red" d="M0 0h8v8H0z"/></svg>"#,
    )
    .unwrap();
    let state = root.join("bex-worktrees.json");
    std::fs::write(state.with_file_name(crate::CONVERSATION_DATABASE_FILE), "{").unwrap();
    let service = HostRpcService::new(
        [crate::providers::codex::Codex::new(Err("provider is offline".into())).into()],
        state,
    );
    let params = op::ReadListDecorations {
        scope: op::ListDecorationScope::Root,
        projects: vec![(
            "listed-project".into(),
            vec![root.to_string_lossy().into_owned()],
        )],
        threads: Vec::new(),
    };
    let result = service.list_decorations(&params).await.unwrap();
    assert!(result.icons["listed-project"].is_some());
    assert!(result.statuses.is_empty());
}

#[tokio::test]
async fn native_project_metadata_does_not_change_registered_membership() {
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let workspace = dunce::canonicalize(&workspace).unwrap();
    let projects = ProjectStore::fixture(root.path().join("bex-worktrees.json"));
    projects.register(&workspace).await.unwrap();
    let registered = projects.load().await.unwrap().projects[0].id.clone();
    let native = root.path().join("native");
    let transcripts = native.join("projects/shared");
    std::fs::create_dir_all(&transcripts).unwrap();
    let mut expected = Vec::new();
    for cwd in [&workspace, &root.path().join("outside")] {
        let id = uuid::Uuid::new_v4();
        let metadata = serde_json::json!({"type":"attachment","attachment":{
            "type":"prompt_snapshot","systemPrompt":[
                "## Where you are\n\nProject: \"Native\" (id: `chan_native`)\n"
            ]
        }});
        let user = serde_json::json!({"type":"user","uuid":uuid::Uuid::new_v4(),"parentUuid":null,"sessionId":id,"cwd":cwd,"message":{"content":"hello"}});
        std::fs::write(
            transcripts.join(format!("{id}.jsonl")),
            format!("{metadata}\n{user}\n"),
        )
        .unwrap();
        expected.push(id.to_string());
    }
    std::fs::write(native.join(".claude.json"), "{").unwrap();
    let claude = crate::providers::Claude::load(
        root.path().join("missing-cli"),
        root.path().join("state"),
        Some(native.clone()),
    )
    .await
    .unwrap();
    let service = HostRpcService::new([claude.into()], projects.path());
    let session = service.open_session();
    // Both conversations are explicitly opened, including the projectless
    // one. Their saved membership must not follow native project metadata.
    for id in &expected {
        let _: agent_protocol::conversation::records::Opened =
            crate::host_rpc::runtime::native_fixture::call(
                &service,
                session.id(),
                Call::SubscribeConversation(agent_protocol::conversation::records::Subscribe {
                    session: agent_protocol::session::SessionRef::new(
                        ProviderKind::Claude,
                        id.clone(),
                    )
                    .unwrap(),
                    subscription: uuid::Uuid::new_v4(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
    }
    for repaired in [false, true] {
        if repaired {
            std::fs::write(native.join(".claude.json"), "{}").unwrap();
        }
        let response = service
            .dispatch(
                session.id(),
                &Call::SearchList(agent_protocol::conversation::records::SearchList {
                    term: String::new(),
                    limit: Some(10_000),
                }),
            )
            .await
            .unwrap();
        let Response::Success { result } = agent_protocol::protocol::decode::<
            Response<agent_protocol::models::ThreadList>,
        >(&response.initial)
        .unwrap() else {
            panic!("project metadata failure must not fail session listing")
        };
        assert!(
            result
                .projects
                .iter()
                .any(|project| project.id == registered)
        );
        assert_eq!(result.data.len(), 2);
        for id in &expected {
            assert!(
                result
                    .data
                    .iter()
                    .any(|thread| thread.id.as_ref().is_some_and(|thread| &thread.id == id))
            );
        }
        assert_eq!(result.projects.len(), 1);
        assert!(
            result
                .data
                .iter()
                .any(|thread| thread.project_id.as_deref() == Some(registered.as_str()))
        );
        assert!(result.data.iter().any(|thread| thread.project_id.is_none()));
    }
}

#[tokio::test]
async fn registered_roots_determine_membership_for_both_providers() {
    use super::*;
    use agent_protocol::models::ProjectMembership;
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    let store = ProjectStore::fixture(root.join("bex-worktrees.json"));
    store.register(&project).await.unwrap();
    let projects = store.load().await.unwrap();
    let registered = projects.projects[0].id.as_str();
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        for (cwd, membership, expected) in [
            (
                project.clone(),
                ProjectMembership::Unassigned {},
                Some(registered),
            ),
            (
                project.clone(),
                ProjectMembership::Unknown {},
                Some(registered),
            ),
            (
                project.clone(),
                ProjectMembership::Assigned("native-other".into()),
                Some(registered),
            ),
            (
                root.join("outside"),
                ProjectMembership::Assigned("native-other".into()),
                None,
            ),
        ] {
            let mut thread = Thread {
                id: Some(agent_protocol::session::SessionRef {
                    provider: Some(provider),
                    id: "thread".into(),
                }),
                cwd: Some(cwd.to_str().unwrap().into()),
                project_id: membership,
                ..Default::default()
            };
            describe_thread(&mut thread, Default::default(), &projects);
            assert_eq!(thread.project_id.as_deref(), expected);
        }
    }
}

#[tokio::test]
async fn task_list_refreshes_branding_for_all_project_headers() {
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("remote-agent");
    let workspaces = std::iter::once(workspace.clone())
        .chain((0..5).map(|index| root.path().join(format!("empty-project-{index}"))))
        .collect::<Vec<_>>();
    let native = root.path().join("native");
    for workspace in &workspaces {
        std::fs::create_dir(workspace).unwrap();
        std::fs::write(workspace.join("favicon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="#ff0000"/></svg>"##).unwrap();
    }
    let transcript = native.join("projects/p");
    std::fs::create_dir_all(&transcript).unwrap();
    let id = "12345678-1234-4234-8234-123456789abc";
    std::fs::write(transcript.join(format!("{id}.jsonl")),serde_json::json!({"type":"user","uuid":"native-question","parentUuid":null,"sessionId":id,"cwd":dunce::canonicalize(&workspace).unwrap(),"message":{"content":"Branding task"}}).to_string()+"\n").unwrap();
    let projects = ProjectStore::fixture(root.path().join("bex-worktrees.json"));
    for workspace in &workspaces {
        projects.register(workspace).await.unwrap();
    }
    let claude = crate::providers::Claude::load(
        root.path().join("does-not-exist"),
        root.path().join("state"),
        Some(native),
    )
    .await
    .unwrap();
    let service = HostRpcService::new(
        [
            Backend::unavailable(ProviderKind::Codex, "unavailable"),
            claude.into(),
        ],
        projects.path(),
    );
    crate::host_rpc::runtime::Runtime::import_project_threads(
        &service,
        &[workspace.to_str().unwrap().into()],
    )
    .await
    .unwrap();
    use agent_core::state::operations::Operation;
    use agent_protocol::conversation::records;
    let session = service.open_session();
    let subscription = uuid::Uuid::new_v4();
    let reply = service
        .dispatch(
            session.id(),
            &Call::SubscribeList(records::SubscribeList {
                refresh: false,
                subscription,
                after_sequence: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: opened } =
        agent_protocol::protocol::decode::<Response<records::OpenedList>>(&reply.initial).unwrap()
    else {
        panic!("list subscription failed")
    };
    let mut shell = None;
    for item in opened.initial {
        shell = agent_core::orchestration::shell::receive(shell.as_ref(), item, true).unwrap();
    }
    let mut shell = shell.unwrap();
    let mut updates = reply.updates.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while shell.threads.is_empty() {
            let message = agent_protocol::protocol::decode(&updates.recv().await.unwrap()).unwrap();
            shell = agent_core::orchestration::shell::receive(Some(&shell), message, false)
                .unwrap()
                .unwrap();
        }
    })
    .await
    .expect("native catalog header is committed");
    for has_icon in [true, false] {
        if !has_icon {
            for workspace in &workspaces {
                std::fs::remove_file(workspace.join("favicon.svg")).unwrap();
            }
        }
        for project_limit in [5, 15] {
            let mut initial = agent_core::state::Snapshot::default();
            initial.lists.list_query = Arc::new(agent_protocol::models::ListQuery {
                project_limit,
                ..Default::default()
            });
            initial.lists.subscription = Some(subscription);
            let (mut snapshot, _) = agent_core::state::reduce(
                &initial,
                "",
                agent_core::state::Event::ListStream {
                    subscription_id: subscription,
                    message: Box::new(
                        agent_protocol::orchestration::shell::ShellStreamItem::Snapshot {
                            snapshot: shell.clone(),
                            resolved_repository_identity_roots: None,
                        },
                    ),
                },
            );
            let projects = &snapshot.lists.threads.as_ref().unwrap().projects;
            assert!(
                projects.iter().all(|project| project.icon.is_none()),
                "filesystem icons never enter saved list rows"
            );
            let request = op::ReadListDecorations {
                scope: op::ListDecorationScope::Root,
                projects: projects
                    .iter()
                    .map(|project| {
                        (
                            project.id.clone(),
                            project.roots.iter().map(|root| root.path.clone()).collect(),
                        )
                    })
                    .collect(),
                threads: Vec::new(),
            };
            let response = service
                .dispatch(session.id(), &Call::ReadListDecorations(request.clone()))
                .await
                .unwrap();
            let Response::Success { result } = agent_protocol::protocol::decode::<
                Response<op::ListDecorations>,
            >(&response.initial)
            .unwrap() else {
                panic!("decoration failed")
            };
            let project_ids = projects
                .iter()
                .map(|project| project.id.clone())
                .collect::<Vec<_>>();
            request.apply(&mut snapshot, "", result);
            for id in project_ids {
                Arc::make_mut(&mut snapshot.lists.expanded_projects).insert(id, 5);
            }
            let list = snapshot.thread_list().unwrap();
            assert_eq!(list.projects.len(), (project_limit as usize).min(6));
            assert_eq!(list.has_more_projects, project_limit == 5);
            assert!(list.threads.is_empty());
            assert_eq!(list.projects[0].threads.len(), 1);
            assert_eq!(
                list.projects[0].threads[0].project_id.as_ref(),
                Some(&list.projects[0].id)
            );
            assert!(
                list.projects[1..]
                    .iter()
                    .all(|project| project.threads.is_empty())
            );
            assert!(
                list.projects
                    .iter()
                    .all(|project| project.icon.is_some() == has_icon)
            );
        }
    }
}

#[tokio::test]
async fn search_reads_saved_message_bodies_without_discovering_native_threads() {
    use super::*;
    use crate::host_rpc::runtime::{
        Runtime,
        native_fixture::{NativeAgent, own},
    };
    use crate::providers::AgentChange;
    use agent_protocol::{
        execution::ItemStatus,
        items::{AssistantPhase, Item, ItemBody},
        models::{Thread, ThreadResponse},
        session::{SessionChange, SessionRef},
    };
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("projects.json");
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let native = SessionRef::new(ProviderKind::Codex, "saved-source".into()).unwrap();
    let public = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let response = ThreadResponse {
        thread: Thread {
            id: Some(native.clone()),
            cwd: Some(directory.path().to_string_lossy().into()),
            name: Some("A conversation about storage".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let service = HostRpcService::new([agent.backend()], project_path.clone());
    own(&service, &public, &response).await;
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native,
            change: SessionChange::Item {
                turn_id: "recorded-turn".into(),
                item: Arc::new(Item::new(
                    "recorded-answer".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: "A persisted Needle exists only in the message body".into(),
                        phase: AssistantPhase::Final,
                    },
                )),
            },
        },
        None,
    )
    .await
    .unwrap();
    agent.records.lock().unwrap().insert(
        "unimported".into(),
        ThreadResponse {
            thread: Thread {
                id: Some(SessionRef::new(ProviderKind::Codex, "unimported".into()).unwrap()),
                preview: Some("Needle persisted in an unimported native thread".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = agent_protocol::conversation::records::SearchList {
        term: "needle persisted".into(),
        limit: None,
    };
    let result = service.search_list(&request).await.unwrap();
    assert_eq!(
        result
            .data
            .iter()
            .map(|thread| thread.id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec![public.clone()]
    );
    // A new service has the saved DB, no provider backend, and no native files.
    let reopened = HostRpcService::new([], project_path);
    let result = reopened.search_list(&request).await.unwrap();
    assert_eq!(
        result
            .data
            .iter()
            .map(|thread| thread.id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec![public]
    );
}
