//! The detached updater reads a progress report. Continuations themselves live
//! in the canonical Run/effect records, as in T3 RestartContinuation.
use super::*;
use agent_local_host::update::{ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery};
use agent_protocol::orchestration as model;

fn read_report(path: &std::path::Path) -> anyhow::Result<Option<Recovery>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

// T3 updateHandoff.desktopUpdateRestartPending consumes a one-minute marker.
// Keep it in the detached updater's report so marker + tasks publish atomically.
pub(super) fn consume_restart_marker(path: &std::path::Path) -> anyhow::Result<bool> {
    let Some(mut report) = read_report(path)? else {
        return Ok(false);
    };
    let Some(marker) = report.restart_marker.take() else {
        return Ok(false);
    };
    agent_local_host::save_private_json(path, &report)?;
    Ok((super::super::runtime::now().max(0) as u64).saturating_sub(marker) < 60_000)
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
        let shell = self
            .inner
            .conversation
            .storage
            .shell_snapshot()
            .map_err(anyhow::Error::msg)?;
        let activity = agent_protocol::orchestration::shell::activity(
            shell
                .threads
                .iter()
                .chain(&shell.archived_threads)
                .map(AsRef::as_ref),
        )
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
                .thread_state(&session.application_id(), &[])
                .map_err(anyhow::Error::msg)?;
            // T3 continues application runs, not unowned native work or a
            // completed parent whose detached background work is still shown.
            let Some(run) = state.active_run() else {
                continue;
            };
            tasks.push(InterruptedTask {
                native_session: self
                    .inner
                    .conversation
                    .storage
                    .active_source(&session)
                    .map_err(anyhow::Error::msg)?
                    .unwrap_or_else(|| session.clone()),
                cwd: state.thread.worktree_path.clone(),
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
                last_turn: Some(run.id.clone().into()),
                delivery: ContinuationDelivery::Pending,
            });
        }
        let mut report_committed = self.inner.conversation.update_report.lock().await;
        agent_local_host::save_private_json(
            &directory.join(RECOVERY_FILE),
            &Recovery {
                tasks,
                restart_marker: Some(super::super::runtime::now().max(0) as u64),
            },
        )?;
        *report_committed = true;
        let _ = scopeguard::ScopeGuard::into_inner(reset);
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
                    let Some(source) = task.last_turn.as_ref() else {
                        task.delivery =
                            ContinuationDelivery::Failed("継続元の実行を確認できません。".into());
                        continue;
                    };
                    let state = self
                        .inner
                        .conversation
                        .storage
                        .restart_continuation_state(
                            &task.submission.thread_id.application_id(),
                            source.as_str(),
                        )
                        .map_err(anyhow::Error::msg)?;
                    task.delivery = continuation_delivery(&state, source.as_str());
                    if let ContinuationDelivery::Accepted(run) = &task.delivery
                        && let Some(saved) = state.runs.get(run.as_str())
                    {
                        task.submission.client_user_message_id =
                            saved.user_message_id.clone().into();
                    }
                    pending |= matches!(task.delivery, ContinuationDelivery::Pending);
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

// The stable source run and T3 restartContinuationOfRunId are the proof. A
// transport reply or a new default provider is not evidence of input delivery.
fn continuation_delivery(state: &model::State, source: &str) -> ContinuationDelivery {
    if let Some(run) = state
        .runs
        .values()
        .find(|run| run.restart_continuation_of_run_id.as_deref() == Some(source))
    {
        if state
            .active_attempt(run.active_attempt_id.as_deref())
            .is_some_and(|attempt| attempt.provider_turn_id.is_some())
        {
            return ContinuationDelivery::Accepted(run.id.clone().into());
        }
        let error = state.turn_items.values().find_map(|item| {
            if item.run_id.as_deref() != Some(&run.id) {
                return None;
            }
            match &item.body {
                model::TurnItemBody::Error { failure, .. } => Some(failure.message.clone()),
                _ => None,
            }
        });
        if state.turn_items.values().any(|item|item.run_id.as_deref()==Some(&run.id) && matches!(&item.body,model::TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")) {
            return ContinuationDelivery::Failed("継続の配送結果を確認できません。会話を開いて確認してください。".into());
        }
        if matches!(
            run.status,
            model::RunStatus::Failed
                | model::RunStatus::Cancelled
                | model::RunStatus::Interrupted
                | model::RunStatus::RolledBack
                | model::RunStatus::Completed
        ) {
            return ContinuationDelivery::Failed(
                error.unwrap_or_else(|| "継続入力の受け付けを確認できません。".into()),
            );
        }
        return ContinuationDelivery::Pending;
    }
    if let Some(effect)=state.effects.values().find(|effect|matches!(&effect.request,model::EffectRequest::ProviderRuntimeContinue {source_run_id} if source_run_id==source)) {
        if matches!(effect.status,model::EffectStatus::Pending|model::EffectStatus::Running) {return ContinuationDelivery::Pending;}
        if effect.status==model::EffectStatus::Failed {return ContinuationDelivery::Failed(effect.last_error.clone().unwrap_or_else(||"継続の準備に失敗しました。".into()));}
    }
    match state.runs.get(source).map(|run| run.status) {
        Some(
            model::RunStatus::Completed
            | model::RunStatus::Cancelled
            | model::RunStatus::Interrupted
            | model::RunStatus::Failed
            | model::RunStatus::RolledBack,
        ) => ContinuationDelivery::Completed,
        Some(_) => ContinuationDelivery::Pending,
        None => ContinuationDelivery::Failed("継続元の実行を確認できません。".into()),
    }
}

#[cfg(test)]
mod tests;
