//! Thread-scoped MCP bridge. Provider tools use the same durable commands as clients.
use super::*;
use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_MESSAGE: usize = 6 * 1024 * 1024;
const TOKEN_ENV: &str = "BEX_ORCHESTRATION_TOKEN";

#[derive(Clone, PartialEq, Eq)]
struct ToolScope {
    thread: ThreadId,
    instance: ProviderInstanceId,
}

pub(super) struct AgentTools {
    address: SocketAddr,
    scopes: Arc<Mutex<HashMap<String, ToolScope>>>,
    stop: CancellationToken,
}
impl Drop for AgentTools {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl AgentTools {
    pub(super) fn start(service: &HostRpcService) -> Result<Self, String> {
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
        let scopes = Arc::new(Mutex::new(HashMap::<String, ToolScope>::new()));
        let stop = CancellationToken::new();
        let weak = Arc::downgrade(&service.inner);
        let scope_map = scopes.clone();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            let capacity = Arc::new(tokio::sync::Semaphore::new(32));
            let mut calls = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    Some(_) = calls.join_next(), if !calls.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                        let weak = weak.clone(); let scopes = scope_map.clone();
                        calls.spawn(async move {
                            let _permit = permit;
                            let (read, write) = socket.into_split();
                            let mut input = JsonlReader::with_max_message_bytes(read, 1024 * 1024);
                            let mut output = JsonlWriter::with_max_message_bytes(write, MAX_MESSAGE);
                            let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(5), input.read_line()).await else { return; };
                            let Ok(request) = serde_json::from_str::<BridgeRequest>(&line) else { return; };
                            let thread = scopes.lock().unwrap_or_else(|e| e.into_inner()).get(&request.token).cloned();
                            let result = if let (Some(thread), Some(inner)) = (thread, weak.upgrade()) {
                                let service = HostRpcService {inner};
                                tokio::select! {
                                    result = service.agent_tool(&thread.thread, &thread.instance, &request.invocation, &request.name, request.arguments) => result,
                                    _ = input.read_line() => return,
                                }
                            } else { Err("Invalid orchestration scope".into()) };
                            if let Ok(line) = serde_json::to_string(&result) { let _ = output.write_line(&line).await; }
                        });
                    }
                }
            }
            calls.abort_all();
            while calls.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            scopes,
            stop,
        })
    }
    pub(super) fn provider_config(
        &self,
        thread: &ThreadId,
        instance: &ProviderInstanceId,
    ) -> Result<Value, String> {
        let scope = ToolScope {
            thread: thread.clone(),
            instance: instance.clone(),
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
        Ok(
            json!({"command":std::env::current_exe().map_err(|e| e.to_string())?,
            "args":["agent-mcp","--address",self.address.to_string()],"env":{TOKEN_ENV:token}}),
        )
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
                    Some("initialize") => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"bex-orchestration","version":env!("CARGO_PKG_VERSION")}}),
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
fn budget(input: &Value) -> Result<Duration, String> {
    let ms = input
        .get("timeoutMs")
        .map_or(Some(600_000.0), Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 3_600_000.0)
        .ok_or("Invalid timeoutMs")?;
    Ok(Duration::from_secs_f64(ms / 1000.0))
}
fn terminal(status: NodeStatus) -> bool {
    matches!(
        status,
        NodeStatus::Completed
            | NodeStatus::Failed
            | NodeStatus::Cancelled
            | NodeStatus::Interrupted
    )
}
fn task_status(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Pending | NodeStatus::Idle => "queued",
        NodeStatus::Waiting => "waiting",
        NodeStatus::Completed => "completed",
        NodeStatus::Failed => "failed",
        NodeStatus::Cancelled => "cancelled",
        NodeStatus::Interrupted => "interrupted",
        _ => "running",
    }
}
fn item_text(body: &TurnItemBody) -> Option<&str> {
    match body {
        TurnItemBody::UserMessage { text, .. }
        | TurnItemBody::AssistantMessage { text, .. }
        | TurnItemBody::Reasoning { text, .. }
        | TurnItemBody::ProposedPlan { markdown: text, .. } => Some(text),
        TurnItemBody::CommandExecution { output, .. } => output.as_deref(),
        TurnItemBody::Subagent { result, .. } => result.as_deref(),
        TurnItemBody::Error { failure, .. } => Some(&failure.message),
        _ => None,
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
struct WakeOnDrop {
    service: HostRpcService,
    thread: ThreadId,
    task: Option<NodeId>,
}
impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        if let Some(task_id) = self.task.take() {
            let _ = self.service.dispatch_command(&Command {
                command_id: CommandId::new(format!("mcp:wake:{}", uuid::Uuid::new_v4()))
                    .expect("derived id"),
                thread_id: self.thread.clone(),
                body: CommandBody::DelegatedTaskWakePolicy {
                    task_id,
                    completion_wake: CompletionWake::Always,
                },
            });
        }
    }
}

impl HostRpcService {
    fn agent_projection(&self, id: &ThreadId) -> Result<ThreadProjection, String> {
        self.inner.store.projection(id).map_err(|e| e.to_string())
    }
    fn scoped_projection(
        &self,
        scope: &ThreadProjection,
        input: &Value,
    ) -> Result<ThreadProjection, String> {
        let id = ThreadId::new(text(input, "threadId")?).map_err(|e| e.to_string())?;
        let target = self.agent_projection(&id)?;
        if target.thread.deleted_at.is_some() || target.thread.project_id != scope.thread.project_id
        {
            return Err("Thread is outside this project".into());
        }
        Ok(target)
    }
    fn agent_dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        body: CommandBody,
    ) -> Result<(), String> {
        self.dispatch_command(&Command {
            command_id: id,
            thread_id: thread.clone(),
            body,
        })
        .map(|_| ())
        .map_err(|e| e.message)
    }
    fn acknowledge_task(&self, parent: &ThreadProjection, task: &Subagent) -> Result<(), String> {
        if terminal(task.status) {
            self.agent_dispatch(
                &parent.thread.id,
                CommandId::new(format!("mcp:ack:{}:{}", task.id, uuid::Uuid::new_v4()))
                    .expect("derived id"),
                CommandBody::DelegatedTaskAcknowledge {
                    task_id: task.id.clone(),
                    observed_by_run_id: parent
                        .runs
                        .iter()
                        .filter(|r| r.status.is_blocking())
                        .max_by_key(|r| r.ordinal)
                        .map(|r| r.id.clone()),
                },
            )?;
        }
        Ok(())
    }
    fn task_result(
        &self,
        parent: &ThreadProjection,
        id: &NodeId,
        timed_out: bool,
    ) -> Result<Value, String> {
        let task = parent
            .subagents
            .iter()
            .find(|t| t.id == *id && t.origin == SubagentOrigin::AppOwned)
            .ok_or("Unknown delegated task")?;
        let child = self.agent_projection(
            task.child_thread_id
                .as_ref()
                .ok_or("Missing child thread")?,
        )?;
        let first = child.runs.iter().min_by_key(|r| r.ordinal);
        let latest = child
            .runs
            .iter()
            .filter(|r| {
                !r.status.is_blocking()
                    && r.status != RunStatus::Queued
                    && !child
                        .messages
                        .iter()
                        .any(|m| m.id == r.user_message_id && m.delegated_completion.is_some())
            })
            .max_by_key(|r| r.ordinal);
        let summary = latest
            .and_then(|run| {
                child.visible_turn_items.iter().rev().find_map(|i| {
                    (i.item.run_id.as_ref() == Some(&run.id))
                        .then(|| item_text(&i.item.body))
                        .flatten()
                })
            })
            .map(|s| s.chars().take(16000).collect::<String>());
        let transfer = parent.context_transfers.iter().find(|t| {
            t.kind == TransferKind::SubagentResult && t.source_thread_id == child.thread.id
        });
        let nested = child.subagents.iter().any(|t| !terminal(t.status));
        Ok(
            json!({"taskId":task.id,"childThreadId":child.thread.id,"childRunId":first.map(|r|&r.id),"childNodeId":first.and_then(|r|r.root_node_id.as_ref()),"status":task_status(task.status),"workState":if terminal(task.status){"result_available"}else if nested&&first.is_some_and(|r|!r.status.is_blocking()){ "waiting_for_children" }else{"working"},"hasPendingChildRuns":child.runs.iter().any(|r|r.status.is_blocking()||r.status==RunStatus::Queued),"latestTerminalRunId":latest.map(|r|&r.id),"latestTerminalStatus":latest.map(|r|r.status),"latestTerminalSummary":summary,"latestTerminalResultContextTransferId":transfer.filter(|t|latest.is_some_and(|r|t.source_point.run_id.as_ref()==Some(&r.id))).map(|t|&t.id),"providerInstanceId":task.provider_instance_id,"model":task.model,"summary":task.result,"resultContextTransferId":transfer.map(|t|&t.id),"waitTimedOut":timed_out}),
        )
    }
    async fn live_models(&self) -> Result<Vec<agent_protocol::models::Model>, String> {
        let mut cursor = None;
        let mut result = vec![];
        loop {
            let page = self
                .models(&op::ListModels {
                    limit: 100,
                    cursor: cursor.clone(),
                })
                .await
                .map_err(|e| e.message)?;
            result.extend(page.data);
            if page.next_cursor.is_none() {
                break;
            }
            if cursor == page.next_cursor {
                return Err("Model catalog repeated its cursor".into());
            }
            cursor = page.next_cursor;
        }
        Ok(result)
    }
    async fn agent_tool(
        &self,
        thread: &ThreadId,
        caller_instance: &ProviderInstanceId,
        invocation: &str,
        name: &str,
        input: Value,
    ) -> Result<Value, String> {
        let parent = self.agent_projection(thread)?;
        if parent.thread.deleted_at.is_some() || parent.thread.archived_at.is_some() {
            return Err("Calling thread is unavailable".into());
        }
        match name {
            "orchestrator_capabilities" => {
                let models = self.live_models().await?;
                let mut providers = vec![];
                for kind in [ProviderKind::Codex, ProviderKind::Claude] {
                    let models:Vec<_>=models.iter().filter(|m|m.model.provider==kind).map(|m|json!({"id":m.id,"label":m.display_name,"options":model_options(m)})).collect();
                    providers.push(json!({"providerInstanceId":provider_id(kind),"driverKind":provider_id(kind),"displayName":provider_id(kind),"canRunChildTask":!models.is_empty(),"canRunCrossProviderChildTask":!models.is_empty(),"models":models,"constraints":[]}));
                }
                Ok(
                    json!({"parentThreadId":thread,"inheritedProviderInstanceId":parent.thread.provider_instance_id,"inheritedModel":parent.thread.model_selection.model,"runtimeMode":parent.thread.runtime_mode,"interactionMode":parent.thread.interaction_mode,"providers":providers,"features":{"appOwnedSubagents":true,"asyncPolling":true,"cancellation":true,"batchThreadCreation":false,"threadManagement":true,"incrementalThreadRead":true,"scheduledTasks":false,"maxBatchThreads":0}}),
                )
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
                if !parent.subagents.iter().any(|t| t.id == task_id) {
                    let (runtime, interaction) = child_modes(
                        parent.thread.runtime_mode,
                        parent.thread.interaction_mode,
                        &input,
                    )?;
                    let models = self.live_models().await?;
                    let selection =
                        child_model(&parent.thread.model_selection, &input["target"], &models)?;
                    let task = text(&input, "task")?.trim().to_owned();
                    let task = match input["role"].as_str() {
                        None | Some("general") => task,
                        Some(
                            role @ ("implementation" | "research" | "review" | "design" | "test"),
                        ) => format!("Act as the {role} sub-agent for this task.\n\n{task}"),
                        _ => return Err("Invalid task role".into()),
                    };
                    let title = input
                        .get("title")
                        .map(|v| {
                            v.as_str()
                                .filter(|s| !s.trim().is_empty() && s.chars().count() <= 512)
                                .map(|s| s.trim().to_owned())
                                .ok_or("Invalid task title")
                        })
                        .transpose()?;

                    let run = parent
                        .runs
                        .iter()
                        .filter(|r| r.status.is_blocking())
                        .max_by_key(|r| r.ordinal)
                        .ok_or("Delegation requires an active parent run")?;
                    if run.provider_instance_id != *caller_instance {
                        return Err("Delegation caller does not own the active parent run".into());
                    }
                    self.agent_dispatch(
                        thread,
                        command_id,
                        CommandBody::DelegatedTaskRequest(Box::new(DelegatedTaskRequest {
                            parent_run_id: run.id.clone(),
                            parent_node_id: run
                                .root_node_id
                                .clone()
                                .ok_or("Missing parent root")?,
                            task,
                            title,
                            model_selection: selection,
                            runtime_mode: runtime,
                            interaction_mode: interaction,
                            completion_wake: if wait {
                                CompletionWake::SettledOnly
                            } else {
                                CompletionWake::Always
                            },
                            created_by: CreatedBy::Agent,
                            creation_source: CreationSource::Mcp,
                        })),
                    )?;
                }
                if !wait {
                    return self.task_result(&self.agent_projection(thread)?, &task_id, false);
                }
                let mut wake = WakeOnDrop {
                    service: self.clone(),
                    thread: thread.clone(),
                    task: Some(task_id.clone()),
                };
                let deadline = tokio::time::Instant::now() + timeout;
                loop {
                    let parent = self.agent_projection(thread)?;
                    let result = self.task_result(&parent, &task_id, false)?;
                    if result["workState"] == "result_available" {
                        self.acknowledge_task(
                            &parent,
                            parent
                                .subagents
                                .iter()
                                .find(|t| t.id == task_id)
                                .expect("known task"),
                        )?;
                        wake.task = None;
                        return Ok(result);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return self.task_result(&parent, &task_id, true);
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
                    .subagents
                    .iter()
                    .find(|t| t.id == id && t.origin == SubagentOrigin::AppOwned)
                    .ok_or("Unknown delegated task")?;
                if name == "task_status" {
                    let result = self.task_result(&parent, &id, false)?;
                    self.acknowledge_task(&parent, task)?;
                    return Ok(result);
                }
                // Disposing delivery reconciles interruption of the original child run.
                self.agent_dispatch(
                    thread,
                    key(thread, invocation, name, &input)?,
                    CommandBody::DelegatedTaskDispose {
                        task_id: id.clone(),
                    },
                )?;
                Ok(
                    json!({"taskId":id,"status":if terminal(task.status){task_status(task.status)}else{"cancel_requested"}}),
                )
            }
            "t3_thread_read" => self.read_agent_thread(&parent, &input),
            "t3_thread_send" => {
                let target = self.scoped_projection(&parent, &input)?;
                child_modes(
                    parent.thread.runtime_mode,
                    parent.thread.interaction_mode,
                    &json!({"runtimeMode":target.thread.runtime_mode,"interactionMode":target.thread.interaction_mode}),
                )?;
                let id = key(thread, invocation, name, &input)?;
                let message_id = MessageId::new(format!("message:{id}")).expect("derived id");
                let (mode, intent) = match input["mode"].as_str() {
                    None | Some("auto") => {
                        (DispatchMode::QueueAfterActive, Some(DeliveryIntent::Auto))
                    }
                    Some("queue") => (DispatchMode::QueueAfterActive, None),
                    Some("steer") => (
                        DispatchMode::SteerActive {
                            target_run_id: target
                                .runs
                                .iter()
                                .find(|r| r.status.is_blocking())
                                .ok_or("No active run")?
                                .id
                                .clone(),
                        },
                        None,
                    ),
                    Some("restart") => (
                        DispatchMode::QueueAfterActive,
                        Some(DeliveryIntent::Restart),
                    ),
                    _ => return Err("Invalid send mode".into()),
                };
                self.agent_dispatch(
                    &target.thread.id,
                    id,
                    CommandBody::MessageDispatch(MessageDispatch {
                        delegated_completion: None,
                        source_plan_ref: None,
                        created_by: CreatedBy::Agent,
                        creation_source: CreationSource::Mcp,
                        message_id: message_id.clone(),
                        text: text(&input, "message")?.trim().to_owned(),
                        context: None,
                        attachments: vec![],
                        model_selection: None,
                        delivery_intent: intent,
                        dispatch_mode: mode,
                    }),
                )?;
                let target = self.agent_projection(&target.thread.id)?;
                let message = target
                    .messages
                    .iter()
                    .find(|m| m.id == message_id)
                    .ok_or("Message missing")?;
                let run = target
                    .runs
                    .iter()
                    .find(|r| Some(&r.id) == message.run_id.as_ref())
                    .ok_or("Run missing")?;
                Ok(
                    json!({"threadId":target.thread.id,"messageId":message_id,"runId":run.id,"status":run.status,"delivery":if intent==Some(DeliveryIntent::Restart){"restarted"}else if message.id!=run.user_message_id{"steered"}else if run.status==RunStatus::Queued{"queued"}else{"started"}}),
                )
            }
            "t3_thread_wait" | "t3_thread_interrupt" => {
                let target = self.scoped_projection(&parent, &input)?;
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
                    let active = run.is_some_and(|r| r.status.is_blocking());
                    if active {
                        self.agent_dispatch(
                            &target.thread.id,
                            key(thread, invocation, name, &input)?,
                            CommandBody::RunInterrupt {
                                run_id: id.clone().expect("active run"),
                                reason: input["reason"].as_str().map(str::to_owned),
                                hold_queue: true,
                            },
                        )?;
                    }
                    return Ok(
                        json!({"threadId":target.thread.id,"runId":id,"status":if active{json!("interrupt_requested")}else{run.map(|r|json!(r.status)).unwrap_or(json!("no_active_run"))}}),
                    );
                }
                let deadline = tokio::time::Instant::now() + budget(&input)?;
                loop {
                    let projection = self.agent_projection(&target.thread.id)?;
                    let run = projection.runs.iter().find(|r| Some(&r.id) == id.as_ref());
                    let timed_out = tokio::time::Instant::now() >= deadline;
                    if run.is_none_or(|r| !r.status.is_blocking() && r.status != RunStatus::Queued)
                        || timed_out
                    {
                        return Ok(
                            json!({"threadId":target.thread.id,"runId":id,"status":run.map(|r|json!(r.status)).unwrap_or(json!("idle")),"timedOut":timed_out&&run.is_some_and(|r|r.status.is_blocking()||r.status==RunStatus::Queued)}),
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
    fn read_agent_thread(&self, parent: &ThreadProjection, input: &Value) -> Result<Value, String> {
        let target = self.scoped_projection(parent, input)?;
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
        let mut matching = target
            .visible_turn_items
            .iter()
            .map(|r| (r.position, r))
            .filter(|(position, r)| {
                selected.map_or(after.is_none_or(|after| *position > after), |id| {
                    r.item.id.as_str() == id
                }) && (activity
                    || matches!(
                        r.item.body,
                        TurnItemBody::UserMessage { .. }
                            | TurnItemBody::AssistantMessage { .. }
                            | TurnItemBody::ProposedPlan { .. }
                    ))
            });
        let mut items = vec![];
        let result_run = parent
            .context_transfers
            .iter()
            .find(|t| {
                t.kind == TransferKind::SubagentResult && t.source_thread_id == target.thread.id
            })
            .and_then(|t| t.source_point.run_id.as_ref());
        let mut acknowledge = false;
        let mut next = None;
        for (position, r) in matching.by_ref().take(limit) {
            let item = &r.item;
            let (text, next_offset) = item_text(&item.body)
                .map(|s| utf16_slice(s, offset, max))
                .unwrap_or_default();
            acknowledge |= item.run_id.as_ref() == result_run
                && next_offset.is_none()
                && matches!(
                    item.body,
                    TurnItemBody::AssistantMessage { .. } | TurnItemBody::Error { .. }
                )
                && matches!(item.status, ItemStatus::Completed | ItemStatus::Failed);
            let message_id = match &item.body {
                TurnItemBody::UserMessage { message_id, .. }
                | TurnItemBody::AssistantMessage { message_id, .. } => Some(message_id),
                _ => None,
            };
            let kind = serde_json::to_value(&item.body)
                .map_err(|e| e.to_string())?
                .as_object()
                .and_then(|m| m.keys().next())
                .cloned()
                .unwrap_or_default();
            items.push(json!({"position":position,"visibility":r.visibility,"sourceThreadId":r.source_thread_id,"itemId":item.id,"runId":item.run_id,"messageId":message_id,"createdBy":message_id.and_then(|id|target.messages.iter().find(|m|m.id==*id)).map(|m|m.created_by),"creationSource":message_id.and_then(|id|target.messages.iter().find(|m|m.id==*id)).map(|m|m.creation_source),"type":kind,"status":item.status,"title":item.title,"text":text,"textTruncated":next_offset.is_some(),"nextTextOffset":next_offset,"updatedAt":item.updated_at}));
            next = Some(position);
        }
        let more = matching.next().is_some();
        if acknowledge
            && offset == 0
            && let Some(task) = parent.subagents.iter().find(|t| {
                t.origin == SubagentOrigin::AppOwned
                    && t.child_thread_id.as_ref() == Some(&target.thread.id)
                    && terminal(t.status)
            })
        {
            self.acknowledge_task(parent, task)?;
        }
        let recent:Vec<_>=target.runs.iter().rev().take(run_limit).map(|r|json!({"runId":r.id,"ordinal":r.ordinal,"status":r.status,"providerInstanceId":r.provider_instance_id,"model":r.model_selection.model,"requestedAt":r.requested_at,"startedAt":r.started_at,"completedAt":r.completed_at})).collect();
        let latest = target.runs.iter().max_by_key(|r| r.ordinal);
        let active = target
            .runs
            .iter()
            .filter(|r| r.status.is_blocking())
            .max_by_key(|r| r.ordinal);
        Ok(
            json!({"thread":{"threadId":target.thread.id,"projectId":target.thread.project_id,"title":target.thread.title,"createdBy":target.thread.created_by,"creationSource":target.thread.creation_source,"status":latest.map(|r|json!(r.status)).unwrap_or(json!("idle")),"latestRunId":latest.map(|r|&r.id),"activeRunId":active.map(|r|&r.id),"providerInstanceId":target.thread.provider_instance_id,"model":target.thread.model_selection.model,"runtimeMode":target.thread.runtime_mode,"interactionMode":target.thread.interaction_mode,"linkedPullRequest":null,"branch":target.thread.branch,"worktreePath":target.thread.worktree_path,"parentThreadId":target.thread.lineage.parent_thread_id,"relationshipToParent":target.thread.lineage.relationship_to_parent,"runCount":target.runs.len(),"itemCount":target.visible_turn_items.len(),"pendingRequestCount":target.runtime_requests.iter().filter(|r|r.status==RequestStatus::Pending).count(),"archived":target.thread.archived_at.is_some(),"settled":target.thread.settled_at.is_some(),"settledAt":target.thread.settled_at,"createdAt":target.thread.created_at,"updatedAt":target.thread.updated_at},"recentRuns":recent,"items":items,"nextPosition":next,"hasMore":more}),
        )
    }
}

fn child_modes(
    parent: RuntimeMode,
    interaction: InteractionMode,
    input: &Value,
) -> Result<(RuntimeMode, InteractionMode), String> {
    let runtime = match input["runtimeMode"].as_str() {
        None | Some("inherit") => parent,
        Some(_) => serde_json::from_value(input["runtimeMode"].clone())
            .map_err(|_| "Invalid runtimeMode")?,
    };
    let mode = match input["interactionMode"].as_str() {
        None | Some("inherit") => interaction,
        Some(_) => serde_json::from_value(input["interactionMode"].clone())
            .map_err(|_| "Invalid interactionMode")?,
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
fn model_options(model: &agent_protocol::models::Model) -> Vec<Value> {
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
    models: &[agent_protocol::models::Model],
) -> Result<ModelSelection, String> {
    let instance = target["providerInstanceId"]
        .as_str()
        .or(target["driverKind"].as_str())
        .unwrap_or(parent.instance_id.as_str());
    let driver = orchestration::capabilities::driver(
        &ProviderInstanceId::new(instance).map_err(|e| e.to_string())?,
    )
    .ok_or("Unknown child provider")?;
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
        .or_else(|| (instance == parent.instance_id.as_str()).then_some(parent.model.as_str()))
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
    let mut options = if instance == parent.instance_id.as_str() && model == parent.model {
        parent.options.clone()
    } else {
        BTreeMap::new()
    };
    if let Some(value) = target.get("options") {
        options = if let Some(values) = value.as_object() {
            values
                .iter()
                .map(|(id, value)| (id.clone(), Json(value.clone())))
                .collect()
        } else if let Some(values) = value.as_array() {
            values
                .iter()
                .map(|v| {
                    Ok((
                        text(v, "id")?.to_owned(),
                        Json(v.get("value").ok_or("Missing option value")?.clone()),
                    ))
                })
                .collect::<Result<_, String>>()?
        } else {
            return Err("Invalid model options".into());
        };
    }
    for (id, value) in &options {
        if !value.0.is_string() && !value.0.is_boolean() {
            return Err("Model option must be a string or boolean".into());
        }
        let descriptors = model_options(descriptor);
        if !descriptors.iter().any(|o| {
            o["id"] == *id
                && o["options"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|v| v["id"] == value.0))
        }) {
            return Err(format!("Unsupported child model option: {id}"));
        }
    }
    Ok(ModelSelection {
        instance_id: ProviderInstanceId::new(instance).map_err(|e| e.to_string())?,
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
    fn fixture() -> (tempfile::TempDir, HostRpcService, ThreadId) {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("fixture".into()),
            ProjectStore::new(directory.path().join("projects.json")),
        )
        .unwrap();
        let thread = ThreadId::new("parent").unwrap();
        service
            .agent_dispatch(
                &thread,
                CommandId::new("create").unwrap(),
                CommandBody::ThreadCreate {
                    created_by: CreatedBy::User,
                    creation_source: CreationSource::Desktop,
                    project_id: ProjectId::new("project").unwrap(),
                    title: "Parent".into(),
                    model_selection: ModelSelection {
                        instance_id: ProviderInstanceId::new("codex").unwrap(),
                        model: "test-model".into(),
                        options: Default::default(),
                    },
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    branch: None,
                    worktree_path: None,
                },
            )
            .unwrap();
        service
            .agent_dispatch(
                &thread,
                CommandId::new("send").unwrap(),
                CommandBody::MessageDispatch(MessageDispatch {
                    delegated_completion: None,
                    source_plan_ref: None,
                    created_by: CreatedBy::User,
                    creation_source: CreationSource::Desktop,
                    message_id: MessageId::new("input").unwrap(),
                    text: "Parent input".into(),
                    context: None,
                    attachments: vec![],
                    model_selection: None,
                    delivery_intent: None,
                    dispatch_mode: DispatchMode::StartImmediately,
                }),
            )
            .unwrap();
        (directory, service, thread)
    }
    fn task(service: &HostRpcService, thread: &ThreadId, key_input: &Value) -> NodeId {
        let p = service.agent_projection(thread).unwrap();
        let run = &p.runs[0];
        let id = key(thread, "invocation", "delegate_task", key_input).unwrap();
        let task = NodeId::new(format!("node:delegated:{id}")).unwrap();
        service
            .agent_dispatch(
                thread,
                id,
                CommandBody::DelegatedTaskRequest(Box::new(DelegatedTaskRequest {
                    parent_run_id: run.id.clone(),
                    parent_node_id: run.root_node_id.clone().unwrap(),
                    task: "Inspect only this brief".into(),
                    title: None,
                    model_selection: p.thread.model_selection.clone(),
                    runtime_mode: p.thread.runtime_mode,
                    interaction_mode: p.thread.interaction_mode,
                    completion_wake: CompletionWake::SettledOnly,
                    created_by: CreatedBy::Agent,
                    creation_source: CreationSource::Mcp,
                })),
            )
            .unwrap();
        task
    }
    #[test]
    fn model_and_mode_overrides_use_the_live_catalog_without_escalation() {
        let parent = ModelSelection {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            model: "a".into(),
            options: BTreeMap::from([("reasoningEffort".into(), Json(json!("high")))]),
        };
        let model = |id: &str, provider| agent_protocol::models::Model {
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
        };
        let models = vec![
            model("a", ProviderKind::Codex),
            model("b", ProviderKind::Claude),
        ];
        assert_eq!(child_model(&parent, &Value::Null, &models).unwrap(), parent);
        assert!(child_model(&parent, &json!({"model":"invented"}), &models).is_err());
        let cross = child_model(&parent, &json!({"providerInstanceId":"claude"}), &models).unwrap();
        assert_eq!(cross.model, "b");
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
    async fn wait_timeout_and_dropped_wait_upgrade_delivery_without_cancelling_child() {
        let (_directory, service, thread) = fixture();
        let input = json!({"clientRequestId":"wait","mode":"wait","timeoutMs":0});
        let id = task(&service, &thread, &input);
        let result = service
            .agent_tool(
                &thread,
                &ProviderInstanceId::new("codex").unwrap(),
                "retry",
                "delegate_task",
                input,
            )
            .await
            .unwrap();
        assert_eq!(result["taskId"], id.as_str());
        assert_eq!(result["waitTimedOut"], true);
        let p = service.agent_projection(&thread).unwrap();
        assert_eq!(p.subagents[0].completion_wake, CompletionWake::Always);
        let child = service
            .agent_projection(p.subagents[0].child_thread_id.as_ref().unwrap())
            .unwrap();
        assert!(child.runs[0].status.is_blocking());
        let other = task(&service, &thread, &json!({"clientRequestId":"disconnect"}));
        drop(WakeOnDrop {
            service: service.clone(),
            thread: thread.clone(),
            task: Some(other.clone()),
        });
        assert_eq!(
            service
                .agent_projection(&thread)
                .unwrap()
                .subagents
                .iter()
                .find(|t| t.id == other)
                .unwrap()
                .completion_wake,
            CompletionWake::Always
        );
    }
    #[tokio::test]
    async fn bridge_authenticates_scope_and_keeps_calls_open_until_response() {
        let (_directory, service, thread) = fixture();
        let bridge_owner = AgentTools::start(&service).unwrap();
        let config = bridge_owner
            .provider_config(&thread, &ProviderInstanceId::new("codex").unwrap())
            .unwrap();
        let token = config["env"][TOKEN_ENV].as_str().unwrap().to_owned();
        let request = |token| BridgeRequest {
            token,
            invocation: "read".into(),
            name: "t3_thread_read".into(),
            arguments: json!({"threadId":thread,"limit":1}),
        };
        assert!(
            bridge(bridge_owner.address, request("invalid".into()))
                .await
                .is_err()
        );
        let result = bridge(bridge_owner.address, request(token)).await.unwrap();
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        let parent = service.agent_projection(&thread).unwrap();
        let read = service
            .read_agent_thread(
                &parent,
                &json!({"threadId":thread,"afterPosition":result["nextPosition"],"limit":1}),
            )
            .unwrap();
        assert_eq!(read["items"].as_array().unwrap().len(), 0);
        assert!(
            service
                .read_agent_thread(&parent, &json!({"threadId":thread,"limit":101}))
                .is_err()
        );
        bridge_owner.stop.cancel();
    }
}
