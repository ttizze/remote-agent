//! Thread-scoped MCP tools for provider sessions: the orchestrator, thread and
//! project toolkits. Tools send the same commands as clients and read the
//! committed thread state.
mod backend;
mod catalog;
mod orchestrator;
mod project;
mod read;
#[cfg(test)]
mod tests;
mod thread;

pub(crate) use backend::HostOrchestration;
pub(crate) use catalog::read_only_tools;
pub(crate) use catalog::tools;

use agent_domain::{CommandId, Reply, State, ThreadId};
use agent_protocol::device as device_protocol;
use agent_protocol::models::ProviderInstance;
use agent_transport::peer::{JsonlError, JsonlReader, JsonlWriter};
use backend::Orchestration;
use futures_util::future::BoxFuture;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

/// Tool calls are bounded by their schemas. Results have no limit of their own:
/// a valid `thread_read` carries up to 100 items of 50,000 characters twice, as
/// structured content and as its JSON text, so the result direction keeps only
/// the transport's framing guard.
const MAX_REQUEST: usize = 1024 * 1024;
/// Effect's `McpServer` answer to a tool whose result could not be delivered.
const INTERNAL_TOOL_ERROR: &str = "Tool execution failed due to an internal server error.";
const TOKEN_ENV: &str = "AGENT_TOOLS_TOKEN";
pub(crate) const SERVER_NAME: &str = "orchestration";

/// The live provider instances and models the tools offer for delegation.
pub(crate) trait ModelCatalog: Send + Sync {
    fn providers(&self) -> BoxFuture<'_, Result<Vec<ProviderInstance>, String>>;
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
                            let mut input = JsonlReader::with_max_message_bytes(read, MAX_REQUEST);
                            let mut output = JsonlWriter::new(write);
                            let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(5), input.read_line()).await else { return; };
                            let Ok(request) = serde_json::from_str::<BridgeRequest>(&line) else { return; };
                            let scope = scopes.lock().unwrap_or_else(|e| e.into_inner()).get(&request.token).cloned();
                            let result: Result<Value, String> = match (scope, tools.upgrade()) {
                                (Some(scope), Some(tools)) => tokio::select! {
                                    result = tools.call(&scope.thread, &scope.instance, &request.invocation, &request.name, request.arguments) => Ok(result),
                                    _ = input.read_line() => return,
                                },
                                _ => Err("Invalid orchestration scope".into()),
                            };
                            answer(&mut output, &result).await;
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

    /// The thread's tokens for the instance, or for every instance, stop working.
    pub(crate) fn revoke(&self, thread: &ThreadId, instance: Option<&str>) {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, scope| {
                !(scope.thread == *thread && instance.is_none_or(|i| scope.instance == i))
            });
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

fn error_content(error: &str) -> Value {
    json!({"content":[{"type":"text","text":error}],"isError":true})
}

/// Writes one line, or the internal tool error in its place when the line
/// exceeds the framing guard.
async fn write_or_internal_error<W: AsyncWrite + Unpin>(
    output: &mut JsonlWriter<W>,
    line: &str,
    internal_error: impl FnOnce() -> String,
) -> Result<(), JsonlError> {
    match output.write_line(line).await {
        Err(error @ JsonlError::MessageTooLarge { .. }) => {
            tracing::warn!(operation = "conversation.tools", message = %error, "replacing an undeliverable tool result");
            output.write_line(&internal_error()).await
        }
        written => written,
    }
}

/// The bridge's answer to one call.
async fn answer<W: AsyncWrite + Unpin>(
    output: &mut JsonlWriter<W>,
    result: &Result<Value, String>,
) {
    let line = serde_json::to_string(result).expect("JSON values serialize");
    let internal = || json!({"Ok": error_content(INTERNAL_TOOL_ERROR)}).to_string();
    if let Err(error) = write_or_internal_error(output, &line, internal).await {
        tracing::warn!(operation = "conversation.tools", message = %error, "could not answer a tool call");
    }
}

/// The `agent-mcp` stdio server a provider session starts; it forwards tool calls
/// to the Host's bridge with the session's token.
pub async fn serve(address: SocketAddr) -> Result<(), String> {
    if !address.ip().is_loopback() {
        return Err("Local orchestration address required".into());
    }
    let token = std::env::var(TOKEN_ENV).map_err(|_| "Missing orchestration scope")?;
    serve_stdio(address, token, tokio::io::stdin(), tokio::io::stdout()).await
}

async fn serve_stdio(
    address: SocketAddr,
    token: String,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), String> {
    let mut input = JsonlReader::with_max_message_bytes(input, MAX_REQUEST);
    let mut output = JsonlWriter::new(output);
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
                    Some("initialize") => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":SERVER_NAME,"version":env!("CARGO_PKG_VERSION")}}),
                    Some("tools/list") => json!({"tools":tools()}),
                    Some("ping") => json!({}),
                    Some("tools/call") if pending.len() < 16 && !pending.contains_key(&id.to_string()) => {
                        let name = request["params"]["name"].as_str().unwrap_or_default().to_owned();
                        let arguments = request["params"]["arguments"].clone();
                        let token = token.clone(); let id = id.clone(); let key = id.to_string();
                        let call = calls.spawn(async move {
                            let result = bridge(address, BridgeRequest{token,invocation:uuid::Uuid::new_v4().to_string(),name,arguments}).await;
                            (id, result.unwrap_or_else(|error| error_content(&error)))
                        });
                        pending.insert(key,call); continue;
                    }
                    Some("tools/call") => error_content("Tool capacity reached"),
                    _ => { output.write_line(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}).to_string()).await.map_err(|e| e.to_string())?; continue; }
                }; (id.clone(),response)
            }
        };
        let line = json!({"jsonrpc":"2.0","id":id,"result":response}).to_string();
        let internal = || {
            json!({"jsonrpc":"2.0","id":id,"result":error_content(INTERNAL_TOOL_ERROR)}).to_string()
        };
        write_or_internal_error(&mut output, &line, internal)
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
    let mut output = JsonlWriter::with_max_message_bytes(write, MAX_REQUEST);
    output
        .write_line(&serde_json::to_string(&request).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;
    let line = JsonlReader::new(read)
        .read_line()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Orchestration bridge closed")?;
    serde_json::from_str(&line).map_err(|_| "Invalid orchestration response".to_owned())?
}

/// An orchestration failure, or a parameter validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolError {
    Failure { code: &'static str, message: String },
    Invalid(String),
}
pub(crate) type Outcome = Result<Value, ToolError>;

pub(crate) fn failure(code: &'static str, message: impl Into<String>) -> ToolError {
    ToolError::Failure {
        code,
        message: message.into(),
    }
}
pub(crate) fn unavailable() -> ToolError {
    failure(
        "orchestration_error",
        "The operation could not be completed.",
    )
}
pub(crate) fn invalid(description: impl Into<String>) -> ToolError {
    ToolError::Invalid(description.into())
}

/// A tool result as Effect's `McpServer.toolkit` encodes it: failures declared
/// with `failureMode: "return"` are ordinary results.
fn encode(name: &str, outcome: Outcome) -> Value {
    let value = match outcome {
        Ok(value) => value,
        Err(ToolError::Failure { code, message }) => {
            json!({"_tag":"OrchestratorMcpFailure","code":code,"message":message})
        }
        Err(ToolError::Invalid(description)) => json!({
            "_tag": "AiError",
            "module": "Toolkit",
            "method": format!("{name}.handle"),
            "reason": {"_tag":"ToolParameterValidationError","toolName":name,"description":description},
        }),
    };
    json!({
        "isError": false,
        "structuredContent": value,
        "content": [{"type":"text","text":value.to_string()}],
    })
}

/// The calling session: its thread and the provider instance it runs on.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub(crate) thread: &'a ThreadId,
    pub(crate) instance: &'a str,
}

/// The client's retry key, or a fresh one.
pub(crate) fn request_key(client: Option<&str>) -> String {
    client.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned)
}
fn component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' => {
                (byte as char).to_string()
            }
            b'\'' | b'(' | b')' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
/// The session part of stable ids: the caller's thread and instance.
fn session(scope: Scope<'_>) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for part in [scope.thread.as_str(), scope.instance] {
        digest.update(&(part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &digest.finish().as_ref()[..12],
    )
}
pub(crate) fn stable_command(
    scope: Scope<'_>,
    operation: &str,
    key: &str,
    index: Option<usize>,
) -> CommandId {
    let mut parts = vec![
        "command".to_owned(),
        "mcp".into(),
        session(scope),
        component(operation),
        component(key),
    ];
    parts.extend(index.map(|index| index.to_string()));
    CommandId::new(parts.join(":")).expect("derived id")
}
pub(crate) fn stable_id(kind: &str, scope: Scope<'_>, parts: &[&str]) -> String {
    let mut all = vec![kind.to_owned(), "mcp".into(), session(scope)];
    all.extend(parts.iter().map(|part| component(part)));
    all.join(":")
}
pub(crate) fn new_command() -> CommandId {
    CommandId::new(format!("mcp:{}", uuid::Uuid::new_v4())).expect("derived id")
}

pub(crate) fn decode<T: DeserializeOwned>(input: &Value) -> Result<T, ToolError> {
    let value = if input.is_null() {
        json!({})
    } else {
        input.clone()
    };
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()))
}
/// Trims the value and rejects empty, with an optional UTF-16 length cap.
pub(crate) fn trimmed(field: &str, value: &str, max: Option<usize>) -> Result<String, ToolError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if let Some(max) = max
        && value.encode_utf16().count() > max
    {
        return Err(invalid(format!("{field} must be at most {max} characters")));
    }
    Ok(value.to_owned())
}
pub(crate) fn bounded(
    field: &str,
    value: u64,
    min: u64,
    max: Option<u64>,
) -> Result<(), ToolError> {
    if value < min || max.is_some_and(|max| value > max) {
        return Err(invalid(format!("{field} is out of range")));
    }
    Ok(())
}
pub(crate) fn thread_id(value: &str) -> Result<ThreadId, ToolError> {
    ThreadId::new(value).map_err(|error| invalid(error.to_string()))
}

pub(crate) struct AgentTools {
    backend: Arc<dyn Orchestration>,
    devices: Arc<crate::device::DeviceService>,
}

impl AgentTools {
    pub(crate) fn new(backend: Arc<dyn Orchestration>) -> Self {
        Self {
            backend,
            devices: crate::device::DeviceService::new(
                std::env::temp_dir().join("remote-agent-device-tools"),
            ),
        }
    }

    pub(crate) fn with_devices(mut self, devices: Arc<crate::device::DeviceService>) -> Self {
        self.devices = devices;
        self
    }

    /// One tool call as an MCP `CallToolResult`.
    pub(crate) async fn call(
        &self,
        thread: &ThreadId,
        instance: &str,
        _invocation: &str,
        name: &str,
        input: Value,
    ) -> Value {
        let scope = Scope { thread, instance };
        let outcome = match name {
            "orchestrator_capabilities" => self.capabilities(scope).await,
            "delegate_task" => self.delegate_task(scope, &input).await,
            "task_status" => self.task_status(scope, &input).await,
            "task_cancel" => self.task_cancel(scope, &input).await,
            "create_threads" => self.create_threads(scope, &input).await,
            "thread_list" => self.list_threads(scope, &input).await,
            "thread_read" => self.read_thread(scope, &input).await,
            "thread_update" => self.update_thread(scope, &input).await,
            "thread_send" => self.send_to_thread(scope, &input).await,
            "thread_wait" => self.wait_for_thread(scope, &input).await,
            "thread_interrupt" => self.interrupt_thread(scope, &input).await,
            "thread_search" => self.search(scope, &input).await,
            "thread_fork" => self.fork(scope, &input).await,
            "thread_merge_back" => self.merge_back(scope, &input).await,
            "thread_transfers" => self.transfers(scope, &input).await,
            "thread_configuration" => self.configuration(scope, &input).await,
            "thread_configure" => self.configure(scope, &input).await,
            "pending_request_list" => self.pending_requests(scope, &input).await,
            "pending_request_read" => self.pending_request(scope, &input).await,
            "pending_request_respond" => self.respond(scope, &input).await,
            "thread_organize" => self.organize(scope, &input).await,
            "queue_list" => self.queue_list(scope, &input).await,
            "queue_read" => self.queue_read(scope, &input).await,
            "queue_edit" | "queue_cancel" | "queue_reorder" | "queue_promote_to_steer" => {
                self.queue_command(scope, name, &input).await
            }
            "thread_launch" => self.launch(scope, &input).await,
            "project_list" => self.project_list(scope, &input).await,
            "project_read" => self.project_read(scope, &input).await,
            "project_create" => self.project_create(scope, &input).await,
            "device_list" => self.device_list(scope, &input).await,
            "device_open" => self.device_open(scope, &input).await,
            "device_screenshot" => self.device_screenshot(scope, &input).await,
            "device_close" => self.device_close(scope, &input).await,
            _ => {
                return error_content(&format!("Tool {name} not found"));
            }
        };
        encode(name, outcome)
    }

    async fn state(&self, thread: &ThreadId) -> Result<Arc<State>, String> {
        self.backend.state(thread).await
    }

    async fn device_list(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let host_id = input.get("hostId").and_then(Value::as_str).map(str::to_owned);
        let current = self.devices.state_async().await;
        if current.host_status == device_protocol::DeviceHostStatus::Disabled {
            return Err(failure(
                "device_unavailable",
                "Device support is off. Enable it in the Device panel first.",
            ));
        }
        if !current.agent_access_enabled {
            return Err(failure(
                "device_unavailable",
                "Agent device access is off. Enable it in the Device panel first.",
            ));
        }
        let state = self
            .devices
            .list(device_protocol::DeviceListInput::default())
            .await
            .map_err(|error| failure("device_list_failed", error))?;
        let open = state
            .sessions
            .iter()
            .filter(|session| session.thread_id == *scope.thread)
            .map(|session| json!({"hostId": session.host_id, "deviceId": session.device_id}))
            .collect::<Vec<_>>();
        Ok(json!({
            "hostStatuses": state.host_statuses.iter().filter(|(id, _)| host_id.as_ref().is_none_or(|host| host == *id)).map(|(id, status)| (id.clone(), json!({"status": device_host_status_name(status.status), "detail": status.detail}))).collect::<serde_json::Map<_, _>>(),
            "hosts": state.hosts.iter().filter(|host| host_id.as_ref().is_none_or(|id| id == &host.id)).map(device_host_json).collect::<Vec<_>>(),
            "devices": state.devices.iter().filter(|device| host_id.as_ref().is_none_or(|id| id == &device.host_id)).map(device_summary_json).collect::<Vec<_>>(),
            "open": open,
        }))
    }

    async fn device_open(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let host_id = input
            .get("hostId")
            .and_then(Value::as_str)
            .unwrap_or(device_protocol::LOCAL_DEVICE_HOST_ID)
            .to_owned();
        let requested_device = input.get("deviceId").and_then(Value::as_str);
        let requested_platform = input.get("platform").and_then(Value::as_str);
        let current = self.devices.state_async().await;
        if current.host_status == device_protocol::DeviceHostStatus::Disabled {
            return Err(failure(
                "device_unavailable",
                "Device support is off. Enable it in the Device panel first.",
            ));
        }
        if !current.agent_access_enabled {
            return Err(failure(
                "device_unavailable",
                "Agent device access is off. Enable it in the Device panel first.",
            ));
        }
        let state = self
            .devices
            .list(device_protocol::DeviceListInput::default())
            .await
            .map_err(|error| failure("device_list_failed", error))?;
        let platform = requested_platform.map(parse_device_platform).transpose()?;
        let candidates = state
            .devices
            .iter()
            .filter(|device| {
                device.host_id == host_id
                    && requested_device.is_none_or(|id| id == device.id)
                    && platform.is_none_or(|platform| platform == device.platform)
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(failure(
                "device_unavailable",
                format!("No matching device was found on host {host_id}. Call device_list for current ids."),
            ));
        }
        if requested_device.is_none() && platform.is_none() {
            let platforms = candidates
                .iter()
                .map(|device| device.platform)
                .collect::<std::collections::BTreeSet<_>>();
            if platforms.len() > 1 {
                return Err(invalid("Both iOS and Android devices are available; pass platform or deviceId."));
            }
        }
        let target = candidates
            .iter()
            .find(|device| device.booted)
            .copied()
            .unwrap_or(candidates[0]);
        // Resolve consent and the helper endpoint before booting. This keeps
        // a missing agent installation from powering up a device that cannot
        // be driven by the tool result.
        let agent = match self
            .devices
            .agent_device_target(target.host_id.as_str(), scope.thread, target.id.as_str())
            .await
        {
            Ok(Some(agent)) => agent,
            Ok(None) => {
                return Err(failure(
                    "device_agent_unavailable",
                    format!("Agent device access is not bootstrapped for host {}.", target.host_id),
                ));
            }
            Err(error) => return Err(failure("device_agent_unavailable", error)),
        };
        let session = self
            .devices
            .open(device_protocol::DeviceOpenInput {
                thread_id: scope.thread.clone(),
                host_id: Some(target.host_id.clone()),
                device_id: target.id.clone(),
                platform: target.platform,
                boot: true,
            })
            .await
            .map_err(|error| failure("device_open_failed", error))?;
        let device = self
            .devices
            .state_async()
            .await
            .devices
            .into_iter()
            .find(|device| device.host_id == session.host_id && device.id == session.device_id)
            .unwrap_or_else(|| (*target).clone());
        let mut target_args = device_protocol::agent_device_target_args(&device);
        target_args.extend(agent.target_args.iter().cloned());
        Ok(json!({
            "device": device_summary_json(&device),
            "agentDevice": {"command": agent.command, "targetArgs": target_args},
            "quickStart": device_quick_start(&device, &target_args, &agent.command),
        }))
    }

    async fn device_screenshot(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let state = self.devices.state_async().await;
        if state.host_status == device_protocol::DeviceHostStatus::Disabled {
            return Err(failure(
                "device_unavailable",
                "Device support is off. Enable it in the Device panel first.",
            ));
        }
        if !state.agent_access_enabled {
            return Err(failure(
                "device_unavailable",
                "Agent device access is off. Enable it in the Device panel first.",
            ));
        }
        let host_id = input.get("hostId").and_then(Value::as_str);
        let device_id = input.get("deviceId").and_then(Value::as_str);
        let target = if let Some(device_id) = device_id {
            (host_id.unwrap_or(device_protocol::LOCAL_DEVICE_HOST_ID), device_id.to_owned())
        } else {
            let sessions = self.devices.sessions_for_thread(scope.thread).await;
            let session = sessions
                .iter()
                .rev()
                .find(|session| host_id.is_none_or(|host| host == session.host_id))
                .ok_or_else(|| failure("device_unavailable", "No device is open in this thread. Call device_open first."))?;
            (session.host_id.as_str(), session.device_id.clone())
        };
        let shot = self
            .devices
            .screenshot(device_protocol::DeviceScreenshotInput {
                host_id: Some(target.0.to_owned()),
                device_id: target.1,
            })
            .await
            .map_err(|error| failure("device_screenshot_failed", error))?;
        Ok(json!({
            "device": device_summary_json(&shot.device),
            "screenshot": {
                "mimeType": "image/png",
                "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, shot.png),
                "width": shot.width,
                "height": shot.height,
            }
        }))
    }

    async fn device_close(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let state = self.devices.state_async().await;
        if state.host_status == device_protocol::DeviceHostStatus::Disabled {
            return Err(failure(
                "device_unavailable",
                "Device support is off. Enable it in the Device panel first.",
            ));
        }
        if !state.agent_access_enabled {
            return Err(failure(
                "device_unavailable",
                "Agent device access is off. Enable it in the Device panel first.",
            ));
        }
        self.devices
            .close(device_protocol::DeviceCloseInput {
                thread_id: scope.thread.clone(),
                host_id: input.get("hostId").and_then(Value::as_str).map(str::to_owned),
                device_id: input.get("deviceId").and_then(Value::as_str).map(str::to_owned),
                shutdown: input.get("shutdown").and_then(Value::as_bool).unwrap_or(false),
            })
            .await
            .map_err(|error| failure("device_close_failed", error))?;
        Ok(json!({}))
    }
    /// A command already handled replays its receipt whatever it now carries: a
    /// retry resolves its dispatch mode or target run from the current state,
    /// which may have moved on since the first attempt. A receipt for another
    /// thread is a conflict.
    async fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: agent_domain::Command,
    ) -> Result<u64, String> {
        if let Some(receipt) = self.backend.receipt(&id).await? {
            if receipt.thread != *thread {
                return Err("command-id-conflict".into());
            }
            return match receipt.reply {
                Reply::Rejected { reason } => Err(reason),
                _ => Ok(receipt.sequence),
            };
        }
        let dispatched = self.backend.dispatch(thread, id, command).await?;
        match dispatched.reply {
            Reply::Rejected { reason } => Err(reason),
            _ => Ok(dispatched.sequence),
        }
    }
}

fn parse_device_platform(value: &str) -> Result<device_protocol::DevicePlatform, ToolError> {
    match value {
        "ios" => Ok(device_protocol::DevicePlatform::Ios),
        "android" => Ok(device_protocol::DevicePlatform::Android),
        _ => Err(invalid("platform must be ios or android")),
    }
}

fn device_platform_name(platform: device_protocol::DevicePlatform) -> &'static str {
    match platform {
        device_protocol::DevicePlatform::Ios => "ios",
        device_protocol::DevicePlatform::Android => "android",
    }
}

fn device_host_kind_name(kind: device_protocol::DeviceHostKind) -> &'static str {
    match kind {
        device_protocol::DeviceHostKind::Local => "local",
        device_protocol::DeviceHostKind::Ssh => "ssh",
    }
}

fn device_host_status_name(status: device_protocol::DeviceHostStatus) -> &'static str {
    match status {
        device_protocol::DeviceHostStatus::Disabled => "disabled",
        device_protocol::DeviceHostStatus::Idle => "idle",
        device_protocol::DeviceHostStatus::Installing => "installing",
        device_protocol::DeviceHostStatus::Starting => "starting",
        device_protocol::DeviceHostStatus::Ready => "ready",
        device_protocol::DeviceHostStatus::Failed => "failed",
    }
}

fn device_summary_json(device: &device_protocol::DeviceSummary) -> Value {
    json!({
        "hostId": device.host_id,
        "id": device.id,
        "platform": device_platform_name(device.platform),
        "name": device.name,
        "version": device.version,
        "booted": device.booted,
        "physical": device.physical,
    })
}

fn device_host_json(host: &device_protocol::DeviceHostSummary) -> Value {
    json!({
        "id": host.id,
        "kind": device_host_kind_name(host.kind),
        "label": host.label,
        "platforms": host.platforms.iter().map(|platform| json!({
            "platform": device_platform_name(platform.platform),
            "available": platform.available,
            "reason": platform.reason,
        })).collect::<Vec<_>>(),
        "tools": host.tools.as_ref().map(|tools| json!({
            "hub": device_tool_version_json(&tools.hub),
            "agent": device_tool_version_json(&tools.agent),
        })),
        "toolInspectionError": host.tool_inspection_error,
        "hubInstalled": host.hub_installed,
        "agentDeviceInstalled": host.agent_device_installed,
    })
}

fn device_tool_version_json(version: &device_protocol::DeviceToolVersion) -> Value {
    json!({
        "requiredVersion": version.required_version,
        "installedVersions": version.installed_versions,
        "runningVersion": version.running_version,
    })
}

fn device_quick_start(device: &device_protocol::DeviceSummary, target_args: &[String], command: &str) -> String {
    let quote = |value: &str| {
        if value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/:-".contains(&byte))
        {
            value.to_owned()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    };
    let executable = quote(command);
    let target = target_args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
    let platform_note = match device.platform {
        device_protocol::DevicePlatform::Ios => "First use builds an XCTest runner and can take a couple of minutes; later commands are fast.",
        device_protocol::DevicePlatform::Android => "The Android snapshot helper installs itself on first use.",
    };
    format!(
        "The user is watching {} ({}).\nDrive it with {}. Use this exact executable path; login shells may reset PATH. Always pass {}.\nTypical loop:\n  {} open <bundle-or-package-id> {}     # or: open <app> <deep-link-url>\n  {} snapshot -i {}                     # accessibility tree with @eN refs\n  {} click @e3 {}\n  {} fill @e5 \"text\" {}\n  {} screenshot /tmp/shot.png {}        # or call device_screenshot\n  {} install <app> <path-to-.app-or-.apk> {}\nPrefer snapshot refs over coordinates. Run {} help for workflow guides and {} <command> --help for flags.\nPrefer agent-device for driving this device. simctl, adb, and xcrun remain available for anything it does not cover.\nFor remote hosts, arrange builds, app installation, and any Metro reverse forwarding yourself. The host provides discovery, streaming, and control only.\nKeep the returned --config and --session flags on every command. Other hosts can be used concurrently; opening one does not switch these commands.\n{}",
        device.name,
        device.version,
        executable,
        target,
        executable,
        target,
        executable,
        target,
        executable,
        target,
        executable,
        target,
        executable,
        target,
        executable,
        target,
        executable,
        executable,
        platform_note,
    )
}
