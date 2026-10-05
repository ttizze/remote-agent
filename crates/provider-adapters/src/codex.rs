use crate::{
    ProviderBatch, error,
    normalize::{self, TurnState},
    now,
};
use agent_transport::peer::PeerEvent;
use codex_app_server::CodexAppServer;
use orchestration::capabilities::capabilities;
use orchestration::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, mpsc, watch};

pub struct CodexAdapter {
    server: Arc<CodexAppServer>,
    states: Mutex<BTreeMap<String, TurnState>>,
    pending_native: Mutex<VecDeque<Value>>,
    output: mpsc::Sender<ProviderBatch>,
    changed: Notify,
    shutdown: watch::Sender<bool>,
}
impl CodexAdapter {
    pub async fn fork(
        &self,
        point: &ContextSourcePoint,
        cwd: &Path,
        model: &str,
    ) -> Result<ProviderRef, AdapterError> {
        let native = point
            .provider_thread_ref
            .as_ref()
            .and_then(|r| r.native_id.as_ref())
            .ok_or_else(|| error("fork source native thread missing"))?;
        let mut params = json!({"threadId":native,"cwd":cwd,"model":model});
        if let Some(turn) = point
            .provider_turn_ref
            .as_ref()
            .and_then(|r| r.native_id.as_ref())
        {
            params["lastTurnId"] = json!(turn);
        }
        let result = self.request("thread/fork", params).await?;
        let id = result["thread"]["id"]
            .as_str()
            .ok_or_else(|| error("Codex fork returned no thread"))?;
        Ok(ProviderRef {
            driver: Driver::Codex,
            native_id: Some(id.into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        })
    }
    pub async fn rollback(
        &self,
        projection: &ThreadProjection,
        scope: &CheckpointScopeId,
        checkpoint: &CheckpointId,
        cwd: &Path,
    ) -> Result<ProviderThread, AdapterError> {
        let (_, checkpoint, provider, target) =
            orchestration::rollback::target(projection, scope, checkpoint).map_err(error)?;
        let native = provider
            .native_thread_ref
            .as_ref()
            .and_then(|r| r.native_id.as_ref())
            .ok_or_else(|| error("native thread missing"))?;
        // An absolute native boundary makes retrying a completed revert safe.
        let boundary = rollback_boundary(
            &provider.id,
            target.map(|t| t.ordinal),
            &projection.provider_turns,
            &projection.attempts,
            &projection.runs,
        );
        if let Some(before) = boundary {
            let metadata = self
                .request(
                    "thread/read",
                    json!({"threadId":native,"includeTurns":false}),
                )
                .await?;
            if metadata["thread"]["historyMode"] != "paginated" {
                return Err(error("Codex legacy history cannot be reverted"));
            }
            self.request("thread/resume", json!({"threadId":native,"excludeTurns":true,"cwd":cwd,"model":projection.thread.model_selection.model})).await?;
            let mut cursor = Value::Null;
            let mut visited = std::collections::BTreeSet::new();
            loop {
                if !visited.insert(cursor.to_string()) {
                    return Err(error("Codex history pagination repeated a cursor"));
                }
                let page = self.request("thread/turns/list", json!({"threadId":native,"cursor":cursor,"limit":100,"sortDirection":"desc","itemsView":"summary"})).await?;
                let turns = page["data"]
                    .as_array()
                    .ok_or_else(|| error("Codex history page is invalid"))?;
                if turns.iter().any(|turn| turn["id"].as_str() == Some(before)) {
                    self.request(
                        "thread/revert",
                        json!({"threadId":native,"beforeTurnId":before}),
                    )
                    .await?;
                    break;
                }
                cursor = page["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
            }
        }
        let mut provider = provider.clone();
        provider.native_conversation_head_ref = target.and_then(|t| t.native_turn_ref.clone());
        provider.status = ProviderThreadStatus::Idle;
        provider.last_run_ordinal = checkpoint.app_run_ordinal.filter(|n| *n > 0);
        provider.updated_at = now();
        Ok(provider)
    }
    pub fn new(server: Arc<CodexAppServer>, output: mpsc::Sender<ProviderBatch>) -> Arc<Self> {
        let (shutdown, receiver) = watch::channel(false);
        let adapter = Arc::new(Self {
            server,
            states: Mutex::new(BTreeMap::new()),
            pending_native: Mutex::new(VecDeque::new()),
            output,
            changed: Notify::new(),
            shutdown,
        });
        let events = adapter.server.subscribe();
        tokio::spawn(pump(Arc::downgrade(&adapter), events, receiver));
        adapter
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value, AdapterError> {
        self.server
            .request::<_, Value>(method, &params)
            .await
            .map_err(error)?
            .outcome
            .map_err(error)
    }
    fn state(
        &self,
        provider_thread_id: &ProviderThreadId,
    ) -> Result<(String, TurnState), AdapterError> {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(_, state)| state.provider_thread.id == *provider_thread_id)
            .map(|(native, state)| (native.clone(), state.clone()))
            .ok_or_else(|| error("Codex runtime is no longer live"))
    }
    pub async fn execute(
        &self,
        effect: &EffectBody,
        projection: &ThreadProjection,
        cwd: &Path,
        tool_servers: Option<Value>,
    ) -> Result<(), AdapterError> {
        match effect {
            EffectBody::Start { run_id } => {
                self.start(projection, run_id, None, cwd, tool_servers)
                    .await
            }
            EffectBody::Steer {
                run_id,
                provider_turn_id: _,
                message_id,
            } => {
                let run = projection
                    .runs
                    .iter()
                    .find(|run| run.id == *run_id)
                    .ok_or_else(|| error("run missing"))?;
                let (native, state) = self.state(
                    run.provider_thread_id
                        .as_ref()
                        .ok_or_else(|| error("provider thread missing"))?,
                )?;
                if state.terminal {
                    return Err(crate::turn_completed());
                }
                let text = projection
                    .messages
                    .iter()
                    .find(|message| message.id == *message_id)
                    .ok_or_else(|| error("steer message missing"))?
                    .text
                    .clone();
                let turn_id = state
                    .turn
                    .native_turn_ref
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_ref())
                    .ok_or_else(|| error("native turn missing"))?;
                if let Err(error) = self.request("turn/steer",json!({"threadId":native,"expectedTurnId":turn_id,"input":[{"type":"text","text":text,"text_elements":[]}]})).await {
                    let completed = self.state(run.provider_thread_id.as_ref().expect("provider thread")).is_ok_and(|(_, state)| state.terminal)
                        || error.message.to_ascii_lowercase().contains("turn completed")
                        || error.message.to_ascii_lowercase().contains("no active turn");
                    let completed = completed
                        || error.message.to_ascii_lowercase().contains("cannot steer a compact turn")
                        || error.message.to_ascii_lowercase().contains("expected active turn id");
                    return Err(if completed { crate::turn_completed() } else { error });
                }
                Ok(())
            }
            EffectBody::Interrupt { run_id, .. } => {
                let run = projection
                    .runs
                    .iter()
                    .find(|run| run.id == *run_id)
                    .ok_or_else(|| error("run missing"))?;
                self.interrupt(
                    run.provider_thread_id
                        .as_ref()
                        .ok_or_else(|| error("provider thread missing"))?,
                )
                .await
            }
            EffectBody::Restart {
                run_id, message_id, ..
            } => {
                let run = projection
                    .runs
                    .iter()
                    .find(|run| run.id == *run_id)
                    .ok_or_else(|| error("run missing"))?;
                let provider_thread_id = run
                    .provider_thread_id
                    .as_ref()
                    .ok_or_else(|| error("provider thread missing"))?;
                self.interrupt(provider_thread_id).await?;
                tokio::time::timeout(std::time::Duration::from_secs(15), async {
                    loop {
                        let changed = self.changed.notified();
                        if self.state(provider_thread_id)?.1.terminal {
                            return Ok::<_, AdapterError>(());
                        }
                        changed.await;
                    }
                })
                .await
                .map_err(|_| error("Codex interrupt did not reach a terminal state"))??;
                self.start(projection, run_id, Some(message_id), cwd, tool_servers)
                    .await
            }
            EffectBody::Respond {
                request_id,
                decision,
                answers,
            } => self.respond(request_id, *decision, answers.as_ref()).await,
            EffectBody::Detach {
                provider_session_id,
                ..
            } => self.detach(provider_session_id).await,
            _ => Err(error("effect belongs to Host resource management")),
        }
    }
    pub async fn respond(
        &self,
        request_id: &RuntimeRequestId,
        decision: Option<ApprovalDecision>,
        answers: Option<&Answers>,
    ) -> Result<(), AdapterError> {
        let request = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            states
                .values_mut()
                .find_map(|state| state.take_request(request_id, &now()))
        }
        .ok_or_else(|| error("Codex approval callback is no longer live"))?;
        let response = normalize::codex_response(&request, decision, answers);
        self.server
            .send_raw(&json!({"id":request.id,"result":response}).to_string())
            .await
            .map_err(error)
    }
    pub async fn detach(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<(), AdapterError> {
        let natives: Vec<_> = {
            let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            states
                .iter()
                .filter(|(_, state)| state.session.id == *provider_session_id)
                .map(|(native, _)| native.clone())
                .collect()
        };
        for native in natives {
            self.request("thread/unsubscribe", json!({"threadId":native}))
                .await?;
            self.states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&native);
        }
        Ok(())
    }
    async fn start(
        &self,
        projection: &ThreadProjection,
        run_id: &RunId,
        message_id: Option<&MessageId>,
        cwd: &Path,
        tool_servers: Option<Value>,
    ) -> Result<(), AdapterError> {
        let run = projection
            .runs
            .iter()
            .find(|run| run.id == *run_id)
            .ok_or_else(|| error("run missing"))?
            .clone();
        if !run.status.is_blocking() {
            return Ok(());
        }
        let attempt = projection
            .attempts
            .iter()
            .find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref())
            .ok_or_else(|| error("attempt missing"))?
            .clone();
        let mut provider_thread = projection
            .provider_threads
            .iter()
            .find(|thread| Some(&thread.id) == run.provider_thread_id.as_ref())
            .ok_or_else(|| error("provider thread missing"))?
            .clone();
        let mut projection = projection.clone();
        let mut params = json!({"cwd":cwd,"model":run.model_selection.model});
        if let Some(tool_servers) = tool_servers {
            params["config"] = json!({"mcp_servers":tool_servers});
        }
        let resume_native = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|r| r.native_id.clone());
        let native = if let Some(native) = resume_native {
            params["threadId"] = json!(native);
            params["excludeTurns"] = json!(true);
            let resumed = match self.request("thread/resume", params.clone()).await {
                Ok(_) => true,
                Err(_) => {
                    self.request("thread/unarchive", json!({"threadId":native}))
                        .await
                        .is_ok()
                        && self.request("thread/resume", params.clone()).await.is_ok()
                }
            };
            if resumed {
                native
            } else {
                projection = crate::portable_fallback(&self.output, &projection, &run).await?;
                provider_thread = projection
                    .provider_threads
                    .iter()
                    .find(|p| p.id == provider_thread.id)
                    .expect("provider retained")
                    .clone();
                params.as_object_mut().expect("object").remove("threadId");
                params
                    .as_object_mut()
                    .expect("object")
                    .remove("excludeTurns");
                self.request("thread/start", params).await?["thread"]["id"]
                    .as_str()
                    .ok_or_else(|| error("native thread missing"))?
                    .to_owned()
            }
        } else {
            self.request("thread/start", params).await?["thread"]["id"]
                .as_str()
                .ok_or_else(|| error("native thread missing"))?
                .to_owned()
        };
        let timestamp = now();
        let session = ProviderSession {
            id: ProviderSessionId::new(format!("provider-session:codex:{}", provider_thread.id))
                .expect("derived id"),
            driver: Driver::Codex,
            provider_instance_id: run.provider_instance_id.clone(),
            status: SessionStatus::Ready,
            cwd: cwd.to_string_lossy().into_owned(),
            model: Some(run.model_selection.model.clone()),
            capabilities: capabilities(Driver::Codex),
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
            last_error: None,
        };
        let mut session = session;
        if let Some(old) = projection
            .provider_sessions
            .iter()
            .find(|old| old.id == session.id)
        {
            session.created_at = old.created_at.clone();
        }
        provider_thread.provider_session_id = Some(session.id.clone());
        provider_thread.native_thread_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some(native.clone()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        for handoff in projection
            .context_handoffs
            .iter()
            .filter(|h| h.target_run_id == run.id && h.status == HandoffStatus::Ready)
        {
            if !provider_thread.handoff_ids.contains(&handoff.id) {
                provider_thread.handoff_ids.push(handoff.id.clone());
            }
        }
        provider_thread.first_run_ordinal.get_or_insert(run.ordinal);
        provider_thread.last_run_ordinal = Some(run.ordinal);
        provider_thread.updated_at = timestamp.clone();
        let ordinal = projection
            .provider_turns
            .iter()
            .filter(|turn| turn.provider_thread_id == provider_thread.id)
            .map(|turn| turn.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let mut state = TurnState::prepare(
            run.clone(),
            attempt,
            provider_thread,
            session,
            ordinal,
            &timestamp,
        );
        let mut initial = state.batch(state.initial_payloads(), &timestamp);
        state.native_agents = Some(Box::new(crate::native_agents::NativeAgents::new(
            projection.thread.clone(),
        )));
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        initial.acknowledged = Some(acknowledged);
        {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = states.get_mut(&native)
                && previous.native_agents.is_some()
            {
                state.native_agents = previous.native_agents.take();
                if let Some(agents) = &mut state.native_agents {
                    agents.update_template(projection.thread.clone());
                }
            }
            states.insert(native.clone(), state);
        }
        self.output.send(initial).await.map_err(error)?;
        if !receipt.await.map_err(error)? {
            self.states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&native);
            return Ok(());
        }
        if self
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&native)
            .is_some_and(|state| state.interrupted)
        {
            self.disconnected(&native, "Interrupted before provider input")
                .await?;
            return Ok(());
        }
        let message_id = message_id.unwrap_or(&run.user_message_id);
        let message = projection
            .messages
            .iter()
            .find(|message| message.id == *message_id)
            .ok_or_else(|| error("run input missing"))?;
        let params = turn_start_params(
            &native,
            &orchestration::context::input_text(&projection, &run, &message.text),
            &run.model_selection,
            projection.thread.runtime_mode,
            projection.thread.interaction_mode,
            cwd,
        );
        if message.text.trim().eq_ignore_ascii_case("/compact") && message.attachments.is_empty() {
            self.request("thread/compact/start", json!({"threadId":native}))
                .await?;
            return Ok(());
        }
        let result = match self.request("turn/start", params).await {
            Ok(result) => result,
            Err(error) => {
                self.disconnected(&native, &error.message).await?;
                return Err(error);
            }
        };
        self.handle(
            "turn/started",
            &json!({"threadId":native,"turn":result["turn"]}),
            None,
        )
        .await
    }
    pub async fn cancel_start(&self, run: &RunId) -> Result<(), AdapterError> {
        let provider = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            states
                .values_mut()
                .find(|state| state.run.id == *run)
                .map(|state| {
                    state.interrupted = true;
                    (
                        state.provider_thread.id.clone(),
                        state.turn.native_turn_ref.is_some(),
                    )
                })
        };
        if let Some((provider, true)) = provider {
            self.interrupt(&provider).await?;
        }
        Ok(())
    }
    async fn interrupt(&self, provider_thread_id: &ProviderThreadId) -> Result<(), AdapterError> {
        let (native, state) = self.state(provider_thread_id)?;
        let (children, closed) = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let state = states
                .get_mut(&native)
                .ok_or_else(|| error("runtime lost"))?;
            state.interrupted = true;
            state
                .native_agents
                .as_mut()
                .map(|a| (a.live_codex_turns(), a.stop(&now())))
                .unwrap_or_default()
        };
        for batch in closed {
            self.output.send(batch.batch()).await.map_err(error)?;
        }
        let mut turns = children;
        if !state.terminal
            && let Some(id) = state
                .turn
                .native_turn_ref
                .as_ref()
                .and_then(|r| r.native_id.as_ref())
        {
            turns.push((native, id.clone()));
        }
        for (thread, turn) in turns {
            if let Err(failure) = self
                .request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
                .await
            {
                self.shutdown();
                return Err(failure);
            }
        }
        Ok(())
    }
    async fn handle(
        &self,
        method: &str,
        params: &Value,
        request_id: Option<&Value>,
    ) -> Result<(), AdapterError> {
        let native = params["threadId"]
            .as_str()
            .or_else(|| params["thread_id"].as_str());
        let Some(native) = native else {
            return Ok(());
        };
        let timestamp = now();
        let (batch, native_batches) = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let owner = if states.contains_key(native) {
                native.to_owned()
            } else {
                match states
                    .iter()
                    .find(|(_, s)| s.native_agents.as_ref().is_some_and(|a| a.contains(native)))
                {
                    Some((id, _)) => id.clone(),
                    None => {
                        let mut pending = self
                            .pending_native
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let frame = json!({"method":method,"params":params,"id":request_id});
                        if pending.len() < 512
                            && pending.iter().map(|v| v.to_string().len()).sum::<usize>()
                                + frame.to_string().len()
                                <= 1024 * 1024
                        {
                            pending.push_back(frame);
                        }
                        return Ok(());
                    }
                }
            };
            let Some(state) = states.get(&owner) else {
                return Ok(());
            };
            if owner == native
                && let (Some(received), Some(current)) = (
                    params["turnId"].as_str(),
                    state
                        .turn
                        .native_turn_ref
                        .as_ref()
                        .and_then(|reference| reference.native_id.as_deref()),
                )
                && received != current
            {
                return Ok(());
            }
            let translated = normalize::codex(
                states.remove(&owner).expect("validated live turn"),
                method,
                params,
                request_id,
                &timestamp,
            );
            let mut state = translated.state;
            let batch = state.batch(translated.payloads, &timestamp);
            let native_batches = std::mem::take(&mut state.native_batches);
            states.insert(owner, state);
            (batch, native_batches)
        };
        if !batch.events.is_empty() {
            self.output.send(batch).await.map_err(error)?;
        }
        for batch in native_batches {
            self.output.send(batch.batch()).await.map_err(error)?;
        }
        self.changed.notify_waiters();
        let ready = {
            let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let mut pending = self
                .pending_native
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let (ready, held): (VecDeque<_>, VecDeque<_>) =
                std::mem::take(&mut *pending).into_iter().partition(|f| {
                    f["params"]["threadId"].as_str().is_some_and(|id| {
                        states.contains_key(id)
                            || states
                                .values()
                                .any(|s| s.native_agents.as_ref().is_some_and(|a| a.contains(id)))
                    })
                });
            *pending = held;
            ready
        };
        for frame in ready {
            Box::pin(self.handle(
                frame["method"].as_str().unwrap_or_default(),
                &frame["params"],
                frame.get("id").filter(|id| !id.is_null()),
            ))
            .await?;
        }
        if method == "turn/started" {
            let provider = self
                .states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(native)
                .filter(|state| state.interrupted && !state.terminal)
                .map(|state| state.provider_thread.id.clone());
            if let Some(provider) = provider {
                self.interrupt(&provider).await?;
            }
        }
        Ok(())
    }
    async fn disconnected(&self, native: &str, message: &str) -> Result<(), AdapterError> {
        let timestamp = now();
        let (batch, native_batches) = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = states.get(native) else {
                return Ok(());
            };
            let translated = normalize::disconnected(state, message, &timestamp);
            let mut state = translated.state;
            let batch = state.batch(translated.payloads, &timestamp);
            let native_batches = std::mem::take(&mut state.native_batches);
            states.insert(native.into(), state);
            (batch, native_batches)
        };
        if !batch.events.is_empty() {
            self.output.send(batch).await.map_err(error)?;
        }
        for batch in native_batches {
            self.output.send(batch.batch()).await.map_err(error)?;
        }
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}
pub fn turn_start_params(
    native_thread_id: &str,
    text: &str,
    model: &ModelSelection,
    runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    cwd: &Path,
) -> Value {
    let (approval, reviewer, sandbox) = match runtime_mode {
        RuntimeMode::ApprovalRequired => ("untrusted", "user", json!({"type":"readOnly"})),
        RuntimeMode::AutoAcceptEdits => ("on-request", "user", json!({"type":"workspaceWrite"})),
        RuntimeMode::Auto => (
            "on-request",
            "auto_review",
            json!({"type":"workspaceWrite"}),
        ),
        RuntimeMode::FullAccess => ("never", "user", json!({"type":"dangerFullAccess"})),
    };
    let mut params = json!({"threadId":native_thread_id,"input":[{"type":"text","text":text,"text_elements":[]}],"cwd":cwd,"model":model.model,"summary":"detailed","approvalPolicy":approval,"approvalsReviewer":reviewer,"sandboxPolicy":sandbox});
    if let Some(effort) = model.options.get("reasoningEffort") {
        params["effort"] = effort.0.clone();
    }
    if let Some(tier) = model.options.get("serviceTier") {
        params["serviceTier"] = tier.0.clone();
    }
    if interaction_mode == InteractionMode::Plan {
        params["collaborationMode"] = json!({"mode":"plan","settings":{"model":model.model,"reasoning_effort":model.options.get("reasoningEffort").map(|value|value.0.clone()).unwrap_or(json!("medium")),"developer_instructions":null}});
    }
    params
}
fn rollback_boundary<'a>(
    provider: &ProviderThreadId,
    target_ordinal: Option<u64>,
    turns: &'a [ProviderTurn],
    attempts: &[RunAttempt],
    runs: &[Run],
) -> Option<&'a str> {
    let removed: std::collections::BTreeSet<_> = runs
        .iter()
        .filter(|r| r.status == RunStatus::RolledBack)
        .map(|r| &r.id)
        .collect();
    turns
        .iter()
        .filter(|turn| {
            &turn.provider_thread_id == provider
                && target_ordinal.is_none_or(|ordinal| turn.ordinal > ordinal)
                && turn
                    .native_turn_ref
                    .as_ref()
                    .and_then(|r| r.native_id.as_ref())
                    .is_some()
                && turn.run_attempt_id.as_ref().is_none_or(|id| {
                    attempts
                        .iter()
                        .find(|a| &a.id == id)
                        .is_none_or(|a| !removed.contains(&a.run_id))
                })
        })
        .min_by_key(|turn| turn.ordinal)
        .and_then(|turn| turn.native_turn_ref.as_ref())
        .and_then(|r| r.native_id.as_deref())
}

async fn pump(
    adapter: std::sync::Weak<CodexAdapter>,
    mut events: tokio::sync::broadcast::Receiver<PeerEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut buffer = crate::stream_buffer::DeltaBuffer::default();
    let mut last_processed_sequence = 0;
    let mut flush = tokio::time::interval(crate::stream_buffer::WINDOW);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    tracing::info!(target: "bex", operation="orchestration.codex.closed", message=%format_args!("cause=shutdown last_processed_sequence={last_processed_sequence} shutdown_requested=true"));
                    break;
                }
            }
            _ = flush.tick() => { if let (Some(adapter), Some(frame)) = (adapter.upgrade(), buffer.flush()) { ingest_frame(&adapter, frame).await; } }
            event = events.recv() => {
                let Some(adapter) = adapter.upgrade() else { break; };
                match event {
                    Ok(PeerEvent::Message(message)) => {
                        match serde_json::from_str::<Value>(&message.value) {
                            Ok(value) => for frame in buffer.push(value) { ingest_frame(&adapter, frame).await; },
                            Err(_) => tracing::error!(target: "bex", operation="orchestration.codex.decode", message="cause=invalid_json"),
                        }
                        last_processed_sequence = message.sequence;
                    },
                    Ok(PeerEvent::Response { sequence }) => { last_processed_sequence = sequence; },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        let _ = buffer.flush();
                        tracing::warn!(target: "bex", operation="orchestration.codex.lag", message=%format_args!("cause=event_stream_lagged last_processed_sequence={last_processed_sequence} missed_events={count}"));
                        let natives: Vec<_> = adapter.states.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
                        for native in natives {
                            let state = adapter.states.lock().unwrap_or_else(|e| e.into_inner()).get(&native).cloned();
                            if let Some(state) = state {
                                let mut turns = state.native_agents.as_ref().map(|a|a.live_codex_turns()).unwrap_or_default();
                                if !state.terminal && let Some(turn) = state.turn.native_turn_ref.as_ref().and_then(|r|r.native_id.as_ref()) { turns.push((native.clone(),turn.clone())); }
                                for (thread, turn) in turns { if adapter.request("turn/interrupt",json!({"threadId":thread,"turnId":turn})).await.is_err() { adapter.server.shutdown().await.ok(); } }
                            }
                            let _ = adapter.disconnected(&native, "Codex notification stream lost events").await;
                        }
                    }
                    result => {
                        if let Some(frame) = buffer.flush() { ingest_frame(&adapter, frame).await; }
                        let message = match result { Ok(PeerEvent::Closed(message)) => message, Err(error) => error.to_string(), _ => unreachable!() };
                        tracing::error!(target: "bex", operation="orchestration.codex.closed", message=%format_args!("cause=peer_closed last_processed_sequence={last_processed_sequence} shutdown_requested=false"));
                        let natives: Vec<_> = adapter.states.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
                        for native in natives { let _ = adapter.disconnected(&native, &message).await; }
                        break;
                    }
                }
            }
        }
    }
}
async fn ingest_frame(adapter: &CodexAdapter, value: Value) {
    let method = value["method"].as_str().unwrap_or("");
    if adapter
        .handle(method, &value["params"], value.get("id"))
        .await
        .is_err()
    {
        // Provider method/error strings can contain conversation content.
        tracing::error!(target: "bex", operation="orchestration.codex.ingest", message="cause=event_processing_failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn child_frames_before_spawn_are_replayed_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fixture-provider");
        std::fs::write(&program, "#!/bin/sh\nread -r initialize\nprintf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fixture\",\"codexHome\":\"/tmp\"}}'\ncat >/dev/null\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(codex_app_server::AppServerConfig {
                program,
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let (output, mut batches) = mpsc::channel(16);
        let adapter = CodexAdapter::new(server.clone(), output);
        let mut state = crate::normalize::tests::state(Driver::Codex);
        state.native_agents = Some(Box::new(crate::native_agents::NativeAgents::new(
            crate::normalize::tests::projection(Driver::Codex).thread,
        )));
        adapter
            .states
            .lock()
            .unwrap()
            .insert("root-native".into(), state);
        adapter
            .handle(
                "turn/started",
                &json!({"threadId":"child","turn":{"id":"actual-turn"}}),
                None,
            )
            .await
            .unwrap();
        adapter
            .handle(
                "item/agentMessage/delta",
                &json!({"threadId":"child","itemId":"answer","delta":"Early output"}),
                None,
            )
            .await
            .unwrap();
        assert!(batches.try_recv().is_err());
        adapter.handle("item/completed",&json!({"threadId":"root-native","item":{"type":"collabAgentToolCall","tool":"spawnAgent","receiverThreadIds":["child"],"prompt":"Task"}}),None).await.unwrap();
        let mut received = vec![];
        while let Ok(batch) = batches.try_recv() {
            received.push(batch);
        }
        assert!(received.iter().all(|b| b.native_owner.is_some()));
        assert!(received.iter().flat_map(|b|&b.events).any(|e| matches!(&e.payload,EventPayload::MessageUpdated(m) if m.text=="Early output" && m.run_id.is_none())));
        assert!(adapter.pending_native.lock().unwrap().is_empty());
        adapter.shutdown();
        server.shutdown().await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_retried_rollback_does_not_revert_valid_native_turns_again() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fixture-provider");
        let calls = directory.path().join("reverts.jsonl");
        std::fs::write(&program, format!(r#"#!/usr/bin/env python3
import json, sys
turns = ["one", "two"]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    method = request["method"]
    if method == "initialize": result = {{"userAgent":"fixture","codexHome":"/tmp"}}
    elif method == "thread/turns/list": result = {{"data":[{{"id":id}} for id in reversed(turns)],"nextCursor":None}}
    elif method == "thread/revert":
        before = request["params"]["beforeTurnId"]
        with open({calls:?}, "a") as log: log.write(json.dumps(request["params"]) + "\n")
        turns = turns[:turns.index(before)]
        result = {{"thread":{{"id":"native"}}}}
    else: result = {{"thread":{{"id":"native","historyMode":"paginated"}}}}
    print(json.dumps({{"id":request["id"],"result":result}}), flush=True)
"#)).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(codex_app_server::AppServerConfig {
                program,
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let (output, _) = mpsc::channel(8);
        let adapter = CodexAdapter::new(server.clone(), output);
        let mut p = crate::normalize::tests::projection(Driver::Codex);
        let state = crate::normalize::tests::state(Driver::Codex);
        let mut provider = state.provider_thread.clone();
        provider.provider_session_id = Some(state.session.id.clone());
        provider.native_thread_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("native".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        p.thread.active_provider_thread_id = Some(provider.id.clone());
        p.provider_threads = vec![provider].into();
        p.provider_sessions.push(state.session);
        p.runs[0].status = RunStatus::Completed;
        let mut second = p.runs[0].clone();
        second.id = RunId::new("second").unwrap();
        second.ordinal = 2;
        let mut attempt = p.attempts[0].clone();
        attempt.id = RunAttemptId::new("second-attempt").unwrap();
        attempt.run_id = second.id.clone();
        second.active_attempt_id = Some(attempt.id.clone());
        let mut first_turn = state.turn.clone();
        first_turn.status = TurnStatus::Completed;
        first_turn.native_turn_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("one".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        let mut second_turn = first_turn.clone();
        second_turn.id = ProviderTurnId::new("second-turn").unwrap();
        second_turn.ordinal = 2;
        second_turn.run_attempt_id = Some(attempt.id.clone());
        second_turn.native_turn_ref.as_mut().unwrap().native_id = Some("two".into());
        let mut failed_before_input = second_turn.clone();
        failed_before_input.id = ProviderTurnId::new("never-started").unwrap();
        failed_before_input.ordinal = 3;
        failed_before_input.status = TurnStatus::Failed;
        failed_before_input.native_turn_ref = None;
        p.runs.push(second);
        p.attempts.push(attempt);
        p.provider_turns = vec![first_turn, second_turn, failed_before_input].into();
        let scope = CheckpointScope {
            id: CheckpointScopeId::new("scope").unwrap(),
            thread_id: p.thread.id.clone(),
            run_id: Some(p.runs[0].id.clone()),
            node_id: p.runs[0].root_node_id.clone().unwrap(),
            parent_scope_id: None,
            provider_thread_id: p.runs[0].provider_thread_id.clone(),
            kind: ScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: directory.path().to_string_lossy().into_owned(),
            created_at: now(),
        };
        let checkpoint = Checkpoint {
            id: CheckpointId::new("checkpoint").unwrap(),
            thread_id: p.thread.id.clone(),
            scope_id: scope.id.clone(),
            run_id: scope.run_id.clone(),
            node_id: scope.node_id.clone(),
            parent_checkpoint_id: None,
            ordinal_within_scope: 1,
            app_run_ordinal: Some(1),
            reference: CheckpointRef::new("refs/test/1").unwrap(),
            status: CheckpointStatus::Ready,
            files: vec![],
            captured_at: now(),
        };
        p.checkpoint_scopes.push(scope.clone());
        p.checkpoints.push(checkpoint.clone());
        for _ in 0..2 {
            adapter
                .rollback(&p, &scope.id, &checkpoint.id, directory.path())
                .await
                .unwrap();
        }
        let reverts = std::fs::read_to_string(calls).unwrap();
        assert_eq!(reverts.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&reverts).unwrap()["beforeTurnId"],
            "two"
        );
        server.shutdown().await.unwrap();
    }
    #[test]
    fn rollback_uses_a_native_boundary_and_ignores_pre_input_failures() {
        let state = crate::normalize::tests::state(Driver::Codex);
        let mut accepted = state.turn.clone();
        accepted.ordinal = 2;
        accepted.native_turn_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("accepted-turn".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        let mut never_started = state.turn.clone();
        never_started.ordinal = 3;
        never_started.status = TurnStatus::Failed;
        assert_eq!(
            rollback_boundary(
                &state.provider_thread.id,
                Some(1),
                &[accepted.clone(), never_started],
                std::slice::from_ref(&state.attempt),
                std::slice::from_ref(&state.run)
            ),
            Some("accepted-turn")
        );
        let mut removed = state.run.clone();
        removed.status = RunStatus::RolledBack;
        assert_eq!(
            rollback_boundary(
                &state.provider_thread.id,
                Some(1),
                &[accepted.clone()],
                std::slice::from_ref(&state.attempt),
                &[removed]
            ),
            None
        );
        assert_eq!(
            rollback_boundary(
                &state.provider_thread.id,
                Some(2),
                &[accepted],
                &[state.attempt],
                &[state.run]
            ),
            None
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn lag_interrupts_the_native_turn_before_failing_its_app_run() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fixture-provider");
        let calls = directory.path().join("interrupts.jsonl");
        std::fs::write(&program, format!(r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    method = request["method"]
    if method == "turn/interrupt":
        with open({calls:?}, "a") as log: log.write(json.dumps(request["params"]) + "\n")
    print(json.dumps({{"id":request["id"],"result":{{"userAgent":"fixture","codexHome":"/tmp"}} if method == "initialize" else {{}}}}), flush=True)
"#)).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(codex_app_server::AppServerConfig {
                program,
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let (output, mut batches) = mpsc::channel(16);
        let (shutdown, receiver) = watch::channel(false);
        let mut state = crate::normalize::tests::state(Driver::Codex);
        state.turn.status = TurnStatus::Running;
        state.turn.native_turn_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("live-turn".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        let adapter = Arc::new(CodexAdapter {
            server: server.clone(),
            states: Mutex::new(BTreeMap::from([("native".into(), state)])),
            pending_native: Mutex::new(VecDeque::new()),
            output,
            changed: Notify::new(),
            shutdown,
        });
        let (events, incoming) = tokio::sync::broadcast::channel(2);
        for _ in 0..6 {
            events.send(PeerEvent::Response { sequence: 0 }).unwrap();
        }
        let task = tokio::spawn(pump(Arc::downgrade(&adapter), incoming, receiver));
        let batch = tokio::time::timeout(std::time::Duration::from_secs(3), batches.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(batch.events.iter().any(|e| matches!(e.payload, EventPayload::RunUpdated(ref r) if r.status == RunStatus::Failed)));
        let params: Value =
            serde_json::from_str(std::fs::read_to_string(calls).unwrap().trim()).unwrap();
        assert_eq!(params["threadId"], "native");
        assert_eq!(params["turnId"], "live-turn");
        assert!(!task.is_finished());
        adapter.shutdown.send(true).unwrap();
        task.await.unwrap();
        server.shutdown().await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn notification_pump_recovers_and_records_causes_without_conversation_payloads() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        agent_transport::diagnostics::initialize(
            directory.path(),
            agent_transport::diagnostics::Component::Host,
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();
        let log = directory.path().join("logs/host.jsonl");
        let program = directory.path().join("fixture-provider");
        std::fs::write(&program, "#!/bin/sh\nread -r initialize\nprintf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fixture\",\"codexHome\":\"/tmp\"}}'\ncat >/dev/null\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(codex_app_server::AppServerConfig {
                program,
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let (output, mut batches) = mpsc::channel(16);
        let (shutdown, receiver) = watch::channel(false);
        let adapter = Arc::new(CodexAdapter {
            server: server.clone(),
            states: Mutex::new(BTreeMap::new()),
            pending_native: Mutex::new(VecDeque::new()),
            output,
            changed: Notify::new(),
            shutdown,
        });
        let (events, incoming) = tokio::sync::broadcast::channel(2);
        for _ in 0..10 {
            events.send(PeerEvent::Message(agent_transport::peer::Reply { sequence: 0, value: json!({"method":"fixture/ignored", "params":{"text":"PRIVATE_CONVERSATION_SENTINEL"}}).to_string().into() })).unwrap();
        }
        let task = tokio::spawn(pump(Arc::downgrade(&adapter), incoming, receiver));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!task.is_finished(), "lag must not stop the pump");
        adapter.states.lock().unwrap().insert(
            "native".into(),
            crate::normalize::tests::state(Driver::Codex),
        );
        events.send(PeerEvent::Message(agent_transport::peer::Reply { sequence: 1, value: json!({"method":"item/agentMessage/delta","params":{"threadId":"native","itemId":"text","delta":"after lag"}}).to_string().into() })).unwrap();
        let batch = tokio::time::timeout(std::time::Duration::from_secs(2), batches.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(batch.events.iter().any(|event| matches!(&event.payload, EventPayload::MessageUpdated(message) if message.text == "after lag")));
        drop(batches);
        events.send(PeerEvent::Message(agent_transport::peer::Reply { sequence: 2, value: json!({"method":"item/agentMessage/delta","params":{"threadId":"native","itemId":"text","delta":"PRIVATE_CONVERSATION_SENTINEL"}}).to_string().into() })).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !std::fs::read_to_string(&log)
                .unwrap()
                .contains("orchestration.codex.ingest")
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !task.is_finished(),
            "a processing error must not stop the pump"
        );
        adapter.shutdown.send(true).unwrap();
        task.await.unwrap();
        // An independently closed stream must report a different terminal cause.
        let (_shutdown, receiver) = watch::channel(false);
        let (events, incoming) = tokio::sync::broadcast::channel(2);
        let task = tokio::spawn(pump(Arc::downgrade(&adapter), incoming, receiver));
        events
            .send(PeerEvent::Closed("PRIVATE_CONVERSATION_SENTINEL".into()))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let lines = std::fs::read_to_string(log).unwrap();
        assert!(!lines.contains("PRIVATE_CONVERSATION_SENTINEL"));
        let records: Vec<Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (operation, cause, level) in [
            (
                "orchestration.codex.lag",
                "cause=event_stream_lagged",
                "warn",
            ),
            (
                "orchestration.codex.ingest",
                "cause=event_processing_failed",
                "error",
            ),
            ("orchestration.codex.closed", "cause=shutdown", "info"),
            ("orchestration.codex.closed", "cause=peer_closed", "error"),
        ] {
            assert!(
                records.iter().any(|record| record["operation"] == operation
                    && record["level"] == level
                    && record["message"].as_str().unwrap().contains(cause)),
                "missing {cause}: {lines}"
            );
        }
        assert!(lines.contains("last_processed_sequence=2 shutdown_requested=true"));
        assert!(lines.contains("missed_events=8"));
        server.shutdown().await.unwrap();
    }
    #[test]
    fn runtime_modes_match_t3_without_enlarging_sandbox_permissions() {
        let model = ModelSelection {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            model: "test-model".into(),
            options: Default::default(),
        };
        for (mode, approval, reviewer, sandbox) in [
            (
                RuntimeMode::ApprovalRequired,
                "untrusted",
                "user",
                "readOnly",
            ),
            (
                RuntimeMode::AutoAcceptEdits,
                "on-request",
                "user",
                "workspaceWrite",
            ),
            (
                RuntimeMode::Auto,
                "on-request",
                "auto_review",
                "workspaceWrite",
            ),
            (RuntimeMode::FullAccess, "never", "user", "dangerFullAccess"),
        ] {
            let params = turn_start_params(
                "thread",
                "input",
                &model,
                mode,
                InteractionMode::Default,
                Path::new("/tmp"),
            );
            assert_eq!(params["approvalPolicy"], approval);
            assert_eq!(params["approvalsReviewer"], reviewer);
            assert_eq!(params["sandboxPolicy"], json!({"type":sandbox}));
        }
        let plan = turn_start_params(
            "thread",
            "input",
            &model,
            RuntimeMode::ApprovalRequired,
            InteractionMode::Plan,
            Path::new("/tmp"),
        );
        assert_eq!(plan["collaborationMode"]["mode"], "plan");
    }
}
