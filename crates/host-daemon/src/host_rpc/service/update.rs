use super::*;
use agent_local_host::update::{ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery};

impl HostRpcService {
    async fn restart_snapshot(
        &self,
        session: &agent_protocol::session::SessionRef,
    ) -> Result<ThreadResponse, Failure> {
        let response = self
            .inner
            .providers
            .get(session.provider)?
            .open(&session.id, 1, true)
            .await?;
        op::validate_thread(&response, Some(session))
            .map_err(|error| Failure::new("invalid_session", error))?;
        Ok(response)
    }

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
            .read_task_activity(true)
            .await
            .map_err(anyhow::Error::msg)?;
        let mut tasks = Vec::new();
        for (session, status) in activity.statuses {
            if status != agent_protocol::models::SessionStatus::Running {
                continue;
            }
            let row = self
                .inner
                .conversations
                .get(&session)
                .map_err(anyhow::Error::msg)?;
            let active = row.as_ref().map_or(&session, |row| &row.active);
            let response = self
                .restart_snapshot(active)
                .await
                .map_err(anyhow::Error::msg)?;
            let options = self
                .inner
                .model_selections
                .resolve(
                    active,
                    response.model.as_ref(),
                    response.effort.as_deref(),
                    response.service_tier.as_deref(),
                )
                .map_err(anyhow::Error::msg)?;
            tasks.push(InterruptedTask {
                native_session: active.clone(),
                cwd: response.thread.cwd,
                last_turn: response.thread.turns.as_ref().and_then(|turns| turns.last()).map(|turn| turn.id.clone()),
                submission: op::Submission {
                fresh_context: false,
                    thread_id: session,
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    input: vec![op::Input::Text { text: "Bexの更新に伴う再起動で作業が中断されました。この同じ会話の履歴から進捗と未完了の作業を確認し、中断したところから続けてください。元の依頼を最初からやり直したり、代わりの会話を作成したりしないでください。".into() }],
                    model: options.model,
                    effort: options.effort,
                    service_tier: options.service_tier,
                    follow_up: Default::default(),
                },
                delivery: ContinuationDelivery::Pending,
            });
        }
        agent_local_host::save_private_json(&directory.join(RECOVERY_FILE), &Recovery { tasks })?;
        let _ = scopeguard::ScopeGuard::into_inner(reset);
        Ok(())
    }

    pub async fn resume_after_update(&self, directory: &std::path::Path) -> anyhow::Result<()> {
        let path = directory.join(RECOVERY_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut recovery: Recovery = serde_json::from_slice(&bytes)?;
        let connection = scopeguard::guard(self.open_session(), |connection| {
            self.close_session(connection.id())
        });
        for index in 0..recovery.tasks.len() {
            if recovery.tasks[index].delivery != ContinuationDelivery::Pending {
                continue;
            }
            let response = self
                .restart_snapshot(&recovery.tasks[index].native_session)
                .await;
            match response {
                Ok(response)
                    if response
                        .thread
                        .turns
                        .as_ref()
                        .and_then(|turns| turns.last())
                        .is_some_and(|turn| {
                            Some(&turn.id) == recovery.tasks[index].last_turn.as_ref()
                                && turn.status == agent_protocol::execution::TurnStatus::Completed
                        }) =>
                {
                    recovery.tasks[index].delivery = ContinuationDelivery::Completed;
                    agent_local_host::save_private_json(&path, &recovery)?;
                    continue;
                }
                Err(error) => {
                    recovery.tasks[index].delivery =
                        ContinuationDelivery::Failed(error.to_string());
                    agent_local_host::save_private_json(&path, &recovery)?;
                    continue;
                }
                _ => {}
            }
            recovery.tasks[index].delivery = ContinuationDelivery::Sending;
            agent_local_host::save_private_json(&path, &recovery)?;
            let input = recovery.tasks[index].submission.clone();
            let result = self.dispatch(connection.id(), &Call::Submit(input)).await;
            let receipt = result.and_then(|reply| {
                match agent_protocol::protocol::decode::<Response<op::SubmissionReceipt>>(
                    &reply.initial,
                )
                .map_err(|e| e.to_string())?
                {
                    Response::Success { result } => result
                        .turn_id
                        .ok_or_else(|| "継続した応答を確認できません。".into()),
                    Response::Failure { error } => Err(error.message),
                }
            });
            recovery.tasks[index].delivery = match receipt {
                Ok(turn) => ContinuationDelivery::Accepted(turn),
                Err(error) => ContinuationDelivery::Failed(error),
            };
            agent_local_host::save_private_json(&path, &recovery)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_preparation_does_not_leave_the_host_frozen() {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        assert!(service.prepare_update().await.is_err());
        service.set_history_scope(directory.path().to_owned(), true);
        service.prepare_update().await.unwrap();
        let saved: Recovery =
            serde_json::from_slice(&std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap())
                .unwrap();
        assert!(saved.tasks.is_empty());
        let broken = HostRpcService::new(
            [Backend::unavailable(ProviderKind::Codex, "not installed")],
            ProjectStore::new(directory.path().join("broken.json")),
        );
        broken.set_history_scope(directory.path().to_owned(), true);
        broken.prepare_update().await.unwrap();
    }

    #[tokio::test]
    async fn restart_snapshot_skips_codex_that_never_started() {
        let directory = tempfile::tempdir().unwrap();
        // Codex retains an adapter after a failed spawn so its CLI can still
        // be installed/updated. It has no process, unlike a broken live read.
        let missing = HostRpcService::new(
            [crate::providers::codex::Codex::new(Err("Codex is not installed".into())).into()],
            ProjectStore::new(directory.path().join("missing-codex.json")),
        );
        missing.set_history_scope(directory.path().to_owned(), true);
        assert!(missing.read_task_activity(false).await.is_err());
        missing.prepare_update().await.unwrap();
        let saved: Recovery =
            serde_json::from_slice(&std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap())
                .unwrap();
        assert!(saved.tasks.is_empty());
    }
}
