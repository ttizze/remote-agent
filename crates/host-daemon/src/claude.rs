//! Claude Code owns inference, credentials and its transcript. The Host owns
//! the client-facing conversation and adapts the CLI's streaming protocol.
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
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::host_rpc::routing::SessionRouter;
use process::Process;

pub(crate) const MODEL_PREFIX: &str = "claude:";

pub(crate) fn is_permission_id(raw_id: &str) -> bool {
    raw_id.starts_with("\"claude-permission:")
}

pub(crate) struct Claude {
    program: PathBuf,
    directory: PathBuf,
    records: AsyncMutex<HashMap<String, Arc<AsyncMutex<Record>>>>,
    models: OnceCell<Vec<Model>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    router: SessionRouter,
    stop: CancellationToken,
    workers: AsyncMutex<tokio::task::JoinSet<()>>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    response: ThreadResponse,
    session_id: Uuid,
    resumable: bool,
    #[serde(skip)]
    running: Option<Running>,
}

impl Record {
    fn turn_mut(&mut self) -> &mut Turn {
        Arc::make_mut(
            self.response
                .thread
                .turns
                .as_mut()
                .and_then(|turns| turns.last_mut())
                .expect("a running Claude conversation has a turn"),
        )
    }
}

struct Running {
    turn_id: String,
    input: mpsc::Sender<Value>,
    interrupt: watch::Receiver<Option<Result<(), String>>>,
}

struct Pending {
    thread_id: String,
    request_id: String,
    input: Value,
    sender: mpsc::Sender<Value>,
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
        router: SessionRouter,
    ) -> Result<Self, String> {
        crate::platform::create_state_directory(&directory).map_err(|error| error.to_string())?;
        let mut records = HashMap::new();
        let mut files = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| error.to_string())?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let path = file.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| error.to_string())?;
            let mut record: Record = serde_json::from_slice(&bytes).map_err(|error| {
                format!("invalid Claude conversation {}: {error}", path.display())
            })?;
            let id = format!("claude:{}", record.session_id);
            if record.response.thread.id.as_deref() != Some(&id)
                || path.file_stem().and_then(|s| s.to_str())
                    != Some(record.session_id.to_string().as_str())
            {
                return Err(format!(
                    "Claude conversation identity mismatch: {}",
                    path.display()
                ));
            }
            let mut interrupted = false;
            for turn in record.response.thread.turns.iter_mut().flatten() {
                if turn.status.as_deref() == Some("inProgress") {
                    let turn = Arc::make_mut(turn);
                    turn.status = Some("interrupted".into());
                    turn.error = Some(
                        json!({"message":"Hostが終了したためClaudeの実行が中断されました。もう一度送信してください。"}),
                    );
                    interrupted = true;
                }
            }
            record.response.thread.status = Some(status("idle"));
            if interrupted {
                save(&directory, &record)?;
            }
            records.insert(id, Arc::new(AsyncMutex::new(record)));
        }
        Ok(Self {
            program,
            directory,
            records: AsyncMutex::new(records),
            models: OnceCell::new(),
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
                let (process, initialized) =
                    Process::start(&self.program, cwd.path(), None, None, None).await?;
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
        let record = Record {
            response: ThreadResponse {
                thread: Thread {
                    id: Some(id.clone()),
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    status: Some(status("idle")),
                    turns: Some(Vec::new()),
                    created_at: Some(now().into()),
                    updated_at: Some(now().into()),
                    history_cursor: Some(None),
                    ..Default::default()
                },
                model: Some(model.into()),
                extra: Default::default(),
            },
            session_id,
            resumable: false,
            running: None,
        };
        save(&self.directory, &record)?;
        let response = record.response.clone();
        self.records
            .lock()
            .await
            .insert(id, Arc::new(AsyncMutex::new(record)));
        Ok(response)
    }

    async fn record(&self, id: &str) -> Result<Arc<AsyncMutex<Record>>, String> {
        self.records
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| "Claudeの会話が見つかりません。".into())
    }

    pub(crate) async fn list(&self, search: &str) -> Vec<Thread> {
        let records: Vec<_> = self.records.lock().await.values().cloned().collect();
        let mut threads = Vec::new();
        let search = search.trim().to_lowercase();
        for record in records {
            let record = record.lock().await;
            let thread = &record.response.thread;
            if !search.is_empty()
                && !thread
                    .name
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&search)
                && !thread
                    .preview
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&search)
            {
                continue;
            }
            let mut thread = thread.clone();
            thread.turns = None;
            threads.push(thread);
        }
        threads.sort_by(|left, right| {
            updated_at(right)
                .cmp(&updated_at(left))
                .then_with(|| left.id.cmp(&right.id))
        });
        threads
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = params["threadId"].as_str().ok_or("threadId is required")?;
        let record = self.record(id).await?;
        match method {
            "host/thread/read" | "thread/read" | "thread/resume" | "host/thread/resume" => {
                let record = record.lock().await;
                let mut response = record.response.clone();
                if params["includeTurns"] == false {
                    response.thread.turns = None;
                }
                if params["deferItemDetails"] == true {
                    response.thread.defer_item_details();
                }
                serde_json::to_value(response).map_err(|error| error.to_string())
            }
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
                    input.send(json!({"type":"control_request","request_id":"interrupt","request":{"subtype":"interrupt"}})).await.map_err(|_| "Claude Code input is closed")?;
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
            "host/thread/item/read" => {
                let record = record.lock().await;
                let item = record
                    .response
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .find(|turn| Some(turn.id.as_str()) == params["turnId"].as_str())
                    .and_then(|turn| turn.items.as_ref())
                    .into_iter()
                    .flatten()
                    .find(|item| Some(item.id.as_str()) == params["itemId"].as_str())
                    .ok_or("Claudeの履歴項目が見つかりません。")?;
                Ok(json!({"item":item}))
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
            .or(state.response.model.as_deref())
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
        let cwd = state
            .response
            .thread
            .cwd
            .as_deref()
            .ok_or("Claude working directory is missing")?;
        let (mut process, initialized) = Process::start(
            &self.program,
            Path::new(cwd),
            Some((&session, state.resumable)),
            Some(&model_name),
            effort,
        )
        .await?;
        if !initialized["account"]["subscriptionType"]
            .as_str()
            .is_some_and(|plan| !plan.is_empty())
        {
            process.finish().await?;
            return Err("Claudeのサブスク認証がありません。Hostの端末で claude auth login を実行し、APIキーではなくClaudeアカウントでログインしてください。".into());
        }
        let turn_id = Uuid::new_v4().to_string();
        let user: Item = serde_json::from_value(json!({"id":Uuid::new_v4().to_string(),"type":"userMessage","content":params["input"],"clientId":params["clientUserMessageId"]})).map_err(|error| error.to_string())?;
        let turn = Turn {
            id: turn_id.clone(),
            status: Some("inProgress".into()),
            items: Some(vec![Arc::new(user)]),
            started_at: Some(Some(now().into())),
            ..Default::default()
        };
        let previous = state.response.clone();
        state.response.model = Some(model);
        state.response.thread.status = Some(status("active"));
        state.response.thread.updated_at = Some(now().into());
        if state.response.thread.preview.is_none() {
            state.response.thread.preview = Some(
                params["input"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|input| input["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .chars()
                    .take(120)
                    .collect(),
            );
        }
        state
            .response
            .thread
            .turns
            .get_or_insert_default()
            .push(Arc::new(turn.clone()));
        if let Err(error) = save(&self.directory, &state) {
            state.response = previous;
            return Err(error);
        }
        if let Err(error) = process.write(&json!({"type":"user","session_id":session,"message":{"role":"user","content":content},"parent_tool_use_id":null})).await {
            state.response = previous;
            save(&self.directory, &state)?;
            return Err(error);
        }
        let (input, receiver) = mpsc::channel(32);
        let (interrupt, interrupted) = watch::channel(None);
        state.running = Some(Running {
            turn_id: turn_id.clone(),
            input: input.clone(),
            interrupt: interrupted,
        });
        drop(state);
        emit(
            &self.router,
            "turn/started",
            json!({"threadId":id,"turn":turn}),
        );
        emit(
            &self.router,
            "thread/status/changed",
            json!({"threadId":id,"status":{"type":"active"}}),
        );
        let worker = Worker {
            record,
            directory: self.directory.clone(),
            router: self.router.clone(),
            pending: self.pending.clone(),
            thread_id: id.into(),
            turn_id: turn_id.clone(),
            input,
            stop: self.stop.child_token(),
            stream: None,
            interrupt,
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
        pending.sender.send(json!({"type":"control_response","response":{"subtype":"success","request_id":pending.request_id,"response":response}})).await.map_err(|_| "Claude Code input is closed")?;
        emit(
            &self.router,
            "serverRequest/resolved",
            json!({"requestId":id}),
        );
        Ok(true)
    }
}

struct Worker {
    record: Arc<AsyncMutex<Record>>,
    directory: PathBuf,
    router: SessionRouter,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    thread_id: String,
    turn_id: String,
    input: mpsc::Sender<Value>,
    stop: CancellationToken,
    stream: Option<(String, usize)>,
    interrupt: watch::Sender<Option<Result<(), String>>>,
}

impl Worker {
    async fn run(mut self, mut process: Process, mut input: mpsc::Receiver<Value>) {
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
                        process.write(&command).await?;
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
                            emit(&self.router, "serverRequest/resolved", json!({"requestId":id}));
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
        let exited = process.finish().await;
        let outcome = outcome.and(exited);
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
            emit(
                &self.router,
                "serverRequest/resolved",
                json!({"requestId":id}),
            );
        }
        let mut record = self.record.lock().await;
        let turn = record.turn_mut();
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
        record.response.thread.status = Some(status("idle"));
        record.response.thread.updated_at = Some(now().into());
        if let Err(error) = save(&self.directory, &record) {
            let turn = record.turn_mut();
            turn.status = Some("failed".into());
            turn.error = Some(json!({"message":format!("Claudeの会話を保存できません: {error}")}));
        }
        record.running = None;
        let turn = record
            .response
            .thread
            .turns
            .as_ref()
            .unwrap()
            .last()
            .unwrap();
        emit(
            &self.router,
            "turn/completed",
            json!({"threadId":self.thread_id,"turn":turn}),
        );
        emit(
            &self.router,
            "thread/status/changed",
            json!({"threadId":self.thread_id,"status":{"type":"idle"}}),
        );
    }

    fn permission(&self, message: &Value) -> Result<(), String> {
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
            .handle_server_message(&RpcMessage::parse(&line).map_err(|error| error.to_string())?);
        Ok(())
    }

    fn approval_id(&self, request_id: &str) -> String {
        format!("claude-permission:{}:{request_id}", self.thread_id)
    }

    async fn message(&mut self, message: Value) -> Result<(), String> {
        if !message["parent_tool_use_id"].is_null() {
            return Ok(());
        }
        if message["type"] == "stream_event" {
            return self.stream_event(&message["event"]).await;
        }
        let mut record = self.record.lock().await;
        let kind = message["type"].as_str().unwrap_or_default();
        if kind == "system" && message["subtype"] == "init" {
            if message["session_id"].as_str() != Some(record.session_id.to_string().as_str()) {
                return Err("Claude session identity changed".into());
            }
            record.resumable = true;
            save(&self.directory, &record)?;
        }
        if kind != "assistant" && kind != "user" {
            return Ok(());
        }
        let blocks = message["message"]["content"]
            .as_array()
            .ok_or("Claude message content is missing")?;
        let message_id = message["message"]["id"].as_str();
        let turn = record.turn_mut();
        let items = turn.items.get_or_insert_default();
        for (index, block) in blocks.iter().enumerate() {
            // Claude emits one assistant envelope per completed block, often
            // with the same message ID. Preserve the stream's block index.
            let id = if let Some((stream_id, block)) = &self.stream
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
                    emit(
                        &self.router,
                        "item/completed",
                        json!({"threadId":self.thread_id,"turnId":self.turn_id,"item":item}),
                    );
                    continue;
                }
                _ => continue,
            };
            let item: Item = serde_json::from_value(value).map_err(|error| error.to_string())?;
            let method = if item.status.as_deref() == Some("inProgress") {
                "item/started"
            } else {
                "item/completed"
            };
            emit(
                &self.router,
                method,
                json!({"threadId":self.thread_id,"turnId":self.turn_id,"item":item}),
            );
            if let Some(existing) = items.iter_mut().find(|existing| existing.id == item.id) {
                *existing = Arc::new(item);
            } else {
                items.push(Arc::new(item));
            }
        }
        save(&self.directory, &record)
    }

    async fn stream_event(&mut self, event: &Value) -> Result<(), String> {
        match event["type"].as_str() {
            Some("message_start") => {
                self.stream = Some((
                    event["message"]["id"]
                        .as_str()
                        .ok_or("Claude stream message ID is missing")?
                        .into(),
                    0,
                ));
            }
            Some("content_block_start") => {
                let (message, index) = self
                    .stream
                    .as_mut()
                    .ok_or("Claude stream started a block without a message")?;
                *index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")? as usize;
                let kind = match event["content_block"]["type"].as_str() {
                    Some("text") => "agentMessage",
                    Some("thinking") => "reasoning",
                    _ => return Ok(()),
                };
                let item = Item {
                    id: format!("{message}:{index}"),
                    kind: Some(kind.into()),
                    text: Some(String::new()),
                    ..Default::default()
                };
                let mut record = self.record.lock().await;
                let turn = record.turn_mut();
                emit(
                    &self.router,
                    "item/started",
                    json!({"threadId":self.thread_id,"turnId":self.turn_id,"item":item}),
                );
                turn.items.get_or_insert_default().push(Arc::new(item));
            }
            Some("content_block_delta") => {
                let (method, field) = match event["delta"]["type"].as_str() {
                    Some("text_delta") => ("item/agentMessage/delta", "text"),
                    Some("thinking_delta") => ("item/reasoning/textDelta", "thinking"),
                    _ => return Ok(()),
                };
                let (message, _) = self
                    .stream
                    .as_ref()
                    .ok_or("Claude stream delta has no message")?;
                let index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")?;
                let id = format!("{message}:{index}");
                let delta = event["delta"][field]
                    .as_str()
                    .ok_or("Claude stream text is missing")?;
                let mut record = self.record.lock().await;
                let turn = record.turn_mut();
                let item = turn
                    .items
                    .as_mut()
                    .and_then(|items| items.iter_mut().find(|item| item.id == id))
                    .ok_or("Claude stream delta has no block")?;
                Arc::make_mut(item)
                    .text
                    .get_or_insert_default()
                    .push_str(delta);
                emit(
                    &self.router,
                    method,
                    json!({"threadId":self.thread_id,"turnId":self.turn_id,"itemId":id,"delta":delta}),
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
fn save(directory: &Path, record: &Record) -> Result<(), String> {
    crate::platform::save_private_json(
        &directory.join(format!("{}.json", record.session_id)),
        record,
    )
}
fn emit(router: &SessionRouter, method: &str, params: Value) {
    let line = json!({"method":method,"params":params}).to_string();
    router.handle_server_message(&RpcMessage::parse(&line).expect("serialized notification"));
}
