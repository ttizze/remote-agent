use super::*;
use agent_protocol::{
    automations::{self as automation, Automation, RunState},
    conversation::{self as decision, records},
    session::SessionRef,
};

/// A committed receipt certifies acceptance or rejection of the dispatch.
fn receipt_state(receipt: records::Acknowledged) -> RunState {
    receipt
        .error
        .map_or(RunState::Accepted, |message| RunState::Failed { message })
}

impl HostRpcService {
    pub(super) async fn automations(
        &self,
        request: &automation::Request,
    ) -> Result<automation::Reply, Failure> {
        request
            .validate()
            .map_err(|e| Failure::new("invalid_params", e))?;
        self.require_conversation(&request.session)?;
        if !matches!(request.action, automation::Action::List) {
            self.require_not_updating()?;
        }
        self.reconcile_automations(Some(&request.session))?;
        if let Some(value) = self
            .inner
            .automations
            .change(request, super::super::runtime::now())
            .map_err(|e| Failure::new("automation_failed", e))?
        {
            self.dispatch_automation(value).await;
        }
        self.inner
            .automations
            .list(Some(&request.session))
            .map_err(|e| Failure::new("automation_failed", e))
    }
    fn reconcile_automations(&self, session: Option<&SessionRef>) -> Result<(), Failure> {
        for value in self
            .inner
            .automations
            .uncertain(session)
            .map_err(|e| Failure::new("automation_failed", e))?
        {
            let Some(run) = value
                .last_run
                .as_ref()
                .filter(|run| matches!(run.state, RunState::Sending | RunState::Unknown))
            else {
                continue;
            };
            if let Some((_, receipt)) = self
                .inner
                .conversation
                .storage
                .receipts(&value.definition.session, &[run.id])
                .map_err(super::super::runtime::storage_failure)?
                .into_iter()
                .next()
            {
                self.inner
                    .automations
                    .finish(
                        &value.definition.session,
                        value.definition.id,
                        run.id,
                        receipt_state(receipt),
                    )
                    .map_err(|e| Failure::new("automation_failed", e))?;
            }
        }
        Ok(())
    }
    async fn dispatch_automation(&self, value: Automation) {
        let Some(run) = &value.last_run else {
            return;
        };
        let definition = &value.definition;
        let request = records::Change {
            session: definition.session.clone(),
            operation: run.id,
            command: decision::Command::Submit {
                id: run.id.to_string().into(),
                content: decision::InputContent {
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: definition.prompt.clone(),
                    }],
                    model: definition.model.clone(),
                    effort: definition.effort.clone(),
                    service_tier: definition.service_tier.clone(),
                    follow_up: op::FollowUp::Queue,
                },
            },
        };
        let automation = definition.id.to_string();
        let state = match super::super::runtime::Runtime::change_from(
            self,
            &request,
            super::super::runtime::Origin::Automation(&automation),
        )
        .await
        {
            Ok(receipt) => receipt_state(receipt),
            Err(error) if error.delivery == agent_protocol::error::Delivery::NotSent => {
                RunState::Failed {
                    message: error.message,
                }
            }
            Err(_) => RunState::Unknown,
        };
        if let Err(error) =
            self.inner
                .automations
                .finish(&definition.session, definition.id, run.id, state)
        {
            tracing::error!(operation="automation.finish", message=%error);
        }
    }
    pub(super) fn start_automations(&self) {
        let owner = Arc::downgrade(&self.inner);
        let stop = self.inner.automation_stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {} }
                let Some(inner) = owner.upgrade() else {
                    break;
                };
                let service = Self { inner };
                if service.require_not_updating().is_err() {
                    continue;
                }
                if service.reconcile_automations(None).is_err() {
                    continue;
                }
                let Ok(due) = service.inner.automations.due(super::super::runtime::now()) else {
                    continue;
                };
                for value in due {
                    if stop.is_cancelled() {
                        break;
                    }
                    service.dispatch_automation(value).await;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
    use uuid::Uuid;

    #[tokio::test]
    async fn manual_runs_use_the_normal_queue_and_carry_the_saved_automation_identity() {
        let root = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex);
        let host = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
        let connection = host.open_session();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let _: records::Opened = call(
            &host,
            connection.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                session: target.clone(),
                provider: ProviderKind::Codex,
                subscription: Uuid::new_v4(),
                cwd: Some(root.path().to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: None,
            }),
        )
        .await
        .unwrap();
        let definition = automation::Definition {
            id: Uuid::new_v4(),
            session: target.clone(),
            title: "Daily review".into(),
            prompt: "Check working changes".into(),
            model: None,
            effort: None,
            service_tier: None,
            schedule: automation::Schedule::Interval { every_ms: 60_000 },
            enabled: false,
        };
        let saved: automation::Reply = call(
            &host,
            connection.id(),
            Call::Automations(automation::Request {
                session: target.clone(),
                action: automation::Action::Save {
                    definition: definition.clone().into(),
                },
            }),
        )
        .await
        .unwrap();
        assert_eq!(saved.automations.len(), 1);
        assert_eq!(agent.inputs.lock().unwrap().len(), 0);
        let result: automation::Reply = call(
            &host,
            connection.id(),
            Call::Automations(automation::Request {
                session: target.clone(),
                action: automation::Action::RunNow { id: definition.id },
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            result.automations[0].last_run.as_ref().unwrap().state,
            RunState::Accepted
        );
        let run = result.automations[0].last_run.as_ref().unwrap().id;
        let item_id =
            agent_protocol::orchestration::user_item_id(&target.application_id(), &run.to_string());
        let user = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(item) = host
                    .inner
                    .conversation
                    .storage
                    .thread_item(&target, &item_id)
                    .unwrap()
                    && !agent.inputs.lock().unwrap().is_empty()
                {
                    break item;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // T3 scheduled task input: system-authored, carrying the automation id.
        let agent_protocol::orchestration::TurnItemBody::UserMessage {
            created_by,
            scheduled_task_id,
            ..
        } = user.body
        else {
            panic!("user message")
        };
        assert_eq!(created_by, agent_protocol::orchestration::Actor::System);
        assert_eq!(scheduled_task_id, Some(definition.id.to_string()));
        assert_eq!(agent.inputs.lock().unwrap().len(), 1);
        assert!(host.inner.automations.due(i64::MAX).unwrap().is_empty());
        host.shutdown_owned_processes().await;
    }
}
