//! Each workspace preparation owns cancellation and serialized resource I/O.
use super::*;

pub(super) struct Preparation {
    pub io: AsyncMutex<()>,
    pub active: Mutex<Option<(Uuid, tokio_util::sync::CancellationToken)>>,
}

impl Runtime {
    pub(super) async fn prepare_effect(
        service: &HostRpcService,
        target: &SessionRef,
        id: ClientInputId,
        action: Uuid,
        stage: PreparationStage,
        cwd: Option<String>,
        provider: ProviderKind,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let workspace = runtime.storage.header(&target)?.details.workspace;
        let mut setup_generation = workspace.as_ref().map(|workspace| workspace.generation);
        if stage == PreparationStage::Worktree {
            setup_generation = Some(action);
        }
        if stage == PreparationStage::Provider
            && let Some(mut workspace) = workspace
            && let Some(setup) = &mut workspace.setup
            && setup.phase != agent_protocol::workspace::SetupPhase::Running
        {
            workspace.generation = action;
            workspace.visible = true;
            setup.phase = agent_protocol::workspace::SetupPhase::Running;
            setup.ended_at_ms = None;
            setup.error = None;
            setup.can_cancel = false;
            setup.stage(
                agent_protocol::workspace::SetupStageId::Agent,
                agent_protocol::workspace::SetupStageStatus::Pending,
                now().max(0) as u64,
            );
            service
                .setup_update(
                    &target,
                    action,
                    super::super::storage::workspace::SetupChange::Begin {
                        input: id.clone(),
                        workspace,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            setup_generation = Some(action);
        }
        let result = Self::prepare(service, &target, &id, action, stage, cwd, provider).await;
        match result {
            Ok((cwd, source, created)) => {
                Self::event(
                    service,
                    &target,
                    Event::Prepared {
                        id,
                        action,
                        cwd: Some(cwd.clone()),
                        source: source.clone(),
                    },
                )
                .await?;
                if let Ok(projects) = service.project_snapshot().await {
                    runtime
                        .owners
                        .workspace(
                            &target,
                            cwd.clone(),
                            projects.project_membership(Some(&cwd)),
                        )
                        .await?;
                }
                let saved = runtime.storage.state(&target)?;
                if let Some(native) = source {
                    Self::release_unused_source(service, &target, provider, &native).await?;
                }
                if created && saved.thread.workspace.cwd() != Some(cwd.as_str()) {
                    if let Err(error) = service.inner.worktrees.discard(cwd).await {
                        tracing::warn!(target: "agent", operation="conversation.discard_worktree", %error);
                    }
                }
            }
            Err(error) => {
                if let Some(generation) = setup_generation {
                    service.setup_update(&target, generation, super::super::storage::workspace::SetupChange::Finish {
                                phase: if runtime.storage.state(&target)?.inputs.get(&id).is_some_and(|input| matches!(input.state, decision::InputState::Preparing { action: current, .. } if current == action)) { agent_protocol::workspace::SetupPhase::Failed } else { agent_protocol::workspace::SetupPhase::Cancelled },
                                error: Some(error.message.clone()),
                            }).await.map_err(|error| error.to_string())?;
                }
                Self::event(
                    service,
                    &target,
                    Event::PreparationFailed {
                        id,
                        action,
                        error: execution_error(error),
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn prepare(
        service: &HostRpcService,
        target: &SessionRef,
        id: &ClientInputId,
        action: Uuid,
        stage: PreparationStage,
        cwd: Option<String>,
        provider: ProviderKind,
    ) -> Result<(String, Option<String>, bool), Failure> {
        let _workspace = service.inner.worktree_access.read().await;
        service.require_not_updating()?;
        let runtime = &service.inner.conversation;
        let _retire = scopeguard::guard((), |_| {
            let mut preparations = runtime.preparations.lock().unwrap();
            if preparations.get(target).is_some_and(|preparation| {
                Arc::strong_count(preparation) == 1 && preparation.active.lock().unwrap().is_none()
            }) {
                preparations.remove(target);
            }
        });
        let preparation = runtime
            .preparations
            .lock()
            .unwrap()
            .entry(target.clone())
            .or_insert_with(|| {
                Arc::new(Preparation {
                    io: Default::default(),
                    active: Default::default(),
                })
            })
            .clone();
        let _io = preparation.io.lock().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        *preparation.active.lock().unwrap() = Some((action, cancel.clone()));
        let _active = scopeguard::guard(preparation.clone(), |preparation| {
            let mut active = preparation.active.lock().unwrap();
            if active
                .as_ref()
                .is_some_and(|(current, _)| *current == action)
            {
                *active = None;
            }
        });
        if !runtime
            .storage
            .state(target)
            .map_err(storage_failure)?
            .permits(&Action::Prepare {
                id: id.clone(),
                action,
                stage,
                cwd: cwd.clone(),
                provider,
            })
        {
            return Err(Failure::new(
                "preparation_interrupted",
                "Preparation cancelled",
            ));
        }
        match stage {
            PreparationStage::Workspace => {
                let directory = cwd
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| service.inner.projects.chat_directory());
                tokio::fs::create_dir_all(&directory)
                    .await
                    .map_err(|error| Failure::new("workspace_unavailable", error))?;
                let directory = tokio::fs::canonicalize(&directory)
                    .await
                    .map_err(|error| Failure::new("workspace_unavailable", error))?;
                Ok((
                    dunce::simplified(&directory)
                        .to_str()
                        .ok_or_else(|| {
                            Failure::new("workspace_unavailable", "workspace path is not UTF-8")
                        })?
                        .to_owned(),
                    None,
                    false,
                ))
            }
            PreparationStage::Worktree => {
                let cwd = cwd
                    .ok_or_else(|| Failure::new("workspace_unavailable", "workspace is missing"))?;
                let (cwd, created) = service
                    .prepare_task_workspace(target, id, action, &cwd, cancel)
                    .await?;
                Ok((cwd, None, created))
            }
            PreparationStage::SetupScript => {
                let cwd = cwd
                    .ok_or_else(|| Failure::new("workspace_unavailable", "workspace is missing"))?;
                if let Some(workspace) = runtime
                    .storage
                    .header(target)
                    .map_err(storage_failure)?
                    .details
                    .workspace
                {
                    let script = workspace
                        .setup
                        .as_ref()
                        .and_then(|setup| setup.script.clone());
                    service
                        .run_setup_script(
                            target,
                            workspace.generation,
                            (&cwd, &workspace.project_cwd),
                            script,
                            cancel,
                        )
                        .await?;
                }
                Ok((cwd, None, false))
            }
            PreparationStage::Provider => {
                if let Some(workspace) = runtime
                    .storage
                    .header(target)
                    .map_err(storage_failure)?
                    .details
                    .workspace
                {
                    service
                        .setup_update(
                            target,
                            workspace.generation,
                            super::super::storage::workspace::SetupChange::AgentStarted,
                        )
                        .await?;
                }
                Self::prepare_provider(service, target, id, action, cwd, provider).await
            }
        }
    }
}
