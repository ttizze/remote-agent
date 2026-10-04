//! Claude Code owns inference, credentials and its transcript. The Host owns
//! the client-facing conversation and adapts the CLI's streaming protocol.
use agent_protocol::{execution::*, items::*};
mod accounts;
mod history;
mod native;
mod process;

use anyhow::Context;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use agent_protocol::operations as op;

use agent_protocol::models::Item;

use agent_protocol::models::Model;

use agent_protocol::models::ReasoningEffort;

use agent_protocol::models::Thread;

use agent_protocol::models::ThreadResponse;

use agent_protocol::models::SessionStatus;

use agent_protocol::models::Turn;

use agent_protocol::session::ProviderKind;

use agent_protocol::session::SessionChange;

use agent_protocol::session::SessionRef;

use agent_protocol::session::TextField;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::host_rpc::{
    agent::{
        Agent, AgentChange, AgentEvent, AnswerWrite, SessionPage, SessionSummary, SubmissionState,
        emit,
    },
    service::Failure,
};
use futures_util::FutureExt;
use process::Process;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct OperationError {
    message: String,
    delivery: agent_transport::peer::Delivery,
}
impl From<OperationError> for Failure {
    fn from(error: OperationError) -> Self {
        Self {
            code: "provider_failed",
            message: error.message,
            delivery: error.delivery,
            execution: None,
        }
    }
}

impl From<String> for OperationError {
    fn from(message: String) -> Self {
        Self {
            message,
            delivery: agent_transport::peer::Delivery::NotSent,
        }
    }
}
impl From<anyhow::Error> for OperationError {
    fn from(error: anyhow::Error) -> Self {
        format!("{error:#}").into()
    }
}
impl From<&str> for OperationError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub(crate) struct Claude {
    program: PathBuf,
    directory: PathBuf,
    native_home: PathBuf,
    accounts: AsyncMutex<accounts::Accounts>,
    records: AsyncMutex<HashMap<String, Arc<AsyncMutex<Record>>>>,
    processes: Arc<Semaphore>,
    events: mpsc::Sender<AgentEvent>,
    event_receiver: std::sync::Mutex<Option<mpsc::Receiver<AgentEvent>>>,
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
    auth_revision: u64,
    model: String,
    effort: Option<String>,
    released: CancellationToken,
}

struct Running {
    turn_id: agent_protocol::ids::TurnId,
    input: mpsc::Sender<Command>,
    interrupt: watch::Receiver<Option<Result<(), String>>>,
}

pub(crate) struct Command {
    pub(crate) value: Value,
    pub(crate) user: Option<Item>,
    pub(crate) delivered: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
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
    ) -> anyhow::Result<Self> {
        crate::platform::create_state_directory(&directory)?;
        let native_home = native_home.map(Ok).unwrap_or_else(history::home)?;
        let accounts = accounts::Accounts::load(
            program.clone(),
            directory.join("accounts"),
            native_home.clone(),
        )
        .await?;
        let (events, event_receiver) = mpsc::channel(256);
        Ok(Self {
            accounts: AsyncMutex::new(accounts),
            program,
            directory,
            native_home,
            records: AsyncMutex::new(HashMap::new()),
            processes: Arc::new(Semaphore::new(8)),
            events,
            event_receiver: std::sync::Mutex::new(Some(event_receiver)),
            stop: CancellationToken::new(),
            workers: AsyncMutex::new(tokio::task::JoinSet::new()),
        })
    }

    async fn models(&self) -> Result<Vec<Model>, String> {
        if self.availability().is_err() {
            return Ok(Vec::new());
        }
        let Some(auth_home) = self.accounts.lock().await.selected_home()? else {
            return Ok(Vec::new());
        };
        let cwd = tempfile::tempdir_in(&self.directory).map_err(|error| error.to_string())?;
        let (process, initialized) = Process::start(
            &self.program,
            &self.native_home,
            &auth_home,
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
                // The CLI's short display name omits the model generation.
                // Its description starts with the versioned name and context size.
                let title = entry["description"]
                    .as_str()
                    .and_then(|description| description.split('·').next())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or(display);
                let display = if name == "default" && title != display {
                    format!("{display} · {title}")
                } else {
                    title.to_owned()
                };
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
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let default = efforts
                    .iter()
                    .find(|effort| effort.reasoning_effort == "high")
                    .or(efforts.first())
                    .map(|effort| effort.reasoning_effort.clone())
                    .unwrap_or_default();
                let model = agent_protocol::models::ModelRef {
                    provider: ProviderKind::Claude,
                    id: name.into(),
                };
                Ok(Model {
                    id: name.into(),
                    model,
                    display_name: format!("Claude · {display}"),
                    default_reasoning_effort: default,
                    supported_reasoning_efforts: efforts,
                    service_tiers: Some(Vec::new()),
                    default_service_tier: None,
                    is_default: Some(false),
                })
            })
            .collect::<Result<Vec<Model>, String>>()
    }

    async fn create(&self, cwd: &str, model: &str) -> anyhow::Result<ThreadResponse> {
        let cwd = dunce::simplified(&tokio::fs::canonicalize(cwd).await?).to_owned();
        if !cwd.is_dir() {
            return Err(anyhow::anyhow!("Claudeの作業フォルダがありません。"));
        }
        let session_id = Uuid::new_v4();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(SessionRef {
                    provider: ProviderKind::Claude,
                    id: session_id.to_string(),
                }),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                status: SessionStatus::Idle,
                turns: Some(Vec::new()),
                updated_at: Some(now() as f64),
                ..Default::default()
            },
            model: Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: model.into(),
            }),
        };
        let record = Record {
            cwd: cwd.to_string_lossy().into_owned(),
            model: model.into(),
            session_id,
            resumable: false,
            running: None,
            idle: None,
        };
        self.retain_record(session_id.to_string(), Arc::new(AsyncMutex::new(record)))
            .await?;
        Ok(response)
    }

    async fn record(&self, id: &str) -> anyhow::Result<Arc<AsyncMutex<Record>>> {
        if let Some(record) = self.records.lock().await.get(id).cloned() {
            return Ok(record);
        }
        let native_id = Uuid::parse_str(id).context("invalid Claude session ID")?;
        let home = self.native_home.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native_id)?;
            history::summary(&path)
        })
        .await??;
        let record = Arc::new(AsyncMutex::new(Record {
            cwd: summary
                .thread
                .cwd
                .context("Claude working directory is unavailable")?,
            model: "default".into(),
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
    ) -> anyhow::Result<Arc<AsyncMutex<Record>>> {
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
            return Err(anyhow::anyhow!(
                "Claude session capacity reached; wait for an active task to finish"
            ));
        }
        records.insert(id, record.clone());
        Ok(record)
    }

    async fn list(&self, search: &str) -> anyhow::Result<Vec<SessionSummary>> {
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
                                .map(|id| SessionRef {
                                    provider: ProviderKind::Claude,
                                    id: id.into(),
                                }),
                            name: Some("Claude履歴を読み取れません".into()),
                            status: SessionStatus::Unknown,
                            ..Default::default()
                        };
                        thread.history_read_state =
                            Some(agent_protocol::session::HistoryReadState::new(
                                agent_protocol::session::HistoryReadKind::Unavailable,
                                vec![format!("{error:#}")],
                            ));
                        SessionSummary {
                            thread,
                            branch: None,
                        }
                    }))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .await??;
        let records: Vec<_> = self.records.lock().await.values().cloned().collect();
        for record in records {
            let record = record.lock().await;
            if record.running.is_some() {
                let id = SessionRef {
                    provider: ProviderKind::Claude,
                    id: record.session_id.to_string(),
                };
                if let Some(summary) = threads
                    .iter_mut()
                    .find(|summary| summary.thread.id.as_ref() == Some(&id))
                {
                    summary.thread.status = SessionStatus::Running;
                } else {
                    threads.push(SessionSummary {
                        thread: Thread {
                            id: Some(SessionRef {
                                provider: ProviderKind::Claude,
                                id: record.session_id.to_string(),
                            }),
                            cwd: Some(record.cwd.clone()),
                            status: SessionStatus::Running,
                            ..Default::default()
                        },
                        branch: None,
                    });
                }
            }
        }
        let search = search.trim().to_lowercase();
        threads.retain(|summary| {
            let thread = &summary.thread;
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
            updated_at(&right.thread)
                .cmp(&updated_at(&left.thread))
                .then_with(|| left.thread.id.cmp(&right.thread.id))
        });
        Ok(threads)
    }

    /// Read native history on each open; the router overlays only owned execution.
    async fn read(&self, id: &str, requested: usize) -> anyhow::Result<ThreadResponse> {
        let native = Uuid::parse_str(id)?;
        let home = self.native_home.clone();
        let history: anyhow::Result<ThreadResponse> = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            match history::read(&path, requested) {
                Ok(response) => Ok(response),
                Err(error) => {
                    let mut thread = history::summary(&path)?.thread;
                    thread.history_read_state =
                        Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Unavailable,
                            vec![format!("{error:#}")],
                        ));
                    Ok(ThreadResponse {
                        thread,
                        model: None,
                    })
                }
            }
        })
        .await?;
        match history {
            Ok(response) => Ok(response),
            Err(error) => {
                // A newly created execution can precede its first native write.
                // Only execution metadata lives here; the router overlays live output.
                let records = self.records.lock().await;
                let record = match records.get(id) {
                    Some(record) => record.lock().await,
                    None => return Err(error),
                };
                let mut thread = Thread {
                    id: Some(SessionRef {
                        provider: ProviderKind::Claude,
                        id: id.into(),
                    }),
                    cwd: Some(record.cwd.clone()),
                    ..Default::default()
                };
                if record.resumable {
                    thread.history_read_state =
                        Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Unavailable,
                            vec![format!("{error:#}")],
                        ));
                } else {
                    thread.turns = Some(Vec::new());
                }
                Ok(ThreadResponse {
                    thread,
                    model: Some(agent_protocol::models::ModelRef {
                        provider: ProviderKind::Claude,
                        id: record.model.clone(),
                    }),
                })
            }
        }
    }

    async fn read_item(&self, params: &op::ReadItem) -> Result<op::ItemResponse, OperationError> {
        let native = Uuid::parse_str(&params.thread_id.id).map_err(|_| "invalid Claude ID")?;
        let home = self.native_home.clone();
        let native_history = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            history::read_details(&path)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))?;
        let response = native_history.response;
        let item = response
            .thread
            .turns
            .iter()
            .flatten()
            .find(|turn| turn.id == params.turn_id)
            .and_then(|turn| turn.items.as_ref())
            .into_iter()
            .flatten()
            .find(|item| item.id == params.item_id)
            .ok_or("Claude native history item is unavailable")?;
        let mut item = (**item).clone();
        if let Some(path) = native_history.output_paths.get(&item.id) {
            use tokio::io::AsyncReadExt;
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            file.take(history::MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > history::MAX_FILE_BYTES {
                return Err("native output exceeds parser read budget".into());
            }
            let content =
                Value::String(String::from_utf8(bytes).map_err(|error| error.to_string())?);
            item.body = ItemContent::Inline {
                body: Box::new(native::tool_result_body(
                    item.body(),
                    &content,
                    &Value::Null,
                )),
            };
        }
        if let ItemBody::Subagent {
            agent_id: Some(agent_id),
            ..
        } = item.body()
        {
            let home = self.native_home.clone();
            let agent_id = agent_id.to_owned();
            let related = tokio::task::spawn_blocking(move || {
                history::read_related(&home, native, &agent_id)
            })
            .await
            .map_err(|error| error.to_string())?;
            if let ItemBody::Subagent { result, .. } = item.body_mut() {
                *result = Some(match related {
                    Ok(response) => json!({"output":result.take(),"subagent":response.thread}),
                    Err(error) => {
                        json!({"output":result.take(),"subagentHistory":{"type":"unavailable","message":format!("{error:#}")}})
                    }
                });
            }
        }
        Ok(op::ItemResponse {
            item,
            transfer: None,
        })
    }

    async fn discard_workspace_processes(&self, directory: &Path) -> anyhow::Result<()> {
        let records: Vec<_> = self.records.lock().await.values().cloned().collect();
        for record in records {
            let idle = {
                let mut record = record.lock().await;
                if Path::new(&record.cwd).starts_with(directory) {
                    record.idle.take()
                } else {
                    None
                }
            };
            // Retained processes still hold the deleted directory's inode.
            if let Some(idle) = idle {
                idle.released.cancel();
                idle.process.finish().await.map_err(anyhow::Error::msg)?;
            }
        }
        Ok(())
    }

    async fn interrupt(
        &self,
        id: &str,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<(), OperationError> {
        let record = self.record(id).await?;
        let (input, mut interrupt) = {
            let record = record.lock().await;
            let running = record
                .running
                .as_ref()
                .ok_or("Claudeは実行中ではありません。")?;
            if turn_id != &running.turn_id {
                return Err("Claudeの実行対象が変わりました。会話を更新してください。".into());
            }
            (running.input.clone(), running.interrupt.clone())
        };
        if interrupt.borrow().is_none() {
            input.send(Command { value: json!({"type":"control_request","request_id":"interrupt","request":{"subtype":"interrupt"}}), user: None, delivered: None }).await.map_err(|_| "Claude Code input is closed")?;
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
        Ok(())
    }

    async fn additional_input(
        &self,
        id: &str,
        input: &[op::Input],
        client_id: &str,
    ) -> Result<agent_protocol::ids::TurnId, OperationError> {
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        let record = self.record(id).await?;
        let content = input_content(input).await?;
        let state = record.lock().await;
        let running = state
            .running
            .as_ref()
            .ok_or("Claudeは実行中ではありません。")?;
        let sender = running.input.clone();
        let turn_id = running.turn_id.clone();
        let session = state.session_id;
        drop(state);
        let id = client_id.to_owned();
        let (delivered, receipt) = tokio::sync::oneshot::channel();
        sender.send(Command {
            value: json!({"type":"user","uuid":id,"session_id":session,"message":{"role":"user","content":content},"parent_tool_use_id":null}),
            user: Some(Item { id: id.clone().into(), status: ItemStatus::Unknown, client_input_id: Some(client_id.into()), body: ItemContent::Inline { body: Box::new(ItemBody::UserMessage { text: None, content: op::Input::message_parts(input) }) } }),
            delivered: Some(delivered),
        }).await.map_err(|_| "Claude Code input is closed")?;
        tokio::time::timeout(std::time::Duration::from_secs(15), receipt)
            .await
            .map_err(|_| OperationError {
                message: "Claude input delivery is unknown".into(),
                delivery: agent_transport::peer::Delivery::Unknown,
            })?
            .map_err(|_| OperationError {
                message: "Claude exited before confirming additional input".into(),
                delivery: agent_transport::peer::Delivery::Unknown,
            })?
            .map_err(|message| OperationError {
                message,
                delivery: agent_transport::peer::Delivery::Unknown,
            })?;
        Ok(turn_id)
    }

    async fn start_turn(
        &self,
        params: &op::Submission,
        browser: Option<Value>,
    ) -> Result<agent_protocol::ids::TurnId, OperationError> {
        let record = self.record(&params.thread_id.id).await?;
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        let content = input_content(&params.input).await?;
        let mut state = record.lock().await;
        if state.running.is_some() {
            return Err("Claudeはすでに実行中です。".into());
        }
        let model = params
            .model
            .as_ref()
            .map(|model| model.id.as_str())
            .unwrap_or(&state.model)
            .to_owned();
        let effort = params.effort.as_deref();
        // Core's "default" means the backend's normal service. Claude has no
        // equivalent of Codex's explicit priority/flex tiers.
        if params
            .service_tier
            .as_deref()
            .is_some_and(|tier| tier != "default")
        {
            return Err("ClaudeではCodexのサービス階層を指定できません。".into());
        }
        let session = state.session_id.to_string();
        let cwd = state.cwd.clone();
        let (auth_home, auth_revision) = {
            let accounts = self.accounts.lock().await;
            (
                accounts
                    .selected_home()?
                    .ok_or("Claude アカウントを選択してください。")?,
                accounts.revision(),
            )
        };
        let idle = state.idle.take();
        let mut process = if idle.as_ref().is_some_and(|idle| {
            idle.auth_revision == auth_revision
                && idle.model == model
                && idle.effort.as_deref() == effort
        }) {
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
                &auth_home,
                Path::new(&cwd),
                Some((&session, state.resumable)),
                Some((&model, effort)),
                browser,
            )
            .await?;
            process.retain_capacity(permit);
            if !initialized["account"]["subscriptionType"]
                .as_str()
                .is_some_and(|plan| !plan.is_empty())
            {
                process.finish().await?;
                return Err("Claudeのサブスク認証がありません。アカウント設定から Claude アカウントを追加してください。".into());
            }
            process
        };
        let turn_id: agent_protocol::ids::TurnId = params.client_user_message_id.as_str().into();
        let user = Item {
            id: params.client_user_message_id.as_str().into(),
            status: ItemStatus::Unknown,
            client_input_id: Some(params.client_user_message_id.clone()),
            body: ItemContent::Inline {
                body: Box::new(ItemBody::UserMessage {
                    text: None,
                    content: op::Input::message_parts(&params.input),
                }),
            },
        };
        let turn = Turn {
            id: turn_id.clone(),
            status: TurnStatus::Running,
            items: Some(vec![Arc::new(user)]),
            started_at: Some(now() as f64),
            ..Default::default()
        };
        let mut workers = self.workers.lock().await;
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        if let Err(error) = process.write(&json!({"type":"user","uuid":params.client_user_message_id,"session_id":session,"message":{"role":"user","content":content},"parent_tool_use_id":null})).await {
            return Err(OperationError { message: error, delivery: agent_transport::peer::Delivery::Unknown });
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
        let target = SessionRef {
            provider: ProviderKind::Claude,
            id: params.thread_id.id.clone(),
        };
        let worker = Worker {
            record,
            events: self.events.clone(),
            live: agent_protocol::session::Timeline {
                turns: Some(vec![Arc::new(turn)]),
                ..Default::default()
            },
            instance: Uuid::new_v4(),
            session: target,
            turn_id: turn_id.clone(),
            input,
            stop: self.stop.child_token(),
            stream: None,
            interrupt,
            model,
            effort: effort.map(str::to_owned),
            auth_revision,
        };
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                tracing::error!(target: "bex", operation = "claude.worker", message = %error);
            }
        }
        workers.spawn(worker.run(process, receiver));
        Ok(turn_id)
    }
}

struct Worker {
    instance: Uuid,
    auth_revision: u64,
    record: Arc<AsyncMutex<Record>>,
    events: mpsc::Sender<AgentEvent>,
    live: agent_protocol::session::Timeline,
    session: SessionRef,
    turn_id: agent_protocol::ids::TurnId,
    input: mpsc::Sender<Command>,
    stop: CancellationToken,
    stream: Option<String>,
    interrupt: watch::Sender<Option<Result<(), String>>>,
    model: String,
    effort: Option<String>,
}

impl Worker {
    fn current_turn(&self) -> Option<&Turn> {
        self.live
            .turns
            .as_ref()?
            .iter()
            .find(|t| t.id == self.turn_id)
            .map(AsRef::as_ref)
    }
    async fn change(&mut self, change: SessionChange) -> Result<(), String> {
        let (live, result) = change.apply_timeline(std::mem::take(&mut self.live));
        self.live = live;
        result.map_err(|e| e.to_string())?;
        emit(
            &self.events,
            AgentChange::Session {
                session: self.session.clone(),
                change,
            },
        )
        .await
    }

    async fn run(mut self, mut process: Process, mut input: mpsc::Receiver<Command>) {
        let events = self.events.clone();
        let _requests = scopeguard::guard(self.instance, move |instance| {
            tokio::spawn(async move {
                let _ = events
                    .send(AgentEvent {
                        change: AgentChange::SourceClosed(instance),
                        applied: None,
                    })
                    .await;
            });
        });
        let mut interrupted = false;
        let mut pending_inputs = HashSet::new();
        let mut result_received = false;
        let mut idle = false;
        let outcome = async {
            let turn=self.current_turn().cloned().ok_or("Claude execution state is unavailable")?;
            self.change(SessionChange::Turn {turn,completed:false}).await?;
            loop {
                let message = tokio::select! {
                    _ = self.stop.cancelled() => {
                        interrupted = true;
                        process.write(&json!({"type":"control_request","request_id":"shutdown","request":{"subtype":"interrupt"}})).await?;
                        return Ok(());
                    }
                    command = input.recv() => {
                        let command = command.ok_or("Claude input queue is closed")?;
                        let result = process.write(&command.value).await;
                        if result.is_ok() && let Some(user) = command.user {
                            result_received = false;
                            idle = false;
                            pending_inputs.insert(user.id.clone());
                            self.change(SessionChange::Item {
                                turn_id: self.turn_id.clone(), item: Arc::new(user),
                            }).await?;
                        }
                        if let Some(delivered) = command.delivered { let _ = delivered.send(result.clone()); }
                        result?;
                        continue;
                    }
                    message = process.read() => message?.ok_or("Claude Code exited before the turn completed")?,
                };
                let kind = message["type"].as_str().unwrap_or_default();
                match kind {
                    "system" if message["subtype"] == "session_state_changed" => {
                        idle = message["state"] == "idle";
                    }
                    "result" => {
                        if message["is_error"] == true {
                            let mut error = execution_error(&message, false);
                            if matches!(error.category, ErrorCategory::Other | ErrorCategory::Provider(_))
                                && let Some(previous) = self.current_turn().and_then(|turn| turn.error.as_ref())
                            {
                                error.category = previous.category.clone();
                                error.provider_code = previous.provider_code.clone();
                            }
                            let text = error.message.clone();
                            self.change(SessionChange::Error {turn_id: self.turn_id.clone(), error}).await?;
                            return Err(text);
                        }
                        result_received = true;
                        if interrupted {
                            return Ok(());
                        }
                    }
                    "control_request" => {
                        if let Err(error) = self.permission(&message).await {
                            tracing::warn!(target: "bex", operation = "host.claude.request_rejected", message = %error);
                            let response = if message["request"]["subtype"] == "request_user_dialog" {
                                json!({"subtype":"success","request_id":message["request_id"],"response":{"behavior":"cancelled"}})
                            } else { json!({"subtype":"error","request_id":message["request_id"],"error":error}) };
                            process.write(&json!({"type":"control_response","response":response})).await?;
                        }
                    }
                    "control_cancel_request" => {
                        let request_id = message["request_id"].as_str().ok_or("Claude canceled request ID is missing")?;
                        emit(&self.events, AgentChange::Resolved {instance:self.instance,native_id:request_id.into()}).await?;
                    }
                    "control_response" if message["response"]["request_id"] == "interrupt" => {
                        let result = if message["response"]["subtype"] == "success" { interrupted = true; Ok(()) }
                            else { Err(format!("Claude Codeの停止に失敗しました: {}", message["response"]["error"])) };
                        self.interrupt.send_replace(Some(result));
                    }
                    _ => {
                        let input_consumed = kind == "user"
                            && message["uuid"].as_str().is_some_and(|id| pending_inputs.remove(id));
                        let response_started = message["parent_tool_use_id"].is_null()
                            && (kind == "assistant"
                                || (kind == "stream_event" && message["event"]["type"] == "message_start"));
                        if input_consumed || response_started {
                            result_received = false;
                            idle = false;
                        }
                        self.message(message).await?;
                    }
                }
                // A result ends one response, not necessarily the background
                // work and its follow-up. Idle is Claude's run-end signal.
                // It can precede the result; queued input must still be consumed.
                if result_received && idle && pending_inputs.is_empty() {
                    return Ok(());
                }
            }
        }.await;
        // Reject commands that lost the race with completion before retaining the process.
        drop(input);
        let _ = emit(&self.events, AgentChange::SourceClosed(self.instance)).await;
        scopeguard::ScopeGuard::into_inner(_requests);
        let mut retained = None;
        let outcome = if outcome.is_ok() && !self.stop.is_cancelled() {
            retained = Some(process);
            outcome
        } else {
            let exited = process.finish().await;
            outcome.and(exited)
        };
        let owned_record = self.record.clone();
        let mut record = owned_record.lock().await;
        let Some(mut turn) = self.current_turn().cloned() else {
            record.running = None;
            drop(record);
            if let Some(process) = retained {
                let _ = process.finish().await;
            }
            let _ = self
                .change(SessionChange::Status {
                    status: SessionStatus::Unknown,
                })
                .await;
            return;
        };
        turn.status = if interrupted {
            TurnStatus::Interrupted
        } else if outcome.is_ok() {
            TurnStatus::Completed
        } else {
            TurnStatus::Failed
        };
        if let Some(items) = &mut turn.items {
            for item in items
                .iter_mut()
                .filter(|item| item.status == ItemStatus::Running)
            {
                Arc::make_mut(item).status = if interrupted {
                    ItemStatus::Interrupted
                } else {
                    ItemStatus::Failed
                };
            }
        }
        if let Err(error) = outcome {
            turn.error.get_or_insert(ExecutionError {
                category: ErrorCategory::Network,
                message: error,
                ..Default::default()
            });
        }
        record.running = None;
        let released = CancellationToken::new();
        if let Some(process) = retained {
            record.idle = Some(Idle {
                process,
                auth_revision: self.auth_revision,
                model: self.model.clone(),
                effort: self.effort.clone(),
                released: released.clone(),
            });
        }
        let _ = self
            .change(SessionChange::Turn {
                turn,
                completed: true,
            })
            .await;
        let retained = record.idle.is_some();
        drop(record);
        self.live = Default::default();
        self.stream = None;
        if retained {
            tokio::select! {
                _ = released.cancelled() => return,
                _ = self.stop.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {},
            }
            let idle = {
                let owned_record = self.record.clone();
                let mut record = owned_record.lock().await;
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

    async fn permission(&self, message: &Value) -> Result<(), String> {
        if message.to_string().len() > 32 * 1024 {
            return Err("Claude control request exceeds its size limit".into());
        }
        let request_id = message["request_id"]
            .as_str()
            .ok_or("Claude request ID is missing")?;
        let adapted = crate::host_rpc::requests::claude(
            Uuid::new_v4().to_string().into(),
            &self.turn_id,
            &message["request"],
        )?;
        emit(
            &self.events,
            AgentChange::Request {
                session: self.session.clone(),
                origin: request_origin(
                    self.instance,
                    request_id.into(),
                    self.input.clone(),
                    adapted.answers,
                ),
                request: adapted.request,
            },
        )
        .await
    }

    async fn message(&mut self, message: Value) -> Result<(), String> {
        // Child messages belong to the subagent's native transcript, surfaced
        // through its tool result and related history rather than the parent answer.
        if message["parent_tool_use_id"].is_string() {
            return Ok(());
        }
        if message["type"] == "stream_event" {
            return self.stream_event(&message["event"]).await;
        }
        if message["type"] == "rate_limit_event" {
            if let Some(error) = usage_limit_error(&message["rate_limit_info"]) {
                self.change(SessionChange::Error {
                    turn_id: self.turn_id.clone(),
                    error,
                })
                .await?;
            }
            return Ok(());
        }
        if message["type"] == "system" && message["subtype"] == "api_retry" {
            if let Some(id) = self.stream.take()
                && let Some(turn) = self.current_turn()
            {
                let prefix = format!("{id}:");
                let items: Vec<_> = turn
                    .items
                    .iter()
                    .flatten()
                    .filter(|item| {
                        item.id.starts_with(&prefix)
                            && item.status == ItemStatus::Running
                            && matches!(
                                item.body(),
                                ItemBody::AssistantText { .. } | ItemBody::Reasoning { .. }
                            )
                    })
                    .map(|item| item.id.clone())
                    .collect();
                for item_id in items {
                    self.change(SessionChange::RemoveItem {
                        turn_id: self.turn_id.clone(),
                        item_id,
                    })
                    .await?;
                }
            }
            self.change(SessionChange::Error {
                turn_id: self.turn_id.clone(),
                error: execution_error(&message, true),
            })
            .await?;
            return Ok(());
        }
        let kind = message["type"].as_str().unwrap_or_default();
        let outcome = if kind == "system"
            && message["subtype"] == "task_notification"
            && message["skip_transcript"] != true
            && message["ambient"] != true
        {
            native::task_outcome(
                message["tool_use_id"].as_str(),
                message["status"].as_str(),
                message["summary"].as_str(),
                message["output_file"].as_str(),
            )
        } else if kind == "attachment" && message["attachment"]["type"] == "queued_command" {
            native::queued_task_outcome(
                message["attachment"]["commandMode"].as_str(),
                &message["attachment"]["prompt"],
            )
        } else {
            None
        };
        if let Some(outcome) = outcome
            && let Some(item) = self
                .current_turn()
                .and_then(|turn| turn.items.as_ref())
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.id.as_str() == outcome.tool_id)
                })
            && let Some(item) = outcome.item(item.id.clone(), item.body())
        {
            self.change(SessionChange::Item {
                turn_id: self.turn_id.clone(),
                item: Arc::new(item),
            })
            .await?;
            return Ok(());
        }
        if kind == "attachment" {
            if let Some(item) = native::attachment_item(
                message["uuid"]
                    .as_str()
                    .ok_or("Claude attachment ID is missing")?,
                &message["attachment"],
            )
            .map_err(|error| error.to_string())?
            {
                self.change(SessionChange::Item {
                    turn_id: self.turn_id.clone(),
                    item: Arc::new(item),
                })
                .await?;
            }
            return Ok(());
        }
        if kind == "system" && message["subtype"] == "init" {
            let mut record = self.record.lock().await;
            if message["session_id"].as_str() != Some(record.session_id.to_string().as_str()) {
                return Err("Claude session identity changed".into());
            }
            record.resumable = true;
            return Ok(());
        }
        if kind != "assistant" && kind != "user" {
            return Ok(());
        }
        if kind == "assistant" && message["error"].is_string() {
            self.change(SessionChange::Error {
                turn_id: self.turn_id.clone(),
                error: execution_error(&message, false),
            })
            .await?;
        }
        let blocks = native::message_blocks(&message["message"]["content"])
            .ok_or("Claude message content is missing")?;
        let message_id = message["message"]["id"].as_str();
        let cwd = self.record.lock().await.cwd.clone();
        for (index, block) in blocks.iter().enumerate() {
            let items = self
                .current_turn()
                .ok_or("Claude execution state is unavailable")?
                .items
                .as_deref()
                .unwrap_or_default();
            let item = match block["type"].as_str() {
                Some("tool_result") => {
                    let item = items
                        .iter()
                        .find(|item| Some(item.id.as_str()) == block["tool_use_id"].as_str())
                        .ok_or("Claude tool result has no matching tool call")?;
                    native::tool_result_item(
                        item.id.clone(),
                        item.body(),
                        &block["content"],
                        &message["tool_use_result"],
                        block["is_error"] == true,
                    )
                }
                Some("text" | "image") if kind == "user" => continue,
                _ => {
                    // Claude emits one assistant envelope per completed block,
                    // often with the same message ID. Preserve the stream index.
                    let id = message_item_id(
                        items,
                        message_id
                            .or_else(|| message["uuid"].as_str())
                            .unwrap_or("tool-result"),
                        index,
                        blocks.len(),
                        block,
                    );
                    native::content_item(
                        &self.session,
                        id,
                        block,
                        Some(cwd.as_str()),
                        ItemStatus::Running,
                    )
                    .map_err(|error| error.to_string())?
                }
            };
            self.change(SessionChange::Item {
                turn_id: self.turn_id.clone(),
                item: item.into(),
            })
            .await?;
        }
        Ok(())
    }

    async fn stream_event(&mut self, event: &Value) -> Result<(), String> {
        match event["type"].as_str() {
            Some("message_start") => {
                self.stream = Some(
                    event["message"]["id"]
                        .as_str()
                        .ok_or("Claude stream message ID is missing")?
                        .into(),
                );
            }
            Some("content_block_start") => {
                let message = self
                    .stream
                    .as_ref()
                    .ok_or("Claude stream started a block without a message")?;
                let index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")? as usize;
                if !matches!(
                    event["content_block"]["type"].as_str(),
                    Some("text" | "thinking")
                ) {
                    return Ok(());
                }
                let mut item = native::content_item(
                    &self.session,
                    format!("{message}:{index}"),
                    &event["content_block"],
                    None,
                    ItemStatus::Running,
                )
                .map_err(|error| error.to_string())?;
                item.status = ItemStatus::Running;
                self.change(SessionChange::Item {
                    turn_id: self.turn_id.clone(),
                    item: item.into(),
                })
                .await?;
            }
            Some("content_block_delta") => {
                let (target, field) = match event["delta"]["type"].as_str() {
                    Some("text_delta") => (TextField::AssistantText, "text"),
                    Some("thinking_delta") => {
                        (TextField::ReasoningContent { index: 0 }, "thinking")
                    }
                    _ => return Ok(()),
                };
                let message = self
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
                self.change(SessionChange::Text {
                    turn_id: self.turn_id.clone(),
                    item_id: id.into(),
                    field: target,
                    delta: delta.into(),
                })
                .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

// Final assistant envelopes can lag the start of subsequent blocks. Match
// their completed text, rather than assigning the stream's most recent index.
fn message_item_id(
    items: &[Arc<Item>],
    message: &str,
    index: usize,
    count: usize,
    block: &Value,
) -> String {
    if count == 1 {
        let prefix = format!("{message}:");
        let candidates = items.iter().filter(|item| item.id.starts_with(&prefix));
        let matches = |item: &Item| match (block["type"].as_str(), item.body()) {
            (Some("text"), ItemBody::AssistantText { text, .. }) => {
                block["text"].as_str() == Some(text)
            }
            (Some("thinking"), ItemBody::Reasoning { content, .. }) => {
                block["thinking"].as_str() == Some(content.join("").as_str())
            }
            _ => false,
        };
        if let Some(item) = candidates
            .clone()
            .filter(|item| matches(item))
            .min_by_key(|item| item.status != ItemStatus::Running)
        {
            return item.id.to_string();
        }
        if let Some(item) = candidates
            .filter(|item| item.status == ItemStatus::Running)
            .find(|item| {
                matches!(
                    (block["type"].as_str(), item.body()),
                    (Some("text"), ItemBody::AssistantText { .. })
                        | (Some("thinking"), ItemBody::Reasoning { .. })
                )
            })
        {
            return item.id.to_string();
        }
    }
    format!("{message}:{index}")
}

/// Classify native evidence before reducing it to a user-facing message.
fn usage_limit_error(info: &Value) -> Option<ExecutionError> {
    if info["status"] != "rejected" {
        return None;
    }
    Some(ExecutionError {
        category: ErrorCategory::UsageLimit,
        message: "Claudeの利用上限に達しました。".into(),
        provider_code: info["rateLimitType"].as_str().map(str::to_owned),
        ..Default::default()
    })
}
fn execution_error(message: &Value, retrying: bool) -> ExecutionError {
    let code = message["error"]
        .as_str()
        .or_else(|| message["subtype"].as_str());
    let status = message["api_error_status"]
        .as_u64()
        .or_else(|| message["error_status"].as_u64());
    let category = match code {
        Some("authentication_failed" | "oauth_org_not_allowed") => ErrorCategory::Auth,
        Some("rate_limit") => ErrorCategory::RateLimited,
        Some("overloaded") => ErrorCategory::Overloaded,
        Some("invalid_request" | "model_not_found") => ErrorCategory::InvalidInput,
        Some(
            "error_max_turns" | "error_max_budget_usd" | "error_max_structured_output_retries",
        ) => ErrorCategory::SessionLimit,
        Some("server_error") => ErrorCategory::Internal,
        _ => match status {
            Some(401 | 403) => ErrorCategory::Auth,
            Some(429) => ErrorCategory::RateLimited,
            Some(503) => ErrorCategory::Overloaded,
            Some(400 | 404 | 422) => ErrorCategory::InvalidInput,
            Some(500..=599) => ErrorCategory::Internal,
            None if retrying => ErrorCategory::Network,
            _ if code.is_some() => {
                ErrorCategory::Provider(serde_json::json!({"code":code,"httpStatus":status}))
            }
            _ => ErrorCategory::Other,
        },
    };
    let text = message["result"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            message["errors"].as_array().map(|errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .or_else(|| {
            message["message"]["content"].as_array().map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Claude Codeの実行に失敗しました。".into());
    let overloaded = category == ErrorCategory::Overloaded;
    ExecutionError {
        category,
        message: text,
        provider_code: code.map(str::to_owned),
        retry: retrying.then_some(RetryEvidence {
            retrying,
            overloaded,
        }),
        ..Default::default()
    }
}

fn skill_invocation(name: &str, path: &str) -> agent_protocol::composer::Invocation {
    agent_protocol::composer::Invocation {
        provider: ProviderKind::Claude,
        kind: agent_protocol::composer::InvocationKind::Skill,
        name: name.into(),
        path: path.into(),
    }
}

async fn input_content(input: &[op::Input]) -> Result<Vec<Value>, String> {
    if input.is_empty() {
        return Err("メッセージを入力してください。".into());
    }
    let mut content = Vec::with_capacity(input.len());
    for part in input {
        match part {
            op::Input::Text { text, .. } => {
                let text = input.iter().fold(text.clone(), |text, part| match part {
                    op::Input::Skill { name, path } => {
                        let invocation = skill_invocation(name, path);
                        invocation.replace_in(&text, &format!("/{name}"))
                    }
                    _ => text,
                });
                content.push(json!({"type":"text","text":text}));
            }
            op::Input::Skill { name, path } => {
                let invocation = skill_invocation(name, path);
                if !input
                    .iter()
                    .any(|part| matches!(part,op::Input::Text {text} if invocation.is_in(text)))
                {
                    content.push(json!({"type":"text","text":format!("/{name}")}));
                }
            }
            op::Input::Mention { path, .. } => {
                content.push(json!({"type":"text","text":format!("添付ファイル: {path}")}))
            }
            op::Input::LocalImage { path } => {
                let bytes = tokio::fs::read(path)
                    .await
                    .map_err(|error| format!("画像を読み込めません: {error}"))?;
                let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    "image/png"
                } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                    "image/jpeg"
                } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
                    "image/gif"
                } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                    "image/webp"
                } else {
                    return Err("Claudeに送る画像はPNG・JPEG・GIF・WebPを使用してください。".into());
                };
                content.push(json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":STANDARD.encode(bytes)}}));
            }
        }
    }
    Ok(content)
}

fn updated_at(thread: &Thread) -> u64 {
    thread
        .updated_at
        .as_ref()
        .map(|number| *number as u64)
        .unwrap_or_default()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[async_trait::async_trait]
impl crate::host_rpc::agent::Identity for Claude {
    async fn list(&self) -> Result<op::Accounts, crate::host_rpc::service::Failure> {
        let (accounts, selected) =
            self.accounts.lock().await.list().await.map_err(|e| {
                crate::host_rpc::service::Failure::new("account_operation_failed", e)
            })?;
        Ok(op::Accounts {
            accounts,
            selected: selected
                .map(|id| (ProviderKind::Claude, id))
                .into_iter()
                .collect(),
            error: None,
        })
    }
    async fn account(
        &self,
        command: crate::host_rpc::agent::AccountCommand,
    ) -> Result<crate::host_rpc::agent::AccountReply, crate::host_rpc::service::Failure> {
        self.accounts
            .lock()
            .await
            .request(command)
            .await
            .map_err(|e| crate::host_rpc::service::Failure::new("account_operation_failed", e))
    }
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, crate::host_rpc::service::Failure> {
        let fetch =
            self.accounts.lock().await.usage_request(id).map_err(|e| {
                crate::host_rpc::service::Failure::new("account_operation_failed", e)
            })?;
        Ok(fetch.await)
    }
}

struct RequestSource {
    input: tokio::sync::mpsc::Sender<Command>,
    answers: crate::host_rpc::requests::NativeAnswers,
}
pub(crate) fn request_origin(
    instance: uuid::Uuid,
    native_id: Value,
    input: tokio::sync::mpsc::Sender<Command>,
    answers: crate::host_rpc::requests::NativeAnswers,
) -> crate::host_rpc::requests::RequestOrigin {
    crate::host_rpc::requests::RequestOrigin {
        instance,
        native_id,
        provider: ProviderKind::Claude,
        source: std::sync::Arc::new(RequestSource { input, answers }),
    }
}
#[async_trait::async_trait]
impl crate::host_rpc::requests::AnswerSource for RequestSource {
    fn is_alive(&self) -> bool {
        !self.input.is_closed()
    }
    async fn prepare(
        &self,
        native_id: &Value,
        body: &agent_protocol::requests::RequestBody,
        answer: &agent_protocol::requests::Answer,
    ) -> Result<AnswerWrite, Failure> {
        let request_id = native_id
            .as_str()
            .ok_or_else(|| Failure::new("invalid_answer", "request ID must be a string"))?
            .to_owned();
        let result = self
            .answers
            .translate(body, answer)
            .map_err(|e| Failure::new("invalid_answer", e))?;
        let permit = self
            .input
            .clone()
            .reserve_owned()
            .await
            .map_err(|_| Failure::new("answer_not_sent", "agent input is closed"))?;
        Ok(async move {
            let (delivered, receipt) = tokio::sync::oneshot::channel();
            permit.send(Command { value: json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":result}}), user: None, delivered: Some(delivered) });
            tokio::time::timeout(std::time::Duration::from_secs(15), receipt).await.map_err(|_| Failure::unknown("answer_delivery_unknown", "answer delivery timed out"))?
                .map_err(|_| Failure::unknown("answer_delivery_unknown", "agent exited before confirming the answer write"))?
                .map_err(|e| Failure::unknown("answer_delivery_unknown", e))
        }.boxed())
    }
}

#[async_trait::async_trait]
impl Agent for Claude {
    fn running_input(&self) -> crate::host_rpc::submission::RunningInput {
        crate::host_rpc::submission::RunningInput::Queue
    }
    fn capabilities(&self) -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            additional_input: true,
            fork: false,
            rename: false,
            model_change: true,
        }
    }
    fn validate_create(&self) -> Result<(), Failure> {
        if self.stop.is_cancelled() {
            return Err(Failure::new(
                "provider_unavailable",
                "agent is shutting down",
            ));
        }
        Ok(())
    }
    fn availability(&self) -> Result<(), Failure> {
        self.validate_create()?;
        let program = self.program.as_path();
        #[cfg(windows)]
        let program = if program.extension().is_none() {
            std::borrow::Cow::Owned(program.with_extension("exe"))
        } else {
            std::borrow::Cow::Borrowed(program)
        };
        let available = if program.components().count() > 1 {
            program.is_file()
        } else {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path)
                    .any(|directory| directory.join(program.as_os_str()).is_file())
            })
        };
        if !available {
            return Err(Failure::new(
                "provider_unavailable",
                "agent executable is missing",
            ));
        }
        Ok(())
    }
    fn storage_directory(&self) -> &Path {
        &self.native_home
    }
    async fn list(&self, search: &str, cursor: Option<String>) -> Result<SessionPage, Failure> {
        if cursor.is_some() {
            return Err(Failure::new("invalid_cursor", "unexpected session cursor"));
        }
        Ok(SessionPage {
            data: Claude::list(self, search)
                .await
                .map_err(|e| Failure::new("session_list_failed", e))?,
            next_cursor: None,
        })
    }
    async fn open(&self, id: &str, limit: usize) -> Result<ThreadResponse, Failure> {
        self.read(id, limit)
            .await
            .map_err(|e| Failure::new("session_read_failed", e))
    }
    async fn read_history(
        &self,
        _id: &str,
        _cursor: &str,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        Err(Failure::new(
            "invalid_cursor",
            "Claude history does not issue timeline cursors",
        ))
    }
    async fn read_item(&self, params: &op::ReadItem) -> Result<op::ItemResponse, Failure> {
        Claude::read_item(self, params).await.map_err(Into::into)
    }
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        _browser: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        Claude::create(self, cwd, model.unwrap_or("default"))
            .await
            .map_err(|e| Failure::new("provider_unavailable", e))
    }
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure> {
        let mut response = self
            .read(id, 1)
            .await
            .map_err(|e| Failure::new("session_read_failed", e))?;
        if let Some(record) = self.records.lock().await.get(id).cloned() {
            let record = record.lock().await;
            if let Some(running) = &record.running {
                response.thread.status = SessionStatus::Running;
                response.thread.turns = Some(vec![Arc::new(Turn {
                    id: running.turn_id.clone(),
                    status: TurnStatus::Running,
                    ..Default::default()
                })]);
            }
        }
        Ok(SubmissionState {
            response,
            needs_reload: false,
        })
    }
    async fn submit(
        &self,
        input: &op::Submission,
        route: crate::host_rpc::submission::SubmissionTarget<'_>,
        _reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use crate::host_rpc::submission::SubmissionTarget;
        let turn_id = match route {
            SubmissionTarget::Steer(_) => {
                return Err(Failure::new(
                    "unsupported_operation",
                    "this provider accepts queued input",
                ));
            }
            SubmissionTarget::Queue => Some(
                self.additional_input(
                    &input.thread_id.id,
                    &input.input,
                    &input.client_user_message_id,
                )
                .await?,
            ),
            SubmissionTarget::Start { .. } => Some(self.start_turn(input, browser).await?),
        };
        Ok(op::SubmissionReceipt { turn_id })
    }
    async fn interrupt(
        &self,
        id: &str,
        turn: &agent_protocol::ids::TurnId,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        Claude::interrupt(self, id, turn).await?;
        Ok(agent_protocol::models::Empty {})
    }
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        Ok(op::ModelPage {
            data: if params.cursor.is_none() {
                Claude::models(self)
                    .await
                    .map_err(|e| Failure::new("models_unavailable", e))?
            } else {
                Vec::new()
            },
            next_cursor: None,
            provider_errors: None,
        })
    }
    async fn catalog(&self, cwd: &str) -> agent_protocol::composer::ComposerCatalog {
        use agent_protocol::composer::{ComposerCandidate, ComposerCatalog};
        let mut catalog = ComposerCatalog {
            cwd: cwd.into(),
            ..Default::default()
        };
        if self.availability().is_err() {
            return catalog;
        }
        let candidates = async {
            let Some(auth_home) = self.accounts.lock().await.selected_home()? else {
                return Ok(Vec::new());
            };
            let directory = if cwd.is_empty() {
                self.directory.as_path()
            } else {
                Path::new(cwd)
            };
            let _permit = self
                .processes
                .clone()
                .try_acquire_owned()
                .map_err(|error| error.to_string())?;
            let (process, initialized) = Process::start(
                &self.program,
                &self.native_home,
                &auth_home,
                directory,
                None,
                None,
                None,
            )
            .await?;
            if let Err(error) = process.finish().await {
                catalog
                    .errors
                    .entry(ProviderKind::Claude)
                    .or_default()
                    .push(error);
            }
            let commands = initialized["commands"]
                .as_array()
                .ok_or("スキル一覧を取得できませんでした。")?;
            Ok::<_, String>(
                commands
                    .iter()
                    .filter_map(|command| {
                        let name = command["name"].as_str()?;
                        Some(ComposerCandidate {
                            invocation: skill_invocation(name, &format!("skill://claude/{name}")),
                            description: command["description"].as_str().unwrap_or_default().into(),
                        })
                    })
                    .collect(),
            )
        }
        .await;
        match candidates {
            Ok(candidates) => catalog.candidates = candidates,
            Err(error) => {
                catalog
                    .errors
                    .entry(ProviderKind::Claude)
                    .or_default()
                    .push(error);
            }
        }
        catalog
    }
    async fn active_sessions_in(&self, dir: &Path) -> Result<Vec<SessionRef>, Failure> {
        let threads = Claude::list(self, "")
            .await
            .map_err(|e| Failure::new("session_list_failed", e))?;
        let mut active = Vec::new();
        for summary in threads {
            let thread = summary.thread;
            if let Some(cwd) = &thread.cwd {
                let cwd = tokio::fs::canonicalize(cwd)
                    .await
                    .unwrap_or_else(|_| cwd.into());
                if dunce::simplified(&cwd).starts_with(dir)
                    && let Some(id) = thread.id
                {
                    if thread.status == SessionStatus::Running {
                        active.push(id);
                    } else {
                        let record = self.records.lock().await.get(&id.id).cloned();
                        let observed_idle = if let Some(record) = record {
                            record.lock().await.idle.is_some()
                        } else {
                            false
                        };
                        if !observed_idle {
                            return Err(Failure::new(
                                "activity_unavailable",
                                "native transcript does not expose external process activity",
                            ));
                        }
                    }
                }
            }
        }
        Ok(active)
    }
    async fn discard_workspace_processes(&self, dir: &Path) -> Result<(), Failure> {
        Claude::discard_workspace_processes(self, dir)
            .await
            .map_err(|e| Failure::new("workspace_resume_failed", e))
    }
    async fn read_permissions(
        &self,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        crate::host_rpc::permissions::read_claude_permissions(&self.native_home)
    }
    async fn update_permissions(
        &self,
        mode: agent_protocol::permissions::PermissionMode,
        version: &str,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        crate::host_rpc::permissions::update_claude_permissions(&self.native_home, mode, version)
    }
    async fn fork(
        &self,
        _id: &str,
        _turn: &str,
        _browser: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        Err(Failure::new(
            "unsupported_operation",
            "fork is unsupported by this provider",
        ))
    }
    async fn rename(
        &self,
        _id: &str,
        _name: &str,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        Err(Failure::new(
            "unsupported_operation",
            "rename is unsupported by this provider",
        ))
    }
    fn event_stream(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.event_receiver
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
    async fn shutdown(&self) {
        self.stop.cancel();
        let _ = self.accounts.lock().await.cancel().await;
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(target: "bex", operation = "claude.worker", message = %error);
            }
        }
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;

    #[tokio::test]
    async fn queued_input_requires_write_confirmation_to_prove_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let claude = Claude::load(
            root.join("unused"),
            root.join("adapter"),
            Some(root.join("native")),
        )
        .await
        .unwrap();
        let session = Claude::create(&claude, root.to_str().unwrap(), "default")
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        let record = claude.record(&session.id).await.unwrap();
        for enqueued in [false, true] {
            let (input, mut receiver) = mpsc::channel(1);
            let (_interrupt, interrupt) = watch::channel(None);
            record.lock().await.running = Some(Running {
                turn_id: "turn".into(),
                input,
                interrupt,
            });
            if !enqueued {
                receiver.close();
            }
            let parts = [op::Input::Text {
                text: "additional input".into(),
            }];
            let (result, ()) = tokio::join!(
                claude.additional_input(&session.id, &parts, "input"),
                async move {
                    if enqueued {
                        // Once accepted by the worker queue, closing the receipt
                        // does not establish whether the native write happened.
                        drop(receiver.recv().await.unwrap());
                    }
                },
            );
            assert_eq!(
                result.unwrap_err().delivery,
                if enqueued {
                    agent_transport::peer::Delivery::Unknown
                } else {
                    agent_transport::peer::Delivery::NotSent
                }
            );
        }
    }

    #[test]
    fn final_blocks_match_content_kind_and_native_message_before_falling_back() {
        let text = |id: &str, status, value: &str| {
            Arc::new(Item::new(
                id.into(),
                status,
                ItemBody::AssistantText {
                    text: value.into(),
                    phase: AssistantPhase::Unknown,
                },
            ))
        };
        let thinking = |id: &str, content| {
            Arc::new(Item::new(
                id.into(),
                ItemStatus::Running,
                ItemBody::Reasoning {
                    content,
                    summary: vec![],
                },
            ))
        };
        let items = [
            text("other:0", ItemStatus::Running, "changed"),
            text("message:0", ItemStatus::Completed, "same"),
            text("message:1", ItemStatus::Running, "same"),
            thinking("message:2", vec!["one".into(), "two".into()]),
            text("message:3", ItemStatus::Running, "later"),
            thinking("message:4", vec!["later".into()]),
        ];
        for (block, expected) in [
            (json!({"type":"text","text":"same"}), "message:1"),
            (json!({"type":"text","text":"changed"}), "message:1"),
            (json!({"type":"text","text":"later"}), "message:3"),
            (json!({"type":"thinking","thinking":"onetwo"}), "message:2"),
            (json!({"type":"thinking","thinking":"changed"}), "message:2"),
            (json!({"type":"thinking","thinking":"later"}), "message:4"),
        ] {
            assert_eq!(
                message_item_id(&items, "message", 0, 1, &block),
                expected,
                "{block}"
            );
        }
        assert_eq!(
            message_item_id(
                &items,
                "message",
                3,
                2,
                &json!({"type":"text","text":"same"})
            ),
            "message:3"
        );
        assert_eq!(
            message_item_id(&items, "message", 3, 1, &json!({"type":"future"})),
            "message:3"
        );
        assert_eq!(
            message_item_id(
                &items[..2],
                "message",
                3,
                1,
                &json!({"type":"text","text":"changed"})
            ),
            "message:3"
        );
        assert_eq!(
            message_item_id(
                &items[..2],
                "message",
                3,
                1,
                &json!({"type":"text","text":"same"})
            ),
            "message:0"
        );
    }
    #[tokio::test]
    async fn late_blocks_and_api_retry_preserve_only_valid_stream_items() {
        let router = crate::host_rpc::routing::SessionRouter::new();
        let (events, mut receiver) = mpsc::channel::<AgentEvent>(256);
        let consumer = router.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let result = event.change.apply(&consumer);
                if let Some(applied) = event.applied {
                    let _ = applied.send(result);
                }
            }
        });
        let uuid = Uuid::new_v4();
        let session = SessionRef::new(ProviderKind::Claude, uuid.to_string()).unwrap();
        router.session_change(
            &session,
            SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "turn".into(),
                    ..Default::default()
                },
                completed: false,
            },
        );
        let (input, _receiver) = mpsc::channel(1);
        let (interrupt, _) = watch::channel(None);
        let mut worker = Worker {
            instance: Uuid::new_v4(),
            auth_revision: 0,
            record: Arc::new(AsyncMutex::new(Record {
                cwd: "/work".into(),
                model: "default".into(),
                session_id: uuid,
                resumable: false,
                running: None,
                idle: None,
            })),
            events,
            live: agent_protocol::session::Timeline {
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            session: session.clone(),
            turn_id: "turn".into(),
            input,
            stop: CancellationToken::new(),
            stream: None,
            interrupt,
            model: "default".into(),
            effort: None,
        };
        for child in [
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"child"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}}}),
            json!({"type":"assistant","error":"authentication_failed","message":{"id":"child","content":[{"type":"text","text":"child answer"}]}}),
        ] {
            let mut child = child;
            child["parent_tool_use_id"] = "subagent-tool".into();
            worker.message(child).await.unwrap();
        }
        assert!(worker.stream.is_none());
        assert!(worker.current_turn().unwrap().error.is_none());
        for content in [
            json!("echoed input"),
            json!([
                {"type":"text","text":"echoed input"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}
            ]),
        ] {
            worker
                .message(json!({"type":"user","uuid":"user-echo","message":{"content":content}}))
                .await
                .unwrap();
        }
        assert!(
            worker
                .current_turn()
                .unwrap()
                .items
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        );
        for event in [
            json!({"type":"message_start","message":{"id":"message"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking"}}),
        ] {
            worker.stream_event(&event).await.unwrap();
        }
        worker.message(json!({"type":"assistant","uuid":"envelope","message":{"id":"message","content":[{"type":"text","text":"answer"}]}})).await.unwrap();
        worker.stream_event(&json!({"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"abandoned"}})).await.unwrap();
        let turn = router.current_turn(&session, "turn").unwrap();
        let items = turn.items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.as_str(), "message:0");
        assert_eq!(items[0].status, ItemStatus::Completed);
        assert!(
            matches!(items[1].body(),ItemBody::Reasoning {content,..} if content == &["abandoned"])
        );
        worker
            .message(json!({"type":"system","subtype":"api_retry","attempt":1}))
            .await
            .unwrap();
        let turn = router.current_turn(&session, "turn").unwrap();
        let items = turn.items.as_ref().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), "message:0");
        assert!(worker.stream.is_none());
        let attachments = [
            json!({"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification><status>failed</status></task-notification>"}),
            json!({"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"source_uuid":"queued-input","prompt":"additional input"}),
            json!({"type":"hook_success","stdout":"hook output"}),
            json!({"type":"environment","snapshot":{"cwd":"/work"}}),
        ];
        let mut rows = vec![
            json!({"type":"assistant","uuid":"answer","parentUuid":null,"cwd":"/work",
            "message":{"id":"message","content":[{"type":"text","text":"answer"}]}}),
        ];
        let mut parent = "answer".to_owned();
        for (index, attachment) in attachments.iter().enumerate() {
            let id = format!("attachment-{index}");
            worker
                .message(json!({"type":"attachment","uuid":id,"attachment":attachment}))
                .await
                .unwrap();
            rows.push(
                json!({"type":"attachment","uuid":id,"parentUuid":parent,"attachment":attachment}),
            );
            parent = id;
        }
        let call = json!({"type":"assistant","uuid":"tool-call","parentUuid":parent,
            "message":{"id":"tool-message","content":[{"type":"tool_use","id":"command","name":"Bash","input":{"command":"false"}}]}});
        let result = json!({"type":"user","uuid":"tool-result","parentUuid":"tool-call",
            "message":{"content":[{"type":"tool_result","tool_use_id":"command","content":"failed","is_error":true}]},
            "tool_use_result":{"exitCode":7}});
        worker.message(call.clone()).await.unwrap();
        worker.message(result.clone()).await.unwrap();
        rows.push(call);
        let mut saved_result = result;
        saved_result["toolUseResult"] = saved_result
            .as_object_mut()
            .unwrap()
            .remove("tool_use_result")
            .unwrap();
        rows.push(saved_result);
        let mut parent = "tool-result".to_owned();
        for (index, status) in ["completed", "failed", "stopped"].iter().enumerate() {
            let id = format!("background-{index}");
            let call = json!({"type":"assistant","uuid":format!("call-{id}"),"parentUuid":parent,
                "message":{"id":format!("message-{id}"),"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":"work"}}]}});
            let result = json!({"type":"user","uuid":format!("result-{id}"),"parentUuid":format!("call-{id}"),
                "message":{"content":[{"type":"tool_result","tool_use_id":id,"content":"launched"}]},
                "tool_use_result":{"backgroundTaskId":id}});
            worker.message(call.clone()).await.unwrap();
            worker.message(result.clone()).await.unwrap();
            let notice = json!({"type":"system","subtype":"task_notification","tool_use_id":id,"status":status,"summary":"work & result <details>"});
            for flag in ["skip_transcript", "ambient"] {
                let mut ambient = notice.clone();
                ambient[flag] = true.into();
                worker.message(ambient).await.unwrap();
                assert_eq!(
                    worker
                        .current_turn()
                        .unwrap()
                        .items
                        .as_ref()
                        .unwrap()
                        .last()
                        .unwrap()
                        .status,
                    ItemStatus::Running
                );
            }
            worker.message(notice.clone()).await.unwrap();
            worker.message(notice).await.unwrap();
            rows.push(call);
            let mut saved_result = result;
            saved_result["toolUseResult"] = saved_result
                .as_object_mut()
                .unwrap()
                .remove("tool_use_result")
                .unwrap();
            rows.push(saved_result);
            parent = format!("notice-{id}");
            rows.push(json!({"type":"attachment","uuid":parent,"parentUuid":format!("result-{id}"),
                "attachment":{"type":"queued_command","commandMode":"task-notification",
                    "prompt":format!("<task-notification><tool-use-id>{id}</tool-use-id><status>{status}</status><summary>work & result <details></summary></task-notification>")}}));
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(format!("{uuid}.jsonl"));
        std::fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let history = history::read(&path, 10).unwrap();
        let saved = history.thread.turns.unwrap();
        let live = router.current_turn(&session, "turn").unwrap();
        assert_eq!(live.items, saved[0].items);
        let items = live.items.as_ref().unwrap();
        assert_eq!(items.len(), 8);
        assert_eq!(items[4].status, ItemStatus::Failed);
        assert!(
            matches!(items[4].body(), ItemBody::CommandExecution {output, exit_code: Some(7), ..} if output == "failed")
        );
        for (item, status) in items[5..].iter().zip([
            ItemStatus::Completed,
            ItemStatus::Failed,
            ItemStatus::Interrupted,
        ]) {
            assert_eq!(item.status, status);
            assert!(
                matches!(item.body(), ItemBody::CommandExecution {output, exit_code:None, ..} if output == "work & result <details>")
            );
        }
    }
    #[test]
    fn native_error_evidence_distinguishes_quota_retry_auth_and_warning() {
        for (message, category) in [
            (
                json!({"error":"authentication_failed"}),
                ErrorCategory::Auth,
            ),
            (json!({"api_error_status":429}), ErrorCategory::RateLimited),
            (json!({"api_error_status":503}), ErrorCategory::Overloaded),
            (
                json!({"subtype":"error_max_turns"}),
                ErrorCategory::SessionLimit,
            ),
            (
                json!({"error":"invalid_request"}),
                ErrorCategory::InvalidInput,
            ),
        ] {
            assert_eq!(execution_error(&message, false).category, category);
        }
        let retry = execution_error(&json!({"error":"overloaded","error_status":503}), true);
        assert_eq!(
            retry.retry,
            Some(RetryEvidence {
                retrying: true,
                overloaded: true,
            })
        );
        for status in ["allowed", "allowed_warning"] {
            assert!(
                usage_limit_error(
                    &json!({"status":status,"rateLimitType":"five_hour","utilization":0.9})
                )
                .is_none()
            );
        }
        let limit = usage_limit_error(
            &json!({"status":"rejected","rateLimitType":"five_hour","resetsAt":123456}),
        )
        .unwrap();
        assert_eq!(limit.category, ErrorCategory::UsageLimit);
        assert_eq!(limit.provider_code.as_deref(), Some("five_hour"));
        let future = json!({"code":"future_failure","httpStatus":null});
        assert_eq!(
            execution_error(&json!({"error":"future_failure"}), false).category,
            ErrorCategory::Provider(future)
        );
    }
}
