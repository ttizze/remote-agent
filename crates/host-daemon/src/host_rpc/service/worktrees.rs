//! Worktree listing and removal. Removal waits until no provider can be running there.
use super::*;

#[derive(Default)]
struct WorktreeBlockers {
    running: bool,
    terminal: bool,
    warnings: std::collections::BTreeSet<String>,
}

impl WorktreeBlockers {
    fn reason(self) -> Option<String> {
        // Known occupancy precedes uncertainty; warnings are sorted and deduplicated.
        let mut reasons = Vec::new();
        if self.running {
            reasons.push(
                "このワークツリーで作業を実行中です。完了または停止してから削除してください。"
                    .to_owned(),
            );
        }
        if self.terminal {
            reasons.push("このワークツリーのターミナルを閉じてから削除してください。".to_owned());
        }
        reasons.extend(
            self.warnings
                .into_iter()
                .map(|warning| format!("稼働状況を確認できないため削除できません: {warning}")),
        );
        (!reasons.is_empty()).then(|| reasons.join("\n"))
    }
}

impl HostRpcService {
    // Callers hold worktree_access so removal cannot race restoration.
    pub(in crate::host_rpc) async fn restore_workspace(&self, cwd: &str) -> Result<bool, Failure> {
        let Some(directory) = self
            .inner
            .worktrees
            .ensure_available(cwd)
            .await
            .map_err(|error| Failure::new("worktree_creation_failed", error))?
        else {
            return Ok(false);
        };
        self.inner
            .providers
            .discard_workspace_processes(&directory)
            .await?;
        Ok(true)
    }

    pub(super) async fn worktree_list(
        &self,
    ) -> Result<Vec<agent_protocol::models::Worktree>, Failure> {
        let worktrees = self
            .inner
            .worktrees
            .list()
            .await
            .map_err(|error| Failure::new("worktree_list_failed", error))?;
        if worktrees.is_empty() {
            return Ok(worktrees);
        }
        let saved = self
            .inner
            .conversation
            .storage
            .summaries("", true)
            .map_err(super::super::runtime::storage_failure)?
            .into_iter()
            .filter_map(|thread| Some((thread.id.clone()?, thread)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let (threads, warning) = self.worktree_threads(&saved).await?;
        let mut worktrees = worktrees
            .into_iter()
            .map(|tree| {
                (
                    tree,
                    WorktreeBlockers {
                        warnings: warning.iter().cloned().collect(),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        for (thread, from_provider) in threads {
            let active = worktree_active(
                thread.status,
                thread.turns.as_deref().unwrap_or_default(),
                !thread.requests.is_empty(),
            );
            let Some(cwd) = thread.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) else {
                if active {
                    return Err(Failure::new(
                        "worktree_activity_unknown",
                        "実行中の会話の作業場所を確認できないため削除できません。",
                    ));
                }
                continue;
            };
            let cwd = tokio::fs::canonicalize(cwd)
                .await
                .unwrap_or_else(|_| std::path::PathBuf::from(cwd));
            let cwd = dunce::simplified(&cwd);
            let matching: Vec<_> = worktrees
                .iter()
                .enumerate()
                .filter_map(|(index, (tree, _))| cwd.starts_with(&tree.path).then_some(index))
                .collect();
            if matching.is_empty() {
                continue;
            }
            // Resolve provider-specific uncertainty once per matching session,
            // reusing the history scan instead of rereading it for every tree.
            let idle_warning = if !active && let Some(id) = &thread.id {
                // Keep native IDs for conservative occupancy checks, including
                // released sessions. Public snapshots need their active binding.
                let native = if from_provider {
                    Some(id.clone())
                } else {
                    self.inner
                        .conversation
                        .storage
                        .active_source(id)
                        .map_err(super::super::runtime::storage_failure)?
                };
                if let Some(native) = native
                    && let Some(provider) = native.provider
                    && let Ok(agent) = self.inner.providers.get(provider)
                {
                    agent.workspace_idle_warning(&native.id).await
                } else {
                    None
                }
            } else {
                None
            };
            let display = if from_provider {
                self.inner
                    .conversation
                    .storage
                    .native_summary(thread)
                    .map_err(super::super::runtime::storage_failure)?
            } else {
                Some(thread)
            };
            for index in matching {
                let (worktree, blockers) = &mut worktrees[index];
                blockers.running |= active;
                if let Some(warning) = idle_warning {
                    blockers.warnings.insert(warning.into());
                }
                if let Some(thread) = &display
                    && let Some(id) = &thread.id
                {
                    if let Some(existing) =
                        worktree.threads.iter_mut().find(|thread| &thread.id == id)
                    {
                        existing.active |= active;
                        continue;
                    }
                    worktree
                        .threads
                        .push(agent_protocol::models::WorktreeThread {
                            id: id.clone(),
                            name: thread
                                .name
                                .clone()
                                .filter(|name| !name.is_empty())
                                .or_else(|| {
                                    thread
                                        .preview
                                        .as_deref()
                                        .map(|preview| preview.chars().take(120).collect())
                                })
                                .unwrap_or_else(|| "新しいチャット".into()),
                            active,
                        });
                }
            }
        }
        Ok(worktrees
            .into_iter()
            .map(|(mut worktree, mut blockers)| {
                blockers.terminal = self
                    .inner
                    .terminals
                    .in_use(std::path::Path::new(&worktree.path));
                worktree.blocked_reason = blockers.reason();
                worktree
            })
            .collect())
    }
    async fn worktree_threads(
        &self,
        saved: &std::collections::BTreeMap<agent_protocol::session::SessionRef, Thread>,
    ) -> Result<(Vec<(Thread, bool)>, Option<String>), Failure> {
        let mut warnings = std::collections::BTreeMap::new();
        let mut threads = Vec::new();
        for (provider, agent) in self.inner.providers.all() {
            let pages = session_pages(
                agent.as_ref(),
                "",
                SessionListScope::All,
                100,
                100,
                None,
                None,
            );
            futures_util::pin_mut!(pages);
            while let Some(result) = pages.next().await {
                match result {
                    Ok(page) => {
                        threads.extend(page.into_iter().map(|summary| (summary.thread, true)))
                    }
                    Err(error) if error.code == "invalid_session_list" => return Err(error),
                    Err(error) => {
                        warnings.insert(provider, error.to_string());
                        break;
                    }
                }
            }
        }
        // Committed preparation and execution headers also cover work the
        // provider has not listed yet.
        for thread in saved.values() {
            if let Some(provider) = thread.active_provider
                && let Err(error) = self.inner.providers.get(provider)
            {
                warnings.insert(provider, error.to_string());
            }
        }
        threads.extend(saved.values().cloned().map(|thread| (thread, false)));
        Ok((threads, self.inner.providers.error_summary(warnings)))
    }
    pub(super) async fn remove_worktree(&self, path: String) -> Result<(), Failure> {
        let _exclusive = self.inner.worktree_access.write().await;
        let entries = self.worktree_list().await?;
        let entry = entries
            .iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                Failure::new(
                    "worktree_remove_failed",
                    "Bexが作成したワークツリーではありません。",
                )
            })?;
        if let Some(reason) = &entry.blocked_reason {
            return Err(Failure::new("worktree_remove_failed", reason));
        }
        self.inner
            .worktrees
            .remove(path, crate::worktrees::Removal::Manual)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }
    pub(super) async fn update_worktree_settings(
        &self,
        settings: agent_protocol::models::WorktreeSettings,
    ) -> anyhow::Result<agent_protocol::models::WorktreeSettings> {
        let previous = self.inner.worktrees.settings(None).await?;
        let saved = self.inner.worktrees.settings(Some(settings)).await?;
        if (
            previous.delete_unchanged,
            &previous.project_delete_unchanged,
        ) != (saved.delete_unchanged, &saved.project_delete_unchanged)
        {
            self.inner.worktree_cleanup.notify_one();
        }
        Ok(saved)
    }
    /// Resolves when changed cleanup rules call for a sweep before the next interval.
    pub(crate) async fn worktree_cleanup_requested(&self) {
        self.inner.worktree_cleanup.notified().await;
    }
    /// Remove idle checkouts that the project's default branch already contains.
    pub(crate) async fn cleanup_unchanged_worktrees(&self) -> anyhow::Result<()> {
        let settings = self.inner.worktrees.settings(None).await?;
        let projects = self.project_snapshot().await?;
        let candidates: Vec<_> = self
            .inner
            .worktrees
            .list()
            .await?
            .into_iter()
            .filter(|tree| std::path::Path::new(&tree.path).exists())
            .filter_map(|tree| {
                let project = projects
                    .project_for_workspace(&tree.path)
                    .map(str::to_owned);
                settings
                    .deletes_unchanged(project.as_deref())
                    .then_some((tree, project))
            })
            .collect();
        if candidates.is_empty() {
            return Ok(());
        }
        // Fetch before excluding session starts; refreshing a base can wait on the network.
        let mut bases = std::collections::HashMap::new();
        for (tree, _) in &candidates {
            if bases.contains_key(&tree.project_path) {
                continue;
            }
            let project = std::path::PathBuf::from(&tree.project_path);
            let base =
                tokio::task::spawn_blocking(move || crate::worktrees::integration_base(&project))
                    .await?;
            if let Err(error) = &base {
                tracing::warn!(target: "agent", operation = "host.worktree.cleanup", message = %format!("{error:#}"));
            }
            bases.insert(tree.project_path.clone(), base.ok());
        }
        let _exclusive = self.inner.worktree_access.write().await;
        let idle: std::collections::HashSet<_> = self
            .worktree_list()
            .await?
            .into_iter()
            .filter(|entry| entry.blocked_reason.is_none())
            .map(|entry| entry.path)
            .collect();
        for (tree, project) in candidates {
            let Some(Some(base)) = bases.get(&tree.project_path) else {
                continue;
            };
            if idle.contains(&tree.path)
                && let Err(error) = self
                    .inner
                    .worktrees
                    .remove(
                        tree.path,
                        crate::worktrees::Removal::Unchanged {
                            project,
                            base: base.clone(),
                        },
                    )
                    .await
            {
                tracing::warn!(target: "agent", operation = "host.worktree.cleanup", message = %error);
            }
        }
        Ok(())
    }
}

pub(super) fn worktree_active(
    status: agent_protocol::models::SessionStatus,
    turns: &[Arc<agent_protocol::models::Turn>],
    has_requests: bool,
) -> bool {
    agent_protocol::models::task_active(None, status)
        || has_requests
        || turns
            .iter()
            .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
}

#[cfg(test)]
pub(super) mod tests {
    pub(in crate::host_rpc::service) fn repository(
        directory: &std::path::Path,
    ) -> std::path::PathBuf {
        let repository = directory.join("repository");
        std::fs::create_dir(&repository).unwrap();
        crate::git::text(&repository, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        crate::git::text(
            &repository,
            &[
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
            ],
        )
        .unwrap();
        repository
    }

    #[tokio::test]
    async fn worktree_resolves_public_tasks_and_preserves_native_idle_warnings() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
        for warning in [None, Some("native idle warning")] {
            let directory = tempfile::tempdir().unwrap();
            let repository = repository(directory.path());
            let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
            Arc::get_mut(&mut agent).unwrap().idle_warning = warning;
            let host = HostRpcService::new(
                [agent.backend()],
                ProjectStore::new(directory.path().join("worktrees.json")),
            );
            host.inner
                .worktrees
                .settings(Some(agent_protocol::models::WorktreeSettings {
                    create_on_new_session: true,
                    ..Default::default()
                }))
                .await
                .unwrap();
            let checkout = host
                .inner
                .worktrees
                .prepare_selected(
                    repository.to_str().unwrap(),
                    String::new(),
                    true,
                    crate::worktrees::Observer::default(),
                )
                .await
                .unwrap();
            let response = agent
                .create(checkout.to_str().unwrap(), None, None)
                .await
                .unwrap();
            let public = agent_protocol::session::SessionRef::new(
                ProviderKind::Codex,
                "bex-task:managed".into(),
            )
            .unwrap();
            own(&host, &public, &response);
            let entries = host.worktree_list().await.unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].threads.len(), 1);
            assert_eq!(entries[0].threads[0].id, public);
            assert!(!entries[0].threads[0].active);
            assert_eq!(entries[0].blocked_reason.is_some(), warning.is_some());
            if let Some(warning) = warning {
                assert!(
                    entries[0]
                        .blocked_reason
                        .as_ref()
                        .unwrap()
                        .contains(warning)
                );
                assert!(host.remove_worktree(entries[0].path.clone()).await.is_err());
            } else {
                host.remove_worktree(entries[0].path.clone()).await.unwrap();
                assert!(!checkout.exists());
                assert!(
                    host.inner
                        .conversation
                        .storage
                        .lookup(&public)
                        .unwrap()
                        .is_some()
                );
            }
        }
    }

    #[tokio::test]
    async fn worktree_occupancy_uses_the_imported_status_when_source_history_is_incomplete() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
        use agent_protocol::{
            execution::TurnStatus,
            session::{HistoryReadKind, HistoryReadState, SessionRef},
        };
        for history in [HistoryReadKind::Incomplete, HistoryReadKind::Unavailable] {
            for status in [TurnStatus::Completed, TurnStatus::Running] {
                let directory = tempfile::tempdir().unwrap();
                let repository = repository(directory.path());
                let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
                let host = HostRpcService::new(
                    [agent.backend()],
                    ProjectStore::new(directory.path().join("worktrees.json")),
                );
                host.inner
                    .worktrees
                    .settings(Some(agent_protocol::models::WorktreeSettings {
                        create_on_new_session: true,
                        ..Default::default()
                    }))
                    .await
                    .unwrap();
                let checkout = host
                    .inner
                    .worktrees
                    .prepare_selected(
                        repository.to_str().unwrap(),
                        String::new(),
                        true,
                        crate::worktrees::Observer::default(),
                    )
                    .await
                    .unwrap();
                let mut response = agent
                    .create(checkout.to_str().unwrap(), None, None)
                    .await
                    .unwrap();
                response.thread.history_read_state =
                    Some(HistoryReadState::new(history.clone(), vec![]));
                response.thread.turns = Some(vec![Arc::new(agent_protocol::models::Turn {
                    id: "turn".into(),
                    status,
                    ..Default::default()
                })]);
                let native = response.thread.id.clone().unwrap();
                agent
                    .records
                    .lock()
                    .unwrap()
                    .insert(native.id.clone(), response.clone());
                let public =
                    SessionRef::new(ProviderKind::Claude, "bex-task:managed".into()).unwrap();
                own(&host, &public, &response);
                let entries = host.worktree_list().await.unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].threads.len(), 1);
                assert_eq!(entries[0].threads[0].id, public);
                assert_eq!(entries[0].threads[0].active, status == TurnStatus::Running);
                assert_eq!(
                    entries[0].blocked_reason.is_some(),
                    status == TurnStatus::Running
                );
                assert_eq!(
                    host.remove_worktree(entries[0].path.clone()).await.is_err(),
                    status == TurnStatus::Running
                );
                assert_eq!(checkout.exists(), status == TurnStatus::Running);
            }
        }
    }

    #[tokio::test]
    async fn worktree_keeps_idle_uncertainty_and_provider_failures_alongside_running_work() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(directory.path());
        let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        Arc::get_mut(&mut agent).unwrap().idle_warning = Some("native idle warning");
        let host = HostRpcService::new(
            [
                agent.backend(),
                Backend::unavailable(ProviderKind::Claude, "claude startup failed"),
            ],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        host.inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let checkout = host
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                String::new(),
                true,
                crate::worktrees::Observer::default(),
            )
            .await
            .unwrap();
        agent
            .create(checkout.to_str().unwrap(), None, None)
            .await
            .unwrap();
        let running = agent
            .create(checkout.to_str().unwrap(), None, None)
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        agent
            .records
            .lock()
            .unwrap()
            .get_mut(&running.id)
            .unwrap()
            .thread
            .status = agent_protocol::models::SessionStatus::Running;
        let client = host.open_session();
        let worktrees: Vec<agent_protocol::models::Worktree> = call(
            &host,
            client.id(),
            Call::ListWorktrees(agent_protocol::models::Empty {}),
        )
        .await
        .unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].threads.len(), 2);
        assert!(worktrees[0].threads.iter().any(|thread| thread.active));
        assert!(worktrees[0].threads.iter().any(|thread| !thread.active));
        assert_eq!(
            worktrees[0].blocked_reason.as_deref(),
            Some(
                "このワークツリーで作業を実行中です。完了または停止してから削除してください。\n稼働状況を確認できないため削除できません: claude startup failed\n稼働状況を確認できないため削除できません: native idle warning"
            )
        );
    }

    #[tokio::test]
    async fn worktree_block_reasons_keep_every_provider_failure_in_provider_order() {
        for listing_fails in [false, true] {
            use super::*;
            use crate::host_rpc::runtime::native_fixture::NativeAgent;
            use std::sync::atomic::Ordering;
            let directory = tempfile::tempdir().unwrap();
            let repository = repository(directory.path());
            for order in [
                [
                    ProviderKind::DeepSeek,
                    ProviderKind::Claude,
                    ProviderKind::Codex,
                ],
                [
                    ProviderKind::Claude,
                    ProviderKind::Codex,
                    ProviderKind::DeepSeek,
                ],
                [
                    ProviderKind::Codex,
                    ProviderKind::DeepSeek,
                    ProviderKind::Claude,
                ],
            ] {
                let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
                let host = HostRpcService::new(
                    order.map(|provider| {
                        if listing_fails && provider == ProviderKind::Codex {
                            a.backend()
                        } else {
                            Backend::unavailable(
                                provider,
                                format!("{} startup failed", provider.key()),
                            )
                        }
                    }),
                    ProjectStore::new(directory.path().join("worktrees.json")),
                );
                host.inner
                    .worktrees
                    .settings(Some(agent_protocol::models::WorktreeSettings {
                        create_on_new_session: true,
                        ..Default::default()
                    }))
                    .await
                    .unwrap();
                host.inner
                    .worktrees
                    .prepare_selected(
                        repository.to_str().unwrap(),
                        String::new(),
                        true,
                        crate::worktrees::Observer::default(),
                    )
                    .await
                    .unwrap();
                let expected = if listing_fails {
                    "稼働状況を確認できないため削除できません: codex list failed\nclaude startup failed\ndeepseek startup failed"
                } else {
                    "稼働状況を確認できないため削除できません: codex startup failed\nclaude startup failed\ndeepseek startup failed"
                };

                a.fail_list.store(1, Ordering::SeqCst);
                let entries = host.worktree_list().await.unwrap();
                assert!(!entries.is_empty());
                assert!(
                    entries
                        .iter()
                        .all(|entry| entry.blocked_reason.as_deref() == Some(expected))
                );
            }
        }
    }

    #[test]
    fn blockers_keep_all_occupancy_and_uncertainty_reasons_in_a_fixed_order() {
        assert_eq!(super::WorktreeBlockers::default().reason(), None);
        for warnings in [
            ["native idle warning", "codex startup failed"],
            ["codex startup failed", "native idle warning"],
        ] {
            let blockers = super::WorktreeBlockers {
                running: true,
                terminal: true,
                warnings: warnings.into_iter().map(str::to_owned).collect(),
            };
            assert_eq!(
                blockers.reason().as_deref(),
                Some(
                    "このワークツリーで作業を実行中です。完了または停止してから削除してください。\nこのワークツリーのターミナルを閉じてから削除してください。\n稼働状況を確認できないため削除できません: codex startup failed\n稼働状況を確認できないため削除できません: native idle warning"
                )
            );
        }
    }

    #[test]
    fn worktree_activity_includes_running_turns_and_unanswered_requests() {
        use agent_protocol::{execution::TurnStatus, models::SessionStatus};
        for status in [
            TurnStatus::Running,
            TurnStatus::Unknown,
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let turns = [std::sync::Arc::new(agent_protocol::models::Turn {
                status,
                ..Default::default()
            })];
            assert_eq!(
                super::worktree_active(SessionStatus::Idle, &turns, false),
                status == TurnStatus::Running
            );
            assert!(super::worktree_active(
                SessionStatus::Running,
                &turns,
                false
            ));
            assert!(super::worktree_active(
                SessionStatus::Preparing,
                &turns,
                false
            ));
            assert!(super::worktree_active(SessionStatus::Idle, &turns, true));
        }
    }

    #[tokio::test]
    async fn worktree_listing_blocks_only_checkouts_with_unverified_claude_activity() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        crate::git::text(&repository, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        crate::git::text(
            &repository,
            &[
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
            ],
        )
        .unwrap();
        let native = root.path().join("native");
        let project = native.join("projects/example");
        std::fs::create_dir_all(&project).unwrap();
        let claude = crate::providers::Claude::load(
            root.path().join("unused-cli"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("projects.json")),
        );
        service
            .inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let first = service
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                String::new(),
                true,
                crate::worktrees::Observer::default(),
            )
            .await
            .unwrap();
        let other = service
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                String::new(),
                true,
                crate::worktrees::Observer::default(),
            )
            .await
            .unwrap();
        let id = "12345678-1234-4234-8234-123456789abc";
        let transcript = project.join(format!("{id}.jsonl"));
        let history = include_str!("../../../tests/fixtures/claude-2.1.266.jsonl")
            .lines()
            .map(|line| {
                let mut entry: serde_json::Value = serde_json::from_str(line).unwrap();
                entry["cwd"] = serde_json::json!(first);
                // Another agent's `claude -p` run stays out of task lists, but
                // its unverifiable activity must still block removal.
                entry["entrypoint"] = serde_json::json!("sdk-cli");
                entry.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&transcript, history).unwrap();
        let entries = service.worktree_list().await.unwrap();
        let first_entry = entries
            .iter()
            .find(|entry| std::path::Path::new(&entry.path) == first)
            .unwrap();
        assert_eq!(first_entry.threads[0].id.id, id);
        assert!(!first_entry.threads[0].active);
        assert!(
            first_entry
                .blocked_reason
                .as_ref()
                .unwrap()
                .contains("native transcript does not expose external process activity")
        );
        let other_entry = entries
            .iter()
            .find(|entry| std::path::Path::new(&entry.path) == other)
            .unwrap();
        assert!(other_entry.blocked_reason.is_none());
        assert!(
            service
                .remove_worktree(first_entry.path.clone())
                .await
                .is_err()
        );
        assert!(first.is_dir());
        std::fs::remove_file(transcript).unwrap();
        assert!(
            service
                .worktree_list()
                .await
                .unwrap()
                .iter()
                .all(|entry| entry.blocked_reason.is_none())
        );
    }

    #[tokio::test]
    async fn failed_provider_preparation_keeps_the_owned_checkout_for_retry() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
        use agent_protocol::conversation::{InputContent, InputState, records};
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(directory.path());
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        agent.create_gate.close();
        let host = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        host.inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let client = host.open_session();
        let target =
            agent_protocol::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string())
                .unwrap();
        let input = uuid::Uuid::new_v4();
        let _: records::Opened = call(
            &host,
            client.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                provider: agent_protocol::session::ProviderKind::Codex,
                session: target.clone(),
                subscription: uuid::Uuid::new_v4(),
                cwd: Some(repository.to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: Some((
                    input.to_string().into(),
                    InputContent {
                        parts: vec![op::Input::Text {
                            text: "keep this input".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                )),
            }),
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if host
                    .inner
                    .conversation
                    .storage
                    .state(&target)
                    .unwrap()
                    .inputs
                    .values()
                    .any(|input| matches!(input.state, InputState::Failed(_)))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let saved = host.inner.conversation.storage.header(&target).unwrap();
        let checkout = saved.state.workspace.cwd().unwrap();
        assert_ne!(checkout, repository.to_str().unwrap());
        assert!(std::path::Path::new(checkout).is_dir());
        let entries = host.worktree_list().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].threads[0].id, target);
        assert!(!entries[0].threads[0].active);
        assert!(entries[0].blocked_reason.is_none());
    }

    #[tokio::test]
    async fn unchanged_cleanup_follows_each_projects_override() {
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(directory.path());
        let host = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        host.inner.projects.register(&repository).await.unwrap();
        let project = host.project_snapshot().await.unwrap().projects[0]
            .id
            .clone();
        let mut settings = agent_protocol::models::WorktreeSettings {
            create_on_new_session: true,
            delete_unchanged: true,
            project_delete_unchanged: [(project.clone(), false)].into(),
            ..Default::default()
        };
        host.update_worktree_settings(settings.clone())
            .await
            .unwrap();
        let kept = host
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                String::new(),
                true,
                crate::worktrees::Observer::default(),
            )
            .await
            .unwrap();
        host.cleanup_unchanged_worktrees().await.unwrap();
        assert!(
            kept.is_dir(),
            "the project's override disables the global rule"
        );
        settings.delete_unchanged = false;
        settings.project_delete_unchanged = [(project, true)].into();
        host.update_worktree_settings(settings).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            host.worktree_cleanup_requested(),
        )
        .await
        .expect("changed cleanup rules request a sweep");
        host.cleanup_unchanged_worktrees().await.unwrap();
        assert!(!kept.exists(), "the project's override enables cleanup");
    }
}
