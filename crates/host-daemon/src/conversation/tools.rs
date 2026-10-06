//! Thread-scoped MCP tools for provider sessions (T3 `OrchestratorMcpService`).
//! Tools send the same commands as clients and read the committed thread state.
use agent_domain::{
    Command, CommandId, CompletionWake, DeliveryIntent, DispatchMode, Driver, InteractionMode,
    ItemKind, ItemStatus, MessageAuthor, MessageId, ModelSelection, NodeId, Reply, RequestStatus,
    RunStatus, RuntimeMode, SendMessage, State, Task, ThreadId, TransferKind,
    delegated_task_status,
};
use agent_protocol::{models::Model, provider::ProviderKind};
use agent_runtime::{Runtime, timeline_rows};
use agent_transport::peer::{JsonlReader, JsonlWriter};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_MESSAGE: usize = 6 * 1024 * 1024;
const TOKEN_ENV: &str = "AGENT_TOOLS_TOKEN";

/// The live model catalog the tools offer for delegation.
pub(crate) trait ModelCatalog: Send + Sync {
    fn models(&self) -> BoxFuture<'_, Result<Vec<Model>, String>>;
}

#[derive(Clone, PartialEq, Eq)]
struct ToolScope {
    thread: ThreadId,
    instance: String,
}

/// The loopback listener provider sessions reach through `agent-mcp`, and the
/// per-session tokens that scope each call to its thread.
pub(crate) struct ToolBridge {
    address: SocketAddr,
    listener: Mutex<Option<std::net::TcpListener>>,
    scopes: Arc<Mutex<HashMap<String, ToolScope>>>,
    stop: CancellationToken,
}
impl Drop for ToolBridge {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl ToolBridge {
    pub(crate) fn bind() -> Result<Self, String> {
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            address: listener.local_addr().map_err(|e| e.to_string())?,
            listener: Mutex::new(Some(listener)),
            scopes: Arc::default(),
            stop: CancellationToken::new(),
        })
    }

    /// Answers calls until the bridge is dropped.
    pub(crate) fn serve(&self, tools: Weak<AgentTools>) -> Result<(), String> {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or("the tool bridge is already serving")?;
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
        let (scopes, stop) = (self.scopes.clone(), self.stop.clone());
        tokio::spawn(async move {
            let capacity = Arc::new(tokio::sync::Semaphore::new(32));
            let mut calls = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    Some(_) = calls.join_next(), if !calls.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                        let (tools, scopes) = (tools.clone(), scopes.clone());
                        calls.spawn(async move {
                            let _permit = permit;
                            let (read, write) = socket.into_split();
                            let mut input = JsonlReader::with_max_message_bytes(read, 1024 * 1024);
                            let mut output = JsonlWriter::with_max_message_bytes(write, MAX_MESSAGE);
                            let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(5), input.read_line()).await else { return; };
                            let Ok(request) = serde_json::from_str::<BridgeRequest>(&line) else { return; };
                            let scope = scopes.lock().unwrap_or_else(|e| e.into_inner()).get(&request.token).cloned();
                            let result = match (scope, tools.upgrade()) {
                                (Some(scope), Some(tools)) => tokio::select! {
                                    result = tools.call(&scope.thread, &scope.instance, &request.invocation, &request.name, request.arguments) => result,
                                    _ = input.read_line() => return,
                                },
                                _ => Err("Invalid orchestration scope".into()),
                            };
                            if let Ok(line) = serde_json::to_string(&result) { let _ = output.write_line(&line).await; }
                        });
                    }
                }
            }
            calls.abort_all();
            while calls.join_next().await.is_some() {}
        });
        Ok(())
    }

    /// The MCP server entry for one provider session; stable for the session's scope.
    pub(crate) fn provider_config(
        &self,
        thread: &ThreadId,
        instance: &str,
    ) -> Result<Value, String> {
        let scope = ToolScope {
            thread: thread.clone(),
            instance: instance.to_owned(),
        };
        let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
        let token = scopes
            .iter()
            .find(|(_, current)| **current == scope)
            .map(|(token, _)| token.clone())
            .unwrap_or_else(|| {
                let token = uuid::Uuid::new_v4().to_string();
                scopes.insert(token.clone(), scope);
                token
            });
        Ok(json!({
            "command": std::env::current_exe().map_err(|e| e.to_string())?,
            "args": ["agent-mcp", "--address", self.address.to_string()],
            "env": {TOKEN_ENV: token},
        }))
    }

    /// The session ended; its token stops working.
    pub(crate) fn revoke(&self, thread: &ThreadId, instance: &str) {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, scope| !(scope.thread == *thread && scope.instance == instance));
    }

    #[cfg(test)]
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }
}

#[derive(serde::Serialize, Deserialize)]
struct BridgeRequest {
    token: String,
    invocation: String,
    name: String,
    arguments: Value,
}

fn schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
fn tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let timeout = json!({"type":"number","minimum":0,"maximum":3600000});
    vec![
        schema(
            "orchestrator_capabilities",
            "List live provider models and inherited settings for this conversation. Use models from this catalog when delegating.",
            json!({}),
            &[],
        ),
        schema(
            "delegate_task",
            "Delegate a self-contained task without parent history. Prefer native subagents when they support the selected model; use this for cross-provider or app-owned work. A new review round needs a new task and clientRequestId, stable across retries. Async completion wakes the parent; end the turn instead of polling. Wait timeout never cancels the child.",
            json!({"task":{"type":"string","minLength":1,"maxLength":120000},"title":{"type":"string","maxLength":512},"role":{"type":"string","enum":["implementation","research","review","design","test","general"]},"target":{"type":"object","properties":{"providerInstanceId":string,"driverKind":string,"model":string,"options":{"oneOf":[{"type":"object"},{"type":"array","items":{"type":"object","properties":{"id":string,"value":{"type":["string","boolean"]}},"required":["id","value"]}}]}},"additionalProperties":false},"mode":{"type":"string","enum":["async","wait"]},"timeoutMs":timeout,"clientRequestId":string,"runtimeMode":{"type":"string","enum":["inherit","approval-required","auto-accept-edits","auto","full-access"]},"interactionMode":{"type":"string","enum":["inherit","default","plan"]}}),
            &["task"],
        ),
        schema(
            "task_status",
            "Read the original delegated result; terminal results stay stable after later child turns. Reading a terminal result acknowledges automatic delivery. Nested work is not a completed task.",
            json!({"taskId":string}),
            &["taskId"],
        ),
        schema(
            "task_cancel",
            "Interrupt only the original delegated task and dispose its automatic delivery. A terminal task does not cancel later child-thread turns.",
            json!({"taskId":string,"reason":string,"clientRequestId":string}),
            &["taskId"],
        ),
        schema(
            "t3_thread_read",
            "Read a conversation in this project. Paginate with nextPosition and recover long text with nextTextOffset (UTF-16 units). Reading a complete terminal child result acknowledges delivery.",
            json!({"threadId":string,"itemId":string,"textOffset":{"type":"integer","minimum":0},"view":{"type":"string","enum":["messages","activity"]},"afterPosition":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100},"runLimit":{"type":"integer","minimum":1,"maximum":50},"maxCharsPerItem":{"type":"integer","minimum":1,"maximum":50000}}),
            &["threadId"],
        ),
        schema(
            "t3_thread_send",
            "Send to a conversation in this project. Auto starts idle, steers active, or queues while preparing. Child storage is not a new delegated review round; call delegate_task with a new brief instead.",
            json!({"threadId":string,"message":{"type":"string","minLength":1,"maxLength":120000},"mode":{"type":"string","enum":["auto","queue","steer","restart"]},"clientRequestId":string}),
            &["threadId", "message"],
        ),
        schema(
            "t3_thread_wait",
            "Wait for a durable terminal run. Timeout does not interrupt or acknowledge the delegated result.",
            json!({"threadId":string,"runId":string,"timeoutMs":timeout}),
            &["threadId"],
        ),
        schema(
            "t3_thread_interrupt",
            "Interrupt an active run in this project. Terminal runs return without another side effect.",
            json!({"threadId":string,"runId":string,"reason":string,"clientRequestId":string}),
            &["threadId"],
        ),
    ]
}
fn content(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    }
}

/// The `agent-mcp` stdio server a provider session starts; it forwards tool calls
/// to the Host's bridge with the session's token.
pub async fn serve(address: SocketAddr) -> Result<(), String> {
    if !address.ip().is_loopback() {
        return Err("Local orchestration address required".into());
    }
    let token = std::env::var(TOKEN_ENV).map_err(|_| "Missing orchestration scope")?;
    let mut input = JsonlReader::with_max_message_bytes(tokio::io::stdin(), 1024 * 1024);
    let mut output = JsonlWriter::with_max_message_bytes(tokio::io::stdout(), MAX_MESSAGE);
    let mut calls = tokio::task::JoinSet::<(Value, Value)>::new();
    let mut pending = HashMap::<String, tokio::task::AbortHandle>::new();
    loop {
        let (id, response) = tokio::select! {
            completed = calls.join_next(), if !calls.is_empty() => {
                let Some(Ok((id, response))) = completed else { continue; };
                pending.remove(&id.to_string()); (id, response)
            }
            line = input.read_line() => {
                let Some(line) = line.map_err(|e| e.to_string())? else { break; };
                let request: Value = serde_json::from_str(&line).map_err(|_| "Invalid MCP JSON")?;
                let Some(id) = request.get("id") else {
                    if request["method"] == "notifications/cancelled"
                        && let Some(call) = pending.remove(&request["params"]["requestId"].to_string()) { call.abort(); }
                    continue;
                };
                let response = match request["method"].as_str() {
                    Some("initialize") => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"orchestration","version":env!("CARGO_PKG_VERSION")}}),
                    Some("tools/list") => json!({"tools":tools()}),
                    Some("ping") => json!({}),
                    Some("tools/call") if pending.len() < 16 && !pending.contains_key(&id.to_string()) => {
                        let name = request["params"]["name"].as_str().unwrap_or_default().to_owned();
                        let arguments = request["params"]["arguments"].clone();
                        let token = token.clone(); let id = id.clone(); let key = id.to_string();
                        let call = calls.spawn(async move {
                            (id,content(bridge(address,BridgeRequest{token,invocation:uuid::Uuid::new_v4().to_string(),name,arguments}).await))
                        });
                        pending.insert(key,call); continue;
                    }
                    Some("tools/call") => content(Err("Tool capacity reached".into())),
                    _ => { output.write_line(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}).to_string()).await.map_err(|e| e.to_string())?; continue; }
                }; (id.clone(),response)
            }
        };
        output
            .write_line(&json!({"jsonrpc":"2.0","id":id,"result":response}).to_string())
            .await
            .map_err(|e| e.to_string())?;
    }
    calls.abort_all();
    while calls.join_next().await.is_some() {}
    Ok(())
}
async fn bridge(address: SocketAddr, request: BridgeRequest) -> Result<Value, String> {
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|e| e.to_string())?;
    let (read, write) = socket.into_split();
    let mut output = JsonlWriter::with_max_message_bytes(write, 1024 * 1024);
    output
        .write_line(&serde_json::to_string(&request).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;
    let line = JsonlReader::with_max_message_bytes(read, MAX_MESSAGE)
        .read_line()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Orchestration bridge closed")?;
    serde_json::from_str(&line).map_err(|_| "Invalid orchestration response".to_owned())?
}

fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
/// One command id per tool call, or per `clientRequestId` across retries.
fn key(scope: &ThreadId, invocation: &str, name: &str, input: &Value) -> Result<CommandId, String> {
    let client = match input.get("clientRequestId") {
        Some(value) => value
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 256)
            .ok_or("Invalid clientRequestId")?,
        None => invocation,
    };
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for part in [scope.as_str(), name, client] {
        digest.update(&(part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    CommandId::new(format!(
        "mcp:{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            digest.finish().as_ref()
        )
    ))
    .map_err(|e| e.to_string())
}
fn unique(kind: &str, scope: &str) -> CommandId {
    CommandId::new(format!("mcp:{kind}:{scope}:{}", uuid::Uuid::new_v4())).expect("derived id")
}
fn budget(input: &Value) -> Result<Duration, String> {
    let ms = input
        .get("timeoutMs")
        .map_or(Some(600_000.0), Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 3_600_000.0)
        .ok_or("Invalid timeoutMs")?;
    Ok(Duration::from_secs_f64(ms / 1000.0))
}
fn task_status(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Pending => "queued",
        ItemStatus::Waiting => "waiting",
        ItemStatus::Completed => "completed",
        ItemStatus::Failed => "failed",
        ItemStatus::Cancelled => "cancelled",
        ItemStatus::Interrupted => "interrupted",
        ItemStatus::Running => "running",
    }
}
fn runtime_mode(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}
fn interaction_mode(mode: InteractionMode) -> &'static str {
    match mode {
        InteractionMode::Default => "default",
        InteractionMode::Plan => "plan",
    }
}
fn utf16_slice(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let mut units = 0;
    let mut end = offset;
    let mut result = String::new();
    let mut more = false;
    for ch in text.chars() {
        let start = units;
        units += ch.len_utf16();
        if units <= offset {
            continue;
        }
        if start < offset {
            end = units;
            continue;
        }
        if !result.is_empty() && end.saturating_sub(offset) + ch.len_utf16() > limit {
            more = true;
            break;
        }
        result.push(ch);
        end = units;
    }
    (result, more.then_some(end))
}
fn driver(instance: &str) -> Option<Driver> {
    match instance {
        "codex" => Some(Driver::Codex),
        "claude" => Some(Driver::Claude),
        _ => None,
    }
}

/// Upgrades an abandoned wait to an automatic completion wake.
struct WakeOnDrop {
    runtime: Arc<Runtime>,
    thread: ThreadId,
    task: Option<NodeId>,
}
impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let (runtime, thread) = (self.runtime.clone(), self.thread.clone());
            tokio::spawn(async move {
                let id = unique("wake", task.as_str());
                let _ = runtime
                    .dispatch(
                        thread,
                        id,
                        Command::SetTaskWake {
                            task,
                            wake: CompletionWake::Always,
                        },
                    )
                    .await;
            });
        }
    }
}

pub(crate) struct AgentTools {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) models: Arc<dyn ModelCatalog>,
}

impl AgentTools {
    async fn state(&self, thread: &ThreadId) -> Result<Arc<State>, String> {
        Ok(self
            .runtime
            .state(thread)
            .await
            .map_err(|e| e.to_string())?
            .state)
    }
    async fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
    ) -> Result<Reply, String> {
        let committed = self
            .runtime
            .dispatch(thread.clone(), id, command)
            .await
            .map_err(|e| e.to_string())?;
        match committed.reply {
            Reply::Rejected { reason } => Err(reason),
            reply => Ok(reply),
        }
    }
    /// A live thread of the caller's project.
    async fn scoped(&self, parent: &State, input: &Value) -> Result<Arc<State>, String> {
        let id = ThreadId::new(text(input, "threadId")?).map_err(|e| e.to_string())?;
        let target = self.state(&id).await?;
        let project = parent.thread.as_ref().map(|thread| &thread.project);
        match &target.thread {
            Some(thread) if thread.deleted_at.is_none() && Some(&thread.project) == project => {
                Ok(target)
            }
            _ => Err("Thread is outside this project".into()),
        }
    }
    async fn acknowledge(&self, parent: &ThreadId, task: &Task) -> Result<(), String> {
        if task.status.terminal() {
            self.dispatch(
                parent,
                unique("ack", task.id.as_str()),
                Command::AcknowledgeTask {
                    task: task.id.clone(),
                },
            )
            .await?;
        }
        Ok(())
    }
    async fn task_result(
        &self,
        parent: &State,
        id: &NodeId,
        timed_out: bool,
    ) -> Result<Value, String> {
        let task = parent
            .tasks
            .iter()
            .find(|task| task.id == *id && task.app_owned())
            .ok_or("Unknown delegated task")?;
        let child = self.state(&task.child_thread).await?;
        let status = delegated_task_status(
            task,
            &child.runs,
            &child.items,
            &parent.transfers,
            &child.messages,
        );
        let original_settled = status
            .child_run_id
            .as_ref()
            .and_then(|id| child.runs.iter().find(|run| run.id == *id))
            .is_some_and(|run| !run.status.blocking());
        let nested = child.tasks.iter().any(|task| !task.status.terminal());
        let work_state = if task.status.terminal() {
            "result_available"
        } else if nested && original_settled {
            "waiting_for_children"
        } else {
            "working"
        };
        Ok(json!({
            "taskId": task.id,
            "childThreadId": task.child_thread,
            "childRunId": status.child_run_id,
            "status": task_status(task.status),
            "workState": work_state,
            "hasPendingChildRuns": status.has_pending_child_runs,
            "latestTerminalRunId": status.latest_terminal_run_id,
            "latestTerminalStatus": status.latest_terminal_status,
            "latestTerminalSummary": status.latest_terminal_summary,
            "latestTerminalResultContextTransferId": status.latest_terminal_result_context_transfer_id,
            "providerInstanceId": status.provider_instance_id,
            "model": task.model,
            "summary": status.summary,
            "resultContextTransferId": status.result_context_transfer_id,
            "waitTimedOut": timed_out,
        }))
    }
    async fn live_models(&self) -> Result<Vec<Model>, String> {
        self.models.models().await
    }

    pub(crate) async fn call(
        &self,
        thread: &ThreadId,
        caller_instance: &str,
        invocation: &str,
        name: &str,
        input: Value,
    ) -> Result<Value, String> {
        let parent = self.state(thread).await?;
        let Some(current) = parent
            .thread
            .as_ref()
            .filter(|t| t.deleted_at.is_none() && t.archived_at.is_none())
        else {
            return Err("Calling thread is unavailable".into());
        };
        match name {
            "orchestrator_capabilities" => {
                let models = self.live_models().await?;
                let mut providers = vec![];
                for kind in [ProviderKind::Codex, ProviderKind::Claude] {
                    let models: Vec<_> = models
                        .iter()
                        .filter(|m| m.model.provider == kind)
                        .map(|m| json!({"id":m.id,"label":m.display_name,"options":model_options(m)}))
                        .collect();
                    providers.push(json!({"providerInstanceId":provider_id(kind),"driverKind":provider_id(kind),"displayName":provider_id(kind),"canRunChildTask":!models.is_empty(),"canRunCrossProviderChildTask":!models.is_empty(),"models":models,"constraints":[]}));
                }
                Ok(json!({
                    "parentThreadId": thread,
                    "inheritedProviderInstanceId": current.selection.instance,
                    "inheritedModel": current.selection.model,
                    "runtimeMode": runtime_mode(current.runtime_mode),
                    "interactionMode": interaction_mode(current.interaction_mode),
                    "providers": providers,
                    "features": {"appOwnedSubagents":true,"asyncPolling":true,"cancellation":true,"batchThreadCreation":false,"threadManagement":true,"incrementalThreadRead":true,"scheduledTasks":false,"maxBatchThreads":0},
                }))
            }
            "delegate_task" => {
                let timeout = budget(&input)?;
                let wait = match input["mode"].as_str() {
                    None | Some("async") => false,
                    Some("wait") => true,
                    _ => return Err("Invalid delegation mode".into()),
                };
                let command_id = key(thread, invocation, name, &input)?;
                // A replay may happen after the original parent turn has completed.
                let task_id =
                    NodeId::new(format!("node:delegated:{command_id}")).expect("derived id");
                if !parent.tasks.iter().any(|task| task.id == task_id) {
                    // Children run with the parent's modes; an override may only narrow them.
                    child_modes(current.runtime_mode, current.interaction_mode, &input)?;
                    let run = parent
                        .active_run()
                        .ok_or("Delegation requires an active parent run")?;
                    if run.selection.instance != caller_instance {
                        return Err("Delegation caller does not own the active parent run".into());
                    }
                    let models = self.live_models().await?;
                    let selection = child_model(&current.selection, &input["target"], &models)?;
                    let task = text(&input, "task")?.trim().to_owned();
                    let task = match input["role"].as_str() {
                        None | Some("general") => task,
                        Some(
                            role @ ("implementation" | "research" | "review" | "design" | "test"),
                        ) => {
                            format!("Act as the {role} sub-agent for this task.\n\n{task}")
                        }
                        _ => return Err("Invalid task role".into()),
                    };
                    self.dispatch(
                        thread,
                        command_id.clone(),
                        Command::Delegate {
                            task: task_id.clone(),
                            child: ThreadId::new(format!("thread:delegated:{command_id}"))
                                .expect("derived id"),
                            prompt: task,
                            selection,
                            wake: if wait {
                                CompletionWake::SettledOnly
                            } else {
                                CompletionWake::Always
                            },
                        },
                    )
                    .await?;
                }
                if !wait {
                    return self
                        .task_result(&*self.state(thread).await?, &task_id, false)
                        .await;
                }
                let mut wake = WakeOnDrop {
                    runtime: self.runtime.clone(),
                    thread: thread.clone(),
                    task: Some(task_id.clone()),
                };
                let deadline = tokio::time::Instant::now() + timeout;
                loop {
                    let parent = self.state(thread).await?;
                    let result = self.task_result(&parent, &task_id, false).await?;
                    if result["workState"] == "result_available" {
                        let task = parent
                            .tasks
                            .iter()
                            .find(|task| task.id == task_id)
                            .expect("known task");
                        self.acknowledge(thread, task).await?;
                        wake.task = None;
                        return Ok(result);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return self.task_result(&parent, &task_id, true).await;
                    }
                    tokio::time::sleep_until(
                        (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
                    )
                    .await;
                }
            }
            "task_status" | "task_cancel" => {
                let id = NodeId::new(text(&input, "taskId")?).map_err(|e| e.to_string())?;
                let task = parent
                    .tasks
                    .iter()
                    .find(|task| task.id == id && task.app_owned())
                    .ok_or("Unknown delegated task")?;
                if name == "task_status" {
                    let result = self.task_result(&parent, &id, false).await?;
                    self.acknowledge(thread, task).await?;
                    return Ok(result);
                }
                // Disposing delivery reconciles interruption of the original child run.
                self.dispatch(
                    thread,
                    key(thread, invocation, name, &input)?,
                    Command::DisposeTask { task: id.clone() },
                )
                .await?;
                Ok(
                    json!({"taskId":id,"status":if task.status.terminal(){task_status(task.status)}else{"cancel_requested"}}),
                )
            }
            "t3_thread_read" => self.read_thread(thread, &parent, &input).await,
            "t3_thread_send" => {
                let target = self.scoped(&parent, &input).await?;
                let target_thread = target.thread.as_ref().expect("scoped thread");
                child_modes(
                    current.runtime_mode,
                    current.interaction_mode,
                    &json!({"runtimeMode":runtime_mode(target_thread.runtime_mode),"interactionMode":interaction_mode(target_thread.interaction_mode)}),
                )?;
                let id = key(thread, invocation, name, &input)?;
                let message = MessageId::new(format!("message:{id}")).expect("derived id");
                let (mode, intent) = match input["mode"].as_str() {
                    None | Some("auto") => {
                        (DispatchMode::QueueAfterActive, Some(DeliveryIntent::Auto))
                    }
                    Some("queue") => (DispatchMode::QueueAfterActive, None),
                    Some("steer") => (
                        DispatchMode::SteerActive {
                            run: target.active_run().ok_or("No active run")?.id.clone(),
                        },
                        None,
                    ),
                    Some("restart") => (
                        DispatchMode::QueueAfterActive,
                        Some(DeliveryIntent::Restart),
                    ),
                    _ => return Err("Invalid send mode".into()),
                };
                self.dispatch(
                    &target_thread.id,
                    id,
                    Command::Send(SendMessage {
                        created_by: MessageAuthor::Agent,
                        creation_source: "mcp".into(),
                        id: message.clone(),
                        text: text(&input, "message")?.trim().to_owned(),
                        attachments: vec![],
                        selection: None,
                        mode,
                        intent,
                        source_plan: None,
                        title_seed: None,
                    }),
                )
                .await?;
                let target = self.state(&target_thread.id).await?;
                let sent = target
                    .messages
                    .iter()
                    .find(|m| m.id == message)
                    .ok_or("Message missing")?;
                let run = target
                    .runs
                    .iter()
                    .find(|r| Some(&r.id) == sent.run.as_ref())
                    .ok_or("Run missing")?;
                let delivery = if intent == Some(DeliveryIntent::Restart) {
                    "restarted"
                } else if sent.id != run.message {
                    "steered"
                } else if run.status == RunStatus::Queued {
                    "queued"
                } else {
                    "started"
                };
                Ok(
                    json!({"threadId":target_thread.id,"messageId":message,"runId":run.id,"status":run.status,"delivery":delivery}),
                )
            }
            "t3_thread_wait" | "t3_thread_interrupt" => {
                let target = self.scoped(&parent, &input).await?;
                let target_id = target.thread.as_ref().expect("scoped thread").id.clone();
                let run = match input["runId"].as_str() {
                    Some(id) => Some(
                        target
                            .runs
                            .iter()
                            .find(|r| r.id.as_str() == id)
                            .ok_or("Unknown run")?,
                    ),
                    None => target.runs.iter().max_by_key(|r| r.ordinal),
                };
                let id = run.map(|r| r.id.clone());
                if name == "t3_thread_interrupt" {
                    let active = run.is_some_and(|r| r.status.blocking());
                    if active {
                        self.dispatch(
                            &target_id,
                            key(thread, invocation, name, &input)?,
                            Command::Interrupt {
                                run: id.clone().expect("active run"),
                                hold_queue: true,
                            },
                        )
                        .await?;
                    }
                    return Ok(
                        json!({"threadId":target_id,"runId":id,"status":if active{json!("interrupt_requested")}else{run.map(|r|json!(r.status)).unwrap_or(json!("no_active_run"))}}),
                    );
                }
                let deadline = tokio::time::Instant::now() + budget(&input)?;
                loop {
                    let state = self.state(&target_id).await?;
                    let run = state.runs.iter().find(|r| Some(&r.id) == id.as_ref());
                    let timed_out = tokio::time::Instant::now() >= deadline;
                    let pending = |r: &agent_domain::Run| {
                        r.status.blocking() || r.status == RunStatus::Queued
                    };
                    if run.is_none_or(|r| !pending(r)) || timed_out {
                        return Ok(
                            json!({"threadId":target_id,"runId":id,"status":run.map(|r|json!(r.status)).unwrap_or(json!("idle")),"timedOut":timed_out&&run.is_some_and(pending)}),
                        );
                    }
                    tokio::time::sleep_until(
                        (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
                    )
                    .await;
                }
            }
            _ => Err("Unknown orchestration tool".into()),
        }
    }

    async fn read_thread(
        &self,
        caller: &ThreadId,
        parent: &State,
        input: &Value,
    ) -> Result<Value, String> {
        let target = self.scoped(parent, input).await?;
        let thread = target.thread.as_ref().expect("scoped thread");
        let number = |key: &str, default: usize, max: usize| -> Result<usize, String> {
            input.get(key).map_or(Ok(default), |v| {
                v.as_u64()
                    .filter(|n| *n > 0 && *n <= max as u64)
                    .map(|n| n as usize)
                    .ok_or_else(|| format!("Invalid {key}"))
            })
        };
        let limit = number("limit", 30, 100)?;
        let run_limit = number("runLimit", 10, 50)?;
        let max = number("maxCharsPerItem", 4000, 50000)?;
        let after = input
            .get("afterPosition")
            .map(|v| v.as_u64().ok_or("Invalid afterPosition"))
            .transpose()?;
        let offset = input["textOffset"].as_u64().unwrap_or(0) as usize;
        let selected = input["itemId"].as_str();
        let activity = match input["view"].as_str() {
            None | Some("messages") => false,
            Some("activity") => true,
            _ => return Err("Invalid view".into()),
        };
        let rows = timeline_rows(&target);
        let mut matching = rows.iter().filter(|row| {
            let position = row.position as u64;
            selected.map_or(after.is_none_or(|after| position > after), |id| {
                row.item.id.as_str() == id
            }) && (activity
                || matches!(
                    row.item.kind,
                    ItemKind::UserMessage { .. }
                        | ItemKind::AssistantMessage { .. }
                        | ItemKind::ProposedPlan { .. }
                ))
        });
        // The run whose result the parent received from this child.
        let result_run = parent
            .transfers
            .iter()
            .find(|t| t.kind == TransferKind::SubagentResult && t.source == thread.id)
            .and_then(|t| target.runs.iter().find(|run| run.ordinal == t.boundary))
            .map(|run| &run.id);
        let mut items = vec![];
        let mut acknowledge = false;
        let mut next = None;
        for row in matching.by_ref().take(limit) {
            let item = &row.item;
            let body = match &item.kind {
                ItemKind::Error { message, .. } => message.clone(),
                _ => row
                    .message
                    .as_ref()
                    .map(|m| m.text.clone())
                    .or_else(|| row.plan.as_ref().map(|p| p.markdown.clone()))
                    .unwrap_or_else(|| item.text.clone()),
            };
            let (text, next_offset) = utf16_slice(&body, offset, max);
            acknowledge |= item.run.as_ref() == result_run
                && next_offset.is_none()
                && matches!(
                    item.kind,
                    ItemKind::AssistantMessage { .. } | ItemKind::Error { .. }
                )
                && matches!(item.status, ItemStatus::Completed | ItemStatus::Failed);
            let kind = match serde_json::to_value(&item.kind).map_err(|e| e.to_string())? {
                Value::String(kind) => kind,
                Value::Object(map) => map.keys().next().cloned().unwrap_or_default(),
                _ => String::new(),
            };
            items.push(json!({
                "position": row.position,
                "visibility": if row.inherited { "inherited" } else { "local" },
                "sourceThreadId": row.source,
                "itemId": item.id,
                "runId": item.run,
                "messageId": row.message.as_ref().map(|m| &m.id),
                "createdBy": row.message.as_ref().map(|m| m.created_by),
                "creationSource": row.message.as_ref().map(|m| &m.creation_source),
                "type": kind,
                "status": item.status,
                "text": text,
                "textTruncated": next_offset.is_some(),
                "nextTextOffset": next_offset,
                "updatedAt": item.completed_at.as_ref().unwrap_or(&item.started_at),
            }));
            next = Some(row.position);
        }
        let more = matching.next().is_some();
        if acknowledge
            && offset == 0
            && let Some(task) = parent
                .tasks
                .iter()
                .find(|t| t.app_owned() && t.child_thread == thread.id && t.status.terminal())
        {
            self.acknowledge(caller, task).await?;
        }
        let recent: Vec<_> = target
            .runs
            .iter()
            .rev()
            .take(run_limit)
            .map(|r| json!({"runId":r.id,"ordinal":r.ordinal,"status":r.status,"providerInstanceId":r.selection.instance,"model":r.selection.model,"requestedAt":r.requested_at,"startedAt":r.started_at,"completedAt":r.completed_at}))
            .collect();
        let latest = target.runs.iter().max_by_key(|r| r.ordinal);
        let active = target.active_run();
        Ok(json!({
            "thread": {
                "threadId": thread.id,
                "projectId": thread.project,
                "title": thread.title,
                "status": latest.map(|r| json!(r.status)).unwrap_or(json!("idle")),
                "latestRunId": latest.map(|r| &r.id),
                "activeRunId": active.map(|r| &r.id),
                "providerInstanceId": thread.selection.instance,
                "model": thread.selection.model,
                "runtimeMode": runtime_mode(thread.runtime_mode),
                "interactionMode": interaction_mode(thread.interaction_mode),
                "linkedPullRequest": null,
                "branch": thread.workspace.as_ref().and_then(|w| w.branch.as_ref()),
                "worktreePath": thread.workspace.as_ref().and_then(|w| w.worktree_path.as_ref()),
                "parentThreadId": thread.parent,
                "runCount": target.runs.len(),
                "itemCount": rows.len(),
                "pendingRequestCount": target.requests.iter().filter(|r| r.status == RequestStatus::Pending).count(),
                "archived": thread.archived_at.is_some(),
                "settled": thread.settled_at.is_some(),
                "settledAt": thread.settled_at,
                "createdAt": thread.created_at,
                "updatedAt": thread.updated_at,
            },
            "recentRuns": recent,
            "items": items,
            "nextPosition": next,
            "hasMore": more,
        }))
    }
}

fn child_modes(
    parent: RuntimeMode,
    interaction: InteractionMode,
    input: &Value,
) -> Result<(RuntimeMode, InteractionMode), String> {
    let runtime = match input["runtimeMode"].as_str() {
        None | Some("inherit") => parent,
        Some("approval-required") => RuntimeMode::ApprovalRequired,
        Some("auto-accept-edits") => RuntimeMode::AutoAcceptEdits,
        Some("auto") => RuntimeMode::Auto,
        Some("full-access") => RuntimeMode::FullAccess,
        Some(_) => return Err("Invalid runtimeMode".into()),
    };
    let mode = match input["interactionMode"].as_str() {
        None | Some("inherit") => interaction,
        Some("default") => InteractionMode::Default,
        Some("plan") => InteractionMode::Plan,
        Some(_) => return Err("Invalid interactionMode".into()),
    };
    let rank = |r| match r {
        RuntimeMode::ApprovalRequired => 0,
        RuntimeMode::AutoAcceptEdits => 1,
        RuntimeMode::Auto => 2,
        RuntimeMode::FullAccess => 3,
    };
    if rank(runtime) > rank(parent)
        || (interaction == InteractionMode::Plan && mode == InteractionMode::Default)
    {
        return Err("Child settings cannot exceed parent permissions".into());
    }
    Ok((runtime, mode))
}
fn model_options(model: &Model) -> Vec<Value> {
    let mut options = vec![];
    let effort = if model.model.provider == ProviderKind::Claude {
        "effort"
    } else {
        "reasoningEffort"
    };
    if !model.supported_reasoning_efforts.is_empty() {
        options.push(json!({"id":effort,"type":"select","label":"Reasoning effort","options":model.supported_reasoning_efforts.iter().map(|e|json!({"id":e.reasoning_effort,"label":e.reasoning_effort,"isDefault":e.reasoning_effort==model.default_reasoning_effort})).collect::<Vec<_>>() }));
    }
    if let Some(tiers) = &model.service_tiers {
        options.push(json!({"id":"serviceTier","type":"select","label":"Service tier","options":tiers.iter().map(|t|json!({"id":t.id,"label":t.name.as_ref().unwrap_or(&t.id),"isDefault":model.default_service_tier.as_ref()==Some(&t.id)})).collect::<Vec<_>>()}));
    }
    options
}
fn child_model(
    parent: &ModelSelection,
    target: &Value,
    models: &[Model],
) -> Result<ModelSelection, String> {
    let instance = target["providerInstanceId"]
        .as_str()
        .or(target["driverKind"].as_str())
        .unwrap_or(parent.instance.as_str());
    let driver = driver(instance).ok_or("Unknown child provider")?;
    if target["driverKind"]
        .as_str()
        .is_some_and(|kind| kind != instance)
    {
        return Err("Provider instance and driver disagree".into());
    }
    let available: Vec<_> = models
        .iter()
        .filter(|m| {
            matches!(
                (driver, m.model.provider),
                (Driver::Codex, ProviderKind::Codex) | (Driver::Claude, ProviderKind::Claude)
            )
        })
        .collect();
    let model = target["model"]
        .as_str()
        .or_else(|| (instance == parent.instance).then_some(parent.model.as_str()))
        .or_else(|| {
            available
                .iter()
                .find(|m| m.is_default == Some(true))
                .map(|m| m.id.as_str())
        })
        .or_else(|| available.first().map(|m| m.id.as_str()))
        .ok_or("No models available for child provider")?;
    let descriptor = available
        .iter()
        .find(|m| m.id == model || m.model.id == model)
        .ok_or("Child model is not in the live catalog")?;
    let mut options = if instance == parent.instance && model == parent.model {
        parent.options.clone()
    } else {
        BTreeMap::new()
    };
    let option = |value: &Value| match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        _ => Err("Model option must be a string or boolean".to_owned()),
    };
    if let Some(value) = target.get("options") {
        options = if let Some(values) = value.as_object() {
            values
                .iter()
                .map(|(id, value)| Ok((id.clone(), option(value)?)))
                .collect::<Result<_, String>>()?
        } else if let Some(values) = value.as_array() {
            values
                .iter()
                .map(|v| {
                    Ok((
                        text(v, "id")?.to_owned(),
                        option(v.get("value").ok_or("Missing option value")?)?,
                    ))
                })
                .collect::<Result<_, String>>()?
        } else {
            return Err("Invalid model options".into());
        };
    }
    let descriptors = model_options(descriptor);
    for (id, value) in &options {
        if !descriptors.iter().any(|o| {
            o["id"] == *id
                && o["options"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|v| v["id"] == *value))
        }) {
            return Err(format!("Unsupported child model option: {id}"));
        }
    }
    Ok(ModelSelection {
        instance: instance.to_owned(),
        driver,
        model: model.into(),
        options,
    })
}

fn provider_id(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, provider: ProviderKind) -> Model {
        Model {
            id: id.into(),
            model: agent_protocol::models::ModelRef {
                provider,
                id: id.into(),
            },
            display_name: id.into(),
            default_reasoning_effort: "high".into(),
            supported_reasoning_efforts: vec![agent_protocol::models::ReasoningEffort {
                reasoning_effort: "high".into(),
            }],
            service_tiers: None,
            default_service_tier: None,
            is_default: Some(true),
        }
    }

    #[test]
    fn model_and_mode_overrides_use_the_live_catalog_without_escalation() {
        let parent = ModelSelection {
            instance: "codex".into(),
            driver: Driver::Codex,
            model: "a".into(),
            options: BTreeMap::from([("reasoningEffort".into(), "high".into())]),
        };
        let models = vec![
            model("a", ProviderKind::Codex),
            model("b", ProviderKind::Claude),
        ];
        assert_eq!(child_model(&parent, &Value::Null, &models).unwrap(), parent);
        assert!(child_model(&parent, &json!({"model":"invented"}), &models).is_err());
        let cross = child_model(&parent, &json!({"providerInstanceId":"claude"}), &models).unwrap();
        assert_eq!((cross.model.as_str(), cross.driver), ("b", Driver::Claude));
        assert!(cross.options.is_empty());
        assert!(
            child_model(
                &parent,
                &json!({"providerInstanceId":"claude","options":{"reasoningEffort":"high"}}),
                &models
            )
            .is_err()
        );
        assert!(
            child_modes(
                RuntimeMode::ApprovalRequired,
                InteractionMode::Default,
                &json!({"runtimeMode":"full-access"})
            )
            .is_err()
        );
        assert!(
            child_modes(
                RuntimeMode::FullAccess,
                InteractionMode::Plan,
                &json!({"interactionMode":"default"})
            )
            .is_err()
        );
        assert_eq!(
            child_modes(
                RuntimeMode::FullAccess,
                InteractionMode::Default,
                &json!({"runtimeMode":"approval-required","interactionMode":"plan"})
            )
            .unwrap(),
            (RuntimeMode::ApprovalRequired, InteractionMode::Plan)
        );
    }

    #[test]
    fn mutation_keys_are_scoped_and_utf16_chunks_always_advance() {
        let thread = ThreadId::new("parent").unwrap();
        let input = json!({"clientRequestId":"round-one"});
        assert_eq!(
            key(&thread, "a", "delegate_task", &input).unwrap(),
            key(&thread, "b", "delegate_task", &input).unwrap()
        );
        assert_ne!(
            key(&thread, "a", "delegate_task", &input).unwrap(),
            key(&thread, "a", "task_cancel", &input).unwrap()
        );
        assert_ne!(
            key(&thread, "a", "delegate_task", &input).unwrap(),
            key(
                &ThreadId::new("other").unwrap(),
                "a",
                "delegate_task",
                &input
            )
            .unwrap()
        );
        assert_eq!(utf16_slice("😀日ok", 0, 1), ("😀".into(), Some(2)));
        assert_eq!(utf16_slice("😀日ok", 2, 1), ("日".into(), Some(3)));
        assert_eq!(utf16_slice("😀日ok", 3, 20), ("ok".into(), None));
        assert!(budget(&json!({"timeoutMs":-1})).is_err());
    }

    #[tokio::test]
    async fn the_bridge_authenticates_its_scope_and_revoked_tokens_stop_working() {
        let listener = ToolBridge::bind().unwrap();
        let thread = ThreadId::new("parent").unwrap();
        let config = listener.provider_config(&thread, "codex").unwrap();
        assert_eq!(listener.provider_config(&thread, "codex").unwrap(), config);
        let token = config["env"][TOKEN_ENV].as_str().unwrap().to_owned();
        listener.serve(Weak::new()).unwrap();
        let request = |token: String| BridgeRequest {
            token,
            invocation: "read".into(),
            name: "t3_thread_read".into(),
            arguments: json!({"threadId":"parent"}),
        };
        assert_eq!(
            bridge(listener.address(), request("invalid".into())).await,
            Err("Invalid orchestration scope".into())
        );
        listener.revoke(&thread, "codex");
        assert_eq!(
            bridge(listener.address(), request(token)).await,
            Err("Invalid orchestration scope".into())
        );
    }
}
