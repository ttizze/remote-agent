use crate::{
    ProviderBatch,
    capabilities::capabilities,
    error,
    normalize::{self, TurnState},
    now,
};
use agent_transport::peer::{JsonlReader, JsonlWriter};
use orchestration::{worker::AdapterError, *};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, Semaphore, mpsc, watch};

pub struct ClaudeConfig {
    pub program: PathBuf,
    pub config_home: PathBuf,
    pub credentials_home: PathBuf,
}
struct ProcessHandle {
    native_session: String,
    state: Arc<Mutex<TurnState>>,
    input: mpsc::Sender<Value>,
    stop: watch::Sender<bool>,
    ready: watch::Receiver<Option<Result<(), String>>>,
    done: watch::Receiver<bool>,
    changed: Arc<Notify>,
    cwd: PathBuf,
    runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: ModelSelection,
}
pub struct ClaudeAdapter {
    config: ClaudeConfig,
    processes: tokio::sync::Mutex<BTreeMap<ProviderThreadId, Arc<ProcessHandle>>>,
    capacity: Arc<Semaphore>,
    output: mpsc::Sender<ProviderBatch>,
}
impl ClaudeAdapter {
    pub fn new(config: ClaudeConfig, output: mpsc::Sender<ProviderBatch>) -> Self {
        Self {
            config,
            processes: tokio::sync::Mutex::new(BTreeMap::new()),
            capacity: Arc::new(Semaphore::new(8)),
            output,
        }
    }
    pub async fn execute(
        &self,
        effect: &EffectBody,
        projection: &ThreadProjection,
        cwd: &Path,
        browser: Option<Value>,
    ) -> Result<(), AdapterError> {
        match effect {
            EffectBody::Start { run_id } => self.start(projection, run_id, cwd, browser).await,
            EffectBody::Steer {
                run_id, message_id, ..
            } => {
                let handle = self.for_run(projection, run_id).await?;
                let message = projection
                    .messages
                    .iter()
                    .find(|message| message.id == *message_id)
                    .ok_or_else(|| error("steer input missing"))?;
                handle
                    .input
                    .send(user_frame(&handle.native_session, message, true))
                    .await
                    .map_err(error)
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
                handle.input.send(json!({"type":"control_request","request_id":"interrupt","request":{"subtype":"interrupt"}})).await.map_err(error)?;
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
            } => {
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
                let response = normalize::claude_response(&request, *decision, answers.as_ref());
                handle.input.send(json!({"type":"control_response","response":{"subtype":"success","request_id":request.id,"response":response}})).await.map_err(error)
            }
            EffectBody::Detach {
                provider_session_id,
            } => {
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
    async fn start(
        &self,
        projection: &ThreadProjection,
        run_id: &RunId,
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
        let existing = self
            .processes
            .lock()
            .await
            .get(&provider_thread.id)
            .cloned();
        let reusable = existing.as_ref().is_some_and(|handle| {
            handle.cwd == cwd
                && handle.model == run.model_selection
                && handle.runtime_mode == projection.thread.runtime_mode
                && handle.interaction_mode == projection.thread.interaction_mode
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
        let resume = provider_thread
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
        provider_thread.provider_session_id = Some(session.id.clone());
        provider_thread.native_thread_ref = Some(ProviderRef {
            driver: Driver::Claude,
            native_id: Some(native.clone()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
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
                    })?,
            )
        };
        let mut initial = state.batch(state.initial_payloads(), &timestamp);
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        initial.acknowledged = Some(acknowledged);
        self.output.send(initial).await.map_err(error)?;
        if !receipt.await.map_err(error)? {
            return Ok(());
        }
        let handle = if reusable {
            let handle = existing.expect("reusable process exists");
            *handle.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
            handle
        } else {
            let command = process_command(
                &self.config,
                cwd,
                &native,
                resume,
                &run.model_selection,
                projection.thread.runtime_mode,
                projection.thread.interaction_mode,
                browser,
            )?;
            spawn(
                command,
                state,
                native,
                permit.expect("new process has reserved capacity"),
                self.output.clone(),
                cwd,
                projection.thread.runtime_mode,
                projection.thread.interaction_mode,
                run.model_selection.clone(),
            )?
        };
        self.processes
            .lock()
            .await
            .insert(provider_thread_id, handle.clone());
        let mut ready = handle.ready.clone();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(result) = ready.borrow().clone() {
                    return result.map_err(error);
                }
                ready.changed().await.map_err(error)?;
            }
        })
        .await
        .map_err(|_| error("Claude initialization timed out"))??;
        let message = projection
            .messages
            .iter()
            .find(|message| message.id == run.user_message_id)
            .ok_or_else(|| error("run input missing"))?;
        handle
            .input
            .send(user_frame(&handle.native_session, message, false))
            .await
            .map_err(error)
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
                    handle
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .terminal
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
    cwd: &Path,
    native: &str,
    resume: bool,
    model: &ModelSelection,
    runtime: RuntimeMode,
    interaction: InteractionMode,
    browser: Option<Value>,
) -> Result<tokio::process::Command, AdapterError> {
    let mut command = bex_process::command(&config.program).map_err(error)?;
    command
        .env("CLAUDE_CONFIG_DIR", &config.config_home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &config.credentials_home)
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
    if let Some(browser) = browser {
        command
            .arg("--mcp-config")
            .arg(json!({"mcpServers":{"bex_browser":browser}}).to_string());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(command)
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
    state: TurnState,
    native: String,
    permit: tokio::sync::OwnedSemaphorePermit,
    output: mpsc::Sender<ProviderBatch>,
    cwd: &Path,
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
    let (input_sender, mut commands) = mpsc::channel::<Value>(256);
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
        runtime_mode,
        interaction_mode,
        model,
    });
    tokio::spawn(async move {
        let _permit = permit;
        let mut writer = JsonlWriter::new(input);
        let mut reader = JsonlReader::new(stdout);
        let result:Result<(),AdapterError>=async{
            writer.write_line(&json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize"}}).to_string()).await.map_err(error)?;
            loop{tokio::select!{
                stopped=shutdown.changed()=>{if stopped.is_err()||*shutdown.borrow(){break;}}
                command=commands.recv()=>{let Some(command)=command else{break;};writer.write_line(&command.to_string()).await.map_err(error)?;}
                line=reader.read_line()=>{let Some(line)=line.map_err(error)?else{break;};let frame:Value=serde_json::from_str(&line).map_err(error)?;
                    if frame["type"]=="control_response"&&frame["response"]["request_id"]=="initialize" {ready.send_replace(Some(if frame["response"]["subtype"]=="success"{Ok(())}else{Err("Claude initialization rejected".into())}));continue;}
                    let timestamp=now();let (batch,responses)={let mut state=state.lock().unwrap_or_else(|e|e.into_inner());let translated=normalize::claude(&state,&frame,&timestamp);*state=translated.state;let batch=state.batch(translated.payloads,&timestamp);(batch,translated.immediate_responses)};
                    if !batch.events.is_empty(){output.send(batch).await.map_err(error)?;}
                    for (id,response) in responses{writer.write_line(&json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}}).to_string()).await.map_err(error)?;}
                    changed.notify_waiters();
                }
            }}Ok(())
        }.await;
        let timestamp = now();
        let message = result
            .as_ref()
            .err()
            .map(|error| error.message.as_str())
            .unwrap_or("Claude process closed");
        let batch = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let translated = normalize::disconnected(&state, message, &timestamp);
            *state = translated.state;
            state.batch(translated.payloads, &timestamp)
        };
        if !batch.events.is_empty() {
            let _ = output.send(batch).await;
        }
        if ready.borrow().is_none() {
            ready.send_replace(Some(Err("Claude exited before initialization".into())));
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

#[cfg(test)]
mod tests {
    use super::*;
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
