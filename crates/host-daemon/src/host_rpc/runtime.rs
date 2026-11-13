//! Resource execution for committed conversation decisions. Provider I/O never
//! runs inside a conversation receiver or a database transaction.
use super::{
    owners::{Effect, Owners},
    service::HostRpcService,
    storage::Storage,
};
use crate::providers::{AgentChange, Failure, RequestOrigin, SubmissionTarget};
use agent_protocol::{
    conversation::{
        self as decision, Action, Attempt, Command, Context, Event, Observation, Outcome, records,
    },
    ids::{ClientInputId, RequestId},
    operations as op,
    session::{PreparationStage, ProviderKind, SessionChange, SessionRef},
    sync::Cursor,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use uuid::Uuid;

mod bodies;
mod events;
pub(in crate::host_rpc) mod list;
#[cfg(test)]
pub(in crate::host_rpc) mod native_fixture;
pub(in crate::host_rpc) mod output;
mod preparation;
mod recovery;
mod resources;
mod rpc;
#[cfg(test)]
mod tests;
mod text;

pub(super) struct Runtime {
    pub storage: Arc<Storage>,
    pub owners: Arc<Owners>,
    effects: Mutex<Option<mpsc::UnboundedReceiver<Effect>>>,
    resources: Mutex<HashMap<SessionRef, Arc<resources::Source>>>,
    preparations: Mutex<HashMap<SessionRef, Arc<preparation::Preparation>>>,
    catalog_access: HashMap<ProviderKind, tokio::sync::RwLock<()>>,
    requests: Mutex<HashMap<RequestId, resources::PendingRequest>>,
    outputs: Arc<output::Outputs>,
    recovered: Mutex<HashMap<SessionRef, Arc<tokio::sync::OnceCell<()>>>>,
    updating_recovery: bool,
    pub(in crate::host_rpc) update_report: AsyncMutex<()>,
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn execution_error(error: Failure) -> agent_protocol::execution::ExecutionError {
    error
        .execution
        .map(|error| *error)
        .unwrap_or(agent_protocol::execution::ExecutionError {
            message: error.message,
            ..Default::default()
        })
}

pub(in crate::host_rpc) fn storage_failure(error: String) -> Failure {
    Failure::new("conversation_storage_failed", error)
}

impl Runtime {
    pub(super) fn new(
        storage: Arc<Storage>,
        providers: impl IntoIterator<Item = ProviderKind>,
    ) -> Self {
        let updating_recovery = storage.updating().unwrap_or(false);
        let (owners, effects) = Owners::new(storage.clone());
        Self {
            storage,
            owners,
            effects: Mutex::new(Some(effects)),
            resources: Default::default(),
            preparations: Default::default(),
            catalog_access: providers
                .into_iter()
                .map(|provider| (provider, Default::default()))
                .collect(),
            requests: Default::default(),
            outputs: Default::default(),
            recovered: Default::default(),
            updating_recovery,
            update_report: Default::default(),
        }
    }

    pub(super) fn cancel_preparations(&self) {
        for preparation in self.preparations.lock().unwrap().values() {
            if let Some((_, cancel)) = &*preparation.active.lock().unwrap() {
                cancel.cancel();
            }
        }
    }

    fn recovery(&self, target: &SessionRef) -> Arc<tokio::sync::OnceCell<()>> {
        self.recovered
            .lock()
            .unwrap()
            .entry(target.clone())
            .or_default()
            .clone()
    }

    pub(super) fn start(service: &HostRpcService) {
        let Some(mut effects) = service.inner.conversation.effects.lock().unwrap().take() else {
            return;
        };
        let owner = Arc::downgrade(&service.inner);
        service.restore_setup_expiry();
        Self::start_list_updates(service);
        Self::start_recovery(service);
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            while let Some(effect) = effects.recv().await {
                while let Some(result) = workers.try_join_next() {
                    if let Err(error) = result {
                        tracing::error!(target: "agent", operation="conversation.executor", %error);
                    }
                }
                let Some(inner) = owner.upgrade() else {
                    break;
                };
                workers.spawn(async move {
                    let service = HostRpcService { inner };
                    if let Err(error) = Self::execute(&service, effect).await {
                        tracing::error!(target: "agent", operation="conversation.executor", %error);
                    }
                });
            }
            workers.abort_all();
        });
    }

    fn start_list_updates(service: &HostRpcService) {
        let owner = Arc::downgrade(&service.inner);
        let mut changed = service.inner.conversation.storage.changes();
        tokio::spawn(async move {
            let mut replica = records::Replica::new(agent_protocol::sync::Scope::List, 0);
            loop {
                let Some(inner) = owner.upgrade() else { break };
                let storage = inner.conversation.storage.clone();
                let cached = replica.cursor();
                let next = tokio::task::spawn_blocking(move || storage.list(cached))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
                    .and_then(|message| {
                        replica.apply(0, message).map_err(|error| error.to_string())
                    });
                let published = match next {
                    Ok(next) => {
                        replica = next;
                        true
                    }
                    Err(error) => {
                        tracing::error!(target:"agent",operation="list.activity",%error);
                        replica = records::Replica::new(agent_protocol::sync::Scope::List, 0);
                        false
                    }
                };
                if published && let Some(apns) = inner.router.apns() {
                    let headers = replica.rows().values().filter_map(|row| {
                        let record = match &row.content {
                            agent_protocol::sync::Content::Full(record)
                            | agent_protocol::sync::Content::Summary(record) => record,
                        };
                        match (&row.key, record.as_ref()) {
                            (
                                agent_protocol::sync::Key::Thread(id),
                                records::Record::Thread(header),
                            ) => Some((id, header)),
                            _ => None,
                        }
                    });
                    let activity = records::list_activity(headers);
                    if inner.providers.startup_errors().next().is_none()
                        && inner
                            .conversation
                            .storage
                            .catalog_complete()
                            .unwrap_or(false)
                    {
                        apns.activate(activity.display)
                    } else {
                        apns.update(activity.display)
                    }
                }
                drop(inner);
                if changed.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    fn context(service: &HostRpcService, provider: Option<ProviderKind>) -> Context {
        Context {
            external_busy: false,
            now: now(),
            seed: Uuid::new_v4(),
            steer_ready: true,
            steers: provider
                .and_then(|provider| service.inner.providers.get(provider).ok())
                .is_some_and(|agent| agent.capabilities().steer),
        }
    }

    pub(super) async fn event(
        service: &HostRpcService,
        target: &SessionRef,
        event: Event,
    ) -> Result<(Outcome, Cursor), String> {
        let header = service.inner.conversation.storage.lookup(target)?;
        let provider = header
            .as_ref()
            .map(|header| header.state.provider)
            .or(target.provider);
        if let Some(header) = header {
            Self::recover(service, target, header.state.provider).await?;
        }
        let result = service
            .inner
            .conversation
            .owners
            .input(target, event, Self::context(service, provider), None)
            .await?;
        service.inner.conversation.prune_requests(target);
        Ok(result)
    }

    pub(super) async fn change(
        service: &HostRpcService,
        request: &records::Change,
    ) -> Result<records::Acknowledged, Failure> {
        if let Some((_, receipt)) = service
            .inner
            .conversation
            .storage
            .receipts(&request.session, &[request.operation])
            .map_err(storage_failure)?
            .into_iter()
            .next()
        {
            Self::await_resolution(service, request, &receipt.outcome)
                .await
                .map_err(storage_failure)?;
            return Ok(receipt);
        }
        // Catalog-only rows have no authoritative history or provider
        // binding yet. An operation cannot start a second native session.
        if request.session.provider.is_some()
            && !service
                .inner
                .conversation
                .storage
                .loaded(&request.session)
                .map_err(storage_failure)?
        {
            Self::await_import(service, &request.session).await?;
        }
        let _workspace = if matches!(
            request.command,
            Command::Submit { .. }
                | Command::ResumeQueue
                | Command::SteerInput(_)
                | Command::RetryInput { .. }
                | Command::ResolveUnknown { retry: true, .. }
                | Command::Answer { .. }
                | Command::Rewind(_)
        ) {
            let guard = service.inner.worktree_access.read().await;
            service.require_not_updating()?;
            Some(guard)
        } else {
            None
        };
        let admission = (|| {
            if let Command::Interrupt { turn } = &request.command {
                let header = service
                    .inner
                    .conversation
                    .storage
                    .header(&request.session)
                    .map_err(storage_failure)?;
                let state = service
                    .inner
                    .conversation
                    .storage
                    .state(&request.session)
                    .map_err(storage_failure)?;
                let starting = state.inputs.values().any(|input| {
                    matches!(
                        input.state,
                        decision::InputState::Preparing {
                            stage: PreparationStage::Provider,
                            ..
                        }
                    ) && turn
                        .as_ref()
                        .is_none_or(|turn| turn.as_str() == input.id.as_str())
                });
                if starting
                    && header
                        .details
                        .workspace
                        .as_ref()
                        .and_then(|workspace| workspace.setup.as_ref())
                        .is_some_and(|setup| {
                            !setup.can_cancel
                                && setup.phase == agent_protocol::workspace::SetupPhase::Running
                        })
                {
                    return Err(Failure::new(
                        "preparation_started",
                        "The agent has already started",
                    ));
                }
            }
            if let Command::Submit { content, .. } = &request.command {
                let current = service
                    .inner
                    .conversation
                    .storage
                    .lookup(&request.session)
                    .map_err(storage_failure)?;
                let Some(current) = current else {
                    return Ok(());
                };
                let provider = content
                    .model
                    .as_ref()
                    .map_or(current.state.provider, |model| model.provider);
                service
                    .creation_agent(provider, content.model.as_ref())?
                    .availability()?;
                if current.details.can_accept_direct_input == Some(false) {
                    return Err(Failure::new(
                        "read_only_conversation",
                        "この会話は親の会話から操作してください。",
                    ));
                }
            }
            Ok::<_, Failure>(())
        })();
        let event = match admission {
            Ok(()) => Event::Command {
                id: decision::OperationId::Client(request.operation),
                command: request.command.clone(),
                receipt: None,
            },
            Err(error) => Event::RejectedCommand {
                id: decision::OperationId::Client(request.operation),
                reason: error.message,
                receipt: None,
            },
        };
        let (outcome, cursor) = Self::event(service, &request.session, event)
            .await
            .map_err(storage_failure)?;
        Self::await_resolution(service, request, &outcome)
            .await
            .map_err(storage_failure)?;
        Ok(records::Acknowledged { outcome, cursor })
    }

    async fn await_resolution(
        service: &HostRpcService,
        request: &records::Change,
        outcome: &Outcome,
    ) -> Result<(), String> {
        let Command::ResolveUnknown { id, .. } = &request.command else {
            return Ok(());
        };
        if *outcome != Outcome::Accepted {
            return Ok(());
        }
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        loop {
            let state = storage.state(&request.session)?;
            if !state.inputs.get(id).is_some_and(|input| {
                matches!(
                    input.state,
                    decision::InputState::Unknown {
                        status: decision::UnknownState::Resolving { .. },
                        ..
                    }
                )
            }) {
                return Ok(());
            }
            changed.changed().await.map_err(|error| error.to_string())?;
        }
    }

    async fn execute(service: &HostRpcService, effect: Effect) -> Result<(), String> {
        let Effect { target, action } = effect;
        let runtime = &service.inner.conversation;
        if !runtime.storage.state(&target)?.permits(&action)
            && !matches!(action, Action::Resume { .. })
        {
            return Ok(());
        }
        let native = match &action {
            Action::Deliver { attempt, .. }
            | Action::Resume { attempt, .. }
            | Action::Interrupt { attempt, .. }
            | Action::FenceDelivery { attempt, .. } => Some(
                SessionRef::new(attempt.provider, attempt.source.clone()).map_err(str::to_owned)?,
            ),
            _ => None,
        };
        let _retire = scopeguard::guard(native, |native| {
            if let Some(native) = native {
                runtime.retire_source(&native);
            }
        });
        match action {
            Action::Prepare {
                id,
                action,
                stage,
                cwd,
                provider,
            } => {
                Self::prepare_effect(service, &target, id, action, stage, cwd, provider).await?;
            }
            Action::Deliver {
                id,
                action,
                attempt,
                cwd,
                content,
                steer,
            } => {
                Self::deliver(service, &target, (id, action), attempt, cwd, content, steer).await?;
            }
            Action::Interrupt {
                ref attempt,
                action: token,
                background,
            } => {
                let source = runtime.source(&target, attempt);
                let _io = source.io.lock().await;
                if !runtime.storage.state(&target)?.permits(&action) {
                    return Ok(());
                }
                // Resume can still be waiting for the workspace lock. Keep Stop
                // pending until Resume cancels or the first native turn is bound.
                let Some(native) = runtime
                    .storage
                    .execution(&target, attempt.id)?
                    .and_then(|execution| execution.native_turn)
                else {
                    return Ok(());
                };
                let result = async {
                    service
                        .inner
                        .providers
                        .get(attempt.provider)?
                        .interrupt(&attempt.source, &native)
                        .await
                }
                .await;
                if background || result.is_err() {
                    let _updates = source.updates.lock().await;
                    let tasks = source.background(attempt.id);
                    if background && result.is_ok() && tasks.is_empty() {
                        Self::complete_command_outputs(service, &target, attempt.id, Some(native))
                            .await?;
                    }
                    Self::event(
                        service,
                        &target,
                        Event::InterruptResult {
                            attempt: attempt.id,
                            action: token,
                            background: tasks,
                            error: result.err().map(execution_error),
                        },
                    )
                    .await?;
                }
            }
            Action::Answer {
                attempt,
                id,
                action: token,
                answer,
            } => {
                Self::answer(service, &target, attempt, id, token, answer).await?;
            }
            Action::FenceDelivery {
                ref id,
                action: token,
                ref attempt,
            } => {
                let source = runtime.source(&target, attempt);
                let _io = source.io.lock().await;
                let Some(unload) = runtime
                    .storage
                    .fence_delivery(&target, id, token, attempt)?
                else {
                    return Ok(());
                };
                source.cancel_background(attempt.id);
                if unload {
                    let cleanup = async {
                        service
                            .inner
                            .providers
                            .get(attempt.provider)
                            .map_err(|error| error.to_string())?
                            .unload(&attempt.source)
                            .await
                            .map_err(|error| error.to_string())
                    }
                    .await;
                    if let Err(error) = cleanup {
                        tracing::warn!(target:"agent",operation="conversation.fence.unload",%error);
                    }
                }
                Self::event(
                    service,
                    &target,
                    Event::DeliveryFenced {
                        id: id.clone(),
                        action: token,
                    },
                )
                .await?;
            }
            Action::Resume {
                attempt,
                cwd,
                error,
            } => {
                Self::resume(service, &target, &attempt, &cwd, error).await?;
            }
            Action::WaitUntil(at) => {
                let delay = at.saturating_sub(now()).max(0) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                Self::event(service, &target, Event::Wake).await?;
            }
            Action::CancelPreparation { action } => {
                if let Some(preparation) = runtime.preparations.lock().unwrap().get(&target)
                    && let Some((current, cancel)) = &*preparation.active.lock().unwrap()
                    && *current == action
                {
                    cancel.cancel();
                }
                let state = runtime.storage.state(&target)?;
                if !state
                    .inputs
                    .values()
                    .any(|input| matches!(input.state, decision::InputState::Preparing { .. }))
                    && let Some(workspace) = runtime.storage.header(&target)?.details.workspace
                    && workspace
                        .setup
                        .as_ref()
                        .is_some_and(|setup| setup.can_cancel)
                {
                    service
                        .setup_update(
                            &target,
                            workspace.generation,
                            super::storage::workspace::SetupChange::Finish {
                                phase: agent_protocol::workspace::SetupPhase::Cancelled,
                                error: None,
                            },
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }
}
