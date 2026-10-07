use super::checkpoint::checkpoint_scope;
use super::{ExecutorContext, effect_command_id};
use crate::{
    Durability, EffectError, EffectHandler, EffectJob, LaunchOperation, LaunchRecord, LaunchStatus,
    SetupEvent, SetupProgress, SetupRequest, SetupRun, SetupTracker, TextGenerationRequest,
    WorkspaceStrategy, WorktreeRequest, branch_name_output_schema, branch_name_prompt,
    generated_branch_name, is_temporary_worktree_branch,
};
use agent_domain::{
    BranchNamingMode, Command, CommandId, EffectBody, EffectResult, Input, PreparationPhase, RunId,
    RunStatus, ThreadId, Workspace, WorktreeSetupPhase, WorktreeSetupScript, WorktreeSetupStageId,
    WorktreeSetupStageStatus,
};
use futures_util::future::BoxFuture;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Why a workspace could not be prepared.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PrepareError {
    /// The preparation itself failed; the run fails with this message.
    #[error("Workspace preparation failed during {}: {cause}", operation.as_str().replace('-', " "))]
    Failed {
        operation: LaunchOperation,
        cause: String,
    },
    /// The setup was cancelled from its card.
    #[error("Worktree setup cancelled.")]
    Cancelled,
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

/// A prepared workspace whose setup card, if tracked, waits for the turn to start.
#[must_use]
pub struct Prepared {
    tracking: Option<(Arc<SetupTracker>, ThreadId)>,
    /// An asynchronous setup script still running.
    pending: Option<BoxFuture<'static, Option<i32>>>,
}

impl Prepared {
    /// Right before the turn is released: a late cancel can no longer roll back.
    pub fn starting(&self) {
        if let Some((setups, thread)) = &self.tracking {
            setups.mark_uncancellable(thread);
            setups.stage_status(
                thread,
                WorktreeSetupStageId::Agent,
                WorktreeSetupStageStatus::Running,
                None,
            );
        }
    }

    /// The turn was released; the card settles once an asynchronous setup exits.
    pub fn started(self) {
        let Some((setups, thread)) = self.tracking else {
            return;
        };
        setups.stage_status(
            &thread,
            WorktreeSetupStageId::Agent,
            WorktreeSetupStageStatus::Done,
            None,
        );
        let pending = self.pending;
        tokio::spawn(async move {
            if let Some(completion) = pending {
                let code = completion.await;
                setups.stage_status(
                    &thread,
                    WorktreeSetupStageId::SetupScript,
                    setup_status(code),
                    Some(Some(exit_detail(code))),
                );
            }
            setups.finish(&thread, WorktreeSetupPhase::Done, None);
        });
    }
}

fn setup_status(code: Option<i32>) -> WorktreeSetupStageStatus {
    if code == Some(0) {
        WorktreeSetupStageStatus::Done
    } else {
        WorktreeSetupStageStatus::Failed
    }
}

fn exit_code(code: Option<i32>) -> String {
    code.map_or("no exit code".into(), |code| code.to_string())
}

fn exit_detail(code: Option<i32>) -> String {
    format!("exited with {}", exit_code(code))
}

/// Shows the preparation phase on the run's preparation row.
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

/// A worktree this preparation checked out, and whether the thread recorded it.
#[derive(Default)]
struct Checkout {
    path: Option<String>,
    recorded: bool,
    /// The Host is checking it out.
    creating: bool,
}

/// Provisions the thread's workspace for its launch (reusing a worktree an earlier
/// attempt recorded), binds it and its checkpoint scope, and runs the project
/// setup. A launch that prepares a worktree is tracked on its setup card and can
/// be cancelled from it until the turn starts.
pub async fn prepare_workspace(
    context: &ExecutorContext,
    thread: &ThreadId,
    prepared: Option<PreparedRun<'_>>,
) -> Result<Prepared, PrepareError> {
    let lookup = thread.clone();
    let record = context
        .store
        .blocking(move |store| store.thread_launch(&lookup))
        .await
        .map_err(|error| PrepareError::Retry(error.to_string()))?;
    let Some((record, base_ref, branch)) = record.and_then(|record| match &record.strategy {
        WorkspaceStrategy::Worktree {
            base_ref, branch, ..
        } => {
            let (base_ref, branch) = (base_ref.clone(), branch.clone());
            Some((record, base_ref, branch))
        }
        _ => None,
    }) else {
        return prepare(
            context,
            thread,
            prepared,
            None,
            &Mutex::default(),
            &CancellationToken::new(),
        )
        .await;
    };
    let setups = context.setups.clone();
    let stages: &[WorktreeSetupStageId] = if record.worktree_path.is_some() {
        &[
            WorktreeSetupStageId::SetupScript,
            WorktreeSetupStageId::Agent,
        ]
    } else {
        &[
            WorktreeSetupStageId::Fetch,
            WorktreeSetupStageId::Checkout,
            WorktreeSetupStageId::SetupScript,
            WorktreeSetupStageId::Agent,
        ]
    };
    let cancel = CancellationToken::new();
    setups.begin(
        thread,
        record.branch.clone().or(branch),
        Some(base_ref),
        stages,
        Some(cancel.clone()),
    );
    let checkout = Mutex::new(Checkout::default());
    let result = {
        let preparation = prepare(context, thread, prepared, Some(&setups), &checkout, &cancel);
        tokio::pin!(preparation);
        tokio::select! {
            biased;
            result = &mut preparation => result,
            () = cancel.cancelled() => {
                // A checkout in progress stops its Git command and removes what
                // it created; wait for that before cleaning up, so nothing it
                // does outlives the cancellation or escapes the cleanup.
                if checkout.lock().unwrap().creating {
                    let _ = (&mut preparation).await;
                }
                Err(PrepareError::Cancelled)
            }
        }
    };
    let error = match result {
        Ok(mut done) => {
            done.tracking = Some((setups, thread.clone()));
            return Ok(done);
        }
        Err(error) => error,
    };
    let cancelled = error == PrepareError::Cancelled;
    let checkout = checkout
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    // A cancelled setup leaves nothing behind. A failed one keeps a worktree the
    // thread recorded, so a retry reuses it, and removes one it never recorded.
    if let Some(path) = checkout.path.filter(|_| cancelled || !checkout.recorded) {
        abandon(context, thread, &record, &path).await;
    }
    setups.finish(
        thread,
        if cancelled {
            WorktreeSetupPhase::Cancelled
        } else {
            WorktreeSetupPhase::Failed
        },
        (!cancelled).then(|| error.to_string()).as_deref(),
    );
    Err(error)
}

/// Removes a worktree the launch gives up; the thread forgets it only once it
/// is gone.
async fn abandon(context: &ExecutorContext, thread: &ThreadId, record: &LaunchRecord, path: &str) {
    if let Err(error) = context.ops.cleanup_terminals(thread.clone()).await {
        tracing::warn!(%thread, %error, "could not stop the abandoned setup");
    }
    let Some(project) = context.ops.project(&record.project) else {
        return;
    };
    if let Err(error) = context
        .ops
        .remove_worktree(project.root, path.to_owned())
        .await
    {
        tracing::warn!(%thread, %path, %error, "could not remove an abandoned thread worktree");
        return;
    }
    let now = context.registry.context().clock.now().millis();
    if let Err(error) = context
        .store
        .forget_launch_worktree(&record.command, now)
        .await
    {
        tracing::warn!(%thread, %error, "could not forget an abandoned thread worktree");
    }
    let bound = context.registry.state(thread).await.ok().and_then(|state| {
        state
            .thread
            .as_ref()
            .and_then(|current| current.workspace.clone())
    });
    if bound.is_some_and(|workspace| workspace.worktree_path.as_deref() == Some(path)) {
        let _binding = context.workspaces.bind().await;
        if let Err(error) = context
            .input(thread, Input::Workspace { workspace: None })
            .await
        {
            tracing::warn!(%thread, %error, "could not unbind an abandoned thread worktree");
        }
    }
}

async fn prepare(
    context: &ExecutorContext,
    thread: &ThreadId,
    prepared: Option<PreparedRun<'_>>,
    setups: Option<&Arc<SetupTracker>>,
    checkout: &Mutex<Checkout>,
    cancel: &CancellationToken,
) -> Result<Prepared, PrepareError> {
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
    let stage = |id, status| {
        if let Some(setups) = setups {
            setups.stage_status(thread, id, status, None);
        }
    };
    // A worktree the thread already has keeps its binding: rewriting it could
    // undo the first attempt's branch rename.
    let bound = |path: &str| {
        current
            .workspace
            .clone()
            .filter(|workspace| workspace.worktree_path.as_deref() == Some(path))
    };
    // Whether this attempt provides the worktree, so it names its branch.
    let mut provided = false;
    let workspace = match (&record, strategy) {
        (Some(record), _) if record.worktree_path.is_some() => {
            let path = record.worktree_path.clone().unwrap_or_default();
            bound(&path).unwrap_or(Workspace {
                cwd: path.clone(),
                worktree_path: Some(path),
                branch: record.branch.clone(),
            })
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
            let reporter = setups.map(|setups| {
                let (setups, thread) = (setups.clone(), thread.clone());
                SetupProgress::new(move |event| {
                    if let SetupEvent::Stage(id, status) = event {
                        setups.stage_status(&thread, id, status, None);
                    }
                })
            });
            checkout.lock().unwrap().creating = true;
            let created = context
                .ops
                .create_worktree(WorktreeRequest {
                    thread: thread.clone(),
                    project: current.project.clone(),
                    project_root: project.root.clone(),
                    base_ref,
                    branch,
                    start_from_origin,
                    progress: reporter.unwrap_or_default(),
                    cancel: cancel.clone(),
                })
                .await;
            {
                let mut checkout = checkout.lock().unwrap();
                checkout.creating = false;
                checkout.path = created.as_ref().ok().map(|created| created.path.clone());
            }
            if cancel.is_cancelled() {
                return Err(PrepareError::Cancelled);
            }
            let created = created.map_err(failed(LaunchOperation::ProvisionWorktree))?;
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
                checkout.lock().unwrap().path = None;
                return Err(PrepareError::Retry(error.to_string()));
            }
            if let Some(setups) = setups {
                setups.update(thread, |snapshot| {
                    snapshot.worktree_path = Some(created.path.clone());
                    snapshot.branch = created.branch.clone().or(snapshot.branch.take());
                });
            }
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Done,
            );
            provided = true;
            Workspace {
                cwd: created.path.clone(),
                worktree_path: Some(created.path),
                branch: created.branch,
            }
        }
        // Like the reference, a chosen worktree is bound as the launch named it,
        // and its temporary branch is named again by every attempt.
        (_, WorkspaceStrategy::ExistingWorktree { path, branch }) => {
            provided = true;
            Workspace {
                cwd: path.clone(),
                worktree_path: Some(path),
                branch,
            }
        }
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
    checkout.lock().unwrap().recorded = true;
    // Like the reference, a temporary branch gets its generated name in the
    // background, so naming never delays the setup or the turn.
    if provided
        && let (Some(path), Some(branch)) = (&workspace.worktree_path, &workspace.branch)
        && is_temporary_worktree_branch(branch)
        && let Some(message) = run
            .and_then(|run| state.runs.iter().find(|candidate| &candidate.id == run))
            .and_then(|run| {
                state
                    .messages
                    .iter()
                    .find(|message| message.id == run.message)
            })
    {
        tokio::spawn(rename_branch(
            context.clone(),
            thread.clone(),
            BranchRename {
                project: current.project.clone(),
                worktree: path.clone(),
                branch: branch.clone(),
                text: message.text.clone(),
                attachments: message.attachments.clone(),
            },
        ));
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
    let mut done = Prepared {
        tracking: None,
        pending: None,
    };
    // A setup that completed is not run again by a retried preparation.
    if record.as_ref().is_some_and(|record| record.prepared) {
        stage(
            WorktreeSetupStageId::SetupScript,
            WorktreeSetupStageStatus::Skipped,
        );
        return Ok(done);
    }
    progress(context, thread, prepared.as_ref(), PreparationPhase::Setup).await?;
    stage(
        WorktreeSetupStageId::SetupScript,
        WorktreeSetupStageStatus::Running,
    );
    let observe = setups.map(|setups| {
        let (setups, thread) = (setups.clone(), thread.clone());
        SetupProgress::new(move |event| match event {
            SetupEvent::Output(line) => {
                setups.append_tail(&thread, WorktreeSetupStageId::SetupScript, &line)
            }
            SetupEvent::Stage(id, status) => setups.stage_status(&thread, id, status, None),
        })
    });
    let setup = context
        .ops
        .run_setup(SetupRequest {
            thread: thread.clone(),
            project: current.project.clone(),
            project_root: project.root,
            cwd: workspace.cwd,
            observe: observe.unwrap_or_default(),
        })
        .await
        .map_err(failed(LaunchOperation::RunSetupScript))?;
    match setup {
        SetupRun::NoScript => stage(
            WorktreeSetupStageId::SetupScript,
            WorktreeSetupStageStatus::Skipped,
        ),
        SetupRun::Started(started) => {
            if let Some(setups) = setups {
                setups.update(thread, |snapshot| {
                    snapshot.setup_script = Some(WorktreeSetupScript {
                        name: started.name.clone(),
                        command: started.command.clone(),
                        terminal_id: Some(started.terminal_id.clone()),
                    });
                });
            }
            match started.completion {
                Some(completion) if !started.run_async => {
                    let code = completion.await;
                    if let Some(setups) = setups {
                        setups.stage_status(
                            thread,
                            WorktreeSetupStageId::SetupScript,
                            setup_status(code),
                            Some(Some(exit_detail(code))),
                        );
                    }
                    if code != Some(0) {
                        return Err(PrepareError::Failed {
                            operation: LaunchOperation::RunSetupScript,
                            cause: format!("Setup script exited with {}.", exit_code(code)),
                        });
                    }
                }
                Some(completion) => done.pending = Some(completion),
                None => stage(
                    WorktreeSetupStageId::SetupScript,
                    WorktreeSetupStageStatus::Done,
                ),
            }
        }
    }
    if let Some(record) = &record {
        let now = context.registry.context().clock.now().millis();
        context
            .store
            .set_launch_status(&record.command, LaunchStatus::Prepared, now)
            .await
            .map_err(|error| PrepareError::Retry(error.to_string()))?;
    }
    Ok(done)
}

/// A launch's temporary branch and the first message it is named after.
struct BranchRename {
    project: String,
    worktree: String,
    branch: String,
    text: String,
    attachments: Vec<agent_domain::Attachment>,
}

/// Generates a branch name from the launch's first message, renames the
/// temporary branch, and shows the name on the thread while it still works in
/// that worktree. Any failure keeps the temporary name.
async fn rename_branch(context: ExecutorContext, thread: ThreadId, rename: BranchRename) {
    let renamed = async {
        let naming = context.ops.branch_naming(&rename.project);
        let raw = context
            .ops
            .generate_text(TextGenerationRequest {
                operation: "generateBranchName",
                project: rename.project.clone(),
                cwd: rename.worktree.clone(),
                prompt: branch_name_prompt(&naming, &rename.text, &rename.attachments),
                attachments: rename.attachments.clone(),
                output_schema: branch_name_output_schema(),
            })
            .await?;
        let generated = generated_branch_name(&raw, &naming)
            .ok_or_else(|| "the model did not return a branch name".to_owned())?;
        let renamed = context
            .ops
            .rename_branch(
                rename.worktree.clone(),
                rename.branch.clone(),
                generated,
                naming.mode == BranchNamingMode::Custom,
            )
            .await?;
        let _binding = context.workspaces.bind().await;
        let state = context
            .registry
            .state(&thread)
            .await
            .map_err(|error| error.to_string())?;
        let Some(workspace) = state
            .thread
            .as_ref()
            .and_then(|current| current.workspace.clone())
            .filter(|workspace| workspace.worktree_path.as_deref() == Some(&rename.worktree))
        else {
            return Ok(());
        };
        if workspace.branch.as_deref() != Some(renamed.as_str()) {
            context
                .input(
                    &thread,
                    Input::Workspace {
                        workspace: Some(Workspace {
                            branch: Some(renamed),
                            ..workspace
                        }),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = renamed {
        tracing::warn!(%thread, branch = %rename.branch, %error, "thread worktree branch rename failed");
    }
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
            let (command, done) = match prepare_workspace(context, &job.thread, Some(prepared))
                .await
            {
                Ok(done) => {
                    done.starting();
                    (Command::ReleasePrepared { run: run.clone() }, Some(done))
                }
                Err(PrepareError::Retry(error)) if job.will_retry => {
                    return Err(EffectError::Retryable(error));
                }
                // The last attempt fails the run so `RetryPrepared` can run it again.
                Err(PrepareError::Retry(cause)) => (
                    Command::FailPrepared {
                        run: run.clone(),
                        message: PrepareError::Failed {
                            operation: LaunchOperation::UpdateThread,
                            cause,
                        }
                        .to_string(),
                    },
                    None,
                ),
                Err(error) => {
                    tracing::warn!(thread = %job.thread, %run, %error, "workspace preparation failed");
                    (
                        Command::FailPrepared {
                            run: run.clone(),
                            message: error.to_string(),
                        },
                        None,
                    )
                }
            };
            context
                .dispatch(&job.thread, effect_command_id(&job.effect.id), command)
                .await
                .map_err(|error| EffectError::Retryable(error.to_string()))?;
            if let Some(done) = done {
                done.started();
            }
            Ok(None)
        })
    }
}
