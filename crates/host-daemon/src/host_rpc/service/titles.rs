//! Conversation lists read only Host-owned rows; discovery is an import operation.
use super::*;

impl HostRpcService {
    pub(super) async fn list_sessions(
        &self,
        params: &op::ListSessions,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let started = std::time::Instant::now();
        let result = self
            .host_title_list(
                &params.query.search_term,
                crate::projects::titles::TitleQuery::Root {
                    limit: params.query.limit,
                    project_limit: params.query.project_limit,
                    project_limits: params.query.project_limits.as_ref(),
                    part: params.query.part,
                    searching: !params.query.search_term.trim().is_empty(),
                },
            )
            .await;
        tracing::info!(target: "agent", operation = "host.thread.list.performance",
            message = %format_args!("elapsed_ms={} success={}", started.elapsed().as_millis(), result.is_ok()));
        result
    }

    pub(super) async fn list_project_sessions(
        &self,
        params: &op::ListProjectSessions,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        self.host_title_list(
            &params.search_term,
            crate::projects::titles::TitleQuery::Project {
                id: &params.project_id,
                limit: params.limit,
                searching: !params.search_term.trim().is_empty(),
            },
        )
        .await
    }

    pub(super) async fn project_snapshot(
        &self,
    ) -> Result<crate::projects::state::Snapshot, Failure> {
        self.inner
            .projects
            .load()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))
    }
    pub(super) fn saved_conversation_titles(&self, search: &str) -> Result<Vec<Thread>, Failure> {
        let saved = self
            .inner
            .database
            .titles(search)
            .map_err(transcripts::storage_failure)?;
        let mut threads = Vec::with_capacity(saved.len());
        for title in saved {
            let mut thread = title.thread;
            let Some(id) = thread.id.clone() else {
                continue;
            };
            let provider = thread.active_provider.unwrap_or(id.provider);
            let capabilities = if title.managed {
                self.conversation_capabilities(provider, title.sources.into_iter())
            } else {
                self.inner
                    .providers
                    .get(provider)
                    .map(|agent| agent.capabilities())
                    .unwrap_or_default()
            };
            thread.capabilities = Some(capabilities);
            if thread.preparation.is_none()
                && self
                    .inner
                    .providers
                    .get(provider)
                    .and_then(|agent| agent.availability())
                    .is_err()
            {
                thread.status = agent_protocol::models::SessionStatus::Unavailable;
            }
            threads.push(self.inner.router.overlay_execution(&id, thread));
        }
        threads.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(threads)
    }

    pub(super) async fn host_title_list(
        &self,
        search: &str,
        query: crate::projects::titles::TitleQuery<'_>,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let revision = self.inner.router.list_revision();
        let task_revision = self.inner.router.task_activity_revision();
        let snapshot = self.project_snapshot().await?;
        let mut titles = crate::projects::titles::TitleList::new(&snapshot.projects, query);
        for mut thread in self.saved_conversation_titles(search)? {
            let capabilities = thread.capabilities.unwrap_or_default();
            describe_thread(&mut thread, capabilities, &snapshot);
            titles.push(thread);
            if titles.done() {
                break;
            }
        }
        let mut page = titles.finish();
        self.inner
            .router
            .task_activity(page.data.clone(), task_revision);
        page.revision = revision;
        for thread in &mut page.data {
            thread.list_revision = revision;
        }
        for project in &mut page.projects {
            project.list_revision = revision;
        }
        Ok(page)
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
        let worktrees = if params.threads.is_empty() {
            Default::default()
        } else {
            crate::worktrees::workspace_roots(self.inner.projects.path())
                .await
                .map_err(|error| Failure::new("worktree_status_failed", error))?
        };
        let sources: Vec<_> = params
            .threads
            .iter()
            .map(|(id, cwd, branch)| {
                let mapping =
                    crate::projects::state::worktree_mapping(&worktrees, std::path::Path::new(cwd));
                (
                    id.clone(),
                    crate::worktrees::StatusSource {
                        cwd: cwd.clone(),
                        checkout: mapping.map(|(checkout, _)| checkout.to_owned()),
                        repository: mapping.map(|(_, repository)| repository.to_owned()),
                        branch: branch.clone(),
                    },
                )
            })
            .collect();
        let (icons, statuses) = tokio::join!(
            icons,
            crate::worktrees::directory_statuses(
                sources.iter().map(|(_, source)| source.clone()).collect()
            )
        );
        let icons = icons.map_err(|error| Failure::new("project_icons_unavailable", error))?;
        let statuses = statuses.map_err(|error| Failure::new("worktree_status_failed", error))?;
        Ok(op::ListDecorations {
            icons,
            statuses: sources
                .into_iter()
                .map(|(id, source)| (id, statuses.get(&source).copied()))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn saved_list_and_transcript_survive_provider_failure_without_native_listing() {
        use super::*;
        use crate::host_rpc::service::conversations::tests::{NativeAgent, call};
        use std::sync::atomic::Ordering;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("worktrees.json");
        let agent = NativeAgent::new(ProviderKind::Codex, root.path().join("codex"));
        let service = HostRpcService::new([agent.backend()], ProjectStore::new(&path));
        let connection = service.open_session();
        let created: agent_protocol::session::OpenedSession = call(
            &service,
            connection.id(),
            Call::CreateSession(op::CreateSession {
                initial_input: None,
                provider: ProviderKind::Codex,
                cwd: Some(root.path().to_str().unwrap().into()),
                model: None,
            }),
        )
        .await
        .unwrap();
        let id = created.session;
        service
            .import_transcript(id.clone(), false)
            .unwrap()
            .await
            .unwrap();
        let _: op::SubmissionReceipt = call(
            &service,
            connection.id(),
            Call::Submit(op::Submission {
                thread_id: id.clone(),
                client_user_message_id: "input".into(),
                input: vec![op::Input::Text {
                    text: "Saved task".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
            }),
        )
        .await
        .unwrap();
        agent.fail_list.store(1, Ordering::SeqCst);
        let list = service
            .list_sessions(&op::ListSessions::new(Default::default()))
            .await
            .unwrap();
        assert!(
            list.data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&id))
        );
        assert_eq!(
            agent.fail_list.load(Ordering::SeqCst),
            1,
            "list reads must not reach the provider"
        );
        drop(connection);
        drop(service);
        let service = HostRpcService::new(
            [Backend::unavailable(ProviderKind::Codex, "offline")],
            ProjectStore::new(path),
        );
        let connection = service.open_session();
        let list = service
            .list_sessions(&op::ListSessions::new(Default::default()))
            .await
            .unwrap();
        assert!(
            list.data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&id))
        );
        assert!(
            serde_json::to_value(list)
                .unwrap()
                .get("providerErrors")
                .is_none()
        );
        let opened: agent_protocol::session::OpenedSession = call(
            &service,
            connection.id(),
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: id,
                limit: 10,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
        let turns = opened.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 1);
        assert!(
            agent_protocol::handoff::Message::from_turn(&turns[0])
                .any(|message| message.text == "Saved task")
        );
        assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn imported_project_titles_page_past_the_first_native_page() {
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
        service
            .import_projects(&[cwd.to_str().unwrap().into()])
            .await
            .unwrap();
        let snapshot = service.project_snapshot().await.unwrap();
        let page = service
            .host_title_list(
                "",
                crate::projects::titles::TitleQuery::Project {
                    id: &snapshot.projects[0].id,
                    limit: 101,
                    searching: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.data.len(), 101);
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
            scope: op::ListDecorationScope::Root { part: None },
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
        // Seed observations independently; membership/list reads must not consult native metadata.
        for (_, agent) in service.inner.providers.all() {
            for summary in Agent::list(agent.as_ref(), "", None, SessionListScope::All, 100)
                .await
                .unwrap()
                .data
            {
                service.inner.database.observe(&summary.thread).unwrap();
            }
        }
        let session = service.open_session();
        for repaired in [false, true] {
            if repaired {
                std::fs::write(native.join(".claude.json"), "{}").unwrap();
            }
            let response = service
                .dispatch(
                    session.id(),
                    &Call::ListSessions(op::ListSessions {
                        query: Default::default(),
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
                        provider,
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
        service
            .import_projects(&[workspace.to_str().unwrap().into()])
            .await
            .unwrap();
        let session = service.open_session();
        for has_icon in [true, false] {
            if !has_icon {
                for workspace in &workspaces {
                    std::fs::remove_file(workspace.join("favicon.svg")).unwrap();
                }
            }
            for project_limit in [5, 15] {
                let call = Call::ListSessions(op::ListSessions {
                    query: agent_protocol::models::ListQuery {
                        project_limit,
                        ..Default::default()
                    },
                });
                let response = service.dispatch(session.id(), &call).await.unwrap();
                let reply = agent_protocol::protocol::decode::<
                    Response<agent_protocol::models::ThreadList>,
                >(&response.initial)
                .unwrap();
                let Response::Success { result } = reply else {
                    panic!("task list failed: {reply:?}")
                };
                assert_eq!(result.data.len(), 1);
                assert!(result.data[0].turns.is_none() && result.data[0].preview.is_none());
                let decoration = op::ReadListDecorations {
                    scope: op::ListDecorationScope::Root { part: None },
                    projects: result
                        .projects
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
                let decorated = service
                    .dispatch(session.id(), &Call::ReadListDecorations(decoration))
                    .await
                    .unwrap();
                let Response::Success { result: decorated } = agent_protocol::protocol::decode::<
                    Response<op::ListDecorations>,
                >(&decorated.initial)
                .unwrap() else {
                    panic!("decoration failed")
                };
                let mut result = result;
                for project in &mut result.projects {
                    project.icon = decorated.icons[&project.id].clone();
                }
                assert_eq!(result.projects.len(), (project_limit as usize).min(6));
                assert_eq!(result.has_more_projects, project_limit == 5);
                let project_id = result.projects[0].id.clone();
                let response = service
                    .dispatch(
                        session.id(),
                        &Call::ListProjectSessions(op::ListProjectSessions {
                            project_id: project_id.clone(),
                            limit: 5,
                            search_term: String::new(),
                        }),
                    )
                    .await
                    .unwrap();
                let Response::Success { result: page } = agent_protocol::protocol::decode::<
                    Response<agent_protocol::models::ThreadList>,
                >(&response.initial)
                .unwrap() else {
                    panic!("project list failed")
                };
                let snapshot = agent_core::state::Snapshot {
                    lists: agent_core::state::Lists {
                        threads: Some(Arc::new(result)),
                        expanded_projects: Arc::new([(project_id.clone(), 5)].into()),
                        project_threads: Arc::new([(project_id, Arc::new(page))].into()),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let list = snapshot.thread_list().unwrap();
                assert_eq!(list.projects.len(), (project_limit as usize).min(6));
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
                assert_eq!(list.projects[0].monogram, "RA");
                for project in &list.projects {
                    assert_eq!(project.icon.is_some(), has_icon, "{}", project.name);
                    if let Some(icon) = &project.icon {
                        let image = image::load_from_memory(&icon.light_png)
                            .unwrap()
                            .into_rgba8();
                        assert_eq!(image.dimensions(), (64, 64));
                        assert_eq!(image.get_pixel(32, 32).0, [255, 0, 0, 255]);
                    }
                }
            }
        }
    }
}
