use super::*;
use crate::host_rpc::routing::SessionId;
use crate::providers::Discovery as _;
use crate::providers::{
    AccountCommand, AccountReply, AgentChange, AgentEvent, Identity, SessionPage, SessionSummary,
    SubmissionState,
};
use crate::providers::{Agent, Backend, SessionListScope};
use agent_protocol::protocol::{Call, Response};
use agent_protocol::{
    composer::ComposerCatalog,
    execution::{ItemStatus, SessionStatus, TurnStatus},
    ids::TurnId,
    items::{AssistantPhase, ItemBody},
    models::{Empty, Item, ModelRef, Turn},
    permissions::{PermissionMode, PermissionSettings},
    session::{Capabilities, SessionChange},
};
use agent_protocol::{
    handoff,
    models::{Thread, ThreadResponse},
    session::HistoryPage,
};
use serde::de::DeserializeOwned;
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;

/// Independent native sessions, including persisted history and provider
/// acknowledgements. Assertions observe the Host boundary, not a reimplementation
/// of its handoff/routing decisions.
pub(in crate::host_rpc) struct NativeAgent {
    provider: ProviderKind,
    directory: PathBuf,
    pub(in crate::host_rpc) records: Arc<Mutex<HashMap<String, ThreadResponse>>>,
    pub(in crate::host_rpc) idle_warning: Option<&'static str>,
    pub(in crate::host_rpc) deferred_history: bool,
    pub(in crate::host_rpc) metadata_only_state: bool,
    pub(in crate::host_rpc) needs_reload: bool,
    /// Item IDs whose native bodies cannot be read.
    pub(in crate::host_rpc) unreadable: Mutex<std::collections::HashSet<String>>,
    pub(in crate::host_rpc) inputs: Arc<Mutex<Vec<op::Submission>>>,
    pub(in crate::host_rpc) injected: Arc<Mutex<Vec<(String, handoff::Context)>>>,
    pub(in crate::host_rpc) released: Arc<Mutex<Vec<String>>>,
    pub(in crate::host_rpc) next: Arc<AtomicUsize>,
    pub(in crate::host_rpc) fail_submit: AtomicUsize,
    pub(in crate::host_rpc) create_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) create_started: tokio::sync::Notify,
    pub(in crate::host_rpc) created_in: Mutex<Vec<bool>>,
    pub(in crate::host_rpc) create_response_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) created: tokio::sync::Notify,
    fail_inject: AtomicUsize,
    pub(in crate::host_rpc) open_gate: tokio::sync::Semaphore,
    open_error: Mutex<Option<Failure>>,
    state_error: Mutex<Option<Failure>>,
    /// Every open fails with this, including the open an import makes.
    open_failure: Mutex<Option<Failure>>,
    pub(in crate::host_rpc) fail_list: AtomicUsize,
    pub(in crate::host_rpc) fail_accounts: AtomicUsize,
    pub(in crate::host_rpc) accounts_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) accounts_started: tokio::sync::Notify,
    events: mpsc::Sender<AgentEvent>,
    receiver: Mutex<Option<mpsc::Receiver<AgentEvent>>>,
}

fn unsupported() -> Failure {
    Failure::new("unsupported", "not used in this fixture")
}

impl NativeAgent {
    pub(in crate::host_rpc) fn new(provider: ProviderKind, directory: PathBuf) -> Arc<Self> {
        let (events, receiver) = mpsc::channel(32);
        Arc::new(Self {
            provider,
            directory,
            records: Default::default(),
            idle_warning: None,
            deferred_history: false,
            metadata_only_state: false,
            needs_reload: false,
            unreadable: Default::default(),
            inputs: Default::default(),
            injected: Default::default(),
            released: Default::default(),
            next: Default::default(),
            fail_submit: AtomicUsize::new(0),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            created_in: Mutex::default(),
            create_response_gate: tokio::sync::Semaphore::new(1),
            created: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
            open_gate: tokio::sync::Semaphore::new(1),
            open_error: Mutex::new(None),
            state_error: Mutex::new(None),
            open_failure: Mutex::new(None),
            fail_list: AtomicUsize::new(0),
            fail_accounts: AtomicUsize::new(0),
            accounts_gate: tokio::sync::Semaphore::new(1),
            accounts_started: tokio::sync::Notify::new(),
            events,
            receiver: Mutex::new(Some(receiver)),
        })
    }
    pub(in crate::host_rpc) fn restarted(&self) -> Arc<Self> {
        let (events, receiver) = mpsc::channel(32);
        Arc::new(Self {
            provider: self.provider,
            directory: self.directory.clone(),
            records: self.records.clone(),
            idle_warning: self.idle_warning,
            deferred_history: self.deferred_history,
            metadata_only_state: self.metadata_only_state,
            needs_reload: self.needs_reload,
            unreadable: Default::default(),
            inputs: self.inputs.clone(),
            injected: self.injected.clone(),
            released: self.released.clone(),
            next: self.next.clone(),
            fail_submit: AtomicUsize::new(0),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            created_in: Mutex::default(),
            create_response_gate: tokio::sync::Semaphore::new(1),
            created: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
            open_gate: tokio::sync::Semaphore::new(1),
            open_error: Mutex::new(None),
            state_error: Mutex::new(None),
            open_failure: Mutex::new(None),
            fail_list: AtomicUsize::new(0),
            fail_accounts: AtomicUsize::new(0),
            accounts_gate: tokio::sync::Semaphore::new(1),
            accounts_started: tokio::sync::Notify::new(),
            events,
            receiver: Mutex::new(Some(receiver)),
        })
    }
    pub(in crate::host_rpc) fn backend(self: &Arc<Self>) -> Backend {
        Backend {
            provider: self.provider,
            agent: Ok(self.clone()),
            dictation: None,
        }
    }
}

#[async_trait::async_trait]
impl Identity for NativeAgent {
    async fn list(&self) -> Result<op::Accounts, Failure> {
        self.accounts_started.notify_one();
        let _permit = self.accounts_gate.acquire().await.unwrap();
        if self.fail_accounts.swap(0, Ordering::SeqCst) != 0 {
            return Err(Failure::new(
                "fixture_accounts_failed",
                format!("{} accounts failed", self.provider.key()),
            ));
        }
        Ok(op::Accounts {
            accounts: vec![],
            selected: Default::default(),
            errors: Default::default(),
        })
    }
    async fn account(&self, _: AccountCommand) -> Result<AccountReply, Failure> {
        Err(unsupported())
    }
    async fn usage(&self, _: &str) -> Result<op::AccountUsage, Failure> {
        Err(unsupported())
    }
}

#[async_trait::async_trait]
impl crate::providers::Configuration for NativeAgent {
    fn data_recipient(&self) -> &'static str {
        "fixture"
    }
    async fn models(&self, _: &op::ListModels) -> Result<op::ModelPage, Failure> {
        Ok(op::ModelPage {
            data: vec![],
            next_cursor: None,
            provider_errors: None,
        })
    }
    async fn catalog(&self, _: &str) -> ComposerCatalog {
        ComposerCatalog::default()
    }
    async fn read_permissions(&self) -> Result<PermissionSettings, Failure> {
        Err(unsupported())
    }
    async fn update_permissions(
        &self,
        _: PermissionMode,
        _: &str,
    ) -> Result<PermissionSettings, Failure> {
        Err(unsupported())
    }
}

#[async_trait::async_trait]
impl crate::providers::Discovery for NativeAgent {
    fn storage_directory(&self) -> &Path {
        &self.directory
    }
    async fn list(
        &self,
        search: &str,
        _: Option<String>,
        scope: SessionListScope<'_>,
        _: u32,
    ) -> Result<SessionPage, Failure> {
        if self.fail_list.swap(0, Ordering::SeqCst) != 0 {
            return Err(Failure::new(
                "fixture_list_failed",
                format!("{} list failed", self.provider.key()),
            ));
        }
        let mut data = self
            .records
            .lock()
            .unwrap()
            .values()
            .filter(|record| {
                !matches!(scope, SessionListScope::Descendants(_))
                    && record
                        .thread
                        .preview
                        .as_deref()
                        .unwrap_or("")
                        .contains(search)
            })
            .map(|record| {
                let mut thread = record.thread.clone();
                thread.turns = None;
                SessionSummary {
                    thread,
                    branch: None,
                }
            })
            .collect::<Vec<_>>();
        data.sort_by(|a, b| {
            b.thread
                .updated_at
                .partial_cmp(&a.thread.updated_at)
                .unwrap()
        });
        Ok(SessionPage {
            data,
            next_cursor: None,
        })
    }
    async fn open(&self, id: &str, limit: usize, _: bool) -> Result<ThreadResponse, Failure> {
        let _permit = self.open_gate.acquire().await.unwrap();
        if let Some(error) = self.open_error.lock().unwrap().take() {
            return Err(error);
        }
        if let Some(error) = self.open_failure.lock().unwrap().clone() {
            return Err(error);
        }
        let mut response = self
            .records
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(unsupported)?;
        let turns = response.thread.turns.take().unwrap_or_default();
        let start = turns.len().saturating_sub(limit);
        response.thread.turns = Some(turns[start..].to_vec());
        response.thread.history_cursor = (start > 0).then(|| start.to_string());
        response.thread.history_has_more = Some(start > 0);
        if self.deferred_history {
            response.thread.defer_item_details(0);
        }
        Ok(response)
    }
    async fn read_history(&self, id: &str, cursor: &str, _: bool) -> Result<HistoryPage, Failure> {
        let response = self
            .records
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(unsupported)?;
        let end: usize = cursor.parse().unwrap();
        let start = end.saturating_sub(100);
        let turns = response.thread.turns.unwrap();
        Ok(HistoryPage {
            history_read_state: None,
            turns: turns[start..end].to_vec(),
            next_cursor: (start > 0).then(|| start.to_string()),
        })
    }
    async fn read_item(&self, params: &op::ReadItem) -> Result<op::ItemResponse, Failure> {
        if self
            .unreadable
            .lock()
            .unwrap()
            .contains(params.item_id.as_str())
        {
            return Err(Failure::new("unreadable", "the native body is not UTF-8"));
        }
        let records = self.records.lock().unwrap();
        let turns = records
            .get(&params.thread_id.id)
            .and_then(|record| record.thread.turns.as_ref())
            .ok_or_else(unsupported)?;
        let (turn, item) =
            agent_protocol::session::item_position(turns, &params.turn_id, &params.item_id)
                .ok_or_else(unsupported)?;
        let item = turns[turn].items.as_ref().unwrap()[item].clone();
        Ok(op::ItemResponse {
            item: (*item).clone(),
            transfer: None,
        })
    }
}

#[async_trait::async_trait]
impl Agent for NativeAgent {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            steer: true,
            model_change: true,
            rename: true,
            fork: self.provider == ProviderKind::Codex,
        }
    }
    fn availability(&self) -> Result<(), Failure> {
        Ok(())
    }
    fn validate_create(&self) -> Result<(), Failure> {
        Ok(())
    }
    async fn workspace_idle_warning(&self, _: &str) -> Option<&'static str> {
        self.idle_warning
    }
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        _: Option<serde_json::Value>,
    ) -> Result<ThreadResponse, Failure> {
        self.created_in
            .lock()
            .unwrap()
            .push(Path::new(cwd).is_dir());
        self.create_started.notify_one();
        let _permit = self
            .create_gate
            .acquire()
            .await
            .map_err(|_| Failure::new("fixture_create_failed", "native creation failed"))?;
        let id = format!(
            "{}-{}",
            self.provider.key(),
            self.next.fetch_add(1, Ordering::SeqCst)
        );
        let response = ThreadResponse {
            thread: Thread {
                id: Some(SessionRef {
                    provider: Some(self.provider),
                    id: id.clone(),
                }),
                cwd: Some(cwd.into()),
                name: Some(format!("{} conversation", self.provider.key())),
                status: SessionStatus::Idle,
                turns: Some(vec![]),
                ..Default::default()
            },
            model: Some(ModelRef {
                provider: self.provider,
                id: model.unwrap_or("model").into(),
            }),
            ..Default::default()
        };
        self.records.lock().unwrap().insert(id, response.clone());
        self.created.notify_one();
        let _reply = self.create_response_gate.acquire().await.unwrap();
        Ok(response)
    }
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure> {
        if let Some(error) = self.state_error.lock().unwrap().take() {
            return Err(error);
        }
        let mut response = self
            .records
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(unsupported)?;
        if self.metadata_only_state {
            response.thread.turns = None;
        }
        Ok(SubmissionState {
            response,
            needs_reload: self.needs_reload,
        })
    }
    async fn inject_history(&self, id: &str, context: &handoff::Context) -> Result<bool, Failure> {
        if self.provider != ProviderKind::Codex {
            return Ok(false);
        }
        self.injected
            .lock()
            .unwrap()
            .push((id.into(), context.clone()));
        if self.fail_inject.swap(0, Ordering::SeqCst) != 0 {
            return Err(Failure::unknown(
                "injection_unknown",
                "connection lost after writing history",
            ));
        }
        Ok(true)
    }
    async fn unload(&self, id: &str) -> Result<(), Failure> {
        self.released.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn submit(
        &self,
        input: &op::Submission,
        _: crate::providers::SubmissionTarget<'_>,
        _: bool,
        _: Option<serde_json::Value>,
        execution: uuid::Uuid,
    ) -> Result<op::SubmissionReceipt, Failure> {
        match self.fail_submit.swap(0, Ordering::SeqCst) {
            1 => return Err(Failure::new("refused", "prompt refused before consumption")),
            2 => {
                return Err(Failure::unknown(
                    "submission_unknown",
                    "acknowledgement lost",
                ));
            }
            _ => {}
        }
        self.inputs.lock().unwrap().push(input.clone());
        let ordinal = self.records.lock().unwrap()[&input.thread_id.id]
            .thread
            .turns
            .as_ref()
            .map_or(0, Vec::len);
        let mut user = Item::new(
            format!("user-{ordinal}").into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: op::Input::message_parts(&input.input),
            },
        );
        user.client_input_id = Some(input.client_user_message_id.clone());
        let turn_id: TurnId = format!("turn-{ordinal}").into();
        let turn = Turn {
            id: turn_id.clone(),
            status: TurnStatus::Completed,
            items: Some(vec![
                Arc::new(user),
                Arc::new(Item::new(
                    format!("answer-{ordinal}").into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: format!("answer {}", input.client_user_message_id),
                        phase: AssistantPhase::Final,
                    },
                )),
            ]),
            ..Default::default()
        };
        {
            let mut records = self.records.lock().unwrap();
            let record = records.get_mut(&input.thread_id.id).unwrap();
            record
                .thread
                .turns
                .get_or_insert_default()
                .push(Arc::new(turn.clone()));
            record.thread.updated_at = Some(record.thread.turns.as_ref().unwrap().len() as f64);
            record
                .thread
                .preview
                .get_or_insert_with(|| format!("request {}", input.client_user_message_id));
            record.model = input.model.clone();
        }
        crate::providers::emit_for(
            &self.events,
            AgentChange::Session {
                session: input.thread_id.clone(),
                change: SessionChange::Turn {
                    turn,
                    completed: true,
                },
            },
            Some(execution),
        )
        .await
        .map_err(|error| Failure::new("event_failed", error))?;
        Ok(op::SubmissionReceipt {
            turn_id: Some(turn_id),
        })
    }
    async fn interrupt(&self, _: &str, _: &TurnId) -> Result<Empty, Failure> {
        Ok(Empty {})
    }
    async fn discard_workspace_processes(&self, _: &Path) -> Result<(), Failure> {
        Ok(())
    }
    async fn fork(
        &self,
        id: &str,
        turn: &str,
        _: Option<serde_json::Value>,
    ) -> Result<ThreadResponse, Failure> {
        let source = self.open(id, usize::MAX, true).await?;
        let mut response = self
            .create(source.thread.cwd.as_deref().unwrap(), None, None)
            .await?;
        let mut turns = source.thread.turns.unwrap();
        turns.truncate(
            turns
                .iter()
                .rposition(|candidate| candidate.id.as_str() == turn)
                .unwrap()
                + 1,
        );
        response.thread.turns = Some(turns);
        self.records.lock().unwrap().insert(
            response.thread.id.as_ref().unwrap().id.clone(),
            response.clone(),
        );
        Ok(response)
    }
    fn event_stream(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.receiver.lock().unwrap().take()
    }
    async fn shutdown(&self) {}
}

pub(in crate::host_rpc) async fn call<T: DeserializeOwned>(
    service: &HostRpcService,
    session: SessionId,
    request: Call,
) -> Result<T, agent_protocol::error::RpcFailure> {
    let reply = service.dispatch(session, &request).await.unwrap();
    match agent_protocol::protocol::decode::<Response<T>>(&reply.initial).unwrap() {
        Response::Success { result } => Ok(result),
        Response::Failure { error } => Err(error),
    }
}

pub(in crate::host_rpc) fn own(
    host: &HostRpcService,
    public: &SessionRef,
    response: &ThreadResponse,
) {
    let native = response.thread.id.as_ref().unwrap();
    let mut imported = response.clone();
    imported.thread.id = Some(public.clone());
    imported.thread.active_provider = native.provider;
    host.inner
        .conversation
        .storage
        .import(public, &imported, Some(native), now())
        .unwrap();
}

pub(in crate::host_rpc) struct AnswerResource;
#[async_trait::async_trait]
impl crate::providers::AnswerSource for AnswerResource {
    fn is_alive(&self) -> bool {
        true
    }
    async fn prepare(
        &self,
        _: &serde_json::Value,
        _: &agent_protocol::requests::RequestBody,
        _: &agent_protocol::requests::Answer,
    ) -> Result<crate::providers::AnswerWrite, Failure> {
        Err(Failure::new(
            "fixture_not_answered",
            "this request is not answered",
        ))
    }
}
