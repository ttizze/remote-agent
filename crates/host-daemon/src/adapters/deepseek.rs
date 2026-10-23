//! DeepSeek Harness owns inference, tools, credentials and native sessions.
//! BEX speaks its standard ACP v1 stdio interface and checkpoints display state.
use crate::host_rpc::{
    agent::{
        AccountCommand, AccountReply, Agent, AgentChange, AgentEvent, Identity, SessionListScope,
        SessionPage, SessionSummary, SubmissionState, emit,
    },
    requests::{AnswerSource, RequestOrigin},
    service::Failure,
    submission::{RunningInput, SubmissionTarget},
};
use agent_protocol::{
    composer::ComposerCatalog,
    ids::TurnId,
    models::*,
    operations as op,
    permissions::{PermissionMode, PermissionSettings},
    requests::{Answer, Choice, Request, RequestBody, RequestTarget},
    session::{Capabilities, ProviderKind, RequestDelivery, SessionChange, SessionRef},
};
use agent_transport::peer::{JsonlReader, JsonlWriter};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::ChildStdin,
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use agent_protocol::dsh::{KeyProvider, KeyUpdate};

mod credentials;
mod model_catalog;
use credentials::ManagedKeys;
use model_catalog::{catalog, credential_route, provider_profile, resolved_model_settings};

const PROVIDER: ProviderKind = ProviderKind::DeepSeek;
const ACCOUNT: &str = "deepseek:native";
const DEFAULT_MODEL: &str = "[\"deepseek-official\",\"deepseek-flash\"]";
type Pending = HashMap<u64, oneshot::Sender<Result<Value, String>>>;

#[derive(Clone)]
pub struct DeepSeek(Arc<Inner>);
struct Inner {
    program: PathBuf,
    directory: PathBuf,
    records: Mutex<BTreeMap<String, ThreadResponse>>,
    rpc: Mutex<Option<Arc<Rpc>>>,
    events: mpsc::Sender<AgentEvent>,
    receiver: std::sync::Mutex<Option<mpsc::Receiver<AgentEvent>>>,
    stop: CancellationToken,
    workers: Mutex<tokio::task::JoinSet<()>>,
    credential_updates: Mutex<()>,
}
struct Rpc {
    input: Arc<Mutex<JsonlWriter<ChildStdin>>>,
    pending: Mutex<Pending>,
    next: AtomicU64,
    loaded: Mutex<HashSet<String>>,
    model_settings: Mutex<HashMap<String, agent_protocol::models::ModelSettings>>,
    instance: uuid::Uuid,
    stop: CancellationToken,
    stopped: CancellationToken,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl From<DeepSeek> for super::Backend {
    fn from(agent: DeepSeek) -> Self {
        Self {
            provider: PROVIDER,
            agent: Ok(Arc::new(agent)),
            dictation: None,
        }
    }
}
fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new("deepseek_failed", error)
}
fn unsupported() -> Failure {
    Failure::new(
        "unsupported_operation",
        "DeepSeek Harness does not expose this operation over ACP",
    )
}
fn session(id: &str) -> SessionRef {
    SessionRef {
        provider: PROVIDER,
        id: id.into(),
    }
}
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

impl DeepSeek {
    pub async fn load(program: PathBuf, directory: PathBuf) -> anyhow::Result<Self> {
        crate::platform::create_state_directory(&directory)?;
        let mut records: BTreeMap<String, ThreadResponse> =
            match tokio::fs::read(directory.join("conversations.json")).await {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            };
        // A new Host never claims that a dead ACP process is still running.
        for record in records.values_mut() {
            record.thread.status = SessionStatus::Idle;
            for turn in record.thread.turns.iter_mut().flatten() {
                if turn.status == TurnStatus::Running {
                    Arc::make_mut(turn).status = TurnStatus::Interrupted;
                }
            }
        }
        let (events, receiver) = mpsc::channel(256);
        Ok(Self(Arc::new(Inner {
            program,
            directory,
            records: Mutex::new(records),
            rpc: Mutex::new(None),
            events,
            receiver: std::sync::Mutex::new(Some(receiver)),
            stop: CancellationToken::new(),
            workers: Mutex::new(tokio::task::JoinSet::new()),
            credential_updates: Mutex::new(()),
        })))
    }
    fn command(&self) -> Result<tokio::process::Command, Failure> {
        let program = &self.0.program;
        if program.extension().is_some_and(|e| e == "js" || e == "mjs") {
            let runtime = std::env::var_os("BEX_DEEPSEEK_NODE").unwrap_or_else(|| "node".into());
            let mut command = bex_process::command(Path::new(&runtime)).map_err(failure)?;
            command.arg(program);
            return Ok(command);
        }
        #[cfg(windows)]
        {
            let managed = directories::BaseDirs::new().map(|base| {
                base.data_local_dir()
                    .join("BEX/deepseek-harness/node_modules/@deepseek-ai/dsh/lib/bin.js")
            });
            let path = std::env::var_os("PATH").unwrap_or_default();
            let script = managed
                .filter(|p| p.is_file())
                .into_iter()
                .chain(
                    std::env::split_paths(&path)
                        .map(|p| p.join("node_modules/@deepseek-ai/dsh/lib/bin.js")),
                )
                .find(|p| p.is_file());
            if program == Path::new("dsh")
                && let Some(script) = script
            {
                let runtime = directories::BaseDirs::new()
                    .map(|base| {
                        base.data_local_dir()
                            .join("BEX/deepseek-harness/runtime/node.exe")
                    })
                    .filter(|p| p.is_file());
                let runtime = std::env::var_os("BEX_DEEPSEEK_NODE")
                    .or_else(|| runtime.map(|p| p.into_os_string()))
                    .unwrap_or_else(|| "node".into());
                let mut command = bex_process::command(Path::new(&runtime)).map_err(failure)?;
                command.arg(script);
                return Ok(command);
            }
        }
        bex_process::command(program).map_err(failure)
    }
    async fn rpc(&self) -> Result<Arc<Rpc>, Failure> {
        let mut slot = self.0.rpc.lock().await;
        if let Some(rpc) = slot.as_ref().filter(|rpc| !rpc.stop.is_cancelled()) {
            return Ok(rpc.clone());
        }
        let mut command = self.command()?;
        let patch = self.0.directory.join("acp.patch.json");
        let keys = ManagedKeys::load(&self.0.directory).await?;
        let mut providers = serde_json::Map::new();
        for provider in KeyProvider::ALL {
            if provider != KeyProvider::DeepSeek && keys.get(provider).is_some() {
                providers.insert(provider.route().into(), provider_profile(provider));
            }
        }
        let mut overlays = vec![
            json!({"id":"acp","config":{"provider":"deepseek-official","model":"deepseek-flash"}}),
            json!({"id":"llm-pi-ai","config":{"providers":providers}}),
        ];
        if !keys.is_empty() {
            let credentials = self.0.directory.join("dsh-runtime-credentials.json");
            crate::platform::save_private_json(&credentials, &keys).map_err(failure)?;
            overlays.push(json!({"id":"credentials","config":{"path":credentials}}));
        }
        crate::platform::save_private_json(&patch, &overlays).map_err(failure)?;
        command
            .args(["--profile", "acp", "--patch"])
            .arg(patch)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(key) = keys.get(KeyProvider::DeepSeek) {
            command.env("DEEPSEEK_API_KEY", key);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().map_err(|e| {
            Failure::new(
                "provider_unavailable",
                format!(
                    "DeepSeek Harness を起動できません。npm install -g @deepseek-ai/dsh@next: {e}"
                ),
            )
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| failure("Harness stdin unavailable"))?;
        let mut output = JsonlReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| failure("Harness stdout unavailable"))?,
        );
        let rpc = Arc::new(Rpc {
            input: Arc::new(Mutex::new(JsonlWriter::new(input))),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            loaded: Mutex::new(HashSet::new()),
            model_settings: Mutex::new(HashMap::new()),
            instance: uuid::Uuid::new_v4(),
            stop: self.0.stop.child_token(),
            stopped: CancellationToken::new(),
        });
        let weak = Arc::downgrade(&self.0);
        let reader = rpc.clone();
        tokio::spawn(async move {
            loop {
                let line = tokio::select! { _ = reader.stop.cancelled() => break, line = output.read_line() => line };
                let Ok(Some(line)) = line else {
                    break;
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                if let Some(method) = value["method"].as_str() {
                    if let Some(inner) = weak.upgrade() {
                        let adapter = DeepSeek(inner);
                        if method == "session/update" {
                            if adapter.update(&value["params"]).await.is_err() {
                                break;
                            }
                        } else if value.get("id").is_some() {
                            if method == "session/request_permission" {
                                if adapter.permission(&reader, &value).await.is_err() {
                                    break;
                                }
                            } else {
                                let _ = reader.write(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Unsupported client method"}})).await;
                            }
                        }
                    } else {
                        break;
                    }
                } else if let Some(id) = value["id"].as_u64()
                    && let Some(reply) = reader.pending.lock().await.remove(&id)
                {
                    let result = if value.get("error").is_some() {
                        Err(value["error"]["message"]
                            .as_str()
                            .unwrap_or("Harness request failed")
                            .into())
                    } else {
                        Ok(value["result"].clone())
                    };
                    let _ = reply.send(result);
                }
            }
            reader.stop.cancel();
            let _ = reader.input.lock().await.shutdown().await;
            if tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
            reader.stopped.cancel();
            for (_, reply) in reader.pending.lock().await.drain() {
                let _ = reply.send(Err("DeepSeek Harness disconnected".into()));
            }
            if let Some(inner) = weak.upgrade() {
                let _ = emit(&inner.events, AgentChange::SourceClosed(reader.instance)).await;
            }
        });
        let initialized = match rpc.call("initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"bex","version":env!("CARGO_PKG_VERSION")}}), Some(Duration::from_secs(90))).await {Ok(value)=>value,Err(error)=>{rpc.stop.cancel();return Err(error);}};
        if initialized["protocolVersion"] != 1 {
            rpc.stop.cancel();
            return Err(failure("Harness requires ACP v1"));
        }
        *slot = Some(rpc.clone());
        Ok(rpc)
    }
    async fn update_key(&self, update: KeyUpdate) -> Result<(), Failure> {
        let original = update.key.map(zeroize::Zeroizing::new);
        let key = original.as_ref().map(|key| key.trim());
        credentials::validate_key(update.provider, key)?;
        let mut keys = ManagedKeys::load(&self.0.directory).await?;
        self.reset_rpc().await?;
        keys.set(update.provider, key);
        keys.save(&self.0.directory).await
    }
    async fn configured(&self) -> Result<bool, Failure> {
        Ok(!ManagedKeys::load(&self.0.directory).await?.is_empty()
            || self.external_key_configured().await)
    }
    async fn external_key_configured(&self) -> bool {
        if std::env::var("DEEPSEEK_API_KEY").is_ok_and(|value| !value.trim().is_empty()) {
            return true;
        }
        let home = std::env::var_os("DSH_HOME")
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|base| base.home_dir().join(".dsh")));
        if let Some(home) = home {
            tokio::fs::read_to_string(home.join(".credentials.yaml"))
                .await
                .is_ok_and(|text| {
                    text.lines()
                        .any(|line| line.trim_start().starts_with("DEEPSEEK_API_KEY:"))
                })
        } else {
            false
        }
    }
    async fn reset_rpc(&self) -> Result<(), Failure> {
        if self
            .0
            .records
            .lock()
            .await
            .values()
            .any(|r| r.thread.status == SessionStatus::Running)
        {
            return Err(failure(
                "DeepSeek のタスクが完了してから認証設定を変更してください。",
            ));
        }
        if let Some(rpc) = self.0.rpc.lock().await.take() {
            let loaded: Vec<_> = rpc.loaded.lock().await.iter().cloned().collect();
            for id in loaded {
                rpc.call(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
            }
            rpc.stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(15), rpc.stopped.cancelled()).await;
        }
        Ok(())
    }
    async fn save(&self, records: &BTreeMap<String, ThreadResponse>) -> Result<(), Failure> {
        crate::platform::save_private_json(&self.0.directory.join("conversations.json"), records)
            .map_err(failure)
    }
    async fn change(&self, id: &str, change: SessionChange) -> Result<(), Failure> {
        let mut records = self.0.records.lock().await;
        let record = records
            .get_mut(id)
            .ok_or_else(|| failure("Unknown DeepSeek conversation"))?;
        record.thread = change.apply(&record.thread).map_err(failure)?;
        record.thread.updated_at = Some(now());
        self.save(&records).await?;
        drop(records);
        emit(
            &self.0.events,
            AgentChange::Session {
                session: session(id),
                change,
            },
        )
        .await
        .map_err(failure)
    }
    async fn ensure_loaded(&self, rpc: &Rpc, record: &ThreadResponse) -> Result<(), Failure> {
        let id = &record
            .thread
            .id
            .as_ref()
            .ok_or_else(|| failure("Missing session ID"))?
            .id;
        let mut loaded = rpc.loaded.lock().await;
        if !loaded.contains(id) {
            rpc.call(
                "session/resume",
                json!({"sessionId":id,"cwd":record.thread.cwd,"mcpServers":[]}),
                Some(Duration::from_secs(90)),
            )
            .await?;
            loaded.insert(id.clone());
        }
        Ok(())
    }
    async fn select(
        &self,
        rpc: &Rpc,
        id: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(), Failure> {
        if let Some(model) = model {
            self.validate_model_key(model).await?;
            rpc.call(
                "session/set_config_option",
                json!({"sessionId":id,"configId":"model","value":model}),
                Some(Duration::from_secs(30)),
            )
            .await?;
        }
        if let Some(effort) = effort {
            rpc.call(
                "session/set_config_option",
                json!({"sessionId":id,"configId":"reasoning_effort","value":effort}),
                Some(Duration::from_secs(30)),
            )
            .await?;
        }
        Ok(())
    }
    async fn validate_model_key(&self, model: &str) -> Result<(), Failure> {
        if credential_route(model) == Some(KeyProvider::DeepSeek) {
            let keys = ManagedKeys::load(&self.0.directory).await?;
            let key = keys
                .get(KeyProvider::DeepSeek)
                .map(|key| zeroize::Zeroizing::new(key.to_owned()))
                .or_else(|| {
                    std::env::var("DEEPSEEK_API_KEY")
                        .ok()
                        .map(zeroize::Zeroizing::new)
                });
            if key
                .as_ref()
                .is_some_and(|key| credentials::misplaced_key(KeyProvider::DeepSeek, key))
            {
                return Err(Failure::new(
                    "dsh_key_provider_mismatch",
                    "選択中の送信先はDeepSeek公式ですが、保存されているのはOpenRouterのキーです。モデル選択で「OpenRouter · DeepSeek: DeepSeek V4.1 Flash」など、OpenRouterのモデルを選択してください。",
                ));
            }
        }
        Ok(())
    }
    async fn update(&self, params: &Value) -> Result<(), Failure> {
        let Some(id) = params["sessionId"].as_str() else {
            return Ok(());
        };
        let update = &params["update"];
        let records = self.0.records.lock().await;
        let Some(record) = records.get(id) else {
            return Ok(());
        };
        let Some(turn) = record
            .thread
            .turns
            .as_ref()
            .and_then(|t| t.last())
            .filter(|t| t.status == TurnStatus::Running)
        else {
            return Ok(());
        };
        let turn_id = turn.id.clone();
        let item = translate_update(update, turn);
        drop(records);
        if let Some(item) = item {
            self.change(
                id,
                SessionChange::Item {
                    turn_id,
                    item: Arc::new(item),
                },
            )
            .await?;
        }
        Ok(())
    }
    async fn permission(&self, rpc: &Arc<Rpc>, value: &Value) -> Result<(), Failure> {
        let params = &value["params"];
        let id = params["sessionId"]
            .as_str()
            .ok_or_else(|| failure("Missing permission session"))?;
        let turn = self
            .0
            .records
            .lock()
            .await
            .get(id)
            .and_then(|record| record.thread.active_turn_id());
        let Some(turn_id) = turn else {
            return rpc.write(json!({"jsonrpc":"2.0","id":value["id"],"result":{"outcome":{"outcome":"cancelled"}}})).await;
        };
        let choices = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                Some(Choice {
                    id: option["optionId"].as_str()?.into(),
                    label: option["name"].as_str().unwrap_or("Permission").into(),
                    description: option["kind"].as_str().unwrap_or_default().into(),
                })
            })
            .collect();
        let request = Request {
            id: format!("deepseek:{}:{}", rpc.instance, value["id"]).into(),
            target: RequestTarget::Turn {
                turn_id,
                item_id: params["toolCall"]["toolCallId"].as_str().map(Into::into),
            },
            delivery: RequestDelivery::Awaiting,
            body: RequestBody::Permission {
                description: params["toolCall"]["title"]
                    .as_str()
                    .unwrap_or("DeepSeek Harness permission")
                    .into(),
                details: serde_json::to_string_pretty(&params["toolCall"]).unwrap_or_default(),
                choices,
            },
        };
        emit(
            &self.0.events,
            AgentChange::Request {
                session: session(id),
                origin: RequestOrigin {
                    instance: rpc.instance,
                    native_id: value["id"].clone(),
                    provider: PROVIDER,
                    source: rpc.clone(),
                },
                request,
            },
        )
        .await
        .map_err(failure)
    }
}

impl Rpc {
    async fn write(&self, value: Value) -> Result<(), Failure> {
        self.input
            .lock()
            .await
            .write_line(&value.to_string())
            .await
            .map_err(failure)
    }
    async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, Failure> {
        if self.stop.is_cancelled() {
            return Err(failure("Harness disconnected"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (send, receive) = oneshot::channel();
        self.pending.lock().await.insert(id, send);
        if let Err(e) = self
            .write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        let result = if let Some(timeout) = timeout {
            match tokio::time::timeout(timeout, receive).await {
                Ok(result) => result.map_err(failure)?,
                Err(_) => {
                    self.pending.lock().await.remove(&id);
                    return Err(Failure::unknown(
                        "deepseek_timeout",
                        "Harness did not acknowledge the request",
                    ));
                }
            }
        } else {
            receive.await.map_err(failure)?
        };
        result.map_err(failure)
    }
}
#[async_trait::async_trait]
impl AnswerSource for Rpc {
    fn is_alive(&self) -> bool {
        !self.stop.is_cancelled()
    }
    async fn prepare(
        &self,
        native_id: &Value,
        body: &RequestBody,
        answer: &Answer,
    ) -> Result<BoxFuture<'static, Result<(), Failure>>, Failure> {
        agent_protocol::requests::validate_answer(body, answer).map_err(failure)?;
        let Answer::Permission { choice_id } = answer else {
            return Err(unsupported());
        };
        let response = json!({"jsonrpc":"2.0","id":native_id,"result":{"outcome":{"outcome":"selected","optionId":choice_id}}});
        let input = self.input.clone();
        let stop = self.stop.clone();
        Ok(Box::pin(async move {
            if stop.is_cancelled() {
                return Err(failure("Harness disconnected"));
            }
            input
                .lock()
                .await
                .write_line(&response.to_string())
                .await
                .map_err(|e| Failure::unknown("answer_delivery_unknown", e))
        }))
    }
}

fn translate_update(update: &Value, turn: &Turn) -> Option<Item> {
    let kind = update["sessionUpdate"].as_str()?;
    let items = turn.items.as_deref().unwrap_or_default();
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            let text = update["content"]["text"].as_str()?;
            let reasoning = kind == "agent_thought_chunk";
            let previous = items.last().filter(|item| {
                if reasoning {
                    matches!(item.body(), ItemBody::Reasoning { .. })
                } else {
                    matches!(item.body(), ItemBody::AssistantText { .. })
                }
            });
            let id = previous
                .map(|i| i.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
            let body = if reasoning {
                let mut content = previous
                    .and_then(|i| {
                        if let ItemBody::Reasoning { content, .. } = i.body() {
                            Some(content.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| vec![String::new()]);
                content.last_mut()?.push_str(text);
                ItemBody::Reasoning {
                    content,
                    summary: vec![],
                }
            } else {
                let mut content = previous
                    .and_then(|i| {
                        if let ItemBody::AssistantText { text, .. } = i.body() {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                content.push_str(text);
                ItemBody::AssistantText {
                    text: content,
                    phase: AssistantPhase::Unknown,
                }
            };
            Some(Item::new(id, ItemStatus::Completed, body))
        }
        "tool_call" | "tool_call_update" => {
            let id = update["toolCallId"].as_str()?;
            let previous = items.iter().find(|item| item.id.as_str() == id);
            let status = match update["status"].as_str() {
                Some("completed") => ItemStatus::Completed,
                Some("failed") => ItemStatus::Failed,
                Some(_) => ItemStatus::Running,
                None => previous.map(|i| i.status).unwrap_or(ItemStatus::Running),
            };
            let mut body =
                previous
                    .map(|i| i.body().clone())
                    .unwrap_or_else(|| ItemBody::ToolCall {
                        kind: ToolKind::Local,
                        tool: update["title"].as_str().unwrap_or("Harness tool").into(),
                        server: None,
                        namespace: None,
                        resource_uri: None,
                        plugin_id: None,
                        arguments: update["rawInput"].clone(),
                        result: None,
                        error: None,
                        content: vec![],
                        success: None,
                        duration_ms: None,
                    });
            if let ItemBody::ToolCall {
                result,
                content,
                success,
                arguments,
                ..
            } = &mut body
            {
                if update.get("rawInput").is_some() {
                    *arguments = update["rawInput"].clone();
                }
                if update.get("rawOutput").is_some() {
                    *result = Some(update["rawOutput"].clone());
                }
                if let Some(blocks) = update["content"].as_array() {
                    *content = blocks
                        .iter()
                        .filter_map(|b| {
                            b["content"]["text"].as_str().map(|s| {
                                agent_protocol::requests::ToolContent::Text { text: s.into() }
                            })
                        })
                        .collect();
                }
                *success = match status {
                    ItemStatus::Completed => Some(true),
                    ItemStatus::Failed => Some(false),
                    _ => None,
                };
            }
            Some(Item::new(id.into(), status, body))
        }
        "plan" => Some(Item::new(
            "deepseek-plan".into(),
            ItemStatus::Completed,
            ItemBody::Plan {
                text: update["entries"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| e["content"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
        )),
        _ => None,
    }
}

#[async_trait::async_trait]
impl Identity for DeepSeek {
    async fn list(&self) -> Result<op::Accounts, Failure> {
        let configured = self.configured().await?;
        Ok(op::Accounts {
            accounts: if configured {
                vec![op::Account {
                    id: ACCOUNT.into(),
                    provider: PROVIDER,
                    email: Some("DSH APIキー".into()),
                    plan_type: None,
                    usage: None,
                }]
            } else {
                vec![]
            },
            selected: if configured {
                HashMap::from([(PROVIDER, ACCOUNT.into())])
            } else {
                HashMap::new()
            },
            error: None,
        })
    }
    async fn dsh_credentials(&self) -> Result<agent_protocol::dsh::Credentials, Failure> {
        Ok(ManagedKeys::load(&self.0.directory)
            .await?
            .status(self.external_key_configured().await))
    }
    async fn update_dsh_credential(
        &self,
        update: KeyUpdate,
    ) -> Result<agent_protocol::dsh::Credentials, Failure> {
        let _update = self.0.credential_updates.lock().await;
        self.update_key(update).await?;
        self.dsh_credentials().await
    }
    async fn account(&self, command: AccountCommand) -> Result<AccountReply, Failure> {
        match command {
            AccountCommand::Select { id } if id == ACCOUNT && self.configured().await? => {
                Ok(op::AccountSelection {
                    provider: PROVIDER,
                    selected_id: id,
                    persistence_error: None,
                }
                .into())
            }
            AccountCommand::Logout { id } if id == ACCOUNT => {
                let _update = self.0.credential_updates.lock().await;
                self.reset_rpc().await?;
                ManagedKeys::default().save(&self.0.directory).await?;
                Ok(AccountReply::Complete)
            }
            _ => Err(failure("Unknown DeepSeek account")),
        }
    }
    async fn usage(&self, _: &str) -> Result<op::AccountUsage, Failure> {
        // ACP has no quota API. An empty result means unavailable usage, not a
        // failed credential setup, and keeps account settings usable.
        Ok(op::AccountUsage {
            windows: vec![],
            fetched_at: now() as i64,
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl Agent for DeepSeek {
    fn data_recipient(&self) -> &'static str {
        "DeepSeek"
    }
    fn running_input(&self) -> RunningInput {
        RunningInput::Queue
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            additional_input: false,
            fork: false,
            rename: true,
            model_change: true,
        }
    }
    fn availability(&self) -> Result<(), Failure> {
        self.validate_create()
    }
    fn validate_create(&self) -> Result<(), Failure> {
        if self.0.stop.is_cancelled() {
            Err(failure("Harness is shutting down"))
        } else {
            Ok(())
        }
    }
    fn storage_directory(&self) -> &Path {
        &self.0.directory
    }
    fn history_scope_directory(&self) -> Option<&Path> {
        // These are Host-owned display checkpoints, not an external native
        // history root. Enabling Harness must retain existing BEX drafts.
        None
    }
    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        scope: SessionListScope<'_>,
        limit: u32,
    ) -> Result<SessionPage, Failure> {
        let records = self.0.records.lock().await;
        let mut data: Vec<_> = records
            .iter()
            .filter(|(id, r)| {
                !matches!(scope, SessionListScope::Descendants(_))
                    && cursor.as_ref().is_none_or(|c| *id > c)
                    && format!(
                        "{} {}",
                        r.thread.name.as_deref().unwrap_or_default(),
                        r.thread.preview.as_deref().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search.to_lowercase())
            })
            .take(limit.max(1) as usize + 1)
            .map(|(_, r)| {
                let mut thread = r.thread.clone();
                thread.turns = None;
                SessionSummary {
                    thread,
                    branch: None,
                }
            })
            .collect();
        let next_cursor = if data.len() > limit.max(1) as usize {
            data.pop();
            data.last()
                .and_then(|r| r.thread.id.as_ref())
                .map(|id| id.id.clone())
        } else {
            None
        };
        Ok(SessionPage { data, next_cursor })
    }
    async fn open(&self, id: &str, _: usize, _: bool) -> Result<ThreadResponse, Failure> {
        self.0
            .records
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| failure("Unknown DeepSeek conversation"))
    }
    async fn read_history(
        &self,
        _: &str,
        _: &str,
        _: bool,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        Err(unsupported())
    }
    async fn read_item(&self, params: &op::ReadItem) -> Result<op::ItemResponse, Failure> {
        let record = self.open(&params.thread_id.id, usize::MAX, true).await?;
        let item = record
            .thread
            .turns
            .iter()
            .flatten()
            .filter(|t| t.id == params.turn_id)
            .flat_map(|t| t.items.iter().flatten())
            .find(|i| i.id == params.item_id)
            .ok_or_else(|| failure("Item not found"))?;
        Ok(op::ItemResponse {
            item: (**item).clone(),
            transfer: None,
        })
    }
    async fn read_turn_items(&self, id: &str, turn_id: &TurnId) -> Result<Vec<Arc<Item>>, Failure> {
        let record = self.open(id, usize::MAX, true).await?;
        record
            .thread
            .turns
            .iter()
            .flatten()
            .find(|t| &t.id == turn_id)
            .and_then(|t| t.items.clone())
            .ok_or_else(|| failure("Turn not found"))
    }
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        _: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        if let Some(model) = model {
            self.validate_model_key(model).await?;
        }
        let rpc = self.rpc().await?;
        let created = rpc
            .call(
                "session/new",
                json!({"cwd":cwd,"mcpServers":[]}),
                Some(Duration::from_secs(90)),
            )
            .await?;
        let id = created["sessionId"]
            .as_str()
            .ok_or_else(|| failure("Harness did not return a session ID"))?;
        rpc.loaded.lock().await.insert(id.into());
        if let Err(error) = self.select(&rpc, id, model, None).await {
            let _ = rpc
                .call(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(15)),
                )
                .await;
            return Err(error);
        }
        let record = ThreadResponse {
            thread: Thread {
                id: Some(session(id)),
                cwd: Some(cwd.into()),
                status: SessionStatus::Idle,
                capabilities: Some(self.capabilities()),
                turns: Some(vec![]),
                updated_at: Some(now()),
                ..Default::default()
            },
            model: Some(ModelRef {
                provider: PROVIDER,
                id: model
                    .or_else(|| {
                        created["configOptions"]
                            .as_array()
                            .and_then(|v| v.iter().find(|o| o["id"] == "model"))
                            .and_then(|o| o["currentValue"].as_str())
                    })
                    .unwrap_or(DEFAULT_MODEL)
                    .into(),
            }),
        };
        let mut records = self.0.records.lock().await;
        records.insert(id.into(), record.clone());
        self.save(&records).await?;
        Ok(record)
    }
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure> {
        Ok(SubmissionState {
            response: self.open(id, usize::MAX, true).await?,
            needs_reload: false,
        })
    }
    async fn submit(
        &self,
        input: &op::Submission,
        route: SubmissionTarget<'_>,
        _: bool,
        _: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure> {
        if !matches!(route, SubmissionTarget::Start { .. }) {
            return Err(Failure::new(
                "unsupported_operation",
                "Harness accepts input after the running task finishes",
            ));
        }
        let id = input.thread_id.id.clone();
        let record = self.open(&id, usize::MAX, true).await?;
        let rpc = self.rpc().await?;
        self.ensure_loaded(&rpc, &record).await?;
        self.select(
            &rpc,
            &id,
            input
                .model
                .as_ref()
                .or(record.model.as_ref())
                .map(|m| m.id.as_str()),
            input.effort.as_deref(),
        )
        .await?;
        let prompt = prompt(&input.input).await?;
        {
            let mut records = self.0.records.lock().await;
            if let Some(record) = records.get_mut(&id) {
                if let Some(model) = &input.model {
                    record.model = Some(model.clone());
                }
                if record.thread.preview.is_none() {
                    record.thread.preview = input.input.iter().find_map(|part| {
                        if let op::Input::Text { text } = part {
                            Some(text.chars().take(200).collect())
                        } else {
                            None
                        }
                    });
                }
            }
            self.save(&records).await?;
        }
        let turn_id: TurnId = uuid::Uuid::new_v4().to_string().into();
        let mut user = Item::new(
            input.client_user_message_id.as_str().into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: op::Input::message_parts(&input.input),
            },
        );
        user.client_input_id = Some(input.client_user_message_id.clone());
        self.change(
            &id,
            SessionChange::Turn {
                turn: Turn {
                    id: turn_id.clone(),
                    status: TurnStatus::Running,
                    items: Some(vec![Arc::new(user)]),
                    started_at: Some(now()),
                    ..Default::default()
                },
                completed: false,
            },
        )
        .await?;
        self.change(
            &id,
            SessionChange::Status {
                status: SessionStatus::Running,
            },
        )
        .await?;
        let adapter = self.clone();
        let running_turn = turn_id.clone();
        self.0.workers.lock().await.spawn(async move {
            let result = rpc
                .call(
                    "session/prompt",
                    json!({"sessionId":id,"prompt":prompt}),
                    None,
                )
                .await;
            let status = match &result {
                Ok(value) if value["stopReason"] == "cancelled" => TurnStatus::Interrupted,
                Ok(_) => TurnStatus::Completed,
                Err(_) => TurnStatus::Failed,
            };
            let record = adapter.open(&id, usize::MAX, true).await;
            if let Ok(record) = record
                && let Some(turn) = record
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .find(|t| t.id == running_turn)
            {
                let mut turn = (**turn).clone();
                turn.status = status;
                if let Err(error) = result {
                    turn.error = Some(ExecutionError {
                        message: error.message,
                        ..Default::default()
                    });
                }
                let _ = adapter
                    .change(
                        &id,
                        SessionChange::Turn {
                            turn,
                            completed: true,
                        },
                    )
                    .await;
            }
            let _ = adapter
                .change(
                    &id,
                    SessionChange::Status {
                        status: SessionStatus::Idle,
                    },
                )
                .await;
        });
        Ok(op::SubmissionReceipt {
            turn_id: Some(turn_id),
        })
    }
    async fn interrupt(&self, id: &str, turn: &TurnId) -> Result<Empty, Failure> {
        let record = self.open(id, usize::MAX, true).await?;
        if record.thread.active_turn_id().as_ref() != Some(turn) {
            return Err(failure("Turn is no longer running"));
        }
        self.rpc()
            .await?
            .write(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
            .await?;
        Ok(Empty {})
    }
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        let rpc = self.rpc().await?;
        let data = if params.cursor.is_some() {
            vec![]
        } else {
            let created = rpc
                .call(
                    "session/new",
                    json!({"cwd":self.0.directory,"mcpServers":[]}),
                    Some(Duration::from_secs(90)),
                )
                .await?;
            let mut models = catalog(&created);
            if let Some(id) = created["sessionId"].as_str() {
                let known = rpc.model_settings.lock().await;
                for model in &mut models {
                    if let Some(settings) = known.get(&model.model.id) {
                        model.settings = Some(settings.clone());
                    }
                }
                drop(known);
                rpc.call(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
            }
            models
        };
        Ok(op::ModelPage {
            data,
            next_cursor: None,
            provider_errors: None,
        })
    }
    async fn model_settings(
        &self,
        model_id: &str,
    ) -> Result<agent_protocol::models::ModelSettings, Failure> {
        let rpc = self.rpc().await?;
        if let Some(settings) = rpc.model_settings.lock().await.get(model_id).cloned() {
            return Ok(settings);
        }
        let created = rpc
            .call(
                "session/new",
                json!({"cwd":self.0.directory,"mcpServers":[]}),
                Some(Duration::from_secs(90)),
            )
            .await?;
        let id = created["sessionId"]
            .as_str()
            .ok_or_else(|| failure("Model discovery returned no session"))?;
        let result = async {
            if !catalog(&created)
                .iter()
                .any(|model| model.model.id == model_id)
            {
                return Err(failure("The requested model is not available"));
            }
            let config = rpc
                .call(
                    "session/set_config_option",
                    json!({"sessionId":id,"configId":"model","value":model_id}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
            resolved_model_settings(model_id, &config)
        }
        .await;
        let closed = rpc
            .call(
                "session/close",
                json!({"sessionId":id}),
                Some(Duration::from_secs(30)),
            )
            .await;
        let settings = result?;
        closed?;
        rpc.model_settings
            .lock()
            .await
            .insert(model_id.to_owned(), settings.clone());
        Ok(settings)
    }
    async fn catalog(&self, cwd: &str) -> ComposerCatalog {
        ComposerCatalog {
            cwd: cwd.into(),
            ..Default::default()
        }
    }
    async fn discard_workspace_processes(&self, dir: &Path) -> Result<(), Failure> {
        let rpc = self.rpc().await?;
        let ids: Vec<_> = self
            .0
            .records
            .lock()
            .await
            .iter()
            .filter(|(_, r)| {
                r.thread
                    .cwd
                    .as_deref()
                    .is_some_and(|p| Path::new(p).starts_with(dir))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if rpc.loaded.lock().await.remove(&id) {
                rpc.call(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
            }
        }
        Ok(())
    }
    async fn read_permissions(&self) -> Result<PermissionSettings, Failure> {
        Ok(PermissionSettings {
            mode: None,
            version: "deepseek-native".into(),
        })
    }
    async fn update_permissions(
        &self,
        _: PermissionMode,
        _: &str,
    ) -> Result<PermissionSettings, Failure> {
        Err(Failure::new(
            "native_permissions",
            "Harness の権限設定を使用します。必要な操作はBEXの承認画面に表示されます。",
        ))
    }
    async fn fork(&self, _: &str, _: &str, _: Option<Value>) -> Result<ThreadResponse, Failure> {
        Err(unsupported())
    }
    async fn rename(&self, id: &str, name: &str) -> Result<Empty, Failure> {
        let mut records = self.0.records.lock().await;
        records
            .get_mut(id)
            .ok_or_else(|| failure("Unknown conversation"))?
            .thread
            .name = Some(name.into());
        self.save(&records).await?;
        drop(records);
        emit(
            &self.0.events,
            AgentChange::Renamed {
                session: session(id),
                name: Some(name.into()),
            },
        )
        .await
        .map_err(failure)?;
        Ok(Empty {})
    }
    fn event_stream(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.0.receiver.lock().unwrap().take()
    }
    async fn shutdown(&self) {
        if let Some(rpc) = self.0.rpc.lock().await.take() {
            let loaded: Vec<_> = rpc.loaded.lock().await.iter().cloned().collect();
            futures_util::future::join_all(loaded.into_iter().map(|id| {
                let rpc = rpc.clone();
                async move {
                    let _ = rpc
                        .call(
                            "session/close",
                            json!({"sessionId":id}),
                            Some(Duration::from_secs(30)),
                        )
                        .await;
                }
            }))
            .await;
            rpc.stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(15), rpc.stopped.cancelled()).await;
        }
        self.0.stop.cancel();
        let mut workers = self.0.workers.lock().await;
        while workers.join_next().await.is_some() {}
    }
}

async fn prompt(input: &[op::Input]) -> Result<Vec<Value>, Failure> {
    use base64::Engine;
    let mut prompt = vec![];
    for part in input {
        prompt.push(match part {
        op::Input::Text {text}=>json!({"type":"text","text":text}),
        op::Input::Mention {path,name}|op::Input::Skill {path,name}=>json!({"type":"text","text":format!("{name}: {path}")}),
        op::Input::LocalImage {path}=>{let bytes=tokio::fs::read(path).await.map_err(failure)?;let format=image::guess_format(&bytes).map_err(failure)?;json!({"type":"image","mimeType":format.to_mime_type(),"data":base64::engine::general_purpose::STANDARD.encode(bytes)})},
    });
    }
    Ok(prompt)
}
// NanoGPT is an OpenAI-compatible custom route, not an installed pi-ai catalog.
// Model IDs/capabilities: https://nano-gpt.com/api/v1/models?detailed=true
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[tokio::test]
    #[ignore = "requires an installed DeepSeek Harness and its supervisor companion"]
    async fn installed_harness_creates_selects_and_resumes_the_same_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        let mut events = adapter.event_stream().unwrap();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let Some(applied) = event.applied {
                    let _ = applied.send(Ok(()));
                }
            }
        });
        let models = adapter
            .models(&op::ListModels {
                limit: 100,
                cursor: None,
            })
            .await
            .unwrap();
        assert!(models.data.iter().any(|m| m.model.id == DEFAULT_MODEL));
        let created = adapter
            .create(dir.path().to_str().unwrap(), Some(DEFAULT_MODEL), None)
            .await
            .unwrap();
        let id = created.thread.id.as_ref().unwrap().id.clone();
        adapter.rename(&id, "Harness smoke check").await.unwrap();
        adapter.reset_rpc().await.unwrap();
        let rpc = adapter.rpc().await.unwrap();
        adapter.ensure_loaded(&rpc, &created).await.unwrap();
        assert!(rpc.loaded.lock().await.contains(&id));
        let reopened = adapter.open(&id, 100, true).await.unwrap();
        assert_eq!(reopened.thread.name.as_deref(), Some("Harness smoke check"));
        assert_eq!(reopened.model.unwrap().id, DEFAULT_MODEL);
        rpc.call(
            "session/close",
            json!({"sessionId":id}),
            Some(Duration::from_secs(30)),
        )
        .await
        .unwrap();
        adapter.shutdown().await;
    }

    async fn save_test_key(adapter: &DeepSeek, provider: KeyProvider, key: Option<&str>) {
        adapter
            .update_dsh_credential(KeyUpdate {
                provider,
                key: key.map(str::to_owned),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dsh_keys_rotate_independently_persist_and_never_appear_in_account_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("native-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("router-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenAI, Some("openai-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("rotated-secret")).await;
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        let usage = adapter.usage(ACCOUNT).await.unwrap();
        assert!(usage.windows.is_empty());
        assert!(usage.error.is_none());
        let keys = ManagedKeys::load(&adapter.0.directory).await.unwrap();
        assert_eq!(keys.get(KeyProvider::OpenRouter), Some("rotated-secret"));
        assert_eq!(keys.get(KeyProvider::OpenAI), Some("openai-secret"));
        assert_eq!(keys.get(KeyProvider::DeepSeek), Some("native-secret"));
        let credentials = adapter.dsh_credentials().await.unwrap();
        assert!(
            credentials
                .routes
                .iter()
                .any(|route| route.provider == KeyProvider::OpenRouter
                    && route.source == agent_protocol::dsh::CredentialSource::Managed)
        );
        assert!(
            !serde_json::to_string(&credentials)
                .unwrap()
                .contains("secret")
        );
        assert!(
            !serde_json::to_string(&Identity::list(&adapter).await.unwrap())
                .unwrap()
                .contains("secret")
        );
        save_test_key(&adapter, KeyProvider::OpenRouter, None).await;
        let keys = ManagedKeys::load(&adapter.0.directory).await.unwrap();
        assert!(keys.get(KeyProvider::OpenRouter).is_none());
        assert_eq!(keys.get(KeyProvider::OpenAI), Some("openai-secret"));
        save_test_key(&adapter, KeyProvider::OpenAI, None).await;
        save_test_key(&adapter, KeyProvider::DeepSeek, None).await;
        assert!(
            ManagedKeys::load(&adapter.0.directory)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_credential_updates_keep_independent_routes_and_do_not_leak_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            adapter.update_dsh_credential(KeyUpdate {
                provider: KeyProvider::DeepSeek,
                key: Some("native-secret".into())
            }),
            adapter.update_dsh_credential(KeyUpdate {
                provider: KeyProvider::OpenRouter,
                key: Some("router-secret".into())
            })
        );
        first.unwrap();
        second.unwrap();
        let credentials = adapter.dsh_credentials().await.unwrap();
        assert_eq!(credentials.routes.len(), 9);
        assert_eq!(
            credentials
                .routes
                .iter()
                .filter(|route| route.source == agent_protocol::dsh::CredentialSource::Managed)
                .count(),
            2
        );
        assert!(
            !serde_json::to_string(&credentials)
                .unwrap()
                .contains("secret")
        );
        let keys = ManagedKeys::load(&adapter.0.directory).await.unwrap();
        assert_eq!(keys.get(KeyProvider::DeepSeek), Some("native-secret"));
        assert_eq!(keys.get(KeyProvider::OpenRouter), Some("router-secret"));
    }

    #[tokio::test]
    async fn dsh_keys_reject_invalid_values_without_overwriting_existing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("keep-secret")).await;
        for key in [
            "".to_owned(),
            "has space".to_owned(),
            "bad\0key".to_owned(),
            "x".repeat(4097),
        ] {
            assert!(
                adapter
                    .update_key(KeyUpdate {
                        provider: KeyProvider::OpenRouter,
                        key: Some(key)
                    })
                    .await
                    .is_err()
            );
            assert_eq!(
                ManagedKeys::load(&adapter.0.directory)
                    .await
                    .unwrap()
                    .get(KeyProvider::OpenRouter),
                Some("keep-secret")
            );
        }
    }

    #[tokio::test]
    async fn misplaced_credentials_are_reported_as_typed_attention_without_exposing_keys() {
        use agent_protocol::dsh::CredentialSource;
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("native-secret")).await;
        let rejected = adapter
            .update_dsh_credential(KeyUpdate {
                provider: KeyProvider::DeepSeek,
                key: Some("sk-or-test-only".into()),
            })
            .await
            .unwrap_err();
        assert_eq!(rejected.code, "dsh_key_provider_mismatch");
        assert_eq!(
            ManagedKeys::load(&adapter.0.directory)
                .await
                .unwrap()
                .get(KeyProvider::DeepSeek),
            Some("native-secret")
        );
        let mut keys = ManagedKeys::load(&adapter.0.directory).await.unwrap();
        keys.set(KeyProvider::DeepSeek, Some("sk-or-misplaced"));
        crate::platform::save_private_json(&dir.path().join("dsh-keys.json"), &keys).unwrap();
        let error = adapter.validate_model_key(DEFAULT_MODEL).await.unwrap_err();
        assert_eq!(error.code, "dsh_key_provider_mismatch");
        adapter
            .validate_model_key(r#"["openrouter","deepseek/model"]"#)
            .await
            .unwrap();
        let credentials = adapter.dsh_credentials().await.unwrap();
        let route = credentials
            .routes
            .iter()
            .find(|route| route.provider == KeyProvider::DeepSeek)
            .unwrap();
        assert_eq!(route.source, CredentialSource::Managed);
        assert!(route.needs_attention);
        assert!(
            !serde_json::to_string(&credentials)
                .unwrap()
                .contains("sk-or-")
        );
        let accounts = Identity::list(&adapter).await.unwrap();
        assert_eq!(accounts.accounts[0].email.as_deref(), Some("DSH APIキー"));
        assert_eq!(accounts.accounts[0].plan_type, None);
    }

    #[tokio::test]
    #[ignore = "requires installed DeepSeek Harness; no inference or paid request"]
    async fn installed_harness_exposes_configured_openrouter_models() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("test-catalog-only")).await;
        let models = adapter
            .models(&op::ListModels {
                limit: 100,
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            models
                .data
                .iter()
                .any(|model| model.model.id.contains("openrouter"))
        );
        let selected = models
            .data
            .iter()
            .find(|model| {
                model.model.id.contains("openrouter")
                    && model.model.id.contains("deepseek-v4.1-flash")
            })
            .expect("OpenRouter DeepSeek route")
            .model
            .clone();
        let settings = adapter.model_settings(&selected.id).await.unwrap();
        assert!(!settings.supported_reasoning_efforts.is_empty());
        let models = adapter
            .models(&op::ListModels {
                limit: 100,
                cursor: None,
            })
            .await
            .unwrap();
        let router = models
            .data
            .iter()
            .find(|model| {
                model.model.id.contains("openrouter")
                    && model.model.id.contains("deepseek-v4.1-flash")
            })
            .unwrap();
        assert!(
            !router
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .is_empty(),
            "exact-model reasoning missing"
        );
        eprintln!(
            "routes={} deepseek_efforts={:?}",
            models.data.len(),
            router
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
        );
        let patch: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("acp.patch.json")).unwrap())
                .unwrap();
        assert!(!patch.to_string().contains("test-catalog-only"));
        adapter.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires installed Harness; catalog only, no inference"]
    async fn installed_harness_exposes_venice_models() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::Venice, Some("test-catalog-only")).await;
        let id = r#"["venice","qwen3-coder-480b-a35b-instruct-turbo"]"#;
        adapter.model_settings(id).await.unwrap();
        let page = adapter
            .models(&op::ListModels {
                limit: 100,
                cursor: None,
            })
            .await
            .unwrap();
        let model = page
            .data
            .iter()
            .find(|model| model.model.id == id)
            .expect("Venice route missing");
        assert_eq!(model.credential_route, Some(KeyProvider::Venice));
        assert_eq!(model.display_name, "Venice AI · Qwen 3 Coder 480B Turbo");
        adapter.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires installed Harness; catalog only, no inference"]
    async fn installed_harness_exposes_nanogpt_deepseek_and_native_efforts() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        save_test_key(&adapter, KeyProvider::NanoGpt, Some("test-catalog-only")).await;
        let selected = ModelRef {
            provider: PROVIDER,
            id: r#"["nanogpt","deepseek/deepseek-v4.1-flash"]"#.into(),
        };
        adapter.model_settings(&selected.id).await.unwrap();
        let page = adapter
            .models(&op::ListModels {
                limit: 100,
                cursor: None,
            })
            .await
            .unwrap();
        let model = page
            .data
            .iter()
            .find(|m| m.model == selected)
            .expect("NanoGPT route missing");
        assert_eq!(model.display_name, "NanoGPT · DeepSeek V4.1 Flash");
        assert!(
            model
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .iter()
                .any(|effort| effort.reasoning_effort == "high")
        );
        assert!(
            model
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .iter()
                .any(|effort| effort.reasoning_effort == "off")
        );
        eprintln!("nanogpt model={:?}", model);
        adapter.shutdown().await;
    }

    proptest! {
        #[test]
        fn assistant_chunks_preserve_unicode_and_interleaved_reasoning(parts in prop::collection::vec(".{0,60}",1..12)) {
            let mut turn=Turn {id:"turn".into(),status:TurnStatus::Running,items:Some(vec![]),..Default::default()};
            for part in &parts {
                let item=translate_update(&json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":part}}),&turn).unwrap();
                turn.items=Some(vec![Arc::new(item)]);
            }
            let item=turn.items.as_ref().unwrap().last().unwrap();
            let ItemBody::AssistantText {text,..}=item.body() else {prop_assert!(false);return Ok(());};
            prop_assert_eq!(text, &parts.concat());
            let thought=translate_update(&json!({"sessionUpdate":"agent_thought_chunk","content":{"text":"think"}}),&turn).unwrap();
            prop_assert_ne!(&thought.id,&item.id);
            prop_assert!(matches!(thought.body(),ItemBody::Reasoning {..}), "thought must remain separate");
        }
    }

    #[test]
    fn partial_tool_updates_keep_the_original_name_arguments_and_result() {
        let mut turn = Turn::default();
        let first=translate_update(&json!({"sessionUpdate":"tool_call","toolCallId":"tool","title":"Read","rawInput":{"path":"a.txt"},"status":"in_progress"}),&turn).unwrap();
        turn.items = Some(vec![Arc::new(first)]);
        let done=translate_update(&json!({"sessionUpdate":"tool_call_update","toolCallId":"tool","status":"completed","rawOutput":"ok","content":[{"type":"content","content":{"type":"text","text":"contents"}}]}),&turn).unwrap();
        assert_eq!(done.status, ItemStatus::Completed);
        let ItemBody::ToolCall {
            tool,
            arguments,
            result,
            success,
            content,
            ..
        } = done.body()
        else {
            panic!("tool call expected")
        };
        assert_eq!(tool, "Read");
        assert_eq!(arguments["path"], "a.txt");
        assert_eq!(result.as_ref().unwrap(), "ok");
        assert_eq!(*success, Some(true));
        assert_eq!(content.len(), 1);
        assert!(
            translate_update(&json!({"sessionUpdate":"config_option_update"}), &turn).is_none()
        );
    }

    #[tokio::test]
    async fn restarting_marks_running_turns_interrupted_and_preserves_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let record = ThreadResponse {
            thread: Thread {
                id: Some(session("native")),
                name: Some("Keep me".into()),
                status: SessionStatus::Running,
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: TurnStatus::Running,
                    ..Default::default()
                })]),
                ..Default::default()
            },
            model: Some(ModelRef {
                provider: PROVIDER,
                id: DEFAULT_MODEL.into(),
            }),
        };
        crate::platform::save_private_json(
            &dir.path().join("conversations.json"),
            &BTreeMap::from([("native", record)]),
        )
        .unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        let restored = adapter.open("native", 100, true).await.unwrap();
        assert_eq!(restored.thread.name.as_deref(), Some("Keep me"));
        assert_eq!(restored.thread.status, SessionStatus::Idle);
        assert_eq!(
            restored.thread.turns.as_ref().unwrap()[0].status,
            TurnStatus::Interrupted
        );
        assert_eq!(restored.model.unwrap().id, DEFAULT_MODEL);
    }

    #[tokio::test]
    async fn credentials_have_a_dedicated_contract_and_logout_clears_managed_routes() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load("dsh".into(), dir.path().into())
            .await
            .unwrap();
        assert!(adapter.account(AccountCommand::StartLogin).await.is_err());
        assert!(
            adapter
                .account(AccountCommand::SubmitLogin {
                    id: "fake".into(),
                    code: "test-secret".into()
                })
                .await
                .is_err()
        );
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("test-secret")).await;
        assert!(
            !serde_json::to_string(&Identity::list(&adapter).await.unwrap())
                .unwrap()
                .contains("test-secret")
        );
        adapter
            .account(AccountCommand::Logout { id: ACCOUNT.into() })
            .await
            .unwrap();
        assert!(
            ManagedKeys::load(&adapter.0.directory)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
