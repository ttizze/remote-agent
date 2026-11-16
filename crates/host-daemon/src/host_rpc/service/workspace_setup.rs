//! Workspace checkout and script execution publish through the conversation owner.
use super::super::runtime::now;
use super::*;
use agent_protocol::workspace::SetupChange;
use agent_protocol::{
    session::SessionRef,
    workspace::{
        SetupStageId, SetupStageStatus, WorkspacePreparation, WorkspaceSetup, WorkspaceStrategy,
    },
};
use std::path::Path;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod script;
#[cfg(test)]
mod tests;

fn workspace_setup(strategy: &WorkspaceStrategy, started_at: u64) -> Option<WorkspaceSetup> {
    match strategy {
        WorkspaceStrategy::Root => None,
        WorkspaceStrategy::Worktree { base_ref, .. } => {
            Some(WorkspaceSetup::new(Some(base_ref.clone()), started_at))
        }
        WorkspaceStrategy::ExistingWorktree { .. } => Some(WorkspaceSetup::new(None, started_at)),
    }
}

impl HostRpcService {
    pub(in crate::host_rpc) async fn cleanup_setup_resources(
        &self,
        path: Option<String>,
        branch: Option<String>,
        terminal: Option<&str>,
    ) -> Result<(), Failure> {
        if let Some(handle) = terminal {
            self.inner.terminals.stop_setup(handle).await;
        }
        if let Some(path) = path {
            self.inner
                .worktrees
                .discard(path, branch)
                .await
                .map_err(|error| Failure::new("worktree_cleanup_failed", error))?;
        }
        Ok(())
    }

    pub(in crate::host_rpc) async fn workspace_preparation(
        &self,
        project_cwd: String,
        strategy: Option<WorkspaceStrategy>,
    ) -> Result<WorkspacePreparation, Failure> {
        let settings = self
            .inner
            .worktrees
            .settings(None)
            .await
            .map_err(|error| Failure::new("worktree_settings_failed", error))?;
        let strategy = strategy
            .or_else(|| {
                settings
                    .create_on_new_session
                    .then_some(WorkspaceStrategy::Worktree {
                        base_ref: String::new(),
                        start_from_origin: true,
                    })
            })
            .unwrap_or(WorkspaceStrategy::Root);
        let mut setup = workspace_setup(&strategy, now().max(0) as u64);
        if let Some(setup) = &mut setup {
            let projects = self.project_snapshot().await?;
            setup.script = projects
                .project_for_workspace(&project_cwd)
                .and_then(|id| settings.project_setup_scripts.get(id))
                .cloned()
                .or(settings.setup_script)
                .filter(|script| !script.command.trim().is_empty());
        }
        Ok(WorkspacePreparation {
            run_id: None,
            project_cwd,
            strategy: Some(strategy),
            generation: Uuid::nil(),
            created_worktree: false,
            visible: setup.is_some(),
            setup,
        })
    }

    pub(in crate::host_rpc) async fn setup_update(
        &self,
        id: &SessionRef,
        generation: Uuid,
        update: SetupChange,
    ) -> Result<(), Failure> {
        let output = matches!(&update, SetupChange::Output(_));
        let changed = self
            .inner
            .conversation
            .storage
            .setup(id, generation, update, now())
            .map_err(super::super::runtime::storage_failure)?;
        if !changed || output {
            return Ok(());
        }
        if let Some(workspace) = self
            .inner
            .conversation
            .storage
            .thread_header(id)
            .map_err(super::super::runtime::storage_failure)?
            .and_then(|thread| thread.workspace_launch)
            .filter(|workspace| workspace.generation == generation)
        {
            self.schedule_setup_expiry(id.clone(), *workspace);
        }
        Ok(())
    }

    pub(in crate::host_rpc) fn schedule_setup_events(
        &self,
        events: &[agent_protocol::orchestration::StoredEvent],
    ) {
        for saved in events {
            if let agent_protocol::orchestration::Change::Thread(thread) = &saved.event.change
                && let Some(workspace) = &thread.workspace_launch
                && workspace
                    .setup
                    .as_ref()
                    .is_some_and(|setup| setup.ended_at_ms.is_some())
                && let Ok(id) = agent_protocol::orchestration::shell::public_session_ref(&thread.id)
            {
                self.schedule_setup_expiry(id, *workspace.clone());
            }
        }
    }

    pub(in crate::host_rpc) fn schedule_setup_expiry(
        &self,
        id: SessionRef,
        workspace: WorkspacePreparation,
    ) {
        let Some(ended_at) = workspace
            .setup
            .as_ref()
            .filter(|_| workspace.visible)
            .and_then(|setup| setup.ended_at_ms)
        else {
            return;
        };
        let remaining = ended_at
            .saturating_add(30_000)
            .saturating_sub(now().max(0) as u64);
        let owner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(remaining)).await;
            if let Some(inner) = owner.upgrade() {
                if let Err(error) = inner.conversation.storage.setup(
                    &id,
                    workspace.generation,
                    SetupChange::Expire { ended_at },
                    now(),
                ) {
                    tracing::warn!(target:"agent",operation="workspace.expire",%error);
                }
                if let Some(handle) = workspace.setup.and_then(|setup| setup.terminal_handle) {
                    inner.terminals.forget_setup(&inner.router, &handle);
                }
            }
        });
    }

    pub(in crate::host_rpc) fn restore_setup_expiry(&self) {
        let owner = Arc::downgrade(&self.inner);
        let storage = self.inner.conversation.storage.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || storage.thread_headers()).await;
            match result {
                Ok(Ok(headers)) => {
                    if let Some(inner) = owner.upgrade() {
                        let service = HostRpcService { inner };
                        for header in headers {
                            if let Some(workspace) = header.workspace_launch
                                && let Ok(id) =
                                    agent_protocol::orchestration::shell::public_session_ref(
                                        &header.id,
                                    )
                            {
                                service.schedule_setup_expiry(id, *workspace);
                            }
                        }
                    }
                }
                result => {
                    tracing::warn!(target:"agent",operation="workspace.restore_expiry",?result)
                }
            }
        });
    }

    pub(in crate::host_rpc) async fn prepare_task_workspace(
        &self,
        target: &SessionRef,
        action: Uuid,
        cancel: CancellationToken,
    ) -> Result<(String, bool), Failure> {
        let mut workspace = self
            .inner
            .conversation
            .storage
            .thread_header(target)
            .map_err(super::super::runtime::storage_failure)?
            .and_then(|thread| thread.workspace_launch)
            .map(|workspace| *workspace)
            .filter(|workspace| workspace.generation == action)
            .ok_or_else(|| Failure::new("preparation_interrupted", "Preparation cancelled"))?;
        let strategy = workspace
            .strategy
            .clone()
            .unwrap_or(WorkspaceStrategy::Root);
        let previous = workspace.setup.take();
        let retained = previous
            .as_ref()
            .filter(|_| !matches!(strategy, WorkspaceStrategy::Root))
            .and_then(|setup| setup.worktree_path.clone())
            .filter(|path| Path::new(path).exists());

        workspace.created_worktree =
            matches!(strategy, WorkspaceStrategy::Worktree { .. }) && retained.is_none();
        let mut setup = workspace_setup(&strategy, now().max(0) as u64);
        if let Some(setup) = &mut setup {
            setup.script = previous.as_ref().and_then(|setup| setup.script.clone());
        }
        workspace.strategy = Some(strategy.clone());
        workspace.generation = action;
        workspace.visible = setup.is_some();
        workspace.setup = setup;
        self.setup_update(
            target,
            action,
            SetupChange::Begin {
                workspace: Box::new(workspace.clone()),
            },
        )
        .await?;
        let observer = {
            let storage = self.inner.conversation.storage.clone();
            let id = target.clone();
            let cancelled = cancel.clone();
            crate::worktrees::Observer {
                cancel,
                notify: Arc::new(move |progress| {
                    anyhow::ensure!(!cancelled.is_cancelled(), "workspace preparation cancelled");
                    storage
                        .setup(&id, action, progress.into(), now())
                        .map_err(anyhow::Error::msg)
                        .map(drop)
                }),
            }
        };
        if let Some(path) = retained {
            let branch = previous
                .as_ref()
                .and_then(|setup| setup.branch.clone())
                .unwrap_or_default();
            observer
                .send(crate::worktrees::Progress::Created {
                    path: path.clone(),
                    branch,
                })
                .map_err(|error| Failure::new("worktree_creation_failed", error))?;
            for id in [SetupStageId::Fetch, SetupStageId::Checkout] {
                self.setup_update(
                    target,
                    action,
                    SetupChange::Stage {
                        id,
                        status: if matches!(strategy, WorkspaceStrategy::ExistingWorktree { .. }) {
                            SetupStageStatus::Skipped
                        } else {
                            SetupStageStatus::Done
                        },
                        exit_code: None,
                        error: None,
                    },
                )
                .await?;
            }
            return Ok((path, false));
        }
        match strategy {
            WorkspaceStrategy::Worktree {
                base_ref,
                start_from_origin,
            } => {
                let path = self
                    .inner
                    .worktrees
                    .prepare_selected(
                        &workspace.project_cwd,
                        base_ref,
                        start_from_origin,
                        observer,
                    )
                    .await
                    .map_err(|error| Failure::new("worktree_creation_failed", error))?;
                Ok((path.to_string_lossy().into_owned(), true))
            }
            WorkspaceStrategy::ExistingWorktree { path } => {
                let original = workspace.project_cwd;
                let path = tokio::task::spawn_blocking(move || validate_existing(&original, &path))
                    .await
                    .map_err(|error| Failure::new("worktree_unavailable", error))?
                    .map_err(|error| Failure::new("worktree_unavailable", error))?;
                self.setup_update(
                    target,
                    action,
                    SetupChange::Created {
                        path: path.clone(),
                        branch: String::new(),
                    },
                )
                .await?;
                for id in [SetupStageId::Fetch, SetupStageId::Checkout] {
                    self.setup_update(
                        target,
                        action,
                        SetupChange::Stage {
                            id,
                            status: SetupStageStatus::Skipped,
                            exit_code: None,
                            error: None,
                        },
                    )
                    .await?;
                }
                Ok((path, false))
            }
            WorkspaceStrategy::Root => {
                let path = tokio::fs::canonicalize(&workspace.project_cwd)
                    .await
                    .map_err(|error| Failure::new("workspace_unavailable", error))?;
                if !path.is_dir() {
                    return Err(Failure::new(
                        "workspace_unavailable",
                        "Workspace is not a directory",
                    ));
                }
                Ok((
                    dunce::simplified(&path).to_string_lossy().into_owned(),
                    false,
                ))
            }
        }
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
