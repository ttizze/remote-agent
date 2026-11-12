//! Admission is durable before filesystem or provider preparation starts. The
//! public address survives failures, retries, disconnection and native binding.
use super::*;
use crate::host_rpc::conversations::{Conversation, Startup, StartupChange};
use agent_protocol::session::{
    PreparationStage, PreparationStatus, SessionChange, SessionPreparation, SessionRef,
    SubmissionDelivery,
};

mod script;
#[cfg(test)]
mod tests;
mod workspace;
use agent_protocol::workspace::{
    SetupPhase, SetupStageId, SetupStageStatus, WorkspaceSetup, WorkspaceStrategy,
};

impl HostRpcService {
    pub(super) async fn admit_task(
        &self,
        params: &op::CreateSession,
    ) -> Result<(ThreadResponse, bool), Failure> {
        self.creation_agent(params.provider, params.model.as_ref())?;
        if self
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Failure::new(
                "host_updating",
                "Bexを更新しています。再接続後にもう一度お試しください。",
            ));
        }
        let initial = params
            .initial_input
            .as_ref()
            .expect("task admission has input");
        if let Some(workspace) = &initial.workspace {
            workspace
                .validate()
                .map_err(|error| Failure::new("invalid_params", error))?;
        }
        if initial.client_user_message_id.is_empty() || initial.client_user_message_id.len() > 256 {
            return Err(Failure::new(
                "invalid_params",
                "clientUserMessageId is required",
            ));
        }
        let id = SessionRef::new(
            params.provider,
            format!("bex-task:{}", initial.client_user_message_id),
        )
        .map_err(|error| Failure::new("invalid_params", error))?;
        let cwd = match params.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
            Some(cwd) => cwd.into(),
            None => self
                .inner
                .projects
                .chat_directory()
                .into_os_string()
                .into_string()
                .map_err(|_| {
                    Failure::new("chat_directory_unavailable", "chat path is not UTF-8")
                })?,
        };
        let submission = op::Submission {
            thread_id: id.clone(),
            client_user_message_id: initial.client_user_message_id.clone(),
            input: initial.input.clone(),
            model: params.model.clone(),
            effort: initial.effort.clone(),
            service_tier: initial.service_tier.clone(),
            follow_up: Default::default(),
        };
        let projects = self.project_snapshot().await?;
        let settings = self
            .inner
            .worktrees
            .settings(None)
            .await
            .map_err(|error| Failure::new("worktree_settings_failed", error))?;
        let workspace = initial.workspace.clone().or_else(|| {
            (params.cwd.is_some() && settings.create_on_new_session).then(|| {
                WorkspaceStrategy::Worktree {
                    base_ref: String::new(),
                    start_from_origin: true,
                }
            })
        });
        let mut setup = match &workspace {
            Some(WorkspaceStrategy::Worktree { base_ref, .. }) => {
                Some(WorkspaceSetup::new(Some(base_ref.clone()), now_ms()))
            }
            Some(WorkspaceStrategy::ExistingWorktree { .. }) => {
                Some(WorkspaceSetup::new(None, now_ms()))
            }
            _ => None,
        };
        if let Some(setup) = &mut setup {
            setup.script = projects
                .project_for_workspace(&cwd)
                .and_then(|id| settings.project_setup_scripts.get(id))
                .cloned()
                .or(settings.setup_script)
                .filter(|script| !script.command.trim().is_empty());
        }
        let row = Conversation {
            id: id.clone(),
            active: id.clone(),
            cwd: cwd.clone(),
            name: None,
            preview: initial.input.iter().find_map(|part| match part {
                op::Input::Text { text } => Some(agent_protocol::models::compact_title(
                    text.lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or_default(),
                )),
                _ => None,
            }),
            bindings: Vec::new(),
            segments: Vec::new(),
            handoffs: Vec::new(),
            startup: Some(Startup {
                project_cwd: cwd,
                workspace,
                preparation: SessionPreparation {
                    submission,
                    status: PreparationStatus::Preparing,
                    stage: PreparationStage::Workspace,
                },
                started_at: now(),
            }),
            workspace_setup: setup,
        };
        let response = self.preparation_snapshot(&row, &projects);
        let (row, created) = self.inner.conversations.admit(row)?;
        if !created {
            return Ok((
                self.conversation_history(&row, conversations::HistoryUse::Read)
                    .await?,
                false,
            ));
        }
        self.publish_preparation(&row, &response, &projects.projects);
        Ok((response, true))
    }

    pub(super) async fn preparation_response(
        &self,
        row: &Conversation,
    ) -> Result<ThreadResponse, Failure> {
        Ok(self.preparation_snapshot(row, &self.project_snapshot().await?))
    }

    pub(super) async fn preparation_history(
        &self,
        row: &Conversation,
    ) -> Result<ThreadResponse, Failure> {
        let mut response = self.preparation_response(row).await?;
        let preparation = &row
            .startup
            .as_ref()
            .expect("preparation history has startup")
            .preparation;
        if preparation.status != PreparationStatus::Unknown || row.bindings.is_empty() {
            return Ok(response);
        }
        let native = match self
            .native_history(&row.active, conversations::HistoryUse::Read)
            .await
        {
            Ok(native) => native,
            Err(error) => {
                response.thread.history_read_state =
                    Some(agent_protocol::session::HistoryReadState::new(
                        agent_protocol::session::HistoryReadKind::Incomplete,
                        vec![error.message],
                    ));
                return Ok(response);
            }
        };
        let receipt = native
            .thread
            .turns
            .iter()
            .flatten()
            .find(|turn| {
                turn.items.iter().flatten().any(|item| {
                    item.client_input_id.as_ref()
                        == Some(&preparation.submission.client_user_message_id)
                })
            })
            .map(|turn| row.identity(&row.active, &turn.id).into());
        response.thread.turns = native.thread.turns.map(|turns| {
            turns
                .iter()
                .map(|turn| Arc::new(row.turn(&row.active, turn.as_ref().clone())))
                .collect()
        });
        if let Some(turn_id) = receipt {
            self.change_preparation(&row.id, StartupChange::Finished)
                .await?;
            response.thread.preparation = None;
            response.thread.status = native.thread.status;
            self.inner.router.session_change(
                &row.id,
                SessionChange::Status {
                    status: native.thread.status,
                },
            );
            self.inner.router.session_change(
                &row.id,
                SessionChange::Submission {
                    id: preparation.submission.client_user_message_id.clone(),
                    delivery: SubmissionDelivery::Accepted {
                        turn_id: Some(turn_id),
                    },
                },
            );
        }
        Ok(response)
    }

    fn preparation_snapshot(
        &self,
        row: &Conversation,
        projects: &crate::projects::state::Snapshot,
    ) -> ThreadResponse {
        let mut thread = Thread::default();
        row.describe(&mut thread);
        let input = row
            .startup
            .as_ref()
            .map(|startup| &startup.preparation.submission);
        describe_thread(
            &mut thread,
            self.conversation_capabilities(
                row.active.provider,
                row.sources().map(|source| source.provider),
            ),
            projects,
        );
        ThreadResponse {
            thread,
            model: input.and_then(|input| input.model.clone()),
            effort: input.and_then(|input| input.effort.clone()),
            service_tier: input.and_then(|input| input.service_tier.clone()),
        }
    }

    fn publish_preparation(
        &self,
        row: &Conversation,
        response: &ThreadResponse,
        projects: &[agent_protocol::models::Project],
    ) {
        self.inner.router.session_change(
            &row.id,
            SessionChange::Workspace {
                cwd: row.cwd.clone(),
            },
        );
        self.inner.router.session_change(
            &row.id,
            SessionChange::Preparation {
                preparation: row
                    .startup
                    .as_ref()
                    .map(|startup| startup.preparation.clone()),
            },
        );
        self.inner.router.session_change(
            &row.id,
            SessionChange::WorkspaceSetup {
                setup: response.thread.workspace_setup.clone(),
            },
        );
        self.inner.router.session_change(
            &row.id,
            SessionChange::Status {
                status: response.thread.status,
            },
        );
        self.notify_session_updated(response.thread.clone(), projects);
    }

    async fn change_preparation(
        &self,
        id: &SessionRef,
        change: StartupChange,
    ) -> Result<(Conversation, bool), Failure> {
        let projects = self.project_snapshot().await?;
        let (row, changed) = self.inner.conversations.update_startup(id, change)?;
        if changed {
            if row.startup.is_some() {
                let response = self.preparation_snapshot(&row, &projects);
                self.publish_preparation(&row, &response, &projects.projects);
            } else {
                self.inner
                    .router
                    .session_change(id, SessionChange::Preparation { preparation: None });
            }
        }
        Ok((row, changed))
    }

    pub(super) fn schedule_preparation(&self, id: SessionRef) {
        let service = self.clone();
        tokio::spawn(async move {
            let _serial = service.inner.router.submission_lock(&id).lock_owned().await;
            let _execution = service
                .inner
                .router
                .retain_execution(id.clone())
                .expect("validated admitted task");
            let _workspace = service.inner.worktree_access.read().await;
            let cancel = tokio_util::sync::CancellationToken::new();
            service
                .inner
                .preparations
                .lock()
                .unwrap()
                .insert(id.clone(), cancel.clone());
            let result = service.prepare_task(&id, cancel.clone()).await;
            if cancel.is_cancelled()
                && let Err(error) = service.cleanup_cancelled_workspace(&id).await
            {
                let _ = service.setup_update(&id, |setup| setup.error = Some(error.message));
            }
            service.inner.preparations.lock().unwrap().remove(&id);
            if let Err(error) = result
                && let Err(failure) = service.fail_preparation(&id, error).await
            {
                tracing::error!(target: "agent", operation = "session.prepare", message = %failure);
            }
        });
    }

    async fn prepare_task(
        &self,
        id: &SessionRef,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), Failure> {
        if self
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Failure::new(
                "host_updating",
                "Bexを更新しています。再接続後にもう一度お試しください。",
            ));
        }
        let Some(row) = self.preparing_task(id)? else {
            return Ok(());
        };
        if matches!(
            row.startup.as_ref().unwrap().preparation.stage,
            PreparationStage::Workspace | PreparationStage::Worktree
        ) {
            self.prepare_task_workspace(&row, cancel.clone()).await?;
        } else if row.bindings.is_empty() {
            self.restore_workspace(&row.cwd).await?;
        }
        let Some(row) = self.preparing_task(id)? else {
            return Ok(());
        };
        if row.startup.as_ref().unwrap().preparation.stage == PreparationStage::SetupScript {
            self.run_setup_script(&row, cancel.clone()).await?;
            self.change_preparation(id, StartupChange::Prepared).await?;
        }
        let Some(row) = self.preparing_task(id)? else {
            return Ok(());
        };
        let input = row.startup.as_ref().unwrap().preparation.submission.clone();
        self.setup_update(id, |setup| {
            setup.can_cancel = false;
            setup.stage(SetupStageId::Agent, SetupStageStatus::Running, now_ms());
        })?;
        if row.bindings.is_empty() {
            let agent = self.creation_agent(id.provider, input.model.as_ref())?;
            let response = agent
                .create(
                    &row.cwd,
                    input.model.as_ref().map(|model| model.id.as_str()),
                    self.browser_config(&id.to_string())?,
                )
                .await
                .map_err(Failure::before_submission)?;
            op::validate_thread(&response, None)
                .map_err(|error| Failure::new("invalid_thread", error))?;
            let name = response.thread.name.clone();
            let native = response
                .thread
                .id
                .ok_or_else(|| Failure::new("invalid_thread", "created session ID is missing"))?;
            if native.provider != id.provider {
                return Err(Failure::new(
                    "provider_mismatch",
                    "created session provider does not match",
                ));
            }
            self.change_preparation(
                id,
                StartupChange::Binding {
                    native: native.clone(),
                },
            )
            .await?;
            // A provider that names its session at creation never renames it,
            // so its name reaches the open clients as the rename it would be;
            // they would otherwise see it only in a later list.
            if let Some(name) = name.filter(|name| !name.trim().is_empty())
                && let Some(renamed) = self.inner.conversations.route(
                    crate::providers::AgentChange::Renamed {
                        session: native,
                        name: Some(name),
                    },
                    false,
                )?
            {
                self.inner
                    .router
                    .apply(renamed, &[])
                    .map_err(|error| Failure::new("rename_failed", error))?;
            }
        }
        let (row, _) = self.change_preparation(id, StartupChange::Sending).await?;
        let mut native = input;
        native.thread_id = row.active.clone();
        let mut receipt = self.submit_native(id, &row.active, &native).await?;
        receipt.turn_id = receipt
            .turn_id
            .map(|turn| row.identity(&row.active, &turn).into());
        self.change_preparation(id, StartupChange::Finished)
            .await
            .map_err(|error| Failure::unknown("preparation_receipt_failed", error))?;
        self.setup_update(id, |setup| {
            setup.stage(SetupStageId::Agent, SetupStageStatus::Done, now_ms())
        })?;
        self.finish_setup_if_ready(id)?;
        self.inner.router.session_change(
            id,
            SessionChange::Submission {
                id: native.client_user_message_id,
                delivery: SubmissionDelivery::Accepted {
                    turn_id: receipt.turn_id,
                },
            },
        );
        Ok(())
    }

    fn preparing_task(&self, id: &SessionRef) -> Result<Option<Conversation>, Failure> {
        Ok(self.inner.conversations.get(id)?.filter(|row| {
            row.startup
                .as_ref()
                .is_some_and(|startup| startup.preparation.status == PreparationStatus::Preparing)
        }))
    }

    async fn fail_preparation(&self, id: &SessionRef, error: Failure) -> Result<(), Failure> {
        let interrupted = self
            .inner
            .conversations
            .get(id)?
            .and_then(|row| row.startup)
            .is_some_and(|startup| startup.preparation.status == PreparationStatus::Interrupted);
        self.setup_update(id, |setup| {
            setup.finish(
                if interrupted {
                    SetupPhase::Cancelled
                } else {
                    SetupPhase::Failed
                },
                (!interrupted).then(|| error.message.clone()),
                now_ms(),
            )
        })?;
        self.expire_current_setup(id)?;
        let status = if error.delivery == agent_protocol::error::Delivery::Unknown {
            PreparationStatus::Unknown
        } else {
            PreparationStatus::Failed {
                error: error.execution.map(|error| *error).unwrap_or(
                    agent_protocol::execution::ExecutionError {
                        message: error.message,
                        ..Default::default()
                    },
                ),
            }
        };
        self.change_preparation(id, StartupChange::Failed { status })
            .await?;
        Ok(())
    }

    pub(super) async fn retry_preparation(
        &self,
        id: &SessionRef,
        work_locally: bool,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        if work_locally {
            self.change_preparation(id, StartupChange::UseRoot).await?;
        }
        let (_, changed) = self
            .change_preparation(id, StartupChange::Retry { started_at: now() })
            .await?;
        if changed {
            self.schedule_preparation(id.clone());
        }
        Ok(agent_protocol::models::Empty {})
    }

    pub(super) async fn interrupt_preparation(
        &self,
        id: &SessionRef,
        turn: &agent_protocol::ids::TurnId,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        self.change_preparation(
            id,
            StartupChange::Interrupt {
                input_id: turn.to_string(),
            },
        )
        .await?;
        if let Some(cancel) = self.inner.preparations.lock().unwrap().get(id) {
            cancel.cancel();
        }
        self.setup_update(id, |setup| {
            setup.finish(SetupPhase::Cancelled, None, now_ms())
        })?;
        self.expire_current_setup(id)?;
        Ok(agent_protocol::models::Empty {})
    }

    pub(super) async fn preempt_setup(&self, id: &SessionRef) -> Result<(), Failure> {
        let Some(row) = self.inner.conversations.get(id)? else {
            return Ok(());
        };
        let Some(startup) = row.startup else {
            return Ok(());
        };
        if startup.preparation.status == PreparationStatus::Preparing {
            if !row
                .workspace_setup
                .as_ref()
                .is_some_and(|setup| setup.can_cancel)
            {
                return Err(Failure::new(
                    "preparation_started",
                    "The agent has already started",
                ));
            }
            self.interrupt_preparation(
                id,
                &startup
                    .preparation
                    .submission
                    .client_user_message_id
                    .to_string()
                    .into(),
            )
            .await?;
        }
        Ok(())
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn now_ms() -> u64 {
    (now() * 1000.) as u64
}
