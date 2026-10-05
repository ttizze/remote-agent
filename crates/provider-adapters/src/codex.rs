use crate::{
    ProviderBatch,
    capabilities::capabilities,
    error,
    normalize::{self, TurnState},
    now,
};
use agent_transport::peer::PeerEvent;
use codex_app_server::CodexAppServer;
use orchestration::{worker::AdapterError, *};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, mpsc, watch};

pub struct CodexAdapter {
    server: Arc<CodexAppServer>,
    states: Mutex<BTreeMap<String, TurnState>>,
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
        let removed: std::collections::BTreeSet<_> = projection
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::RolledBack)
            .map(|r| &r.id)
            .collect();
        let count = projection
            .provider_turns
            .iter()
            .filter(|t| {
                t.provider_thread_id == provider.id
                    && target.is_none_or(|target| t.ordinal > target.ordinal)
                    && t.run_attempt_id.as_ref().is_none_or(|id| {
                        projection
                            .attempts
                            .iter()
                            .find(|a| &a.id == id)
                            .is_none_or(|a| !removed.contains(&a.run_id))
                    })
            })
            .count();
        if count > 0 {
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
            let mut remaining = count;
            let mut cursor = Value::Null;
            let mut visited = std::collections::BTreeSet::new();
            let mut before = None;
            while remaining > 0 {
                if !visited.insert(cursor.to_string()) {
                    return Err(error("Codex history pagination repeated a cursor"));
                }
                let page = self.request("thread/turns/list", json!({"threadId":native,"cursor":cursor,"limit":remaining.min(100),"sortDirection":"desc","itemsView":"summary"})).await?;
                for turn in page["data"]
                    .as_array()
                    .ok_or_else(|| error("Codex history page is invalid"))?
                {
                    before = Some(
                        turn["id"]
                            .as_str()
                            .ok_or_else(|| error("native turn id missing"))?
                            .to_owned(),
                    );
                    remaining -= 1;
                    if remaining == 0 {
                        break;
                    }
                }
                cursor = page["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
            }
            if let Some(before) = before {
                self.request(
                    "thread/revert",
                    json!({"threadId":native,"beforeTurnId":before}),
                )
                .await?;
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
        browser: Option<Value>,
    ) -> Result<(), AdapterError> {
        match effect {
            EffectBody::Start { run_id } => {
                self.start(projection, run_id, None, cwd, browser).await
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
                self.request("turn/steer",json!({"threadId":native,"expectedTurnId":turn_id,"input":[{"type":"text","text":text,"text_elements":[]}]})).await?;
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
                self.start(projection, run_id, Some(message_id), cwd, browser)
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
        browser: Option<Value>,
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
        if let Some(browser) = browser {
            params["config"] = json!({"mcp_servers":{"bex_browser":browser}});
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
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        initial.acknowledged = Some(acknowledged);
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(native.clone(), state);
        self.output.send(initial).await.map_err(error)?;
        if !receipt.await.map_err(error)? {
            self.states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&native);
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
    async fn interrupt(&self, provider_thread_id: &ProviderThreadId) -> Result<(), AdapterError> {
        let (native, state) = self.state(provider_thread_id)?;
        if state.terminal {
            return Ok(());
        }
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&native)
            .ok_or_else(|| error("runtime lost"))?
            .interrupted = true;
        let turn_id = state
            .turn
            .native_turn_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_ref())
            .ok_or_else(|| error("native turn is not started"))?;
        self.request(
            "turn/interrupt",
            json!({"threadId":native,"turnId":turn_id}),
        )
        .await?;
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
        let batch = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = states.get(native) else {
                return Ok(());
            };
            if let (Some(received), Some(current)) = (
                params["turnId"].as_str(),
                state
                    .turn
                    .native_turn_ref
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_deref()),
            ) && received != current
            {
                return Ok(());
            }
            let translated = normalize::codex(state, method, params, request_id, &timestamp);
            let mut state = translated.state;
            let batch = state.batch(translated.payloads, &timestamp);
            states.insert(native.into(), state);
            batch
        };
        if !batch.events.is_empty() {
            self.output.send(batch).await.map_err(error)?;
        }
        self.changed.notify_waiters();
        Ok(())
    }
    async fn disconnected(&self, native: &str, message: &str) -> Result<(), AdapterError> {
        let timestamp = now();
        let batch = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = states.get(native) else {
                return Ok(());
            };
            let translated = normalize::disconnected(state, message, &timestamp);
            let mut state = translated.state;
            let batch = state.batch(translated.payloads, &timestamp);
            states.insert(native.into(), state);
            batch
        };
        if !batch.events.is_empty() {
            self.output.send(batch).await.map_err(error)?;
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
async fn pump(
    adapter: std::sync::Weak<CodexAdapter>,
    mut events: tokio::sync::broadcast::Receiver<PeerEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed=shutdown.changed()=>{if changed.is_err()||*shutdown.borrow(){break;}}
            event=events.recv()=>{let Some(adapter)=adapter.upgrade() else{break;};match event{
                Ok(PeerEvent::Message(message))=>{match serde_json::from_str::<Value>(&message.value){Ok(value)=>{let method=value["method"].as_str().unwrap_or("");if let Err(error)=adapter.handle(method,&value["params"],value.get("id")).await{tracing::error!(operation="orchestration.codex.ingest",message=%error);}},Err(error)=>{tracing::error!(operation="orchestration.codex.decode",message=%error);}}}
                Ok(PeerEvent::Response{..})=>{},
                result=>{let message=match result{Ok(PeerEvent::Closed(message))=>message,Err(error)=>error.to_string(),_=>unreachable!()};let natives:Vec<_>=adapter.states.lock().unwrap_or_else(|e|e.into_inner()).keys().cloned().collect();for native in natives{let _=adapter.disconnected(&native,&message).await;}break;}
            }}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
