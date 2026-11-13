//! The detached updater reads a progress report. Continuations themselves live
//! in the conversation database and use the durable job receipt exactly once.
use super::*;
use agent_local_host::update::{ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery};
use agent_protocol::conversation::Outcome;

impl HostRpcService {
    pub(crate) async fn prepare_update(&self) -> anyhow::Result<()> {
        let _workspace = self.inner.worktree_access.write().await;
        anyhow::ensure!(
            !self
                .inner
                .updating
                .swap(true, std::sync::atomic::Ordering::AcqRel),
            "ホストを更新中です。"
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
                super::super::runtime::Runtime::await_import(self, &session),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!("task_activity_incomplete: 履歴の読み込みが完了しませんでした。")
            })?
            .map_err(anyhow::Error::msg)?;
            let state = self
                .inner
                .conversation
                .storage
                .state(&session)
                .map_err(anyhow::Error::msg)?;
            tasks.push(InterruptedTask {
                session,
                input: uuid::Uuid::new_v4().to_string().into(),
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
        agent_local_host::save_private_json(&directory.join(RECOVERY_FILE), &Recovery { tasks })?;
        self.inner
            .conversation
            .storage
            .set_updating(true)
            .map_err(anyhow::Error::msg)?;
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
        let path = directory.join(RECOVERY_FILE);
        let mut report: Recovery = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let mut changed = false;
        for task in &mut report.tasks {
            if task.session != *target || task.last_turn.as_ref() != Some(source) {
                continue;
            }
            changed = true;
            task.input = job.to_string().into();
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
        let path = directory.join(RECOVERY_FILE);
        let mut report: Recovery = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let pending = self.inner.conversation.storage.continuations(target)?;
        let mut changed = false;
        for task in &mut report.tasks {
            if task.session == *target
                && matches!(task.delivery, ContinuationDelivery::Pending)
                && !task
                    .last_turn
                    .as_ref()
                    .is_some_and(|turn| pending.contains(turn))
            {
                let input = &task.input;
                if self
                    .inner
                    .conversation
                    .storage
                    .item(target, &input.to_string().into())?
                    .is_none()
                    && !self
                        .inner
                        .conversation
                        .storage
                        .state(target)?
                        .inputs
                        .contains_key(input)
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
                let mut report: Recovery = serde_json::from_slice(&std::fs::read(&path)?)?;
                let previous = serde_json::to_vec(&report)?;
                let mut pending = false;
                for task in &mut report.tasks {
                    if !matches!(task.delivery, ContinuationDelivery::Pending) {
                        continue;
                    }
                    let target = &task.session;
                    if let Some(item) = self
                        .inner
                        .conversation
                        .storage
                        .item(target, &task.input.to_string().into())
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
                            let state = self
                                .inner
                                .conversation
                                .storage
                                .state(target)
                                .map_err(anyhow::Error::msg)?;
                            match state.inputs.get(&task.input) {
                                Some(input) => match &input.state {
                                    agent_protocol::conversation::InputState::Failed(error) => task.delivery = ContinuationDelivery::Failed(error.message.clone()),
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
mod tests {
    use super::*;
    use agent_protocol::{
        conversation::{Action, Command, Context, Event, InputContent, OperationId},
        models::{Thread, ThreadResponse, Turn, TurnStatus},
        session::{ProviderKind, SessionRef},
    };

    #[tokio::test]
    async fn update_handover_continues_running_work_once_and_settles_stopped_or_retrying_work() {
        for mode in ["stopped", "retrying", "running"] {
            use crate::host_rpc::runtime::{Runtime, native_fixture::NativeAgent};
            use agent_protocol::conversation::{Observation, TurnState};
            let directory = tempfile::tempdir().unwrap();
            let agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
            let service = HostRpcService::new(
                [agent.backend()],
                crate::ProjectStore::new(directory.path().join("projects.json")),
            );
            service.set_history_scope(directory.path().into(), true);
            let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
            let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
            let storage = &service.inner.conversation.storage;
            storage
                .import(
                    &target,
                    &ThreadResponse {
                        thread: Thread {
                            id: Some(target.clone()),
                            active_provider: Some(ProviderKind::Codex),
                            cwd: Some(directory.path().to_str().unwrap().into()),
                            turns: Some(vec![]),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    Some(&native),
                    100,
                )
                .unwrap();
            let context = Context {
                now: 100,
                seed: uuid::Uuid::new_v4(),
                steers: true,
                external_busy: false,
            };
            let input = uuid::Uuid::new_v4();
            let admitted = storage
                .decide(
                    &target,
                    Event::Command {
                        id: OperationId::Client(input),
                        receipt: None,
                        command: Command::Submit {
                            id: input.to_string().into(),
                            content: InputContent {
                                parts: vec![op::Input::Text {
                                    text: "work".into(),
                                }],
                                model: None,
                                effort: None,
                                service_tier: None,
                                follow_up: Default::default(),
                            },
                        },
                    },
                    context,
                    None,
                )
                .unwrap();
            let Action::Deliver {
                id,
                action,
                attempt,
                ..
            } = &admitted.step.actions[0]
            else {
                panic!("delivery")
            };
            storage
                .decide(
                    &target,
                    Event::Delivered {
                        id: id.clone(),
                        action: *action,
                        native_turn: Some("native-turn".into()),
                    },
                    context,
                    None,
                )
                .unwrap();
            if mode == "retrying" {
                storage
                    .decide(
                        &target,
                        Event::Provider {
                            attempt: attempt.id,
                            native_turn: None,
                            native_items: vec![],
                            observation: Observation::Failed(
                                agent_protocol::execution::ExecutionError {
                                    message: "overloaded".into(),
                                    provider_code: Some("serverOverloaded".into()),
                                    ..Default::default()
                                },
                            ),
                        },
                        context,
                        None,
                    )
                    .unwrap();
            } else if mode == "stopped" {
                storage
                    .decide(
                        &target,
                        Event::Command {
                            id: OperationId::Client(uuid::Uuid::new_v4()),
                            receipt: None,
                            command: Command::Interrupt {
                                turn: Some(attempt.turn.clone()),
                            },
                        },
                        context,
                        None,
                    )
                    .unwrap();
            }
            agent.records.lock().unwrap().insert(
                native.id.clone(),
                ThreadResponse {
                    thread: Thread {
                        id: Some(native.clone()),
                        cwd: Some(directory.path().to_str().unwrap().into()),
                        status: agent_protocol::models::SessionStatus::Idle,
                        turns: Some(vec![]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let turn = attempt.turn.clone();
            service.prepare_update().await.unwrap();
            let operation = uuid::Uuid::new_v4();
            let rejected = Runtime::change(
                &service,
                &agent_protocol::conversation::records::Change {
                    session: target.clone(),
                    operation,
                    command: Command::Submit {
                        id: operation.to_string().into(),
                        content: InputContent {
                            parts: vec![op::Input::Text {
                                text: "must not start during update".into(),
                            }],
                            model: None,
                            effort: None,
                            service_tier: None,
                            follow_up: Default::default(),
                        },
                    },
                },
            )
            .await
            .unwrap_err();
            assert_eq!(rejected.code, "host_updating");
            assert!(storage.receipts(&target, &[operation]).unwrap().is_empty());
            drop(service);
            let service = HostRpcService::new(
                [agent.restarted().backend()],
                crate::ProjectStore::new(directory.path().join("projects.json")),
            );
            service.set_history_scope(directory.path().into(), true);
            let _connection = service.open_session();
            let storage = &service.inner.conversation.storage;
            Runtime::recover(&service, &target, ProviderKind::Codex)
                .await
                .unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                service.resume_after_update(directory.path()),
            )
            .await
            .unwrap()
            .unwrap();
            let report: Recovery = serde_json::from_slice(
                &std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap(),
            )
            .unwrap();
            assert_eq!(report.tasks.len(), 1);
            if mode == "running" {
                assert!(matches!(
                    report.tasks[0].delivery,
                    ContinuationDelivery::Accepted(_)
                ));
                service.resume_after_update(directory.path()).await.unwrap();
                assert_eq!(
                    agent.inputs.lock().unwrap().len(),
                    1,
                    "the durable continuation runs once"
                );
            } else {
                assert_eq!(report.tasks[0].delivery, ContinuationDelivery::Completed);
                assert!(agent.inputs.lock().unwrap().is_empty());
            }
            assert!(storage.continuations(&target).unwrap().is_empty());
            if mode == "stopped" {
                assert_eq!(
                    storage.state(&target).unwrap().turns[&turn].state,
                    TurnState::Interrupted
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupted_original_turn_does_not_confirm_a_pending_update_continuation() {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [],
            crate::ProjectStore::new(directory.path().join("projects.json")),
        );
        let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let storage = &service.inner.conversation.storage;
        storage
            .import(
                &target,
                &ThreadResponse {
                    thread: Thread {
                        id: Some(target.clone()),
                        active_provider: Some(ProviderKind::Codex),
                        cwd: Some("/project".into()),
                        turns: Some(vec![std::sync::Arc::new(Turn {
                            id: "original".into(),
                            status: TurnStatus::Interrupted,
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(&native),
                100,
            )
            .unwrap();
        let input = uuid::Uuid::new_v4();
        let path = directory.path().join(RECOVERY_FILE);
        agent_local_host::save_private_json(
            &path,
            &Recovery {
                tasks: vec![InterruptedTask {
                    session: target.clone(),
                    input: input.to_string().into(),
                    last_turn: Some("original".into()),
                    delivery: ContinuationDelivery::Pending,
                }],
            },
        )
        .unwrap();
        let mut resumed = Box::pin(service.resume_after_update(directory.path()));
        assert!(futures_util::poll!(&mut resumed).is_pending());
        let read = || serde_json::from_slice::<Recovery>(&std::fs::read(&path).unwrap()).unwrap();
        assert!(matches!(
            read().tasks[0].delivery,
            ContinuationDelivery::Pending
        ));
        let context = Context {
            now: 200,
            seed: uuid::Uuid::new_v4(),
            steers: true,
            external_busy: false,
        };
        let admitted = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(input),
                    receipt: None,
                    command: Command::Submit {
                        id: input.to_string().into(),
                        content: InputContent {
                            parts: vec![op::Input::Text {
                                text: "Continue".into(),
                            }],
                            model: None,
                            effort: None,
                            service_tier: None,
                            follow_up: Default::default(),
                        },
                    },
                },
                context,
                None,
            )
            .unwrap();
        let Action::Deliver {
            id,
            action,
            attempt,
            ..
        } = &admitted.step.actions[0]
        else {
            panic!("continuation delivery")
        };
        storage
            .decide(
                &target,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: Some("continued".into()),
                },
                context,
                None,
            )
            .unwrap();
        tokio::time::advance(std::time::Duration::from_millis(50)).await;
        resumed.await.unwrap();
        assert!(
            matches!(&read().tasks[0].delivery,ContinuationDelivery::Accepted(turn) if turn==&attempt.turn)
        );
        service.resume_after_update(directory.path()).await.unwrap();
        assert_eq!(
            storage
                .history(&target, 0)
                .unwrap()
                .thread
                .turns
                .unwrap()
                .len(),
            2
        );
    }
}
