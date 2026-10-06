use super::checkpoint::checkpoint_scope;
use super::{ExecutorContext, effect_command_id};
use crate::{
    Durability, EffectError, EffectHandler, EffectJob, LaunchOperation, LaunchStatus, SetupRequest,
    WorkspaceStrategy, WorktreeRequest,
};
use agent_domain::{
    Command, CommandId, EffectBody, EffectResult, Input, PreparationPhase, RunId, RunStatus,
    ThreadId, Workspace,
};
use futures_util::future::BoxFuture;

/// Why a workspace could not be prepared.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PrepareError {
    /// The preparation itself failed; the run fails with this message.
    #[error("Workspace preparation failed during {}: {cause}", operation.as_str().replace('-', " "))]
    Failed {
        operation: LaunchOperation,
        cause: String,
    },
    /// The runtime could not record progress; retrying is safe.
    #[error("{0}")]
    Retry(String),
}

fn failed(operation: LaunchOperation) -> impl FnOnce(String) -> PrepareError {
    move |cause| PrepareError::Failed { operation, cause }
}

/// The deferred run a preparation reports its phases to, and the effect that
/// runs it (which names each progress command).
pub struct PreparedRun<'a> {
    pub run: &'a RunId,
    pub effect: &'a str,
}

/// Shows the preparation phase on the run's preparation row (T3 prepared-run.progress).
async fn progress(
    context: &ExecutorContext,
    thread: &ThreadId,
    prepared: Option<&PreparedRun<'_>>,
    phase: PreparationPhase,
) -> Result<(), PrepareError> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    let id = CommandId::new(format!(
        "{}:progress:{}",
        effect_command_id(prepared.effect),
        match phase {
            PreparationPhase::Worktree => "worktree",
            PreparationPhase::Setup => "setup",
        }
    ))
    .expect("derived id");
    context
        .dispatch(
            thread,
            id,
            Command::PreparedRunProgress {
                run: prepared.run.clone(),
                phase,
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| PrepareError::Failed {
            operation: LaunchOperation::UpdateThread,
            cause: error.to_string(),
        })
}

/// Provisions the thread's workspace for its launch (reusing a worktree an earlier
/// attempt recorded), binds it and its checkpoint scope, and runs the project setup.
pub async fn prepare_workspace(
    context: &ExecutorContext,
    thread: &ThreadId,
    prepared: Option<PreparedRun<'_>>,
) -> Result<(), PrepareError> {
    let run = prepared.as_ref().map(|prepared| prepared.run);
    let retry = |error: crate::RuntimeError| PrepareError::Retry(error.to_string());
    let state = context.registry.state(thread).await.map_err(retry)?;
    let current = state
        .thread
        .as_ref()
        .ok_or_else(|| PrepareError::Retry(format!("thread {thread} does not exist")))?;
    let Some(project) = context.ops.project(&current.project) else {
        return Err(PrepareError::Failed {
            operation: LaunchOperation::ResolveProject,
            cause: "Project no longer exists.".into(),
        });
    };
    let lookup = thread.clone();
    let record = context
        .store
        .blocking(move |store| store.thread_launch(&lookup))
        .await
        .map_err(|error| PrepareError::Retry(error.to_string()))?;
    let strategy = record
        .as_ref()
        .map_or(WorkspaceStrategy::Root { branch: None }, |record| {
            record.strategy.clone()
        });
    let observe_completion = matches!(strategy, WorkspaceStrategy::Worktree { .. });
    let workspace = match (&record, strategy) {
        (Some(record), _) if record.worktree_path.is_some() => {
            let path = record.worktree_path.clone().unwrap_or_default();
            Workspace {
                cwd: path.clone(),
                worktree_path: Some(path),
                branch: record.branch.clone(),
            }
        }
        (
            Some(record),
            WorkspaceStrategy::Worktree {
                base_ref,
                branch,
                start_from_origin,
            },
        ) => {
            progress(
                context,
                thread,
                prepared.as_ref(),
                PreparationPhase::Worktree,
            )
            .await?;
            let created = context
                .ops
                .create_worktree(WorktreeRequest {
                    thread: thread.clone(),
                    project: current.project.clone(),
                    project_root: project.root.clone(),
                    base_ref,
                    branch,
                    start_from_origin,
                })
                .await
                .map_err(failed(LaunchOperation::ProvisionWorktree))?;
            let now = context.registry.context().clock.now().millis();
            if let Err(error) = context
                .store
                .record_launch_worktree(
                    &record.command,
                    &created.path,
                    created.branch.as_deref(),
                    now,
                )
                .await
            {
                // Unrecorded, a retry would check out a second worktree beside it.
                if let Err(removal) = context
                    .ops
                    .remove_worktree(project.root.clone(), created.path.clone())
                    .await
                {
                    tracing::warn!(%thread, path = %created.path, %removal,
                        "could not remove an unrecorded thread worktree");
                }
                return Err(PrepareError::Retry(error.to_string()));
            }
            Workspace {
                cwd: created.path.clone(),
                worktree_path: Some(created.path),
                branch: created.branch,
            }
        }
        (_, WorkspaceStrategy::ExistingWorktree { path, branch }) => Workspace {
            cwd: path.clone(),
            worktree_path: Some(path),
            branch,
        },
        (_, WorkspaceStrategy::Root { branch })
        | (None, WorkspaceStrategy::Worktree { branch, .. }) => {
            current.workspace.clone().unwrap_or(Workspace {
                cwd: project.root.clone(),
                worktree_path: None,
                branch,
            })
        }
    };
    if current.workspace.as_ref() != Some(&workspace) {
        let _binding = context.workspaces.bind().await;
        context
            .input(
                thread,
                Input::Workspace {
                    workspace: Some(workspace.clone()),
                },
            )
            .await
            .map_err(retry)?;
    }
    let scope = checkpoint_scope(thread, &workspace.cwd);
    if state.checkpoint_scope.as_ref() != Some(&scope) {
        context
            .input(
                thread,
                Input::CheckpointScope {
                    run: None,
                    attempt: None,
                    scope: Some(scope.clone()),
                },
            )
            .await
            .map_err(retry)?;
    }
    if let Some(run) = run
        && state
            .runs
            .iter()
            .any(|candidate| &candidate.id == run && candidate.checkpoint_scope.is_none())
    {
        context
            .input(
                thread,
                Input::CheckpointScope {
                    run: Some(run.clone()),
                    attempt: None,
                    scope: Some(scope),
                },
            )
            .await
            .map_err(retry)?;
    }
    // A setup that completed is not run again by a retried preparation.
    if record.as_ref().is_some_and(|record| record.prepared) {
        return Ok(());
    }
    progress(context, thread, prepared.as_ref(), PreparationPhase::Setup).await?;
    context
        .ops
        .run_setup(SetupRequest {
            thread: thread.clone(),
            project: current.project.clone(),
            project_root: project.root,
            cwd: workspace.cwd,
            observe_completion,
        })
        .await
        .map_err(failed(LaunchOperation::RunSetupScript))?;
    if let Some(record) = &record {
        let now = context.registry.context().clock.now().millis();
        context
            .store
            .set_launch_status(&record.command, LaunchStatus::Prepared, now)
            .await
            .map_err(|error| PrepareError::Retry(error.to_string()))?;
    }
    Ok(())
}

/// Prepares a deferred run's workspace, then releases the run to its provider.
/// A failed preparation fails the run; `RetryPrepared` emits this effect again.
pub(crate) struct PrepareWorkspace(pub(crate) ExecutorContext);

impl EffectHandler for PrepareWorkspace {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }

    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::PrepareWorkspace { run } = &job.effect.body else {
                return Err(EffectError::Permanent("not a workspace preparation".into()));
            };
            let context = &self.0;
            let state = context.state(&job.thread).await?;
            if !state
                .runs
                .iter()
                .any(|candidate| &candidate.id == run && candidate.status == RunStatus::Preparing)
            {
                return Ok(None);
            }
            let prepared = PreparedRun {
                run,
                effect: &job.effect.id,
            };
            let command = match prepare_workspace(context, &job.thread, Some(prepared)).await {
                Ok(()) => Command::ReleasePrepared { run: run.clone() },
                Err(PrepareError::Retry(error)) if job.will_retry => {
                    return Err(EffectError::Retryable(error));
                }
                // The last attempt fails the run so `RetryPrepared` can run it again.
                Err(PrepareError::Retry(cause)) => Command::FailPrepared {
                    run: run.clone(),
                    message: PrepareError::Failed {
                        operation: LaunchOperation::UpdateThread,
                        cause,
                    }
                    .to_string(),
                },
                Err(error) => {
                    tracing::warn!(thread = %job.thread, %run, %error, "workspace preparation failed");
                    Command::FailPrepared {
                        run: run.clone(),
                        message: error.to_string(),
                    }
                }
            };
            context
                .dispatch(&job.thread, effect_command_id(&job.effect.id), command)
                .await
                .map_err(|error| EffectError::Retryable(error.to_string()))?;
            Ok(None)
        })
    }
}
