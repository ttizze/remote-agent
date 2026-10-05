use crate::{
    ProviderBatch, error,
    normalize::{self, TurnState},
    now,
};
use agent_transport::peer::{JsonlReader, JsonlWriter};
use orchestration::capabilities::capabilities;
use orchestration::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, Semaphore, mpsc, watch};

#[derive(Clone)]
pub struct ClaudeConfig {
    pub program: PathBuf,
    pub config_home: PathBuf,
}
enum ProcessInput {
    Frame(Value),
    DrainWake {
        run_id: RunId,
        message_id: MessageId,
        acknowledged: tokio::sync::oneshot::Sender<Result<(), AdapterError>>,
    },
    Prompt {
        run_id: RunId,
        frame: Value,
        steer: bool,
        acknowledged: tokio::sync::oneshot::Sender<Result<(), AdapterError>>,
    },
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum EchoMode {
    #[default]
    Unknown,
    Early,
    ResultOnly,
    Acknowledged,
}
#[derive(Default)]
struct PromptGate {
    mode: EchoMode,
    prompt: Option<String>,
    confirmed: bool,
    frames_before_echo: usize,
    held: Vec<Value>,
    wake_frames: Vec<Value>,
    wake_covered: bool,
}
impl PromptGate {
    fn begin(&mut self, uuid: String) {
        self.prompt = Some(uuid);
        self.confirmed = false;
        self.frames_before_echo = 0;
        self.held.clear();
    }
    fn route(&mut self, frame: Value) -> Vec<Value> {
        let Some(uuid) = self.prompt.as_deref() else {
            return vec![frame];
        };
        let echoed: Vec<&str> = if let Some(array) = frame["user_message_uuids"].as_array() {
            array.iter().filter_map(Value::as_str).collect()
        } else {
            frame["user_message_uuid"].as_str().into_iter().collect()
        };
        if self.confirmed {
            if frame["type"] == "result"
                && (!echoed.is_empty() && !echoed.contains(&uuid)
                    || !frame["origin"].is_null() && frame["origin"]["kind"] != "human")
            {
                self.wake_covered = frame["num_turns"] != 0;
                return vec![];
            }
            return vec![frame];
        }
        if echoed.contains(&uuid) {
            if matches!(self.mode, EchoMode::Unknown | EchoMode::Acknowledged) {
                self.mode = if frame["type"] != "result" && self.frames_before_echo == 0 {
                    EchoMode::Early
                } else {
                    EchoMode::ResultOnly
                };
            }
            self.confirmed = true;
            let mut frames = std::mem::take(&mut self.held);
            frames.push(frame);
            return frames;
        }
        if self.mode == EchoMode::Unknown
            && frame["type"] == "command_lifecycle"
            && frame["command_uuid"].as_str() == Some(uuid)
        {
            self.mode = EchoMode::Acknowledged;
        }
        let root = match frame["type"].as_str() {
            Some("assistant" | "stream_event" | "user") => frame["parent_tool_use_id"].is_null(),
            Some("result") => true,
            _ => false,
        };
        if !root {
            let tool = frame["parent_tool_use_id"]
                .as_str()
                .or_else(|| frame["tool_use_id"].as_str());
            if tool.is_some_and(|tool| {
                self.held.iter().any(|held| {
                    held["message"]["content"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["type"] == "tool_use" && block["id"].as_str() == Some(tool)
                        })
                    }) || held["event"]["content_block"]["id"].as_str() == Some(tool)
                })
            }) {
                self.held.push(frame);
                return vec![];
            }
            return vec![frame];
        }
        self.frames_before_echo += 1;
        if frame["type"] == "result"
            && (!echoed.is_empty()
                || self.mode != EchoMode::Unknown
                    && !frame["origin"].is_null()
                    && frame["origin"]["kind"] != "human")
        {
            if frame["num_turns"] != 0 {
                if self.mode == EchoMode::Early {
                    self.wake_frames.extend(std::mem::take(&mut self.held));
                    self.wake_frames.push(frame.clone());
                } else {
                    self.held.clear();
                    self.wake_covered = true;
                }
            }
            self.frames_before_echo = 0;
            return vec![];
        }
        if self.mode == EchoMode::Early && frame["type"] != "result" {
            self.held.push(frame);
            return vec![];
        }
        let mut frames = std::mem::take(&mut self.held);
        frames.push(frame);
        frames
    }
}
fn admit_prompt(
    current_run: &RunId,
    requested_run: &RunId,
    terminal: bool,
    interrupted: bool,
    stopping: bool,
) -> Result<(), AdapterError> {
    if current_run != requested_run || interrupted || stopping {
        Err(error("Claude prompt cancelled"))
    } else if terminal {
        Err(crate::turn_completed())
    } else {
        Ok(())
    }
}
async fn send_prompt(
    handle: &ProcessHandle,
    run_id: &RunId,
    frame: Value,
    steer: bool,
) -> Result<(), AdapterError> {
    let (acknowledged, receipt) = tokio::sync::oneshot::channel();
    handle
        .input
        .send(ProcessInput::Prompt {
            run_id: run_id.clone(),
            frame,
            steer,
            acknowledged,
        })
        .await
        .map_err(error)?;
    receipt.await.map_err(error)?
}
struct ProcessHandle {
    native_session: String,
    state: Arc<Mutex<TurnState>>,
    input: mpsc::Sender<ProcessInput>,
    stop: watch::Sender<bool>,
    ready: watch::Receiver<Option<Result<(), String>>>,
    done: watch::Receiver<bool>,
    changed: Arc<Notify>,
    cwd: PathBuf,
    credentials_home: PathBuf,
    runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: ModelSelection,
}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
pub struct ClaudeAdapter {
    config: ClaudeConfig,
    processes: tokio::sync::Mutex<BTreeMap<ProviderThreadId, Arc<ProcessHandle>>>,
    capacity: Arc<Semaphore>,
    cancelled_starts: Mutex<std::collections::BTreeSet<RunId>>,
    output: mpsc::Sender<ProviderBatch>,
}
impl ClaudeAdapter {
    pub async fn rollback(
        &self,
        projection: &ThreadProjection,
        scope: &CheckpointScopeId,
        checkpoint: &CheckpointId,
    ) -> Result<ProviderThread, AdapterError> {
        let (_, checkpoint, provider, target) =
            orchestration::rollback::target(projection, scope, checkpoint).map_err(error)?;
        if let Some(handle) = self.processes.lock().await.remove(&provider.id) {
            let _ = handle.stop.send(true);
            wait_done(&handle).await?;
        }
        let mut provider = provider.clone();
        if let Some(target) = target {
            let reference = target
                .native_turn_ref
                .as_ref()
                .filter(|r| {
                    r.native_id
                        .as_ref()
                        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                })
                .ok_or_else(|| error("Claude assistant message cursor is unavailable"))?;
            provider.native_conversation_head_ref = Some(reference.clone());
        } else {
            provider.native_thread_ref = None;
            provider.native_conversation_head_ref = None;
        }
        provider.status = ProviderThreadStatus::Idle;
        provider.last_run_ordinal = checkpoint.app_run_ordinal.filter(|n| *n > 0);
        provider.updated_at = now();
        Ok(provider)
    }
    pub fn new(config: ClaudeConfig, output: mpsc::Sender<ProviderBatch>) -> Self {
        Self {
            config,
            processes: tokio::sync::Mutex::new(BTreeMap::new()),
            capacity: Arc::new(Semaphore::new(8)),
            cancelled_starts: Mutex::new(Default::default()),
            output,
        }
    }
    pub async fn cancel_start(&self, run: &RunId) -> Result<(), AdapterError> {
        self.cancelled_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(run.clone());
        let handle = self
            .processes
            .lock()
            .await
            .values()
            .find(|handle| {
                handle
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .run
                    .id
                    == *run
            })
            .cloned();
        if let Some(handle) = handle {
            handle
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interrupted = true;
            let _ = handle.stop.send(true);
            wait_done(&handle).await?;
        }
        Ok(())
    }
    pub async fn execute(
        &self,
        effect: &EffectBody,
        projection: &ThreadProjection,
        cwd: &Path,
        tool_servers: Option<Value>,
        credentials_home: &Path,
    ) -> Result<(), AdapterError> {
        match effect {
            EffectBody::Start { run_id } => {
                self.start(projection, run_id, cwd, tool_servers, credentials_home)
                    .await
            }
            EffectBody::Steer {
                run_id, message_id, ..
            } => {
                let handle = self.for_run(projection, run_id).await?;
                let message = projection
                    .messages
                    .iter()
                    .find(|message| message.id == *message_id)
                    .ok_or_else(|| error("steer input missing"))?;
                send_prompt(
                    &handle,
                    run_id,
                    user_frame(&handle.native_session, message, true),
                    true,
                )
                .await
            }
            EffectBody::Interrupt { run_id, .. } => {
                let handle = self.for_run(projection, run_id).await?;
                {
                    let mut state = handle.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.terminal {
                        return Ok(());
                    }
                    state.interrupted = true;
                }
                handle.input.send(ProcessInput::Frame(json!({"type":"control_request","request_id":"interrupt","request":{"subtype":"interrupt"}}))).await.map_err(error)?;
                let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        let changed = handle.changed.notified();
                        if handle
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .terminal
                        {
                            break;
                        }
                        changed.await;
                    }
                })
                .await;
                if result.is_err() {
                    let _ = handle.stop.send(true);
                    wait_done(&handle).await?;
                }
                Ok(())
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
            EffectBody::Restart { .. } => {
                Err(error("Claude does not support interrupt-restart steering"))
            }
            _ => Err(error("effect belongs to Host resource management")),
        }
    }
    async fn for_run(
        &self,
        projection: &ThreadProjection,
        run_id: &RunId,
    ) -> Result<Arc<ProcessHandle>, AdapterError> {
        let provider_thread_id = projection
            .runs
            .iter()
            .find(|run| run.id == *run_id)
            .and_then(|run| run.provider_thread_id.as_ref())
            .ok_or_else(|| error("provider thread missing"))?;
        self.processes
            .lock()
            .await
            .get(provider_thread_id)
            .cloned()
            .ok_or_else(|| error("Claude runtime is no longer live"))
    }
    pub async fn respond(
        &self,
        request_id: &RuntimeRequestId,
        decision: Option<ApprovalDecision>,
        answers: Option<&Answers>,
    ) -> Result<(), AdapterError> {
        let (handle, request) = {
            let processes = self.processes.lock().await;
            processes.values().find_map(|handle| {
                let request = handle
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take_request(request_id, &now())?;
                Some((handle.clone(), request))
            })
        }
        .ok_or_else(|| error("Claude callback is no longer live"))?;
        let response = normalize::claude_response(&request, decision, answers);
        handle.input.send(ProcessInput::Frame(json!({"type":"control_response","response":{"subtype":"success","request_id":request.id,"response":response}}))).await.map_err(error)
    }
    pub async fn detach(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<(), AdapterError> {
        let removed = {
            let mut processes = self.processes.lock().await;
            let id = processes
                .iter()
                .find(|(_, handle)| {
                    handle
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .session
                        .id
                        == *provider_session_id
                })
                .map(|(id, _)| id.clone());
            id.and_then(|id| processes.remove(&id))
        };
        if let Some(handle) = removed {
            let _ = handle.stop.send(true);
            wait_done(&handle).await?;
        }
        Ok(())
    }
    async fn initialized(
        &self,
        handle: &ProcessHandle,
        timeout: std::time::Duration,
    ) -> Result<(), AdapterError> {
        let mut ready = handle.ready.clone();
        let result = tokio::time::timeout(timeout, async {
            loop {
                if let Some(result) = ready.borrow().clone() {
                    return result.map_err(error);
                }
                ready.changed().await.map_err(error)?;
            }
        })
        .await
        .unwrap_or_else(|_| Err(error("Claude initialization timed out")));
        if result.is_err() {
            let provider = handle
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .provider_thread
                .id
                .clone();
            self.processes.lock().await.remove(&provider);
            let _ = handle.stop.send(true);
            wait_done(handle).await?;
        }
        result
    }
    async fn start(
        &self,
        projection: &ThreadProjection,
        run_id: &RunId,
        cwd: &Path,
        tool_servers: Option<Value>,
        credentials_home: &Path,
    ) -> Result<(), AdapterError> {
        let _cancel = scopeguard::guard(run_id.clone(), |run| {
            self.cancelled_starts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&run);
        });
        if self
            .cancelled_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(run_id)
        {
            return Ok(());
        }
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
        let existing = self
            .processes
            .lock()
            .await
            .get(&provider_thread.id)
            .cloned();
        let wake = projection
            .messages
            .iter()
            .find(|m| m.id == run.user_message_id)
            .filter(|m| m.native_continuation.is_some())
            .map(|m| m.id.clone());
        if let Some(id) = &wake
            && existing.as_ref().is_none_or(|h| {
                *h.done.borrow()
                    || !h
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .native_agents
                        .as_ref()
                        .is_some_and(|a| a.has_buffered_wake(id))
            })
        {
            return Err(error("Native continuation buffer is no longer available"));
        }
        let reusable = existing.as_ref().is_some_and(|handle| {
            wake.is_some()
                || handle.cwd == cwd
                    && handle.credentials_home == credentials_home
                    && handle.model == run.model_selection
                    && handle.runtime_mode == projection.thread.runtime_mode
                    && handle.interaction_mode == projection.thread.interaction_mode
                    && provider_thread
                        .native_thread_ref
                        .as_ref()
                        .and_then(|r| r.native_id.as_ref())
                        == Some(&handle.native_session)
                    && !*handle.done.borrow()
        });
        if !reusable && let Some(handle) = &existing {
            self.processes.lock().await.remove(&provider_thread.id);
            let _ = handle.stop.send(true);
            wait_done(handle).await?;
        }
        let native = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let native_fork = projection.context_transfers.iter().any(|t| {
            t.target_run_id.as_ref() == Some(&run.id) && t.status == TransferStatus::ResolvedNative
        });
        let resume = !native_fork
            && provider_thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_ref())
                .is_some();
        let timestamp = now();
        let session = ProviderSession {
            id: ProviderSessionId::new(format!("provider-session:claude:{}", provider_thread.id))
                .expect("derived id"),
            driver: Driver::Claude,
            provider_instance_id: run.provider_instance_id.clone(),
            status: SessionStatus::Ready,
            cwd: cwd.to_string_lossy().into_owned(),
            model: Some(run.model_selection.model.clone()),
            capabilities: capabilities(Driver::Claude),
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
            driver: Driver::Claude,
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
        let provider_thread_id = provider_thread.id.clone();
        let mut state = TurnState::prepare(
            run.clone(),
            attempt,
            provider_thread,
            session,
            ordinal,
            &timestamp,
        );
        state.native_wake_drain = wake.clone();
        let permit = if reusable {
            None
        } else {
            self.evict_idle().await?;
            Some(
                self.capacity
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| AdapterError {
                        message: "Claude process capacity is busy".into(),
                        retryable: true,
                        turn_completed: false,
                    })?,
            )
        };
        let mut initial = state.batch(state.initial_payloads(), &timestamp);
        state.native_agents = Some(Box::new(crate::native_agents::NativeAgents::new(
            projection.thread.clone(),
        )));
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        initial.acknowledged = Some(acknowledged);
        self.output.send(initial).await.map_err(error)?;
        if !receipt.await.map_err(error)? {
            return Ok(());
        }
        let handle = if reusable {
            let handle = existing.expect("reusable process exists");
            {
                let mut previous = handle.state.lock().unwrap_or_else(|e| e.into_inner());
                if previous.native_agents.is_some() {
                    state.native_agents = previous.native_agents.take();
                    if let Some(agents) = &mut state.native_agents {
                        agents.update_template(projection.thread.clone());
                        if let Some(id) = &wake {
                            agents.begin_drain(id);
                        }
                    }
                }
                *previous = state;
            }
            handle
        } else {
            let (mut command, tool_config) = process_command(
                &self.config,
                credentials_home,
                cwd,
                &native,
                resume,
                &run.model_selection,
                projection.thread.runtime_mode,
                projection.thread.interaction_mode,
                tool_servers.clone(),
            )?;
            if let Some(transfer) = projection.context_transfers.iter().find(|t| {
                t.target_run_id.as_ref() == Some(&run.id)
                    && t.status == TransferStatus::ResolvedNative
            }) {
                if let Some(source) = transfer
                    .source_point
                    .provider_thread_ref
                    .as_ref()
                    .and_then(|r| r.native_id.as_ref())
                {
                    command.args(["--resume", source, "--fork-session"]);
                    if let Some(cursor) = transfer
                        .source_point
                        .provider_turn_ref
                        .as_ref()
                        .and_then(|r| r.native_id.as_ref())
                    {
                        command.args(["--resume-session-at", cursor]);
                    }
                }
            } else if let Some(cursor) = state
                .provider_thread
                .native_conversation_head_ref
                .as_ref()
                .and_then(|r| r.native_id.as_ref())
            {
                command.args(["--resume-session-at", cursor]);
            }
            spawn(
                command,
                tool_config,
                state,
                native,
                permit.expect("new process has reserved capacity"),
                self.output.clone(),
                cwd,
                credentials_home,
                projection.thread.runtime_mode,
                projection.thread.interaction_mode,
                run.model_selection.clone(),
            )?
        };
        self.processes
            .lock()
            .await
            .insert(provider_thread_id, handle.clone());
        if let Err(initial) = self
            .initialized(&handle, std::time::Duration::from_secs(30))
            .await
        {
            if resume || native_fork {
                let fresh = crate::portable_fallback(&self.output, projection, &run).await?;
                return Box::pin(self.start(&fresh, run_id, cwd, tool_servers, credentials_home))
                    .await;
            }
            return Err(initial);
        }
        if self
            .cancelled_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(run_id)
        {
            let _ = handle.stop.send(true);
            wait_done(&handle).await?;
            return Ok(());
        }
        if *handle.done.borrow() {
            let provider_id = handle
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .provider_thread
                .id
                .clone();
            self.processes.lock().await.remove(&provider_id);
            return Err(error("Claude exited before accepting input"));
        }
        let message = projection
            .messages
            .iter()
            .find(|message| message.id == run.user_message_id)
            .ok_or_else(|| error("run input missing"))?;
        let mut message = message.clone();
        if let Some(message_id) = wake {
            let (acknowledged, received) = tokio::sync::oneshot::channel();
            handle
                .input
                .send(ProcessInput::DrainWake {
                    run_id: run_id.clone(),
                    message_id,
                    acknowledged,
                })
                .await
                .map_err(error)?;
            return received.await.map_err(error)?;
        }
        message.text = orchestration::context::input_text(projection, &run, &message.text);
        send_prompt(
            &handle,
            run_id,
            user_frame(&handle.native_session, &message, false),
            false,
        )
        .await
    }
    async fn evict_idle(&self) -> Result<(), AdapterError> {
        if self.capacity.available_permits() > 0 {
            return Ok(());
        }
        let removed = {
            let mut processes = self.processes.lock().await;
            let id = processes
                .iter()
                .find(|(_, handle)| {
                    let state = handle.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.terminal && !state.native_agents.as_ref().is_some_and(|a| a.is_live())
                })
                .map(|(id, _)| id.clone());
            id.and_then(|id| processes.remove(&id))
        };
        if let Some(handle) = removed {
            let _ = handle.stop.send(true);
            wait_done(&handle).await?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        let handles = std::mem::take(&mut *self.processes.lock().await);
        for handle in handles.values() {
            let _ = handle.stop.send(true);
        }
        for handle in handles.values() {
            let _ = wait_done(handle).await;
        }
    }
}
pub fn permission_mode(runtime: RuntimeMode, interaction: InteractionMode) -> &'static str {
    if interaction == InteractionMode::Plan {
        return "plan";
    }
    match runtime {
        RuntimeMode::ApprovalRequired => "default",
        RuntimeMode::AutoAcceptEdits => "acceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "bypassPermissions",
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "explicit resolved provider launch values"
)]
fn process_command(
    config: &ClaudeConfig,
    credentials_home: &Path,
    cwd: &Path,
    native: &str,
    resume: bool,
    model: &ModelSelection,
    runtime: RuntimeMode,
    interaction: InteractionMode,
    tool_servers: Option<Value>,
) -> Result<(tokio::process::Command, Option<tempfile::NamedTempFile>), AdapterError> {
    let mut command = bex_process::command(&config.program).map_err(error)?;
    command
        .env("CLAUDE_CONFIG_DIR", &config.config_home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", credentials_home)
        .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("CLAUDE_CODE_OAUTH_REFRESH_TOKEN")
        .current_dir(cwd)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            permission_mode(runtime, interaction),
        ])
        .arg(if resume { "--resume" } else { "--session-id" })
        .arg(native)
        .arg("--model")
        .arg(&model.model);
    if runtime == RuntimeMode::FullAccess {
        command.arg("--allow-dangerously-skip-permissions");
    }
    if let Some(effort) = model
        .options
        .get("effort")
        .and_then(|option| option.0.as_str())
    {
        command.arg("--effort").arg(effort);
    }
    let tool_config = tool_servers
        .map(|servers| {
            let mut file = tempfile::NamedTempFile::new().map_err(error)?;
            serde_json::to_writer(file.as_file_mut(), &json!({"mcpServers":servers}))
                .map_err(error)?;
            file.as_file_mut().sync_all().map_err(error)?;
            command.arg("--mcp-config").arg(file.path());
            Ok::<_, AdapterError>(file)
        })
        .transpose()?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok((command, tool_config))
}
/// Short-lived native control query for provider-owned metadata; no inference input is sent.
pub async fn query_control(
    config: &ClaudeConfig,
    credentials_home: &Path,
    cwd: &Path,
    request: Option<(&str, Value)>,
) -> Result<Value, AdapterError> {
    let model = ModelSelection {
        instance_id: ProviderInstanceId::new("claude").expect("constant id"),
        model: "default".into(),
        options: Default::default(),
    };
    let (mut command, _tool_config) = process_command(
        config,
        credentials_home,
        cwd,
        &uuid::Uuid::new_v4().to_string(),
        false,
        &model,
        RuntimeMode::ApprovalRequired,
        InteractionMode::Default,
        None,
    )?;
    let mut child = command.spawn().map_err(error)?;
    let mut writer = JsonlWriter::new(
        child
            .stdin
            .take()
            .ok_or_else(|| error("Claude stdin missing"))?,
    );
    let mut reader = JsonlReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| error("Claude stdout missing"))?,
    );
    let result=tokio::time::timeout(std::time::Duration::from_secs(30),async {
        writer.write_line(&json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize"}}).to_string()).await.map_err(error)?;
        let mut expected="initialize";
        while let Some(line)=reader.read_line().await.map_err(error)? {
            let frame:Value=serde_json::from_str(&line).map_err(error)?;
            if frame["type"]!="control_response"||frame["response"]["request_id"]!=expected {continue;}
            if frame["response"]["subtype"]!="success" {return Err(error("Claude control query rejected"));}
            if expected=="initialize" && let Some((id,body))=&request {writer.write_line(&json!({"type":"control_request","request_id":id,"request":body}).to_string()).await.map_err(error)?;expected=id;continue;}
            return Ok(frame["response"]["response"].clone());
        }
        Err(error("Claude closed before returning metadata"))
    }).await.map_err(|_|error("Claude control query timed out"));
    drop(writer);
    drop(reader);
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    result?
}
fn user_frame(native: &str, message: &ConversationMessage, steer: bool) -> Value {
    let mut frame = json!({"type":"user","uuid":uuid::Uuid::new_v4().to_string(),"session_id":native,"message":{"role":"user","content":[{"type":"text","text":message.text}]},"parent_tool_use_id":null});
    if steer {
        frame["priority"] = json!("now");
    }
    frame
}
#[expect(
    clippy::too_many_arguments,
    reason = "process owner receives resolved launch values"
)]
fn spawn(
    mut command: tokio::process::Command,
    tool_config: Option<tempfile::NamedTempFile>,
    state: TurnState,
    native: String,
    permit: tokio::sync::OwnedSemaphorePermit,
    output: mpsc::Sender<ProviderBatch>,
    cwd: &Path,
    credentials_home: &Path,
    runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: ModelSelection,
) -> Result<Arc<ProcessHandle>, AdapterError> {
    let mut child = command.spawn().map_err(error)?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| error("Claude stdin missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("Claude stdout missing"))?;
    let (input_sender, mut commands) = mpsc::channel::<ProcessInput>(256);
    let (stop, mut shutdown) = watch::channel(false);
    let (ready, ready_receiver) = watch::channel(None);
    let (done, done_receiver) = watch::channel(false);
    let state = Arc::new(Mutex::new(state));
    let changed = Arc::new(Notify::new());
    let handle = Arc::new(ProcessHandle {
        native_session: native,
        state: state.clone(),
        input: input_sender,
        stop,
        ready: ready_receiver,
        done: done_receiver,
        changed: changed.clone(),
        cwd: cwd.into(),
        credentials_home: credentials_home.into(),
        runtime_mode,
        interaction_mode,
        model,
    });
    tokio::spawn(async move {
        let _permit = permit;
        let _tool_config = tool_config;
        let mut writer = JsonlWriter::new(input);
        let mut reader = JsonlReader::new(stdout);
        let mut gate = PromptGate::default();
        let mut buffer = crate::stream_buffer::DeltaBuffer::default();
        let mut flush = tokio::time::interval(crate::stream_buffer::WINDOW);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result:Result<(),AdapterError>=async{
            writer.write_line(&json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize"}}).to_string()).await.map_err(error)?;
            loop{tokio::select!{
                stopped=shutdown.changed()=>{if stopped.is_err()||*shutdown.borrow(){break;}}
                _=flush.tick()=>{if let Some(frame)=buffer.flush(){ingest_claude_frame(&state,&output,&mut writer,frame).await?;changed.notify_waiters();}}
                command=commands.recv()=>{let Some(command)=command else{break;}; match command {
                    ProcessInput::Frame(frame) => writer.write_line(&frame.to_string()).await.map_err(error)?,
                    ProcessInput::DrainWake { run_id,message_id,acknowledged } => {
                        if let Some(frame)=buffer.flush() { ingest_claude_frame(&state,&output,&mut writer,frame).await?; }
                        let frames={ let mut current=state.lock().unwrap_or_else(|e|e.into_inner());let admission=admit_prompt(&current.run.id,&run_id,current.terminal,current.interrupted,*shutdown.borrow());
                            match admission { Err(failure)=>Err(failure),Ok(())=>{ current.native_wake_drain=None;current.native_agents.as_mut().and_then(|a|a.take_wake(&message_id)).ok_or_else(||error("Native continuation buffer is unavailable")) } }
                        };
                        match frames { Err(failure)=>{ let _=acknowledged.send(Err(failure)); },Ok(frames)=>{
                            gate.prompt=None;gate.held.clear();
                            for frame in frames { ingest_claude_frame(&state,&output,&mut writer,frame).await?; }
                            let _=acknowledged.send(Ok(()));
                        } }
                    }
                    ProcessInput::Prompt { run_id, frame, steer, acknowledged } => {
                        if let Some(buffered) = buffer.flush() { ingest_claude_frame(&state, &output, &mut writer, buffered).await?; }
                        let admission = {
                            let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
                            let admission = admit_prompt(&current.run.id, &run_id, current.terminal, current.interrupted, *shutdown.borrow());
                            if admission.is_ok() { current.steered = steer; }
                            admission
                        };
                        if let Err(error) = admission { let _ = acknowledged.send(Err(error)); continue; }
                        gate.begin(frame["uuid"].as_str().unwrap_or_default().into());
                        let written = writer.write_line(&frame.to_string()).await.map_err(error);
                        let failed = written.is_err();
                        let _ = acknowledged.send(written);
                        if failed { return Err(error("Claude prompt write failed")); }
                    }
                }}
                line=reader.read_line()=>{let Some(line)=line.map_err(error)?else{break;};let frame:Value=serde_json::from_str(&line).map_err(error)?;
                    if frame["type"]=="control_response"&&frame["response"]["request_id"]=="initialize" {ready.send_replace(Some(if frame["response"]["subtype"]=="success"{Ok(())}else{Err("Claude initialization rejected".into())}));continue;}
                    { let current=state.lock().unwrap_or_else(|e|e.into_inner());if current.terminal && !current.interrupted && current.native_agents.as_ref().is_some_and(|a|a.has_wake()) { gate.prompt=None;gate.held.clear(); } }
                    for routed in gate.route(frame) {
                        for frame in buffer.push(routed) { ingest_claude_frame(&state, &output, &mut writer, frame).await?; }
                    }
                    if std::mem::take(&mut gate.wake_covered) && let Some(agents)=&mut state.lock().unwrap_or_else(|e|e.into_inner()).native_agents { agents.clear_wake_report(); }
                    if !gate.wake_frames.is_empty() {
                        let batch={ let mut current=state.lock().unwrap_or_else(|e|e.into_inner());let mut agents=current.native_agents.take();let mut offer=None;
                            if let Some(agents)=&mut agents { for frame in std::mem::take(&mut gate.wake_frames) { if let Some(next)=agents.buffer_wake(&current,frame) { offer=Some(next); } } }
                            current.native_agents=agents;current.native_continuation_offer=offer;current.batch(vec![],&now())
                        };
                        send_provider_batch(&state,&output,batch).await?;
                    }
                    changed.notify_waiters();
                }
            }}Ok(())
        }.await;
        if let Some(frame) = buffer.flush() {
            let _ = ingest_claude_frame(&state, &output, &mut writer, frame).await;
        }
        let timestamp = now();
        let message = result
            .as_ref()
            .err()
            .map(|error| error.message.as_str())
            .unwrap_or("Claude process closed");
        let initialized = ready.borrow().as_ref().is_some_and(Result::is_ok);
        if ready.borrow().is_none() {
            ready.send_replace(Some(Err("Claude exited before initialization".into())));
        }
        let (batch, native_batches) = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if initialized {
                let translated = normalize::disconnected(&state, message, &timestamp);
                *state = translated.state;
                (
                    Some(state.batch(translated.payloads, &timestamp)),
                    std::mem::take(&mut state.native_batches),
                )
            } else {
                (None, vec![])
            }
        };
        if let Some(batch) = batch
            && !batch.events.is_empty()
        {
            let _ = output.send(batch).await;
        }
        for batch in native_batches {
            let _ = output.send(batch.batch()).await;
        }
        drop(writer);
        let _ = child.wait().await;
        done.send_replace(true);
        changed.notify_waiters();
    });
    Ok(handle)
}
async fn wait_done(handle: &ProcessHandle) -> Result<(), AdapterError> {
    let mut done = handle.done.clone();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if *done.borrow() {
                return Ok::<_, AdapterError>(());
            }
            done.changed().await.map_err(error)?;
        }
    })
    .await
    .map_err(|_| error("Claude supervisor did not finish"))?
}

async fn ingest_claude_frame<W: tokio::io::AsyncWrite + Unpin>(
    state: &Mutex<TurnState>,
    output: &mpsc::Sender<ProviderBatch>,
    writer: &mut JsonlWriter<W>,
    frame: Value,
) -> Result<(), AdapterError> {
    let timestamp = now();
    let (batch, native_batches, responses) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let translated = normalize::claude(state.take_owned(), &frame, &timestamp);
        *state = translated.state;
        let batch = state.batch(translated.payloads, &timestamp);
        let native_batches = std::mem::take(&mut state.native_batches);
        (batch, native_batches, translated.immediate_responses)
    };
    send_provider_batch(state, output, batch).await?;
    for batch in native_batches {
        send_provider_batch(state, output, batch.batch()).await?;
    }
    for (id, response) in responses {
        writer.write_line(&json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}}).to_string()).await.map_err(error)?;
    }
    Ok(())
}

async fn send_provider_batch(
    state: &Mutex<TurnState>,
    output: &mpsc::Sender<ProviderBatch>,
    mut batch: ProviderBatch,
) -> Result<(), AdapterError> {
    if batch.events.is_empty() && batch.native_continuation_offer.is_none() {
        return Ok(());
    }
    let offer_id = batch
        .native_continuation_offer
        .as_ref()
        .map(|o| o.message_id.clone());
    let receipt = if offer_id.is_some() {
        let (ack, received) = tokio::sync::oneshot::channel();
        batch.acknowledged = Some(ack);
        Some(received)
    } else {
        None
    };
    output.send(batch).await.map_err(error)?;
    if let Some(receipt) = receipt
        && !receipt.await.map_err(error)?
        && let Some(id) = offer_id
        && let Some(agents) = &mut state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .native_agents
    {
        agents.discard_wake(&id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_scope_is_kept_in_a_private_ephemeral_file_instead_of_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let config = ClaudeConfig {
            program: "/bin/sh".into(),
            config_home: directory.path().into(),
        };
        let model = ModelSelection {
            instance_id: ProviderInstanceId::new("claude").unwrap(),
            model: "default".into(),
            options: Default::default(),
        };
        let servers = json!({"bex_orchestration":{"command":"host-daemon","env":{"BEX_ORCHESTRATION_TOKEN":"fixture-scope"}}});
        let (command, file) = process_command(
            &config,
            directory.path(),
            directory.path(),
            "native",
            false,
            &model,
            RuntimeMode::ApprovalRequired,
            InteractionMode::Default,
            Some(servers.clone()),
        )
        .unwrap();
        assert!(
            command
                .as_std()
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("fixture-scope"))
        );
        let file = file.unwrap();
        let path = file.path().to_owned();
        let saved: Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
        assert_eq!(saved["mcpServers"], servers);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                file.as_file().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(file);
        assert!(!path.exists());
    }
    #[test]
    fn supervisor_admission_distinguishes_completion_from_stop_and_stale_run() {
        let run = RunId::new("run").unwrap();
        let other = RunId::new("other").unwrap();
        assert!(
            admit_prompt(&run, &run, true, false, false)
                .unwrap_err()
                .turn_completed
        );
        assert!(
            !admit_prompt(&run, &run, true, true, false)
                .unwrap_err()
                .turn_completed
        );
        assert!(
            !admit_prompt(&run, &run, false, false, true)
                .unwrap_err()
                .turn_completed
        );
        assert!(
            !admit_prompt(&run, &other, false, false, false)
                .unwrap_err()
                .turn_completed
        );
        assert!(admit_prompt(&run, &run, false, false, false).is_ok());
    }
    #[test]
    fn prompt_echo_releases_only_current_turn_and_keeps_process_echo_mode() {
        let mut gate = PromptGate::default();
        gate.begin("first".into());
        assert_eq!(
            gate.route(
                json!({"type":"stream_event","parent_tool_use_id":null,"user_message_uuid":"first"})
            )
            .len(),
            1
        );
        assert!(gate.mode == EchoMode::Early);
        gate.begin("second".into());
        assert!(gate.route(json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":"previous output"}})).is_empty());
        assert!(
            gate.route(json!({"type":"result","user_message_uuids":["first"],"num_turns":1}))
                .is_empty()
        );
        assert!(gate.held.is_empty());
        assert_eq!(gate.wake_frames.len(), 2);
        assert_eq!(gate.wake_frames[0]["message"]["content"], "previous output");
        gate.wake_frames.clear();
        assert!(gate.route(json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":"current output"}})).is_empty());
        let released = gate.route(
            json!({"type":"stream_event","parent_tool_use_id":null,"user_message_uuid":"second"}),
        );
        assert_eq!(released.len(), 2);
        assert_eq!(released[0]["message"]["content"], "current output");
        assert!(
            gate.route(json!({"type":"result","user_message_uuid":"first"}))
                .is_empty()
        );
        assert!(
            gate.route(json!({"type":"result","origin":{"kind":"task_notification"}}))
                .is_empty()
        );
        assert_eq!(
            gate.route(json!({"type":"result","user_message_uuid":"second"}))
                .len(),
            1
        );
    }
    #[test]
    fn result_only_and_legacy_clis_stream_without_waiting_but_foreign_results_do_not_finish() {
        let mut gate = PromptGate::default();
        gate.begin("prompt".into());
        assert_eq!(
            gate.route(json!({"type":"assistant","parent_tool_use_id":null}))
                .len(),
            1
        );
        assert!(
            gate.route(json!({"type":"result","user_message_uuid":"other"}))
                .is_empty()
        );
        assert_eq!(
            gate.route(json!({"type":"result","user_message_uuid":"prompt"}))
                .len(),
            1
        );
        assert!(gate.mode == EchoMode::ResultOnly);
        gate.begin("next".into());
        assert_eq!(
            gate.route(json!({"type":"assistant","parent_tool_use_id":null}))
                .len(),
            1
        );
        assert!(
            gate.route(json!({"type":"result","origin":{"kind":"task_notification"}}))
                .is_empty()
        );
        assert_eq!(
            gate.route(json!({"type":"result","subtype":"success"}))
                .len(),
            1
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_initialization_releases_the_process_and_does_not_terminalize_resume_fallback() {
        for reject in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (output, mut batches) = mpsc::channel(8);
            let adapter = ClaudeAdapter::new(
                ClaudeConfig {
                    program: "/bin/sh".into(),
                    config_home: directory.path().into(),
                },
                output.clone(),
            );
            let state = crate::normalize::tests::state(Driver::Claude);
            let provider = state.provider_thread.id.clone();
            let model = state.run.model_selection.clone();
            let mut command = tokio::process::Command::new("/bin/sh");
            command.arg("-c").arg(if reject {
                "read -r initialize; printf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"request_id\":\"initialize\",\"subtype\":\"error\"}}'; cat >/dev/null"
            } else { "cat >/dev/null" }).stdin(Stdio::piped()).stdout(Stdio::piped());
            let permit = adapter.capacity.clone().try_acquire_owned().unwrap();
            let handle = spawn(
                command,
                None,
                state,
                uuid::Uuid::new_v4().to_string(),
                permit,
                output,
                directory.path(),
                directory.path(),
                RuntimeMode::FullAccess,
                InteractionMode::Default,
                model,
            )
            .unwrap();
            adapter
                .processes
                .lock()
                .await
                .insert(provider, handle.clone());
            let error = adapter
                .initialized(&handle, std::time::Duration::from_millis(100))
                .await
                .unwrap_err();
            assert!(
                error
                    .message
                    .contains(if reject { "rejected" } else { "timed out" })
            );
            assert!(adapter.processes.lock().await.is_empty());
            assert_eq!(adapter.capacity.available_permits(), 8);
            assert!(batches.try_recv().is_err());
        }
    }
    #[test]
    fn plan_mode_overrides_runtime_permissions() {
        for mode in [
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ] {
            assert_eq!(permission_mode(mode, InteractionMode::Plan), "plan");
        }
        assert_eq!(
            permission_mode(RuntimeMode::ApprovalRequired, InteractionMode::Default),
            "default"
        );
        assert_eq!(
            permission_mode(RuntimeMode::AutoAcceptEdits, InteractionMode::Default),
            "acceptEdits"
        );
        assert_eq!(
            permission_mode(RuntimeMode::Auto, InteractionMode::Default),
            "auto"
        );
        assert_eq!(
            permission_mode(RuntimeMode::FullAccess, InteractionMode::Default),
            "bypassPermissions"
        );
    }
}
