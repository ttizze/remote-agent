//! Claude Code owns inference, credentials and its transcript. The Host owns
//! the client-facing conversation and adapts the CLI's streaming protocol.
mod history;
mod process;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::{
    models::{Item, Model, ReasoningEffort, Thread, ThreadResponse, ThreadStatus, Turn},
    peer::RpcMessage,
    session::{ProviderKind, SessionChange, TextField},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::host_rpc::routing::SessionRouter;
use process::Process;

pub(crate) const MODEL_PREFIX: &str = "claude:";

pub(crate) struct Claude {
    program: PathBuf,
    directory: PathBuf,
    native_home: PathBuf,
    records: AsyncMutex<HashMap<String, Arc<AsyncMutex<Record>>>>,
    models: OnceCell<Vec<Model>>,
    processes: Arc<Semaphore>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    router: SessionRouter,
    stop: CancellationToken,
    workers: AsyncMutex<tokio::task::JoinSet<()>>,
}

struct Record {
    cwd: String,
    model: String,
    session_id: Uuid,
    resumable: bool,
    running: Option<Running>,
    idle: Option<Idle>,
}

struct Idle {
    process: Process,
    model: String,
    effort: Option<String>,
    released: CancellationToken,
}

struct Running {
    turn_id: String,
    input: mpsc::Sender<Command>,
    interrupt: watch::Receiver<Option<Result<(), String>>>,
}

struct Command {
    value: Value,
    delivered: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

struct Pending {
    thread_id: String,
    request_id: String,
    input: Value,
    sender: mpsc::Sender<Command>,
}

impl Drop for Claude {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Claude {
    pub(crate) async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
        router: SessionRouter,
    ) -> Result<Self, String> {
        crate::platform::create_state_directory(&directory).map_err(|error| error.to_string())?;
        let native_home = native_home.map(Ok).unwrap_or_else(history::home)?;
        Ok(Self {
            program,
            directory,
            native_home,
            records: AsyncMutex::new(HashMap::new()),
            models: OnceCell::new(),
            processes: Arc::new(Semaphore::new(8)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            router,
            stop: CancellationToken::new(),
            workers: AsyncMutex::new(tokio::task::JoinSet::new()),
        })
    }

    pub(crate) async fn shutdown(&self) {
        self.stop.cancel();
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                agent_core::diagnostics::error("claude.worker", &error.to_string());
            }
        }
    }

    pub(crate) async fn models(&self) -> Result<&[Model], String> {
        let available = if self.program.components().count() > 1 {
            self.program.is_file()
        } else {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path)
                    .any(|directory| directory.join(&self.program).is_file())
            })
        };
        if !available {
            return Ok(&[]);
        }
        self.models
            .get_or_try_init(|| async {
                let cwd =
                    tempfile::tempdir_in(&self.directory).map_err(|error| error.to_string())?;
                let (process, initialized) = Process::start(
                    &self.program,
                    &self.native_home,
                    cwd.path(),
                    None,
                    None,
                    None,
                )
                .await?;
                process.finish().await?;
                let entries = initialized["models"]
                    .as_array()
                    .ok_or("Claude Code did not return a model catalog")?;
                entries
                    .iter()
                    .map(|entry| {
                        let name = entry["value"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .ok_or("Claude model has no value")?;
                        let display = entry["displayName"]
                            .as_str()
                            .ok_or("Claude model has no display name")?;
                        let values = match entry.get("supportedEffortLevels") {
                            Some(value) => value
                                .as_array()
                                .ok_or("invalid Claude effort levels")?
                                .as_slice(),
                            None => &[],
                        };
                        let efforts = values
                            .iter()
                            .map(|value| {
                                Ok(ReasoningEffort {
                                    reasoning_effort: value
                                        .as_str()
                                        .ok_or("invalid Claude effort level")?
                                        .into(),
                                    extra: Default::default(),
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        let default = efforts
                            .iter()
                            .find(|effort| effort.reasoning_effort == "high")
                            .or(efforts.first())
                            .map(|effort| effort.reasoning_effort.clone())
                            .unwrap_or_default();
                        let model = format!("{MODEL_PREFIX}{name}");
                        Ok(Model {
                            id: model.clone(),
                            model,
                            display_name: format!("Claude · {display}"),
                            default_reasoning_effort: default,
                            supported_reasoning_efforts: efforts,
                            service_tiers: Some(Vec::new()),
                            default_service_tier: None,
                            is_default: Some(false),
                            extra: Default::default(),
                        })
                    })
                    .collect::<Result<Vec<Model>, String>>()
            })
            .await
            .map(Vec::as_slice)
    }

    pub(crate) async fn create(&self, cwd: &str, model: &str) -> Result<ThreadResponse, String> {
        if !self
            .models()
            .await?
            .iter()
            .any(|entry| entry.model == model)
        {
            return Err("このClaudeモデルは利用できません。モデル一覧を更新してください。".into());
        }
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|error| error.to_string())?;
        if !cwd.is_dir() {
            return Err("Claudeの作業フォルダがありません。".into());
        }
        let session_id = Uuid::new_v4();
        let id = format!("claude:{session_id}");
        let response = ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                status: Some(status("idle")),
                turns: Some(Vec::new()),
                created_at: Some(now().into()),
                updated_at: Some(now().into()),
                ..Default::default()
            },
            model: Some(model.into()),
            extra: Default::default(),
        };
        let record = Record {
            cwd: cwd.to_string_lossy().into_owned(),
            model: model.into(),
            session_id,
            resumable: false,
            running: None,
            idle: None,
        };
        self.retain_record(id, Arc::new(AsyncMutex::new(record)))
            .await?;
        self.router.created_session(response.clone());
        Ok(response)
    }

    async fn record(&self, id: &str) -> Result<Arc<AsyncMutex<Record>>, String> {
        if let Some(record) = self.records.lock().await.get(id).cloned() {
            return Ok(record);
        }
        let native_id = Uuid::parse_str(
            id.strip_prefix(MODEL_PREFIX)
                .ok_or("invalid Claude session ID")?,
        )
        .map_err(|_| "invalid Claude session ID")?;
        let home = self.native_home.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native_id)?;
            history::summary(&path)
        })
        .await
        .map_err(|e| e.to_string())??;
        let record = Arc::new(AsyncMutex::new(Record {
            cwd: summary
                .cwd
                .ok_or("Claude working directory is unavailable")?,
            model: "claude:default".into(),
            session_id: native_id,
            resumable: true,
            running: None,
            idle: None,
        }));
        self.retain_record(id.into(), record).await
    }

    async fn retain_record(
        &self,
        id: String,
        record: Arc<AsyncMutex<Record>>,
    ) -> Result<Arc<AsyncMutex<Record>>, String> {
        let mut records = self.records.lock().await;
        if let Some(existing) = records.get(&id) {
            return Ok(existing.clone());
        }
        if records.len() >= 128 {
            records.retain(|_, record| {
                Arc::strong_count(record) > 1
                    || record.try_lock().map_or(true, |record| {
                        record.running.is_some() || record.idle.is_some()
                    })
            });
        }
        if records.len() >= 128 {
            return Err(
                "Claude session capacity reached; wait for an active task to finish".into(),
            );
        }
        records.insert(id, record.clone());
        Ok(record)
    }

    pub(crate) fn storage_directory(&self) -> &Path {
        &self.native_home
    }

    pub(crate) async fn list(&self, search: &str) -> Result<Vec<Thread>, String> {
        let home = self.native_home.clone();
        let mut threads = tokio::task::spawn_blocking(move || {
            history::files(&home)?
                .into_iter()
                .map(|path| {
                    Ok(history::summary(&path).unwrap_or_else(|error| {
                        let mut thread = Thread {
                            id: path
                                .file_stem()
                                .and_then(|id| id.to_str())
                                .map(|id| format!("claude:{id}")),
                            name: Some("Claude履歴を読み取れません".into()),
                            status: Some(status("notLoaded")),
                            ..Default::default()
                        };
                        thread.extra.insert(
                            "historyReadState".into(),
                            json!({"type":"unavailable","issues":[error]}),
                        );
                        thread
                    }))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .await
        .map_err(|e| e.to_string())??;
        let records: Vec<_> = self.records.lock().await.values().cloned().collect();
        for record in records {
            let record = record.lock().await;
            let id = format!("claude:{}", record.session_id);
            let Some(mut thread) = self
                .router
                .current_session(&id)
                .map(|response| response.thread)
            else {
                continue;
            };
            thread.turns = None;
            if let Some(current) = threads.iter_mut().find(|current| current.id == thread.id) {
                *current = thread;
            } else if record.running.is_some() {
                threads.push(thread);
            }
        }
        let search = search.trim().to_lowercase();
        threads.retain(|thread| {
            search.is_empty()
                || thread
                    .name
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&search)
                || thread
                    .preview
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&search)
        });
        threads.sort_by(|left, right| {
            updated_at(right)
                .cmp(&updated_at(left))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(threads)
    }

    /// Read native history; cache selection belongs to SessionRuntime.
    pub(crate) async fn read(&self, id: &str, requested: usize) -> Result<ThreadResponse, String> {
        let current = self.router.current_session(id);
        let native = Uuid::parse_str(
            id.strip_prefix(MODEL_PREFIX)
                .ok_or("invalid Claude session ID")?,
        )
        .map_err(|e| e.to_string())?;
        let home = self.native_home.clone();
        let native: Result<ThreadResponse, String> = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            match history::read(&path, requested) {
                Ok(response) => Ok(response),
                Err(error) => {
                    let mut thread = history::summary(&path)?;
                    thread.extra.insert(
                        "historyReadState".into(),
                        json!({"type":"unavailable","issues":[error]}),
                    );
                    Ok(ThreadResponse {
                        thread,
                        model: None,
                        extra: Default::default(),
                    })
                }
            }
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(match native {
            Ok(mut response) => {
                if let Some(current) = &current {
                    for turn in current
                        .thread
                        .turns
                        .iter()
                        .flatten()
                        .filter(|turn| turn.status.as_deref() == Some("inProgress"))
                    {
                        response.thread = agent_core::session::SessionChange::Turn {
                            turn: (**turn).clone(),
                            completed: false,
                        }
                        .apply(&response.thread)
                        .map_err(str::to_owned)?;
                    }
                }
                response
            }
            Err(error) => current
                .clone()
                .filter(|response| {
                    response
                        .thread
                        .turns
                        .iter()
                        .flatten()
                        .any(|turn| turn.status.as_deref() == Some("inProgress"))
                })
                .ok_or(error)?,
        })
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = params["threadId"].as_str().ok_or("threadId is required")?;
        if matches!(
            method,
            "thread/resume" | "host/thread/resume" | "host/thread/item/read"
        ) {
            let requested = params["historyLimit"].as_u64().unwrap_or(1000) as usize;
            let mut response = self.read(id, requested).await?;
            if method == "host/thread/item/read" {
                let item = response
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .find(|turn| Some(turn.id.as_str()) == params["turnId"].as_str())
                    .and_then(|turn| turn.items.as_ref())
                    .into_iter()
                    .flatten()
                    .find(|item| Some(item.id.as_str()) == params["itemId"].as_str())
                    .ok_or("Claude native history item is unavailable")?;
                let mut item = (**item).clone();
                if item.extra.get("detailDeferred") == Some(&Value::Bool(true)) {
                    return Err(
                        "full native item is not yet available; retry after provider persistence"
                            .into(),
                    );
                }
                if let Some(path) = item.extra.get("detailFile").and_then(Value::as_str) {
                    use tokio::io::AsyncReadExt;
                    let file = tokio::fs::File::open(path)
                        .await
                        .map_err(|error| format!("native tool output is unavailable: {error}"))?;
                    let mut bytes = Vec::new();
                    file.take(4 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|error| error.to_string())?;
                    if bytes.len() > 4 * 1024 * 1024 {
                        return Err("native tool output exceeds detail byte limit".into());
                    }
                    item.result = Some(Value::String(
                        String::from_utf8(bytes).map_err(|_| "native tool output is not UTF-8")?,
                    ));
                }
                if let Some(agent_id) = item.extra.get("agentId").and_then(Value::as_str) {
                    let home = self.native_home.clone();
                    let session_id =
                        Uuid::parse_str(id.strip_prefix(MODEL_PREFIX).ok_or("invalid Claude ID")?)
                            .map_err(|_| "invalid Claude ID")?;
                    let agent_id = agent_id.to_owned();
                    let related = tokio::task::spawn_blocking(move || {
                        history::read_related(&home, session_id, &agent_id, 1000)
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                    item.result = Some(match related {
                        Ok(response) => json!({"output":item.result,"subagent":response.thread}),
                        Err(error) => {
                            json!({"output":item.result,"subagentHistory":{"type":"unavailable","message":error}})
                        }
                    });
                }
                if serde_json::to_vec(&item)
                    .map_err(|error| error.to_string())?
                    .len()
                    > 4 * 1024 * 1024
                {
                    return Err("native item exceeds detail byte limit".into());
                }
                return Ok(json!({"item": item}));
            }
            if params["includeTurns"] == false {
                response.thread.turns = None;
            }
            if params["deferItemDetails"] == true {
                response.thread.defer_item_details();
            }
            return serde_json::to_value(response).map_err(|e| e.to_string());
        }
        let record = self.record(id).await?;
        match method {
            "turn/start" => self.start_turn(id, record, &params).await,
            "turn/interrupt" => {
                let (input, mut interrupt) = {
                    let record = record.lock().await;
                    let running = record
                        .running
                        .as_ref()
                        .ok_or("Claudeは実行中ではありません。")?;
                    if params["turnId"].as_str() != Some(running.turn_id.as_str()) {
                        return Err(
                            "Claudeの実行対象が変わりました。会話を更新してください。".into()
                        );
                    }
                    (running.input.clone(), running.interrupt.clone())
                };
                if interrupt.borrow().is_none() {
                    input.send(Command { value: json!({"type":"control_request","request_id":"interrupt","request":{"subtype":"interrupt"}}), delivered: None }).await.map_err(|_| "Claude Code input is closed")?;
                }
                tokio::time::timeout(std::time::Duration::from_secs(15), async {
                    loop {
                        if let Some(result) = interrupt.borrow().clone() {
                            return result;
                        }
                        interrupt
                            .changed()
                            .await
                            .map_err(|_| "Claude Code exited before acknowledging interruption")?;
                    }
                })
                .await
                .map_err(|_| "Claude Codeの停止要求がタイムアウトしました。")??;
                Ok(json!({}))
            }
            "turn/steer" | "thread/queue/add" => Err(
                "Claudeの実行中は追加送信できません。完了を待つか、停止してから送信してください。"
                    .into(),
            ),
            _ => Err(format!("Claude Codeでは {method} に対応していません。")),
        }
    }

    async fn start_turn(
        &self,
        id: &str,
        record: Arc<AsyncMutex<Record>>,
        params: &Value,
    ) -> Result<Value, String> {
        for field in [
            "model",
            "effort",
            "serviceTierForTurn",
            "clientUserMessageId",
        ] {
            if !params[field].is_null() && !params[field].is_string() {
                return Err(format!("{field} must be a string"));
            }
        }
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        let content = input_content(&params["input"]).await?;
        let mut state = record.lock().await;
        if state.running.is_some() {
            return Err("Claudeはすでに実行中です。".into());
        }
        let model = params["model"]
            .as_str()
            .or(Some(state.model.as_str()))
            .ok_or("Claude model is required")?;
        let model_name = model
            .strip_prefix(MODEL_PREFIX)
            .ok_or("Codexへ切り替える場合は新しい会話を作成してください。")?;
        let model_name = model_name.to_owned();
        let model = model.to_owned();
        let models = self.models().await?;
        let selected = models
            .iter()
            .find(|entry| entry.model == model)
            .ok_or("Claude model is no longer available")?;
        let effort = params["effort"].as_str();
        if effort.is_some_and(|effort| {
            !selected
                .supported_reasoning_efforts
                .iter()
                .any(|entry| entry.reasoning_effort == effort)
        }) {
            return Err("このClaudeモデルは選択した思考強度に対応していません。".into());
        }
        // Core's "default" means the backend's normal service. Claude has no
        // equivalent of Codex's explicit priority/flex tiers.
        if params["serviceTierForTurn"]
            .as_str()
            .is_some_and(|tier| tier != "default")
        {
            return Err("ClaudeではCodexのサービス階層を指定できません。".into());
        }
        let session = state.session_id.to_string();
        let cwd = state.cwd.clone();
        let idle = state.idle.take();
        let mut process = if idle
            .as_ref()
            .is_some_and(|idle| idle.model == model && idle.effort.as_deref() == effort)
        {
            let idle = idle.unwrap();
            idle.released.cancel();
            idle.process
        } else {
            if let Some(idle) = idle {
                idle.released.cancel();
                idle.process.finish().await?;
            }
            let permit = self.processes.clone().try_acquire_owned()
                .map_err(|_| "Claude process capacity reached (8); wait for an active or retained session to finish")?;
            let (mut process, initialized) = Process::start(
                &self.program,
                &self.native_home,
                Path::new(&cwd),
                Some((&session, state.resumable)),
                Some(&model_name),
                effort,
            )
            .await?;
            process.retain_capacity(permit);
            if !initialized["account"]["subscriptionType"]
                .as_str()
                .is_some_and(|plan| !plan.is_empty())
            {
                process.finish().await?;
                return Err("Claudeのサブスク認証がありません。Hostの端末で claude auth login を実行してください。".into());
            }
            process
        };
        let turn_id = Uuid::new_v4().to_string();
        let user: Item = serde_json::from_value(json!({"id":Uuid::new_v4().to_string(),"type":"userMessage","content":params["input"],"clientId":params["clientUserMessageId"]})).map_err(|error| error.to_string())?;
        let turn = Turn {
            id: turn_id.clone(),
            status: Some("inProgress".into()),
            items: Some(vec![Arc::new(user)]),
            started_at: Some(Some(now().into())),
            ..Default::default()
        };
        if let Err(error) = process.write(&json!({"type":"user","uuid":turn_id,"session_id":session,"message":{"role":"user","content":content},"parent_tool_use_id":null})).await {
            return Err(format!("submission outcome unknown: {error}"));
        }
        state.model = model.clone();
        let (input, receiver) = mpsc::channel(32);
        let (interrupt, interrupted) = watch::channel(None);
        state.running = Some(Running {
            turn_id: turn_id.clone(),
            input: input.clone(),
            interrupt: interrupted,
        });
        drop(state);
        self.router.session_change(
            id,
            SessionChange::Turn {
                turn,
                completed: false,
            },
        );
        let worker = Worker {
            record,
            router: self.router.clone(),
            pending: self.pending.clone(),
            thread_id: id.into(),
            turn_id: turn_id.clone(),
            input,
            stop: self.stop.child_token(),
            stream: HashMap::new(),
            interrupt,
            model,
            effort: effort.map(str::to_owned),
        };
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                agent_core::diagnostics::error("claude.worker", &error.to_string());
            }
        }
        workers.spawn(worker.run(process, receiver));
        Ok(json!({"turn":{"id":turn_id}}))
    }

    pub(crate) async fn respond(&self, message: &RpcMessage<'_>) -> Result<bool, String> {
        let Some(raw_id) = message.raw_id() else {
            return Ok(false);
        };
        let id: String = serde_json::from_str(raw_id).map_err(|error| error.to_string())?;
        let pending = self.pending.lock().unwrap().remove(&id);
        let Some(pending) = pending else {
            return Ok(false);
        };
        let result: Value = message
            .raw_result()
            .map(|raw| serde_json::from_str(raw.get()))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or(Value::Null);
        let response = if result["decision"] == "accept" {
            json!({"behavior":"allow","updatedInput":pending.input})
        } else if let Some(answers) = result["answers"].as_object() {
            let mut input = pending.input;
            let answers: serde_json::Map<_, _> = answers
                .iter()
                .map(|(question, answer)| {
                    (
                        question.clone(),
                        Value::String(
                            answer["answers"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    )
                })
                .collect();
            input["answers"] = Value::Object(answers);
            json!({"behavior":"allow","updatedInput":input})
        } else {
            json!({"behavior":"deny","message":"ユーザーがこの操作を拒否しました。"})
        };
        let (delivered, receipt) = tokio::sync::oneshot::channel();
        pending.sender.send(Command { value: json!({"type":"control_response","response":{"subtype":"success","request_id":pending.request_id,"response":response}}), delivered: Some(delivered) }).await.map_err(|_| "Claude Code input is closed")?;
        tokio::time::timeout(std::time::Duration::from_secs(15), receipt)
            .await
            .map_err(|_| "Claude answer delivery is unknown")?
            .map_err(|_| "Claude exited before confirming the answer write")??;
        self.router
            .resolve_native_request(ProviderKind::Claude, &id.into());
        Ok(true)
    }
}

struct Worker {
    record: Arc<AsyncMutex<Record>>,
    router: SessionRouter,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    thread_id: String,
    turn_id: String,
    input: mpsc::Sender<Command>,
    stop: CancellationToken,
    stream: HashMap<String, (String, usize)>,
    interrupt: watch::Sender<Option<Result<(), String>>>,
    model: String,
    effort: Option<String>,
}

impl Worker {
    fn current_turn(&self) -> Option<Turn> {
        self.router
            .current_session(&self.thread_id)?
            .thread
            .turns?
            .iter()
            .rfind(|turn| turn.id == self.turn_id)
            .map(|turn| (**turn).clone())
    }

    async fn run(mut self, mut process: Process, mut input: mpsc::Receiver<Command>) {
        let mut interrupted = false;
        let outcome = async {
            loop {
                tokio::select! {
                    _ = self.stop.cancelled() => {
                        interrupted = true;
                        process.write(&json!({"type":"control_request","request_id":"shutdown","request":{"subtype":"interrupt"}})).await?;
                        return Ok(());
                    }
                    command = input.recv() => {
                        let command = command.ok_or("Claude input queue is closed")?;
                        let result = process.write(&command.value).await;
                        if let Some(delivered) = command.delivered { let _ = delivered.send(result.clone()); }
                        result?;
                    }
                    message = process.read() => {
                        let message = message?.ok_or("Claude Code exited without a result")?;
                        if message["type"] == "result" {
                            if message["is_error"] == true {
                                return Err(message["result"].as_str().map(str::to_owned)
                                    .or_else(|| message["errors"].as_array().map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")))
                                    .unwrap_or_else(|| "Claude Codeの実行に失敗しました。".into()));
                            }
                            return Ok(());
                        }
                        if message["type"] == "control_request" {
                            self.permission(&message)?;
                        } else if message["type"] == "control_cancel_request" {
                            let request_id = message["request_id"].as_str().ok_or("Claude canceled request ID is missing")?;
                            let id = self.approval_id(request_id);
                            self.pending.lock().unwrap().remove(&id);
                            self.router.resolve_native_request(ProviderKind::Claude, &id.into());
                        } else if message["type"] == "control_response" && message["response"]["request_id"] == "interrupt" {
                            let result = if message["response"]["subtype"] == "success" { interrupted = true; Ok(()) }
                                else { Err(format!("Claude Codeの停止に失敗しました: {}", message["response"]["error"])) };
                            self.interrupt.send_replace(Some(result));
                        } else {
                            self.message(message).await?;
                        }
                    }
                }
            }
        }.await;
        let mut retained = None;
        let outcome = if outcome.is_ok() && !self.stop.is_cancelled() {
            retained = Some(process);
            outcome
        } else {
            let exited = process.finish().await;
            outcome.and(exited)
        };
        let pending: Vec<_> = {
            let mut pending = self.pending.lock().unwrap();
            let ids: Vec<_> = pending
                .iter()
                .filter(|(_, request)| request.thread_id == self.thread_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                pending.remove(id);
            }
            ids
        };
        for id in pending {
            self.router
                .resolve_native_request(ProviderKind::Claude, &id.into());
        }
        let mut record = self.record.lock().await;
        let Some(mut turn) = self.current_turn() else {
            record.running = None;
            drop(record);
            if let Some(process) = retained {
                let _ = process.finish().await;
            }
            self.router.session_change(
                &self.thread_id,
                SessionChange::Status {
                    status: ThreadStatus {
                        kind: "notLoaded".into(),
                        extra: Default::default(),
                    },
                },
            );
            return;
        };
        turn.status = Some(
            if interrupted {
                "interrupted"
            } else if outcome.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into(),
        );
        turn.completed_at = Some(Some(now().into()));
        if let Some(items) = &mut turn.items {
            for item in items
                .iter_mut()
                .filter(|item| item.status.as_deref() == Some("inProgress"))
            {
                Arc::make_mut(item).status =
                    Some(if interrupted { "interrupted" } else { "failed" }.into());
            }
        }
        if let Err(error) = outcome {
            turn.error = Some(json!({"message":error}));
        }
        record.running = None;
        let released = CancellationToken::new();
        if let Some(process) = retained {
            record.idle = Some(Idle {
                process,
                model: self.model,
                effort: self.effort,
                released: released.clone(),
            });
        }
        self.router.session_change(
            &self.thread_id,
            SessionChange::Turn {
                turn,
                completed: true,
            },
        );
        let retained = record.idle.is_some();
        drop(record);
        if retained {
            tokio::select! {
                _ = released.cancelled() => return,
                _ = self.stop.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {},
            }
            let idle = {
                let mut record = self.record.lock().await;
                if released.is_cancelled() {
                    None
                } else {
                    record.idle.take()
                }
            };
            if let Some(idle) = idle {
                let _ = idle.process.finish().await;
            }
        }
    }

    fn permission(&self, message: &Value) -> Result<(), String> {
        if message.to_string().len() > 32 * 1024 {
            return Err("Claude control request exceeds its size limit".into());
        }
        let request = &message["request"];
        if request["subtype"] != "can_use_tool" {
            return Err(format!(
                "unsupported Claude control request: {}",
                request["subtype"]
            ));
        }
        let request_id = message["request_id"]
            .as_str()
            .ok_or("Claude permission request ID is missing")?;
        let id = self.approval_id(request_id);
        let tool = request["tool_name"]
            .as_str()
            .ok_or("Claude permission tool name is missing")?;
        let mut params = json!({"threadId":self.thread_id,"turnId":self.turn_id,"itemId":request["tool_use_id"],
            "reason":format!("{tool}\n{}", serde_json::to_string_pretty(&request["input"]).map_err(|error| error.to_string())?),
            "availableDecisions":["accept","decline"]});
        let method = if tool == "AskUserQuestion" {
            let questions = request["input"]["questions"]
                .as_array()
                .ok_or("Claude questions are missing")?;
            params["questions"] = Value::Array(
                questions
                    .iter()
                    .map(|question| {
                        let mut question = question.clone();
                        question["id"] = question["question"].clone();
                        question
                    })
                    .collect(),
            );
            "item/tool/requestUserInput"
        } else {
            match tool {
                "Bash" => "item/commandExecution/requestApproval",
                "Write" | "Edit" | "NotebookEdit" => "item/fileChange/requestApproval",
                _ => "claude/tool/requestApproval",
            }
        };
        let line = json!({"id":id,"method":method,"params":params}).to_string();
        let message = RpcMessage::parse(&line).map_err(|error| error.to_string())?;
        self.router.request_admission(&message)?;
        self.pending.lock().unwrap().insert(
            id,
            Pending {
                thread_id: self.thread_id.clone(),
                request_id: request_id.into(),
                input: request["input"].clone(),
                sender: self.input.clone(),
            },
        );
        self.router
            .handle_server_message(agent_core::session::ProviderKind::Claude, &message);
        Ok(())
    }

    fn approval_id(&self, request_id: &str) -> String {
        format!(
            "claude-permission:{}:{}:{request_id}",
            self.thread_id, self.turn_id
        )
    }

    async fn message(&mut self, message: Value) -> Result<(), String> {
        let scope = message["parent_tool_use_id"].as_str().unwrap_or_default();
        if message["type"] == "stream_event" {
            return self.stream_event(&message["event"], scope).await;
        }
        let mut record = self.record.lock().await;
        let kind = message["type"].as_str().unwrap_or_default();
        if kind == "system" && message["subtype"] == "init" {
            if message["session_id"].as_str() != Some(record.session_id.to_string().as_str()) {
                return Err("Claude session identity changed".into());
            }
            record.resumable = true;
        }
        if kind != "assistant" && kind != "user" {
            return Ok(());
        }
        let blocks = message["message"]["content"]
            .as_array()
            .ok_or("Claude message content is missing")?;
        let message_id = message["message"]["id"].as_str();
        drop(record);
        let mut turn = self
            .current_turn()
            .ok_or("Claude execution state is unavailable")?;
        let items = turn.items.get_or_insert_default();
        for (index, block) in blocks.iter().enumerate() {
            // Claude emits one assistant envelope per completed block, often
            // with the same message ID. Preserve the stream's block index.
            let id = if let Some((stream_id, block)) = self.stream.get(scope)
                && Some(stream_id.as_str()) == message_id
                && blocks.len() == 1
            {
                format!("{stream_id}:{block}")
            } else {
                format!(
                    "{}:{index}",
                    message["uuid"]
                        .as_str()
                        .or(message_id)
                        .unwrap_or("tool-result")
                )
            };
            let value = match block["type"].as_str() {
                Some("text") if kind == "assistant" => {
                    json!({"id":id,"type":"agentMessage","text":block["text"]})
                }
                Some("thinking") => json!({"id":id,"type":"reasoning","text":block["thinking"]}),
                Some("tool_use") => {
                    json!({"id":block["id"],"type":"mcpToolCall","server":"Claude Code","tool":block["name"],"arguments":block["input"],"status":"inProgress"})
                }
                Some("tool_result") => {
                    let item = items
                        .iter_mut()
                        .find(|item| Some(item.id.as_str()) == block["tool_use_id"].as_str())
                        .ok_or("Claude tool result has no matching tool call")?;
                    let item = Arc::make_mut(item);
                    item.status = Some(
                        if block["is_error"] == true {
                            "failed"
                        } else {
                            "completed"
                        }
                        .into(),
                    );
                    item.result = Some(block["content"].clone());
                    self.router.session_change(
                        &self.thread_id,
                        SessionChange::Item {
                            turn_id: self.turn_id.clone(),
                            item: item.clone(),
                        },
                    );
                    continue;
                }
                _ => continue,
            };
            let mut item: Item =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            if let Some(parent) = message["parent_tool_use_id"].as_str() {
                item.extra.insert("parentToolUseId".into(), parent.into());
            }
            self.router.session_change(
                &self.thread_id,
                SessionChange::Item {
                    turn_id: self.turn_id.clone(),
                    item: item.clone(),
                },
            );
            if let Some(existing) = items.iter_mut().find(|existing| existing.id == item.id) {
                *existing = Arc::new(item);
            } else {
                items.push(Arc::new(item));
            }
        }
        Ok(())
    }

    async fn stream_event(&mut self, event: &Value, scope: &str) -> Result<(), String> {
        match event["type"].as_str() {
            Some("message_start") => {
                self.stream.insert(
                    scope.into(),
                    (
                        event["message"]["id"]
                            .as_str()
                            .ok_or("Claude stream message ID is missing")?
                            .into(),
                        0,
                    ),
                );
            }
            Some("content_block_start") => {
                let (message, index) = self
                    .stream
                    .get_mut(scope)
                    .ok_or("Claude stream started a block without a message")?;
                *index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")? as usize;
                let kind = match event["content_block"]["type"].as_str() {
                    Some("text") => "agentMessage",
                    Some("thinking") => "reasoning",
                    _ => return Ok(()),
                };
                let mut item = Item {
                    id: format!("{message}:{index}"),
                    kind: Some(kind.into()),
                    text: Some(String::new()),
                    ..Default::default()
                };
                if !scope.is_empty() {
                    item.extra.insert("parentToolUseId".into(), scope.into());
                }
                self.router.session_change(
                    &self.thread_id,
                    SessionChange::Item {
                        turn_id: self.turn_id.clone(),
                        item,
                    },
                );
            }
            Some("content_block_delta") => {
                let (target, field) = match event["delta"]["type"].as_str() {
                    Some("text_delta") => (TextField::Message, "text"),
                    Some("thinking_delta") => (TextField::Reasoning, "thinking"),
                    _ => return Ok(()),
                };
                let (message, _) = self
                    .stream
                    .get(scope)
                    .ok_or("Claude stream delta has no message")?;
                let index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")?;
                let id = format!("{message}:{index}");
                let delta = event["delta"][field]
                    .as_str()
                    .ok_or("Claude stream text is missing")?;
                self.router.session_change(
                    &self.thread_id,
                    SessionChange::Text {
                        turn_id: self.turn_id.clone(),
                        item_id: id,
                        field: target,
                        delta: delta.into(),
                    },
                );
            }
            _ => {}
        }
        Ok(())
    }
}

async fn input_content(input: &Value) -> Result<Vec<Value>, String> {
    let input = input
        .as_array()
        .filter(|input| !input.is_empty())
        .ok_or("メッセージを入力してください。")?;
    let mut content = Vec::with_capacity(input.len());
    for input in input {
        match input["type"].as_str() {
            Some("text") => content.push(json!({"type":"text","text":input["text"].as_str().ok_or("message text is missing")?})),
            Some("mention") => content.push(json!({"type":"text","text":format!("添付ファイル: {}", input["path"].as_str().ok_or("attachment path is missing")?)})),
            Some("localImage") => {
                let path = input["path"].as_str().ok_or("image path is missing")?;
                let bytes = tokio::fs::read(path).await.map_err(|error| format!("画像を読み込めません: {error}"))?;
                let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { "image/png" }
                    else if bytes.starts_with(&[0xff, 0xd8, 0xff]) { "image/jpeg" }
                    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { "image/gif" }
                    else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { "image/webp" }
                    else { return Err("Claudeに送る画像はPNG・JPEG・GIF・WebPを使用してください。".into()); };
                content.push(json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":STANDARD.encode(bytes)}}));
            }
            _ => return Err("Claudeが対応していない入力形式です。".into()),
        }
    }
    Ok(content)
}

fn status(kind: &str) -> ThreadStatus {
    ThreadStatus {
        kind: kind.into(),
        extra: Default::default(),
    }
}
pub(crate) fn updated_at(thread: &Thread) -> u64 {
    thread
        .updated_at
        .as_ref()
        .and_then(|number| number.as_u64())
        .unwrap_or_default()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
