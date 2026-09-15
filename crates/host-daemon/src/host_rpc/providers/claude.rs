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
    models::{Model, ReasoningEffort, Thread, ThreadResponse},
    peer::RpcMessage,
    session::{Content, Entry, Event, Outcome, Target},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{Provider, catalog::updated_at};
use crate::{host_rpc::routing::SessionRouter, session_log::SessionLog};
use process::Process;

pub(crate) const MODEL_PREFIX: &str = "claude:";

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

struct Record {
    log: SessionLog,
    running: Option<Running>,
}

impl Record {
    fn native_session(&self) -> Result<&str, String> {
        self.log
            .session()
            .provider_state
            .get("claude")
            .and_then(|state| state["session_id"].as_str())
            .ok_or_else(|| "Claude resume identity is missing".into())
    }
    fn resumable(&self) -> bool {
        self.log
            .session()
            .provider_state
            .get("claude")
            .is_some_and(|state| state["resumable"] == true)
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
        let mut paths = Vec::new();
        let mut files = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| error.to_string())?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let path = file.path();
            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("json" | "jsonl")
            ) {
                paths.push(path);
            }
        }
        // A completed log takes precedence over its retained legacy source.
        paths.sort_by_key(|path| path.extension().is_some_and(|ext| ext == "json"));
        for path in paths {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("invalid Claude history path")?;
            let session_id = Uuid::parse_str(stem).map_err(|error| error.to_string())?;
            let id = format!("claude:{session_id}");
            if records.contains_key(&id) {
                continue;
            }
            let mut log = if path.extension().is_some_and(|ext| ext == "jsonl") {
                SessionLog::open(&path).await?
            } else {
                let bytes = tokio::fs::read(&path)
                    .await
                    .map_err(|error| error.to_string())?;
                let legacy: history::Legacy = serde_json::from_slice(&bytes).map_err(|error| {
                    format!("invalid Claude conversation {}: {error}", path.display())
                })?;
                if legacy.session_id != session_id
                    || legacy.response.thread.id.as_deref() != Some(&id)
                {
                    return Err(format!(
                        "Claude conversation identity mismatch: {}",
                        path.display()
                    ));
                }
                SessionLog::create(&path.with_extension("jsonl"), legacy.events()?).await?
            };
            if log.session().id != session_id.to_string() {
                return Err(format!(
                    "Claude conversation identity mismatch: {}",
                    path.display()
                ));
            }
            if let Some(execution) = log
                .session()
                .executions
                .last()
                .filter(|execution| execution.outcome.is_none())
            {
                log.append(now(), Event::ExecutionFinished {
                    execution_id: execution.id.clone(), outcome: Outcome::Interrupted,
                    error: Some(json!({"message":"Hostが終了したためClaudeの実行が中断されました。もう一度送信してください。"})),
                }).await?;
            }
            let record = Record { log, running: None };
            record.native_session()?;
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
        let native_id = Uuid::new_v4();
        let log = SessionLog::create(
            &self.directory.join(format!("{session_id}.jsonl")),
            vec![
                (
                    now(),
                    Event::Created {
                        id: session_id.to_string(),
                        cwd: cwd.to_string_lossy().into_owned(),
                        title: None,
                        selection: Target {
                            provider: "claude".into(),
                            model: model
                                .strip_prefix(MODEL_PREFIX)
                                .ok_or("Claude model is invalid")?
                                .into(),
                        },
                    },
                ),
                (
                    now(),
                    Event::ProviderState {
                        provider: "claude".into(),
                        state: json!({"session_id":native_id,"resumable":false}),
                    },
                ),
            ],
        )
        .await?;
        let response = history::response(log.session(), true);
        let record = Record { log, running: None };
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
            let thread = history::response(record.log.session(), false).thread;
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
            let mut thread = thread;
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
                let mut response =
                    history::response(record.log.session(), params["includeTurns"] != false);
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
                    .log
                    .session()
                    .executions
                    .iter()
                    .find(|execution| Some(execution.id.as_str()) == params["turnId"].as_str())
                    .and_then(|execution| {
                        execution
                            .entries
                            .iter()
                            .find(|entry| Some(entry.id.as_str()) == params["itemId"].as_str())
                    })
                    .ok_or("Claudeの履歴項目が見つかりません。")?;
                Ok(json!({"item":history::item(item)}))
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
        let previous_model = format!("{MODEL_PREFIX}{}", state.log.session().selection.model);
        let model = params["model"]
            .as_str()
            .or(Some(previous_model.as_str()))
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
        let session = state.native_session()?.to_owned();
        let cwd = &state.log.session().cwd;
        let (mut process, initialized) = Process::start(
            &self.program,
            Path::new(cwd),
            Some((&session, state.resumable())),
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
        state
            .log
            .append(
                now(),
                Event::ExecutionStarted {
                    id: turn_id.clone(),
                    target: Some(Target {
                        provider: "claude".into(),
                        model: model_name,
                    }),
                    input: Entry {
                        id: Uuid::new_v4().to_string(),
                        content: Content::UserMessage {
                            input: history::input(&params["input"])?,
                            client_id: params["clientUserMessageId"].as_str().map(str::to_owned),
                        },
                    },
                },
            )
            .await?;
        if let Err(error) = process.write(&json!({"type":"user","session_id":session,"message":{"role":"user","content":content},"parent_tool_use_id":null})).await {
            state.log.append(now(), Event::ExecutionFinished {
                execution_id: turn_id.clone(), outcome: Outcome::Failed, error: Some(json!({"message":error})),
            }).await?;
            return Err(error);
        }
        let turn = history::execution(state.log.session().executions.last().unwrap());
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
        let execution_outcome = if interrupted {
            Outcome::Interrupted
        } else if outcome.is_ok() {
            Outcome::Completed
        } else {
            Outcome::Failed
        };
        let saved = record
            .log
            .append(
                now(),
                Event::ExecutionFinished {
                    execution_id: self.turn_id.clone(),
                    outcome: execution_outcome,
                    error: outcome.err().map(|message| json!({"message":message})),
                },
            )
            .await;
        record.running = None;
        let mut turn = history::execution(record.log.session().executions.last().unwrap());
        if let Err(error) = saved {
            // The log remains authoritative. Surface a storage failure without
            // pretending an unpersisted completion entered it.
            turn.status = Some("failed".into());
            turn.error = Some(json!({"message":format!("Claudeの会話を保存できません: {error}")}));
        }
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
        self.router.handle_server_message(
            Provider::Claude,
            &RpcMessage::parse(&line).map_err(|error| error.to_string())?,
        );
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
            if message["session_id"].as_str() != Some(record.native_session()?) {
                return Err("Claude session identity changed".into());
            }
            let session_id = record.native_session()?.to_owned();
            record
                .log
                .append(
                    now(),
                    Event::ProviderState {
                        provider: "claude".into(),
                        state: json!({"session_id":session_id,"resumable":true}),
                    },
                )
                .await?;
        }
        if kind != "assistant" && kind != "user" {
            return Ok(());
        }
        let blocks = message["message"]["content"]
            .as_array()
            .ok_or("Claude message content is missing")?;
        let message_id = message["message"]["id"].as_str();
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
            let event = match block["type"].as_str() {
                Some("text") if kind == "assistant" => Event::EntrySet {
                    execution_id: self.turn_id.clone(),
                    entry: Entry {
                        id,
                        content: Content::AssistantText {
                            text: block["text"]
                                .as_str()
                                .ok_or("Claude text is missing")?
                                .into(),
                        },
                    },
                },
                Some("thinking") => Event::EntrySet {
                    execution_id: self.turn_id.clone(),
                    entry: Entry {
                        id,
                        content: Content::Thinking {
                            text: block["thinking"]
                                .as_str()
                                .ok_or("Claude thinking is missing")?
                                .into(),
                        },
                    },
                },
                Some("tool_use") => Event::EntrySet {
                    execution_id: self.turn_id.clone(),
                    entry: Entry {
                        id: block["id"]
                            .as_str()
                            .ok_or("Claude tool ID is missing")?
                            .into(),
                        content: Content::ToolCall {
                            name: block["name"]
                                .as_str()
                                .ok_or("Claude tool name is missing")?
                                .into(),
                            arguments: block["input"].clone(),
                            result: None,
                            outcome: None,
                        },
                    },
                },
                Some("tool_result") => Event::ToolFinished {
                    execution_id: self.turn_id.clone(),
                    entry_id: block["tool_use_id"]
                        .as_str()
                        .ok_or("Claude tool result ID is missing")?
                        .into(),
                    result: block["content"].clone(),
                    outcome: if block["is_error"] == true {
                        Outcome::Failed
                    } else {
                        Outcome::Completed
                    },
                },
                _ => continue,
            };
            let entry_id = match &event {
                Event::EntrySet { entry, .. } => entry.id.clone(),
                Event::ToolFinished { entry_id, .. } => entry_id.clone(),
                _ => unreachable!(),
            };
            record.log.append(now(), event).await?;
            let entry = record
                .log
                .session()
                .executions
                .last()
                .unwrap()
                .entries
                .iter()
                .find(|entry| entry.id == entry_id)
                .unwrap();
            let method = if matches!(entry.content, Content::ToolCall { outcome: None, .. }) {
                "item/started"
            } else {
                "item/completed"
            };
            emit(
                &self.router,
                method,
                json!({"threadId":self.thread_id,"turnId":self.turn_id,"item":history::item(entry)}),
            );
        }
        Ok(())
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
                let content = match event["content_block"]["type"].as_str() {
                    Some("text") => Content::AssistantText {
                        text: String::new(),
                    },
                    Some("thinking") => Content::Thinking {
                        text: String::new(),
                    },
                    _ => return Ok(()),
                };
                let entry = Entry {
                    id: format!("{message}:{index}"),
                    content,
                };
                let mut record = self.record.lock().await;
                record
                    .log
                    .append(
                        now(),
                        Event::EntrySet {
                            execution_id: self.turn_id.clone(),
                            entry: entry.clone(),
                        },
                    )
                    .await?;
                emit(
                    &self.router,
                    "item/started",
                    json!({"threadId":self.thread_id,"turnId":self.turn_id,"item":history::item(&entry)}),
                );
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
                record
                    .log
                    .append(
                        now(),
                        Event::TextAppended {
                            execution_id: self.turn_id.clone(),
                            entry_id: id.clone(),
                            text: delta.into(),
                        },
                    )
                    .await?;
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn emit(router: &SessionRouter, method: &str, params: Value) {
    let line = json!({"method":method,"params":params}).to_string();
    router.handle_server_message(
        Provider::Claude,
        &RpcMessage::parse(&line).expect("serialized notification"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn load(directory: &Path) -> Result<Claude, String> {
        Claude::load(
            "unused-claude".into(),
            directory.into(),
            SessionRouter::new(),
        )
        .await
    }

    #[tokio::test]
    async fn legacy_history_is_imported_once_and_preserves_resume_input_and_unknown_content() {
        let directory = tempfile::tempdir().unwrap();
        let native_id = Uuid::new_v4();
        let id = format!("claude:{native_id}");
        let path = directory.path().join(format!("{native_id}.json"));
        let original = json!({
            "session_id":native_id,"resumable":true,
            "response":{"model":"claude:latest","thread":{
                "id":id,"cwd":"/workspace","name":"kept title","createdAt":10,"updatedAt":20,
                "turns":[{"id":"old-execution","status":"completed","startedAt":11,"completedAt":20,
                    "items":[{"id":"input","type":"userMessage","clientId":"sent-once","content":[{"type":"text","text":"original input"}]},
                        {"id":"answer","type":"agentMessage","text":"original answer"},
                        {"id":"future","type":"futureActivity","newField":{"nested":[1,2,3]}}]}]
            }}
        });
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let claude = load(directory.path()).await.unwrap();
        let response = claude
            .request("host/thread/read", json!({"threadId":id}))
            .await
            .unwrap();
        assert_eq!(
            response["thread"]["turns"],
            original["response"]["thread"]["turns"]
        );
        assert_eq!(response["thread"]["name"], "kept title");
        let record = claude.record(&id).await.unwrap();
        {
            let mut record = record.lock().await;
            assert_eq!(record.native_session().unwrap(), native_id.to_string());
            assert!(record.resumable());
            assert!(
                record.log.session().executions[0].target.is_none(),
                "legacy model history is unknown"
            );
            record
                .log
                .append(
                    30,
                    Event::ExecutionStarted {
                        id: "new-execution".into(),
                        target: Some(Target {
                            provider: "claude".into(),
                            model: "new-model".into(),
                        }),
                        input: Entry {
                            id: "new-input".into(),
                            content: Content::UserMessage {
                                input: Vec::new(),
                                client_id: None,
                            },
                        },
                    },
                )
                .await
                .unwrap();
        }
        drop(record);
        drop(claude); // Simulate a Host exit before the execution completes.
        let claude = load(directory.path()).await.unwrap();
        let response = claude
            .request("host/thread/read", json!({"threadId":id}))
            .await
            .unwrap();
        assert_eq!(response["thread"]["turns"].as_array().unwrap().len(), 2);
        assert_eq!(response["thread"]["turns"][1]["status"], "interrupted");
        assert_eq!(response["model"], "claude:new-model");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
            original
        );
        let journal = std::fs::read(path.with_extension("jsonl")).unwrap();
        drop(claude);
        let claude = load(directory.path()).await.unwrap();
        assert_eq!(
            std::fs::read(path.with_extension("jsonl")).unwrap(),
            journal,
            "recovery must not repeat completed transitions"
        );
        drop(claude);
        // Once the journal exists, a damaged log must never restore stale JSON.
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path.with_extension("jsonl"))
            .unwrap()
            .write_all(b"broken\n")
            .unwrap();
        assert!(load(directory.path()).await.is_err());
    }
}
