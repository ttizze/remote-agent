use super::*;
use agent_protocol::requests::Answer;

pub(super) struct Source {
    pub target: SessionRef,
    pub io: AsyncMutex<()>,
    pub(super) updates: AsyncMutex<()>,
    pub(super) text: Mutex<super::text::Text>,
    background: Mutex<HashMap<Uuid, Background>>,
}

#[derive(Default)]
struct Background {
    tasks: Vec<agent_protocol::execution::BackgroundTask>,
    native_ended: bool,
}

pub(super) struct PendingRequest {
    pub target: SessionRef,
    pub attempt: Attempt,
    pub origin: RequestOrigin,
}

impl Source {
    pub(super) fn cancel_background(&self, execution: Uuid) {
        self.background.lock().unwrap().remove(&execution);
    }

    pub(super) fn background(
        &self,
        execution: Uuid,
    ) -> Vec<agent_protocol::execution::BackgroundTask> {
        self.background
            .lock()
            .unwrap()
            .get(&execution)
            .map(|background| background.tasks.clone())
            .unwrap_or_default()
    }

    pub(super) fn finish_foreground(
        &self,
        execution: Uuid,
        fallback: Vec<agent_protocol::execution::BackgroundTask>,
    ) -> Vec<agent_protocol::execution::BackgroundTask> {
        let mut roster = self.background.lock().unwrap();
        if let Some(background) = roster.get_mut(&execution) {
            background.native_ended = true;
            return background.tasks.clone();
        }
        if !fallback.is_empty() {
            roster.insert(
                execution,
                Background {
                    tasks: fallback.clone(),
                    native_ended: true,
                },
            );
        }
        fallback
    }

    pub(super) fn roster(
        &self,
        execution: Uuid,
        tasks: Vec<agent_protocol::execution::BackgroundTask>,
    ) -> Vec<(Uuid, Vec<agent_protocol::execution::BackgroundTask>, bool)> {
        let mut roster = self.background.lock().unwrap();
        let previous = std::mem::take(&mut *roster);
        for task in tasks {
            let owner = previous
                .iter()
                .find_map(|(owner, background)| {
                    background
                        .tasks
                        .iter()
                        .any(|known| known.id == task.id)
                        .then_some(*owner)
                })
                .unwrap_or(execution);
            let background = roster.entry(owner).or_insert_with(|| Background {
                native_ended: previous
                    .get(&owner)
                    .is_some_and(|background| background.native_ended),
                ..Default::default()
            });
            background.tasks.push(task);
        }
        let executions: std::collections::HashSet<_> =
            previous.keys().chain(roster.keys()).copied().collect();
        executions
            .into_iter()
            .map(|execution| {
                (
                    execution,
                    roster
                        .get(&execution)
                        .map(|background| background.tasks.clone())
                        .unwrap_or_default(),
                    previous
                        .get(&execution)
                        .is_some_and(|background| background.native_ended),
                )
            })
            .collect()
    }
}

impl Runtime {
    pub(super) async fn release_unused_source(
        service: &HostRpcService,
        target: &SessionRef,
        provider: ProviderKind,
        id: &str,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let native = SessionRef::new(provider, id.to_owned()).map_err(str::to_owned)?;
        let _retire = scopeguard::guard(native.clone(), |native| runtime.retire_source(&native));
        let resource = runtime.register_native(target, &native);
        let _io = resource.io.lock().await;
        let state = runtime.storage.state(target)?;
        let retained = match state.bindings.get(&provider) {
            Some(decision::Binding::Ready { source, .. }) => source == id,
            Some(decision::Binding::Aligning { source, .. }) => {
                source == id
                    && state.thread.provider == provider
                    && state
                        .inputs
                        .values()
                        .any(|input| matches!(input.state, decision::InputState::Preparing { .. }))
            }
            _ => false,
        };
        if !retained && !state.source_in_use(provider, id, Uuid::nil()) {
            let result = async { service.inner.providers.get(provider)?.unload(id).await }.await;
            if let Err(error) = result {
                tracing::warn!(target:"agent",operation="conversation.release_unused",%error);
            }
        }
        Ok(())
    }

    async fn release_inactive_sources(
        service: &HostRpcService,
        target: &SessionRef,
        id: &ClientInputId,
        action: Uuid,
        provider: ProviderKind,
    ) -> Result<(), Failure> {
        let runtime = &service.inner.conversation;
        let state = runtime.storage.state(target).map_err(storage_failure)?;
        for (kind, binding) in state.bindings {
            let decision::Binding::Ready {
                source: native_id, ..
            } = &binding
            else {
                continue;
            };
            if kind == provider {
                continue;
            }
            let native = SessionRef::new(kind, native_id.clone())
                .map_err(|error| Failure::new("invalid_session", error))?;
            // Preparation remains owned by this token until it returns. A new
            // input cannot reopen this source while the I/O lock is held.
            let result = async {
                let source = runtime.register_native(target, &native);
                let _io = source.io.lock().await;
                let current = runtime.storage.state(target).map_err(storage_failure)?;
                let preparing = Action::Prepare {
                    id: id.clone(),
                    action,
                    stage: PreparationStage::Provider,
                    cwd: current.thread.workspace.cwd().map(str::to_owned),
                    provider,
                };
                if current.permits(&preparing)
                    && current.bindings.get(&kind) == Some(&binding)
                    && !current.source_in_use(kind, native_id, Uuid::nil())
                    && let Ok(agent) = service.inner.providers.get(kind)
                {
                    agent.unload(native_id).await?;
                }
                Ok::<_, Failure>(())
            }
            .await;
            runtime.retire_source(&native);
            if let Err(error) = result {
                tracing::warn!(target:"agent",operation="conversation.release_inactive",%error);
            }
        }
        Ok(())
    }

    pub(super) fn retain_request(
        &self,
        id: RequestId,
        bytes: usize,
        request: PendingRequest,
    ) -> Result<bool, String> {
        if bytes > 64 * 1024 {
            return Err("pending request capacity reached".into());
        }
        let mut requests = self.requests.lock().unwrap();
        if requests.contains_key(&id) {
            return Ok(false);
        }
        if requests.len() >= 256
            || requests
                .values()
                .filter(|pending| pending.target == request.target)
                .count()
                >= 32
        {
            return Err("pending request capacity reached".into());
        }
        requests.insert(id, request);
        Ok(true)
    }

    pub(super) fn prune_requests(&self, target: &SessionRef) {
        let retained: Vec<_> = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, request)| &request.target == target)
            .map(|(id, request)| (id.clone(), request.origin.source.clone()))
            .collect();
        for (id, source) in retained {
            match self.storage.request_owner(&id) {
                Ok(owner) if owner.as_ref() != Some(target) => {
                    let mut requests = self.requests.lock().unwrap();
                    if requests
                        .get(&id)
                        .is_some_and(|request| Arc::ptr_eq(&request.origin.source, &source))
                    {
                        requests.remove(&id);
                    }
                }
                Err(error) => {
                    tracing::error!(target:"agent",operation="conversation.requests.release",%error)
                }
                _ => {}
            }
        }
    }

    pub(super) fn source(&self, target: &SessionRef, attempt: &Attempt) -> Arc<Source> {
        let native = SessionRef::new(attempt.provider, attempt.source.clone())
            .expect("validated provider binding");
        self.register_native(target, &native)
    }
    pub(super) fn register_native(&self, target: &SessionRef, native: &SessionRef) -> Arc<Source> {
        self.resources
            .lock()
            .unwrap()
            .entry(native.clone())
            .or_insert_with(|| {
                Arc::new(Source {
                    target: target.clone(),
                    io: Default::default(),
                    updates: Default::default(),
                    text: Default::default(),
                    background: Default::default(),
                })
            })
            .clone()
    }

    /// The registry lock and the last I/O holder serialize retirement with the
    /// next lookup. Correlation remains in storage for late native callbacks.
    pub(super) fn retire_source(&self, native: &SessionRef) {
        let mut sources = self.resources.lock().unwrap();
        if sources.get(native).is_some_and(|source| {
            Arc::strong_count(source) == 1
                && source.background.lock().unwrap().is_empty()
                && source.text.lock().unwrap().is_empty()
        }) {
            sources.remove(native);
        }
    }

    pub(super) async fn prepare_provider(
        service: &HostRpcService,
        target: &SessionRef,
        id: &ClientInputId,
        action: Uuid,
        cwd: Option<String>,
        provider: ProviderKind,
    ) -> Result<(String, Option<String>, bool), Failure> {
        let state = service
            .inner
            .conversation
            .storage
            .state(target)
            .map_err(storage_failure)?;
        let input = state
            .inputs
            .get(id)
            .ok_or_else(|| Failure::new("input_unavailable", "input no longer exists"))?;
        let cwd =
            cwd.ok_or_else(|| Failure::new("workspace_unavailable", "workspace is missing"))?;
        service.restore_workspace(&cwd).await?;
        let agent = service.creation_agent(provider, input.content.model.as_ref())?;
        let fork = match state.bindings.get(&provider) {
            Some(decision::Binding::Fork { source, through }) if agent.capabilities().fork => {
                Some((source, through))
            }
            _ => None,
        };
        let reusable = if let Some(decision::Binding::Ready { source, covered }) =
            state.bindings.get(&provider)
            && *covered <= state.history_end()
        {
            match agent.state(source).await {
                Ok(native) => {
                    let response = native.response;
                    let expected = SessionRef::new(provider, source.clone())
                        .map_err(|error| Failure::new("invalid_session", error))?;
                    op::validate_thread(&response, Some(&expected))
                        .map_err(|error| Failure::new("invalid_thread", error))?;
                    if response.thread.status == agent_protocol::models::SessionStatus::Running
                        || !response.thread.requests.is_empty()
                    {
                        return Err(Failure::new(
                            "provider_switch_busy",
                            "切り替え先のAIがまだ実行中です。完了を待ってください。",
                        ));
                    }
                    let readable =
                        response
                            .thread
                            .history_read_state
                            .as_ref()
                            .is_none_or(|state| {
                                matches!(
                                    state.kind,
                                    agent_protocol::session::HistoryReadKind::Complete
                                        | agent_protocol::session::HistoryReadKind::Partial
                                )
                            });
                    (readable && response.thread.cwd.as_deref() == Some(cwd.as_str()))
                        .then_some((response, *covered))
                }
                Err(_) => None,
            }
        } else {
            None
        };
        let reused = reusable.is_some();
        let covered = reusable.as_ref().map_or(0, |(_, covered)| *covered);
        // Native forks retain their context. Only portable handoff needs bodies.
        let mut messages = if fork.is_none() {
            let (history, missing_message) =
                Self::handoff_history(service, target, covered).await?;
            if missing_message {
                return Err(Failure::new(
                    "incomplete_history",
                    "会話の履歴を読み込めませんでした。",
                ));
            }
            history
                .thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| agent_protocol::handoff::Message::from_turn(turn))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let browser = service.browser_config(&target.to_string())?;
        // A newly created private source must be recorded before discovery can
        // publish that same native identity as a separate public conversation.
        let catalog = service.inner.conversation.catalog_access[&provider]
            .read()
            .await;
        let response = if let Some((response, _)) = reusable {
            response
        } else if let Some((source, through)) = fork {
            agent.fork(source, through.as_str(), browser).await?
        } else {
            agent
                .create(
                    &cwd,
                    input.content.model.as_ref().map(|model| model.id.as_str()),
                    browser,
                )
                .await?
        };
        let native = response
            .thread
            .id
            .ok_or_else(|| Failure::new("invalid_thread", "provider returned no identity"))?;
        if native.provider != Some(provider) {
            return Err(Failure::new(
                "provider_mismatch",
                "provider returned a different identity",
            ));
        }
        let selection = records::Selection {
            model: response.model,
            effort: response.effort,
            service_tier: response.service_tier,
        };
        let previous_context = service
            .inner
            .conversation
            .storage
            .source_context(target, provider, &native.id)
            .map_err(storage_failure)?;
        let _retire = scopeguard::guard(native.clone(), |native| {
            service.inner.conversation.retire_source(&native);
        });
        let resource = service.inner.conversation.register_native(target, &native);
        let _io = resource.io.lock().await;
        let prepared = async {
        service
            .inner
            .conversation
            .storage
            .save_source(target, provider, &native.id, previous_context.as_ref(), &selection)
            .map_err(storage_failure)?;
        drop(catalog);
        let (outcome,_) = Self::event(service,target,Event::AligningProvider {
            id: id.clone(),action,source:native.id.clone(),provider,cwd:cwd.clone(),
        }).await.map_err(storage_failure)?;
        if outcome!=Outcome::Accepted || !service.inner.conversation.storage.input(target,id).map_err(storage_failure)?
            .is_some_and(|input|input.state.preparing(action,PreparationStage::Provider)) {
            return Err(Failure::new("preparation_cancelled","input preparation is no longer current"));
        }
        Self::release_inactive_sources(service,target,id,action,provider).await?;
        let mut inline = previous_context;
        if !messages.is_empty() {
            if let Some(previous) = &inline {
                let mut pending = previous.messages.clone();
                pending.append(&mut messages);
                messages = pending;
            }
            let coverage = format!(
                "Provider context handoff. Conversation: {target}.\nRead omitted history on this Host with the BEX history command shown below.\n{}",
                service.history_command(target)
            );
            let context = agent_protocol::handoff::select(
                &messages,
                &coverage,
                agent_protocol::handoff::DEFAULT_BUDGET,
            )
            .map_err(|error| Failure::new("history_handoff_failed", error))?;
            inline = if agent.inject_history(&native.id, &context).await? {None} else {Some(context)};
        }
        service
            .inner
            .conversation
            .storage
            .save_source(target, provider, &native.id, inline.as_ref(), &selection)
            .map_err(storage_failure)?;
            Ok::<_, Failure>(())
        }.await;
        if let Err(error) = prepared {
            let current = service
                .inner
                .conversation
                .storage
                .state(target)
                .map_err(storage_failure)?;
            let ready = matches!(current.bindings.get(&provider),Some(decision::Binding::Ready {source,..}) if source == &native.id);
            let aligning = matches!(current.bindings.get(&provider),Some(decision::Binding::Aligning {source,..}) if source == &native.id);
            let another_preparation = current.thread.provider == provider && current.inputs.values().any(|input|
                matches!(input.state,decision::InputState::Preparing {action:token,..} if token != action));
            if !ready
                && (aligning || !reused)
                && !another_preparation
                && !current.source_in_use(provider, &native.id, Uuid::nil())
                && let Err(cleanup) = agent.unload(&native.id).await
            {
                tracing::warn!(target:"agent",operation="conversation.release_preparation",message=%cleanup);
            }
            return Err(error);
        }
        Ok((cwd, Some(native.id), false))
    }

    pub(super) async fn deliver(
        service: &HostRpcService,
        target: &SessionRef,
        (id, token): (ClientInputId, Uuid),
        attempt: Attempt,
        cwd: String,
        content: decision::InputContent,
        steer: bool,
    ) -> Result<(), String> {
        let _workspace = service.inner.worktree_access.read().await;
        let runtime = &service.inner.conversation;
        let source = runtime.source(target, &attempt);
        let _io = source.io.lock().await;
        let action = Action::Deliver {
            id: id.clone(),
            action: token,
            attempt: attempt.clone(),
            cwd: cwd.clone(),
            content: content.clone(),
            steer,
        };
        if !runtime.storage.state(target)?.permits(&action) {
            return Ok(());
        }
        let native_turn = runtime
            .storage
            .execution(target, attempt.id)?
            .and_then(|execution| execution.native_turn);
        if steer && native_turn.is_none() {
            Runtime::event(
                service,
                target,
                Event::DeliveryDeferred { id, action: token },
            )
            .await?;
            return Ok(());
        }
        let route = if steer {
            SubmissionTarget::Steer(
                native_turn
                    .as_ref()
                    .expect("checked steering target")
                    .as_str(),
            )
        } else {
            SubmissionTarget::Start { cwd: &cwd }
        };
        let mut input = op::Submission {
            fresh_context: false,
            thread_id: SessionRef::new(attempt.provider, attempt.source.clone())
                .map_err(str::to_owned)?,
            client_user_message_id: id.clone(),
            input: content.parts,
            model: content.model,
            effort: content.effort,
            service_tier: content.service_tier,
            follow_up: content.follow_up,
        };
        if !op::Input::is_compaction(&input.input)
            && let Some(context) =
                runtime
                    .storage
                    .source_context(target, attempt.provider, &attempt.source)?
        {
            input.input.insert(
                0,
                op::Input::Text {
                    text: format!("{}\n\nUser message:\n", context.render()),
                },
            );
        }
        let result = async {
            service.require_not_updating()?;
            let reload = service.restore_workspace(&cwd).await?;
            let agent = service.inner.providers.get(attempt.provider)?;
            agent
                .submit(
                    &input,
                    route,
                    reload,
                    service.browser_config(&target.to_string())?,
                    attempt.id,
                )
                .await
        }
        .await;
        let input = id.clone();
        let event = match result {
            Ok(receipt) => Event::Delivered {
                native_turn: receipt.turn_id,
                id,
                action: token,
            },
            Err(error) => {
                let unknown = error.delivery == agent_protocol::error::Delivery::Unknown;
                Event::DeliveryFailed {
                    id,
                    action: token,
                    unknown,
                    error: execution_error(error),
                }
            }
        };
        Self::event(service, target, event).await?;
        if let Some(workspace) =
            runtime
                .storage
                .header(target)?
                .details
                .workspace
                .filter(|workspace| {
                    workspace.setup.as_ref().is_some_and(|setup| {
                        setup.phase == agent_protocol::workspace::SetupPhase::Running
                    })
                })
        {
            use super::super::storage::workspace::SetupChange;
            use agent_protocol::workspace::{SetupPhase, SetupStageId, SetupStageStatus};
            let saved = runtime.storage.input(target, &input)?;
            let update = match saved.as_ref().map(|input| &input.state) {
                None => Some(SetupChange::Stage {
                    id: SetupStageId::Agent,
                    status: SetupStageStatus::Done,
                    exit_code: None,
                    error: None,
                }),
                Some(decision::InputState::Failed { error, .. }) => Some(SetupChange::Finish {
                    phase: if error.is_some() {
                        SetupPhase::Failed
                    } else {
                        SetupPhase::Cancelled
                    },
                    error: error.as_ref().map(|error| error.message.clone()),
                }),
                Some(decision::InputState::Unknown { .. }) => Some(SetupChange::Finish {
                    phase: SetupPhase::Failed,
                    error: Some("入力の配送結果を確認できません。".into()),
                }),
                _ => None,
            };
            if let Some(update) = update {
                service
                    .setup_update(target, workspace.generation, update)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }

        Ok(())
    }

    // Called under the source I/O lock before native continuation has begun.
    // Stop's queued I/O will observe the terminal state and become obsolete.
    async fn cancel_unstarted_resume(
        service: &HostRpcService,
        target: &SessionRef,
        attempt: &Attempt,
    ) -> Result<(), String> {
        let state = service.inner.conversation.storage.state(target)?;
        if state.attempts.get(&attempt.turn) == Some(attempt)
            && let Some(decision::Turn {
                state: decision::TurnState::Running { interrupt: Some(_) },
                ..
            }) = state.turns.get(&attempt.turn)
        {
            Self::event(
                service,
                target,
                Event::Provider {
                    attempt: attempt.id,
                    native_turn: None,
                    native_items: vec![],
                    observation: Observation::Interrupted,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn resume(
        service: &HostRpcService,
        target: &SessionRef,
        attempt: &Attempt,
        cwd: &str,
        at: i64,
        original_error: Option<agent_protocol::execution::ExecutionError>,
    ) -> Result<(), String> {
        let _workspace = service.inner.worktree_access.read().await;
        let runtime = &service.inner.conversation;
        let source = runtime.source(target, attempt);
        let _io = source.io.lock().await;
        let action = Action::Resume {
            attempt: attempt.clone(),
            cwd: cwd.to_owned(),
            at,
            error: original_error.clone(),
        };
        if !runtime.storage.state(target)?.permits(&action) {
            return Self::cancel_unstarted_resume(service, target, attempt).await;
        }
        if service.require_not_updating().is_err() {
            Self::event(
                service,
                target,
                Event::ResumeDeferred {
                    attempt: attempt.id,
                    at,
                    error: original_error,
                },
            )
            .await?;
            return Ok(());
        }
        let selection =
            runtime
                .storage
                .source_selection(target, attempt.provider, &attempt.source)?;
        let preflight = async {
            service.require_provider_enabled(attempt.provider)?;
            let agent = service.inner.providers.get(attempt.provider)?;
            if service.restore_workspace(cwd).await? {
                return Err(Failure::new(
                    "retry_unavailable",
                    "再作成した作業フォルダでは実行を再開できません。",
                ));
            }
            let state = agent.state(&attempt.source).await?;
            let native = SessionRef::new(attempt.provider, attempt.source.clone())
                .map_err(|error| Failure::new("invalid_session", error))?;
            op::validate_thread(&state.response, Some(&native))
                .map_err(|error| Failure::new("invalid_thread", error))?;
            let thread = state.response.thread;
            if state.needs_reload
                || thread.cwd.as_deref() != Some(cwd)
                || !matches!(
                    thread.status,
                    agent_protocol::models::SessionStatus::Idle
                        | agent_protocol::models::SessionStatus::Unavailable
                )
                || !thread.requests.is_empty()
                || thread.parent_id.is_some()
            {
                return Err(Failure::new(
                    "retry_unavailable",
                    "再試行の接続先を安全に再開できません。",
                ));
            }
            Ok(agent)
        }
        .await;
        let result = match preflight {
            Ok(agent) => {
                // Stop may have committed while the provider state was being read.
                if !runtime.storage.state(target)?.permits(&action) {
                    return Self::cancel_unstarted_resume(service, target, attempt).await;
                }
                agent
                    .continue_turn(
                        &attempt.source,
                        selection.model.as_ref().map(|model| model.id.as_str()),
                        selection.effort.as_deref(),
                        selection.service_tier.as_deref(),
                        attempt.id,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(receipt) => {
                if let Some(turn) = receipt.turn_id {
                    Self::event(
                        service,
                        target,
                        Event::Provider {
                            attempt: attempt.id,
                            native_turn: Some(turn),
                            native_items: vec![],
                            observation: Observation::Timing {
                                started_at: None,
                                completed_at: None,
                                duration_ms: None,
                            },
                        },
                    )
                    .await?;
                }
            }
            Err(error) => {
                let mut error = original_error.unwrap_or_else(|| execution_error(error));
                error.retry = Some(agent_protocol::execution::RetryEvidence {
                    retrying: true,
                    attempt: None,
                    max_attempts: None,
                });
                Self::event(
                    service,
                    target,
                    Event::Provider {
                        native_items: Vec::new(),
                        native_turn: None,
                        attempt: attempt.id,
                        observation: Observation::Failed(Some(error)),
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn answer(
        service: &HostRpcService,
        target: &SessionRef,
        attempt: Attempt,
        id: RequestId,
        token: Uuid,
        answer: Answer,
    ) -> Result<(), String> {
        let _workspace = service.inner.worktree_access.read().await;
        let runtime = &service.inner.conversation;
        let source = runtime.source(target, &attempt);
        let _io = source.io.lock().await;
        let action = Action::Answer {
            attempt: attempt.clone(),
            id: id.clone(),
            action: token,
            answer: answer.clone(),
        };
        let body = {
            let state = runtime.storage.state(target)?;
            if !state.permits(&action) {
                return Ok(());
            }
            state.requests[&id].body.clone()
        };
        let request = runtime
            .requests
            .lock()
            .unwrap()
            .get(&id)
            .filter(|request| request.target == *target && request.attempt == attempt)
            .map(|request| request.origin.clone());
        let result = if service.require_not_updating().is_err() {
            Err(agent_protocol::error::Delivery::NotSent)
        } else if let Some(origin) = request {
            match origin
                .source
                .prepare(&origin.native_id, &body, &answer)
                .await
            {
                Ok(write) => write.await.map_err(|error| error.delivery),
                Err(error) => Err(error.delivery),
            }
        } else {
            Err(agent_protocol::error::Delivery::NotSent)
        };
        Self::event(
            service,
            target,
            Event::Answered {
                id,
                action: token,
                result,
            },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::execution::{BackgroundTask, BackgroundTaskKind};

    #[rstest::rstest]
    #[tokio::test]
    async fn capacity_resume_checks_native_safety_and_preserves_stop(
        #[values(
            "refused",
            "continued",
            "stop-after-ack",
            "running",
            "question",
            "child",
            "reload",
            "disabled",
            "unknown",
            "preparing",
            "stop",
            "stop-before-start",
            "updating"
        )]
        case: &str,
    ) {
        use super::super::native_fixture::NativeAgent;
        use agent_protocol::{
            conversation::{OperationId, TurnState},
            models::{Thread, ThreadResponse},
        };
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().into());
        Arc::get_mut(&mut agent).unwrap().needs_reload = case == "reload";
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let native = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(native.clone()),
                cwd: Some(directory.path().to_str().unwrap().into()),
                status: agent_protocol::models::SessionStatus::Idle,
                turns: Some(vec![]),
                ..Default::default()
            },
            ..Default::default()
        };
        agent
            .records
            .lock()
            .unwrap()
            .insert(native.id.clone(), response.clone());
        let storage = &service.inner.conversation.storage;
        super::super::native_fixture::own(&service, &target, &response);
        Runtime::recover(&service, &target, ProviderKind::Codex)
            .await
            .unwrap();
        let context = Runtime::context(&service, Some(ProviderKind::Codex));
        let input = Uuid::new_v4();
        let submitted = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(input),
                    receipt: None,
                    command: Command::Submit {
                        id: input.to_string().into(),
                        content: decision::InputContent {
                            fresh_context: false,
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
        } = &submitted.step.actions[0]
        else {
            panic!("delivery")
        };
        storage
            .decide(
                &target,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: Some("first".into()),
                },
                context,
                None,
            )
            .unwrap();
        storage
            .decide(
                &target,
                Event::Provider {
                    attempt: attempt.id,
                    native_turn: None,
                    native_items: vec![],
                    observation: Observation::Failed(Some(
                        agent_protocol::execution::ExecutionError {
                            message: "original overload".into(),
                            provider_code: Some("serverOverloaded".into()),
                            ..Default::default()
                        },
                    )),
                },
                context,
                None,
            )
            .unwrap();
        let due = storage
            .decide(
                &target,
                Event::Wake,
                decision::Context {
                    now: context.now + 2100,
                    seed: Uuid::new_v4(),
                    ..context
                },
                None,
            )
            .unwrap();
        let Action::Resume {
            attempt,
            cwd,
            at,
            error,
        } = &due.step.actions[0]
        else {
            panic!("resume")
        };
        {
            let mut records = agent.records.lock().unwrap();
            let thread = &mut records.get_mut(&native.id).unwrap().thread;
            match case {
                "running" => thread.status = agent_protocol::models::SessionStatus::Running,
                "unknown" => thread.status = agent_protocol::models::SessionStatus::Unknown,
                "preparing" => thread.status = agent_protocol::models::SessionStatus::Preparing,
                "child" => {
                    thread.parent_id =
                        Some(SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap())
                }
                "question" => {
                    thread.requests.insert(
                        "permission".into(),
                        Arc::new(agent_protocol::requests::Request {
                            id: "permission".into(),
                            target: agent_protocol::requests::RequestTarget::Session,
                            delivery: agent_protocol::session::RequestDelivery::Awaiting,
                            body: agent_protocol::requests::RequestBody::ToolExecution {
                                tool: "inspect".into(),
                                namespace: None,
                                arguments: serde_json::Value::Null,
                            },
                        }),
                    );
                }
                _ => {}
            }
        }
        if case == "disabled" {
            let client = service.open_session();
            let _: agent_protocol::providers::ProviderSettings =
                super::super::native_fixture::call(
                    &service,
                    client.id(),
                    agent_protocol::protocol::Call::SetProviderEnabled(
                        agent_protocol::providers::SetProviderEnabled {
                            provider: ProviderKind::Codex,
                            enabled: false,
                        },
                    ),
                )
                .await
                .unwrap();
        }
        if case == "refused" {
            agent.continue_gate.close();
        }
        if case == "updating" {
            service.set_history_scope(directory.path().to_owned(), true);
            service.prepare_update().await.unwrap();
        }
        let effect = Effect {
            target: target.clone(),
            action: Action::Resume {
                attempt: attempt.clone(),
                cwd: cwd.clone(),
                at: *at,
                error: error.clone(),
            },
        };
        if case == "stop-after-ack" {
            let acknowledgement = agent.continue_gate.acquire().await.unwrap();
            let mut resuming = Box::pin(Runtime::execute(&service, effect));
            tokio::select! {
                () = agent.continue_started.notified() => {},
                result = &mut resuming => panic!("continuation must await acknowledgement: {result:?}"),
            }
            assert_eq!(agent.continuations.lock().unwrap().len(), 1);
            let stopped = storage
                .decide(
                    &target,
                    Event::Command {
                        id: OperationId::Client(Uuid::new_v4()),
                        receipt: None,
                        command: Command::Interrupt {
                            turn: Some(attempt.turn.clone()),
                        },
                    },
                    context,
                    None,
                )
                .unwrap();
            let mut stopping = Box::pin(async {
                for action in stopped.step.actions {
                    Runtime::execute(
                        &service,
                        Effect {
                            target: target.clone(),
                            action,
                        },
                    )
                    .await
                    .unwrap();
                }
            });
            assert!(futures_util::poll!(&mut stopping).is_pending());
            drop(acknowledgement);
            let (resumed, ()) = tokio::join!(resuming, stopping);
            resumed.unwrap();
            let native_turn = storage
                .execution(&target, attempt.id)
                .unwrap()
                .unwrap()
                .native_turn
                .unwrap();
            assert_eq!(
                *agent.interrupts.lock().unwrap(),
                [(native.id.clone(), native_turn.clone())]
            );
            assert!(matches!(
                storage.state(&target).unwrap().turns[&attempt.turn].state,
                TurnState::Running { interrupt: Some(_) }
            ));
            Runtime::observe(
                &service,
                AgentChange::Session {
                    session: native.clone(),
                    change: SessionChange::Turn {
                        turn: agent_protocol::models::Turn {
                            id: native_turn,
                            status: agent_protocol::execution::TurnStatus::Interrupted,
                            ..Default::default()
                        },
                        completed: true,
                    },
                },
                Some(attempt.id),
            )
            .await
            .unwrap();
        } else if case.starts_with("stop") {
            let workspace = service.inner.worktree_access.write().await;
            let mut resuming = Box::pin(Runtime::execute(&service, effect));
            if case == "stop" {
                assert!(futures_util::poll!(&mut resuming).is_pending());
            }
            let stopped = storage
                .decide(
                    &target,
                    Event::Command {
                        id: OperationId::Client(Uuid::new_v4()),
                        receipt: None,
                        command: Command::Interrupt {
                            turn: Some(attempt.turn.clone()),
                        },
                    },
                    context,
                    None,
                )
                .unwrap();
            // Stop gets the source lock before Resume gets workspace access.
            for action in stopped.step.actions {
                Runtime::execute(
                    &service,
                    Effect {
                        target: target.clone(),
                        action,
                    },
                )
                .await
                .unwrap();
            }
            drop(workspace);
            resuming.await.unwrap();
        } else {
            Runtime::execute(&service, effect).await.unwrap();
        }
        assert!(
            agent.inputs.lock().unwrap().is_empty(),
            "capacity continuation must never replay user input"
        );
        let state = storage.state(&target).unwrap();
        if case.starts_with("stop") {
            assert_eq!(state.turns[&attempt.turn].state, TurnState::Interrupted);
        } else if case == "continued" {
            assert_eq!(
                state.turns[&attempt.turn].state,
                TurnState::Running { interrupt: None }
            );
            assert_eq!(
                *agent.continuations.lock().unwrap(),
                [(native.id.clone(), attempt.id)]
            );
            let native_turn = storage
                .execution(&target, attempt.id)
                .unwrap()
                .unwrap()
                .native_turn
                .unwrap();
            assert_ne!(native_turn.as_str(), "first");
            Runtime::observe(
                &service,
                AgentChange::Session {
                    session: native.clone(),
                    change: SessionChange::Turn {
                        turn: agent_protocol::models::Turn {
                            id: native_turn,
                            status: agent_protocol::execution::TurnStatus::Completed,
                            ..Default::default()
                        },
                        completed: true,
                    },
                },
                Some(attempt.id),
            )
            .await
            .unwrap();
            let completed = storage.state(&target).unwrap();
            assert_eq!(completed.turns.len(), 1);
            assert_eq!(completed.turns[&attempt.turn].state, TurnState::Completed);
        } else if case == "updating" {
            assert_eq!(
                state.turns[&attempt.turn].state,
                TurnState::Waiting(decision::Wait::Retry { at: *at })
            );
            assert_eq!(
                state.turns[&attempt.turn].error.as_ref().unwrap().message,
                "original overload"
            );
        } else {
            let TurnState::Failed(error) = &state.turns[&attempt.turn].state else {
                panic!("unsafe resume must end without scheduling another retry")
            };
            assert!(error.retry.is_none());
            assert_eq!(error.message, "original overload", "case {case}");
        }
    }

    #[test]
    fn pending_answer_admission_enforces_size_and_per_conversation_and_host_limits() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(
            Arc::new(Storage::open(&directory.path().join("state.sqlite"))),
            [],
        );
        let answer = Arc::new(super::super::native_fixture::AnswerResource);
        let pending = |conversation: usize| PendingRequest {
            target: SessionRef::new(None, Uuid::from_u128(conversation as u128 + 1).to_string())
                .unwrap(),
            attempt: Attempt {
                id: Uuid::new_v4(),
                turn: "turn".into(),
                provider: ProviderKind::Codex,
                source: "native".into(),
            },
            origin: RequestOrigin {
                instance: Uuid::new_v4(),
                native_id: serde_json::json!(1),
                provider: ProviderKind::Codex,
                source: answer.clone(),
            },
        };
        assert!(
            runtime
                .retain_request("oversized".into(), 64 * 1024 + 1, pending(0))
                .is_err()
        );
        for conversation in 0..8 {
            for request in 0..32 {
                assert!(
                    runtime
                        .retain_request(
                            format!("{conversation}:{request}").into(),
                            64 * 1024,
                            pending(conversation)
                        )
                        .unwrap()
                );
            }
            if conversation == 0 {
                assert!(
                    runtime
                        .retain_request("conversation-overflow".into(), 1, pending(0))
                        .is_err()
                );
            }
        }
        // A duplicate never consumes another slot, including at the host limit.
        let duplicate_resource = Arc::new(super::super::native_fixture::AnswerResource);
        let mut duplicate = pending(0);
        duplicate.origin.source = duplicate_resource.clone();
        assert!(
            !runtime
                .retain_request("0:0".into(), 64 * 1024, duplicate)
                .unwrap()
        );
        assert_eq!(
            Arc::strong_count(&duplicate_resource),
            1,
            "a duplicate cannot replace the original native answer handle"
        );
        assert!(
            runtime
                .retain_request("host-overflow".into(), 1, pending(8))
                .is_err()
        );
        drop(runtime);
        assert_eq!(
            Arc::strong_count(&answer),
            1,
            "all admitted native resources are released with their owner"
        );
    }

    #[test]
    fn a_new_foreground_turn_cannot_inherit_or_hide_the_previous_turns_background_work() {
        let source = Source {
            target: SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap(),
            io: Default::default(),
            updates: Default::default(),
            text: Default::default(),
            background: Default::default(),
        };
        let attempt = |turn: &str| Attempt {
            id: Uuid::new_v4(),
            turn: turn.into(),
            provider: ProviderKind::Codex,
            source: "native".into(),
        };
        let first = attempt("first");
        let second = attempt("second");
        let task = BackgroundTask {
            id: "monitor".into(),
            kind: BackgroundTaskKind::Command,
            description: None,
        };
        assert_eq!(
            source.roster(first.id, vec![task.clone()]),
            vec![(first.id, vec![task.clone()], false)]
        );
        assert_eq!(
            source.roster(second.id, vec![task.clone()]),
            vec![(first.id, vec![task.clone()], false)]
        );
        assert_eq!(source.background(first.id), vec![task]);
        assert!(source.background(second.id).is_empty());
        assert_eq!(
            source.roster(second.id, vec![]),
            vec![(first.id, vec![], false)]
        );
        assert!(source.background(first.id).is_empty());
    }
}
