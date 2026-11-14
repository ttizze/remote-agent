//! Claude Code owns inference, credentials and its transcript. The Host owns
//! the client-facing conversation and adapts official Agent SDK events.
use super::refreshed::{DISCOVERY_FRESH, Refreshed};
use agent_protocol::{execution::*, items::*};
mod accounts;
mod response;
use response::{InputDelivery, MessageDisposition, ResponseState};
mod history;
mod native;
mod permissions;
mod process;
mod usage;

use anyhow::Context;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agent_protocol::operations as op;

use agent_protocol::models::Item;

use agent_protocol::models::Model;

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

use crate::providers::{
    Agent, AgentChange, AgentEvent, AnswerWrite, Failure, SessionListScope, SessionPage,
    SessionSummary, SubmissionState, emit,
};
use futures_util::FutureExt;
use process::Event as SdkEvent;
pub(crate) use process::Input as SdkInput;
use process::Process;
pub(crate) use process::SdkRequest;

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

pub struct Claude {
    program: PathBuf,
    directory: PathBuf,
    native_home: PathBuf,
    native_inherited: bool,
    accounts: AsyncMutex<accounts::Accounts>,
    account_discovery: AsyncMutex<()>,
    records: AsyncMutex<HashMap<String, Arc<AsyncMutex<Record>>>>,
    processes: Arc<Semaphore>,
    events: mpsc::Sender<AgentEvent>,
    event_receiver: std::sync::Mutex<Option<mpsc::Receiver<AgentEvent>>>,
    stop: CancellationToken,
    workers: AsyncMutex<tokio::task::JoinSet<()>>,
    /// Discovery launches the CLI: models per account home, skills also per directory.
    models: Refreshed<PathBuf, Vec<Model>>,
    commands: Refreshed<(PathBuf, PathBuf), Vec<agent_protocol::composer::ComposerCandidate>>,
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

impl Idle {
    async fn close(self) -> Result<(), String> {
        self.released.cancel();
        self.process.finish().await
    }
}

struct Running {
    turn_id: agent_protocol::ids::TurnId,
    input: mpsc::Sender<Command>,
    interrupt: watch::Receiver<Option<(String, Result<(), String>)>>,
    effort: Option<String>,
    work: watch::Receiver<Work>,
}

/// What the worker's process is doing beside its turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Work {
    /// Claude runs background work that a new process would end.
    background: bool,
    /// Claude answered; no turn runs until it wakes or input arrives.
    settled: bool,
}

/// T3 Code's ClaudeBackgroundWorkBlocksQueryReplacementError: Claude applies a
/// model or effort only to a new process, which ends its background work.
const BACKGROUND_BLOCKS_CHANGE: &str = "Claudeのバックグラウンドのエージェントやコマンドが実行中のため、モデルや設定を変更するとそれらが終了します。完了を待つか停止してから、もう一度送信してください。: Claude is still running background agents or commands, and this model or setting change would end them. Wait for them to finish, or press Stop, then send the message again.";

pub(crate) struct Command {
    pub(crate) execution: Option<Uuid>,
    pub(crate) value: SdkInput,
    pub(crate) user: Option<Item>,
    /// Confirms the write; the SDK may not have assigned the input's turn yet.
    pub(crate) delivered:
        Option<tokio::sync::oneshot::Sender<Result<Option<agent_protocol::ids::TurnId>, String>>>,
}

impl Drop for Claude {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Claude {
    pub async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        agent_local_host::create_state_directory(&directory)?;
        let native_inherited = native_home.is_none();
        let native_home = native_home.map(Ok).unwrap_or_else(history::home)?;
        let path = std::env::var_os("PATH");
        let program = process::executable(&program, path.as_deref()).unwrap_or(program);
        let accounts = accounts::Accounts::load(
            program.clone(),
            directory.join("accounts"),
            native_home.clone(),
            native_inherited,
        )
        .await?;
        let (events, event_receiver) = mpsc::channel(256);
        Ok(Self {
            accounts: AsyncMutex::new(accounts),
            account_discovery: AsyncMutex::new(()),
            program,
            directory,
            native_home,
            native_inherited,
            records: AsyncMutex::new(HashMap::new()),
            processes: Arc::new(Semaphore::new(8)),
            events,
            event_receiver: std::sync::Mutex::new(Some(event_receiver)),
            stop: CancellationToken::new(),
            workers: AsyncMutex::new(tokio::task::JoinSet::new()),
            models: Refreshed::new(DISCOVERY_FRESH),
            commands: Refreshed::new(DISCOVERY_FRESH),
        })
    }

    /// Replaces the freshness window of CLI discovery.
    pub fn refresh_discovery_after(mut self, fresh: Duration) -> Self {
        (self.models, self.commands) = (Refreshed::new(fresh), Refreshed::new(fresh));
        self
    }

    async fn models(&self) -> Result<Vec<Model>, String> {
        if self.availability().is_err() {
            return Ok(Vec::new());
        }
        let Some(auth_home) = self.accounts.lock().await.selected_home()? else {
            return Ok(Vec::new());
        };
        let (program, directory) = (self.program.clone(), self.directory.clone());
        let (native_home, native_inherited) = (self.native_home.clone(), self.native_inherited);
        self.models
            .read(auth_home.clone(), move || async move {
                let cwd = tempfile::tempdir_in(&directory).map_err(|error| error.to_string())?;
                let probe = Process::probe(
                    &program,
                    process::homes(&native_home, native_inherited, &auth_home),
                    cwd.path(),
                )
                .await?;
                let entries = probe.initialized["models"]
                    .as_array()
                    .ok_or("Claude Code did not return a model catalog")?;
                native::parse_models(entries)
            })
            .await
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
                    provider: Some(ProviderKind::Claude),
                    id: session_id.to_string(),
                }),
                cwd: Some(cwd.to_string_lossy().into_owned()),
                status: SessionStatus::Idle,
                turns: Some(Vec::new()),
                updated_at: Some(now_ms() as f64 / 1000.),
                ..Default::default()
            },
            model: Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: model.into(),
            }),
            ..Default::default()
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

    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        owned: Option<HashSet<String>>,
        limit: u32,
    ) -> anyhow::Result<SessionPage> {
        let home = self.native_home.clone();
        let running = self
            .loaded_threads()
            .await
            .into_iter()
            .filter(|thread| thread.status == SessionStatus::Running)
            .map(|mut thread| {
                thread.updated_at = Some(now_ms() as f64 / 1000.);
                thread
            })
            .collect();
        let search = search.to_owned();
        tokio::task::spawn_blocking(move || {
            history::list_page(
                &home,
                running,
                &search,
                cursor.as_deref(),
                owned.as_ref(),
                limit,
            )
        })
        .await?
    }

    async fn loaded_threads(&self) -> Vec<Thread> {
        let records: Vec<_> = self.records.lock().await.values().cloned().collect();
        let mut threads = Vec::with_capacity(records.len());
        for record in records {
            let record = record.lock().await;
            threads.push(Thread {
                id: Some(SessionRef {
                    provider: Some(ProviderKind::Claude),
                    id: record.session_id.to_string(),
                }),
                cwd: Some(record.cwd.clone()),
                status: if record.running.is_some() {
                    SessionStatus::Running
                } else {
                    SessionStatus::Idle
                },
                ..Default::default()
            });
        }
        threads
    }

    /// A missing limit reads metadata only; opening history overlays owned execution later.
    async fn read(&self, id: &str, requested: Option<usize>) -> anyhow::Result<ThreadResponse> {
        let native = Uuid::parse_str(id)?;
        let home = self.native_home.clone();
        let history: anyhow::Result<ThreadResponse> = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            let Some(requested) = requested else {
                return Ok(ThreadResponse {
                    thread: history::summary(&path)?.thread,
                    ..Default::default()
                });
            };
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
                        ..Default::default()
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
                let record = self.records.lock().await.get(id).cloned();
                let record = match record {
                    Some(record) => record.lock_owned().await,
                    None => return Err(error),
                };
                let mut thread = Thread {
                    id: Some(SessionRef {
                        provider: Some(ProviderKind::Claude),
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
                    ..Default::default()
                })
            }
        }
    }

    async fn read_item(&self, params: &op::ReadItem) -> Result<Item, OperationError> {
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
        let turns = response.thread.turns.as_deref().unwrap_or_default();
        let (turn, item) =
            agent_protocol::session::item_position(turns, &params.turn_id, &params.item_id)
                .ok_or("Claude native history item is unavailable")?;
        let item = &turns[turn].items.as_ref().unwrap()[item];
        let mut item = (**item).clone();
        if let Some(path) = native_history.output_paths.get(&item.id) {
            let content = Value::String(read_tool_output(path).await?);
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
        Ok(item)
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
                idle.close().await.map_err(anyhow::Error::msg)?;
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
        let request_id = Uuid::new_v4().to_string();
        input
            .send(Command {
                execution: None,
                value: SdkInput::Interrupt {
                    request_id: request_id.clone(),
                },
                user: None,
                delivered: None,
            })
            .await
            .map_err(|_| "Claude SDK input is closed")?;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Some((id, result)) = interrupt.borrow().clone()
                    && id == request_id
                {
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

    /// Whether a worker still takes input. One that stopped taking it is
    /// finishing; its record is released for the next turn shortly.
    async fn takes_input(&self, id: &str) -> bool {
        let Some(record) = self.records.lock().await.get(id).cloned() else {
            return false;
        };
        loop {
            match &record.lock().await.running {
                None => return false,
                Some(running) if !running.input.is_closed() => return true,
                Some(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Sends input to the running process: a steer into the running turn,
    /// which takes it at once and stops its streaming and running tools, as
    /// T3 Code steers Claude; without one, input after a settled answer, which
    /// starts the next turn while background work runs on.
    async fn send_running(
        &self,
        params: &op::Submission,
        steer: Option<&str>,
        execution: Uuid,
    ) -> Result<Option<agent_protocol::ids::TurnId>, OperationError> {
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        let (input, client_id) = (&params.input, params.client_user_message_id.as_str());
        let record = self.record(&params.thread_id.id).await?;
        let content: Value = input_content(input).await?.into();
        let state = record.lock().await;
        let running = state
            .running
            .as_ref()
            .ok_or("Claudeは実行中ではありません。")?;
        if steer.is_some_and(|turn| running.turn_id.as_str() != turn) {
            return Err("Claudeの実行対象が変わりました。会話を更新してください。".into());
        }
        let changes_process = params
            .model
            .as_ref()
            .is_some_and(|model| model.id != state.model)
            || params
                .effort
                .as_deref()
                .is_some_and(|effort| Some(effort) != running.effort.as_deref());
        if changes_process && running.work.borrow().background {
            return Err(BACKGROUND_BLOCKS_CHANGE.into());
        }
        let sender = running.input.clone();
        drop(state);
        let id = client_id.to_owned();
        let (delivered, receipt) = tokio::sync::oneshot::channel();
        sender
            .send(Command {
                execution: Some(execution),
                value: if steer.is_some() {
                    SdkInput::Steer {
                        id: id.clone(),
                        content,
                    }
                } else {
                    SdkInput::Message {
                        id: id.clone(),
                        content,
                    }
                },
                user: Some(Item {
                    attempt: None,
                    author: None,
                    id: id.clone().into(),
                    status: ItemStatus::Unknown,
                    client_input_id: Some(client_id.into()),
                    body: ItemContent::Inline {
                        body: Box::new(ItemBody::UserMessage {
                            text: None,
                            content: op::Input::message_parts(input),
                        }),
                    },
                }),
                delivered: Some(delivered),
            })
            .await
            .map_err(|_| "Claude SDK input is closed")?;
        tokio::time::timeout(std::time::Duration::from_secs(15), receipt)
            .await
            .map_err(|_| OperationError {
                message: "Claude input delivery is unknown".into(),
                delivery: agent_transport::peer::Delivery::Unknown,
            })?
            .map_err(|_| OperationError {
                message: "Claude exited before confirming the input".into(),
                delivery: agent_transport::peer::Delivery::Unknown,
            })?
            .map_err(|message| OperationError {
                message,
                delivery: agent_transport::peer::Delivery::Unknown,
            })
    }

    async fn start_turn(
        &self,
        params: &op::Submission,
        browser: Option<Value>,
        execution: Uuid,
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
                process::homes(&self.native_home, self.native_inherited, &auth_home),
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
            attempt: None,
            author: None,
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
            started_at_ms: Some(now_ms()),
            ..Default::default()
        };
        let mut workers = self.workers.lock().await;
        if self.stop.is_cancelled() {
            return Err("Host is shutting down".into());
        }
        if let Err(error) = process
            .write(&SdkInput::Message {
                id: params.client_user_message_id.to_string(),
                content: content.into(),
            })
            .await
        {
            return Err(OperationError {
                message: error,
                delivery: agent_transport::peer::Delivery::Unknown,
            });
        }
        state.model = model.clone();
        let (input, receiver) = mpsc::channel(32);
        let (interrupt, interrupted) = watch::channel(None);
        let (work, work_observed) = watch::channel(Work::default());
        state.running = Some(Running {
            turn_id: turn_id.clone(),
            input: input.clone(),
            interrupt: interrupted,
            effort: effort.map(str::to_owned),
            work: work_observed,
        });
        drop(state);
        let target = SessionRef {
            provider: Some(ProviderKind::Claude),
            id: params.thread_id.id.clone(),
        };
        let worker = Worker {
            execution,
            turn_scopes: [(turn_id.clone(), execution)].into(),
            input_scopes: Default::default(),
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
            work,
            waiting: Vec::new(),
            reports: Vec::new(),
            wake_frame: None,
            model,
            effort: effort.map(str::to_owned),
            auth_revision,
        };
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                tracing::error!(target: "agent", operation = "claude.worker", message = %error);
            }
        }
        workers.spawn(worker.run(process, receiver));
        Ok(turn_id)
    }
}

struct Worker {
    execution: Uuid,
    turn_scopes: HashMap<agent_protocol::ids::TurnId, Uuid>,
    input_scopes: HashMap<agent_protocol::ids::ItemId, Uuid>,
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
    interrupt: watch::Sender<Option<(String, Result<(), String>)>>,
    work: watch::Sender<Work>,
    /// The last published background roster, including while the turn is running.
    waiting: Vec<BackgroundTask>,
    /// Background work that ended while Claude waited, keyed by its task, for
    /// the notification that starts the turn Claude wakes for.
    reports: Vec<(String, BackgroundReport)>,
    /// The notification frame that started the current turn.
    wake_frame: Option<String>,
    model: String,
    effort: Option<String>,
}

async fn read_tool_output(path: &str) -> Result<String, String> {
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
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

async fn read_tool_output_tail(path: &str) -> Result<(String, bool), String> {
    use agent_protocol::sync::TAIL_CHARS;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| error.to_string())?;
    let length = file
        .metadata()
        .await
        .map_err(|error| error.to_string())?
        .len();
    let offset = length.saturating_sub((TAIL_CHARS * 4 * 3) as u64);
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(length - offset)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    let boundary = if offset == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(bytes.len())
    };
    Ok((
        String::from_utf8_lossy(&bytes[boundary..]).into_owned(),
        offset != 0,
    ))
}

impl Worker {
    fn settled(&self) -> bool {
        self.work.borrow().settled
    }

    /// Publishes the live background roster, as T3 Code's Waiting roster.
    async fn publish_waiting(&mut self, response: &ResponseState) -> Result<(), String> {
        self.work
            .send_modify(|work| work.background = response.has_background());
        if response.background_tasks() == self.waiting.as_slice() {
            return Ok(());
        }
        self.waiting = response.background_tasks().to_vec();
        self.change(SessionChange::Background {
            tasks: self.waiting.clone(),
        })
        .await
    }

    /// As T3 Code, Claude's answer completes its turn while its background
    /// work keeps running; the work's report starts another turn.
    async fn settle(&mut self) -> Result<(), String> {
        let mut turn = self
            .current_turn()
            .cloned()
            .ok_or("Claude execution state is unavailable")?;
        turn.status = TurnStatus::Completed;
        turn.completed_at_ms = Some(now_ms());
        self.work.send_modify(|work| work.settled = true);
        self.change(SessionChange::Turn {
            turn,
            completed: true,
        })
        .await
    }

    /// Background work that ended while Claude waited. The roster names it
    /// until the notification removes it; otherwise its call names it.
    fn collect_report(&mut self, message: &Value, response: &ResponseState) {
        let (key, report) = if message["type"] == "system"
            && message["subtype"] == "task_notification"
            && message["ambient"] != true
            && message["skip_transcript"] != true
        {
            let task = response::task_key(message).and_then(|id| response.background_task(id));
            let outcome = match message["status"].as_str() {
                Some("completed") => BackgroundOutcome::Completed,
                Some("failed") => BackgroundOutcome::Failed,
                Some("stopped") => BackgroundOutcome::Cancelled,
                _ if task.is_some_and(|task| task.kind == BackgroundTaskKind::Monitor) => {
                    BackgroundOutcome::Updated
                }
                _ => BackgroundOutcome::Unknown,
            };
            let call = message["tool_use_id"].as_str();
            let report = match (task, call.and_then(|id| self.item(id))) {
                (Some(task), _) => BackgroundReport {
                    kind: task.kind,
                    label: task.description.clone(),
                    outcome,
                    exit_code: None,
                },
                (None, Some(item)) => native::background_report(item.body(), outcome),
                (None, None) => BackgroundReport {
                    kind: BackgroundTaskKind::BackgroundTask,
                    label: None,
                    outcome,
                    exit_code: None,
                },
            };
            let Some(key) = call.or(response::task_key(message)) else {
                return;
            };
            (key.to_owned(), report)
        } else if message["type"] == "user"
            && let Some(outcome) = native::notification_outcome(
                message["origin"]["kind"].as_str(),
                &message["message"]["content"],
            )
            && let Some(item) = self.item(outcome.tool_id)
        {
            (outcome.tool_id.to_owned(), outcome.report(item.body()))
        } else {
            return;
        };
        if !self.reports.iter().any(|(known, _)| known == &key) {
            self.reports.push((key, report));
        }
    }

    fn item(&self, id: &str) -> Option<&Item> {
        self.live
            .turns
            .iter()
            .flatten()
            .rev()
            .flat_map(|turn| turn.items.iter().flatten())
            .find(|item| item.id.as_str() == id)
            .map(AsRef::as_ref)
    }

    /// Claude woke for background work after its answer: as T3 Code, the
    /// report is a new turn that the work's notification starts.
    async fn wake(&mut self, message: &Value) -> Result<(), String> {
        let frame = (message["type"] == "user" && message["origin"]["kind"] == "task-notification")
            .then(|| message["uuid"].as_str())
            .flatten()
            .map(str::to_owned);
        // The transcript names the turn by the answer it follows, too.
        let turn_id: agent_protocol::ids::TurnId = format!("{}-wake", self.turn_id).into();
        let reports: Vec<_> = std::mem::take(&mut self.reports)
            .into_iter()
            .map(|(_, report)| report)
            .collect();
        let notification = (!reports.is_empty()).then(|| {
            Arc::new(Item::new(
                format!("{turn_id}:notification").into(),
                ItemStatus::Completed,
                ItemBody::Notification { reports },
            ))
        });
        self.open_turn(turn_id, notification.into_iter().collect())
            .await?;
        self.wake_frame = frame;
        Ok(())
    }

    async fn open_turn(
        &mut self,
        turn_id: agent_protocol::ids::TurnId,
        items: Vec<Arc<Item>>,
    ) -> Result<(), String> {
        self.record
            .lock()
            .await
            .running
            .as_mut()
            .ok_or("Claude execution state is unavailable")?
            .turn_id = turn_id.clone();
        self.execution = self
            .input_scopes
            .get(&agent_protocol::ids::ItemId::from(turn_id.to_string()))
            .copied()
            .or_else(|| self.turn_scopes.get(&turn_id).copied())
            .unwrap_or_else(Uuid::new_v4);
        self.turn_scopes.insert(turn_id.clone(), self.execution);
        self.turn_id = turn_id.clone();
        self.reports.clear();
        self.wake_frame = None;
        self.work.send_modify(|work| work.settled = false);
        self.change(SessionChange::Turn {
            turn: Turn {
                id: turn_id,
                status: TurnStatus::Running,
                items: Some(items),
                started_at_ms: Some(now_ms()),
                ..Default::default()
            },
            completed: false,
        })
        .await
    }

    fn current_turn(&self) -> Option<&Turn> {
        self.live
            .turns
            .as_ref()?
            .iter()
            .find(|t| t.id == self.turn_id)
            .map(AsRef::as_ref)
    }
    /// Settles the answered turn and continues in `turn_id`. Its background
    /// items keep running; their notifications update this turn later.
    async fn next_turn(&mut self, turn_id: agent_protocol::ids::TurnId) -> Result<(), String> {
        if !self.settled() {
            self.settle().await?;
        }
        self.open_turn(turn_id, Vec::new()).await
    }
    /// The SDK echo can follow an earlier response's terminal frames. Move the
    /// accepted input to the turn it actually starts before applying its answer.
    async fn consume_input(&mut self, id: &str, start_new_turn: bool) -> Result<(), String> {
        let (source, item) = self
            .live
            .turns
            .iter()
            .flatten()
            .find_map(|turn| {
                turn.items
                    .iter()
                    .flatten()
                    .find(|item| item.id.as_str() == id)
                    .map(|item| (turn.id.clone(), item.clone()))
            })
            .ok_or("accepted Claude input is unavailable")?;
        let new_turn = start_new_turn && self.turn_id.as_str() != id;
        if !new_turn && source == self.turn_id {
            return Ok(());
        }
        self.change(SessionChange::RemoveItem {
            turn_id: source,
            item_id: item.id.clone(),
        })
        .await?;
        if new_turn {
            self.next_turn(id.into()).await?;
        }
        self.change(SessionChange::Item {
            turn_id: self.turn_id.clone(),
            item,
        })
        .await
    }

    async fn change(&mut self, change: SessionChange) -> Result<(), String> {
        let (live, result) = change.apply_timeline(std::mem::take(&mut self.live));
        self.live = live;
        result.map_err(|e| e.to_string())?;
        let input = match &change {
            SessionChange::Item { item, .. }
                if matches!(item.body(), ItemBody::UserMessage { .. }) =>
            {
                self.input_scopes.get(&item.id).copied()
            }
            _ => None,
        };
        let execution = input
            .or_else(|| {
                change
                    .turn_id()
                    .and_then(|id| self.turn_scopes.get(id).copied())
            })
            .unwrap_or(self.execution);
        crate::providers::emit_for(
            &self.events,
            AgentChange::Session {
                session: self.session.clone(),
                change,
            },
            Some(execution),
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
                        execution: None,
                        applied: None,
                    })
                    .await;
            });
        });
        let mut response = ResponseState::default();
        let outcome = async {
            let turn=self.current_turn().cloned().ok_or("Claude execution state is unavailable")?;
            self.change(SessionChange::Turn {turn,completed:false}).await?;
            loop {
                let event = tokio::select! {
                    biased;
                    _ = self.stop.cancelled() => {
                        response.interrupt("shutdown");
                        process.write(&SdkInput::Interrupt { request_id: "shutdown".into() }).await?;
                        return Ok(());
                    }
                    // Apply queued SDK state before deciding which turn accepts
                    // new input. A ready result/idle pair can settle the response.
                    message = process.read() => message?.ok_or("Claude SDK exited before the turn completed")?,
                    command = input.recv() => {
                        let command = command.ok_or("Claude input queue is closed")?;
                        let stop=match &command.value {SdkInput::Interrupt {request_id}=>Some(request_id.clone()),_=>None};
                        if let Some(id) = &stop && !response.interrupt(id) {
                            self.interrupt.send_replace(Some((id.clone(), Ok(()))));
                            continue;
                        }
                        let steers = matches!(command.value, SdkInput::Steer { .. });
                        // Input after a settled answer starts its own turn, as in T3 Code.
                        let delivery = if self.settled() { InputDelivery::NewTurn } else { response.delivery() };
                        if let Some(id) = self.deliver_command(&mut process, command, delivery).await? {
                            response.accept(id);
                            if steers {
                                response.steer();
                            }
                        }
                        self.publish_waiting(&response).await?;
                        continue;
                    }
                };
                let message = match event {
                    SdkEvent::Message { message } => message,
                    SdkEvent::Request { request_id, request } => {
                        if let Err(error) = self.permission(&request_id, &request).await {
                            tracing::warn!(target: "agent", operation = "host.claude.request_rejected", message = %error);
                            process.write(&SdkInput::Answer { request_id, response: None, error: Some(error) }).await?;
                        }
                        continue;
                    }
                    SdkEvent::RequestCancelled { request_id } => {
                        emit(&self.events, AgentChange::Resolved { instance: self.instance, native_id: request_id.into() }).await?;
                        continue;
                    }
                    SdkEvent::Interrupted { request_id, error } => {
                        if let Some(result)=response.interrupt_result(&request_id,error) {
                            self.interrupt.send_replace(Some((request_id,result)));
                            // Only background work remains, so no result will follow.
                            if response.interrupted() && response.answered() {
                                return Ok(());
                            }
                        }
                        continue;
                    }
                    SdkEvent::Ready { .. } => return Err("Claude SDK initialized twice".into()),
                    SdkEvent::Usage { .. } => return Err("Unexpected Claude usage response".into()),                    SdkEvent::Forked { .. } => return Err("Unexpected Claude fork response".into()),
                    SdkEvent::Error { message } => return Err(message),
                };
                if self.settled() {
                    self.collect_report(&message, &response);
                }
                let observation = response.observe(&message)?;
                for (id, opens_turn) in &observation.echoes {
                    self.consume_input(id, *opens_turn).await?;
                }
                if observation.woke {
                    if !self.settled() {
                        self.settle().await?;
                    }
                    self.wake(&message).await?;
                }
                match observation.disposition {
                    MessageDisposition::Forward => self.message(message).await?,
                    MessageDisposition::Applied => {},
                    MessageDisposition::Deferred => continue,
                    MessageDisposition::Failed => return self.fail_turn(&message).await,
                    MessageDisposition::Interrupted => return Ok(()),
                }
                self.publish_waiting(&response).await?;
                if !self.settled() && response.settles() {
                    self.settle().await?;
                }
                // A result ends a response. SDK idle covers background agents,
                // but not background shells and monitors, which Claude answers
                // after they finish; accepted input must be consumed before idle.
                if response.finished() && input.is_empty() {
                    return Ok(());
                }
            }
        }.await;
        // The stopped response ended; a still unacknowledged stop has its effect.
        if let Some(id) = response.pending_interrupt() {
            self.interrupt.send_replace(Some((id.to_owned(), Ok(()))));
        }
        // Reject commands that lost the race with completion before retaining the process.
        drop(input);
        let _ = emit(&self.events, AgentChange::SourceClosed(self.instance)).await;
        scopeguard::ScopeGuard::into_inner(_requests);
        self.finish_run(process, outcome, response.interrupted())
            .await;
    }

    async fn fail_turn(&mut self, message: &Value) -> Result<(), String> {
        let mut error = execution_error(message, false);
        if matches!(
            error.category,
            ErrorCategory::Other | ErrorCategory::Provider(_)
        ) && let Some(previous) = self.current_turn().and_then(|turn| turn.error.as_ref())
        {
            error.category = previous.category.clone();
            error.provider_code = previous.provider_code.clone();
        }
        let text = error.message.clone();
        self.change(SessionChange::Error {
            turn_id: self.turn_id.clone(),
            error,
        })
        .await?;
        Err(text)
    }

    async fn deliver_command(
        &mut self,
        process: &mut Process,
        command: Command,
        delivery: InputDelivery,
    ) -> Result<Option<agent_protocol::ids::ItemId>, String> {
        let mut accepted_input = None;
        let result = process.write(&command.value).await;
        let mut accepted_turn = None;
        if result.is_ok()
            && let Some(user) = command.user
        {
            if let Some(execution) = command.execution {
                self.input_scopes.insert(user.id.clone(), execution);
            }
            // An answered response gives new input its own turn,
            // as in the transcript and T3 Code.
            if delivery == InputDelivery::NewTurn {
                self.next_turn(user.id.as_str().into()).await?;
            }
            accepted_turn = (delivery != InputDelivery::Background).then(|| self.turn_id.clone());
            accepted_input = Some(user.id.clone());
            self.change(SessionChange::Item {
                turn_id: self.turn_id.clone(),
                item: Arc::new(user),
            })
            .await?;
        }
        if let Some(delivered) = command.delivered {
            let _ = delivered.send(result.clone().map(|()| accepted_turn));
        }
        result?;
        Ok(accepted_input)
    }

    async fn finish_run(
        mut self,
        process: Process,
        outcome: Result<(), String>,
        interrupted: bool,
    ) {
        let mut retained = None;
        // An interrupt leaves background work running; stop it with its CLI.
        let outcome = if outcome.is_ok() && !self.stop.is_cancelled() && !interrupted {
            retained = Some(process);
            outcome
        } else {
            // Keep the exit status and stderr when the turn already failed.
            match (outcome, process.finish().await) {
                (Err(error), Err(detail)) => Err(format!("{error}. {detail}")),
                (outcome, exited) => outcome.and(exited),
            }
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
        // A settled answer keeps its outcome; Stop ends only its background work.
        if !self.settled() {
            turn.status = if interrupted {
                TurnStatus::Interrupted
            } else if outcome.is_ok() {
                TurnStatus::Completed
            } else {
                TurnStatus::Failed
            };
            turn.completed_at_ms = Some(now_ms());
        }
        let unfinished = if interrupted {
            ItemStatus::Interrupted
        } else {
            ItemStatus::Failed
        };
        if let Some(items) = &mut turn.items {
            for item in items
                .iter_mut()
                .filter(|item| item.status == ItemStatus::Running)
            {
                Arc::make_mut(item).status = unfinished;
            }
        }
        // Background work of earlier settled turns ends with this CLI.
        let stopped: Vec<_> = self
            .live
            .turns
            .iter()
            .flatten()
            .filter(|earlier| earlier.id != turn.id)
            .flat_map(|earlier| {
                earlier
                    .items
                    .iter()
                    .flatten()
                    .filter(|item| item.status == ItemStatus::Running)
                    .map(|item| {
                        let mut item = item.as_ref().clone();
                        item.status = unfinished;
                        (earlier.id.clone(), item)
                    })
            })
            .collect();
        for (turn_id, item) in stopped {
            let _ = self
                .change(SessionChange::Item {
                    turn_id,
                    item: Arc::new(item),
                })
                .await;
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
        // The background work ended with its process; input starts a new one.
        if !self.waiting.is_empty() {
            self.waiting.clear();
            let _ = self
                .change(SessionChange::Background { tasks: Vec::new() })
                .await;
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

    async fn permission(&self, request_id: &str, request: &SdkRequest) -> Result<(), String> {
        let adapted = crate::providers::requests::claude(
            Uuid::new_v4().to_string().into(),
            &self.turn_id,
            request,
        )?;
        crate::providers::emit_for(
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
            Some(self.execution),
        )
        .await
    }

    // Background work may finish after its turn settled for a later input.
    async fn complete_background(
        &mut self,
        outcome: &native::TaskOutcome<'_>,
    ) -> Result<bool, String> {
        let Some((turn_id, mut item)) = self.live.turns.iter().flatten().rev().find_map(|turn| {
            let item = turn
                .items
                .iter()
                .flatten()
                .find(|item| item.id.as_str() == outcome.tool_id)?;
            Some((turn.id.clone(), outcome.item(item.id.clone(), item.body())?))
        }) else {
            return Ok(false);
        };
        if let Some(path) = outcome.output_path
            && let Ok((output, omitted)) = read_tool_output_tail(path).await
        {
            let mut body =
                native::tool_result_body(item.body(), &Value::String(output), &Value::Null);
            if let ItemBody::CommandExecution { output_omitted, .. } = &mut body {
                *output_omitted = omitted;
            }
            item.body = ItemContent::Inline {
                body: Box::new(body),
            };
        }
        self.change(SessionChange::Item {
            turn_id,
            item: Arc::new(item),
        })
        .await?;
        Ok(true)
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
        } else if kind == "user" {
            native::notification_outcome(
                message["origin"]["kind"].as_str(),
                &message["message"]["content"],
            )
        } else {
            None
        };
        if let Some(outcome) = outcome
            && self.complete_background(&outcome).await?
        {
            return Ok(());
        }
        if kind == "user" && message["origin"]["kind"] == "task-notification" {
            return self.notification_frame(&message).await;
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
            let block_kind = block["type"].as_str();
            let items = self
                .current_turn()
                .ok_or("Claude execution state is unavailable")?
                .items
                .as_deref()
                .unwrap_or_default();
            let item = match block_kind {
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
                    let completed_text = match block_kind {
                        Some("text") => block["text"].as_str(),
                        Some("thinking") => block["thinking"].as_str(),
                        _ => None,
                    };
                    let id = message_item_id(
                        items,
                        message_id
                            .or_else(|| message["uuid"].as_str())
                            .unwrap_or("tool-result"),
                        index,
                        blocks.len(),
                        block_kind,
                        completed_text,
                    );
                    native::content_item(
                        &self.session,
                        id,
                        block,
                        message["message"]["stop_reason"].as_str(),
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

    /// A notification Claude answers within its response is activity; one
    /// that woke Claude starts its turn as reports instead.
    async fn notification_frame(&mut self, message: &Value) -> Result<(), String> {
        let id = message["uuid"]
            .as_str()
            .ok_or("Claude notification ID is missing")?;
        if self.wake_frame.as_deref() == Some(id) {
            return Ok(());
        }
        if let Some(item) = native::user_item(
            id,
            &message["message"]["content"],
            message["isMeta"] == true,
            message["origin"]["kind"].as_str(),
            message["promptSource"].as_str(),
        ) {
            self.change(SessionChange::Item {
                turn_id: self.turn_id.clone(),
                item: Arc::new(item),
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
            Some("message_delta") => {
                // Live block envelopes precede the stop reason, so their text
                // starts as commentary and learns its final phase here.
                let Some(stop_reason) = event["delta"]["stop_reason"].as_str() else {
                    return Ok(());
                };
                let message = self
                    .stream
                    .as_ref()
                    .ok_or("Claude stream ended a message without starting it")?;
                let prefix = format!("{message}:");
                let phase = native::text_phase(Some(stop_reason));
                let items: Vec<_> = self
                    .current_turn()
                    .and_then(|turn| turn.items.as_deref())
                    .unwrap_or_default()
                    .iter()
                    .filter(|item| {
                        item.id.starts_with(&prefix)
                            && matches!(item.body(), ItemBody::AssistantText { phase: current, .. } if *current != phase)
                    })
                    .map(|item| {
                        let mut item = Item::clone(item);
                        if let ItemBody::AssistantText { phase: current, .. } = item.body_mut() {
                            *current = phase;
                        }
                        item
                    })
                    .collect();
                for item in items {
                    self.change(SessionChange::Item {
                        turn_id: self.turn_id.clone(),
                        item: item.into(),
                    })
                    .await?;
                }
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
    kind: Option<&str>,
    text: Option<&str>,
) -> String {
    if count == 1 && matches!(kind, Some("text" | "thinking")) {
        let mut first_exact = None;
        let mut first_running = None;
        for item in items {
            if !item
                .id
                .strip_prefix(message)
                .is_some_and(|tail| tail.starts_with(':'))
            {
                continue;
            }
            let exact = match (kind, item.body()) {
                (Some("text"), ItemBody::AssistantText { text: content, .. }) => {
                    Some(text == Some(content.as_str()))
                }
                (Some("thinking"), ItemBody::Reasoning { content, .. }) => {
                    Some(text.is_some_and(|text| {
                        content
                            .iter()
                            .try_fold(text, |remaining, part| {
                                remaining.strip_prefix(part.as_str())
                            })
                            .is_some_and(str::is_empty)
                    }))
                }
                _ => None,
            };
            let Some(exact) = exact else { continue };
            if exact {
                if item.status == ItemStatus::Running {
                    return item.id.to_string();
                }
                first_exact.get_or_insert(item);
            } else if item.status == ItemStatus::Running {
                first_running.get_or_insert(item);
            }
        }
        if let Some(item) = first_exact.or(first_running) {
            return item.id.to_string();
        }
    }
    format!("{message}:{index}")
}

#[cfg(test)]
mod message_match_tests;

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
    ExecutionError {
        category,
        message: text,
        provider_code: code.map(str::to_owned),
        // `api_retry` numbers its attempt of `max_retries`.
        retry: retrying.then(|| {
            let number = |field: &str| message[field].as_u64().and_then(|n| u32::try_from(n).ok());
            RetryEvidence::new(number("attempt"), number("max_retries"))
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
            op::Input::Context { context } => {
                content.push(json!({"type":"text","text":context.provider_text()}));
            }
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[async_trait::async_trait]
impl crate::providers::Identity for Claude {
    async fn account_terminal(&self, id: &str) -> Result<crate::terminals::Launch, Failure> {
        self.accounts
            .lock()
            .await
            .terminal(id)
            .map_err(|error| Failure::new("account_operation_failed", error))
    }
    async fn list(&self) -> Result<op::Accounts, crate::providers::Failure> {
        // Serialize native identity discovery without holding the account state
        // lock across CLI/OAuth I/O. Cached models and skills still need to read
        // the selected home while that discovery is running.
        let _discovery = self.account_discovery.lock().await;
        let refresh = self.accounts.lock().await.native_refresh();
        let mut errors = HashMap::new();
        if let Some((revision, refresh)) = refresh {
            match refresh.await {
                Ok(info) => self.accounts.lock().await.accept_native(revision, info),
                Err(error) => {
                    errors.insert(ProviderKind::Claude, error);
                }
            }
        }
        let (accounts, selected) = self.accounts.lock().await.list();
        Ok(op::Accounts {
            accounts,
            selected: selected
                .map(|id| (ProviderKind::Claude, id))
                .into_iter()
                .collect(),
            errors,
        })
    }
    async fn account(
        &self,
        command: crate::providers::AccountCommand,
    ) -> Result<crate::providers::AccountReply, crate::providers::Failure> {
        self.accounts
            .lock()
            .await
            .request(command)
            .await
            .map_err(crate::providers::Failure::from)
    }
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, crate::providers::Failure> {
        let fetch = self
            .accounts
            .lock()
            .await
            .usage_request(id)
            .map_err(|e| crate::providers::Failure::new("account_operation_failed", e))?;
        Ok(fetch.await)
    }
}

struct RequestSource {
    input: tokio::sync::mpsc::Sender<Command>,
    answers: crate::providers::requests::NativeAnswers,
}
pub(crate) fn request_origin(
    instance: uuid::Uuid,
    native_id: Value,
    input: tokio::sync::mpsc::Sender<Command>,
    answers: crate::providers::requests::NativeAnswers,
) -> crate::providers::RequestOrigin {
    crate::providers::RequestOrigin {
        instance,
        native_id,
        provider: ProviderKind::Claude,
        source: std::sync::Arc::new(RequestSource { input, answers }),
    }
}
#[async_trait::async_trait]
impl crate::providers::AnswerSource for RequestSource {
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
            permit.send(Command {
                execution: None,
                value: SdkInput::Answer {
                    request_id,
                    response: Some(result),
                    error: None,
                },
                user: None,
                delivered: Some(delivered),
            });
            tokio::time::timeout(std::time::Duration::from_secs(15), receipt)
                .await
                .map_err(|_| {
                    Failure::unknown("answer_delivery_unknown", "answer delivery timed out")
                })?
                .map_err(|_| {
                    Failure::unknown(
                        "answer_delivery_unknown",
                        "agent exited before confirming the answer write",
                    )
                })?
                .map(|_| ())
                .map_err(|e| Failure::unknown("answer_delivery_unknown", e))
        }
        .boxed())
    }
}

#[async_trait::async_trait]
impl crate::providers::Configuration for Claude {
    fn data_recipient(&self) -> &'static str {
        "Anthropic"
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
                self.directory.clone()
            } else {
                PathBuf::from(cwd)
            };
            let (program, processes) = (self.program.clone(), self.processes.clone());
            let (native_home, native_inherited) = (self.native_home.clone(), self.native_inherited);
            let key = (auth_home.clone(), directory.clone());
            self.commands
                .read(key, move || async move {
                    let _permit = processes
                        .try_acquire_owned()
                        .map_err(|error| error.to_string())?;
                    let probe = Process::probe(
                        &program,
                        process::homes(&native_home, native_inherited, &auth_home),
                        &directory,
                    )
                    .await?;
                    let commands = probe.initialized["commands"]
                        .as_array()
                        .ok_or("スキル一覧を取得できませんでした。")?;
                    Ok(commands
                        .iter()
                        .filter_map(|command| {
                            let name = command["name"].as_str()?;
                            Some(ComposerCandidate {
                                invocation: skill_invocation(
                                    name,
                                    &format!("skill://claude/{name}"),
                                ),
                                description: command["description"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .into(),
                            })
                        })
                        .collect())
                })
                .await
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
    async fn read_permissions(
        &self,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        permissions::read_claude_permissions(&self.native_home)
    }
    async fn update_permissions(
        &self,
        mode: agent_protocol::permissions::PermissionMode,
        version: &str,
    ) -> Result<agent_protocol::permissions::PermissionSettings, Failure> {
        permissions::update_claude_permissions(&self.native_home, mode, version)
    }
}

#[async_trait::async_trait]
impl crate::providers::Discovery for Claude {
    fn storage_directory(&self) -> &Path {
        &self.native_home
    }
    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        scope: SessionListScope<'_>,
        limit: u32,
    ) -> Result<SessionPage, Failure> {
        let owned = match scope {
            SessionListScope::Descendants(_) => {
                return Ok(SessionPage {
                    data: vec![],
                    next_cursor: None,
                });
            }
            SessionListScope::Loaded => {
                let limit = limit.max(1) as usize;
                let mut threads = self.loaded_threads().await;
                threads.sort_by(|a, b| a.id.cmp(&b.id));
                let mut data: Vec<_> = threads
                    .into_iter()
                    .filter(|thread| {
                        cursor
                            .as_ref()
                            .is_none_or(|cursor| thread.id.as_ref().unwrap().id > *cursor)
                    })
                    .take(limit + 1)
                    .map(|thread| SessionSummary {
                        thread,
                        branch: None,
                    })
                    .collect();
                let next_cursor = (data.len() > limit).then(|| {
                    data.truncate(limit);
                    data.last().unwrap().thread.id.as_ref().unwrap().id.clone()
                });
                return Ok(SessionPage { data, next_cursor });
            }
            SessionListScope::Roots(owned) | SessionListScope::WithSubagents(owned) => Some(
                owned
                    .iter()
                    .filter(|id| id.provider == Some(ProviderKind::Claude))
                    .map(|id| id.id.clone())
                    .collect(),
            ),
            SessionListScope::All => None,
        };
        Claude::list(self, search, cursor, owned, limit)
            .await
            .map_err(|e| Failure::new("session_list_failed", e))
    }

    async fn open(
        &self,
        id: &str,
        limit: usize,
        _include_activity: bool,
    ) -> Result<ThreadResponse, Failure> {
        self.read(id, Some(limit))
            .await
            .map_err(|e| Failure::new("session_read_failed", e))
    }
    async fn read_history(
        &self,
        id: &str,
        cursor: &str,
        _include_activity: bool,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        let end = cursor
            .parse::<usize>()
            .ok()
            .filter(|end| *end > 0)
            .ok_or_else(|| Failure::new("invalid_cursor", "invalid Claude history cursor"))?;
        let native =
            Uuid::parse_str(id).map_err(|error| Failure::new("session_read_failed", error))?;
        let home = self.native_home.clone();
        tokio::task::spawn_blocking(move || {
            history::read_page(&history::resolve(&home, native)?, end)
        })
        .await
        .map_err(|error| Failure::new("session_read_failed", error))?
        .map_err(|error| Failure::new("session_read_failed", error))
    }
    async fn read_item(&self, params: &op::ReadItem) -> Result<Item, Failure> {
        Claude::read_item(self, params).await.map_err(Into::into)
    }
}

#[async_trait::async_trait]
impl Agent for Claude {
    fn capabilities(&self) -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            steer: true,
            fork: true,
            model_change: true,
            ..Default::default()
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
        let path = std::env::var_os("PATH");
        process::executable(&self.program, path.as_deref())
            .map_err(|error| Failure::new("provider_unavailable", error))?;
        process::runtime().map_err(|error| Failure::new("provider_unavailable", error))?;
        Ok(())
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
        // Queued input needs the owned runtime and current titles, not a replay
        // of its transcript. Idle sessions still observe native history changes.
        let record = self.records.lock().await.get(id).cloned();
        let running_model = if let Some(record) = record {
            let record = record.lock().await;
            record.running.as_ref().map(|_| record.model.clone())
        } else {
            None
        };
        let mut response = self
            .read(id, running_model.is_none().then_some(1))
            .await
            .map_err(|e| Failure::new("session_read_failed", e))?;
        if let Some(model) = running_model {
            response.model = Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: model,
            });
        }
        let record = self.records.lock().await.get(id).cloned();
        if let Some(record) = record {
            let record = record.lock().await;
            // A settled answer waits on background work without a running turn.
            if let Some(running) = record
                .running
                .as_ref()
                .filter(|running| !running.work.borrow().settled)
            {
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
        route: crate::providers::SubmissionTarget<'_>,
        _reload: bool,
        browser: Option<Value>,
        execution: uuid::Uuid,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use crate::providers::SubmissionTarget;
        let turn_id = match route {
            SubmissionTarget::Steer(turn) => {
                self.send_running(input, Some(turn), execution).await?
            }
            // Claude waiting on background work after its answer runs the
            // input as its next turn.
            SubmissionTarget::Start { .. } if self.takes_input(&input.thread_id.id).await => {
                self.send_running(input, None, execution).await?
            }
            SubmissionTarget::Start { .. } => {
                Some(self.start_turn(input, browser, execution).await?)
            }
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
    async fn workspace_idle_warning(&self, id: &str) -> Option<&'static str> {
        let record = self.records.lock().await.get(id).cloned();
        if let Some(record) = record
            && record.lock().await.idle.is_some()
        {
            return None;
        }
        Some("native transcript does not expose external process activity")
    }
    async fn discard_workspace_processes(&self, dir: &Path) -> Result<(), Failure> {
        Claude::discard_workspace_processes(self, dir)
            .await
            .map_err(|e| Failure::new("workspace_resume_failed", e))
    }
    async fn unload(&self, id: &str) -> Result<(), Failure> {
        let record = self.records.lock().await.get(id).cloned();
        let idle = if let Some(record) = record {
            let mut record = record.lock().await;
            if record.running.is_some() {
                return Err(Failure::new(
                    "provider_switch_busy",
                    "Claude is still running.",
                ));
            }
            record.idle.take()
        } else {
            None
        };
        if let Some(idle) = idle {
            idle.close()
                .await
                .map_err(|error| Failure::new("provider_unload_failed", error))?;
        }
        Ok(())
    }
    async fn fork(
        &self,
        id: &str,
        turn: &str,
        _browser: Option<Value>,
    ) -> Result<ThreadResponse, Failure> {
        let failure = |error: String| Failure::new("fork_failed", error);
        let native = Uuid::parse_str(id).map_err(|_| failure("invalid Claude ID".into()))?;
        // Forking writes a new session and leaves the source untouched, so a
        // retained or running CLI keeps a current view of it. A running turn
        // only appends after the turns it follows; the SDK copies through the
        // fork point and skips a line still being written. Only the turn still
        // answering has no fork point yet.
        let record = self.records.lock().await.get(id).cloned();
        let running = match record {
            Some(record) => record
                .lock()
                .await
                .running
                .as_ref()
                .map(|running| running.turn_id.clone()),
            None => None,
        };
        let home = self.native_home.clone();
        let history = tokio::task::spawn_blocking(move || {
            history::read_details(&history::resolve(&home, native)?)
        })
        .await
        .map_err(|error| failure(error.to_string()))?
        .map_err(|error| failure(format!("{error:#}")))?;
        let turns = history.response.thread.turns.as_deref().unwrap_or_default();
        let position = |id: &str| turns.iter().position(|turn| turn.id.as_str() == id);
        if let Some(running) = running.and_then(|running| position(running.as_str()))
            && position(turn).is_some_and(|turn| turn >= running)
        {
            return Err(failure(
                "Claude がこの回答を生成中です。完了してから分岐してください。".into(),
            ));
        }
        let up_to = history
            .fork_points
            .get(turn)
            .ok_or_else(|| failure("Claude turn is unavailable in native history".into()))?;
        let cwd = history
            .response
            .thread
            .cwd
            .ok_or_else(|| failure("Claude session workspace is unavailable".into()))?;
        let auth_home = self
            .accounts
            .lock()
            .await
            .selected_home()
            .map_err(failure)?
            .ok_or_else(|| failure("Claude アカウントを選択してください。".into()))?;
        let forked = Process::fork(
            process::homes(&self.native_home, self.native_inherited, &auth_home),
            Path::new(&cwd),
            id,
            up_to,
        )
        .await
        .map_err(failure)?;
        self.read(&forked, Some(usize::MAX))
            .await
            .map_err(|error| failure(format!("{error:#}")))
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
                tracing::error!(target: "agent", operation = "claude.worker", message = %error);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod execution_tests {
    use super::*;

    #[tokio::test]
    async fn structured_context_survives_native_input_without_reading_its_references() {
        use agent_protocol::{
            context::{ContextContent, MessageContext},
            items::MessagePart,
        };
        let context = MessageContext {
            id: "file".into(),
            label: "Source".into(),
            content: ContextContent::File {
                path: "/unread/source.rs".into(),
            },
        };
        let content = input_content(&[op::Input::Context {
            context: Box::new(context.clone()),
        }])
        .await
        .unwrap();
        let item = native::input_item("item", &content, false).unwrap();
        assert!(
            matches!(item.body(), ItemBody::UserMessage { text: None, content } if content == &[MessagePart::Context {context: Box::new(context)}])
        );
    }

    #[tokio::test]
    async fn queued_input_reads_only_metadata_and_keeps_owned_execution() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let native = root.join("native-home");
        let claude = Claude::load(
            root.join("unused"),
            root.join("adapter"),
            Some(native.clone()),
        )
        .await
        .unwrap();
        let session = claude
            .create(root.to_str().unwrap(), "owned-model")
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        let project = native.join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{}.jsonl", session.id)),
            format!(
                "{}\n{}\nmalformed transcript body\n",
                json!({"sessionId":session.id,"cwd":root,"type":"user","message":{"role":"user","content":"Original input"}}),
                json!({"sessionId":session.id,"type":"custom-title","customTitle":"Preserved title"}),
            ),
        )
        .unwrap();
        let record = claude.record(&session.id).await.unwrap();
        let (input, _receiver) = mpsc::channel(1);
        let (_interrupt, interrupt) = watch::channel(None);
        {
            let mut record = record.lock().await;
            record.resumable = true;
            record.running = Some(Running {
                turn_id: "owned-turn".into(),
                input,
                interrupt,
                effort: None,
                work: watch::channel(Work::default()).1,
            });
        }
        let state = Agent::state(&claude, &session.id).await.unwrap();
        assert!(!state.needs_reload);
        assert_eq!(state.response.thread.id.as_ref(), Some(&session));
        assert_eq!(state.response.thread.cwd.as_deref(), root.to_str());
        assert_eq!(
            state.response.thread.name.as_deref(),
            Some("Preserved title")
        );
        assert_eq!(
            state.response.thread.preview.as_deref(),
            Some("Original input")
        );
        assert_eq!(state.response.thread.status, SessionStatus::Running);
        assert_eq!(
            state.response.thread.turns.unwrap()[0].id.as_str(),
            "owned-turn"
        );
        assert_eq!(state.response.model.unwrap().id, "owned-model");
        assert!(state.response.thread.history_read_state.is_none());

        record.lock().await.running = None;
        let state = Agent::state(&claude, &session.id).await.unwrap();
        assert!(
            state
                .response
                .thread
                .history_read_state
                .is_some_and(|state| {
                    state.kind == agent_protocol::session::HistoryReadKind::Incomplete
                })
        );
    }

    #[tokio::test]
    async fn fork_refuses_only_the_turn_still_answering() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let native = root.join("native-home");
        let claude = Claude::load(
            root.join("unused"),
            root.join("adapter"),
            Some(native.clone()),
        )
        .await
        .unwrap();
        let session = claude
            .create(root.to_str().unwrap(), "default")
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        let project = native.join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let row = |kind: &str, uuid: &str, parent: Option<&str>, content: Value| {
            json!({"sessionId":session.id,"cwd":root,"type":kind,"uuid":uuid,"parentUuid":parent,
                "timestamp":"2026-10-28T01:00:00.000Z","message":{"id":uuid,"content":content}})
            .to_string()
        };
        let text = |text: &str| json!([{"type":"text","text":text}]);
        std::fs::write(
            project.join(format!("{}.jsonl", session.id)),
            [
                row("user", "first", None, json!("first")),
                row("assistant", "answer", Some("first"), text("done")),
                row("user", "second", Some("answer"), json!("second")),
                row("assistant", "partial", Some("second"), text("still")),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let (input, _receiver) = mpsc::channel(1);
        let (_interrupt, interrupt) = watch::channel(None);
        claude
            .record(&session.id)
            .await
            .unwrap()
            .lock()
            .await
            .running = Some(Running {
            turn_id: "second".into(),
            input,
            interrupt,
            effort: None,
            work: watch::channel(Work::default()).1,
        });
        let refused = Agent::fork(&claude, &session.id, "second", None)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "fork_failed");
        assert!(refused.message.contains("生成中"), "{}", refused.message);
        // A completed turn gets past the running turn to the account, which
        // this Host has not selected; forking it succeeds in host-fixture.
        let completed = Agent::fork(&claude, &session.id, "first", None)
            .await
            .unwrap_err();
        assert!(
            !completed.message.contains("生成中") && !completed.message.contains("running"),
            "{}",
            completed.message
        );
    }

    #[tokio::test]
    async fn loaded_pages_keep_owned_activity_without_opening_native_history() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let claude = Claude::load(
            root.join("unused"),
            root.join("adapter"),
            Some(root.join("missing-history")),
        )
        .await
        .unwrap();
        let mut expected = Vec::new();
        for _ in 0..125 {
            let session = claude
                .create(root.to_str().unwrap(), "default")
                .await
                .unwrap()
                .thread
                .id
                .unwrap();
            expected.push(session.id);
        }
        expected.sort();
        let (input, _receiver) = mpsc::channel(1);
        let (_interrupt, interrupt) = watch::channel(None);
        claude
            .record(&expected[0])
            .await
            .unwrap()
            .lock()
            .await
            .running = Some(Running {
            turn_id: "turn".into(),
            input,
            interrupt,
            effort: None,
            work: watch::channel(Work::default()).1,
        });
        let mut cursor = None;
        let mut observed = Vec::new();
        let mut running = 0;
        for expected_count in [50, 50, 25] {
            let page = crate::providers::Discovery::list(
                &claude,
                "",
                cursor,
                SessionListScope::Loaded,
                50,
            )
            .await
            .unwrap();
            assert_eq!(page.data.len(), expected_count);
            for summary in page.data {
                assert!(summary.thread.turns.is_none());
                assert_eq!(summary.thread.cwd.as_deref(), root.to_str());
                running += usize::from(summary.thread.status == SessionStatus::Running);
                observed.push(summary.thread.id.unwrap().id);
            }
            cursor = page.next_cursor;
        }
        assert_eq!(observed, expected);
        assert_eq!(running, 1);
        assert!(cursor.is_none());
        let page = crate::providers::Discovery::list(
            &claude,
            "",
            Some(expected[74].clone()),
            SessionListScope::Loaded,
            50,
        )
        .await
        .unwrap();
        assert_eq!(page.data.len(), 50);
        assert!(page.next_cursor.is_none());
        let page =
            crate::providers::Discovery::list(&claude, "", None, SessionListScope::Loaded, 0)
                .await
                .unwrap();
        assert_eq!(page.data.len(), 1);
        assert!(page.next_cursor.is_some());
        let page = crate::providers::Discovery::list(
            &claude,
            "",
            Some(expected.last().unwrap().clone()),
            SessionListScope::Loaded,
            50,
        )
        .await
        .unwrap();
        assert!(page.data.is_empty() && page.next_cursor.is_none());
        let recent =
            crate::providers::Discovery::list(&claude, "", None, SessionListScope::Roots(&[]), 30)
                .await
                .unwrap();
        assert_eq!(
            recent.data.len(),
            1,
            "only a running task without a native transcript belongs in recent titles"
        );
        assert_eq!(recent.data[0].thread.id.as_ref().unwrap().id, expected[0]);
        assert_eq!(recent.data[0].thread.status, SessionStatus::Running);
    }

    #[tokio::test]
    async fn task_listings_include_only_owned_sdk_runs() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("native/projects/p");
        std::fs::create_dir_all(&project).unwrap();
        for (index, entrypoint) in [(1u128, "sdk-ts"), (2, "sdk-cli"), (3, "sdk-py"), (4, "cli")] {
            let id = Uuid::from_u128(index).to_string();
            let row = serde_json::json!({"type":"user","sessionId":id,"cwd":"/fixture","entrypoint":entrypoint,"message":{"content":entrypoint}});
            std::fs::write(project.join(format!("{id}.jsonl")), format!("{row}\n")).unwrap();
        }
        let claude = Claude::load(
            root.join("unused"),
            root.join("adapter"),
            Some(root.join("native")),
        )
        .await
        .unwrap();
        let owned = [
            SessionRef {
                provider: Some(ProviderKind::Claude),
                id: Uuid::from_u128(1).to_string(),
            },
            SessionRef {
                provider: Some(ProviderKind::Codex),
                id: Uuid::from_u128(2).to_string(),
            },
        ];
        for (scope, expected) in [
            (SessionListScope::Roots(&[]), vec!["cli"]),
            (SessionListScope::Roots(&owned), vec!["cli", "sdk-ts"]),
            (
                SessionListScope::WithSubagents(&owned),
                vec!["cli", "sdk-ts"],
            ),
            (
                SessionListScope::All,
                vec!["cli", "sdk-cli", "sdk-py", "sdk-ts"],
            ),
        ] {
            let mut listed: Vec<_> =
                crate::providers::Discovery::list(&claude, "", None, scope, 10)
                    .await
                    .unwrap()
                    .data
                    .into_iter()
                    .map(|summary| summary.thread.preview.unwrap())
                    .collect();
            listed.sort();
            assert_eq!(listed, expected);
        }
    }

    #[tokio::test]
    async fn a_steer_requires_its_turn_and_write_confirmation_to_prove_delivery() {
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
                effort: None,
                work: watch::channel(Work::default()).1,
            });
            if !enqueued {
                receiver.close();
            }
            let submission = op::Submission {
                fresh_context: false,
                thread_id: session.clone(),
                client_user_message_id: "input".into(),
                input: vec![op::Input::Text {
                    text: "steer".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: op::FollowUp::Steer,
            };
            // A turn that ended or changed takes no steer.
            assert_eq!(
                claude
                    .send_running(&submission, Some("earlier"), Uuid::new_v4())
                    .await
                    .unwrap_err()
                    .delivery,
                agent_transport::peer::Delivery::NotSent
            );
            let (result, ()) = tokio::join!(
                claude.send_running(&submission, Some("turn"), Uuid::new_v4()),
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

    #[tokio::test]
    async fn a_model_or_effort_change_is_refused_while_claude_runs_background_work() {
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
        let submission = |model: &str, effort: Option<&str>| op::Submission {
            fresh_context: false,
            thread_id: session.clone(),
            client_user_message_id: "input".into(),
            input: vec![op::Input::Text {
                text: "next".into(),
            }],
            model: Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: model.into(),
            }),
            effort: effort.map(str::to_owned),
            service_tier: None,
            follow_up: op::FollowUp::Queue,
        };
        for (background, model, effort, refused) in [
            (true, "sonnet", None, true),
            (true, "default", Some("low"), true),
            (true, "default", Some("high"), false),
            (false, "sonnet", None, false),
        ] {
            let (input, mut receiver) = mpsc::channel::<Command>(1);
            let (_interrupt, interrupt) = watch::channel(None);
            record.lock().await.running = Some(Running {
                turn_id: "turn".into(),
                input,
                interrupt,
                effort: Some("high".into()),
                work: watch::channel(Work {
                    background,
                    settled: false,
                })
                .1,
            });
            // A forwarded input reaches the worker queue, which drops it unwritten.
            let worker = tokio::spawn(async move { receiver.recv().await.is_some() });
            let error = claude
                .send_running(&submission(model, effort), None, Uuid::new_v4())
                .await
                .unwrap_err();
            record.lock().await.running = None;
            let forwarded = worker.await.unwrap();
            assert_eq!(forwarded, !refused, "{model} {effort:?} {background}");
            if refused {
                assert_eq!(error.message, BACKGROUND_BLOCKS_CHANGE);
                assert_eq!(error.delivery, agent_transport::peer::Delivery::NotSent);
                assert!(error.message.ends_with(
                    ": Claude is still running background agents or commands, and this model or setting change would end them. Wait for them to finish, or press Stop, then send the message again."
                ));
            }
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
        for (kind, text, expected) in [
            ("text", "same", "message:1"),
            ("text", "changed", "message:1"),
            ("text", "later", "message:3"),
            ("thinking", "onetwo", "message:2"),
            ("thinking", "changed", "message:2"),
            ("thinking", "later", "message:4"),
        ] {
            assert_eq!(
                message_item_id(&items, "message", 0, 1, Some(kind), Some(text)),
                expected,
                "{kind}: {text}"
            );
        }
        assert_eq!(
            message_item_id(&items, "message", 3, 2, Some("text"), Some("same")),
            "message:3"
        );
        assert_eq!(
            message_item_id(&items, "message", 3, 1, Some("future"), None),
            "message:3"
        );
        assert_eq!(
            message_item_id(&items[..2], "message", 3, 1, Some("text"), Some("changed")),
            "message:3"
        );
        assert_eq!(
            message_item_id(&items[..2], "message", 3, 1, Some("text"), Some("same")),
            "message:0"
        );
    }
    #[derive(Clone, Default)]
    struct Observed(Arc<std::sync::Mutex<agent_protocol::models::Thread>>);
    impl Observed {
        fn apply(&self, change: SessionChange) -> Result<(), String> {
            let mut thread = self.0.lock().unwrap();
            *thread = change.apply(&thread).map_err(|error| error.to_string())?;
            Ok(())
        }
        fn turn(&self, turn: &str) -> Option<Arc<Turn>> {
            self.0
                .lock()
                .unwrap()
                .turns
                .iter()
                .flatten()
                .rev()
                .find(|candidate| candidate.id.as_str() == turn)
                .cloned()
        }
    }

    fn routed_worker() -> (Observed, SessionRef, Worker) {
        let router = Observed::default();
        let (events, mut receiver) = mpsc::channel::<AgentEvent>(256);
        let consumer = router.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let result = match event.change {
                    AgentChange::Session { change, .. } => consumer.apply(change),
                    _ => Ok(()),
                };
                if let Some(applied) = event.applied {
                    let _ = applied.send(result);
                }
            }
        });
        let uuid = Uuid::new_v4();
        let session = SessionRef::new(ProviderKind::Claude, uuid.to_string()).unwrap();
        router
            .apply(SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "turn".into(),
                    ..Default::default()
                },
                completed: false,
            })
            .unwrap();
        let (input, _receiver) = mpsc::channel(1);
        let (interrupt, interrupted) = watch::channel(None);
        let worker = Worker {
            execution: Uuid::new_v4(),
            turn_scopes: Default::default(),
            input_scopes: Default::default(),
            instance: Uuid::new_v4(),
            auth_revision: 0,
            record: Arc::new(AsyncMutex::new(Record {
                cwd: "/work".into(),
                model: "default".into(),
                session_id: uuid,
                resumable: false,
                running: Some(Running {
                    turn_id: "turn".into(),
                    input: input.clone(),
                    interrupt: interrupted,
                    effort: None,
                    work: watch::channel(Work::default()).1,
                }),
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
            work: watch::channel(Work::default()).0,
            waiting: Vec::new(),
            reports: Vec::new(),
            wake_frame: None,
            model: "default".into(),
            effort: None,
        };
        (router, session, worker)
    }

    /// Exercise the production process reader and response/input state machine.
    /// The reference query bridge has no idle envelope: its returned result is
    /// adapted to Bex's explicit response-idle envelope for these text-only queries.
    pub(crate) async fn replay_sdk(
        rows: &[Value],
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<(SessionRef, tokio::task::JoinHandle<()>, CancellationToken), String> {
        let (_, _, mut worker) = routed_worker();
        worker.events = events;
        let native = rows
            .iter()
            .filter(|row| row["type"] == "emit_inbound")
            .map(|row| &row["frame"])
            .filter(|frame| frame["type"] == "system" && frame["subtype"] == "init")
            .find_map(|frame| frame["session_id"].as_str())
            .unwrap();
        worker.record.lock().await.session_id = Uuid::parse_str(native).unwrap();
        worker.session = SessionRef::new(ProviderKind::Claude, native.into()).unwrap();
        let session = worker.session.clone();
        let prompt = rows
            .iter()
            .find(|row| row["type"] == "expect_outbound" && row["frame"]["type"] == "prompt.offer")
            .unwrap();
        worker
            .change(SessionChange::Item {
                turn_id: worker.turn_id.clone(),
                item: Arc::new(Item::new(
                    "replay-input-0".into(),
                    ItemStatus::Completed,
                    ItemBody::UserMessage {
                        text: Some(
                            prompt["frame"]["message"]["message"]["content"]
                                .as_str()
                                .unwrap()
                                .into(),
                        ),
                        content: Vec::new(),
                    },
                )),
            })
            .await?;
        let (input, receiver) = mpsc::channel(1);
        worker.input = input;
        let file = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        std::fs::write(
            file.path(),
            serde_json::to_vec(rows).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let script = r#"
import { readFileSync } from 'node:fs';
const rows = JSON.parse(readFileSync(process.argv[1], 'utf8'));
const emit = event => process.stdout.write(JSON.stringify(event)+'\n');
emit({type:'ready',initialized:{}});
for (const row of rows) {
  if(row.type!=='emit_inbound') continue;
  // This bridge supplies the input UUID for the single offered query.
  const message=['assistant','result'].includes(row.frame.type) ? {...row.frame,user_message_uuid:'replay-input-0'} : row.frame;
  emit({type:'message',message});
  if(row.frame.type==='result') emit({type:'message',message:{type:'system',subtype:'session_state_changed',state:'idle'}});
}
"#;
        let path = std::env::var_os("PATH");
        let node = process::executable(Path::new("node"), path.as_deref())?;
        let mut command = tokio::process::Command::new(node);
        command
            .args(["--input-type=module", "-e", script])
            .arg(file.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut process = Process::spawn_command(command)?;
        assert!(matches!(
            process.read().await?,
            Some(SdkEvent::Ready { .. })
        ));
        let stop = worker.stop.clone();
        let running = tokio::spawn(worker.run(process, receiver));
        Ok((session, running, stop))
    }

    #[tokio::test]
    async fn recorded_claude_sdk_messages_cross_the_native_content_adapter() {
        let manifest: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/manifest.json")).unwrap();
        for recording in manifest
            .iter()
            .filter(|entry| entry["provider"] == "claude")
        {
            let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/providers/fixtures")
                .join(recording["file"].as_str().unwrap());
            let (observed, _, mut worker) = routed_worker();
            let rows: Vec<Value> = std::fs::read_to_string(file)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            for (index, row) in rows.iter().enumerate() {
                if row["type"] == "expect_outbound" && row["frame"]["type"] == "query.open" {
                    let native = rows[index + 1..]
                        .iter()
                        .take_while(|row| row["frame"]["type"] != "query.open")
                        .filter(|row| {
                            row["type"] == "emit_inbound"
                                && row["frame"]["type"] == "system"
                                && row["frame"]["subtype"] == "init"
                        })
                        .find_map(|row| row["frame"]["session_id"].as_str())
                        .or_else(|| row["frame"]["options"]["sessionId"].as_str())
                        .or_else(|| row["frame"]["options"]["resume"].as_str())
                        .unwrap();
                    worker.record.lock().await.session_id = Uuid::parse_str(native).unwrap();
                    worker.session = SessionRef::new(ProviderKind::Claude, native.into()).unwrap();
                }
                if row["type"] != "emit_inbound" {
                    continue;
                }
                let frame = row["frame"].clone();
                if frame["type"] == "permission.request" {
                    continue;
                }
                worker
                    .message(frame)
                    .await
                    .unwrap_or_else(|error| panic!("{} frame {index}: {error}", recording["file"]));
            }
            if recording["scenario"] == "simple" {
                let turn = observed.turn("turn").unwrap();
                assert!(turn.items.iter().flatten().any(|item| matches!(item.body(), ItemBody::AssistantText {text,..} if text == "fixture simple ok")));
            }
        }
    }
    #[tokio::test]
    async fn background_completion_marks_only_actually_truncated_output() {
        for text in ["short 日".into(), "日".repeat(1_000_000)] {
            let (observed, _, mut worker) = routed_worker();
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), &text).unwrap();
            worker
                .change(SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(Item::new(
                        "command".into(),
                        ItemStatus::Running,
                        ItemBody::CommandExecution {
                            command: "echo output".into(),
                            cwd: None,
                            output: String::new(),
                            output_omitted: false,
                            exit_code: None,
                        },
                    )),
                })
                .await
                .unwrap();
            let outcome = native::task_outcome(
                Some("command"),
                Some("completed"),
                Some("done"),
                file.path().to_str(),
            )
            .unwrap();
            assert!(worker.complete_background(&outcome).await.unwrap());
            let turn = observed.turn("turn").unwrap();
            let item = turn
                .items
                .as_ref()
                .unwrap()
                .iter()
                .find(|item| item.id.as_str() == "command")
                .unwrap();
            assert!(
                matches!(item.body(), ItemBody::CommandExecution { output, output_omitted, .. } if text.ends_with(output) && *output_omitted == (text.len() > agent_protocol::sync::TAIL_CHARS * 4 * 3))
            );
            assert_eq!(item.status, ItemStatus::Completed);
        }
    }

    #[tokio::test]
    async fn sdk_completion_and_input_echo_assign_the_actual_turn() {
        let path = std::env::var_os("PATH");
        let node = process::executable(Path::new("node"), path.as_deref()).unwrap();
        let script = r#"
import { createInterface } from 'node:readline';
const event = message => ({ type: 'message', message });
const emit = messages => process.stdout.write(messages.map(message => JSON.stringify(message)).join('\n') + '\n');
const mode = process.argv[1];
const settled = ['settled', 'background', 'started', 'child-notice', 'stamped', 'batched', 'result-stamped'].includes(mode);
const terminal = [event({ type: 'result', is_error: false }),
  event({ type: 'system', subtype: 'session_state_changed', state: 'idle' })];
if (mode === 'idle-first') terminal.reverse();
emit([
  { type: 'ready', initialized: {} },
  mode === 'started'
    ? event({ type: 'system', subtype: 'task_started', task_id: 'shell', task_type: 'local_bash', is_backgrounded: true })
    : event({ type: 'system', subtype: 'background_tasks_changed', tasks: ['settled', 'answering'].includes(mode) ? [] : [{task_id: 'shell', task_type: 'local_bash'}] }),
  ...(settled ? terminal : []),
]);
const inputs = [];
for await (const line of createInterface({input: process.stdin})) {
  const input = JSON.parse(line);
  if (input.type !== 'message') continue;
  inputs.push(input.id);
  if (mode === 'batched' && inputs.length < 2) continue;
  emit([
    // Claude Code runs input it did not fold into the answering response as
    // the next prompt, right after the result and without an idle level.
    ...(settled ? [] : mode === 'answering' ? [terminal[0],
      event({type: 'system', subtype: 'init', user_message_uuids: [input.id]})] : terminal),
    ...(mode === 'child-notice' ? [event({type: 'user', uuid: 'child',
      origin: {kind: 'task-notification', runId: 'shell'}, parent_tool_use_id: 'agent',
      message: {content: 'task notification'}})] : []),
    ...(['stamped', 'batched'].includes(mode) ? [event({type: 'assistant',
      user_message_uuid: input.id, ...(mode === 'batched' ? {user_message_uuids: inputs} : {}),
      message: {id: 'reply', content: [{type: 'text', text: 'answer'}]}})] : []),
    ...(mode === 'result-stamped' ? [event({type: 'result', user_message_uuid: input.id, is_error: false})]
      : ['stamped', 'batched'].includes(mode) ? []
      : [event({type: 'user', uuid: input.id, message: {content: input.content}})]),
  ]);
}
"#;
        for mode in [
            "settled",
            "background",
            "delayed",
            "idle-first",
            "started",
            "child-notice",
            "stamped",
            "batched",
            "result-stamped",
            "answering",
        ]
        .into_iter()
        .cycle()
        .take(20)
        {
            let mut command = tokio::process::Command::new(&node);
            command
                .arg("--input-type=module")
                .arg("-e")
                .arg(script)
                .arg(mode)
                .kill_on_drop(true);
            let mut process = Process::spawn_command(command).unwrap();
            // One write puts the ready event and the settled response in the
            // reader's buffer before a simultaneously ready client command.
            assert!(matches!(
                process.read().await.unwrap(),
                Some(SdkEvent::Ready { .. })
            ));
            let (_, _, mut worker) = routed_worker();
            // Observe the emitted history as a client does. The router is a
            // live cache and can prune a settled turn before a polling read.
            let (events, mut emitted) = mpsc::channel::<AgentEvent>(256);
            let (history, mut observed) = watch::channel(agent_protocol::models::Thread::default());
            worker.events = events;
            tokio::spawn(async move {
                while let Some(event) = emitted.recv().await {
                    if let AgentChange::Session { change, .. } = event.change {
                        let next = change.apply(&history.borrow()).unwrap();
                        history.send_replace(next);
                    }
                    if let Some(applied) = event.applied {
                        let _ = applied.send(Ok(()));
                    }
                }
            });
            let (input, receiver) = mpsc::channel(2);
            worker.input = input.clone();
            worker.record.lock().await.running.as_mut().unwrap().input = input.clone();
            let content = json!([{"type":"text","text":"next question"}]);
            let ids: &[&str] = if mode == "batched" {
                &["next", "queued"]
            } else {
                &["next"]
            };
            let mut receipts = Vec::new();
            for id in ids {
                let (delivered, receipt) = tokio::sync::oneshot::channel();
                receipts.push(receipt);
                input
                    .send(Command {
                        execution: None,
                        value: SdkInput::Message {
                            id: (*id).into(),
                            content: content.clone(),
                        },
                        user: native::input_item(id, content.as_array().unwrap(), false),
                        delivered: Some(delivered),
                    })
                    .await
                    .unwrap();
            }
            let stop = worker.stop.clone();
            let run = tokio::spawn(worker.run(process, receiver));
            for (index, receipt) in receipts.into_iter().enumerate() {
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(5), receipt)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap(),
                    // A settled answer gives new input its own turn at once;
                    // input before the result waits for Claude's echo.
                    match (mode, index) {
                        ("answering", _) => Some("turn".into()),
                        ("delayed" | "idle-first", _) | ("batched", 1) => None,
                        _ => Some("next".into()),
                    },
                    "{mode}"
                );
            }
            let assigned = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                observed.wait_for(|thread| {
                    thread
                        .turns
                        .iter()
                        .flatten()
                        .find(|turn| turn.id.as_str() == "next")
                        .is_some_and(|turn| {
                            ids.iter().all(|id| {
                                turn.items.iter().flatten().any(|item| item.id.as_str() == *id)
                            }) && (!matches!(mode, "stamped" | "batched")
                                || turn.items.iter().flatten().any(|item| {
                                    matches!(item.body(), ItemBody::AssistantText {text, ..} if text == "answer")
                                }))
                        })
                }),
            )
            .await
            .map(|result| result.map(|_| ()));
            stop.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .unwrap()
                .unwrap();
            assigned
                .unwrap_or_else(|error| panic!("{mode}: {error}"))
                .unwrap();
        }
    }

    #[tokio::test]
    async fn repeated_stop_after_confirmation_acknowledges_without_another_sdk_write() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let path = std::env::var_os("PATH");
            let node = process::executable(Path::new("node"), path.as_deref()).unwrap();
            let script = r#"
import { createInterface } from 'node:readline';
const emit = event => process.stdout.write(JSON.stringify(event) + '\n');
emit({ type: 'ready', initialized: {} });
for await (const line of createInterface({input: process.stdin})) {
  const {type, requestId} = JSON.parse(line);
  if (type !== 'interrupt') continue;
  if (requestId === 'second') throw new Error('a confirmed stop must not be sent twice');
  emit({type: 'interrupted', requestId});
}
"#;
            let mut command = tokio::process::Command::new(node);
            command
                .args(["--input-type=module", "-e", script])
                .kill_on_drop(true);
            let mut process = Process::spawn_command(command).unwrap();
            assert!(matches!(
                process.read().await.unwrap(),
                Some(SdkEvent::Ready { .. })
            ));
            let (_, _, mut worker) = routed_worker();
            let stop = worker.stop.clone();
            let mut acknowledged = worker.interrupt.subscribe();
            let (events, mut changes) = mpsc::channel::<AgentEvent>(256);
            worker.events = events;
            let drain = tokio::spawn(async move {
                while let Some(event) = changes.recv().await {
                    if let Some(applied) = event.applied {
                        let _ = applied.send(Ok(()));
                    }
                }
            });
            let (input, receiver) = mpsc::channel(1);
            let run = tokio::spawn(worker.run(process, receiver));
            for id in ["first", "second"] {
                input
                    .send(Command {
                        execution: None,
                        value: SdkInput::Interrupt {
                            request_id: id.into(),
                        },
                        user: None,
                        delivered: None,
                    })
                    .await
                    .unwrap();
                acknowledged
                    .wait_for(|result| matches!(result, Some((current, Ok(()))) if current == id))
                    .await
                    .unwrap();
            }
            stop.cancel();
            run.await.unwrap();
            drain.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn stop_ends_the_response_interrupted_in_either_acknowledgement_order() {
        let path = std::env::var_os("PATH");
        let node = process::executable(Path::new("node"), path.as_deref()).unwrap();
        // Claude Code ends a stopped response, here one awaiting an answer,
        // with an execution error result.
        let script = r#"
import { createInterface } from 'node:readline';
const emit = events => process.stdout.write(events.map(event => JSON.stringify(event)).join('\n') + '\n');
emit([{ type: 'ready', initialized: {} }]);
for await (const line of createInterface({input: process.stdin})) {
  const { type, requestId } = JSON.parse(line);
  if (type !== 'interrupt') continue;
  const acknowledged = { type: 'interrupted', requestId };
  const ended = [
    { type: 'message', message: { type: 'result', subtype: 'error_during_execution', is_error: true,
      errors: ['[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use'] } },
    { type: 'message', message: { type: 'system', subtype: 'session_state_changed', state: 'idle' } },
  ];
  emit(process.argv[1] === 'acknowledged' ? [acknowledged, ...ended] : [...ended, acknowledged]);
}
"#;
        for first in ["acknowledged", "ended"] {
            let mut command = tokio::process::Command::new(&node);
            command
                .arg("--input-type=module")
                .arg("-e")
                .arg(script)
                .arg(first)
                .kill_on_drop(true);
            let mut process = Process::spawn_command(command).unwrap();
            assert!(matches!(
                process.read().await.unwrap(),
                Some(SdkEvent::Ready { .. })
            ));
            let (_, _, mut worker) = routed_worker();
            let (events, mut changes) = mpsc::channel::<AgentEvent>(256);
            worker.events = events;
            let stopped = worker
                .record
                .lock()
                .await
                .running
                .as_ref()
                .unwrap()
                .interrupt
                .clone();
            let (input, receiver) = mpsc::channel(1);
            input
                .send(Command {
                    execution: None,
                    value: SdkInput::Interrupt {
                        request_id: "interrupt".into(),
                    },
                    user: None,
                    delivered: None,
                })
                .await
                .unwrap();
            let run = tokio::spawn(worker.run(process, receiver));
            let ended = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(event) = changes.recv().await {
                    if let Some(applied) = event.applied {
                        let _ = applied.send(Ok(()));
                    }
                    if let AgentChange::Session { change, .. } = event.change {
                        match change {
                            SessionChange::Error { error, .. } => panic!("{first}: {error:?}"),
                            SessionChange::Turn {
                                turn,
                                completed: true,
                            } => return turn,
                            _ => {}
                        }
                    }
                }
                panic!("{first}: the stopped turn did not end")
            })
            .await
            .unwrap();
            drop(changes);
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(ended.status, TurnStatus::Interrupted, "{first}");
            assert!(ended.error.is_none(), "{first}: {:?}", ended.error);
            assert!(matches!(&*stopped.borrow(), Some((_, Ok(())))), "{first}");
        }
    }

    #[tokio::test]
    async fn consumed_inputs_follow_the_current_turn_without_staying_in_completed_history() {
        let (_, _, mut worker) = routed_worker();
        for id in ["next", "queued"] {
            worker
                .change(SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(
                        native::input_item(id, &[json!({"type":"text","text":id})], false).unwrap(),
                    ),
                })
                .await
                .unwrap();
        }
        worker.consume_input("next", true).await.unwrap();
        worker.consume_input("queued", false).await.unwrap();
        worker.consume_input("next", true).await.unwrap();
        let turns = worker.live.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert!(turns[0].items.as_ref().unwrap().is_empty());
        assert_eq!(turns[1].id.as_str(), "next");
        assert_eq!(
            turns[1]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["next", "queued"]
        );
        assert_eq!(
            worker
                .record
                .lock()
                .await
                .running
                .as_ref()
                .unwrap()
                .turn_id
                .as_str(),
            "next"
        );
    }

    #[tokio::test]
    async fn background_work_that_ends_after_the_answer_reports_in_a_new_turn() {
        let path = std::env::var_os("PATH");
        let node = process::executable(Path::new("node"), path.as_deref()).unwrap();
        // The answer settles while a background command runs; Claude then
        // wakes for its notification and reports without another input.
        let script = r#"
const event = message => ({ type: 'message', message });
const idle = event({ type: 'system', subtype: 'session_state_changed', state: 'idle' });
const session = process.argv[1];
process.stdout.write([
  { type: 'ready', initialized: {} },
  event({ type: 'system', subtype: 'task_started', task_id: 'shell', task_type: 'local_bash', description: 'Start the dev server' }),
  event({ type: 'assistant', message: { id: 'answer', stop_reason: 'end_turn', content: [{ type: 'text', text: 'started' }] } }),
  event({ type: 'result', is_error: false }),
  idle,
  event({ type: 'system', subtype: 'task_notification', task_id: 'shell', status: 'completed', summary: 'done' }),
  event({ type: 'system', subtype: 'init', session_id: session }),
  event({ type: 'assistant', message: { id: 'report', stop_reason: 'end_turn', content: [{ type: 'text', text: 'reported' }] } }),
  event({ type: 'result', is_error: false }),
  idle,
].map(message => JSON.stringify(message)).join('\n') + '\n');
process.stdin.resume();
"#;
        let (_router, session, mut worker) = routed_worker();
        let mut command = tokio::process::Command::new(&node);
        command
            .arg("--input-type=module")
            .arg("-e")
            .arg(script)
            .arg(&session.id)
            .kill_on_drop(true);
        let mut process = Process::spawn_command(command).unwrap();
        assert!(matches!(
            process.read().await.unwrap(),
            Some(SdkEvent::Ready { .. })
        ));
        let (events, mut changes) = mpsc::channel(256);
        worker.events = events;
        let (_input, receiver) = mpsc::channel(1);
        let stop = worker.stop.clone();
        let run = tokio::spawn(worker.run(process, receiver));
        let mut thread = agent_protocol::models::Thread::default();
        let mut waited = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = changes.recv().await {
                if let Some(applied) = event.applied {
                    let _ = applied.send(Ok(()));
                }
                let AgentChange::Session { change, .. } = event.change else {
                    continue;
                };
                if let SessionChange::Background { tasks } = &change {
                    waited.push(tasks.len());
                }
                let reported = matches!(&change, SessionChange::Turn { turn, completed: true }
                    if turn.id == "turn-wake".into());
                thread = change.apply(&thread).unwrap();
                if reported {
                    break;
                }
            }
        })
        .await
        .unwrap();
        // The idle process stays for the next input until the Host stops.
        tokio::spawn(async move {
            while let Some(event) = changes.recv().await {
                if let Some(applied) = event.applied {
                    let _ = applied.send(Ok(()));
                }
            }
        });
        stop.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waited, [1, 0], "the roster shows while Claude waits");
        let turns = thread.turns.unwrap();
        assert_eq!(turns.len(), 2, "{turns:#?}");
        assert_eq!(turns[0].id, "turn".into());
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert!(turns[0].completed_at_ms.is_some());
        assert_eq!(
            turns[1].id,
            "turn-wake".into(),
            "named by the answer it follows"
        );
        assert_eq!(turns[1].status, TurnStatus::Completed);
        let items = turns[1].items.as_ref().unwrap();
        assert_eq!(
            items[0].body(),
            &ItemBody::Notification {
                reports: vec![BackgroundReport {
                    kind: BackgroundTaskKind::Command,
                    label: Some("Start the dev server".into()),
                    outcome: BackgroundOutcome::Completed,
                    exit_code: None,
                }],
            }
        );
        assert!(items.iter().any(
            |item| matches!(item.body(), ItemBody::AssistantText { text, .. } if text == "reported")
        ));
        assert!(thread.background.is_empty());
    }

    #[tokio::test]
    async fn an_answered_turn_completes_and_waits_on_its_background_roster_until_stop() {
        let path = std::env::var_os("PATH");
        let node = process::executable(Path::new("node"), path.as_deref()).unwrap();
        // Claude answers while a background command, a Monitor and a subagent
        // run. The subagent keeps the SDK from reporting idle until the stop.
        let script = r#"
import { createInterface } from 'node:readline';
const event = message => ({ type: 'message', message });
const emit = events => process.stdout.write(events.map(event => JSON.stringify(event)).join('\n') + '\n');
emit([
  { type: 'ready', initialized: {} },
  event({ type: 'assistant', message: { id: 'calls', content: [
    { type: 'tool_use', id: 'watch-call', name: 'Monitor', input: { command: 'tail -f build.log' } }] } }),
  event({ type: 'system', subtype: 'task_started', task_id: 'shell', task_type: 'local_bash', description: 'Start the dev server' }),
  event({ type: 'system', subtype: 'task_started', task_id: 'watch', task_type: 'local_bash', tool_use_id: 'watch-call', description: 'Watch the build' }),
  event({ type: 'system', subtype: 'task_started', task_id: 'agent', task_type: 'local_agent', description: 'Review src/math.ts' }),
  event({ type: 'result', is_error: false }),
]);
for await (const line of createInterface({input: process.stdin})) {
  const { type, requestId } = JSON.parse(line);
  if (type !== 'interrupt') continue;
  emit([{ type: 'interrupted', requestId },
    event({ type: 'system', subtype: 'session_state_changed', state: 'idle' })]);
}
"#;
        let mut command = tokio::process::Command::new(&node);
        command
            .arg("--input-type=module")
            .arg("-e")
            .arg(script)
            .kill_on_drop(true);
        let mut process = Process::spawn_command(command).unwrap();
        assert!(matches!(
            process.read().await.unwrap(),
            Some(SdkEvent::Ready { .. })
        ));
        let (router, _, mut worker) = routed_worker();
        let (input, receiver) = mpsc::channel(2);
        worker.input = input.clone();
        let record = worker.record.clone();
        let work = {
            let mut record = record.lock().await;
            let running = record.running.as_mut().unwrap();
            running.input = input.clone();
            let (sender, observed) = watch::channel(Work::default());
            running.work = observed.clone();
            worker.work = sender;
            observed
        };
        let run = tokio::spawn(worker.run(process, receiver));
        let roster = |router: &Observed| router.0.lock().unwrap().background.clone();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while roster(&router).len() < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            roster(&router),
            [
                BackgroundTask {
                    id: "shell".into(),
                    kind: BackgroundTaskKind::Command,
                    description: Some("Start the dev server".into()),
                },
                BackgroundTask {
                    id: "watch".into(),
                    kind: BackgroundTaskKind::Monitor,
                    description: Some("Watch the build".into()),
                },
                BackgroundTask {
                    id: "agent".into(),
                    kind: BackgroundTaskKind::Subagent,
                    description: Some("Review src/math.ts".into()),
                },
            ]
        );
        assert_eq!(
            *work.borrow(),
            Work {
                background: true,
                settled: true,
            },
            "the answer completed its turn and a new process would end the work"
        );
        input
            .send(Command {
                execution: None,
                value: SdkInput::Interrupt {
                    request_id: "interrupt".into(),
                },
                user: None,
                delivered: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap();
        assert!(roster(&router).is_empty(), "Stop ends the wait");
    }

    #[tokio::test]
    async fn background_work_reports_to_the_turn_it_ran_in_after_input_opens_another() {
        let (router, _, mut worker) = routed_worker();
        worker
            .message(json!({"type":"assistant","message":{"id":"call","content":[
                {"type":"tool_use","id":"work","name":"Bash","input":{"command":"work"}}]}}))
            .await
            .unwrap();
        worker
            .message(json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"work","content":"launched"}]},
                "tool_use_result":{"backgroundTaskId":"work"}}))
            .await
            .unwrap();
        worker.next_turn("next".into()).await.unwrap();
        assert_eq!(
            worker.record.lock().await.running.as_ref().unwrap().turn_id,
            "next".into()
        );
        worker
            .message(json!({"type":"system","subtype":"task_notification",
                "tool_use_id":"work","status":"completed","summary":"done"}))
            .await
            .unwrap();
        // The Host retains only running turns; clients apply this update to
        // the settled turn in their history window.
        let launched = worker
            .live
            .turns
            .iter()
            .flatten()
            .find(|turn| turn.id == "turn".into())
            .unwrap();
        assert_eq!(launched.status, TurnStatus::Completed);
        let work = launched
            .items
            .iter()
            .flatten()
            .find(|item| item.id == "work".into())
            .unwrap();
        assert_eq!(work.status, ItemStatus::Completed);
        let next = router.turn("next").unwrap();
        assert_eq!(next.status, TurnStatus::Running);
        assert!(
            next.items
                .iter()
                .flatten()
                .all(|item| item.id != "work".into())
        );
    }
    #[tokio::test]
    async fn late_blocks_and_api_retry_preserve_only_valid_stream_items() {
        let (router, session, mut worker) = routed_worker();
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
        let turn = router.turn("turn").unwrap();
        let items = turn.items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.as_str(), "message:0");
        assert_eq!(items[0].status, ItemStatus::Completed);
        assert!(
            matches!(items[1].body(),ItemBody::Reasoning {content,..} if content == &["abandoned"])
        );
        let phase = |turn: Arc<Turn>| match turn.items.as_ref().unwrap()[0].body() {
            ItemBody::AssistantText { phase, .. } => *phase,
            _ => panic!("streamed text must remain assistant text"),
        };
        assert_eq!(phase(turn), AssistantPhase::Commentary);
        worker
            .stream_event(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}))
            .await
            .unwrap();
        assert_eq!(phase(router.turn("turn").unwrap()), AssistantPhase::Final);
        worker
            .message(json!({"type":"system","subtype":"api_retry","attempt":1}))
            .await
            .unwrap();
        let turn = router.turn("turn").unwrap();
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
            "message":{"id":"message","stop_reason":"end_turn","content":[{"type":"text","text":"answer"}]}}),
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
                    ItemStatus::Completed
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
            let native_notice = json!({"type":"user","uuid":parent,"parentUuid":format!("result-{id}"),
                "origin":{"kind":"task-notification"},"promptSource":"system",
                "turnOrigin":"task_notification","queueSkipAttachments":true,
                "message":{"role":"user","content":format!("<task-notification><tool-use-id>{id}</tool-use-id><status>{status}</status><summary>work & result <details></summary></task-notification>")}});
            worker.message(native_notice.clone()).await.unwrap();
            rows.push(native_notice);
        }
        let unmatched = json!({"type":"user","uuid":"unmatched-notice","parentUuid":parent,
            "origin":{"kind":"task-notification"},"promptSource":"system",
            "message":{"role":"user","content":"<task-notification><tool-use-id>missing</tool-use-id><status>completed</status><summary>done</summary></task-notification>"}});
        worker.message(unmatched.clone()).await.unwrap();
        rows.push(unmatched);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(format!("{}.jsonl", session.id));
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
        let live = router.turn("turn").unwrap();
        assert_eq!(live.items, saved[0].items);
        let items = live.items.as_ref().unwrap();
        assert_eq!(items.len(), 9);
        assert!(matches!(items[8].body(), ItemBody::Attachment { .. }));
        assert_eq!(items[4].status, ItemStatus::Failed);
        assert!(
            matches!(items[4].body(), ItemBody::CommandExecution {output, exit_code: Some(7), ..} if output == "failed")
        );
        for (item, status) in items[5..8].iter().zip([
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
        assert_eq!(retry.category, ErrorCategory::Overloaded);
        assert_eq!(retry.retry, Some(RetryEvidence::new(None, None)));
        let numbered = execution_error(
            &json!({"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"error_status":529}),
            true,
        );
        assert_eq!(numbered.retry, Some(RetryEvidence::new(Some(2), Some(10))));
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

#[cfg(test)]
#[tokio::test]
async fn background_output_reads_only_a_bounded_utf8_tail_of_large_files() {
    use std::io::{Seek, Write};
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.as_file_mut()
        .seek(std::io::SeekFrom::Start(history::MAX_FILE_BYTES + 1))
        .unwrap();
    let suffix = "日".repeat(30_000);
    file.write_all(suffix.as_bytes()).unwrap();
    let (output, truncated) = read_tool_output_tail(file.path().to_str().unwrap())
        .await
        .unwrap();
    assert!(output.len() <= agent_protocol::sync::TAIL_CHARS * 4 * 3);
    assert!(output.ends_with(&suffix));
    assert!(truncated);
    std::fs::write(file.path(), "short 日").unwrap();
    let (small, truncated) = read_tool_output_tail(file.path().to_str().unwrap())
        .await
        .unwrap();
    assert!(!truncated);
    assert_eq!(small, "short 日");
    std::fs::write(file.path(), b"before\xffafter").unwrap();
    let (binary, truncated) = read_tool_output_tail(file.path().to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(binary, "before\u{fffd}after");
    assert!(!truncated);
}
