use super::*;
use agent_protocol::requests::Answer;

pub(super) struct Source {
    pub target: SessionRef,
    pub session_id: String,
    pub io: AsyncMutex<()>,
    pub(super) updates: AsyncMutex<()>,
    pub(super) text: Mutex<super::text::Text>,
    background: Mutex<HashMap<Uuid, Background>>,
    images: Mutex<HashMap<(Uuid, String), Vec<String>>>,
}

#[derive(Default)]
struct Background {
    tasks: Vec<agent_protocol::execution::BackgroundTask>,
    native_ended: bool,
}

#[derive(Clone)]
pub(super) struct PendingRequest {
    pub target: SessionRef,
    pub attempt: Attempt,
    pub origin: RequestOrigin,
    pub body: agent_protocol::requests::RequestBody,
}

impl Source {
    pub(super) fn record_generated_image(&self, attempt: Uuid, turn: &str, item: &str) {
        let mut pending = self.images.lock().unwrap();
        let images = pending.entry((attempt, turn.into())).or_default();
        if !images.iter().any(|saved| saved == item) {
            images.push(item.into());
        }
    }
    pub(super) fn generated_images(&self, attempt: Uuid, turn: &str) -> Vec<String> {
        self.images
            .lock()
            .unwrap()
            .get(&(attempt, turn.into()))
            .cloned()
            .unwrap_or_default()
    }
    pub(super) fn clear_generated_images(&self, attempt: Uuid, turn: &str) {
        self.images.lock().unwrap().remove(&(attempt, turn.into()));
    }

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

    pub(super) fn background_owners(
        &self,
    ) -> Vec<(Uuid, Vec<agent_protocol::execution::BackgroundTask>)> {
        self.background
            .lock()
            .unwrap()
            .iter()
            .map(|(owner, background)| (*owner, background.tasks.clone()))
            .collect()
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
                    session_id: agent_protocol::orchestration::derived_id(
                        "provider-session",
                        &[
                            "provider-instance",
                            native
                                .native_provider()
                                .expect("native resource has a provider")
                                .key(),
                            "shared",
                        ],
                    ),
                    io: Default::default(),
                    updates: Default::default(),
                    text: Default::default(),
                    background: Default::default(),
                    images: Default::default(),
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
                && source.images.lock().unwrap().is_empty()
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
            inline = if agent.inject_history(&native.id, &context.native_items()).await? {None} else {Some(context)};
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
mod tests;
