use super::*;
use agent_protocol::{
    execution::{BackgroundTask, BackgroundTaskKind, ItemStatus, TurnStatus},
    items::{Item, ItemBody},
    models::Turn,
};

fn background(turn: &Turn) -> Vec<BackgroundTask> {
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
    async fn request(
        service: &HostRpcService,
        session: SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
        execution: Option<Uuid>,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        if Some(origin.provider) != session.provider || !origin.source.is_alive() {
            return Err("request source is no longer live".into());
        }
        let source = runtime.resources.lock().unwrap().get(&session).cloned();
        let Some(source) = source else {
            return Err("request source is not registered".into());
        };
        let Some(info) = execution
            .map(|execution| runtime.storage.execution(&source.target, execution))
            .transpose()?
            .flatten()
            .filter(|info| {
                info.accepting
                    && Some(info.attempt.provider) == session.provider
                    && info.attempt.source == session.id
            })
        else {
            return Err("request has no accepting execution".into());
        };
        let attempt = info.attempt;
        let native = format!("{}:{}", origin.instance, origin.native_id);
        let id = decision::provider_request_id(attempt.id, &native);
        let bytes = serde_json::to_vec(&request)
            .map_err(|error| error.to_string())?
            .len();
        if !runtime.retain_request(
            id.clone(),
            bytes,
            resources::PendingRequest {
                target: source.target.clone(),
                attempt: attempt.clone(),
                origin,
            },
        )? {
            return Ok(());
        }
        let result = Self::event(
            service,
            &source.target,
            Event::Provider {
                native_items: Vec::new(),
                native_turn: None,
                attempt: attempt.id,
                observation: Observation::Request {
                    id: id.clone(),
                    body: request.body,
                },
            },
        )
        .await;
        if !matches!(result, Ok((Outcome::Accepted, _))) {
            runtime.requests.lock().unwrap().remove(&id);
            return Err(match result {
                Err(error) => error,
                _ => "execution no longer accepts requests".into(),
            });
        }
        Ok(())
    }

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
        let _retire = scopeguard::guard(native, |native| {
            if let Some(native) = native {
                runtime.retire_source(&native);
            }
        });
        match event {
            AgentChange::Session { session, change } => {
                let source = runtime.resources.lock().unwrap().get(&session).cloned();
                if let (Some(source), SessionChange::Status { status }) = (&source, &change) {
                    Self::event(
                        service,
                        &source.target,
                        Event::NativeStatus {
                            provider: session.native_provider().map_err(str::to_owned)?,
                            source: session.id.clone(),
                            status: *status,
                        },
                    )
                    .await?;
                    return Ok(());
                }
                if let (Some(source), Some(execution)) = (source, execution) {
                    if runtime
                        .storage
                        .execution(&source.target, execution)?
                        .is_none()
                        && matches!(
                            change,
                            SessionChange::Turn {
                                completed: false,
                                ..
                            }
                        )
                    {
                        Self::event(
                            service,
                            &source.target,
                            Event::ObservedStart {
                                execution,
                                provider: session.native_provider().map_err(str::to_owned)?,
                                source: session.id.clone(),
                            },
                        )
                        .await?;
                    }
                    Self::native_change(service, source, &session, change, execution).await?;
                }
            }
            AgentChange::Request {
                session,
                origin,
                request,
            } => {
                Self::request(service, session, origin, request, execution).await?;
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
                    .map(|(id, request)| (id.clone(), request.target.clone(), request.attempt.id))
                    .collect();
                for (id, target, attempt) in requests {
                    runtime.requests.lock().unwrap().remove(&id);
                    Self::event(
                        service,
                        &target,
                        Event::Provider {
                            native_items: Vec::new(),
                            native_turn: None,
                            attempt,
                            observation: Observation::RequestResolved(id),
                        },
                    )
                    .await?;
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
                    Self::event(service, &target, Event::Title(name)).await?;
                }
            }
            AgentChange::TaskParent { session, parent } => {
                if session.provider != parent.provider || session == parent {
                    return Ok(());
                }
                if let Some(target) = runtime.storage.native_owner(&parent)? {
                    let header = runtime.storage.header(&target)?;
                    if Some(header.state.provider) == parent.provider
                        && runtime.storage.active_source(&target)?.as_ref() == Some(&parent)
                    {
                        let response = agent_protocol::models::ThreadResponse {
                            thread: agent_protocol::models::Thread {
                                id: Some(session.clone()),
                                parent_id: Some(target),
                                active_provider: session.provider,
                                cwd: header.state.workspace.cwd().map(str::to_owned),
                                can_accept_direct_input: Some(false),
                                capabilities: Some(header.details.capabilities),
                                turns: Some(Vec::new()),
                                ..Default::default()
                            },
                            model: header.details.selection.model,
                            effort: header.details.selection.effort,
                            service_tier: header.details.selection.service_tier,
                        };
                        let child = runtime
                            .storage
                            .native_owner(&session)?
                            .unwrap_or_else(|| session.clone());
                        if response.thread.parent_id.as_ref() != Some(&child) {
                            // This native observation writes only the child: there is
                            // no committed parent mutation awaiting a second writer.
                            runtime
                                .owners
                                .write(
                                    &child,
                                    Write::Discover {
                                        source: session.clone(),
                                        header: super::super::storage::history::header(
                                            &response,
                                            now(),
                                        ),
                                        now: now(),
                                    },
                                )
                                .await?;
                            runtime.register_native(&child, &session);
                        }
                    }
                }
            }
            AgentChange::Renamed { name: None, .. } => {}
        }
        Ok(())
    }

    async fn native_change(
        service: &HostRpcService,
        source: Arc<resources::Source>,
        native: &SessionRef,
        change: SessionChange,
        execution: Uuid,
    ) -> Result<(), String> {
        let _updates = source.updates.lock().await;
        let Some(info) = service
            .inner
            .conversation
            .storage
            .execution(&source.target, execution)?
            .filter(|info| {
                info.accepting
                    && Some(info.attempt.provider) == native.provider
                    && info.attempt.source == native.id
            })
        else {
            return Ok(());
        };
        let attempt = info.attempt;
        let became_addressable = info.native_turn.is_none() && change.turn_id().is_some();
        let native_turn = change.turn_id().cloned().or(info.native_turn);
        let items: Vec<_> = match &change {
            SessionChange::Turn { turn, .. } => turn.items.iter().flatten().collect(),
            SessionChange::Item { item, .. } => vec![item],
            _ => Vec::new(),
        };
        let links = items
            .iter()
            .filter_map(|item| match item.body() {
                ItemBody::Subagent {
                    sender,
                    receivers,
                    states,
                    ..
                } => Some((sender, receivers, states)),
                _ => None,
            })
            .flat_map(|(sender, receivers, states)| {
                sender
                    .iter()
                    .chain(receivers)
                    .chain(states.iter().map(|state| &state.session))
            });
        let aliases = service.inner.conversation.storage.native_owners(links)?;
        let user_echo = info.submitted
            && items
                .iter()
                .any(|item| matches!(item.body(), ItemBody::UserMessage { .. }));
        let mappings: Vec<_> = items
            .into_iter()
            .filter(|item| !(info.submitted && matches!(item.body(), ItemBody::UserMessage { .. })))
            .map(|item| {
                (
                    item.id.to_string(),
                    decision::provider_item_id(attempt.id, item.id.as_str()),
                )
            })
            .collect();
        let mut observations = Vec::new();
        if matches!(
            change,
            SessionChange::Turn {
                completed: true,
                ..
            }
        ) {
            Self::flush_text(service, &source).await?;
        }
        for (_, id) in &mappings {
            source.replace_text(id);
        }
        match change {
            SessionChange::Turn { turn, completed } => {
                observations.push(Observation::Timing {
                    started_at: turn
                        .started_at_ms
                        .and_then(|at| at.try_into().ok())
                        .or_else(|| turn.started_at.map(|at| (at * 1000.0) as i64)),
                    completed_at: turn.completed_at_ms.and_then(|at| at.try_into().ok()),
                    duration_ms: turn.duration_ms,
                });
                for item in turn.items.iter().flatten() {
                    Self::native_item(
                        &service.inner.conversation.outputs,
                        &source.target,
                        attempt.id,
                        &aliases,
                        item,
                        info.submitted,
                        &mut observations,
                    )?;
                }
                if completed {
                    let tasks =
                        if matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted) {
                            source.cancel_background(attempt.id);
                            Vec::new()
                        } else {
                            source.finish_foreground(attempt.id, background(&turn))
                        };
                    if tasks.is_empty() {
                        service.inner.conversation.finish_outputs(
                            &source.target,
                            attempt.id,
                            turn.status,
                            &mut observations,
                        )?;
                    }
                    observations.push(match turn.status {
                        TurnStatus::Failed => Observation::Failed(turn.error),
                        TurnStatus::Interrupted => Observation::Interrupted,
                        _ if !tasks.is_empty() => Observation::Background { tasks },
                        _ => Observation::Completed,
                    });
                }
            }
            SessionChange::Item { item, .. } => Self::native_item(
                &service.inner.conversation.outputs,
                &source.target,
                attempt.id,
                &aliases,
                &item,
                info.submitted,
                &mut observations,
            )?,
            SessionChange::Text {
                item_id,
                field,
                delta,
                ..
            } => {
                Self::text_delta(service, &source, &attempt, item_id.as_str(), field, &delta)?;
            }
            SessionChange::Background { tasks } => {
                let state = service.inner.conversation.storage.state(&source.target)?;
                for (owner, tasks, native_ended) in source.roster(execution, tasks) {
                    let Some(owner) = service
                        .inner
                        .conversation
                        .storage
                        .execution(&source.target, owner)?
                        .filter(|execution| execution.accepting)
                        .map(|execution| execution.attempt)
                    else {
                        continue;
                    };
                    if state.turns.get(&owner.turn).is_some_and(|turn| {
                        matches!(
                            turn.state,
                            decision::TurnState::Waiting(decision::Wait::Background { .. })
                        ) || native_ended
                            && matches!(
                                turn.state,
                                decision::TurnState::Running { interrupt: Some(_) }
                            )
                    }) {
                        let native_turn = service
                            .inner
                            .conversation
                            .storage
                            .execution(&source.target, owner.id)?
                            .and_then(|execution| execution.native_turn);
                        if tasks.is_empty() {
                            Self::settle_command_outputs(
                                service,
                                &source.target,
                                owner.id,
                                native_turn.clone(),
                                TurnStatus::Completed,
                            )
                            .await?;
                        }
                        Self::event(
                            service,
                            &source.target,
                            Event::Provider {
                                native_items: Vec::new(),
                                attempt: owner.id,
                                native_turn,
                                observation: Observation::Background { tasks },
                            },
                        )
                        .await?;
                    }
                }
            }
            SessionChange::Error { error, .. } => observations.push(Observation::Notice(error)),
            SessionChange::Status { .. }
            | SessionChange::Workspace { .. }
            | SessionChange::ReasoningPart { .. }
            | SessionChange::Unsupported => {}
            SessionChange::RemoveItem { item_id, .. } => observations.push(
                Observation::RemoveItem(decision::provider_item_id(attempt.id, item_id.as_str())),
            ),
            SessionChange::Request { .. }
            | SessionChange::RequestDelivery { .. }
            | SessionChange::ResolveRequest { .. } => {}
        }
        if observations.is_empty() && (!mappings.is_empty() || user_echo || became_addressable) {
            observations.push(Observation::Timing {
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        for observation in observations {
            let native_items = match &observation {
                Observation::Item { id, .. } => mappings
                    .iter()
                    .filter(|(_, canonical)| canonical == id)
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            Self::event(
                service,
                &source.target,
                Event::Provider {
                    native_items,
                    native_turn: native_turn.clone(),
                    attempt: attempt.id,
                    observation,
                },
            )
            .await?;
        }
        if became_addressable {
            Self::event(service, &source.target, Event::Wake).await?;
        }
        Ok(())
    }

    pub(super) async fn settle_command_outputs(
        service: &HostRpcService,
        target: &SessionRef,
        execution: Uuid,
        native_turn: Option<agent_protocol::ids::TurnId>,
        status: TurnStatus,
    ) -> Result<(), String> {
        let mut observations = Vec::new();
        service
            .inner
            .conversation
            .finish_outputs(target, execution, status, &mut observations)?;
        for observation in observations {
            Self::event(
                service,
                target,
                Event::Provider {
                    native_items: Vec::new(),
                    attempt: execution,
                    native_turn: native_turn.clone(),
                    observation,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(super) fn finish_outputs(
        &self,
        target: &SessionRef,
        execution: Uuid,
        status: TurnStatus,
        observations: &mut Vec<Observation>,
    ) -> Result<(), String> {
        for (key, tail) in self.outputs.finish_attempt(target, execution) {
            let pending = observations.iter_mut().find(|observation| matches!(observation, Observation::Item { id, .. } if id == &key.item));
            let item_status = match status {
                TurnStatus::Failed => ItemStatus::Failed,
                TurnStatus::Interrupted => ItemStatus::Interrupted,
                _ => ItemStatus::Completed,
            };
            if let Some(Observation::Item { body, status, .. }) = pending {
                if !matches!(*status, ItemStatus::Running | ItemStatus::Unknown) {
                    continue;
                }
                if let ItemBody::CommandExecution {
                    output,
                    output_omitted,
                    ..
                } = body
                {
                    *output = tail.text;
                    // Running rows omit their inline body while the live stream
                    // owns it. That placeholder isn't evidence of truncation.
                    *output_omitted = tail.truncated;
                    *status = item_status;
                }
            } else if let Some(mut item) = self
                .storage
                .item(target, &key.item)?
                .filter(|item| matches!(item.status, ItemStatus::Running | ItemStatus::Unknown))
                && let ItemBody::CommandExecution {
                    output,
                    output_omitted,
                    ..
                } = &mut item.body
            {
                *output = tail.text;
                *output_omitted = tail.truncated;
                observations.push(Observation::Item {
                    deferred: false,
                    id: key.item,
                    status: item_status,
                    body: item.body,
                });
            }
        }
        Ok(())
    }

    fn native_item(
        outputs: &Arc<super::output::Outputs>,
        target: &SessionRef,
        execution: Uuid,
        aliases: &HashMap<SessionRef, SessionRef>,
        item: &Item,
        client_input: bool,
        observations: &mut Vec<Observation>,
    ) -> Result<(), String> {
        // The admitted input is canonical; SDK echoes cannot append it again.
        if client_input && matches!(item.body(), ItemBody::UserMessage { .. }) {
            return Ok(());
        }
        let id = decision::provider_item_id(execution, item.id.as_str());
        let mut body = item.body().clone();
        if let ItemBody::Subagent {
            sender,
            receivers,
            states,
            ..
        } = &mut body
        {
            for link in sender
                .iter_mut()
                .chain(receivers)
                .chain(states.iter_mut().map(|state| &mut state.session))
            {
                if let Some(public) = aliases.get(link) {
                    *link = public.clone();
                }
            }
        }
        if let ItemBody::CommandExecution {
            output,
            output_omitted,
            ..
        } = &mut body
        {
            let key = records::OutputKey {
                session: target.clone(),
                item: id.clone(),
            };
            if matches!(item.status, ItemStatus::Running | ItemStatus::Unknown) {
                outputs.write(
                    execution,
                    key,
                    super::output::Change::Replace {
                        text: output,
                        truncated: *output_omitted,
                    },
                )?;
                output.clear();
                *output_omitted = true;
            } else {
                let tail = outputs.finish(&key, output);
                *output_omitted |= tail.truncated;
                *output = tail.text;
            }
        }
        observations.push(Observation::Item {
            deferred: item.is_deferred(),
            id,
            status: item.status,
            body,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_links_use_public_conversations_without_changing_external_addresses() {
        let native = SessionRef::new(ProviderKind::Codex, "private-source".into()).unwrap();
        let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let external = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        let item = Item::new(
            "native-item".into(),
            ItemStatus::Completed,
            ItemBody::Subagent {
                tool: "send_input".into(),
                prompt: None,
                model: None,
                effort: None,
                sender: Some(native.clone()),
                receivers: vec![native.clone(), external.clone()],
                states: vec![agent_protocol::items::SubagentState {
                    session: native.clone(),
                    status: TurnStatus::Completed,
                    message: Some("finished".into()),
                }],
                agent_id: None,
                result: None,
            },
        );
        let mut observations = Vec::new();
        Runtime::native_item(
            &Arc::default(),
            &public,
            Uuid::new_v4(),
            &[(native, public.clone())].into(),
            &item,
            false,
            &mut observations,
        )
        .unwrap();
        let Observation::Item {
            body:
                ItemBody::Subagent {
                    sender,
                    receivers,
                    states,
                    ..
                },
            ..
        } = &observations[0]
        else {
            panic!("subagent item")
        };
        assert_eq!(sender.as_ref(), Some(&public));
        assert_eq!(receivers, &[public.clone(), external]);
        assert_eq!(states[0].session, public);
        assert_eq!(states[0].message.as_deref(), Some("finished"));
    }
}
