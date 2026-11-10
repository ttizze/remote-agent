//! Conversation lists: native titles merged with registered projects and Host conversations.
use super::*;
use crate::providers::SessionSummary;

fn append_named_titles(
    mut summaries: Vec<SessionSummary>,
    threads: Vec<Thread>,
) -> Vec<SessionSummary> {
    let original_len = summaries.len();
    let mut ids: std::collections::HashSet<_> = summaries
        .iter()
        .map(|summary| summary.thread.id.clone())
        .collect();
    for thread in threads {
        if ids.insert(thread.id.clone()) {
            summaries.push(SessionSummary {
                branch: thread.git_branch.clone(),
                thread,
            });
        }
    }
    if summaries.len() != original_len {
        summaries.sort_by(|left, right| {
            right
                .thread
                .updated_at
                .unwrap_or_default()
                .total_cmp(&left.thread.updated_at.unwrap_or_default())
        });
    }
    summaries
}

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
    fn title_capabilities(
        &self,
        session: Option<&agent_protocol::session::SessionRef>,
        native: agent_protocol::session::Capabilities,
    ) -> Result<agent_protocol::session::Capabilities, Failure> {
        let row = session
            .map(|session| self.inner.conversations.get(session))
            .transpose()?
            .flatten();
        Ok(row.map_or(native, |row| {
            self.conversation_capabilities(
                row.active.provider,
                row.sources().map(|source| source.provider),
            )
        }))
    }

    pub(super) fn saved_conversation_titles(&self, search: &str) -> Result<Vec<Thread>, Failure> {
        let search = search.trim().to_lowercase();
        let mut threads = Vec::new();
        for (mut thread, sources) in self.inner.conversations.summaries()? {
            let Some(provider) = thread.active_provider else {
                continue;
            };
            let error = self
                .inner
                .providers
                .get(provider)
                .and_then(|agent| agent.availability())
                .err();
            if error.is_none() && thread.preparation.is_none() {
                continue;
            }
            let text = format!(
                "{} {}",
                thread.name.as_deref().unwrap_or_default(),
                thread.preview.as_deref().unwrap_or_default()
            )
            .to_lowercase();
            if !text.contains(&search) {
                continue;
            }
            if let Some(error) = error
                && thread.preparation.is_none()
            {
                thread.status = agent_protocol::models::SessionStatus::Unavailable;
                thread.history_read_state = Some(agent_protocol::session::HistoryReadState::new(
                    agent_protocol::session::HistoryReadKind::Unavailable,
                    vec![format!("{}: {}", provider.key(), error.message)],
                ));
            }
            thread.capabilities =
                Some(self.conversation_capabilities(provider, sources.into_iter()));
            threads.push(thread);
        }
        threads.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(threads)
    }

    /// A Host-owned name can match even when its native provider title does not.
    async fn named_conversation_titles(
        &self,
        search: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<(ProviderKind, ThreadResponse)>, Failure> {
        if search.trim().is_empty() {
            return Ok(Vec::new());
        }
        let targets = self.inner.conversations.active_sessions(search)?;
        let found = futures_util::future::join_all(targets.into_iter().map(|target| async move {
            let response = tokio::time::timeout_at(deadline, async {
                self.inner
                    .providers
                    .get(target.provider)?
                    .open(&target.id, 1, false)
                    .await
            })
            .await;
            (target.provider, response)
        }))
        .await;
        Ok(found
            .into_iter()
            .filter_map(|(provider, result)| result.ok()?.ok().map(|response| (provider, response)))
            .collect())
    }

    pub(super) async fn host_title_list(
        &self,
        search: &str,
        query: crate::projects::titles::TitleQuery<'_>,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let revision = self.inner.router.list_revision();
        let task_revision = self.inner.router.task_activity_revision();
        let agents = self.inner.providers.all();
        let first_page_size = title_first_page_size(query);
        let started = std::time::Instant::now();
        let owned = self.inner.conversations.active_sessions("")?;
        let scope = if search.trim().is_empty() {
            SessionListScope::Roots(&owned)
        } else {
            SessionListScope::WithSubagents(&owned)
        };
        let ((snapshot, projects_elapsed), (mut first_pages, first_pages_elapsed)) = tokio::join!(
            async { (self.project_snapshot().await, started.elapsed()) },
            async {
                let started = tokio::time::Instant::now();
                let deadline = started + SESSION_LIST_TIMEOUT;
                let pages = futures_util::future::join_all(agents.iter().map(|(provider, agent)| async move {
                    let read_started = std::time::Instant::now();
                    let page = read_session_page(
                        Agent::list(agent.as_ref(), search, None, scope, first_page_size),
                        deadline,
                    ).await;
                    tracing::info!(target: "agent", operation = "host.provider.list",
                        message = %format_args!("provider={} elapsed_ms={} code={}", provider.key(), read_started.elapsed().as_millis(), page.as_ref().err().map_or("ok", |error| error.code)));
                    page
                }))
                .await;
                (pages, started.elapsed())
            },
        );
        let snapshot = snapshot?;
        // Project discovery must not consume the native page-read budget.
        // Reuse the first pages and give subsequent reads only the time left.
        let deadline =
            tokio::time::Instant::now() + SESSION_LIST_TIMEOUT.saturating_sub(first_pages_elapsed);
        let mut named_by_provider = std::collections::BTreeMap::<_, Vec<_>>::new();
        for (provider, response) in self.named_conversation_titles(search, deadline).await? {
            named_by_provider
                .entry(provider)
                .or_default()
                .push(response.thread);
        }
        for (provider, threads) in named_by_provider {
            if let Some(index) = agents
                .iter()
                .position(|(candidate, _)| *candidate == provider)
                && let Ok(page) = &mut first_pages[index]
            {
                page.data = append_named_titles(std::mem::take(&mut page.data), threads);
            }
        }
        let initial_elapsed = started.elapsed();
        let mut listings = futures_util::future::join_all(agents.iter().zip(first_pages).map(
            |((provider, agent), page)| async move {
                let mut threads = session_pages(
                    agent.as_ref(),
                    search,
                    scope,
                    first_page_size,
                    TITLE_PAGE_SIZE,
                    Some(deadline),
                    Some(page),
                )
                .map_ok(|page| futures_util::stream::iter(page.into_iter().map(Ok::<_, Failure>)))
                .try_flatten()
                .try_filter_map(|summary| {
                    let branch = summary.branch;
                    futures_util::future::ready(
                        self.inner
                            .conversations
                            .summary(summary.thread)
                            .map(|thread| thread.map(|thread| SessionSummary { thread, branch })),
                    )
                })
                .boxed();
                let head = threads.try_next().await;
                (*provider, agent.capabilities(), threads, head)
            },
        ))
        .await;
        let saved_titles = self.saved_conversation_titles(search)?;
        let saved = !saved_titles.is_empty();
        let mut saved_by_provider = std::collections::BTreeMap::<_, Vec<_>>::new();
        for thread in saved_titles {
            saved_by_provider
                .entry(thread.active_provider.expect("saved title provider"))
                .or_default()
                .push(Ok(SessionSummary {
                    branch: thread.git_branch.clone(),
                    thread,
                }));
        }
        for (provider, saved) in saved_by_provider {
            let mut threads = futures_util::stream::iter(saved).boxed();
            let head = threads.try_next().await;
            listings.push((
                provider,
                agent_protocol::session::Capabilities::default(),
                threads,
                head,
            ));
        }
        let mut titles = crate::projects::titles::TitleList::new(&snapshot.projects, query);
        let mut provider_errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut listed = std::collections::HashSet::new();
        let successful = listings.iter().any(|(_, _, _, head)| head.is_ok());
        while let Some(newest) = listings
            .iter()
            .filter_map(|(_, _, _, head)| head.as_ref().ok()?.as_ref())
            .map(|summary| summary.thread.updated_at.unwrap_or_default())
            .max_by(f64::total_cmp)
        {
            // Never scan additional pages merely to exhaust a timestamp tie in
            // the requested root window. Order ties within that window by ID.
            let maximum = titles.remaining_chats().unwrap_or(usize::MAX).max(1);
            let mut group = Vec::new();
            for (_, capabilities, threads, head) in &mut listings {
                while group.len() < maximum
                    && head.as_ref().is_ok_and(|thread| {
                        thread.as_ref().is_some_and(|summary| {
                            summary
                                .thread
                                .updated_at
                                .unwrap_or_default()
                                .total_cmp(&newest)
                                == std::cmp::Ordering::Equal
                        })
                    })
                {
                    group.push((head.as_mut().unwrap().take().unwrap(), *capabilities));
                    if group.len() < maximum {
                        *head = threads.try_next().await;
                    }
                }
            }
            group.sort_by(|(a, _), (b, _)| a.thread.id.cmp(&b.thread.id));
            for (summary, capabilities) in group {
                let Some(mut thread) = summary
                    .thread
                    .id
                    .as_ref()
                    .is_none_or(|id| !listed.contains(id))
                    .then_some(summary.thread)
                else {
                    continue;
                };
                thread.git_branch = summary.branch;
                let capabilities = self.title_capabilities(thread.id.as_ref(), capabilities)?;
                describe_thread(&mut thread, capabilities, &snapshot);
                listed.extend(thread.id.clone());
                titles.push(thread);
                if titles.done() {
                    break;
                }
            }
            if titles.done() {
                break;
            }
            // A group can fill its allowance with assigned rows that are not
            // retained. Continue that provider only if the visible page still
            // needs rows; do not start another read after its lookahead.
            for (_, _, threads, head) in &mut listings {
                if matches!(head, Ok(None)) {
                    *head = threads.try_next().await;
                }
            }
        }
        for (provider, _, _, head) in listings {
            if let Err(error) = head {
                provider_errors.insert(provider.key().to_owned(), serde_json::to_value(error)?);
            }
        }
        if !successful && !saved && !provider_errors.is_empty() {
            return Err(Failure::new(
                "sessions_unavailable",
                serde_json::to_value(provider_errors)?,
            ));
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
        if !provider_errors.is_empty() {
            page.provider_errors = Some(provider_errors);
        }
        tracing::info!(target: "agent", operation = "host.thread.list.stages",
            message = %format_args!("projects_ms={} titles_ms={} titles={}", projects_elapsed.as_millis(), (first_pages_elapsed + started.elapsed().saturating_sub(initial_elapsed)).as_millis(), page.data.len()));
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

const TITLE_PAGE_SIZE: u32 = 100;

// One title walk supplies each requested quota plus lookahead.
fn title_first_page_size(query: crate::projects::titles::TitleQuery<'_>) -> u32 {
    match query {
        crate::projects::titles::TitleQuery::Root {
            limit,
            project_limits,
            part,
            ..
        } => {
            let chats = if part == Some(agent_protocol::models::ListPart::Projects) {
                0
            } else {
                limit.max(1).saturating_add(1)
            };
            let projects = project_limits.map_or(30, |limits| {
                limits.values().fold(0u32, |sum, limit| {
                    sum.saturating_add(limit.max(&1).saturating_add(1))
                })
            });
            (chats.saturating_add(projects)).clamp(1, TITLE_PAGE_SIZE)
        }
        crate::projects::titles::TitleQuery::Project { .. } => TITLE_PAGE_SIZE,
    }
}

#[cfg(test)]
mod tests {
    fn named_thread(
        id: Option<&str>,
        provider: agent_protocol::session::ProviderKind,
        name: &str,
        updated_at: Option<f64>,
    ) -> super::Thread {
        super::Thread {
            id: id.map(|id| agent_protocol::session::SessionRef {
                provider,
                id: id.into(),
            }),
            name: Some(name.into()),
            preview: Some(format!("preview-{name}")),
            git_branch: Some(format!("git-{name}")),
            updated_at,
            ..Default::default()
        }
    }

    #[test]
    fn named_titles_keep_existing_rows_first_new_payloads_and_stable_timestamp_ties() {
        use super::*;
        use agent_protocol::session::ProviderKind::{Claude, Codex};
        for existing_count in [3, 128] {
            for incoming_count in [6, 63, 64, 65] {
                let mut existing = vec![
                    SessionSummary {
                        thread: named_thread(Some("a"), Codex, "existing", Some(5.)),
                        branch: Some("original branch".into()),
                    },
                    SessionSummary {
                        thread: named_thread(Some("a"), Codex, "existing duplicate", Some(1.)),
                        branch: None,
                    },
                    SessionSummary {
                        thread: named_thread(None, Codex, "existing anonymous", None),
                        branch: Some("anonymous branch".into()),
                    },
                ];
                for index in 3..existing_count {
                    existing.push(SessionSummary {
                        thread: named_thread(
                            Some("a"),
                            Codex,
                            &format!("existing duplicate {index}"),
                            Some(1.),
                        ),
                        branch: None,
                    });
                }
                let mut incoming = vec![
                    named_thread(Some("a"), Codex, "ignored existing update", Some(999.)),
                    named_thread(Some("b"), Codex, "first new b", Some(5.)),
                    named_thread(Some("b"), Codex, "ignored new b update", Some(999.)),
                    named_thread(Some("b"), Claude, "other provider b", Some(5.)),
                    named_thread(None, Codex, "ignored anonymous update", Some(999.)),
                    named_thread(Some("c"), Codex, "new c", None),
                ];
                incoming.resize(incoming_count, incoming[0].clone());
                let original = |row: &SessionSummary| (row.thread.clone(), row.branch.clone());
                let appended = |thread: &Thread| (thread.clone(), thread.git_branch.clone());
                let mut expected = vec![
                    original(&existing[0]),
                    appended(&incoming[1]),
                    appended(&incoming[3]),
                    original(&existing[1]),
                ];
                expected.extend(existing[3..].iter().map(original));
                expected.push(original(&existing[2]));
                expected.push(appended(&incoming[5]));
                let result = append_named_titles(existing, incoming)
                    .into_iter()
                    .map(|row| (row.thread, row.branch))
                    .collect::<Vec<_>>();
                assert_eq!(
                    result, expected,
                    "existing={existing_count}, incoming={incoming_count}"
                );
            }
        }
    }

    #[test]
    fn duplicate_only_named_titles_leave_the_original_page_order_untouched() {
        use super::*;
        use agent_protocol::session::ProviderKind::Codex;
        for count in [0, 1, 64] {
            let existing = vec![
                SessionSummary {
                    thread: named_thread(None, Codex, "anonymous", None),
                    branch: Some("original".into()),
                },
                SessionSummary {
                    thread: named_thread(Some("a"), Codex, "first", Some(5.)),
                    branch: None,
                },
                SessionSummary {
                    thread: named_thread(Some("a"), Codex, "duplicate", Some(1.)),
                    branch: None,
                },
            ];
            let expected = existing
                .iter()
                .map(|row| (row.thread.clone(), row.branch.clone()))
                .collect::<Vec<_>>();
            let incoming = (0..count)
                .map(|index| {
                    named_thread(
                        if index % 2 == 0 { Some("a") } else { None },
                        Codex,
                        "ignored",
                        Some(999.),
                    )
                })
                .collect();
            let result = append_named_titles(existing, incoming)
                .into_iter()
                .map(|row| (row.thread, row.branch))
                .collect::<Vec<_>>();
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn named_title_index_includes_items_appended_before_it_was_created() {
        use super::*;
        use agent_protocol::session::ProviderKind::Codex;
        let mut incoming = (0..200)
            .map(|index| {
                let id = format!("chat-{index}");
                named_thread(
                    if index == 150 { None } else { Some(&id) },
                    Codex,
                    &format!("original {index}"),
                    Some(0.),
                )
            })
            .collect::<Vec<_>>();
        let expected = incoming.clone();
        incoming.push(named_thread(
            Some("chat-0"),
            Codex,
            "ignored late duplicate",
            Some(999.),
        ));
        incoming.push(named_thread(
            None,
            Codex,
            "ignored anonymous duplicate",
            Some(999.),
        ));
        let result = append_named_titles(Vec::new(), incoming);
        assert_eq!(result.len(), expected.len());
        for (row, thread) in result.into_iter().zip(expected) {
            assert_eq!(row.thread, thread);
            assert_eq!(row.branch, thread.git_branch);
        }
    }

    #[test]
    fn named_titles_keep_total_timestamp_order_and_original_float_bits() {
        use super::*;
        use agent_protocol::session::ProviderKind::Codex;
        let positive_nan = 0x7ff8_0000_0000_0003;
        let negative_nan = 0xfff8_0000_0000_0002;
        let existing = [
            ("z-none", None),
            ("negative zero", Some(-0.)),
            ("positive nan", Some(f64::from_bits(positive_nan))),
            ("negative nan", Some(f64::from_bits(negative_nan))),
            ("a-zero", Some(0.)),
            ("one", Some(1.)),
        ]
        .into_iter()
        .map(|(name, time)| SessionSummary {
            thread: named_thread(Some(name), Codex, name, time),
            branch: None,
        })
        .collect();
        let result = append_named_titles(
            existing,
            vec![named_thread(
                Some("infinity"),
                Codex,
                "infinity",
                Some(f64::INFINITY),
            )],
        );
        assert_eq!(
            result
                .iter()
                .map(|row| row.thread.name.as_deref().unwrap())
                .collect::<Vec<_>>(),
            [
                "positive nan",
                "infinity",
                "one",
                "z-none",
                "a-zero",
                "negative zero",
                "negative nan"
            ]
        );
        assert_eq!(
            result
                .iter()
                .map(|row| row.thread.updated_at.map(f64::to_bits))
                .collect::<Vec<_>>(),
            [
                Some(positive_nan),
                Some(f64::INFINITY.to_bits()),
                Some(1f64.to_bits()),
                None,
                Some(0f64.to_bits()),
                Some((-0f64).to_bits()),
                Some(negative_nan)
            ]
        );
    }

    #[tokio::test]
    async fn sdk_reviews_stay_out_of_titles_and_search_after_host_restart() {
        use super::*;
        use crate::host_rpc::conversations::{Binding, Conversation};
        use agent_protocol::session::SessionRef;
        let root = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(root.path()).unwrap();
        let native = root.path().join("native");
        let transcripts = native.join("projects/shared");
        std::fs::create_dir_all(&transcripts).unwrap();
        let session = |id| SessionRef {
            provider: ProviderKind::Claude,
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
                service
                    .inner
                    .conversations
                    .save(Conversation {
                        id: public.clone(),
                        active: active.clone(),
                        cwd: cwd.to_string_lossy().into_owned(),
                        name: None,
                        preview: None,
                        bindings: vec![Binding {
                            session: active.clone(),
                            covered: None,
                        }],
                        segments: vec![],
                        handoffs: vec![],
                        startup: None,
                    })
                    .unwrap();
            }
            let snapshot = service.project_snapshot().await.unwrap();
            for search in ["", "NSH", "NSH message text sizing"] {
                let page = service
                    .host_title_list(
                        search,
                        crate::projects::titles::TitleQuery::Project {
                            id: &snapshot.projects[0].id,
                            limit: 10,
                            searching: !search.is_empty(),
                        },
                    )
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
                    [public.clone(), session(5)].into_iter().collect()
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
