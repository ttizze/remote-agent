//! DeepSeek Harness owns inference, tools, credentials and native sessions.
//! BEX speaks its standard ACP v1 stdio interface and checkpoints display state.
use crate::providers::{
    AccountCommand, AccountReply, Agent, AgentChange, AgentEvent, AnswerSource, Failure, Identity,
    RequestOrigin, SessionListScope, SessionPage, SessionSummary, SubmissionState,
    SubmissionTarget, emit,
};
use agent_protocol::{
    composer::ComposerCatalog,
    ids::TurnId,
    models::*,
    operations as op,
    permissions::{PermissionMode, PermissionSettings},
    requests::{Answer, Choice, ChoiceEffect, Request, RequestBody, RequestTarget},
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
mod usage;
use credentials::ManagedKeys;
use model_catalog::{catalog, credential_route, provider_profile, resolved_model_settings};

const PROVIDER: ProviderKind = ProviderKind::DeepSeek;
const ACCOUNT: &str = "deepseek:native";
#[cfg(test)]
const DEFAULT_MODEL: &str = "[\"deepseek-official\",\"deepseek-flash\"]";
const INSTALL_DSH_HINT: &str = "HostにDSHをインストールするか、--deepseekで本体のパスを指定してください。APIキーの保存だけでは本体はインストールされません。";
type Pending = HashMap<u64, oneshot::Sender<Result<Value, String>>>;

#[derive(Clone)]
pub struct DeepSeek(Arc<Inner>);
struct Inner {
    program: PathBuf,
    directory: PathBuf,
    home: Option<PathBuf>,
    external_key: Option<zeroize::Zeroizing<String>>,
    records: Mutex<BTreeMap<String, ThreadResponse>>,
    rpc: Mutex<Option<Arc<Rpc>>>,
    events: mpsc::Sender<AgentEvent>,
    receiver: std::sync::Mutex<Option<mpsc::Receiver<AgentEvent>>>,
    stop: CancellationToken,
    workers: Mutex<tokio::task::JoinSet<()>>,
    credential_updates: Mutex<()>,
    usage: std::sync::Mutex<Arc<crate::account_usage::UsageEntry>>,
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
fn missing_program(label: &str, program: &Path, hint: &str) -> Failure {
    Failure::new(
        "provider_unavailable",
        format!(
            "{label}が見つからないか実行できません（{}）。{hint}",
            program.display()
        ),
    )
}
fn script_command(script: &Path, runtime: &Path) -> Result<tokio::process::Command, Failure> {
    if !script.is_file() {
        return Err(missing_program(
            "DeepSeek Harness（DSH）",
            script,
            INSTALL_DSH_HINT,
        ));
    }
    let runtime = which::which(runtime).map_err(|_| missing_program("DSH用のNode.js", runtime,
        "HostにNode.jsをインストールするか、AGENT_DEEPSEEK_NODEで実行ファイルを指定してください。"))?;
    let mut command = agent_process::command(&runtime)
        .map_err(|error| Failure::new("provider_unavailable", error))?;
    command.arg(script);
    Ok(command)
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
    pub async fn load(
        program: PathBuf,
        directory: PathBuf,
        home: Option<PathBuf>,
        external_key: Option<zeroize::Zeroizing<String>>,
    ) -> anyhow::Result<Self> {
        agent_local_host::create_state_directory(&directory)?;
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
            home,
            external_key: external_key.filter(|key| !key.trim().is_empty()),
            records: Mutex::new(records),
            rpc: Mutex::new(None),
            events,
            receiver: std::sync::Mutex::new(Some(receiver)),
            stop: CancellationToken::new(),
            workers: Mutex::new(tokio::task::JoinSet::new()),
            credential_updates: Mutex::new(()),
            usage: Default::default(),
        })))
    }
    fn command(&self) -> Result<tokio::process::Command, Failure> {
        let program = &self.0.program;
        let script = program
            .extension()
            .is_some_and(|e| e == "js" || e == "mjs")
            .then(|| program.clone());
        #[cfg(windows)]
        let script = script.or_else(|| {
            if program != Path::new("dsh") {
                return None;
            }
            let managed = directories::BaseDirs::new().map(|base| {
                base.data_local_dir()
                    .join("BEX/deepseek-harness/node_modules/@deepseek-ai/dsh/lib/bin.js")
            });
            let path = std::env::var_os("PATH").unwrap_or_default();
            managed
                .filter(|p| p.is_file())
                .into_iter()
                .chain(
                    std::env::split_paths(&path)
                        .map(|p| p.join("node_modules/@deepseek-ai/dsh/lib/bin.js")),
                )
                .find(|p| p.is_file())
        });
        if let Some(script) = script {
            let runtime = std::env::var_os("AGENT_DEEPSEEK_NODE");
            #[cfg(windows)]
            let runtime = runtime.or_else(|| {
                directories::BaseDirs::new()
                    .map(|base| {
                        base.data_local_dir()
                            .join("BEX/deepseek-harness/runtime/node.exe")
                    })
                    .filter(|p| p.is_file())
                    .map(|p| p.into_os_string())
            });
            let runtime = runtime.unwrap_or_else(|| "node".into());
            return script_command(&script, Path::new(&runtime));
        }
        let program = which::which(program)
            .map_err(|_| missing_program("DeepSeek Harness（DSH）", program, INSTALL_DSH_HINT))?;
        agent_process::command(&program)
            .map_err(|error| Failure::new("provider_unavailable", error))
    }
    async fn receive(
        owner: &std::sync::Weak<Inner>,
        reader: &Arc<Rpc>,
        value: &Value,
    ) -> Option<()> {
        if let Some(method) = value["method"].as_str() {
            let owner = owner.upgrade()?;
            let adapter = Self(owner);
            if method == "session/update" {
                adapter.update(&value["params"]).await.ok()?;
            } else if value.get("id").is_some() {
                if method == "session/request_permission" {
                    adapter.permission(reader, value).await.ok()?;
                } else {
                    let _ = reader.write(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Unsupported client method"}})).await;
                }
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
        Some(())
    }

    async fn rpc(&self) -> Result<Arc<Rpc>, Failure> {
        let mut slot = self.0.rpc.lock().await;
        if let Some(rpc) = slot.as_ref().filter(|rpc| !rpc.stop.is_cancelled()) {
            return Ok(rpc.clone());
        }
        let patch = self.0.directory.join("acp.patch.json");
        let keys = ManagedKeys::load(&self.0.directory).await?;
        let mut providers = serde_json::Map::new();
        for provider in KeyProvider::ALL {
            if provider != KeyProvider::DeepSeek && keys.get(provider).is_some() {
                for route in provider.routes() {
                    providers.insert((*route).into(), provider_profile(provider));
                }
            }
        }
        let mut overlays = vec![
            json!({"id":"acp","config":{"provider":"deepseek-official","model":"deepseek-flash"}}),
            json!({"id":"llm-pi-ai","config":{"providers":providers}}),
        ];
        if !keys.is_empty() {
            let credentials = self.0.directory.join("dsh-runtime-credentials.json");
            agent_local_host::save_private_json(&credentials, &keys).map_err(failure)?;
            overlays.push(json!({"id":"credentials","config":{"path":credentials}}));
        }
        agent_local_host::save_private_json(&patch, &overlays).map_err(failure)?;
        let mut command = self.command()?;
        command
            .args(["--profile", "acp", "--patch"])
            .arg(patch)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.env_remove("DEEPSEEK_API_KEY");
        if let Some(home) = &self.0.home {
            command.env("DSH_HOME", home);
        }
        if let Some(key) =
            keys.get(KeyProvider::DeepSeek)
                .or(self.0.external_key.as_deref().map(String::as_str))
        {
            command.env("DEEPSEEK_API_KEY", key);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().map_err(|e| {
            Failure::new(
                "provider_unavailable",
                format!(
                    "DeepSeek Harness を起動できません。HostのDSHとNode.jsの起動設定を確認してください: {e}"
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
                if DeepSeek::receive(&weak, &reader, &value).await.is_none() {
                    break;
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
        // Discovery can be cancelled before ACP initialization completes. The
        // reader owns the lifetime pipe, so cancellation must stop it as well.
        let initializing = rpc.stop.clone().drop_guard();
        let initialized = rpc.call("initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"bex","version":env!("CARGO_PKG_VERSION")}}), Some(Duration::from_secs(90))).await
            .map_err(|error| Failure::new("provider_unavailable", format!("DeepSeek Harnessの起動時にACP接続を確立できませんでした。HostのDSHとNode.jsの起動設定を確認してください: {error}")))?;
        if initialized["protocolVersion"] != 1 {
            return Err(failure("Harness requires ACP v1"));
        }
        *slot = Some(rpc.clone());
        initializing.disarm();
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
            || self.0.external_key.is_some()
            || self.external_file_configured().await)
    }
    async fn external_file_configured(&self) -> bool {
        if let Some(home) = &self.0.home {
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
        agent_local_host::save_private_json(&self.0.directory.join("conversations.json"), records)
            .map_err(failure)
    }
    async fn checkpoint(&self, id: &str, changes: &[SessionChange]) -> Result<(), Failure> {
        let mut records = self.0.records.lock().await;
        let record = records
            .get_mut(id)
            .ok_or_else(|| failure("Unknown DeepSeek conversation"))?;
        let mut thread = record.thread.clone();
        for change in changes {
            thread = change.apply(&thread).map_err(failure)?;
        }
        thread.updated_at = Some(now());
        record.thread = thread;
        self.save(&records).await
    }
    async fn change(&self, id: &str, change: SessionChange) -> Result<(), Failure> {
        self.checkpoint(id, std::slice::from_ref(&change)).await?;
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
            let key = keys.get(KeyProvider::DeepSeek).or(self
                .0
                .external_key
                .as_deref()
                .map(String::as_str));
            if key.is_some_and(|key| credentials::misplaced_key(KeyProvider::DeepSeek, key)) {
                return Err(Failure::new(
                    "dsh_key_provider_mismatch",
                    "OpenRouterのキーです。DeepSeek公式のAPIキーを保存するか、このキーを接続先「OpenRouter」に保存し直してください。",
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
            .filter_map(permission_choice)
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
            errors: Default::default(),
        })
    }
    async fn dsh_credentials(&self) -> Result<agent_protocol::dsh::Credentials, Failure> {
        Ok(ManagedKeys::load(&self.0.directory).await?.status(
            self.0.external_key.as_deref().map(String::as_str),
            self.external_file_configured().await,
        ))
    }
    async fn update_dsh_credential(
        &self,
        update: KeyUpdate,
    ) -> Result<agent_protocol::dsh::Credentials, Failure> {
        let _update = self.0.credential_updates.lock().await;
        self.update_key(update).await?;
        *self
            .0
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::default();
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
                *self
                    .0
                    .usage
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::default();
                Ok(AccountReply::Complete)
            }
            _ => Err(failure("Unknown DeepSeek account")),
        }
    }
    async fn usage(&self, _: &str) -> Result<op::AccountUsage, Failure> {
        let keys = ManagedKeys::load(&self.0.directory).await?;
        let Some(key) = keys.get(KeyProvider::OpenCode) else {
            return Ok(op::AccountUsage {
                windows: vec![],
                fetched_at: now() as i64,
                error: None,
            });
        };
        let cache = self
            .0
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(cache.read(usage::read(key)).await)
    }
}

#[async_trait::async_trait]
impl Agent for DeepSeek {
    fn data_recipient(&self) -> &'static str {
        "DeepSeek Harness（選択したAPI事業者：DeepSeek / OpenRouter / NanoGPT / OpenCode / OpenAI / Google / Anthropic / Groq / Mistral AI / xAI、およびモデル提供元）"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            steer: false,
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
        let search = search.to_lowercase();
        let records = self.0.records.lock().await;
        let mut data: Vec<_> = records
            .iter()
            .filter(|(id, r)| {
                !matches!(scope, SessionListScope::Descendants(_))
                    && cursor.as_ref().is_none_or(|c| *id > c)
                    && (search.is_empty()
                        || format!(
                            "{} {}",
                            r.thread.name.as_deref().unwrap_or_default(),
                            r.thread.preview.as_deref().unwrap_or_default()
                        )
                        .to_lowercase()
                        .contains(&search))
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
        let turns = record.thread.turns.as_deref().unwrap_or_default();
        let (turn, item) =
            agent_protocol::session::item_position(turns, &params.turn_id, &params.item_id)
                .ok_or_else(|| failure("Item not found"))?;
        let item = &turns[turn].items.as_ref().unwrap()[item];
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
        let credentials = self.dsh_credentials().await?;
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
        let selection = async {
            let selected = catalog(&created, &credentials.routes)
                .into_iter()
                .find(|candidate| match model {
                    Some(id) => candidate.model.id == id,
                    None => candidate.is_default == Some(true),
                })
                .ok_or_else(|| {
                    failure("設定済みの接続先から利用できるモデルを選択してください。")
                })?;
            self.select(&rpc, id, Some(&selected.model.id), None)
                .await?;
            Ok::<_, Failure>(selected.model)
        }
        .await;
        let model = match selection {
            Ok(model) => model,
            Err(error) => {
                let _ = rpc
                    .call(
                        "session/close",
                        json!({"sessionId":id}),
                        Some(Duration::from_secs(15)),
                    )
                    .await;
                return Err(error);
            }
        };
        rpc.loaded.lock().await.insert(id.into());
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
            model: Some(model),
            ..Default::default()
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
                "Harness cannot steer a running task",
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
        let started = [
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
            SessionChange::Status {
                status: SessionStatus::Running,
            },
        ];
        self.checkpoint(&id, &started).await?;
        // Admission must finish before waiting for Host event processing. Keep
        // startup ahead of ACP callbacks on the same ordered event queue.
        for change in started {
            self.0
                .events
                .send(AgentEvent {
                    change: AgentChange::Session {
                        session: session(&id),
                        change,
                    },
                    applied: None,
                })
                .await
                .map_err(failure)?;
        }
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
            let credentials = self.dsh_credentials().await?;
            let created = rpc
                .call(
                    "session/new",
                    json!({"cwd":self.0.directory,"mcpServers":[]}),
                    Some(Duration::from_secs(90)),
                )
                .await?;
            let mut models = catalog(&created, &credentials.routes);
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
            let credentials = self.dsh_credentials().await?;
            if !catalog(&created, &credentials.routes)
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
            self.unload(&id).await?;
        }
        Ok(())
    }
    async fn unload(&self, id: &str) -> Result<(), Failure> {
        let rpc = self.0.rpc.lock().await.clone();
        if let Some(rpc) = rpc {
            let loaded = rpc.loaded.lock().await.contains(id);
            if loaded {
                rpc.call(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
                rpc.loaded.lock().await.remove(id);
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
            "Harness の権限設定を使用します。必要な操作はBexの承認画面に表示されます。",
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
/// ACP option kinds say what each permission option does.
fn permission_choice(option: &Value) -> Option<Choice> {
    let effect = match option["kind"].as_str()? {
        "allow_once" => ChoiceEffect::Allow,
        "allow_always" => ChoiceEffect::AllowLasting,
        "reject_once" | "reject_always" => ChoiceEffect::Deny,
        _ => return None,
    };
    Some(Choice {
        id: option["optionId"].as_str()?.into(),
        label: option["name"].as_str().unwrap_or("Permission").into(),
        description: String::new(),
        effect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[tokio::test]
    async fn conversation_search_keeps_unicode_whitespace_paging_and_saved_history() {
        let root = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            root.path().join("missing-dsh"),
            root.path().join("records"),
            Some(root.path().join("home")),
            None,
        )
        .await
        .unwrap();
        let records = [
            ("a", Some("ALPHA"), Some("βeta")),
            ("b", Some("case"), Some("İSTANBUL")),
            ("c", None, None),
            ("d", Some("Alpha"), Some("tail")),
        ]
        .into_iter()
        .map(|(id, name, preview)| {
            (
                id.to_owned(),
                ThreadResponse {
                    thread: Thread {
                        id: Some(SessionRef {
                            provider: PROVIDER,
                            id: id.into(),
                        }),
                        name: name.map(str::to_owned),
                        preview: preview.map(str::to_owned),
                        cwd: Some(format!("/work/{id}")),
                        git_branch: Some(format!("branch-{id}")),
                        updated_at: Some(17.),
                        status: SessionStatus::Idle,
                        turns: Some(vec![Arc::new(Turn {
                            id: format!("turn-{id}").into(),
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
        *adapter.0.records.lock().await = records.clone();
        for scope in [SessionListScope::All, SessionListScope::Roots(&[])] {
            for (query, ids) in [
                ("", vec!["a", "b", "c", "d"]),
                (" ", vec!["a", "b", "c", "d"]),
                (" \t", vec![]),
                (" ALPHA ", vec![]),
                ("alpHA", vec!["a", "d"]),
                ("ALPHA βE", vec!["a"]),
                ("İSTANBUL", vec!["b"]),
                ("i\u{307}staN", vec!["b"]),
                ("missing", vec![]),
            ] {
                let result = Agent::list(&adapter, query, None, scope, 10).await.unwrap();
                let actual = result
                    .data
                    .into_iter()
                    .map(|row| {
                        assert!(row.branch.is_none());
                        assert!(row.thread.turns.is_none());
                        row.thread
                    })
                    .collect::<Vec<_>>();
                let expected = ids
                    .into_iter()
                    .map(|id| {
                        let mut thread = records[id].thread.clone();
                        thread.turns = None;
                        thread
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "query={query:?}");
                assert!(result.next_cursor.is_none());
            }
        }
        for (query, cursor, scope, limit, ids, next) in [
            (
                "",
                None,
                SessionListScope::Roots(&[]),
                2,
                vec!["a", "b"],
                Some("b"),
            ),
            (
                "",
                Some("b"),
                SessionListScope::All,
                2,
                vec!["c", "d"],
                None,
            ),
            ("", None, SessionListScope::All, 0, vec!["a"], Some("a")),
            (
                "alpha",
                Some("a"),
                SessionListScope::All,
                u32::MAX,
                vec!["d"],
                None,
            ),
            ("", Some("z"), SessionListScope::All, 2, vec![], None),
            (
                "",
                None,
                SessionListScope::Descendants("a"),
                2,
                vec![],
                None,
            ),
        ] {
            let result = Agent::list(&adapter, query, cursor.map(str::to_owned), scope, limit)
                .await
                .unwrap();
            assert_eq!(
                result
                    .data
                    .iter()
                    .map(|row| row.thread.id.as_ref().unwrap().id.as_str())
                    .collect::<Vec<_>>(),
                ids
            );
            assert_eq!(result.next_cursor.as_deref(), next);
        }
        for (id, record) in records {
            assert_eq!(
                Agent::open(&adapter, &id, usize::MAX, true).await.unwrap(),
                record
            );
        }
        adapter.shutdown().await;
    }

    #[test]
    fn permission_options_keep_what_each_acp_kind_does() {
        for (kind, effect) in [
            ("allow_once", Some(ChoiceEffect::Allow)),
            ("allow_always", Some(ChoiceEffect::AllowLasting)),
            ("reject_once", Some(ChoiceEffect::Deny)),
            ("reject_always", Some(ChoiceEffect::Deny)),
            ("future_kind", None),
        ] {
            let choice = permission_choice(&json!({"optionId":"option","name":"Name","kind":kind}));
            assert_eq!(choice.as_ref().map(|choice| choice.effect), effect);
            if let Some(choice) = choice {
                assert_eq!(
                    (choice.id.as_str(), choice.label.as_str()),
                    ("option", "Name")
                );
                assert!(choice.description.is_empty());
            }
        }
        assert_eq!(
            permission_choice(&json!({"name":"Name","kind":"allow_once"})),
            None
        );
    }

    #[tokio::test]
    async fn stalled_harness_catalog_returns_provider_errors_before_the_client_deadline() {
        use agent_protocol::protocol::{Call, Response};
        let root = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            root.path().join("missing-dsh"),
            root.path().join("dsh"),
            Some(root.path().join("home")),
            None,
        )
        .await
        .unwrap();
        // A concurrent initialization holds the same lock. Model discovery
        // must bound this wait too, not just the eventual ACP request.
        let _initializing = adapter.0.rpc.lock().await;
        let claude = crate::providers::Claude::load(
            root.path().join("missing-claude"),
            root.path().join("claude"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let service = crate::host_rpc::HostRpcService::new(
            [
                crate::providers::Backend::unavailable(ProviderKind::Codex, "offline"),
                claude.into(),
                adapter.clone().into(),
            ],
            crate::ProjectStore::new(root.path().join("worktrees.json")),
        );
        let session = service.open_session();
        tokio::time::pause();
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            service.dispatch(
                session.id(),
                &Call::ListModels(op::ListModels {
                    provider: agent_protocol::session::ProviderKind::DeepSeek,
                    limit: 100,
                    cursor: None,
                }),
            ),
        )
        .await
        .expect("one stalled provider must not exhaust the client deadline")
        .unwrap();
        let Response::Success { result: page } =
            agent_protocol::protocol::decode::<Response<op::ModelPage>>(&response.initial).unwrap()
        else {
            panic!("provider failures must remain part of the catalog");
        };
        assert!(page.data.is_empty());
        assert!(page.next_cursor.is_none());
        let errors = page.provider_errors.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors["deepseek"]["code"], "provider_timeout");
        assert!(!errors.contains_key("claude"));
    }

    #[tokio::test]
    async fn submission_is_checkpointed_before_receipt_without_waiting_for_event_acknowledgements()
    {
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("harness.js");
        std::fs::write(
            &program,
            r#"
const readline = require('node:readline');
readline.createInterface({input: process.stdin}).on('line', line => {
    const request = JSON.parse(line);
    const result = request.method === 'initialize'
        ? {protocolVersion: 1}
        : request.method === 'session/prompt' ? {stopReason: 'end_turn'} : {};
    process.stdout.write(JSON.stringify({jsonrpc: '2.0', id: request.id, result}) + '\n');
});
"#,
        )
        .unwrap();
        let adapter = DeepSeek::load(program, root.path().join("state"), None, None)
            .await
            .unwrap();
        let id = "existing-session";
        adapter.0.records.lock().await.insert(
            id.into(),
            ThreadResponse {
                thread: Thread {
                    id: Some(session(id)),
                    cwd: Some(root.path().to_str().unwrap().into()),
                    status: SessionStatus::Idle,
                    turns: Some(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let mut events = adapter.event_stream().unwrap();
        let input = op::Submission {
            thread_id: session(id),
            client_user_message_id: "existing-input".into(),
            input: vec![op::Input::Text {
                text: "Connection check".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        let receipt = tokio::time::timeout(
            Duration::from_secs(5),
            adapter.submit(
                &input,
                SubmissionTarget::Start {
                    cwd: root.path().to_str().unwrap(),
                },
                false,
                None,
            ),
        )
        .await
        .expect("admission must not wait for the Host's event consumer")
        .unwrap();
        let saved = adapter.open(id, 100, true).await.unwrap();
        assert_eq!(saved.thread.status, SessionStatus::Running);
        assert_eq!(
            saved.thread.turns.unwrap().last().unwrap().id,
            receipt.turn_id.unwrap()
        );
        let started = events.recv().await.unwrap();
        assert!(matches!(
            started.change,
            AgentChange::Session {
                change: SessionChange::Turn {
                    completed: false,
                    ..
                },
                ..
            }
        ));
        assert!(started.applied.is_none());
        let running = events.recv().await.unwrap();
        assert!(matches!(
            running.change,
            AgentChange::Session {
                change: SessionChange::Status {
                    status: SessionStatus::Running
                },
                ..
            }
        ));
        assert!(running.applied.is_none());
        for _ in 0..2 {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap();
            event.applied.unwrap().send(Ok(())).unwrap();
        }
        assert_eq!(
            adapter.open(id, 100, true).await.unwrap().thread.status,
            SessionStatus::Idle
        );
        adapter.shutdown().await;
    }

    #[tokio::test]
    async fn catalog_and_new_sessions_follow_credential_changes() {
        let dir = tempfile::tempdir().unwrap();
        let harness = dir.path().join("harness.js");
        std::fs::write(&harness, r#"
const readline = require('node:readline');
const {appendFileSync} = require('node:fs');
const models = [
    '["deepseek-official","deepseek-flash"]',
    '["opencode-go","deepseek-v4.1-flash"]',
    '["opencode","deepseek-v4.1-flash"]'
];
const config = currentValue => ({configOptions: [
    {id: 'model', currentValue, options: models.map(value => ({value}))},
    {id: 'reasoning_effort', currentValue: 'high', options: [{value: 'high'}]}
]});
let session = 0;
readline.createInterface({input: process.stdin}).on('line', line => {
    const request = JSON.parse(line);
    const key = process.env.DEEPSEEK_API_KEY;
    const environment = request.method === 'initialize' ? {
        home: process.env.DSH_HOME,
        key: key === undefined ? 'missing' : key === 'external-test-key' ? 'external' : key === 'managed-test-key' ? 'managed' : 'unexpected'
    } : undefined;
    appendFileSync(__dirname + '/calls.jsonl', JSON.stringify({...request, environment}) + '\n');
    let result = {};
    switch (request.method) {
        case 'initialize': result = {protocolVersion: 1}; break;
        case 'session/new': result = {sessionId: `session-${++session}`, ...config(models[0])}; break;
        case 'session/set_config_option': result = config(request.params.value); break;
    }
    process.stdout.write(JSON.stringify({jsonrpc: '2.0', id: request.id, result}) + '\n');
});
"#).unwrap();
        let adapter = DeepSeek::load(
            harness,
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        let params = op::ListModels {
            provider: PROVIDER,
            limit: 100,
            cursor: None,
        };
        let go = r#"["opencode-go","deepseek-v4.1-flash"]"#;
        let zen = r#"["opencode","deepseek-v4.1-flash"]"#;
        save_test_key(&adapter, KeyProvider::OpenCode, Some("catalog-test-key")).await;
        let models = adapter.models(&params).await.unwrap().data;
        assert_eq!(
            models
                .iter()
                .map(|m| m.model.id.as_str())
                .collect::<Vec<_>>(),
            [go, zen]
        );
        let created = adapter
            .create(dir.path().to_str().unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(created.model.unwrap().id, go);
        let calls = || -> Vec<Value> {
            std::fs::read_to_string(dir.path().join("calls.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        };
        let environment = || {
            calls()
                .into_iter()
                .rev()
                .find(|call| call["method"] == "initialize")
                .unwrap()["environment"]
                .clone()
        };
        assert_eq!(environment()["home"], json!(dir.path().join("home")));
        assert_eq!(environment()["key"], "missing");
        assert!(
            calls()
                .iter()
                .any(|call| call["method"] == "session/set_config_option"
                    && call["params"]["configId"] == "model"
                    && call["params"]["value"] == go),
            "the Harness must actually switch away from its unconfigured native default"
        );
        let before = calls().len();
        let error = adapter
            .create(dir.path().to_str().unwrap(), Some(DEFAULT_MODEL), None)
            .await
            .unwrap_err();
        assert!(error.message.contains("設定済みの接続先"));
        assert!(
            calls()[before..]
                .iter()
                .any(|call| call["method"] == "session/close"),
            "a rejected model must close the new native session"
        );
        assert_eq!(
            Agent::list(&adapter, "", None, SessionListScope::All, 100)
                .await
                .unwrap()
                .data
                .len(),
            1,
            "failed creation must not save a conversation"
        );
        assert!(
            adapter
                .model_settings(DEFAULT_MODEL)
                .await
                .unwrap_err()
                .message
                .contains("not available")
        );
        assert_eq!(
            adapter
                .model_settings(go)
                .await
                .unwrap()
                .default_reasoning_effort,
            "high"
        );

        save_test_key(&adapter, KeyProvider::OpenCode, None).await;
        assert!(adapter.models(&params).await.unwrap().data.is_empty());
        let before = calls().len();
        let error = adapter
            .create(dir.path().to_str().unwrap(), None, None)
            .await
            .unwrap_err();
        assert!(error.message.contains("設定済みの接続先"));
        assert!(
            calls()[before..]
                .iter()
                .any(|call| call["method"] == "session/close")
        );

        save_test_key(&adapter, KeyProvider::DeepSeek, Some("managed-test-key")).await;
        let models = adapter.models(&params).await.unwrap().data;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model.id, DEFAULT_MODEL);
        assert_eq!(models[0].is_default, Some(true));
        let created = adapter
            .create(dir.path().to_str().unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(created.model.unwrap().id, DEFAULT_MODEL);
        adapter.shutdown().await;

        let external = DeepSeek::load(
            dir.path().join("harness.js"),
            dir.path().into(),
            Some(dir.path().join("home")),
            Some(zeroize::Zeroizing::new("external-test-key".into())),
        )
        .await
        .unwrap();
        external.models(&params).await.unwrap();
        assert_eq!(
            environment()["key"],
            "managed",
            "the saved key overrides the external key in the actual child"
        );
        save_test_key(&external, KeyProvider::DeepSeek, None).await;
        external.models(&params).await.unwrap();
        assert_eq!(
            environment()["key"],
            "external",
            "the external key reaches the actual child after removing the saved key"
        );
        external.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires an installed DeepSeek Harness and its supervisor companion"]
    async fn installed_harness_creates_selects_and_resumes_the_same_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("test-catalog-only")).await;
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
                provider: agent_protocol::session::ProviderKind::DeepSeek,
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
    async fn opencode_allowance_cache_is_invalidated_when_credentials_change() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            dir.path().join("missing-dsh"),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        save_test_key(&adapter, KeyProvider::OpenCode, Some("first-test-key")).await;
        let first = adapter.0.usage.lock().unwrap().clone();
        let original = first
            .read(async {
                Ok(vec![
                    op::UsageWindow::from_used(
                        agent_protocol::operations::UsagePeriod::Minutes { minutes: 10080 },
                        Some("OpenCode Go".into()),
                        20.,
                        None,
                    )
                    .unwrap(),
                ])
            })
            .await;
        assert_eq!(adapter.usage(ACCOUNT).await.unwrap(), original);
        save_test_key(
            &adapter,
            KeyProvider::OpenCode,
            Some("replacement-test-key"),
        )
        .await;
        let replacement = adapter.0.usage.lock().unwrap().clone();
        assert!(!Arc::ptr_eq(&first, &replacement));
        let refreshed = replacement
            .read(async {
                Ok(vec![
                    op::UsageWindow::from_used(
                        agent_protocol::operations::UsagePeriod::Minutes { minutes: 10080 },
                        Some("OpenCode Go".into()),
                        55.,
                        None,
                    )
                    .unwrap(),
                ])
            })
            .await;
        assert_eq!(adapter.usage(ACCOUNT).await.unwrap(), refreshed);
        save_test_key(&adapter, KeyProvider::OpenCode, None).await;
        assert!(adapter.usage(ACCOUNT).await.unwrap().windows.is_empty());
    }

    #[tokio::test]
    async fn missing_harness_reports_installation_instead_of_a_disconnection_and_keeps_the_key() {
        let dir = tempfile::tempdir().unwrap();
        for program in ["not-installed-dsh", "not-installed-dsh.js"] {
            let adapter = DeepSeek::load(
                dir.path().join(program),
                dir.path().into(),
                Some(dir.path().join("home")),
                None,
            )
            .await
            .unwrap();
            save_test_key(&adapter, KeyProvider::OpenCode, Some("test-key")).await;
            let error = adapter
                .models(&op::ListModels {
                    provider: PROVIDER,
                    limit: 100,
                    cursor: None,
                })
                .await
                .unwrap_err();
            assert_eq!(error.code, "provider_unavailable");
            assert!(error.message.contains("DSH"));
            assert!(error.message.contains("--deepseek"));
            assert!(!error.message.contains("disconnected"));
            assert!(!error.message.contains("test-key"));
            assert_eq!(
                ManagedKeys::load(dir.path())
                    .await
                    .unwrap()
                    .get(KeyProvider::OpenCode),
                Some("test-key")
            );
        }
    }

    #[test]
    fn missing_node_reports_the_runtime_without_claiming_the_harness_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("harness.js");
        std::fs::write(&script, "").unwrap();
        let error = script_command(&script, &dir.path().join("missing-node")).unwrap_err();
        assert_eq!(error.code, "provider_unavailable");
        assert!(error.message.contains("Node.js"));
        assert!(error.message.contains("AGENT_DEEPSEEK_NODE"));
        assert!(!error.message.contains("DSHをインストール"));
    }

    #[tokio::test]
    async fn opencode_key_enables_both_routes_and_removal_preserves_other_providers() {
        let dir = tempfile::tempdir().unwrap();
        // Even a failed launch must leave a regenerable, secret-free profile.
        let adapter = DeepSeek::load(
            dir.path().join("missing-dsh"),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("router-test-key")).await;
        save_test_key(&adapter, KeyProvider::OpenCode, Some("opencode-test-key")).await;
        assert!(adapter.rpc().await.is_err());
        let patch = dir.path().join("acp.patch.json");
        let profile: Value = serde_json::from_slice(&std::fs::read(&patch).unwrap()).unwrap();
        assert_eq!(
            profile[1]["config"]["providers"],
            json!({
                "opencode-go":{"apiKeyEnv":"OPENCODE_API_KEY"},
                "opencode":{"apiKeyEnv":"OPENCODE_API_KEY"},
                "openrouter":{"apiKeyEnv":"OPENROUTER_API_KEY"}
            })
        );
        assert!(!profile.to_string().contains("test-key"));
        let keys = ManagedKeys::load(dir.path()).await.unwrap();
        assert_eq!(keys.get(KeyProvider::OpenCode), Some("opencode-test-key"));
        save_test_key(&adapter, KeyProvider::OpenCode, None).await;
        assert!(adapter.rpc().await.is_err());
        let profile: Value = serde_json::from_slice(&std::fs::read(&patch).unwrap()).unwrap();
        assert_eq!(
            profile[1]["config"]["providers"],
            json!({"openrouter":{"apiKeyEnv":"OPENROUTER_API_KEY"}})
        );
        let keys = ManagedKeys::load(dir.path()).await.unwrap();
        assert!(keys.get(KeyProvider::OpenCode).is_none());
        assert_eq!(keys.get(KeyProvider::OpenRouter), Some("router-test-key"));
    }

    #[tokio::test]
    async fn dsh_keys_rotate_independently_persist_and_never_appear_in_account_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("native-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("router-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenAI, Some("openai-secret")).await;
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("rotated-secret")).await;
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
        assert_eq!(credentials.routes.len(), KeyProvider::ALL.len());
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
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            Some(zeroize::Zeroizing::new("sk-or-external-test-key".into())),
        )
        .await
        .unwrap();
        let external = adapter.dsh_credentials().await.unwrap();
        let official = external
            .routes
            .iter()
            .find(|route| route.provider == KeyProvider::DeepSeek)
            .unwrap();
        assert_eq!(official.source, CredentialSource::External);
        assert!(official.needs_attention);
        assert_eq!(
            adapter
                .validate_model_key(DEFAULT_MODEL)
                .await
                .unwrap_err()
                .code,
            "dsh_key_provider_mismatch"
        );
        save_test_key(&adapter, KeyProvider::DeepSeek, Some("native-secret")).await;
        adapter.validate_model_key(DEFAULT_MODEL).await.unwrap();
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
        agent_local_host::save_private_json(&dir.path().join("dsh-keys.json"), &keys).unwrap();
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
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
        .await
        .unwrap();
        save_test_key(&adapter, KeyProvider::OpenRouter, Some("test-catalog-only")).await;
        let models = adapter
            .models(&op::ListModels {
                provider: agent_protocol::session::ProviderKind::DeepSeek,
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
                provider: agent_protocol::session::ProviderKind::DeepSeek,
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
    async fn installed_harness_exposes_nanogpt_deepseek_and_native_efforts() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
                provider: agent_protocol::session::ProviderKind::DeepSeek,
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
            ..Default::default()
        };
        agent_local_host::save_private_json(
            &dir.path().join("conversations.json"),
            &BTreeMap::from([("native", record)]),
        )
        .unwrap();
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
        let adapter = DeepSeek::load(
            "dsh".into(),
            dir.path().into(),
            Some(dir.path().join("home")),
            None,
        )
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
