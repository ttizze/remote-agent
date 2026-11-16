//! Checkout/script I/O owns one launch generation; the canonical thread owns
//! its progress. Empty launches use the same resource path without an app run.
use super::*;
use agent_protocol::workspace::SetupChange;
use agent_protocol::{
    orchestration as model,
    workspace::{SetupPhase, SetupStageId, SetupStageStatus},
};

impl Runtime {
    pub(super) fn schedule_workspace(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let state = runtime
            .storage
            .thread_state(&target.application_id(), &[])?;
        let Some(workspace) = state
            .thread
            .workspace_launch
            .as_ref()
            .filter(|workspace| {
                state
                    .active_run()
                    .is_some_and(|run| run.status == model::RunStatus::Preparing)
                    || workspace
                        .setup
                        .as_ref()
                        .is_some_and(|setup| setup.phase == SetupPhase::Running && setup.can_cancel)
            })
            .cloned()
        else {
            return Ok(());
        };
        let preparation = runtime.preparation(target);
        let generation = workspace.generation;
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let mut active = preparation.active.lock().unwrap();
            if active
                .as_ref()
                .is_some_and(|(current, _)| *current == generation)
            {
                return Ok(());
            }
            if let Some((_, previous)) = active.take() {
                previous.cancel();
            }
            *active = Some((generation, cancel.clone()));
        }
        let service = service.clone();
        let target = target.clone();
        tokio::spawn(async move {
            let _retire = scopeguard::guard((), |_| {
                service.inner.conversation.retire_preparation(&target)
            });
            let preparation = preparation;
            let _io = preparation.io.lock().await;
            let _active = scopeguard::guard((), |_| {
                let mut active = preparation.active.lock().unwrap();
                if active
                    .as_ref()
                    .is_some_and(|(current, _)| *current == generation)
                {
                    *active = None;
                }
            });
            if let Err(error) = Self::prepare_workspace(
                &service,
                &target,
                generation,
                &workspace.project_cwd,
                cancel.clone(),
            )
            .await
                && let Err(failed) = Self::fail_workspace(
                    &service,
                    &target,
                    generation,
                    &error,
                    cancel.is_cancelled(),
                )
                .await
            {
                tracing::error!(target:"agent",operation="workspace.prepare",%failed);
            }
        });
        Ok(())
    }

    async fn prepare_workspace(
        service: &HostRpcService,
        target: &SessionRef,
        generation: Uuid,
        root: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), Failure> {
        let _access = service.inner.worktree_access.read().await;
        service.require_not_updating()?;
        if cancel.is_cancelled() {
            return Err(Failure::new(
                "preparation_interrupted",
                "Preparation cancelled",
            ));
        }
        Self::discard_cancelled_workspace(service, target, generation).await?;
        let (cwd, _) = service
            .prepare_task_workspace(target, generation, cancel.clone())
            .await?;
        let saved = service
            .inner
            .conversation
            .storage
            .thread_header(target)
            .map_err(storage_failure)?
            .and_then(|thread| thread.workspace_launch)
            .filter(|workspace| workspace.generation == generation)
            .ok_or_else(|| Failure::new("preparation_interrupted", "Preparation cancelled"))?;
        let script = saved.setup.as_ref().and_then(|setup| setup.script.clone());
        service
            .run_setup_script(target, generation, (&cwd, root), script, cancel.clone())
            .await?;
        if cancel.is_cancelled() {
            return Err(Failure::new(
                "preparation_interrupted",
                "Preparation cancelled",
            ));
        }
        let at = commands::at();
        let operation = generation.to_string();
        service
            .inner
            .conversation
            .storage
            .observe_thread(&target.application_id(), &[], &at, |state| {
                let Some(workspace) = state
                    .thread
                    .workspace_launch
                    .as_ref()
                    .filter(|workspace| workspace.generation == generation)
                else {
                    return Ok(Default::default());
                };
                let mut workspace = workspace.clone();
                let branch = workspace
                    .setup
                    .as_ref()
                    .and_then(|setup| setup.branch.clone());
                let run = state.active_run().filter(|run| {
                    run.status == model::RunStatus::Preparing
                        && workspace.run_id.as_deref() == Some(&run.id)
                });
                if workspace.run_id.is_some() && run.is_none() {
                    return Ok(Default::default());
                }
                if let Some(setup) = &mut workspace.setup {
                    setup.can_cancel = false;
                    setup.stage(
                        SetupStageId::Agent,
                        if run.is_some() {
                            SetupStageStatus::Running
                        } else {
                            SetupStageStatus::Skipped
                        },
                        now().max(0) as u64,
                    );
                    if run.is_none() {
                        setup.finish_if_ready(now().max(0) as u64);
                    }
                }
                let mut decision = if let Some(run) = run {
                    model::preparation_release(
                        state,
                        &run.id,
                        &cwd,
                        branch.as_deref(),
                        executor::context(state, &operation, &at),
                    )?
                } else {
                    model::Decision::default()
                };
                let mut thread = state.apply(&decision).thread;
                thread.worktree_path = Some(cwd.clone());
                thread.branch = branch;
                thread.workspace_launch = Some(workspace);
                thread.updated_at = at.clone();
                decision
                    .changes
                    .retain(|change| !matches!(change, model::Change::Thread(_)));
                decision.changes.push(model::Change::Thread(thread));
                Ok(decision)
            })
            .map_err(storage_failure)?;
        Ok(())
    }

    async fn fail_workspace(
        service: &HostRpcService,
        target: &SessionRef,
        generation: Uuid,
        error: &Failure,
        cancelled: bool,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let Some(_workspace) = storage
            .thread_header(target)?
            .and_then(|thread| thread.workspace_launch)
            .filter(|workspace| workspace.generation == generation)
        else {
            return Ok(());
        };
        service
            .setup_update(
                target,
                generation,
                SetupChange::Finish {
                    phase: if cancelled {
                        SetupPhase::Cancelled
                    } else {
                        SetupPhase::Failed
                    },
                    error: Some(error.message.clone()),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        if cancelled {
            Self::discard_cancelled_workspace(service, target, generation)
                .await
                .map_err(|error| error.to_string())?;
        }
        let at = commands::at();
        let operation = generation.to_string();
        storage.observe_thread(&target.application_id(), &[], &at, |state| {
            if state
                .thread
                .workspace_launch
                .as_ref()
                .is_none_or(|workspace| workspace.generation != generation)
            {
                return Ok(Default::default());
            }
            let Some(run) = state.active_run().filter(|run| {
                run.status == model::RunStatus::Preparing
                    && state
                        .thread
                        .workspace_launch
                        .as_ref()
                        .and_then(|workspace| workspace.run_id.as_deref())
                        == Some(&run.id)
            }) else {
                return Ok(Default::default());
            };
            model::preparation_fail(
                state,
                &run.id,
                model::ProviderFailure {
                    class: "workspace".into(),
                    message: error.message.clone(),
                    code: None,
                    retryable: Some(true),
                    reset_at: None,
                },
                executor::context(state, &operation, &at),
            )
        })?;
        Ok(())
    }
    pub(super) fn cancel_workspace(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        if !runtime
            .storage
            .thread_header(target)?
            .and_then(|thread| thread.workspace_launch)
            .is_some_and(|workspace| {
                workspace
                    .setup
                    .as_ref()
                    .is_some_and(|setup| setup.phase == SetupPhase::Cancelled)
            })
        {
            return Ok(());
        }
        let preparation = runtime.preparation(target);
        if let Some((_, cancel)) = &*preparation.active.lock().unwrap() {
            cancel.cancel();
        }
        let service = service.clone();
        let target = target.clone();
        tokio::spawn(async move {
            let _retire = scopeguard::guard((), |_| {
                service.inner.conversation.retire_preparation(&target)
            });
            let preparation = preparation;
            let _io = preparation.io.lock().await;
            let result = async {
                if let Some(workspace) = service
                    .inner
                    .conversation
                    .storage
                    .thread_header(&target)?
                    .and_then(|thread| thread.workspace_launch)
                {
                    Self::discard_cancelled_workspace(&service, &target, workspace.generation)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                tracing::error!(target:"agent",operation="workspace.cancel",%error);
            }
        });
        Ok(())
    }

    async fn discard_cancelled_workspace(
        service: &HostRpcService,
        target: &SessionRef,
        generation: Uuid,
    ) -> Result<(), Failure> {
        let Some(workspace) = service
            .inner
            .conversation
            .storage
            .thread_header(target)
            .map_err(storage_failure)?
            .and_then(|thread| thread.workspace_launch)
            .filter(|workspace| {
                workspace.generation == generation
                    && workspace
                        .setup
                        .as_ref()
                        .is_some_and(|setup| setup.phase == SetupPhase::Cancelled)
            })
        else {
            return Ok(());
        };
        let setup = workspace.setup.as_ref().expect("cancelled setup");
        service
            .cleanup_setup_resources(
                setup
                    .worktree_path
                    .clone()
                    .filter(|_| workspace.created_worktree),
                setup.branch.clone().filter(|_| workspace.created_worktree),
                setup.terminal_handle.as_deref(),
            )
            .await?;
        if workspace.created_worktree {
            service
                .setup_update(target, generation, SetupChange::Discarded)
                .await?;
        }
        Ok(())
    }
}
