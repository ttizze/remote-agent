//! Search queries native catalogs; ordinary list reads come from storage.
use super::*;

impl HostRpcService {
    pub(in crate::host_rpc) async fn project_snapshot(
        &self,
    ) -> Result<crate::projects::state::Snapshot, Failure> {
        self.inner
            .projects
            .load()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))
    }
    pub(super) async fn search_list(
        &self,
        request: &agent_protocol::conversation::records::SearchList,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let limit = request.limit.map(|limit| limit.max(1) as usize);
        let owned = self
            .inner
            .conversation
            .storage
            .active_sources()
            .map_err(super::super::runtime::storage_failure)?;
        let scope = SessionListScope::WithSubagents(&owned);
        let providers = self.inner.providers.all();
        let snapshot = self.project_snapshot().await?;
        let listings = futures_util::future::join_all(providers.into_iter().map(
            |(provider, agent)| async move {
                let mut pages = Box::pin(session_pages(
                    agent.as_ref(),
                    &request.term,
                    scope,
                    100,
                    100,
                    Some(tokio::time::Instant::now() + SESSION_LIST_TIMEOUT),
                    None,
                ));
                let mut rows = std::collections::BTreeMap::new();
                let mut error = None;
                'pages: loop {
                    let page = match pages.try_next().await {
                        Ok(Some(page)) => page,
                        Ok(None) => break,
                        Err(failure) => {
                            error = Some(failure);
                            break;
                        }
                    };
                    let storage = self.inner.conversation.storage.clone();
                    let page = tokio::task::spawn_blocking(move || {
                        let mut rows = Vec::new();
                        for summary in page {
                            if let Some(mut thread) = storage.native_summary(summary.thread)? {
                                thread.git_branch = summary.branch.or(thread.git_branch);
                                rows.push(thread);
                            }
                        }
                        Ok::<_, String>(rows)
                    })
                    .await
                    .map_err(|error| super::super::runtime::storage_failure(error.to_string()))?
                    .map_err(super::super::runtime::storage_failure)?;
                    for mut thread in page {
                        let Some(id) = thread.id.clone() else {
                            continue;
                        };
                        thread.capabilities =
                            Some(agent_protocol::conversation::records::capabilities(
                                thread.capabilities.unwrap_or_else(|| agent.capabilities()),
                            ));
                        rows.insert(id, thread);
                        if limit.is_some_and(|limit| rows.len() > limit) {
                            break 'pages;
                        }
                    }
                }
                Ok::<_, Failure>((provider, rows, error))
            },
        ))
        .await;
        let saved = self
            .inner
            .conversation
            .storage
            .summaries(&request.term, false)
            .map_err(super::super::runtime::storage_failure)?;
        let mut rows = std::collections::BTreeMap::new();
        let mut errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut successful = !saved.is_empty();
        for listing in listings {
            let (provider, native, error) = listing?;
            successful |= !native.is_empty();
            rows.extend(native);
            if let Some(error) = error {
                errors.insert(provider.key().into(), serde_json::to_value(error)?);
            } else {
                successful = true;
            }
        }
        for thread in saved {
            if let Some(id) = thread.id.clone() {
                rows.insert(id, thread);
            }
        }
        if !successful && !errors.is_empty() {
            return Err(Failure::new(
                "sessions_unavailable",
                serde_json::to_value(errors)?,
            ));
        }
        let mut data = rows.into_values().collect::<Vec<_>>();
        for thread in &mut data {
            if let Some((_, error)) = self.inner.providers.startup_errors().find(|(provider, _)| {
                Some(*provider)
                    == thread
                        .active_provider
                        .or_else(|| thread.id.as_ref().and_then(|id| id.provider))
            }) {
                *thread = agent_protocol::conversation::records::unavailable(
                    thread.clone(),
                    &error.message,
                );
            }
            let capabilities = thread.capabilities.unwrap_or_default();
            describe_thread(thread, capabilities, &snapshot);
        }
        data.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        let has_more = limit.is_some_and(|limit| data.len() > limit);
        if let Some(limit) = limit {
            data.truncate(limit);
        }
        let projects = snapshot
            .projects
            .into_iter()
            .filter(|project| {
                request.term.trim().is_empty()
                    || data
                        .iter()
                        .any(|thread| thread.project_id.as_deref() == Some(project.id.as_str()))
            })
            .collect();
        Ok(agent_protocol::models::ThreadList {
            limit: request
                .limit
                .unwrap_or_else(|| data.len().try_into().unwrap_or(u32::MAX)),
            data,
            projects,
            has_more,
            provider_errors: (!errors.is_empty()).then_some(errors),
            ..Default::default()
        })
    }
    pub(super) async fn list_decorations(
        &self,
        params: &op::ReadListDecorations,
    ) -> Result<op::ListDecorations, Failure> {
        let projects = params.projects.clone();
        let icons = tokio::task::spawn_blocking(move || {
            projects
                .into_iter()
                .map(|(id, roots)| {
                    let icon = roots.iter().find_map(|root| {
                        crate::projects::icons::resolve(std::path::Path::new(root))
                    });
                    (id, icon)
                })
                .collect()
        });
        let (icons, changes) = tokio::join!(
            icons,
            crate::workspace_review::directory_changes(
                params
                    .threads
                    .iter()
                    .map(|(_, cwd, _)| cwd.clone())
                    .collect()
            )
        );
        let icons = icons.map_err(|error| Failure::new("project_icons_unavailable", error))?;
        let changes = changes.map_err(|error| Failure::new("workspace_review_failed", error))?;
        Ok(op::ListDecorations {
            icons,
            statuses: params
                .threads
                .iter()
                .map(|(id, cwd, _)| (id.clone(), changes.get(cwd).copied()))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_full_search_does_not_truncate_later_conversations_at_an_arbitrary_catalog_limit() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::NativeAgent;
        use agent_protocol::{
            models::{Thread, ThreadResponse},
            session::SessionRef,
        };
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
        for index in 0..10_005 {
            let id = format!("conversation-{index}");
            agent.records.lock().unwrap().insert(
                id.clone(),
                ThreadResponse {
                    thread: Thread {
                        id: Some(SessionRef::new(ProviderKind::Codex, id).unwrap()),
                        preview: Some("matches".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        let service = HostRpcService::new(
            [agent.backend()],
            crate::ProjectStore::new(directory.path().join("projects.json")),
        );
        let all = service
            .search_list(&agent_protocol::conversation::records::SearchList {
                term: "matches".into(),
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(all.data.len(), 10_005);
        assert!(!all.has_more);
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
        for (id, entrypoint, title) in [
            (1, "sdk-ts", "NSH user conversation"),
            (2, "sdk-ts", "NSH message text sizing"),
            (3, "sdk-py", "NSH automated analysis"),
            (4, "sdk-cli", "NSH CLI review"),
            (5, "cli", "NSH terminal conversation"),
        ] {
            let id = session(id).id;
            let rows = [
                serde_json::json!({"type":"user","sessionId":id,"cwd":cwd,"entrypoint":entrypoint,"promptSource":"sdk","message":{"content":title}}),
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
            let projects = ProjectStore::new(root.path().join("bex-projects.json"));
            projects.register(&cwd).await.unwrap();
            let service = HostRpcService::new([agent.into()], projects);
            if !restarting {
                let agent = service
                    .inner
                    .providers
                    .discovery(ProviderKind::Claude)
                    .unwrap();
                let response = agent.open(&active.id, 10, true).await.unwrap();
                own(&service, &public, &response);
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
                own(&service, &id, &response);
                side_chat = Some(id);
            }
            for search in ["", "NSH", "NSH message text sizing"] {
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
                let expected = if search == "NSH message text sizing" {
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
            let user = serde_json::json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":"hello"}});
            std::fs::write(transcripts.join(format!("{id}.jsonl")), format!("{user}\n")).unwrap();
        }
        let agent = crate::providers::Claude::load(
            root.path().join("missing-cli"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let projects = ProjectStore::new(root.path().join("bex-worktrees.json"));
        projects.register(&cwd).await.unwrap();
        let service = HostRpcService::new([agent.into()], projects);
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
        std::fs::write(state.with_file_name("bex-projects.json"), "{").unwrap();
        let service = HostRpcService::new(
            [crate::providers::codex::Codex::new(Err("provider is offline".into())).into()],
            ProjectStore::new(state),
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
        let projects = ProjectStore::new(root.path().join("bex-worktrees.json"));
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
            let user = serde_json::json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":"hello"}});
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
        let service = HostRpcService::new([claude.into()], projects);
        let session = service.open_session();
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
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
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
        std::fs::write(transcript.join(format!("{id}.jsonl")),serde_json::json!({"type":"user","sessionId":id,"cwd":dunce::canonicalize(&workspace).unwrap(),"message":{"content":"Branding task"}}).to_string()+"\n").unwrap();
        let projects = ProjectStore::new(root.path().join("bex-worktrees.json"));
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
            projects,
        );
        use agent_core::state::operations::Operation;
        use agent_protocol::{
            conversation::records,
            sync::{Key, Scope},
        };
        let session = service.open_session();
        let subscription = uuid::Uuid::new_v4();
        let reply = service
            .dispatch(
                session.id(),
                &Call::SubscribeList(records::SubscribeList {
                    refresh: false,
                    subscription,
                    cached: None,
                }),
            )
            .await
            .unwrap();
        let Response::Success { result: opened } =
            agent_protocol::protocol::decode::<Response<records::OpenedList>>(&reply.initial)
                .unwrap()
        else {
            panic!("list subscription failed")
        };
        let mut replica = records::Replica::new(Scope::List, 1)
            .apply(1, opened.initial)
            .unwrap();
        let mut updates = reply.updates.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !replica
                .rows()
                .keys()
                .any(|key| matches!(key, Key::Thread(_)))
            {
                let message =
                    agent_protocol::protocol::decode(&updates.recv().await.unwrap()).unwrap();
                replica = replica.apply(1, message).unwrap();
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
                initial.lists.replica = Some(Arc::new(records::Replica::new(Scope::List, 1)));
                let (mut snapshot, _) = agent_core::state::reduce(
                    &initial,
                    "",
                    agent_core::state::Event::ListRows {
                        subscription_id: subscription,
                        message: Box::new(replica.snapshot().unwrap()),
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
}
