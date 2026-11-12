use super::*;
use std::path::Path;
use tokio_util::sync::CancellationToken;

impl HostRpcService {
    pub(super) async fn cleanup_cancelled_workspace(&self, id: &SessionRef) -> Result<(), Failure> {
        let Some(row) = self.inner.conversations.get(id)? else {
            return Ok(());
        };
        let Some(startup) = &row.startup else {
            return Ok(());
        };
        if !row.bindings.is_empty()
            || startup.preparation.status != PreparationStatus::Interrupted
            || !matches!(startup.workspace, Some(WorkspaceStrategy::Worktree { .. }))
            || row.cwd == startup.project_cwd
        {
            return Ok(());
        }
        if let Some(handle) = row
            .workspace_setup
            .as_ref()
            .and_then(|setup| setup.terminal_handle.as_ref())
        {
            self.inner.terminals.stop_setup(handle).await;
        }
        self.inner
            .worktrees
            .discard(row.cwd)
            .await
            .map_err(|error| Failure::new("worktree_cleanup_failed", error))?;
        self.change_preparation(id, StartupChange::ResetWorkspace)
            .await?;
        Ok(())
    }
    pub(in crate::host_rpc::service) async fn workspace_options(
        &self,
        cwd: &str,
    ) -> Result<agent_protocol::workspace::WorkspaceOptions, Failure> {
        let settings = self
            .inner
            .worktrees
            .settings(None)
            .await
            .map_err(|error| Failure::new("worktree_settings_failed", error))?;
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || {
            let root = Path::new(&cwd);
            let refs = crate::git::text(
                root,
                &[
                    "for-each-ref",
                    "--format=%(refname:short)",
                    "refs/heads",
                    "refs/remotes",
                    "refs/tags",
                ],
            );
            let is_repository = refs.is_ok();
            let refs = refs
                .unwrap_or_default()
                .lines()
                .take(2000)
                .map(str::to_owned)
                .collect();
            let entries = crate::git::text(root, &["worktree", "list", "--porcelain", "-z"])
                .unwrap_or_default();
            let worktrees = entries
                .split("\0\0")
                .filter_map(|entry| {
                    entry
                        .strip_prefix("worktree ")?
                        .split('\0')
                        .next()
                        .map(str::to_owned)
                })
                .collect();
            agent_protocol::workspace::WorkspaceOptions {
                is_repository,
                default_worktree: settings.create_on_new_session,
                default_base: if is_repository {
                    crate::worktrees::default_base(root).unwrap_or_else(|_| "HEAD".into())
                } else {
                    String::new()
                },
                refs,
                worktrees,
            }
        })
        .await
        .map_err(|error| Failure::new("workspace_options_failed", error))
    }
    pub(super) fn setup_update(
        &self,
        id: &SessionRef,
        update: impl FnOnce(&mut WorkspaceSetup),
    ) -> Result<(), Failure> {
        if let Some(setup) = self.inner.conversations.update_setup(id, update)? {
            self.inner
                .router
                .session_change(id, SessionChange::WorkspaceSetup { setup: Some(setup) });
        }
        Ok(())
    }

    pub(super) fn finish_setup_if_ready(&self, id: &SessionRef) -> Result<(), Failure> {
        let mut ended = None;
        self.setup_update(id, |setup| {
            if setup.phase == SetupPhase::Running
                && setup.stages.iter().all(|stage| {
                    !matches!(
                        stage.status,
                        SetupStageStatus::Pending | SetupStageStatus::Running
                    )
                })
            {
                let at = now_ms();
                setup.finish(SetupPhase::Done, None, at);
                ended = Some(at);
            }
        })?;
        if let Some(ended) = ended {
            self.expire_setup(id.clone(), ended);
        }
        Ok(())
    }

    pub(super) fn expire_current_setup(&self, id: &SessionRef) -> Result<(), Failure> {
        if let Some(ended) = self
            .inner
            .conversations
            .get(id)?
            .and_then(|row| row.workspace_setup)
            .and_then(|setup| setup.ended_at_ms)
        {
            self.expire_setup(id.clone(), ended);
        }
        Ok(())
    }

    fn expire_setup(&self, id: SessionRef, ended: u64) {
        let owner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let Some(inner) = owner.upgrade() else { return };
            let service = HostRpcService { inner };
            let Ok(Some(row)) = service.inner.conversations.get(&id) else {
                return;
            };
            if let Ok((_, true)) = service
                .inner
                .conversations
                .update_startup(&id, StartupChange::ExpireSetup { ended_at: ended })
            {
                if let Some(handle) = row.workspace_setup.and_then(|setup| setup.terminal_handle) {
                    service
                        .inner
                        .terminals
                        .forget_setup(&service.inner.router, &handle);
                }
                service
                    .inner
                    .router
                    .session_change(&id, SessionChange::WorkspaceSetup { setup: None });
            }
        });
    }

    pub(super) async fn prepare_task_workspace(
        &self,
        row: &Conversation,
        cancel: CancellationToken,
    ) -> Result<(), Failure> {
        let startup = row.startup.as_ref().unwrap();
        let cwd = match &startup.workspace {
            Some(WorkspaceStrategy::Worktree {
                base_ref,
                start_from_origin,
            }) => {
                self.change_preparation(&row.id, StartupChange::Worktree)
                    .await?;
                let service = self.clone();
                let id = row.id.clone();
                let observer_cancel = cancel.clone();
                let observer = crate::worktrees::Observer {
                    cancel,
                    notify: Arc::new(move |progress| {
                        if observer_cancel.is_cancelled() {
                            return Err(anyhow::anyhow!("workspace preparation cancelled"));
                        }
                        service
                            .setup_update(&id, |setup| match progress {
                                crate::worktrees::Progress::Stage(id, status) => {
                                    setup.stage(id, status, now_ms())
                                }
                                crate::worktrees::Progress::Base(base) => {
                                    setup.base_ref = Some(base)
                                }
                                crate::worktrees::Progress::Checkout(percent) => {
                                    if let Some(stage) = setup
                                        .stages
                                        .iter_mut()
                                        .find(|stage| stage.id == SetupStageId::Checkout)
                                    {
                                        stage.percent = Some(percent);
                                    }
                                }
                                crate::worktrees::Progress::Created { path, branch } => {
                                    setup.worktree_path = Some(path);
                                    setup.branch = Some(branch);
                                }
                            })
                            .map_err(|error| anyhow::anyhow!(error.message))
                    }),
                };
                self.inner
                    .worktrees
                    .prepare_selected(
                        &startup.project_cwd,
                        base_ref.clone(),
                        *start_from_origin,
                        observer,
                    )
                    .await
                    .map_err(|error| Failure::new("worktree_creation_failed", error))?
                    .to_string_lossy()
                    .into_owned()
            }
            Some(WorkspaceStrategy::ExistingWorktree { path }) => {
                let original = startup.project_cwd.clone();
                let path = path.clone();
                let cwd = tokio::task::spawn_blocking(move || validate_existing(&original, &path))
                    .await
                    .map_err(|error| Failure::new("worktree_unavailable", error))?
                    .map_err(|error| Failure::new("worktree_unavailable", error))?;
                self.setup_update(&row.id, |setup| {
                    setup.worktree_path = Some(cwd.clone());
                    setup.stage(SetupStageId::Fetch, SetupStageStatus::Skipped, now_ms());
                    setup.stage(SetupStageId::Checkout, SetupStageStatus::Skipped, now_ms());
                })?;
                cwd
            }
            _ => {
                if Path::new(&startup.project_cwd) == self.inner.projects.chat_directory() {
                    tokio::fs::create_dir_all(&startup.project_cwd)
                        .await
                        .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
                }
                dunce::canonicalize(&startup.project_cwd)
                    .map_err(|error| Failure::new("workspace_unavailable", error))?
                    .to_string_lossy()
                    .into_owned()
            }
        };
        self.change_preparation(&row.id, StartupChange::Workspace { cwd })
            .await?;
        Ok(())
    }
}

fn validate_existing(original: &str, path: &str) -> anyhow::Result<String> {
    let original = dunce::canonicalize(original)?;
    let selected = dunce::canonicalize(path)?;
    let common = |cwd: &Path| -> anyhow::Result<_> {
        let git = crate::git::text(cwd, &["rev-parse", "--git-common-dir"])?;
        Ok(dunce::canonicalize(cwd.join(git.trim()))?)
    };
    anyhow::ensure!(
        common(&original)? == common(&selected)?,
        "worktree belongs to another repository"
    );
    Ok(selected.to_string_lossy().into_owned())
}
