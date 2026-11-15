use super::*;
use agent_protocol::{
    execution::{BackgroundTask, BackgroundTaskKind, ItemStatus},
    items::ItemBody,
    models::Turn,
};

pub(super) fn background(turn: &Turn) -> Vec<BackgroundTask> {
    turn.items
        .iter()
        .flatten()
        .filter(|item| item.status == ItemStatus::Running)
        .filter_map(|item| {
            let (kind, description) = match item.body() {
                ItemBody::CommandExecution { command, .. } => {
                    (BackgroundTaskKind::Command, command.clone())
                }
                ItemBody::Subagent { tool, prompt, .. } => (
                    BackgroundTaskKind::Subagent,
                    prompt.clone().unwrap_or_else(|| tool.clone()),
                ),
                _ => return None,
            };
            Some(BackgroundTask {
                id: item.id.to_string(),
                kind,
                description: Some(description),
            })
        })
        .collect()
}

impl Runtime {
    pub(in crate::host_rpc) async fn observe(
        service: &HostRpcService,
        event: AgentChange,
        execution: Option<Uuid>,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let native = match &event {
            AgentChange::Session { session, .. }
            | AgentChange::Request { session, .. }
            | AgentChange::Renamed { session, .. }
            | AgentChange::TaskParent { session, .. } => Some(session.clone()),
            _ => None,
        };
        if let Some(native) = &native {
            let missing = !runtime.resources.lock().unwrap().contains_key(native);
            if missing && let Some(target) = runtime.storage.native_owner(native)? {
                runtime.register_native(&target, native);
            }
        }
        let _retire = scopeguard::guard(native.clone(), |native| {
            if let Some(native) = native {
                runtime.retire_source(&native);
            }
        });
        let child_turn = match &event {
            AgentChange::Session { change, .. } => change.turn_id().map(|turn| {
                (
                    turn.to_string(),
                    matches!(
                        change,
                        SessionChange::Turn {
                            completed: false,
                            ..
                        }
                    ),
                )
            }),
            _ => None,
        };
        if Self::append_buffered_change(service, &event, execution).await? {
            return Ok(());
        }
        if let Some(execution) = execution
            && matches!(
                &event,
                AgentChange::Session { .. } | AgentChange::Request { .. }
            )
            && let Some(native) = native.as_ref()
        {
            if let Some(target) = runtime
                .storage
                .attempt_owner(&execution.to_string(), native)?
            {
                Self::observe_execution(service, &target, event, execution).await?;
                if let Some((turn, new_turn)) = child_turn {
                    runtime.storage.observe_native_child_turn(
                        native,
                        &turn,
                        new_turn,
                        &commands::at(),
                    )?;
                }
                return Ok(());
            }
            if let Some(owner) = runtime.storage.attempt_thread_id(&execution.to_string())? {
                // A real SDK child may carry its parent's execution token.
                // A known attempt on a replaced physical source must never be
                // reinterpreted as a fresh run-less turn on that same app row.
                let target = runtime.storage.native_owner(native)?;
                let child = target
                    .as_ref()
                    .map(|id| runtime.storage.thread_header(id))
                    .transpose()?
                    .flatten();
                let owner = SessionRef::new(None, owner).map_err(str::to_owned)?;
                let root = runtime
                    .storage
                    .thread_header(&owner)?
                    .map(|thread| thread.lineage.root_thread_id);
                let allowed = child.as_ref().is_some_and(|thread| {
                    thread.lineage.relationship_to_parent.as_deref() == Some("subagent")
                        && Some(&thread.lineage.root_thread_id) == root.as_ref()
                });
                if !allowed {
                    return if matches!(event, AgentChange::Request { .. }) {
                        Err("native request does not belong to this execution source".into())
                    } else {
                        Ok(())
                    };
                }
            }
        }
        match event {
            AgentChange::Session { session, change } => {
                let source = runtime.resources.lock().unwrap().get(&session).cloned();
                if let (Some(source), SessionChange::Status { status }) = (&source, &change) {
                    let provider = session.native_provider().map_err(str::to_owned)?;
                    let now = commands::at();
                    let command = Uuid::new_v4().to_string();
                    runtime.storage.observe_thread(
                        &source.target.application_id(),
                        &[],
                        &now,
                        |state| {
                            agent_protocol::orchestration::observe_native_status(
                                state,
                                provider.key(),
                                provider.driver(),
                                &session.id,
                                super::native_metadata::observed_status(*status),
                                agent_protocol::orchestration::Context {
                                    command_id: &command,
                                    now: &now,
                                    next_run_ordinal: state
                                        .next_run_ordinal()
                                        .ok_or("run ordinal exhausted")?,
                                    can_steer: false,
                                },
                            )
                        },
                    )?;
                    return Ok(());
                }
                if let Some(source) = source {
                    let _updates = source.updates.lock().await;
                    Self::external_change(service, &source, &session, change).await?;
                    if let Some((turn, new_turn)) = child_turn {
                        runtime.storage.observe_native_child_turn(
                            &session,
                            &turn,
                            new_turn,
                            &commands::at(),
                        )?;
                    }
                }
            }
            AgentChange::Request {
                session,
                origin,
                request,
            } => {
                let source = runtime.resources.lock().unwrap().get(&session).cloned();
                if let Some(source) = source {
                    Self::execution_request(
                        service,
                        &source.target,
                        &session,
                        origin,
                        request,
                        None,
                    )
                    .await?;
                }
            }
            change @ (AgentChange::Resolved { .. } | AgentChange::SourceClosed(_)) => {
                let (instance, native_id) = match change {
                    AgentChange::Resolved {
                        instance,
                        native_id,
                    } => (instance, Some(native_id)),
                    AgentChange::SourceClosed(instance) => (instance, None),
                    _ => unreachable!(),
                };
                let requests: Vec<_> = runtime
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, request)| {
                        request.origin.instance == instance
                            && native_id
                                .as_ref()
                                .is_none_or(|id| &request.origin.native_id == id)
                    })
                    .map(|(id, request)| (id.clone(), request.target.clone()))
                    .collect();
                for (id, target) in requests {
                    runtime.requests.lock().unwrap().remove(&id);
                    let now = commands::at();
                    runtime
                        .storage
                        .observe_thread(&target.id, &[], &now, |state| {
                            if native_id.is_some() {
                                agent_protocol::orchestration::resolve_request(
                                    state,
                                    id.as_str(),
                                    None,
                                    &now,
                                )
                            } else {
                                agent_protocol::orchestration::request_source_closed(
                                    state,
                                    id.as_str(),
                                    &now,
                                )
                            }
                        })?;
                }
            }
            AgentChange::Stopped { provider, error } => {
                Self::stopped(service, provider, &error).await?
            }
            AgentChange::Renamed {
                session,
                name: Some(name),
            } => {
                let target = runtime
                    .resources
                    .lock()
                    .unwrap()
                    .get(&session)
                    .map(|source| source.target.clone());
                if let Some(target) = target {
                    let now = commands::at();
                    runtime.storage.observe_thread(
                        &target.application_id(),
                        &[],
                        &now,
                        |state| {
                            if native::external::provider(state, &session).is_none()
                                || state.thread.title == name
                            {
                                return Ok(agent_protocol::orchestration::Decision::default());
                            }
                            Ok(agent_protocol::orchestration::Decision {
                                changes: vec![agent_protocol::orchestration::Change::Thread(
                                    agent_protocol::orchestration::Thread {
                                        title: name,
                                        updated_at: now.clone(),
                                        ..state.thread.clone()
                                    },
                                )],
                                ..Default::default()
                            })
                        },
                    )?;
                }
            }
            AgentChange::TaskParent { session, parent } => {
                Self::native_parent(service, &parent, &session, execution, None).await?;
            }
            AgentChange::Renamed { name: None, .. } => {}
            AgentChange::Head { .. } => {}
        }
        Ok(())
    }
}
