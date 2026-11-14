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
            .headers(true)
            .map_err(super::super::runtime::storage_failure)?
            .into_iter()
            .map(|(id, header)| {
                let waiting = header.details.activity.facts().waiting;
                let thread = agent_protocol::conversation::records::summary(&id, &header);
                (id, (thread, waiting))
            })
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
            let active = agent_protocol::models::task_active(None, thread.status)
                || thread
                    .id
                    .as_ref()
                    .and_then(|id| saved.get(id))
                    .is_some_and(|(_, waiting)| *waiting);
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
        saved: &std::collections::BTreeMap<agent_protocol::session::SessionRef, (Thread, bool)>,
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
        for (thread, _) in saved.values() {
            if let Some(provider) = thread.active_provider
                && let Err(error) = self.inner.providers.get(provider)
            {
                warnings.insert(provider, error.to_string());
            }
        }
        threads.extend(saved.values().map(|(thread, _)| (thread.clone(), false)));
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

#[cfg(test)]
pub(super) mod tests;
