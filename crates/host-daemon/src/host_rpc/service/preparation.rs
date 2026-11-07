//! Admission is durable before filesystem or provider preparation starts. The
//! public address survives failures, retries, disconnection and native binding.
use super::*;
use crate::host_rpc::conversations::{Conversation, Startup, StartupChange};
use agent_protocol::session::{
    PreparationStage, PreparationStatus, SessionChange, SessionPreparation, SessionRef,
    SubmissionDelivery,
};

#[cfg(test)]
mod tests;

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
        };
        let row = Conversation {
            id: id.clone(),
            active: id.clone(),
            cwd,
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
                preparation: SessionPreparation {
                    submission,
                    status: PreparationStatus::Preparing,
                    stage: PreparationStage::Workspace,
                },
                started_at: now(),
            }),
        };
        let projects = self.project_snapshot().await?;
        let response = self.preparation_snapshot(&row, &projects);
        let (row, created) = self.inner.database.admit(row)?;
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
            // Recovery confirmed the initial input from native storage. Commit
            // that evidence before clearing its durable preparation state.
            self.inner
                .database
                .import(&row.id, response.clone())
                .map_err(transcripts::storage_failure)?;
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
        let (row, changed) = self.inner.database.update_startup(id, change)?;
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
            let result = service.prepare_task(&id).await;
            if let Err(error) = result
                && let Err(failure) = service.fail_preparation(&id, error).await
            {
                tracing::error!(target: "agent", operation = "session.prepare", message = %failure);
            }
        });
    }

    async fn prepare_task(&self, id: &SessionRef) -> Result<(), Failure> {
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
        if row.startup.as_ref().unwrap().preparation.stage != PreparationStage::Provider {
            let selected = (std::path::Path::new(&row.cwd) != self.inner.projects.chat_directory())
                .then_some(row.cwd.as_str());
            let (cwd, created) = self
                .prepare_directory(selected, || async {
                    self.change_preparation(id, StartupChange::Worktree)
                        .await
                        .map(|_| ())
                        .map_err(|error| anyhow::anyhow!(error.message))
                })
                .await?;
            // Record the checkout even when interrupted so a retry reuses it;
            // only a checkout no row refers to is rolled back.
            if let Err(error) = self
                .change_preparation(id, StartupChange::Workspace { cwd: cwd.clone() })
                .await
            {
                if created {
                    self.discard_worktree(&cwd).await;
                }
                return Err(error);
            }
        } else if row.bindings.is_empty() {
            self.restore_workspace(&row.cwd).await?;
        }
        let Some(row) = self.preparing_task(id)? else {
            return Ok(());
        };
        let input = row.startup.as_ref().unwrap().preparation.submission.clone();
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
            self.change_preparation(id, StartupChange::Binding { native })
                .await?;
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
        Ok(self.inner.database.get(id)?.filter(|row| {
            row.startup
                .as_ref()
                .is_some_and(|startup| startup.preparation.status == PreparationStatus::Preparing)
        }))
    }

    async fn fail_preparation(&self, id: &SessionRef, error: Failure) -> Result<(), Failure> {
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
    ) -> Result<agent_protocol::models::Empty, Failure> {
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
        Ok(agent_protocol::models::Empty {})
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
