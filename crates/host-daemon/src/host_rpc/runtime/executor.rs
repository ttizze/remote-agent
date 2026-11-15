//! Durable per-thread outbox work. The database owns admission and leases;
//! workers own native I/O, and recheck ownership before publishing its result.
use super::super::storage::outbox::Settlement;

// EffectWorker.ts: five attempts, 100 ms exponential backoff capped at 30 s.
const MAX_ATTEMPTS: u64 = 5;
const CONCURRENCY: usize = 4;
const SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
use super::*;
use agent_protocol::orchestration as model;

pub(super) fn context<'a>(state: &model::State, id: &'a str, now: &'a str) -> model::Context<'a> {
    model::Context {
        command_id: id,
        now,
        next_run_ordinal: state
            .next_run_ordinal()
            .expect("stored run ordinal fits the SQL position range"),
        can_steer: false,
    }
}

pub(super) fn provider_capabilities(
    caps: agent_protocol::session::Capabilities,
) -> model::ProviderCapabilities {
    let mut capabilities = model::ProviderCapabilities::default();
    capabilities.sessions.supports_model_switch_in_session = caps.model_change;
    capabilities.threads.can_create_empty_thread = true;
    capabilities.threads.can_read_thread_snapshot = true;
    capabilities.threads.can_rollback_thread = caps.rollback;
    capabilities
        .checkpointing
        .provider_can_rollback_conversation = caps.rollback;
    capabilities
        .checkpointing
        .provider_rollback_returns_snapshot = caps.rollback;
    capabilities.threads.can_fork_thread = caps.fork;
    capabilities.threads.can_fork_from_turn = caps.fork;
    capabilities.turns.supports_interrupt = true;
    capabilities.turns.supports_active_steering = caps.steer;
    capabilities.turns.supports_queued_messages = true;
    capabilities
}

pub(super) fn native_ref(provider: ProviderKind, id: &str) -> model::ProviderRef {
    model::ProviderRef {
        driver: provider.driver().into(),
        native_id: Some(id.into()),
        strength: if provider == ProviderKind::DeepSeek {
            model::NativeRefStrength::Weak
        } else {
            model::NativeRefStrength::Strong
        },
        fingerprint: None,
        ordinal: None,
    }
}

fn failure(error: &Failure) -> model::ProviderFailure {
    model::ProviderFailure {
        class: "provider".into(),
        message: error.message.clone(),
        code: Some(error.code.into()),
        retryable: None,
        reset_at: None,
    }
}

struct LoadedNative {
    native: SessionRef,
    needs_reload: bool,
    forked: bool,
    fork_failure: Option<String>,
}

/// Allocate native resources before publishing recovery. A busy native owner
/// remains an error; a busy fork source can be copied as portable context.
async fn load_native_start(
    service: &HostRpcService,
    agent: &dyn crate::providers::Agent,
    provider: ProviderKind,
    target: &SessionRef,
    cwd: &str,
    model: &str,
    native_id: Option<&str>,
    fork: Option<(
        &model::ProviderThreadFork,
        Option<&model::ContextSourcePoint>,
    )>,
    will_retry: bool,
) -> Result<LoadedNative, Failure> {
    if let Some(id) = native_id {
        if let Ok(current) = agent.state(id).await {
            let expected = SessionRef::new(provider, id.into())
                .map_err(|error| Failure::new("invalid_session", error))?;
            op::validate_thread(&current.response, Some(&expected))
                .map_err(|error| Failure::new("invalid_thread", error))?;
            if current.response.thread.status == agent_protocol::execution::SessionStatus::Running
                || !current.response.thread.requests.is_empty()
            {
                return Err(Failure::new(
                    "provider_switch_busy",
                    "切り替え先のAIがまだ実行中です。完了を待ってください。",
                ));
            }
            // An explicitly incomplete native transcript cannot prove that
            // the already-delivered prefix remains in this native context.
            // Reconstruct it in a fresh resource from canonical history.
            if current
                .response
                .thread
                .history_read_state
                .as_ref()
                .is_none_or(|read| {
                    matches!(
                        read.kind,
                        agent_protocol::session::HistoryReadKind::Complete
                            | agent_protocol::session::HistoryReadKind::Partial
                    )
                })
            {
                return Ok(LoadedNative {
                    native: expected,
                    needs_reload: current.needs_reload
                        || current.response.thread.cwd.as_deref() != Some(cwd),
                    forked: fork.is_some(),
                    fork_failure: None,
                });
            }
        }
    }
    let browser = service.browser_config(&target.to_string())?;
    let mut fork_failure = None;
    let forked = if let Some((origin, point)) = fork {
        if let Some((source, turn)) = service
            .inner
            .conversation
            .storage
            .native_fork_source(origin, point)
            .map_err(storage_failure)?
        {
            let result = match (source.native_id.as_deref(), turn.native_id.as_deref()) {
                (Some(source_id), Some(turn_id))
                    if source.driver == provider.driver()
                        && source.strength == model::NativeRefStrength::Strong
                        && turn.driver == provider.driver() =>
                {
                    agent.fork(source_id, turn_id, browser.clone()).await
                }
                _ => Err(Failure::new(
                    "fork_context_unavailable",
                    "native fork references are unavailable",
                )),
            };
            match result {
                Ok(response) => Some(response),
                Err(error) if will_retry || point.is_none() => {
                    return Err(Failure::before_submission(error));
                }
                Err(error) => {
                    fork_failure = Some(error.message);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };
    let native_fork = forked.is_some();
    let response = match forked {
        Some(response) => response,
        None => agent
            .create(cwd, Some(model), browser)
            .await
            .map_err(Failure::before_submission)?,
    };
    op::validate_thread(&response, None).map_err(|error| Failure::new("invalid_thread", error))?;
    let native = response
        .thread
        .id
        .filter(|id| id.provider == Some(provider))
        .ok_or_else(|| Failure::new("invalid_thread", "created thread uses another provider"))?;
    Ok(LoadedNative {
        native,
        needs_reload: false,
        forked: native_fork,
        fork_failure,
    })
}

impl Runtime {
    pub(super) fn start_effects(service: &HostRpcService) {
        if let Err(error) = service
            .inner
            .conversation
            .storage
            .recover_effects(&commands::at())
            .and_then(|_| {
                service.inner.conversation.storage.runtime_restarted(
                    service.inner.conversation.updating_recovery,
                    &commands::at(),
                )
            })
        {
            tracing::error!(target:"agent",operation="outbox.recover",%error);
            return;
        }
        let owner = Arc::downgrade(&service.inner);
        let mut changed = service.inner.conversation.storage.changes();
        let worker = Uuid::new_v4().to_string();
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            let slots = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
            let available = Arc::new(tokio::sync::Notify::new());
            loop {
                changed.borrow_and_update();
                loop {
                    let Ok(slot) = slots.clone().try_acquire_owned() else {
                        break;
                    };
                    let Some(inner) = owner.upgrade() else {
                        return;
                    };
                    let effect = inner
                        .conversation
                        .storage
                        .claim_effect(&worker, &commands::at());
                    match effect {
                        Ok(Some(effect)) => {
                            let worker = worker.clone();
                            let available = available.clone();
                            workers.spawn(async move {
                                let execute=async move {
                                    let service=HostRpcService {inner};
                                    if let Err(error)=Self::execute_effect(&service,&effect,&worker).await {
                                        tracing::error!(target:"agent",operation="outbox.execute",effect=%effect.id,%error);
                                        Self::retry_effect(&service,&effect,&worker,&error);
                                    }
                                };
                                tokio::pin!(execute);
                                let finished=tokio::time::timeout(SLOT_WAIT,&mut execute).await.is_ok();
                                // T3 frees the scheduler slot after 30 s; the native
                                // write retains its running row and is never replayed.
                                drop(slot);
                                available.notify_one();
                                if !finished {
                                    execute.await;
                                }
                            });
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::error!(target:"agent",operation="outbox.claim",%error);
                            break;
                        }
                    }
                }
                let delay = owner
                    .upgrade()
                    .and_then(|inner| inner.conversation.storage.next_effect_at().ok().flatten())
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
                    .map(|at| {
                        at.timestamp_millis()
                            .saturating_sub(now())
                            .clamp(25, 30_000) as u64
                    })
                    .unwrap_or(30_000);
                tokio::select! {
                    result=changed.changed()=>if result.is_err() {break;},
                    _=available.notified()=>{},
                    _=tokio::time::sleep(std::time::Duration::from_millis(delay))=>{},
                    result=workers.join_next(), if !workers.is_empty()=>{
                        if let Some(Err(error))=result {tracing::error!(target:"agent",operation="outbox.worker",%error);}
                    }
                }
            }
            workers.abort_all();
        });
    }

    fn retry_effect(service: &HostRpcService, effect: &model::Effect, worker: &str, error: &str) {
        let now = commands::at();
        let delay =
            (100_u64 * (1_u64 << effect.attempt_count.saturating_sub(1).min(9))).min(30_000);
        let available = (chrono::DateTime::from_timestamp_millis(super::now())
            .expect("system time is in chrono's range")
            + chrono::Duration::milliseconds(delay as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if effect.attempt_count >= MAX_ATTEMPTS
            && matches!(
                effect.request,
                model::EffectRequest::ProviderThreadRollback { .. }
            )
        {
            if let Err(error) =
                service
                    .inner
                    .conversation
                    .storage
                    .observe_effect(effect, worker, &now, |state| {
                        Ok(model::rollback_failed(state, &effect.id, error, &now))
                    })
            {
                tracing::error!(target:"agent",operation="outbox.rollback-failed",%error);
            }
            return;
        }
        let result = if effect.attempt_count >= MAX_ATTEMPTS {
            Settlement::Failed(error)
        } else {
            Settlement::Retry {
                available_at: &available,
                error,
            }
        };
        if let Err(error) =
            service
                .inner
                .conversation
                .storage
                .finish_effect(&effect.id, Some(worker), &now, result)
        {
            tracing::error!(target:"agent",operation="outbox.settle",%error);
        }
    }

    async fn execute_effect(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
    ) -> Result<(), String> {
        use model::EffectRequest as Request;
        match &effect.request {
            Request::ProviderRuntimeContinue { source_run_id } => {
                service.inner.conversation.storage.continue_runtime(
                    &effect.id,
                    worker,
                    &effect.thread_id,
                    source_run_id,
                    super::recovery::CONTINUE,
                    &commands::at(),
                )?;
            }
            Request::ProviderTurnStart { run_id } => {
                Self::start_turn(service, effect, worker, run_id).await?
            }
            Request::CheckpointCapture { run_id, scope_id } => {
                Self::capture_checkpoint(service, effect, worker, run_id, scope_id)?
            }
            Request::ProviderTurnInterrupt {
                provider_session_id,
                provider_thread_id,
                provider_turn_id,
            } => {
                Self::interrupt_turn(
                    service,
                    effect,
                    worker,
                    provider_session_id,
                    provider_thread_id,
                    provider_turn_id,
                )
                .await?;
            }
            Request::ProviderTurnSteer {
                provider_thread_id,
                provider_turn_id,
                message_id,
                model_selection,
                ..
            } => {
                Self::steer_turn(
                    service,
                    effect,
                    worker,
                    provider_thread_id,
                    provider_turn_id,
                    message_id,
                    model_selection.as_ref(),
                )
                .await?;
            }
            Request::RuntimeRequestRespond {
                request_id,
                decision,
                answers,
                ..
            } => {
                Self::respond_native(
                    service,
                    effect,
                    worker,
                    request_id,
                    *decision,
                    answers.as_ref(),
                )
                .await?;
            }
            Request::ProviderSessionDetach {
                provider_session_id,
                ..
            } => Self::detach_native_session(service, effect, worker, provider_session_id).await?,
            Request::ProviderThreadRollback {
                provider_thread_id,
                checkpoint_id,
                scope_id,
                restore_files,
            } => {
                if *restore_files == Some(true) {
                    return Err("filesystem checkpoint restoration is unavailable".into());
                }
                Self::rollback_thread(
                    service,
                    effect,
                    worker,
                    provider_thread_id,
                    checkpoint_id,
                    scope_id,
                )
                .await?;
            }
            _ => return Err("native effect handler has not been connected".into()),
        }
        // A guarded decision may already have settled an unknown or failed
        // write atomically with its public result. This final worker settlement
        // applies only while it still owns the running row.
        service.inner.conversation.storage.finish_effect(
            &effect.id,
            Some(worker),
            &commands::at(),
            Settlement::Succeeded,
        )?;
        Ok(())
    }

    async fn start_turn(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        run_id: &str,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let Some(state) = storage.effect_state(effect, worker)? else {
            return Ok(());
        };
        let Some(run) = state
            .runs
            .get(run_id)
            .filter(|run| run.status == model::RunStatus::Starting)
        else {
            return Ok(());
        };
        let attempt = state
            .active_attempt(run.active_attempt_id.as_deref())
            .ok_or("run has no attempt")?
            .clone();
        let result = Self::submit_start(service, effect, worker, &state, run, &attempt).await;
        let now = commands::at();
        if let Err(error) = &result
            && error.delivery == agent_protocol::error::Delivery::NotSent
            && error.code != "context_delivery_uncertain"
            && effect.attempt_count < MAX_ATTEMPTS
        {
            return Err(error.to_string());
        }
        let observed = match result {
            Ok(Some((session_id, provider, turn))) => {
                storage.observe_thread(&effect.thread_id, &[], &now, |state| {
                    model::started(
                        state,
                        &attempt.id,
                        &session_id,
                        native_ref(provider, turn.as_str()),
                        context(state, &effect.id, &now),
                    )
                })
            }
            Ok(None) => Ok(Vec::new()),
            Err(error) if error.delivery == agent_protocol::error::Delivery::Unknown => storage
                .observe_effect(effect, worker, &now, |state| {
                    model::uncertain_delivery(state, &effect.id, &error.message, &now)
                }),
            Err(error) => storage.observe_effect(effect, worker, &now, |state| {
                let mut decision = model::fail_start(
                    state,
                    &attempt.id,
                    failure(&error),
                    context(state, &effect.id, &now),
                )?;
                decision.effects.push(model::Effect {
                    status: model::EffectStatus::Failed,
                    lease_owner: None,
                    lease_expires_at: None,
                    last_error: Some(error.message.clone()),
                    completed_at: Some(now.clone()),
                    updated_at: now.clone(),
                    ..state.effects[&effect.id].clone()
                });
                Ok(decision)
            }),
        }?;
        service.schedule_setup_events(&observed);
        Ok(())
    }

    async fn submit_start(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        state: &model::State,
        run: &model::Run,
        attempt: &model::RunAttempt,
    ) -> Result<Option<(String, ProviderKind, agent_protocol::ids::TurnId)>, Failure> {
        let runtime = &service.inner.conversation;
        let target = SessionRef::new(None, state.thread.id.clone())
            .map_err(|error| storage_failure(error.into()))?;
        let _retire_preparation = scopeguard::guard((), |_| runtime.retire_preparation(&target));
        let preparation = runtime.preparation(&target);
        preparation
            .preparing_input
            .lock()
            .unwrap()
            .insert(effect.id.clone(), attempt.id.clone());
        let allocation_stage = scopeguard::guard((), |_| {
            preparation
                .preparing_input
                .lock()
                .unwrap()
                .remove(&effect.id);
        });
        let allocation_io = preparation.io.lock().await;
        let _workspace = service.inner.worktree_access.read().await;
        service.require_not_updating()?;
        let provider =
            ProviderKind::from_instance_id(&run.provider_instance_id).map_err(storage_failure)?;
        let agent = service.creation_agent(
            provider,
            Some(&agent_protocol::models::ModelRef {
                provider,
                id: run.model_selection.model.clone(),
            }),
        )?;
        let cwd = state
            .thread
            .worktree_path
            .clone()
            .or(runtime
                .storage
                .thread_project_root(&target)
                .map_err(storage_failure)?)
            .ok_or_else(|| {
                Failure::new("workspace_unavailable", "conversation has no workspace")
            })?;
        let directory = crate::workspace_files::absolute_path(&cwd)
            .map_err(|error| Failure::new("workspace_unavailable", error))?;
        let mut reload = service
            .restore_workspace(&directory.to_string_lossy())
            .await?;
        let directory = tokio::fs::canonicalize(&directory)
            .await
            .map_err(|error| Failure::new("workspace_unavailable", error))?;
        let cwd = dunce::simplified(&directory)
            .to_str()
            .ok_or_else(|| Failure::new("workspace_unavailable", "workspace path is not UTF-8"))?
            .to_owned();
        Self::cancel_prepared_input(service, &target).map_err(storage_failure)?;
        let expected_native = runtime
            .storage
            .native_thread_reference(&effect.thread_id, &attempt.provider_thread_id)
            .map_err(storage_failure)?;
        let Some(prepared) = runtime
            .storage
            .prepare_context(
                effect,
                worker,
                &service.history_command(&target),
                &commands::at(),
            )
            .map_err(storage_failure)?
        else {
            return Ok(None);
        };
        let state = &prepared;
        let thread = state
            .provider_threads
            .get(&attempt.provider_thread_id)
            .ok_or_else(|| Failure::new("invalid_run", "provider thread is missing"))?;
        if state.unconsumed_fork().is_some() && thread.forked_from.is_none() {
            return Err(Failure::new(
                "fork_context_unavailable",
                "fork context must be prepared before starting a native input",
            ));
        }
        let pending_fork = state
            .unconsumed_fork()
            .filter(|fork| fork.target_run_id.as_ref() == Some(&run.id));
        let origin = thread.forked_from.as_ref().filter(|_| {
            (pending_fork.is_some() || thread.native_thread_ref.is_none())
                && !state.context_handoffs.values().any(|handoff| {
                    handoff.to_provider_thread_id == thread.id
                        && handoff.status == model::HandoffStatus::Ready
                        && matches!(
                            handoff.strategy,
                            model::HandoffStrategy::FullThreadSummary
                                | model::HandoffStrategy::ManualContext
                        )
                })
        });
        // Discovery must see the private binding before it can publish this
        // newly allocated physical identity as another public conversation.
        let catalog = runtime.native_access[&provider].read().await;
        if runtime
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
            .is_none()
        {
            return Ok(None);
        }
        let loaded = load_native_start(
            service,
            agent.as_ref(),
            provider,
            &target,
            &cwd,
            &run.model_selection.model,
            thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref()),
            origin.map(|origin| (origin, pending_fork.map(|fork| &fork.source_point))),
            effect.attempt_count < MAX_ATTEMPTS,
        )
        .await?;
        reload |= loaded.needs_reload;
        let native = loaded.native;
        let _retire = scopeguard::guard(native.clone(), |native| runtime.retire_source(&native));
        let source = runtime.register_native(&target, &native);
        let _io = source.io.lock().await;
        let now = commands::at();
        let capabilities = provider_capabilities(agent.capabilities());
        let session = model::ProviderSession {
            id: source.session_id.clone(),
            driver: provider.driver().into(),
            provider_instance_id: provider.key().into(),
            status: model::ProviderSessionStatus::Ready,
            cwd: cwd.clone(),
            model: Some(run.model_selection.model.clone()),
            capabilities,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_error: None,
        };
        if !runtime
            .storage
            .bind_native_start(
                effect,
                worker,
                session.clone(),
                native_ref(provider, &native.id),
                loaded.forked,
                loaded.fork_failure.as_deref(),
                &now,
            )
            .map_err(storage_failure)?
        {
            // Stop certifies non-delivery immediately, but native creation may
            // still reply later. Save only its physical resource, then let a
            // separately accepted retry own the input and the attempt binding.
            runtime
                .storage
                .bind_abandoned_native_start(
                    effect,
                    &thread.id,
                    expected_native.as_ref(),
                    session,
                    native_ref(provider, &native.id),
                    loaded.forked,
                    &now,
                )
                .map_err(storage_failure)?;
            return Ok(None);
        }
        drop(catalog);
        Self::release_previous_native_sources(service, effect, worker, provider)
            .await
            .map_err(Failure::before_submission)?;
        let Some(current) = runtime
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
        else {
            return Ok(None);
        };
        if !current.accepts_observation(&attempt.id, &source.session_id) {
            return Ok(None);
        }
        let message=current.turn_items.values().find(|item|matches!(&item.body,model::TurnItemBody::UserMessage {message_id,..} if message_id==&run.user_message_id))
            .ok_or_else(||Failure::new("invalid_run","run input is missing"))?;
        let mut input =
            Self::native_submission(service, &native, &cwd, &run.model_selection, message).await?;
        let before_write = commands::at();
        runtime
            .storage
            .observe_effect(effect, worker, &before_write, |state| {
                model::cancel_start(
                    state,
                    &attempt.id,
                    context(state, &effect.id, &before_write),
                )
            })
            .map_err(storage_failure)?;
        if runtime
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
            .is_none()
        {
            return Ok(None);
        }
        let execution =
            Uuid::parse_str(&attempt.id).map_err(|error| Failure::new("invalid_run", error))?;
        let inline = if op::Input::is_compaction(&input.input) {
            None
        } else {
            Self::deliver_context(service, effect, worker, agent.as_ref(), &native, &target).await?
        };
        if let Some((_, history)) = &inline {
            input.input.insert(
                0,
                op::Input::Text {
                    text: format!(
                        "{}\n\nUser message:\n",
                        model::handoffs::render_history(&history.messages, &history.context)
                    ),
                },
            );
        }
        let before_write = commands::at();
        runtime
            .storage
            .observe_effect(effect, worker, &before_write, |state| {
                model::cancel_start(
                    state,
                    &attempt.id,
                    context(state, &effect.id, &before_write),
                )
            })
            .map_err(storage_failure)?;
        if runtime
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
            .is_none()
        {
            return Ok(None);
        }
        // Clearing the owner's non-delivery proof is the user-write boundary.
        // Stop holds the same mutex while certifying cancellation.
        drop(allocation_stage);
        drop(allocation_io);
        let submitted = agent
            .submit(
                &input,
                SubmissionTarget::Start { cwd: &cwd },
                reload,
                service.browser_config(&target.to_string())?,
                execution,
            )
            .await;
        if submitted
            .as_ref()
            .is_err_and(|error| error.delivery == agent_protocol::error::Delivery::NotSent)
            && let Some((handoffs, history)) = &inline
        {
            runtime
                .storage
                .context_delivery(
                    effect,
                    worker,
                    &native,
                    handoffs,
                    history,
                    None,
                    &commands::at(),
                )
                .map_err(storage_failure)?;
        }
        Ok(submitted?
            .turn_id
            .map(|turn| (source.session_id.clone(), provider, turn)))
    }

    pub(super) fn cancel_prepared_input(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let Some(preparation) = runtime.preparations.lock().unwrap().get(target).cloned() else {
            return Ok(());
        };
        let preparing_input = preparation.preparing_input.lock().unwrap();
        if !preparing_input.is_empty() {
            let at = commands::at();
            let command = Uuid::new_v4().to_string();
            let events=runtime.storage.observe_thread(&target.application_id(),&[],&at,|state| {
                let Some(run)=state.active_run() else{return Ok(Default::default());};
                let Some(attempt)=run.active_attempt_id.as_deref() else{return Ok(Default::default());};
                let owned=state.effects.values().any(|effect|effect.status==model::EffectStatus::Running
                    && preparing_input.get(&effect.id).is_some_and(|saved|saved==attempt)
                    && matches!(&effect.request,model::EffectRequest::ProviderTurnStart{run_id} if run_id==&run.id));
                if !owned {return Ok(Default::default());}
                model::cancel_start(state,attempt,context(state,&command,&at))
            })?;
            service.schedule_setup_events(&events);
        }
        Ok(())
    }

    // Native teardown is part of the leased start, before the new prompt. Keep
    // the old attachment until teardown succeeds, so a failed probe/unload
    // cannot orphan its background callbacks or publish a false detachment.
    async fn release_previous_native_sources(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        destination: ProviderKind,
    ) -> Result<(), Failure> {
        let runtime = &service.inner.conversation;
        let Some(state) = runtime
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
        else {
            return Ok(());
        };
        let target = SessionRef::new(None, effect.thread_id.clone())
            .map_err(|error| storage_failure(error.into()))?;
        for thread in state.provider_threads.values().filter(|thread| {
            thread.app_thread_id.as_deref() == Some(&effect.thread_id)
                && thread.provider_instance_id != destination.key()
                && state.provider_session(&thread.id).is_some()
        }) {
            let Some(reference) = thread.native_thread_ref.as_ref() else {
                continue;
            };
            let Some(id) = reference.native_id.as_deref() else {
                continue;
            };
            let provider = ProviderKind::from_instance_id(&thread.provider_instance_id)
                .map_err(storage_failure)?;
            let native = SessionRef::new(provider, id.into())
                .map_err(|error| storage_failure(error.into()))?;
            let _retire =
                scopeguard::guard(native.clone(), |native| runtime.retire_source(&native));
            let source = runtime.register_native(&target, &native);
            let _io = source.io.lock().await;
            let Some(current) = runtime
                .storage
                .effect_state(effect, worker)
                .map_err(storage_failure)?
            else {
                return Ok(());
            };
            let current_thread = current
                .provider_threads
                .get(&thread.id)
                .filter(|current| current.native_thread_ref.as_ref() == Some(reference))
                .ok_or_else(|| {
                    Failure::new(
                        "provider_source_changed",
                        "The previous provider source changed before release",
                    )
                })?;
            if current.provider_session(&thread.id).is_none() {
                continue;
            }
            if !current_thread.pending_background_tasks.is_empty()
                || current.nodes.values().any(|node| {
                    node.provider_thread_id.as_ref() == Some(&thread.id)
                        && matches!(
                            node.status,
                            model::NodeStatus::Pending
                                | model::NodeStatus::Running
                                | model::NodeStatus::Waiting
                        )
                })
                || current.subagents.values().any(|task| {
                    matches!(
                        task.status,
                        model::SubagentStatus::Pending
                            | model::SubagentStatus::Running
                            | model::SubagentStatus::Waiting
                    ) && current
                        .nodes
                        .get(&task.parent_node_id)
                        .is_some_and(|parent| {
                            parent.provider_thread_id.as_ref() == Some(&thread.id)
                        })
                })
            {
                return Err(Failure::new(
                    "provider_background_running",
                    "The previous provider still has running background work",
                ));
            }
            let agent = service.inner.providers.get(provider)?;
            // Claude can own native children without a saved roster. A failed
            // live probe cannot prove that unloading the process is safe.
            let live = agent.state(id).await?;
            if !live.response.thread.background.is_empty()
                || !live.response.thread.requests.is_empty()
                || live.response.thread.status != agent_protocol::execution::SessionStatus::Idle
            {
                return Err(Failure::new(
                    "provider_background_running",
                    "The previous provider still has running background work",
                ));
            }
            if runtime
                .storage
                .effect_state(effect, worker)
                .map_err(storage_failure)?
                .is_none()
            {
                return Ok(());
            }
            agent.unload(id).await?;
            // The SDK completed this resource operation even if Stop settled
            // the requesting input while unload was awaiting its reply.
            let now = commands::at();
            runtime
                .storage
                .observe_thread(&effect.thread_id, &[], &now, |state| {
                    Ok(model::native_resource_unloaded(
                        state,
                        &thread.provider_instance_id,
                        reference,
                        &now,
                    ))
                })
                .map_err(storage_failure)?;
        }
        Ok(())
    }

    async fn deliver_context(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        agent: &dyn crate::providers::Agent,
        native: &SessionRef,
        target: &SessionRef,
    ) -> Result<Option<(Vec<model::ContextHandoff>, model::handoffs::SelectedHistory)>, Failure>
    {
        let storage = &service.inner.conversation.storage;
        let Some((handoffs, selected)) = storage
            .pending_context(effect, worker, native, &service.history_command(target))
            .map_err(storage_failure)?
        else {
            return Ok(None);
        };
        if !storage
            .context_delivery(
                effect,
                worker,
                native,
                &handoffs,
                &selected,
                Some(model::HandoffDeliveryStatus::Pending),
                &commands::at(),
            )
            .map_err(storage_failure)?
        {
            return Err(Failure::new(
                "context_cancelled",
                "context delivery is no longer current",
            ));
        }
        // The user's input has not been submitted. If history delivery is
        // ambiguous, the durable pending marker forces a fresh native thread
        // on the next attempt, never another injection into this thread.
        let injected = agent
            .inject_history(
                &native.id,
                &model::handoffs::native_items(&selected.messages, &selected.context),
            )
            .await
            .map_err(|error| Failure::new("context_delivery_uncertain", error.message))?;
        if injected {
            storage
                .context_delivery(
                    effect,
                    worker,
                    native,
                    &handoffs,
                    &selected,
                    Some(model::HandoffDeliveryStatus::Injected),
                    &commands::at(),
                )
                .map_err(storage_failure)?;
            Ok(None)
        } else {
            Ok(Some((handoffs, selected)))
        }
    }

    async fn native_submission(
        service: &HostRpcService,
        native: &SessionRef,
        cwd: &str,
        selection: &model::ModelSelection,
        item: &model::TurnItem,
    ) -> Result<op::Submission, Failure> {
        let model::TurnItemBody::UserMessage {
            message_id,
            text,
            attachments,
            context,
            ..
        } = &item.body
        else {
            return Err(Failure::new("invalid_input", "item is not a user message"));
        };
        let mut input = vec![op::Input::Text { text: text.clone() }];
        for attachment in attachments {
            let id = attachment
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Failure::new("invalid_attachment", "attachment has no ID"))?;
            let path = service
                .files()
                .attachment_path(id)
                .map_err(|error| Failure::new("attachment_unavailable", error))?
                .to_string_lossy()
                .into_owned();
            if attachment.get("type").and_then(serde_json::Value::as_str) == Some("image") {
                input.push(op::Input::LocalImage { path });
            } else {
                input.push(op::Input::Mention {
                    path,
                    name: attachment
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("attachment")
                        .into(),
                });
            }
        }
        if let Some(records) = context
            .as_ref()
            .and_then(|context| context.get("records"))
            .and_then(serde_json::Value::as_array)
        {
            let catalog = if records.iter().any(|record| {
                record.get("kind").and_then(serde_json::Value::as_str) == Some("skill")
            }) {
                Some(service.inner.providers.catalog(cwd).await)
            } else {
                None
            };
            for record in records {
                match record.get("kind").and_then(serde_json::Value::as_str) {
                    Some("mention") => {
                        let path = record
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                Failure::new("invalid_context", "mention has no path")
                            })?;
                        input.push(op::Input::Mention {
                            path: path.into(),
                            name: record
                                .get("label")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(path)
                                .into(),
                        });
                    }
                    Some("skill") => {
                        let name = record
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| Failure::new("invalid_context", "skill has no name"))?;
                        let candidate = catalog
                            .as_ref()
                            .and_then(|catalog| {
                                catalog.candidates.iter().find(|candidate| {
                                    Some(candidate.invocation.provider) == native.provider
                                        && candidate.invocation.kind
                                            == agent_protocol::composer::InvocationKind::Skill
                                        && candidate.invocation.name == name
                                })
                            })
                            .ok_or_else(|| {
                                Failure::new(
                                    "skill_unavailable",
                                    format!("Skill {name} is unavailable"),
                                )
                            })?;
                        input.push(candidate.invocation.input());
                    }
                    _ => return Err(Failure::new("invalid_context", "unknown message context")),
                }
            }
        }
        let provider = native
            .native_provider()
            .map_err(|error| Failure::new("invalid_provider", error))?;
        Ok(op::Submission {
            thread_id: native.clone(),
            client_user_message_id: message_id.clone().into(),
            input,
            model: Some(agent_protocol::models::ModelRef {
                provider,
                id: selection.model.clone(),
            }),
            effort: selection
                .text_option(if provider == ProviderKind::Claude {
                    "effort"
                } else {
                    "reasoningEffort"
                })
                .map(str::to_owned),
            service_tier: selection.text_option("serviceTier").map(str::to_owned),
            follow_up: op::FollowUp::Queue,
        })
    }

    fn capture_checkpoint(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        run_id: &str,
        scope_id: &str,
    ) -> Result<(), String> {
        let now = commands::at();
        service
            .inner
            .conversation
            .storage
            .observe_effect(effect, worker, &now, |state| {
                let run = state.runs.get(run_id).ok_or("checkpoint run is missing")?;
                let scope = state
                    .checkpoint_scopes
                    .get(scope_id)
                    .ok_or("checkpoint scope is missing")?;
                let mut checkpoints = super::super::storage::checkpoints::missing(
                    scope,
                    run,
                    &state.checkpoints,
                    &now,
                )?;
                let Some(current) = checkpoints.pop() else {
                    return Ok(model::Decision::default());
                };
                let mut decision = model::Decision {
                    changes: checkpoints
                        .into_iter()
                        .map(model::Change::Checkpoint)
                        .collect(),
                    ..Default::default()
                };
                let captured = model::checkpoint_captured(
                    &state.apply(&decision),
                    run_id,
                    current,
                    context(state, &effect.id, &now),
                )?;
                decision.changes.extend(captured.changes);
                decision.effects.extend(captured.effects);
                Ok(decision)
            })
            .map(|_| ())
    }

    async fn interrupt_turn(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        session_id: &str,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), String> {
        let Some(state) = service
            .inner
            .conversation
            .storage
            .effect_state(effect, worker)?
        else {
            return Ok(());
        };
        let thread = state
            .provider_threads
            .get(thread_id)
            .filter(|thread| thread.provider_session_id.as_deref() == Some(session_id))
            .ok_or("interrupt target is missing")?;
        let provider = ProviderKind::from_instance_id(&thread.provider_instance_id)?;
        let native = thread
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_ref())
            .ok_or("native thread is missing")?;
        let turn = state
            .provider_turns
            .get(turn_id)
            .and_then(|turn| turn.native_turn_ref.as_ref())
            .and_then(|reference| reference.native_id.as_ref())
            .ok_or("native turn is missing")?;
        let native_target = SessionRef::new(provider, native.clone()).map_err(str::to_owned)?;
        let target = SessionRef::new(None, effect.thread_id.clone()).map_err(str::to_owned)?;
        let _retire = scopeguard::guard(native_target.clone(), |native| {
            service.inner.conversation.retire_source(&native)
        });
        let source = service
            .inner
            .conversation
            .register_native(&target, &native_target);
        let _io = source.io.lock().await;
        if service
            .inner
            .conversation
            .storage
            .effect_state(effect, worker)?
            .is_none()
        {
            return Ok(());
        }
        service
            .inner
            .providers
            .get(provider)
            .map_err(|error| error.to_string())?
            .interrupt(native, &turn.clone().into())
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn steer_turn(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        thread_id: &str,
        turn_id: &str,
        message_id: &str,
        selection: Option<&model::ModelSelection>,
    ) -> Result<(), String> {
        match Self::submit_steer(
            service, effect, worker, thread_id, turn_id, message_id, selection,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(error) if error.delivery == agent_protocol::error::Delivery::Unknown => {
                let now = commands::at();
                service
                    .inner
                    .conversation
                    .storage
                    .observe_effect(effect, worker, &now, |state| {
                        model::uncertain_delivery(state, &effect.id, &error.message, &now)
                    })
                    .map(|_| ())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    async fn submit_steer(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        thread_id: &str,
        turn_id: &str,
        message_id: &str,
        selection: Option<&model::ModelSelection>,
    ) -> Result<(), Failure> {
        let _workspace = service.inner.worktree_access.read().await;
        service.require_not_updating()?;
        let Some(state) = service
            .inner
            .conversation
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
        else {
            return Ok(());
        };
        let thread = state
            .provider_threads
            .get(thread_id)
            .ok_or_else(|| Failure::new("invalid_steer", "steer target is missing"))?;
        let provider = ProviderKind::from_instance_id(&thread.provider_instance_id)
            .map_err(storage_failure)?;
        let native = SessionRef::new(
            provider,
            thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.clone())
                .ok_or_else(|| Failure::new("invalid_steer", "native thread is missing"))?,
        )
        .map_err(|error| Failure::new("invalid_session", error))?;
        let turn = state
            .provider_turns
            .get(turn_id)
            .ok_or_else(|| Failure::new("invalid_steer", "steer turn is missing"))?;
        let attempt = turn
            .run_attempt_id
            .as_ref()
            .and_then(|id| state.run_attempts.get(id))
            .ok_or_else(|| Failure::new("invalid_steer", "steer attempt is missing"))?;
        let run = state
            .runs
            .get(&attempt.run_id)
            .ok_or_else(|| Failure::new("invalid_steer", "steer run is missing"))?;
        let native_turn = turn
            .native_turn_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_ref())
            .ok_or_else(|| Failure::new("invalid_steer", "native turn is missing"))?;
        let item=state.turn_items.values().find(|item|matches!(&item.body,model::TurnItemBody::UserMessage {message_id:id,..} if id==message_id)).ok_or_else(|| Failure::new("invalid_steer", "steer message is missing"))?;
        let cwd = state
            .thread
            .worktree_path
            .as_deref()
            .ok_or_else(|| Failure::new("invalid_steer", "steer workspace is missing"))?;
        let input = Self::native_submission(
            service,
            &native,
            cwd,
            selection.unwrap_or(&run.model_selection),
            item,
        )
        .await?;
        let target = SessionRef::new(None, effect.thread_id.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let _retire = scopeguard::guard(native.clone(), |native| {
            service.inner.conversation.retire_source(&native)
        });
        let source = service.inner.conversation.register_native(&target, &native);
        let _io = source.io.lock().await;
        if service
            .inner
            .conversation
            .storage
            .effect_state(effect, worker)
            .map_err(storage_failure)?
            .is_none()
        {
            return Ok(());
        }
        service
            .inner
            .providers
            .get(provider)?
            .submit(
                &input,
                SubmissionTarget::Steer(native_turn),
                false,
                service.browser_config(&effect.thread_id)?,
                Uuid::parse_str(&attempt.id)
                    .map_err(|error| Failure::new("invalid_attempt", error.to_string()))?,
            )
            .await?;
        Ok(())
    }

    async fn detach_native_session(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let Some(state) = runtime.storage.effect_state(effect, worker)? else {
            return Ok(());
        };
        let target = SessionRef::new(None, effect.thread_id.clone()).map_err(str::to_owned)?;
        for thread in state.provider_threads.values().filter(|thread| {
            thread.app_thread_id.as_deref() == Some(&effect.thread_id)
                && thread.provider_session_id.as_deref() == Some(session_id)
        }) {
            let Some(id) = thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_ref())
            else {
                continue;
            };
            let provider = ProviderKind::from_instance_id(&thread.provider_instance_id)?;
            let native = SessionRef::new(provider, id.clone()).map_err(str::to_owned)?;
            let _retire =
                scopeguard::guard(native.clone(), |native| runtime.retire_source(&native));
            let source = runtime.register_native(&target, &native);
            let _io = source.io.lock().await;
            if runtime.storage.effect_state(effect, worker)?.is_none() {
                return Ok(());
            }
            service
                .inner
                .providers
                .get(provider)
                .map_err(|error| error.to_string())?
                .unload(id)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
