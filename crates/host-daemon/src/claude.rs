//! Claude Code owns inference, credentials and its transcript. The Host owns
//! the client-facing conversation and adapts the CLI's streaming protocol.
use agent_protocol::{execution::*, items::*};
mod accounts;
mod history;
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

use crate::host_rpc::routing::SessionRouter;
use process::Process;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct OperationError {
    pub(crate) message: String,
    pub(crate) delivery: agent_transport::peer::Delivery,
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
    browser: Option<Arc<crate::browser::Browser>>,
    program: PathBuf,
    directory: PathBuf,
    native_home: PathBuf,
    pub(crate) accounts: AsyncMutex<accounts::Accounts>,
    records: AsyncMutex<HashMap<String, Arc<AsyncMutex<Record>>>>,
    processes: Arc<Semaphore>,
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
    pub(crate) fn capabilities() -> agent_protocol::session::Capabilities {
        agent_protocol::session::Capabilities {
            additional_input: true,
            fork: false,
            rename: false,
            model_change: true,
        }
    }

    pub(crate) async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
        router: SessionRouter,
        browser: Option<Arc<crate::browser::Browser>>,
    ) -> anyhow::Result<Self> {
        crate::platform::create_state_directory(&directory)?;
        let native_home = native_home.map(Ok).unwrap_or_else(history::home)?;
        let accounts = accounts::Accounts::load(
            program.clone(),
            directory.join("accounts"),
            native_home.clone(),
        )
        .await?;
        Ok(Self {
            browser,
            accounts: AsyncMutex::new(accounts),
            program,
            directory,
            native_home,
            records: AsyncMutex::new(HashMap::new()),
            processes: Arc::new(Semaphore::new(8)),
            router,
            stop: CancellationToken::new(),
            workers: AsyncMutex::new(tokio::task::JoinSet::new()),
        })
    }

    pub(crate) async fn shutdown(&self) {
        self.stop.cancel();
        let _ = self.accounts.lock().await.cancel().await;
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(target: "bex", operation = "claude.worker", message = %error);
            }
        }
    }

    pub(crate) async fn models(&self) -> Result<Vec<Model>, String> {
        let available = if self.program.components().count() > 1 {
            self.program.is_file()
        } else {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path)
                    .any(|directory| directory.join(&self.program).is_file())
            })
        };
        if !available {
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

    pub(crate) async fn create(&self, cwd: &str, model: &str) -> anyhow::Result<ThreadResponse> {
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
                created_at: Some(now() as f64),
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

    pub(crate) fn storage_directory(&self) -> &Path {
        &self.native_home
    }

    pub(crate) async fn list(&self, search: &str) -> anyhow::Result<Vec<Thread>> {
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
                        thread
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
                if let Some(thread) = threads
                    .iter_mut()
                    .find(|thread| thread.id.as_ref() == Some(&id))
                {
                    thread.status = SessionStatus::Running;
                } else {
                    threads.push(Thread {
                        id: Some(SessionRef {
                            provider: ProviderKind::Claude,
                            id: record.session_id.to_string(),
                        }),
                        cwd: Some(record.cwd.clone()),
                        status: SessionStatus::Running,
                        ..Default::default()
                    });
                }
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

    /// Read native history on each open; the router overlays only owned execution.
    pub(crate) async fn read(&self, id: &str, requested: usize) -> anyhow::Result<ThreadResponse> {
        let native = Uuid::parse_str(id)?;
        let home = self.native_home.clone();
        let history: anyhow::Result<ThreadResponse> = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            match history::read(&path, requested) {
                Ok(response) => Ok(response),
                Err(error) => {
                    let mut thread = history::summary(&path)?;
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

    pub(crate) async fn read_item(
        &self,
        id: &str,
        params: &op::ReadItem,
    ) -> Result<op::ItemResponse, OperationError> {
        let native = Uuid::parse_str(id).map_err(|_| "invalid Claude ID")?;
        let home = self.native_home.clone();
        let mut native_history = tokio::task::spawn_blocking(move || {
            let path = history::resolve(&home, native)?;
            history::read_details(&path, usize::MAX)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))?;
        self.router.overlay_execution(
            &SessionRef {
                provider: ProviderKind::Claude,
                id: id.into(),
            },
            &mut native_history.response,
        );
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
        if let Some(path) = native_history
            .details
            .get(&item.id)
            .and_then(|details| details.output_path.as_deref())
        {
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
                body: Box::new(tool_result_body(item.body(), &content, &Value::Null)),
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
                history::read_related(&home, native, &agent_id, usize::MAX)
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

    pub(crate) async fn discard_workspace_processes(&self, directory: &Path) -> anyhow::Result<()> {
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

    pub(crate) async fn interrupt(
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

    pub(crate) async fn additional_input(
        &self,
        id: &str,
        expected_turn_id: Option<&str>,
        input: &[op::Input],
        client_id: &str,
    ) -> Result<String, OperationError> {
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
        if expected_turn_id.is_some_and(|id| id != running.turn_id.as_str()) {
            return Err("Claudeの実行対象が変わりました。会話を更新してください。".into());
        }
        let sender = running.input.clone();
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
            .map_err(|_| "Claude finished before accepting additional input")?
            .map_err(|message| OperationError {
                message,
                delivery: agent_transport::peer::Delivery::Unknown,
            })?;
        Ok(id)
    }

    pub(crate) async fn start_turn(
        &self,
        id: &str,
        params: &op::Submission,
    ) -> Result<agent_protocol::ids::TurnId, OperationError> {
        let record = self.record(id).await?;
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
                self.browser
                    .as_ref()
                    .map(|browser| {
                        browser.provider_config(
                            &SessionRef {
                                provider: ProviderKind::Claude,
                                id: session.to_string(),
                            }
                            .to_string(),
                        )
                    })
                    .transpose()?,
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
            id: id.into(),
        };
        self.router.session_change(
            &target,
            SessionChange::Turn {
                turn,
                completed: false,
            },
        );
        let worker = Worker {
            record,
            router: self.router.clone(),
            instance: Uuid::new_v4(),
            session: target,
            turn_id: turn_id.clone(),
            input,
            stop: self.stop.child_token(),
            stream: HashMap::new(),
            interrupt,
            model,
            effort: effort.map(str::to_owned),
            auth_revision,
        };
        let mut workers = self.workers.lock().await;
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
    router: SessionRouter,
    session: SessionRef,
    turn_id: agent_protocol::ids::TurnId,
    input: mpsc::Sender<Command>,
    stop: CancellationToken,
    stream: HashMap<String, String>,
    interrupt: watch::Sender<Option<Result<(), String>>>,
    model: String,
    effort: Option<String>,
}

impl Worker {
    async fn run(mut self, mut process: Process, mut input: mpsc::Receiver<Command>) {
        let router = self.router.clone();
        let _requests = scopeguard::guard(self.instance, move |instance| {
            router.close_request_source(instance)
        });
        let mut interrupted = false;
        let mut pending_inputs = HashSet::new();
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
                        if result.is_ok() && let Some(user) = command.user {
                            pending_inputs.insert(user.id.clone());
                            self.router.session_change(&self.session, SessionChange::Item {
                                turn_id: self.turn_id.clone(), item: Arc::new(user),
                            });
                        }
                        if let Some(delivered) = command.delivered { let _ = delivered.send(result.clone()); }
                        result?;
                    }
                    message = process.read() => {
                        let message = message?.ok_or("Claude Code exited without a result")?;
                        if message["type"] == "result" {
                            if message["is_error"] == true {
                                let mut error = execution_error(&message, false);
                                if error.category == ErrorCategory::Other && let Some(previous) = self.router.current_turn(&self.session, &self.turn_id).and_then(|turn| turn.error) {
                                    error.category = previous.category;
                                    error.provider_code = previous.provider_code;
                                }
                                let text = error.message.clone();
                                self.router.session_change(&self.session, SessionChange::Error {turn_id: self.turn_id.clone(), error});
                                return Err(text);
                            }
                            // A result can belong to the input before a queued message.
                            // Keep reading until Claude has consumed every submitted input.
                            if interrupted || pending_inputs.is_empty() {
                                return Ok(());
                            }
                            continue;
                        }
                        if message["type"] == "user" && let Some(id) = message["uuid"].as_str() {
                            pending_inputs.remove(id);
                        }
                        if message["type"] == "control_request" {
                            if let Err(error) = self.permission(&message) {
                                tracing::warn!(target: "bex", operation = "host.claude.request_rejected", message = %error);
                                let response = if message["request"]["subtype"] == "request_user_dialog" {
                                    json!({"subtype":"success","request_id":message["request_id"],"response":{"behavior":"cancelled"}})
                                } else { json!({"subtype":"error","request_id":message["request_id"],"error":error}) };
                                process.write(&json!({"type":"control_response","response":response})).await?;
                            }
                        } else if message["type"] == "control_cancel_request" {
                            let request_id = message["request_id"].as_str().ok_or("Claude canceled request ID is missing")?;
                            self.router.resolve_native_request(self.instance, &request_id.into());
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
        // Reject commands that lost the race with completion before retaining the process.
        drop(input);
        drop(_requests);
        let mut retained = None;
        let outcome = if outcome.is_ok() && !self.stop.is_cancelled() {
            retained = Some(process);
            outcome
        } else {
            let exited = process.finish().await;
            outcome.and(exited)
        };
        let mut record = self.record.lock().await;
        let Some(mut turn) = self.router.current_turn(&self.session, &self.turn_id) else {
            record.running = None;
            drop(record);
            if let Some(process) = retained {
                let _ = process.finish().await;
            }
            self.router.session_change(
                &self.session,
                SessionChange::Status {
                    status: SessionStatus::Unknown,
                },
            );
            return;
        };
        turn.status = if interrupted {
            TurnStatus::Interrupted
        } else if outcome.is_ok() {
            TurnStatus::Completed
        } else {
            TurnStatus::Failed
        };
        turn.completed_at = Some(now() as f64);
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
                model: self.model,
                effort: self.effort,
                released: released.clone(),
            });
        }
        self.router.session_change(
            &self.session,
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
        let request_id = message["request_id"]
            .as_str()
            .ok_or("Claude request ID is missing")?;
        let adapted = crate::host_rpc::requests::claude(
            Uuid::new_v4().to_string().into(),
            &self.turn_id,
            &message["request"],
        )?;
        self.router.request(
            self.session.clone(),
            crate::host_rpc::requests::RequestOrigin {
                instance: self.instance,
                native_id: request_id.into(),
                destination: crate::host_rpc::requests::RequestDestination::Claude {
                    input: self.input.clone(),
                },
            },
            adapted,
        )
    }

    async fn message(&mut self, message: Value) -> Result<(), String> {
        let scope = message["parent_tool_use_id"].as_str().unwrap_or_default();
        if message["type"] == "stream_event" {
            return self.stream_event(&message["event"], scope).await;
        }
        if message["type"] == "rate_limit_event" {
            if let Some(error) = usage_limit_error(&message["rate_limit_info"]) {
                self.router.session_change(
                    &self.session,
                    SessionChange::Error {
                        turn_id: self.turn_id.clone(),
                        error,
                    },
                );
            }
            return Ok(());
        }
        if message["type"] == "system" && message["subtype"] == "api_retry" {
            if let Some(id) = self.stream.remove(scope)
                && let Some(turn) = self.router.current_turn(&self.session, &self.turn_id)
            {
                let prefix = format!("{id}:");
                for item in turn.items.iter().flatten().filter(|item| {
                    item.id.starts_with(&prefix)
                        && item.status == ItemStatus::Running
                        && matches!(
                            item.body(),
                            ItemBody::AssistantText { .. } | ItemBody::Reasoning { .. }
                        )
                }) {
                    self.router.session_change(
                        &self.session,
                        SessionChange::RemoveItem {
                            turn_id: self.turn_id.clone(),
                            item_id: item.id.clone(),
                        },
                    );
                }
            }
            self.router.session_change(
                &self.session,
                SessionChange::Error {
                    turn_id: self.turn_id.clone(),
                    error: execution_error(&message, true),
                },
            );
            return Ok(());
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
        if kind == "assistant" && message["error"].is_string() {
            self.router.session_change(
                &self.session,
                SessionChange::Error {
                    turn_id: self.turn_id.clone(),
                    error: execution_error(&message, false),
                },
            );
        }
        let blocks = message["message"]["content"]
            .as_array()
            .ok_or("Claude message content is missing")?;
        let message_id = message["message"]["id"].as_str();
        let cwd = record.cwd.clone();
        drop(record);
        let mut turn = self
            .router
            .current_turn(&self.session, &self.turn_id)
            .ok_or("Claude execution state is unavailable")?;
        let items = turn.items.get_or_insert_default();
        for (index, block) in blocks.iter().enumerate() {
            // Claude emits one assistant envelope per completed block, often
            // with the same message ID. Preserve the stream's block index.
            let id = message_item_id(
                items,
                message_id
                    .or_else(|| message["uuid"].as_str())
                    .unwrap_or("tool-result"),
                index,
                blocks.len(),
                block,
            );
            let item = match block["type"].as_str() {
                Some("tool_result") => {
                    let item = items
                        .iter_mut()
                        .find(|item| Some(item.id.as_str()) == block["tool_use_id"].as_str())
                        .ok_or("Claude tool result has no matching tool call")?;
                    let item = Arc::make_mut(item);
                    item.status = if block["is_error"] == true {
                        ItemStatus::Failed
                    } else {
                        ItemStatus::Completed
                    };
                    item.body = ItemContent::Inline {
                        body: Box::new(tool_result_body(
                            item.body(),
                            &block["content"],
                            &message["toolUseResult"],
                        )),
                    };
                    self.router.session_change(
                        &self.session,
                        SessionChange::Item {
                            turn_id: self.turn_id.clone(),
                            item: item.clone().into(),
                        },
                    );
                    continue;
                }
                Some("text") if kind != "assistant" => continue,
                _ => content_item(
                    &self.session,
                    id,
                    block,
                    Some(cwd.as_str()),
                    ItemStatus::Running,
                )
                .map_err(|error| error.to_string())?,
            };
            self.router.session_change(
                &self.session,
                SessionChange::Item {
                    turn_id: self.turn_id.clone(),
                    item: item.clone().into(),
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
                    event["message"]["id"]
                        .as_str()
                        .ok_or("Claude stream message ID is missing")?
                        .into(),
                );
            }
            Some("content_block_start") => {
                let message = self
                    .stream
                    .get(scope)
                    .ok_or("Claude stream started a block without a message")?;
                let index = event["index"]
                    .as_u64()
                    .ok_or("Claude block index is missing")? as usize;
                let body = match event["content_block"]["type"].as_str() {
                    Some("text") => ItemBody::AssistantText {
                        citation: None,
                        text: String::new(),
                        phase: AssistantPhase::Unknown,
                    },
                    Some("thinking") => ItemBody::Reasoning {
                        content: vec![String::new()],
                        summary: vec![],
                    },
                    _ => return Ok(()),
                };
                let item = Item::new(
                    format!("{message}:{index}").into(),
                    ItemStatus::Running,
                    body,
                );
                self.router.session_change(
                    &self.session,
                    SessionChange::Item {
                        turn_id: self.turn_id.clone(),
                        item: item.into(),
                    },
                );
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
                    &self.session,
                    SessionChange::Text {
                        turn_id: self.turn_id.clone(),
                        item_id: id.into(),
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
        resets_at_seconds: info["resetsAt"].as_u64(),
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
        http_status: status.and_then(|v| v.try_into().ok()),
        retry: retrying.then(|| RetryEvidence {
            retrying,
            overloaded: category == ErrorCategory::Overloaded,
            attempt: message["attempt"].as_u64().and_then(|v| v.try_into().ok()),
            max_attempts: message["max_retries"]
                .as_u64()
                .and_then(|v| v.checked_add(1))
                .and_then(|v| v.try_into().ok()),
        }),
        retry_delay_ms: message["retry_delay_ms"].as_u64(),
        ..Default::default()
    }
}

fn content_item(
    session: &SessionRef,
    id: String,
    block: &Value,
    cwd: Option<&str>,
    tool_status: ItemStatus,
) -> Result<Item, serde_json::Error> {
    let text = |key: &str| block[key].as_str().unwrap_or_default().to_owned();
    let mut id = id.into();
    let mut status = ItemStatus::Completed;
    let body = match block["type"].as_str() {
        Some("text") => ItemBody::AssistantText {
            citation: None,
            text: text("text"),
            phase: AssistantPhase::Unknown,
        },
        Some("thinking") => ItemBody::Reasoning {
            content: vec![text("thinking")],
            summary: vec![],
        },
        Some("tool_use") => {
            id = serde_json::from_value(block["id"].clone())?;
            status = tool_status;
            let input = &block["input"];
            let string = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
            match block["name"].as_str() {
                Some("Bash") => ItemBody::CommandExecution {
                    actions: vec![],
                    source: agent_protocol::items::CommandSource::Unknown,
                    process_id: None,
                    command: string("command"),
                    cwd: cwd.map(str::to_owned),
                    output: String::new(),
                    exit_code: None,
                    duration_ms: None,
                },
                Some("Write" | "Edit" | "NotebookEdit") => {
                    let proposal = match block["name"].as_str() {
                        Some("Write") => FileProposal::Write {
                            content: string("content"),
                        },
                        Some("Edit") => FileProposal::Edit {
                            old_text: string("old_string"),
                            new_text: string("new_string"),
                            replace_all: input["replace_all"] == true,
                        },
                        _ => FileProposal::Notebook {
                            cell_id: input["cell_id"].as_str().map(str::to_owned),
                            source: string("new_source"),
                            mode: input["edit_mode"].as_str().map(str::to_owned),
                        },
                    };
                    ItemBody::FileChange {
                        changes: vec![FileChange {
                            path: string(if block["name"] == "NotebookEdit" {
                                "notebook_path"
                            } else {
                                "file_path"
                            }),
                            kind: FileChangeKind::Unknown,
                            diff: None,
                            proposal: Some(proposal),
                        }],
                        output: String::new(),
                    }
                }
                Some("Agent" | "Task") => ItemBody::Subagent {
                    tool: text("name"),
                    prompt: input["prompt"].as_str().map(str::to_owned),
                    model: input["model"].as_str().map(str::to_owned),
                    effort: None,
                    sender: Some(session.clone()),
                    receivers: vec![],
                    states: vec![],
                    agent_id: None,
                    result: None,
                },
                _ => ItemBody::ToolCall {
                    resource_uri: None,
                    plugin_id: None,
                    kind: ToolKind::Local,
                    tool: text("name"),
                    server: Some("Claude Code".into()),
                    namespace: None,
                    arguments: input.clone(),
                    result: None,
                    error: None,
                    content: vec![],
                    success: None,
                    duration_ms: None,
                },
            }
        }
        _ => {
            status = ItemStatus::Unknown;
            ItemBody::Custom {
                provider: ProviderKind::Claude,
                kind: text("type"),
                value: block.clone(),
            }
        }
    };
    Ok(Item::new(id, status, body))
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePatchHunk {
    old_start: u64,
    old_lines: u64,
    new_start: u64,
    new_lines: u64,
    lines: Vec<String>,
}
fn structured_patch(value: &Value) -> Option<String> {
    use std::fmt::Write as _;
    let hunks: Vec<NativePatchHunk> = serde_json::from_value(value.clone()).ok()?;
    if hunks.is_empty() {
        return None;
    }
    let mut diff = String::new();
    for hunk in hunks {
        if !hunk
            .lines
            .iter()
            .all(|line| line.starts_with([' ', '+', '-', '\\']))
        {
            return None;
        }
        writeln!(
            diff,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        )
        .ok()?;
        for line in hunk.lines {
            writeln!(diff, "{line}").ok()?;
        }
    }
    Some(diff)
}
fn tool_result_body(body: &ItemBody, content: &Value, metadata: &Value) -> ItemBody {
    let mut body = body.clone();
    let output = || {
        content.as_str().map(str::to_owned).unwrap_or_else(|| {
            content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    };
    match &mut body {
        ItemBody::CommandExecution {
            output: result,
            exit_code,
            ..
        } => {
            *result = output();
            if let Some(code) = metadata["exitCode"]
                .as_i64()
                .and_then(|code| code.try_into().ok())
            {
                *exit_code = Some(code);
            }
        }
        ItemBody::FileChange {
            changes,
            output: result,
        } => {
            *result = output();
            if changes.len() == 1 {
                let change = &mut changes[0];
                if let Some(diff) = structured_patch(&metadata["structuredPatch"]) {
                    change.diff = Some(diff);
                }
                match metadata["type"].as_str() {
                    Some("create") => change.kind = FileChangeKind::Add,
                    Some("update") => change.kind = FileChangeKind::Update { move_path: None },
                    _ => {}
                }
            }
        }
        ItemBody::Subagent {
            agent_id, result, ..
        } => {
            if let Some(id) = metadata["agentId"].as_str() {
                *agent_id = Some(id.into());
            }
            *result = Some(content.clone());
        }
        ItemBody::ToolCall { result, .. } => *result = Some(content.clone()),
        _ => {}
    }
    body
}

async fn input_content(input: &[op::Input]) -> Result<Vec<Value>, String> {
    if input.is_empty() {
        return Err("メッセージを入力してください。".into());
    }
    let mut content = Vec::with_capacity(input.len());
    for input in input {
        match input {
            op::Input::Text { text, .. } => content.push(json!({"type":"text","text":text})),
            op::Input::Skill { name, .. } => {
                content.push(json!({"type":"text","text":format!("${name}")}))
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

pub(crate) fn updated_at(thread: &Thread) -> u64 {
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

#[cfg(test)]
mod execution_tests {
    use super::*;

    #[test]
    fn final_blocks_match_content_kind_and_native_message_before_falling_back() {
        let text = |id: &str, status, value: &str| {
            Arc::new(Item::new(
                id.into(),
                status,
                ItemBody::AssistantText {
                    text: value.into(),
                    phase: AssistantPhase::Unknown,
                    citation: None,
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
        let router = SessionRouter::new();
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
            router: router.clone(),
            session: session.clone(),
            turn_id: "turn".into(),
            input,
            stop: CancellationToken::new(),
            stream: HashMap::new(),
            interrupt,
            model: "default".into(),
            effort: None,
        };
        for event in [
            json!({"type":"message_start","message":{"id":"message"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking"}}),
        ] {
            worker.stream_event(&event, "").await.unwrap();
        }
        worker.message(json!({"type":"assistant","uuid":"envelope","message":{"id":"message","content":[{"type":"text","text":"answer"}]}})).await.unwrap();
        worker.stream_event(&json!({"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"abandoned"}}),"").await.unwrap();
        let turn = router.current_turn(&session, "turn").unwrap();
        let items = turn.items.unwrap();
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
        let items = router
            .current_turn(&session, "turn")
            .unwrap()
            .items
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), "message:0");
        assert!(worker.stream.is_empty());
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
        let retry = execution_error(
            &json!({"error":"overloaded","attempt":2,"max_retries":4,"retry_delay_ms":1500,"error_status":503}),
            true,
        );
        assert_eq!(retry.http_status, Some(503));
        assert_eq!(retry.retry_delay_ms, Some(1500));
        assert_eq!(
            retry.retry,
            Some(RetryEvidence {
                retrying: true,
                overloaded: true,
                attempt: Some(2),
                max_attempts: Some(5)
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
        assert_eq!(limit.resets_at_seconds, Some(123456));
    }
    #[test]
    fn native_tools_preserve_observations_without_inventing_exit_codes_or_diffs() {
        let session = SessionRef::new(ProviderKind::Claude, "session".into()).unwrap();
        let command = content_item(
            &session,
            "stream".into(),
            &json!({"type":"tool_use","id":"bash","name":"Bash","input":{"command":"false"}}),
            Some("/work"),
            ItemStatus::Running,
        )
        .unwrap();
        let unknown = tool_result_body(command.body(), &json!("done"), &Value::Null);
        assert!(matches!(
            unknown,
            ItemBody::CommandExecution {
                exit_code: None,
                ..
            }
        ));
        let known = tool_result_body(command.body(), &json!("failed"), &json!({"exitCode":7}));
        assert!(matches!(
            tool_result_body(&known, &json!("expanded"), &Value::Null),
            ItemBody::CommandExecution {
                exit_code: Some(7),
                ..
            }
        ));
        let edit = content_item(&session,"stream".into(),&json!({"type":"tool_use","id":"edit","name":"Edit","input":{"file_path":"/work/a","old_string":"old","new_string":"new"}}),Some("/work"),ItemStatus::Running).unwrap();
        assert!(
            matches!(edit.body(),ItemBody::FileChange {changes,..} if changes[0].diff.is_none() && changes[0].proposal.is_some())
        );
        let applied = tool_result_body(
            edit.body(),
            &json!("edited"),
            &json!({"type":"update","structuredPatch":[{"oldStart":5,"oldLines":1,"newStart":5,"newLines":1,"lines":["-actual old","+actual new"]}]}),
        );
        assert!(
            matches!(applied,ItemBody::FileChange {changes,..} if changes[0].diff.as_deref() == Some("@@ -5,1 +5,1 @@\n-actual old\n+actual new\n"))
        );
        let future = json!({"type":"future_block","nested":{"a":[1,2]}});
        assert!(
            matches!(content_item(&session,"unknown".into(),&future,None,ItemStatus::Unknown).unwrap().body(),ItemBody::Custom {provider:ProviderKind::Claude,value,..} if value == &future)
        );
    }
}
