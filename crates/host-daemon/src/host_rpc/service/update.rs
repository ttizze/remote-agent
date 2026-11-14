//! The detached updater reads a progress report. Continuations themselves live
//! in the conversation database and use the durable job receipt exactly once.
use super::*;
use agent_local_host::update::{ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery};
use agent_protocol::conversation::Outcome;

fn read_report(path: &std::path::Path) -> anyhow::Result<Option<Recovery>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl HostRpcService {
    pub(crate) async fn prepare_update(&self) -> anyhow::Result<()> {
        let _workspace = self.inner.worktree_access.write().await;
        anyhow::ensure!(
            !self
                .inner
                .updating
                .swap(true, std::sync::atomic::Ordering::AcqRel),
            "ホストの更新が既に進行中です。"
        );
        let reset = scopeguard::guard(&self.inner.updating, |updating| {
            updating.store(false, std::sync::atomic::Ordering::Release)
        });
        let (directory, _) = self
            .inner
            .history_scope
            .get()
            .ok_or_else(|| anyhow::anyhow!("Host state directory unavailable"))?;
        let activity = self
            .update_task_snapshot()
            .await
            .map_err(anyhow::Error::msg)?;
        let mut tasks = Vec::new();
        for (session, status) in activity.statuses {
            if status != agent_protocol::models::SessionStatus::Running {
                continue;
            }
            tokio::time::timeout(
                SESSION_LIST_TIMEOUT,
                super::super::runtime::Runtime::ensure_imported(self, &session),
            )
            .await
            .map_err(|_| anyhow::Error::msg(super::activity::incomplete()))?
            .map_err(anyhow::Error::msg)?;
            let state = self
                .inner
                .conversation
                .storage
                .state(&session)
                .map_err(anyhow::Error::msg)?;
            tasks.push(InterruptedTask {
                native_session: self
                    .inner
                    .conversation
                    .storage
                    .active_source(&session)
                    .map_err(anyhow::Error::msg)?
                    .unwrap_or_else(|| session.clone()),
                cwd: state.thread.workspace.cwd().map(str::to_owned),
                submission: op::Submission {
                    thread_id: session,
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    input: vec![op::Input::Text {
                        text: super::super::runtime::recovery::CONTINUE.into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                last_turn: state
                    .turns
                    .values()
                    .filter(|turn| {
                        turn.state != agent_protocol::conversation::TurnState::RolledBack
                    })
                    .max_by_key(|turn| turn.ordinal)
                    .map(|turn| turn.id.clone()),
                delivery: ContinuationDelivery::Pending,
            });
        }
        let mut report_committed = self.inner.conversation.update_report.lock().await;
        let storage = &self.inner.conversation.storage;
        storage.set_updating(true).map_err(anyhow::Error::msg)?;
        // The report lock hides this transaction's admission marker until the
        // replacement is durable. Failed preparation keeps the prior report live.
        let reset_storage = scopeguard::guard(storage, |storage| {
            if let Err(error) = storage.set_updating(false) {
                tracing::error!(target: "agent", operation="host.update.rollback", %error);
            }
        });
        agent_local_host::save_private_json(&directory.join(RECOVERY_FILE), &Recovery { tasks })?;
        *report_committed = true;
        let _ = scopeguard::ScopeGuard::into_inner(reset_storage);
        let _ = scopeguard::ScopeGuard::into_inner(reset);
        Ok(())
    }

    pub(in crate::host_rpc) async fn continuation_scheduled(
        &self,
        target: &agent_protocol::session::SessionRef,
        source: &agent_protocol::ids::TurnId,
        job: uuid::Uuid,
        outcome: Outcome,
    ) -> Result<(), String> {
        let Some((directory, _)) = self.inner.history_scope.get() else {
            return Ok(());
        };
        let _report = self.inner.conversation.update_report.lock().await;
        if *_report {
            return Ok(());
        }
        let path = directory.join(RECOVERY_FILE);
        let Some(mut report) = read_report(&path).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        let mut changed = false;
        for task in &mut report.tasks {
            if task.submission.thread_id != *target || task.last_turn.as_ref() != Some(source) {
                continue;
            }
            changed = true;
            task.submission.client_user_message_id = job.to_string().into();
            match &outcome {
                Outcome::Ignored => task.delivery = ContinuationDelivery::Completed,
                Outcome::DeliveryUnknown => {
                    task.delivery =
                        ContinuationDelivery::Failed("継続の配送結果を確認できません。".into())
                }
                Outcome::Rejected(reason) => {
                    task.delivery = ContinuationDelivery::Failed(reason.clone())
                }
                Outcome::Accepted => {}
            }
        }
        if changed {
            agent_local_host::save_private_json(&path, &report)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(in crate::host_rpc) async fn update_recovery_committed(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<(), String> {
        let Some((directory, _)) = self.inner.history_scope.get() else {
            return Ok(());
        };
        let _report = self.inner.conversation.update_report.lock().await;
        if *_report {
            return Ok(());
        }
        let path = directory.join(RECOVERY_FILE);
        let Some(mut report) = read_report(&path).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        let pending = self.inner.conversation.storage.continuations(target)?;
        let mut changed = false;
        for task in &mut report.tasks {
            if task.submission.thread_id == *target
                && matches!(task.delivery, ContinuationDelivery::Pending)
                && !task
                    .last_turn
                    .as_ref()
                    .is_some_and(|turn| pending.contains(turn))
            {
                let input = &task.submission.client_user_message_id;
                if self
                    .inner
                    .conversation
                    .storage
                    .item(target, &input.to_string().into())?
                    .is_none()
                    && self
                        .inner
                        .conversation
                        .storage
                        .input(target, input)?
                        .is_none()
                {
                    task.delivery = ContinuationDelivery::Completed;
                    changed = true;
                }
            }
        }
        if changed {
            agent_local_host::save_private_json(&path, &report)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub async fn resume_after_update(&self, directory: &std::path::Path) -> anyhow::Result<()> {
        let path = directory.join(RECOVERY_FILE);
        if !path.try_exists()? {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let pending = {
                let _report = self.inner.conversation.update_report.lock().await;
                if *_report {
                    return Ok(());
                }
                let Some(mut report) = read_report(&path)? else {
                    return Ok(());
                };
                let previous = serde_json::to_vec(&report)?;
                let mut pending = false;
                for task in &mut report.tasks {
                    if task.delivery == ContinuationDelivery::Sending {
                        task.delivery = ContinuationDelivery::Failed(
                            "継続の配送結果を確認できません。会話を開いて確認してください。".into(),
                        );
                    }
                    if !matches!(task.delivery, ContinuationDelivery::Pending) {
                        continue;
                    }
                    let target = &task.submission.thread_id;
                    if let Some(item) = self
                        .inner
                        .conversation
                        .storage
                        .item(
                            target,
                            &task.submission.client_user_message_id.to_string().into(),
                        )
                        .map_err(anyhow::Error::msg)?
                    {
                        task.delivery = ContinuationDelivery::Accepted(item.turn);
                    } else {
                        let completed = task
                            .last_turn
                            .as_ref()
                            .map(|turn| self.inner.conversation.storage.turn(target, turn))
                            .transpose()
                            .map_err(anyhow::Error::msg)?
                            .flatten()
                            .is_some_and(|turn| {
                                turn.state == agent_protocol::conversation::TurnState::Completed
                            });
                        if completed {
                            task.delivery = ContinuationDelivery::Completed;
                        } else {
                            let input = self
                                .inner
                                .conversation
                                .storage
                                .input(target, &task.submission.client_user_message_id)
                                .map_err(anyhow::Error::msg)?;
                            match input {
                                Some(input) => match &input.state {
                                    agent_protocol::conversation::InputState::Failed { error, .. } => task.delivery = ContinuationDelivery::Failed(error.as_ref().map_or_else(|| "準備を停止しました。".into(), |error| error.message.clone())),
                                    agent_protocol::conversation::InputState::Unknown { .. } => task.delivery = ContinuationDelivery::Failed("継続の配送結果を確認できません。会話を開いて確認してください。".into()),
                                    _ => pending = true,
                                },
                                None => pending = true,
                            }
                        }
                    }
                }
                if serde_json::to_vec(&report)? != previous {
                    agent_local_host::save_private_json(&path, &report)?;
                }
                pending
            };
            if !pending {
                return Ok(());
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "中断した会話の継続を確認できませんでした。"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
mod tests;
