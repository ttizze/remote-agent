use super::*;
use crate::providers::{
    AccountCommand, AccountReply, AgentChange, AgentEvent, Identity, SessionPage, SessionSummary,
    SubmissionState, emit,
};
use agent_protocol::{
    composer::ComposerCatalog,
    execution::{ItemStatus, SessionStatus, TurnStatus},
    ids::TurnId,
    items::{AssistantPhase, ItemBody},
    models::{Empty, Item, ModelRef, Turn},
    permissions::{PermissionMode, PermissionSettings},
    session::{Capabilities, SessionChange},
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
pub(in crate::host_rpc::service) struct NativeAgent {
    provider: ProviderKind,
    directory: PathBuf,
    pub(in crate::host_rpc::service) records: Arc<Mutex<HashMap<String, ThreadResponse>>>,
    pub(in crate::host_rpc::service) idle_warning: Option<&'static str>,
    deferred_history: bool,
    metadata_only_state: bool,
    /// Item IDs whose native bodies cannot be read.
    unreadable: Mutex<std::collections::HashSet<String>>,
    pub(in crate::host_rpc::service) inputs: Arc<Mutex<Vec<op::Submission>>>,
    injected: Arc<Mutex<Vec<(String, handoff::Context)>>>,
    released: Arc<Mutex<Vec<String>>>,
    pub(in crate::host_rpc::service) next: Arc<AtomicUsize>,
    pub(in crate::host_rpc::service) fail_submit: AtomicUsize,
    pub(in crate::host_rpc::service) create_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc::service) create_started: tokio::sync::Notify,
    fail_inject: AtomicUsize,
    open_error: Mutex<Option<Failure>>,
    state_error: Mutex<Option<Failure>>,
    /// Every open fails with this, including the open an import makes.
    open_failure: Mutex<Option<Failure>>,
    pub(in crate::host_rpc::service) fail_list: AtomicUsize,
    pub(in crate::host_rpc::service) fail_accounts: AtomicUsize,
    pub(in crate::host_rpc::service) accounts_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc::service) accounts_started: tokio::sync::Notify,
    events: mpsc::Sender<AgentEvent>,
    receiver: Mutex<Option<mpsc::Receiver<AgentEvent>>>,
}

fn unsupported() -> Failure {
    Failure::new("unsupported", "not used in this fixture")
}

impl NativeAgent {
    pub(in crate::host_rpc::service) fn new(
        provider: ProviderKind,
        directory: PathBuf,
    ) -> Arc<Self> {
        let (events, receiver) = mpsc::channel(32);
        Arc::new(Self {
            provider,
            directory,
            records: Default::default(),
            idle_warning: None,
            deferred_history: false,
            metadata_only_state: false,
            unreadable: Default::default(),
            inputs: Default::default(),
            injected: Default::default(),
            released: Default::default(),
            next: Default::default(),
            fail_submit: AtomicUsize::new(0),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
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
    pub(in crate::host_rpc::service) fn restarted(&self) -> Arc<Self> {
        let (events, receiver) = mpsc::channel(32);
        Arc::new(Self {
            provider: self.provider,
            directory: self.directory.clone(),
            records: self.records.clone(),
            idle_warning: self.idle_warning,
            deferred_history: self.deferred_history,
            metadata_only_state: self.metadata_only_state,
            unreadable: Default::default(),
            inputs: self.inputs.clone(),
            injected: self.injected.clone(),
            released: self.released.clone(),
            next: self.next.clone(),
            fail_submit: AtomicUsize::new(0),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
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
    pub(in crate::host_rpc::service) fn backend(self: &Arc<Self>) -> Backend {
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
impl Agent for NativeAgent {
    fn data_recipient(&self) -> &'static str {
        "fixture"
    }
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
    fn storage_directory(&self) -> &Path {
        &self.directory
    }
    async fn workspace_idle_warning(&self, _: &str) -> Option<&'static str> {
        self.idle_warning
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
    async fn read_turn_items(&self, id: &str, turn: &TurnId) -> Result<Vec<Arc<Item>>, Failure> {
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(id)
            .and_then(|record| record.thread.turns.as_ref())
            .and_then(|turns| turns.iter().rev().find(|candidate| &candidate.id == turn))
            .and_then(|turn| turn.items.clone())
            .ok_or_else(unsupported)?)
    }
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        _: Option<serde_json::Value>,
    ) -> Result<ThreadResponse, Failure> {
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
                    provider: self.provider,
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
            needs_reload: false,
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
        emit(
            &self.events,
            AgentChange::Session {
                session: input.thread_id.clone(),
                change: SessionChange::Turn {
                    turn,
                    completed: true,
                },
            },
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
    async fn discard_workspace_processes(&self, _: &Path) -> Result<(), Failure> {
        Ok(())
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
    async fn rename(&self, _: &str, _: &str) -> Result<Empty, Failure> {
        Ok(Empty {})
    }
    fn event_stream(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.receiver.lock().unwrap().take()
    }
    async fn shutdown(&self) {}
}

fn service(path: &Path, a: &Arc<NativeAgent>, b: &Arc<NativeAgent>) -> HostRpcService {
    HostRpcService::new(
        [a.backend(), b.backend()],
        ProjectStore::new(path.join("worktrees.json")),
    )
}

#[tokio::test]
async fn app_update_resumes_each_interrupted_conversation_once_with_its_settings() {
    use agent_local_host::update::{ContinuationDelivery, RECOVERY_FILE, Recovery};
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let mut ids = Vec::new();
    for agent in [&a, &b] {
        let mut response = agent
            .create(
                directory.path().to_str().unwrap(),
                Some("selected-model"),
                None,
            )
            .await
            .unwrap();
        let id = response.thread.id.clone().unwrap();
        response.thread.status = SessionStatus::Running;
        response.thread.turns = Some(vec![Arc::new(Turn {
            id: "interrupted-turn".into(),
            status: TurnStatus::Running,
            ..Default::default()
        })]);
        response.effort = Some("high".into());
        response.service_tier = Some("priority".into());
        agent
            .records
            .lock()
            .unwrap()
            .insert(id.id.clone(), response);
        ids.push(id);
    }
    let host = service(directory.path(), &a, &b);
    host.set_history_scope(directory.path().to_owned(), true);
    host.prepare_update().await.unwrap();
    let recovery_path = directory.path().join(RECOVERY_FILE);
    let recovery: Recovery =
        serde_json::from_slice(&std::fs::read(&recovery_path).unwrap()).unwrap();
    assert_eq!(recovery.tasks.len(), 2);
    assert!(
        recovery
            .tasks
            .iter()
            .all(
                |task| task.last_turn.as_ref().unwrap().as_str() == "interrupted-turn"
                    && task.cwd.as_deref() == directory.path().to_str()
            )
    );
    let connection = host.open_session();
    assert_eq!(
        send(
            &host,
            connection.id(),
            &ids[0],
            ProviderKind::Codex,
            "during-update",
            "new input"
        )
        .await
        .unwrap_err()
        .code,
        "host_updating"
    );
    assert!(host.prepare_update().await.is_err());
    drop(host);
    let (a, b) = (a.restarted(), b.restarted());
    for agent in [&a, &b] {
        for response in agent.records.lock().unwrap().values_mut() {
            response.thread.status = SessionStatus::Idle;
            Arc::make_mut(&mut response.thread.turns.as_mut().unwrap()[0]).status =
                TurnStatus::Interrupted;
        }
    }
    let host = service(directory.path(), &a, &b);
    host.resume_after_update(directory.path()).await.unwrap();
    host.resume_after_update(directory.path()).await.unwrap();
    for (agent, id) in [&a, &b].into_iter().zip(ids) {
        let inputs = agent.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].thread_id, id);
        assert_eq!(inputs[0].model.as_ref().unwrap().id, "selected-model");
        assert_eq!(inputs[0].effort.as_deref(), Some("high"));
        assert_eq!(inputs[0].service_tier.as_deref(), Some("priority"));
        assert!(
            matches!(&inputs[0].input[0], op::Input::Text { text } if text.contains("中断したところから"))
        );
    }
    let recovery: Recovery =
        serde_json::from_slice(&std::fs::read(recovery_path).unwrap()).unwrap();
    assert!(recovery.tasks.iter().all(|task| matches!(&task.delivery, ContinuationDelivery::Accepted(turn) if turn.as_str() == "turn-1")));
}

#[tokio::test]
async fn app_update_does_not_repeat_uncertain_failed_or_finished_continuations() {
    use agent_local_host::update::{
        ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery,
    };
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let mut tasks = Vec::new();
    for (delivery, finished) in [
        (ContinuationDelivery::Pending, false),
        (ContinuationDelivery::Sending, false),
        (ContinuationDelivery::Failed("already failed".into()), false),
        (ContinuationDelivery::Pending, true),
    ] {
        let mut response = a
            .create(directory.path().to_str().unwrap(), None, None)
            .await
            .unwrap();
        if finished {
            response.thread.turns = Some(vec![Arc::new(Turn {
                id: "finished-during-shutdown".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            })]);
            a.records.lock().unwrap().insert(
                response.thread.id.as_ref().unwrap().id.clone(),
                response.clone(),
            );
        }
        let last_turn = finished.then(|| "finished-during-shutdown".into());
        let session = response.thread.id.unwrap();
        tasks.push(InterruptedTask {
            native_session: session.clone(),
            submission: op::Submission {
                thread_id: session,
                client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                input: vec![op::Input::Text {
                    text: "Continue".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            cwd: None,
            last_turn,
            delivery,
        });
    }
    let path = directory.path().join(RECOVERY_FILE);
    agent_local_host::save_private_json(&path, &Recovery { tasks }).unwrap();
    a.fail_submit.store(1, Ordering::SeqCst);
    let host = service(directory.path(), &a, &b);
    host.resume_after_update(directory.path()).await.unwrap();
    host.resume_after_update(directory.path()).await.unwrap();
    assert!(a.inputs.lock().unwrap().is_empty());
    let recovery: Recovery = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(matches!(
        recovery.tasks[0].delivery,
        ContinuationDelivery::Failed(_)
    ));
    assert_eq!(recovery.tasks[1].delivery, ContinuationDelivery::Sending);
    assert!(matches!(
        recovery.tasks[2].delivery,
        ContinuationDelivery::Failed(_)
    ));
    assert_eq!(recovery.tasks[3].delivery, ContinuationDelivery::Completed);
}

pub(in crate::host_rpc::service) async fn call<T: DeserializeOwned>(
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

async fn send(
    service: &HostRpcService,
    session: SessionId,
    root: &SessionRef,
    provider: ProviderKind,
    id: &str,
    text: &str,
) -> Result<op::SubmissionReceipt, agent_protocol::error::RpcFailure> {
    call(
        service,
        session,
        Call::Submit(op::Submission {
            thread_id: root.clone(),
            client_user_message_id: id.into(),
            input: vec![op::Input::Text { text: text.into() }],
            model: Some(ModelRef {
                provider,
                id: "model".into(),
            }),
            effort: None,
            service_tier: None,
            follow_up: op::FollowUp::Queue,
        }),
    )
    .await
}

async fn history(
    service: &HostRpcService,
    session: SessionId,
    root: &SessionRef,
) -> ThreadResponse {
    call::<agent_protocol::session::OpenedSession>(
        service,
        session,
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 1000,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap()
    .response
}

struct HandoffFixture {
    codex: Arc<NativeAgent>,
    claude: Arc<NativeAgent>,
    root: SessionRef,
    original: TurnId,
    current: TurnId,
}

// Native import fixtures have never committed a complete Host transcript.
// Provider outages must keep readable ranges visible without claiming completeness.
async fn unimported_handoff(directory: &Path) -> HandoffFixture {
    use crate::host_rpc::conversations::{Binding, Segment};
    let directory = dunce::canonicalize(directory).unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let source = codex
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap();
    let root = source.thread.id.clone().unwrap();
    let original = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "original",
        "Original task",
    )
    .await
    .unwrap()
    .turn_id
    .unwrap();
    let target = claude
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let current = send(
        &host,
        client.id(),
        &target,
        ProviderKind::Claude,
        "review",
        "Review the task",
    )
    .await
    .unwrap()
    .turn_id
    .unwrap();
    let mut row = Conversation::new(&source.thread).unwrap();
    row.segments[0].end = Some(1);
    row.segments.push(Segment {
        session: target.clone(),
        from: 0,
        end: None,
    });
    row.bindings.push(Binding {
        session: target.clone(),
        covered: None,
    });
    row.active = target.clone();
    let current = row.identity(&target, &current).into();
    host.inner.conversations.save(row).unwrap();
    HandoffFixture {
        codex,
        claude,
        root,
        original,
        current,
    }
}

fn message_texts(response: &ThreadResponse) -> Vec<String> {
    response
        .thread
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter_map(|item| match item.body() {
            ItemBody::UserMessage { content, .. } => content.iter().find_map(|part| match part {
                agent_protocol::items::MessagePart::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn rewinding_keeps_the_conversation_and_continues_without_the_later_turns() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let codex = ProviderKind::Codex;
    send(&host, client.id(), &root, codex, "one", "first")
        .await
        .unwrap();
    let edited = send(&host, client.id(), &root, codex, "two", "second")
        .await
        .unwrap()
        .turn_id
        .unwrap();
    send(&host, client.id(), &root, codex, "three", "third")
        .await
        .unwrap();

    let mut other = host.open_session();
    let opened = host
        .dispatch(
            other.id(),
            &Call::OpenSession(agent_protocol::session::OpenSession {
                session: root.clone(),
                limit: 100,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
    let _subscription = opened.updates.unwrap();
    let before_version = host
        .inner
        .transcripts
        .window(&root, None, 100)
        .unwrap()
        .unwrap()
        .response
        .thread
        .transcript_version
        .unwrap();

    let rewound: ThreadResponse = call(
        &host,
        client.id(),
        Call::RewindSession(op::RewindSession::new(root.clone(), edited)),
    )
    .await
    .unwrap();
    assert_eq!(rewound.thread.id.as_ref(), Some(&root));
    assert_eq!(message_texts(&rewound), ["first"]);
    let replaced = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notice: Notification =
                agent_protocol::protocol::decode(&other.recv().await.unwrap()).unwrap();
            if let Notification::SessionHistoryReplaced { thread } = notice {
                break thread;
            }
        }
    })
    .await
    .unwrap();

    let committed_version = host
        .inner
        .transcripts
        .window(&root, None, 100)
        .unwrap()
        .unwrap()
        .response
        .thread
        .transcript_version
        .unwrap();
    assert!(committed_version > before_version);
    assert_eq!(rewound.thread.transcript_version, Some(committed_version));
    assert_eq!(replaced.transcript_version, Some(committed_version));
    assert_eq!(replaced.id.as_ref(), Some(&root));
    assert_eq!(
        message_texts(&ThreadResponse {
            thread: *replaced,
            ..Default::default()
        }),
        ["first"]
    );
    assert_eq!(
        message_texts(&history(&host, client.id(), &root).await),
        ["first"]
    );

    // The next message goes to a native session that never saw the removed turns.
    send(&host, client.id(), &root, codex, "four", "again")
        .await
        .unwrap();
    let delivered = a.inputs.lock().unwrap().last().unwrap().thread_id.clone();
    assert_ne!(delivered, root);
    let next = a.records.lock().unwrap()[&delivered.id].clone();
    assert_eq!(message_texts(&next), ["first", "again"]);
    let reopened = history(&host, client.id(), &root).await;
    assert_eq!(message_texts(&reopened), ["first", "again"]);

    // Rewinding past the first message leaves an empty conversation that still works.
    let first = reopened.thread.turns.as_ref().unwrap()[0].id.clone();
    let empty: ThreadResponse = call(
        &host,
        client.id(),
        Call::RewindSession(op::RewindSession::new(root.clone(), first)),
    )
    .await
    .unwrap();
    assert_eq!(empty.thread.id.as_ref(), Some(&root));
    assert!(message_texts(&empty).is_empty());
    send(&host, client.id(), &root, codex, "five", "restart")
        .await
        .unwrap();
    assert_eq!(
        message_texts(&history(&host, client.id(), &root).await),
        ["restart"]
    );
}

#[tokio::test]
async fn rewind_publication_failure_leaves_the_conversation_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "one",
        "first",
    )
    .await
    .unwrap();
    let edited = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "two",
        "second",
    )
    .await
    .unwrap()
    .turn_id
    .unwrap();
    let binding = host
        .inner
        .conversations
        .get(&root)
        .unwrap()
        .map(|row| row.active);
    let projects = directory.path().join("bex-projects.json");
    std::fs::write(&projects, b"broken projects").unwrap();
    let error = call::<ThreadResponse>(
        &host,
        client.id(),
        Call::RewindSession(op::RewindSession::new(root.clone(), edited)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "project_state_unavailable");
    assert_eq!(
        host.inner
            .conversations
            .get(&root)
            .unwrap()
            .map(|row| row.active),
        binding
    );
    assert_eq!(
        message_texts(&host.inner.transcripts.history(&root).unwrap().unwrap()),
        ["first", "second"]
    );
    std::fs::remove_file(projects).unwrap();
    assert_eq!(
        message_texts(&history(&host, client.id(), &root).await),
        ["first", "second"]
    );
}

#[tokio::test]
async fn rewinding_refuses_running_work_and_unknown_turns_without_a_new_session() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let turn = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "one",
        "first",
    )
    .await
    .unwrap()
    .turn_id
    .unwrap();
    let before = a.records.lock().unwrap().len();
    let rewind = |turn: TurnId| {
        call::<ThreadResponse>(
            &host,
            client.id(),
            Call::RewindSession(op::RewindSession::new(root.clone(), turn)),
        )
    };
    assert_eq!(
        rewind("missing".into()).await.unwrap_err().code,
        "invalid_turn"
    );
    {
        let mut records = a.records.lock().unwrap();
        let record = records.get_mut(&root.id).unwrap();
        let last = record.thread.turns.as_mut().unwrap().last_mut().unwrap();
        Arc::make_mut(last).status = TurnStatus::Running;
    }
    assert_eq!(rewind(turn).await.unwrap_err().code, "conversation_busy");
    assert_eq!(a.records.lock().unwrap().len(), before);
}

#[tokio::test]
async fn covered_rewind_refuses_live_work_missing_from_the_stored_transcript() {
    for observation in [
        "native_status",
        "native_turn",
        "live_status",
        "live_request",
        "live_turn",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        let root = a
            .create(directory.path().to_str().unwrap(), None, None)
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        let host = service(directory.path(), &a, &b);
        let client = host.open_session();
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Codex,
            "one",
            "first",
        )
        .await
        .unwrap();
        let edited = send(
            &host,
            client.id(),
            &root,
            ProviderKind::Claude,
            "two",
            "second",
        )
        .await
        .unwrap()
        .turn_id
        .unwrap();
        let binding = host.inner.conversations.get(&root).unwrap().unwrap().active;
        let before = b.records.lock().unwrap().len();
        let _reader = host.inner.router.retain_execution(root.clone()).unwrap();
        let turn = Turn {
            id: "not-yet-recorded".into(),
            status: TurnStatus::Running,
            ..Default::default()
        };
        match observation {
            "native_status" => {
                b.records
                    .lock()
                    .unwrap()
                    .get_mut(&binding.id)
                    .unwrap()
                    .thread
                    .status = SessionStatus::Running;
            }
            "native_turn" => {
                b.records
                    .lock()
                    .unwrap()
                    .get_mut(&binding.id)
                    .unwrap()
                    .thread
                    .turns
                    .get_or_insert_default()
                    .push(Arc::new(turn));
            }
            "live_status" => host.inner.router.session_change(
                &root,
                SessionChange::Status {
                    status: SessionStatus::Preparing,
                },
            ),
            "live_request" => host.inner.router.session_change(
                &root,
                SessionChange::Request {
                    request: serde_json::from_value(serde_json::json!({
                        "id":"pending", "target":"session", "delivery":"awaiting",
                        "body":{"question":{"questions":[]}}
                    }))
                    .unwrap(),
                },
            ),
            _ => host.inner.router.session_change(
                &root,
                SessionChange::Turn {
                    turn,
                    completed: false,
                },
            ),
        }
        let stored = host.inner.transcripts.history(&root).unwrap().unwrap();
        assert!(
            stored
                .thread
                .turns
                .iter()
                .flatten()
                .all(|turn| turn.status == TurnStatus::Completed)
        );
        let error = call::<ThreadResponse>(
            &host,
            client.id(),
            Call::RewindSession(op::RewindSession::new(root.clone(), edited)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "conversation_busy", "{observation}");
        assert_eq!(
            host.inner.conversations.get(&root).unwrap().unwrap().active,
            binding
        );
        assert_eq!(b.records.lock().unwrap().len(), before);
        assert_eq!(
            message_texts(&host.inner.transcripts.history(&root).unwrap().unwrap()),
            ["first", "second"]
        );
    }
}

#[tokio::test]
async fn rewind_without_subscribers_serializes_input_until_the_new_binding_is_ready() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let id = client.id();
    let edited = send(&host, id, &root, ProviderKind::Codex, "one", "before")
        .await
        .unwrap()
        .turn_id
        .unwrap();
    // No OpenSession subscription retains the idle conversation's actor.
    a.create_started.notified().await;
    let blocked = a.create_gate.acquire().await.unwrap();
    let rewind = {
        let host = host.clone();
        let root = root.clone();
        tokio::spawn(async move {
            call::<ThreadResponse>(
                &host,
                id,
                Call::RewindSession(op::RewindSession::new(root, edited)),
            )
            .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        a.create_started.notified(),
    )
    .await
    .unwrap();
    let mut sending = {
        let host = host.clone();
        let root = root.clone();
        tokio::spawn(
            async move { send(&host, id, &root, ProviderKind::Codex, "two", "after").await },
        )
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut sending)
            .await
            .is_err()
    );
    assert_eq!(a.inputs.lock().unwrap().len(), 1);
    drop(blocked);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        rewind.await.unwrap().unwrap();
        sending.await.unwrap().unwrap();
    })
    .await
    .unwrap();
    assert_eq!(message_texts(&history(&host, id, &root).await), ["after"]);
    let binding = host.inner.conversations.get(&root).unwrap().unwrap().active;
    assert_ne!(binding, root);
    assert_eq!(a.inputs.lock().unwrap().last().unwrap().thread_id, binding);
}

#[tokio::test]
async fn a_failed_native_thread_registration_unloads_and_can_be_retried() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let registry = directory.path().join("bex-conversations.json");
    std::fs::create_dir(&registry).unwrap();
    let request = Call::CreateSession(op::CreateSession {
        provider: ProviderKind::Claude,
        cwd: Some(directory.path().to_string_lossy().into_owned()),
        model: None,
        initial_input: None,
    });
    let error = call::<agent_protocol::session::OpenedSession>(&host, client.id(), request.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "conversation_storage_failed");
    assert_eq!(b.released.lock().unwrap().as_slice(), ["claude-0"]);
    assert!(
        host.inner
            .conversations
            .active_sessions("")
            .unwrap()
            .is_empty()
    );

    std::fs::remove_dir(&registry).unwrap();
    let created: agent_protocol::session::OpenedSession =
        call(&host, client.id(), request).await.unwrap();
    assert_eq!(
        host.inner.conversations.active_sessions("").unwrap(),
        [created.session]
    );
}

#[tokio::test]
async fn native_fork_rejects_an_unsupported_provider_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = b
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let turn = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "native",
        "Native task",
    )
    .await
    .unwrap()
    .turn_id
    .unwrap();
    let before = b.records.lock().unwrap().len();
    let denied = call::<ThreadResponse>(
        &host,
        client.id(),
        Call::ForkSession(op::ForkSession::new(root, turn)),
    )
    .await
    .unwrap_err();
    assert_eq!(b.records.lock().unwrap().len(), before);
    assert_eq!(denied.code, "unsupported_operation");
    assert_eq!(
        denied.message,
        "host/session/fork is unsupported by this provider"
    );
}

#[tokio::test]
async fn unavailable_current_provider_does_not_block_earlier_segments_or_local_rename() {
    let directory = tempfile::tempdir().unwrap();
    let HandoffFixture {
        codex: a,
        root,
        original,
        current,
        ..
    } = unimported_handoff(directory.path()).await;

    // The saved handoff survives a restart, while only its current provider fails.
    let host = HostRpcService::new(
        [
            a.restarted().backend(),
            Backend::unavailable(ProviderKind::Claude, "Claude failed to start"),
        ],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let mut client = host.open_session();
    let opened = host
        .dispatch(
            client.id(),
            &Call::OpenSession(agent_protocol::session::OpenSession {
                session: root.clone(),
                limit: 100,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
    let opened_session: agent_protocol::session::OpenedSession =
        match agent_protocol::protocol::decode::<Response<agent_protocol::session::OpenedSession>>(
            &opened.initial,
        )
        .unwrap()
        {
            Response::Success { result } => result,
            Response::Failure { error } => panic!("open failed: {error:?}"),
        };
    assert_eq!(
        opened_session.response.thread.active_provider,
        Some(ProviderKind::Claude)
    );
    let state = opened_session
        .response
        .thread
        .history_read_state
        .as_ref()
        .unwrap();
    assert_eq!(
        state.kind,
        agent_protocol::session::HistoryReadKind::Incomplete
    );
    assert_eq!(state.issues, ["claude: Claude failed to start"]);
    assert!(opened_session.response.thread.capabilities.unwrap().rename);
    assert!(!opened_session.response.thread.capabilities.unwrap().steer);
    assert!(
        !opened_session
            .response
            .thread
            .capabilities
            .unwrap()
            .model_change
    );
    assert!(opened_session.response.thread.capabilities.unwrap().fork);
    assert!(
        opened_session
            .response
            .thread
            .turns
            .as_ref()
            .unwrap()
            .iter()
            .all(|turn| turn.provider == Some(ProviderKind::Codex)
                && turn.supports_fork == Some(true))
    );
    let mut updates = opened.updates.unwrap();
    let _: Empty = call(
        &host,
        client.id(),
        Call::RenameSession(op::RenameSession {
            thread_id: root.clone(),
            name: "Local title".into(),
        }),
    )
    .await
    .unwrap();
    let notice = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    let notice: agent_protocol::protocol::Notification =
        agent_protocol::protocol::decode(&notice).unwrap();
    assert!(
        matches!(notice, agent_protocol::protocol::Notification::SessionRenamed { session, name: Some(name), .. } if session == root && name == "Local title")
    );
    assert_eq!(
        host.inner
            .conversations
            .get(&root)
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Local title")
    );
    let _: Empty = call(
        &host,
        client.id(),
        Call::ReadTurnItems(agent_protocol::session::ReadTurnItems {
            session: root.clone(),
            turn_id: original.clone(),
        }),
    )
    .await
    .unwrap();
    let hydration = tokio::time::timeout(std::time::Duration::from_secs(2), updates.recv())
        .await
        .unwrap()
        .unwrap();
    let hydration: SessionChange = agent_protocol::protocol::decode(&hydration).unwrap();
    let SessionChange::TurnItems { turn_id, items } = hydration else {
        panic!("turn hydration missing")
    };
    assert_eq!(turn_id, original);
    let texts = handoff::Message::from_turn(&Turn {
        items: Some(items),
        ..Default::default()
    })
    .map(|message| message.text)
    .collect::<Vec<_>>();
    assert_eq!(texts, ["Original task", "answer original"]);
    let fork: ThreadResponse = call(
        &host,
        client.id(),
        Call::ForkSession(op::ForkSession::new(root.clone(), original)),
    )
    .await
    .unwrap();
    assert_ne!(fork.thread.id.as_ref(), Some(&root));
    assert_eq!(fork.thread.active_provider, Some(ProviderKind::Codex));
    let texts = fork
        .thread
        .turns
        .as_ref()
        .unwrap()
        .iter()
        .flat_map(|turn| handoff::Message::from_turn(turn))
        .map(|message| message.text)
        .collect::<Vec<_>>();
    assert_eq!(texts, ["Original task", "answer original"]);
    for search_term in ["", "Local title"] {
        let listed: agent_protocol::models::ThreadList = call(
            &host,
            client.id(),
            Call::ListSessions(op::ListSessions {
                query: agent_protocol::models::ListQuery {
                    search_term: search_term.into(),
                    ..Default::default()
                },
            }),
        )
        .await
        .unwrap();
        let thread = listed
            .data
            .iter()
            .find(|thread| thread.id.as_ref() == Some(&root))
            .expect("saved conversation stays discoverable");
        assert!(thread.capabilities.unwrap().rename);
        assert!(thread.capabilities.unwrap().fork);
        assert!(!thread.capabilities.unwrap().model_change);
        assert!(listed.provider_errors.is_some());
    }
    let send_error = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "unavailable",
        "Continue",
    )
    .await
    .unwrap_err();
    assert_eq!(send_error.code, "provider_unavailable");
    assert_eq!(send_error.message, "Claude failed to start");
    let switch_error = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "switch-unavailable",
        "Change model",
    )
    .await
    .unwrap_err();
    assert_eq!(switch_error.code, "provider_unavailable");
    assert_eq!(switch_error.message, "Claude failed to start");
    let error = call::<Empty>(
        &host,
        client.id(),
        Call::Interrupt(op::Interrupt {
            thread_id: root,
            turn_id: current,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "provider_unavailable");
    assert_eq!(error.message, "Claude failed to start");
}

#[tokio::test]
async fn unavailable_conversations_keep_project_membership_and_page_warnings() {
    let directory = tempfile::tempdir().unwrap();
    let HandoffFixture { codex, root, .. } = unimported_handoff(directory.path()).await;
    let projects = ProjectStore::new(directory.path().join("worktrees.json"));
    projects.register(directory.path()).await.unwrap();
    let project_id = projects.load().await.unwrap().projects[0].id.clone();
    let host = HostRpcService::new(
        [
            codex.restarted().backend(),
            Backend::unavailable(ProviderKind::Claude, "Claude failed to start"),
        ],
        projects,
    );
    let client = host.open_session();
    let listed: agent_protocol::models::ThreadList = call(
        &host,
        client.id(),
        Call::ListProjectSessions(op::ListProjectSessions {
            project_id: project_id.clone(),
            limit: 20,
            search_term: String::new(),
        }),
    )
    .await
    .unwrap();
    let summary = listed
        .data
        .iter()
        .find(|thread| thread.id.as_ref() == Some(&root))
        .unwrap();
    assert_eq!(summary.project_id.as_deref(), Some(project_id.as_str()));
    assert_eq!(
        summary.status,
        agent_protocol::models::SessionStatus::Unavailable
    );
    let page: agent_protocol::session::HistoryPage = call(
        &host,
        client.id(),
        Call::ReadHistory(agent_protocol::session::ReadHistory {
            session: root,
            cursor: "1".into(),
            include_activity: true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.turns.len(), 1);
    assert!(page.next_cursor.is_none());
    let state = page.history_read_state.unwrap();
    assert_eq!(
        state.kind,
        agent_protocol::session::HistoryReadKind::Incomplete
    );
    assert_eq!(state.issues, ["claude: Claude failed to start"]);
}

#[tokio::test]
async fn unavailable_past_provider_keeps_current_history_and_operations_usable() {
    let directory = tempfile::tempdir().unwrap();
    let HandoffFixture {
        claude,
        root,
        original,
        current,
        ..
    } = unimported_handoff(directory.path()).await;
    let host = HostRpcService::new(
        [
            crate::providers::codex::Codex::new(Err("Codex failed to start".into())).into(),
            claude.restarted().backend(),
        ],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 100,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        opened
            .response
            .thread
            .turns
            .as_ref()
            .unwrap()
            .iter()
            .map(|turn| turn.id.clone())
            .collect::<Vec<_>>(),
        [current]
    );
    assert_eq!(
        opened
            .response
            .thread
            .history_read_state
            .as_ref()
            .unwrap()
            .issues,
        ["codex: Codex failed to start"]
    );
    assert_eq!(
        opened.response.thread.turns.as_ref().unwrap()[0].supports_fork,
        Some(false)
    );
    assert!(opened.response.thread.capabilities.unwrap().model_change);
    assert!(!opened.response.thread.capabilities.unwrap().fork);
    let failed = call::<Empty>(
        &host,
        client.id(),
        Call::ReadTurnItems(agent_protocol::session::ReadTurnItems {
            session: root.clone(),
            turn_id: original.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (failed.code.as_str(), failed.message.as_str()),
        ("provider_unavailable", "Codex failed to start")
    );
    let failed = call::<ThreadResponse>(
        &host,
        client.id(),
        Call::ForkSession(op::ForkSession::new(root.clone(), original)),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (failed.code.as_str(), failed.message.as_str()),
        ("provider_unavailable", "Codex failed to start")
    );
    let page: handoff::Page = call(
        &host,
        client.id(),
        Call::ReadHandoffHistory(handoff::ReadHistory {
            session: root.clone(),
            offset: 0,
            limit: 20,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        page.history_read_state.unwrap().issues,
        ["codex: Codex failed to start"]
    );
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.message.text.as_str())
            .collect::<Vec<_>>(),
        ["Review the task", "answer review"]
    );
    assert!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Claude,
            "still-current",
            "Continue"
        )
        .await
        .unwrap()
        .turn_id
        .is_some()
    );
}

#[tokio::test]
async fn saved_handoff_retains_complete_history_when_the_loaded_provider_fails() {
    let directory = tempfile::tempdir().unwrap();
    let HandoffFixture {
        codex,
        claude,
        root,
        ..
    } = unimported_handoff(directory.path()).await;
    let healthy = service(directory.path(), &codex.restarted(), &claude.restarted());
    let client = healthy.open_session();
    send(
        &healthy,
        client.id(),
        &root,
        ProviderKind::Codex,
        "return",
        "Continue",
    )
    .await
    .unwrap();
    drop(client);
    drop(healthy);
    let host = HostRpcService::new(
        [
            crate::providers::codex::Codex::new(Err("Codex failed to start".into())).into(),
            claude.restarted().backend(),
        ],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 100,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    let turns = opened.response.thread.turns.unwrap();
    // A provider outage changes execution availability, not the completeness
    // of the conversation already committed to the Host transcript.
    assert_eq!(
        turns
            .iter()
            .flat_map(|turn| handoff::Message::from_turn(turn))
            .map(|message| message.text)
            .collect::<Vec<_>>(),
        [
            "Original task",
            "answer original",
            "Review the task",
            "answer review",
            "Continue",
            "answer return"
        ]
    );
    assert_eq!(opened.response.thread.status, SessionStatus::Unavailable);
    let state = opened.response.thread.history_read_state.unwrap();
    assert_eq!(
        state.kind,
        agent_protocol::session::HistoryReadKind::Complete
    );
    assert!(state.issues.is_empty());
    assert!(!opened.response.thread.capabilities.unwrap().model_change);
    assert!(!opened.response.thread.capabilities.unwrap().fork);
    let listed: agent_protocol::models::ThreadList = call(
        &host,
        client.id(),
        Call::ListSessions(op::ListSessions {
            query: Default::default(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        listed
            .data
            .iter()
            .filter(|thread| thread.id.as_ref() == Some(&root))
            .count(),
        1
    );
    let error = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "unavailable",
        "Continue",
    )
    .await
    .unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        ("provider_unavailable", "Codex failed to start")
    );
}

#[tokio::test]
async fn opening_does_not_hide_native_history_errors_as_startup_failures() {
    for code in ["native_resume_failed", "provider_unavailable"] {
        let directory = tempfile::tempdir().unwrap();
        let HandoffFixture { codex, root, .. } = unimported_handoff(directory.path()).await;
        let codex = codex.restarted();
        *codex.open_failure.lock().unwrap() =
            Some(Failure::new(code, "native session cannot be loaded"));
        let host = HostRpcService::new(
            [
                codex.backend(),
                Backend::unavailable(ProviderKind::Claude, "Claude failed to start"),
            ],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let error = call::<agent_protocol::session::OpenedSession>(
            &host,
            client.id(),
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: root,
                limit: 100,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "session_open_failed");
        assert_eq!(error.message, "native session cannot be loaded");
    }
}

#[tokio::test]
async fn handoff_does_not_silently_transfer_only_the_readable_sources() {
    for unavailable in [
        Backend::unavailable(ProviderKind::Codex, "Codex failed to start"),
        crate::providers::codex::Codex::new(Err("Codex failed to start".into())).into(),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let HandoffFixture { claude, root, .. } = unimported_handoff(directory.path()).await;
        let deepseek = NativeAgent::new(ProviderKind::DeepSeek, directory.path().join("deepseek"));
        let host = HostRpcService::new(
            [
                unavailable,
                claude.restarted().backend(),
                deepseek.backend(),
            ],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let error = send(
            &host,
            client.id(),
            &root,
            ProviderKind::DeepSeek,
            "transfer",
            "Continue",
        )
        .await
        .unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            ("provider_unavailable", "Codex failed to start")
        );
        assert!(deepseek.injected.lock().unwrap().is_empty());
        assert!(deepseek.inputs.lock().unwrap().is_empty());
        let opened: agent_protocol::session::OpenedSession = call(
            &host,
            client.id(),
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: root,
                limit: 100,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            opened.response.thread.active_provider,
            Some(ProviderKind::Claude)
        );
    }
}

#[tokio::test]
async fn all_unavailable_providers_still_open_saved_metadata_and_report_each_failure() {
    let directory = tempfile::tempdir().unwrap();
    let HandoffFixture { root, .. } = unimported_handoff(directory.path()).await;
    let host = HostRpcService::new(
        [
            Backend::unavailable(ProviderKind::Codex, "Codex failed to start"),
            Backend::unavailable(ProviderKind::Claude, "Claude failed to start"),
        ],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 100,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert!(opened.response.thread.turns.as_ref().unwrap().is_empty());
    assert_eq!(
        opened.response.thread.status,
        agent_protocol::models::SessionStatus::Unavailable
    );
    assert_eq!(opened.response.thread.id.as_ref(), Some(&root));
    assert_eq!(
        opened.response.model.unwrap().provider,
        ProviderKind::Claude
    );
    assert!(opened.response.effort.is_none());
    assert!(opened.response.service_tier.is_none());
    assert_eq!(
        opened.response.thread.history_read_state.unwrap().kind,
        agent_protocol::session::HistoryReadKind::Unavailable
    );
    assert_eq!(
        opened.response.thread.capabilities.unwrap(),
        Capabilities {
            rename: true,
            steer: false,
            model_change: false,
            fork: false
        }
    );
    let listed: agent_protocol::models::ThreadList = call(
        &host,
        client.id(),
        Call::ListSessions(op::ListSessions {
            query: Default::default(),
        }),
    )
    .await
    .unwrap();
    assert!(
        listed
            .data
            .iter()
            .any(|thread| thread.id.as_ref() == Some(&root))
    );
    let create_error = call::<agent_protocol::session::OpenedSession>(
        &host,
        client.id(),
        Call::CreateSession(op::CreateSession {
            initial_input: None,
            provider: ProviderKind::Claude,
            cwd: Some(directory.path().to_str().unwrap().into()),
            model: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (create_error.code.as_str(), create_error.message.as_str()),
        ("provider_unavailable", "Claude failed to start")
    );
}

#[tokio::test]
async fn reopening_after_restart_restores_only_accepted_turn_settings() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), Some("model"), None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let input = |id: &str, provider, effort: &str, tier: &str| op::Submission {
        thread_id: root.clone(),
        client_user_message_id: id.into(),
        input: vec![op::Input::Text {
            text: "Continue".into(),
        }],
        model: Some(ModelRef {
            provider,
            id: "model".into(),
        }),
        effort: Some(effort.into()),
        service_tier: Some(tier.into()),
        follow_up: Default::default(),
    };
    call::<op::SubmissionReceipt>(
        &host,
        client.id(),
        Call::Submit(input("first", ProviderKind::Codex, "high", "priority")),
    )
    .await
    .unwrap();
    let second_client = host.open_session();
    let reopened = history(&host, second_client.id(), &root).await;
    assert_eq!(reopened.effort.as_deref(), Some("high"));
    assert_eq!(reopened.service_tier.as_deref(), Some("priority"));
    // Switching providers must address settings by native identity while the
    // client continues opening the original public conversation.
    call::<op::SubmissionReceipt>(
        &host,
        client.id(),
        Call::Submit(input("switch", ProviderKind::Claude, "max", "default")),
    )
    .await
    .unwrap();
    drop(second_client);
    drop(client);
    drop(host);
    let a = a.restarted();
    let b = b.restarted();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    let reopened = history(&host, client.id(), &root).await;
    assert_eq!(reopened.model.unwrap().provider, ProviderKind::Claude);
    assert_eq!(reopened.effort.as_deref(), Some("max"));
    assert_eq!(reopened.service_tier.as_deref(), Some("default"));
    b.fail_submit.store(1, Ordering::SeqCst);
    assert!(
        call::<op::SubmissionReceipt>(
            &host,
            client.id(),
            Call::Submit(input("refused", ProviderKind::Claude, "low", "priority"))
        )
        .await
        .is_err()
    );
    let reopened = history(&host, client.id(), &root).await;
    assert_eq!(reopened.effort.as_deref(), Some("max"));
    assert_eq!(reopened.service_tier.as_deref(), Some("default"));
}

#[tokio::test]
async fn handoffs_use_the_complete_host_transcript_after_native_history_breaks() {
    use agent_protocol::session::{HistoryReadKind, HistoryReadState};
    let directory = tempfile::tempdir().unwrap();
    let mut a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let mut b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    Arc::get_mut(&mut a).unwrap().metadata_only_state = true;
    Arc::get_mut(&mut b).unwrap().metadata_only_state = true;
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Original requirement",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Review requirement",
    )
    .await
    .unwrap();
    let native_b = b.inputs.lock().unwrap()[0].thread_id.clone();
    for agent in [&a, &b] {
        *agent.open_failure.lock().unwrap() = Some(Failure::new(
            "incomplete_history",
            "transcript parent is unavailable",
        ));
        for record in agent.records.lock().unwrap().values_mut() {
            record.thread.history_read_state = Some(HistoryReadState::new(
                HistoryReadKind::Incomplete,
                vec!["transcript parent is unavailable".into()],
            ));
        }
    }
    b.records
        .lock()
        .unwrap()
        .get_mut(&native_b.id)
        .unwrap()
        .thread
        .status = SessionStatus::Running;
    assert_eq!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Codex,
            "busy",
            "Continue"
        )
        .await
        .unwrap_err()
        .code,
        "provider_switch_busy"
    );
    b.records
        .lock()
        .unwrap()
        .get_mut(&native_b.id)
        .unwrap()
        .thread
        .status = SessionStatus::Idle;
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a2",
        "Apply findings",
    )
    .await
    .unwrap();
    let context = a.injected.lock().unwrap()[0].1.clone();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Original requirement",
            "answer a1",
            "Review requirement",
            "answer b1"
        ]
    );
    let page = host
        .read_handoff_history(&handoff::ReadHistory {
            session: root.clone(),
            offset: 0,
            limit: 20,
            text_offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|entry| entry.message.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Original requirement",
            "answer a1",
            "Review requirement",
            "answer b1",
            "Apply findings",
            "answer a2"
        ]
    );
    drop(client);
    drop(host);
    let (a, b) = (a.restarted(), b.restarted());
    *a.open_failure.lock().unwrap() = Some(Failure::new("incomplete_history", "still unreadable"));
    *b.open_failure.lock().unwrap() = Some(Failure::new("incomplete_history", "still unreadable"));
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b2",
        "Verify findings",
    )
    .await
    .unwrap();
    let input = b.inputs.lock().unwrap()[1].clone();
    assert_ne!(input.thread_id, native_b);
    let op::Input::Text { text } = &input.input[0] else {
        panic!()
    };
    assert!(text.contains("Apply findings"));
    assert!(text.contains("Original requirement"));
    assert!(text.contains("Review requirement"));
}

#[tokio::test]
async fn incomplete_native_history_without_a_complete_host_import_blocks_handoff() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Only a recorded tail",
    )
    .await
    .unwrap();
    assert!(host.inner.transcripts.history(&root).unwrap().is_none());
    *a.open_failure.lock().unwrap() = Some(Failure::new(
        "incomplete_history",
        "transcript parent is unavailable",
    ));
    let error = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Continue",
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "incomplete_history");
    assert_eq!(b.next.load(Ordering::SeqCst), 0);
    assert!(b.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn round_trip_reuses_native_sessions_transfers_only_missing_history_and_reopens_after_restart()
 {
    let directory = tempfile::tempdir().unwrap();
    let mut a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let mut b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    Arc::get_mut(&mut a).unwrap().metadata_only_state = true;
    Arc::get_mut(&mut b).unwrap().metadata_only_state = true;
    let root = a
        .create(directory.path().to_str().unwrap(), Some("model"), None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Keep this original constraint",
    )
    .await
    .unwrap();
    let first = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Review the implementation",
    )
    .await
    .unwrap();
    let b_input = b.inputs.lock().unwrap()[0].clone();
    assert_eq!(
        a.released.lock().unwrap().as_slice(),
        std::slice::from_ref(&root.id)
    );
    let op::Input::Text { text } = &b_input.input[0] else {
        panic!()
    };
    assert!(text.contains("Keep this original constraint"));
    assert!(text.contains("answer a1"));
    let second = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a2",
        "Fix the review findings",
    )
    .await
    .unwrap();
    let injection = a.injected.lock().unwrap()[0].clone();
    assert_eq!(injection.0, root.id);
    assert_eq!(
        injection
            .1
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["Review the implementation", "answer b1"]
    );
    assert_eq!(a.next.load(Ordering::SeqCst), 1);
    let _: agent_protocol::models::Empty = call(
        &host,
        client.id(),
        Call::RenameSession(op::RenameSession {
            thread_id: root.clone(),
            name: "Shared logical title".into(),
        }),
    )
    .await
    .unwrap();
    let listed: agent_protocol::models::ThreadList = call(
        &host,
        client.id(),
        Call::ListSessions(op::ListSessions {
            query: agent_protocol::models::ListQuery {
                search_term: "Shared logical".into(),
                ..Default::default()
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(listed.data.len(), 1);
    assert_eq!(listed.data[0].id.as_ref(), Some(&root));
    let visible = history(&host, client.id(), &root).await;
    assert_eq!(visible.thread.id.as_ref(), Some(&root));
    assert_eq!(visible.thread.active_provider, Some(ProviderKind::Codex));
    let turns = visible.thread.turns.as_ref().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.provider.unwrap())
            .collect::<Vec<_>>(),
        [
            ProviderKind::Codex,
            ProviderKind::Claude,
            ProviderKind::Codex
        ]
    );
    let texts = turns
        .iter()
        .flat_map(|turn| handoff::Message::from_turn(turn))
        .map(|message| message.text)
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            "Keep this original constraint",
            "answer a1",
            "Review the implementation",
            "answer b1",
            "Fix the review findings",
            "answer a2"
        ]
    );
    assert_ne!(first.turn_id, second.turn_id);
    let detail: op::ItemResponse = call(
        &host,
        client.id(),
        Call::ReadItem(op::ReadItem {
            thread_id: root.clone(),
            turn_id: turns[1].id.clone(),
            item_id: turns[1].items.as_ref().unwrap()[0].id.clone(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        handoff::Message::from_turn(&Turn {
            items: Some(vec![Arc::new(detail.item)]),
            ..Default::default()
        })
        .next()
        .unwrap()
        .text,
        "Review the implementation"
    );
    let fork: ThreadResponse = call(
        &host,
        client.id(),
        Call::ForkSession(op::ForkSession::new(root.clone(), turns[2].id.clone())),
    )
    .await
    .unwrap();
    assert_eq!(fork.thread.turns.as_ref().unwrap().len(), 3);
    assert_ne!(fork.thread.id, Some(root.clone()));
    drop(client);
    drop(host);
    let a = a.restarted();
    let b = b.restarted();
    let restarted = service(directory.path(), &a, &b);
    let client = restarted.open_session();
    assert_eq!(
        history(&restarted, client.id(), &root)
            .await
            .thread
            .turns
            .as_ref()
            .unwrap()
            .len(),
        3
    );
    send(
        &restarted,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b2",
        "Check the fix",
    )
    .await
    .unwrap();
    assert_eq!(b.next.load(Ordering::SeqCst), 1);
    let inputs = b.inputs.lock().unwrap();
    let op::Input::Text { text } = &inputs[1].input[0] else {
        panic!()
    };
    assert!(text.contains("Fix the review findings"));
    assert!(!text.contains("Keep this original constraint"));
}

#[tokio::test]
async fn replayed_handoff_receipts_do_not_block_later_provider_switches() {
    use agent_protocol::session::SubmissionDelivery;

    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let root = b
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Original",
    )
    .await
    .unwrap();
    let receipt = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Continue",
    )
    .await
    .unwrap();
    // A retry replays the persisted acknowledgement without another native
    // completion event. Its public turn ID still identifies finished history.
    let replayed = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Continue",
    )
    .await
    .unwrap();
    assert_eq!(receipt.turn_id, replayed.turn_id);
    assert_eq!(a.inputs.lock().unwrap().len(), 1);
    let opened = history(&host, client.id(), &root).await;
    assert!(opened.thread.active_turn_id().is_none());
    assert!(opened.thread.submissions.contains_key("a1"));
    let row = host.inner.conversations.get(&root).unwrap().unwrap();
    host.inner
        .router
        .begin_submission(&root, "pending")
        .unwrap();
    for delivery in [
        SubmissionDelivery::Sending,
        SubmissionDelivery::Unknown,
        SubmissionDelivery::Accepted { turn_id: None },
        SubmissionDelivery::Accepted {
            turn_id: Some(row.identity(&row.active, "unconfirmed").into()),
        },
        SubmissionDelivery::Accepted {
            turn_id: Some("bex:invalid".into()),
        },
    ] {
        host.inner
            .router
            .finish_submission(&root, "pending", delivery);
        assert_eq!(
            send(
                &host,
                client.id(),
                &root,
                ProviderKind::Claude,
                "b2",
                "Review"
            )
            .await
            .unwrap_err()
            .code,
            "provider_switch_busy"
        );
    }
    host.inner
        .router
        .finish_submission(&root, "pending", SubmissionDelivery::Rejected);
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b2",
        "Review",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a2",
        "Finish",
    )
    .await
    .unwrap();
    assert_eq!(a.inputs.lock().unwrap().len(), 2);
    assert_eq!(b.inputs.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn large_saved_history_and_new_requests_reach_the_same_native_session_for_compaction() {
    for provider in [ProviderKind::Codex, ProviderKind::Claude] {
        for (history_text, request_text) in [
            ("history".repeat(25_000), "Continue".into()),
            ("history".into(), "request".repeat(25_000)),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let a = NativeAgent::new(provider, directory.path().join("a"));
            let other = if provider == ProviderKind::Codex {
                ProviderKind::Claude
            } else {
                ProviderKind::Codex
            };
            let b = NativeAgent::new(other, directory.path().join("b"));
            let root = a
                .create(directory.path().to_str().unwrap(), Some("model"), None)
                .await
                .unwrap()
                .thread
                .id
                .unwrap();
            let host = service(directory.path(), &a, &b);
            let client = host.open_session();
            send(&host, client.id(), &root, provider, "first", &history_text)
                .await
                .unwrap();
            send(&host, client.id(), &root, other, "switch", &request_text)
                .await
                .unwrap();
            send(&host, client.id(), &root, provider, "return", &request_text)
                .await
                .unwrap();

            assert_eq!(a.next.load(Ordering::SeqCst), 1);
            assert_eq!(b.next.load(Ordering::SeqCst), 1);
            for agent in [&a, &b] {
                let inputs = agent.inputs.lock().unwrap();
                let last = inputs.last().unwrap();
                assert_eq!(
                    last.input.last(),
                    Some(&op::Input::Text {
                        text: request_text.clone()
                    })
                );
            }
            let visible = history(&host, client.id(), &root).await;
            assert_eq!(visible.thread.active_provider, Some(provider));
            assert_eq!(visible.thread.turns.as_ref().unwrap().len(), 3);
            let original = handoff::Message::from_turn(&visible.thread.turns.as_ref().unwrap()[0])
                .next()
                .unwrap();
            assert_eq!(original.text, history_text);
        }
    }
}

#[tokio::test]
async fn refused_inline_handoff_is_retryable_and_unknown_turn_delivery_is_not_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("a"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("b"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "original",
        "Original constraint",
    )
    .await
    .unwrap();
    b.fail_submit.store(1, Ordering::SeqCst);
    assert_eq!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Claude,
            "refused",
            "Review"
        )
        .await
        .unwrap_err()
        .delivery,
        agent_protocol::error::Delivery::NotSent
    );
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "extra",
        "A new source requirement",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "retry",
        "Review",
    )
    .await
    .unwrap();
    {
        let inputs = b.inputs.lock().unwrap();
        let op::Input::Text { text } = &inputs[0].input[0] else {
            panic!()
        };
        assert!(text.contains("Original constraint"));
        assert!(text.contains("A new source requirement"));
    }
    a.fail_submit.store(2, Ordering::SeqCst);
    assert_eq!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Codex,
            "unknown",
            "May already have run"
        )
        .await
        .unwrap_err()
        .delivery,
        agent_protocol::error::Delivery::Unknown
    );
    assert_eq!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Codex,
            "unknown",
            "May already have run"
        )
        .await
        .unwrap_err()
        .delivery,
        agent_protocol::error::Delivery::Unknown
    );
    assert_eq!(a.inputs.lock().unwrap().len(), 2);
    drop(client);
    drop(host);
    let restarted = service(directory.path(), &a.restarted(), &b.restarted());
    let client = restarted.open_session();
    assert_eq!(
        send(
            &restarted,
            client.id(),
            &root,
            ProviderKind::Codex,
            "unknown",
            "May already have run"
        )
        .await
        .unwrap_err()
        .delivery,
        agent_protocol::error::Delivery::Unknown
    );
}

#[tokio::test]
async fn uncertain_native_injection_uses_a_new_binding_without_replaying_the_request() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("a"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("b"));
    let root = b
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Original",
    )
    .await
    .unwrap();
    a.fail_inject.store(1, Ordering::SeqCst);
    assert!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Codex,
            "a1",
            "Continue"
        )
        .await
        .is_err()
    );
    assert!(a.inputs.lock().unwrap().is_empty());
    let receipt = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Continue",
    )
    .await
    .unwrap();
    let duplicate = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Continue",
    )
    .await
    .unwrap();
    assert_eq!(receipt.turn_id, duplicate.turn_id);
    assert_eq!(a.next.load(Ordering::SeqCst), 2);
    assert_eq!(a.inputs.lock().unwrap().len(), 1);
    let injections = a.injected.lock().unwrap();
    assert_ne!(injections[0].0, injections[1].0);
    assert_eq!(injections[1].1.messages[0].text, "Original");
}

#[tokio::test]
async fn a_running_source_blocks_switching_and_a_failed_resume_gets_full_context() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("a"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("b"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    a.records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .status = SessionStatus::Running;
    assert_eq!(
        send(
            &host,
            client.id(),
            &root,
            ProviderKind::Claude,
            "blocked",
            "Review"
        )
        .await
        .unwrap_err()
        .code,
        "provider_switch_busy"
    );
    assert_eq!(b.next.load(Ordering::SeqCst), 0);
    a.records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .status = SessionStatus::Idle;
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a1",
        "Original constraint",
    )
    .await
    .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Review",
    )
    .await
    .unwrap();
    *a.state_error.lock().unwrap() = Some(Failure::new(
        "native_resume_failed",
        "native session cannot be loaded",
    ));
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "a2",
        "Continue",
    )
    .await
    .unwrap();
    assert_eq!(a.next.load(Ordering::SeqCst), 2);
    {
        let injections = a.injected.lock().unwrap();
        assert_ne!(injections[0].0, root.id);
        assert_eq!(
            injections[0]
                .1
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["Original constraint", "answer a1", "Review", "answer b1"]
        );
    }
    let visible = history(&host, client.id(), &root).await;
    let ids = visible
        .thread
        .turns
        .as_ref()
        .unwrap()
        .iter()
        .map(|turn| &turn.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        ids.len(),
        3,
        "native turn-0 occurs in three sessions and must remain distinct"
    );
}

#[tokio::test]
async fn claude_history_beyond_one_hundred_turns_opens_and_pages_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let native_home = directory.path().join("native");
    let project = native_home.join("projects/workspace");
    std::fs::create_dir_all(&project).unwrap();
    let root = SessionRef {
        provider: ProviderKind::Claude,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let mut rows = Vec::new();
    for index in 0usize..201 {
        rows.push(
            serde_json::json!({
                "type": "user", "sessionId": root.id,
                "cwd": directory.path(), "uuid": format!("user-{index}"),
                "parentUuid": index.checked_sub(1).map(|previous| format!("answer-{previous}")),
                "message": {"content": format!("request {index}")},
            })
            .to_string(),
        );
        rows.push(
            serde_json::json!({
                "type": "assistant", "sessionId": root.id,
                "uuid": format!("answer-{index}"), "parentUuid": format!("user-{index}"),
                "message": {"id": format!("reply-{index}"), "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": format!("response {index}")}]},
            })
            .to_string(),
        );
    }
    let transcript = project.join(format!("{}.jsonl", root.id));
    std::fs::write(&transcript, rows.join("\n") + "\n").unwrap();
    let claude = crate::providers::Claude::load(
        directory.path().join("unused-cli"),
        directory.path().join("adapter"),
        Some(native_home),
    )
    .await
    .unwrap();
    let host = HostRpcService::new(
        [Backend {
            provider: ProviderKind::Claude,
            agent: Ok(Arc::new(claude)),
            dictation: None,
        }],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let complete = host.native_history(&root, HistoryUse::Read).await.unwrap();
    assert_eq!(complete.thread.turns.unwrap().len(), 201);
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 5,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    let mut thread = opened.response.thread;
    assert_eq!(thread.turns.as_ref().unwrap().len(), 5);
    assert_eq!(thread.turns.as_ref().unwrap()[0].id.as_str(), "user-196");
    // Appending a turn must not move the boundary of an already issued cursor.
    rows.push(
        serde_json::json!({
            "type": "user", "sessionId": root.id, "uuid": "user-201",
            "parentUuid": "answer-200", "message": {"content": "new request"},
        })
        .to_string(),
    );
    std::fs::write(&transcript, rows.join("\n") + "\n").unwrap();
    while let Some(cursor) = thread.history_cursor.clone() {
        let page: HistoryPage = call(
            &host,
            client.id(),
            Call::ReadHistory(agent_protocol::session::ReadHistory {
                session: root.clone(),
                cursor,
                include_activity: true,
            }),
        )
        .await
        .unwrap();
        page.prepend_to(&mut thread);
    }
    assert_eq!(
        thread
            .turns
            .unwrap()
            .iter()
            .map(|turn| turn.id.to_string())
            .collect::<Vec<_>>(),
        (0..201)
            .map(|index| format!("user-{index}"))
            .collect::<Vec<_>>()
    );
    for cursor in ["", "invalid", "0", "203"] {
        assert!(
            call::<HistoryPage>(
                &host,
                client.id(),
                Call::ReadHistory(agent_protocol::session::ReadHistory {
                    session: root.clone(),
                    cursor: cursor.into(),
                    include_activity: true,
                }),
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn long_history_is_transferred_beyond_the_visible_page_and_older_pages_keep_order() {
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("a"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("b"));
    let root = a
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let mut turns = Vec::new();
    for index in 0..150 {
        turns.push(Arc::new(Turn {
            id: format!("old-{index}").into(),
            status: TurnStatus::Completed,
            items: Some(vec![Arc::new(Item::new(
                format!("user-{index}").into(),
                ItemStatus::Completed,
                ItemBody::UserMessage {
                    text: Some(if index == 0 {
                        "Original requirement outside the first native page".into()
                    } else {
                        format!("request {index}")
                    }),
                    content: vec![],
                },
            ))]),
            ..Default::default()
        }));
    }
    a.records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .turns = Some(turns);
    let host = service(directory.path(), &a, &b);
    let client = host.open_session();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "b1",
        "Continue",
    )
    .await
    .unwrap();
    {
        let inputs = b.inputs.lock().unwrap();
        let op::Input::Text { text } = &inputs[0].input[0] else {
            panic!()
        };
        assert!(text.contains("Original requirement outside the first native page"));
        assert!(text.contains("request 149"));
    }
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 5,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(opened.response.thread.turns.as_ref().unwrap().len(), 5);
    let mut count = 5;
    let mut cursor = opened.response.thread.history_cursor;
    while let Some(value) = cursor {
        let page: HistoryPage = call(
            &host,
            client.id(),
            Call::ReadHistory(agent_protocol::session::ReadHistory {
                session: root.clone(),
                cursor: value,
                include_activity: true,
            }),
        )
        .await
        .unwrap();
        count += page.turns.len();
        cursor = page.next_cursor;
    }
    assert_eq!(count, 151);
    let page: handoff::Page = call(
        &host,
        client.id(),
        Call::ReadHandoffHistory(handoff::ReadHistory {
            session: root,
            offset: 0,
            limit: 1,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        page.items[0].message.text,
        "Original requirement outside the first native page"
    );
    assert_eq!(page.next_offset, Some(1));
}

#[test]
fn conversation_pages_distinguish_continuation_from_completion() {
    use agent_protocol::session::HistoryReadKind;
    let response = ThreadResponse {
        thread: Thread {
            turns: Some(
                (0..8)
                    .map(|id| {
                        Arc::new(Turn {
                            id: id.to_string().into(),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    for (end, ids, cursor, kind) in [
        (
            None,
            vec!["3", "4", "5", "6", "7"],
            Some("3"),
            HistoryReadKind::Partial,
        ),
        (
            Some(3),
            vec!["0", "1", "2"],
            None,
            HistoryReadKind::Complete,
        ),
        (Some(0), vec![], None, HistoryReadKind::Complete),
    ] {
        let page = HostRpcService::conversation_page(response.clone(), end, 5).unwrap();
        let turns = page.thread.turns.unwrap();
        assert_eq!(
            turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(page.thread.history_cursor.as_deref(), cursor);
        assert_eq!(page.thread.history_has_more, Some(cursor.is_some()));
        let state = page.thread.history_read_state.unwrap();
        assert_eq!(state.kind, kind);
        assert!(state.issues.is_empty());
    }
}

#[tokio::test]
async fn handoff_and_excerpts_recover_deferred_native_messages() {
    let directory = tempfile::tempdir().unwrap();
    let mut codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    Arc::get_mut(&mut codex).unwrap().deferred_history = true;
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let host = service(directory.path(), &codex, &claude);
    let client = host.open_session();
    let created: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::CreateSession(op::CreateSession {
            initial_input: None,
            provider: ProviderKind::Codex,
            cwd: Some(directory.path().to_str().unwrap().into()),
            model: None,
        }),
    )
    .await
    .unwrap();
    let root = created.session;
    let text = "Keep this historical constraint. ".repeat(40);
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "original",
        &text,
    )
    .await
    .unwrap();
    // Native history defers the message; the Host transcript recovers it.
    assert!(
        codex
            .open(&root.id, 100, true)
            .await
            .unwrap()
            .thread
            .turns
            .unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0]
            .is_deferred()
    );
    let opened: agent_protocol::session::OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(agent_protocol::session::OpenSession {
            session: root.clone(),
            limit: 100,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !opened.response.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0]
            .is_deferred()
    );
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "review",
        "Review",
    )
    .await
    .unwrap();
    assert!(claude.inputs.lock().unwrap()[0].input.iter().any(|input| {
        matches!(input, op::Input::Text { text: input } if input.contains(&text))
    }));
    let page: handoff::Page = call(
        &host,
        client.id(),
        Call::ReadHandoffHistory(handoff::ReadHistory {
            session: root,
            offset: 0,
            limit: 20,
            text_offset: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.items[0].message.text, text);
}

/// Imports run in the background; wait until the stored turns are `expected`.
async fn imported_turns(host: &HostRpcService, root: &SessionRef, expected: &[&str]) {
    let stored = || {
        host.inner
            .transcripts
            .window(root, None, 1000)
            .unwrap()
            .map(|window| {
                window
                    .response
                    .thread
                    .turns
                    .unwrap_or_default()
                    .iter()
                    .map(|turn| turn.id.to_string())
                    .collect::<Vec<_>>()
            })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while stored().is_none_or(|turns| turns != expected) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("transcript {:?} never became {expected:?}", stored()));
}

#[tokio::test]
async fn transcripts_replace_native_reads_until_the_project_is_imported_again() {
    let directory = tempfile::tempdir().unwrap();
    let directory = dunce::canonicalize(directory.path()).unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let root = codex
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "first",
        "hello",
    )
    .await
    .unwrap();
    history(&host, client.id(), &root).await;
    imported_turns(&host, &root, &["turn-0"]).await;

    // Native history continued outside the Host waits for a re-import, while
    // turns run through the Host are recorded as they finish.
    codex
        .records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .turns
        .as_mut()
        .unwrap()
        .push(Arc::new(Turn {
            id: "outside".into(),
            status: TurnStatus::Completed,
            items: Some(vec![]),
            ..Default::default()
        }));
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "second",
        "again",
    )
    .await
    .unwrap();
    *codex.open_error.lock().unwrap() = Some(Failure::new(
        "unreadable",
        "native history must not be read",
    ));
    let opened = history(&host, client.id(), &root).await;
    let ids: Vec<_> = opened
        .thread
        .turns
        .unwrap()
        .iter()
        .map(|turn| turn.id.clone())
        .collect();
    assert_eq!(ids, [TurnId::from("turn-0"), "turn-2".into()]);
    assert!(codex.open_error.lock().unwrap().take().is_some());

    call::<agent_protocol::project_import::ProjectImportResult>(
        &host,
        client.id(),
        Call::ImportProjects(agent_protocol::project_import::ImportProjects {
            paths: vec![directory.to_str().unwrap().into()],
        }),
    )
    .await
    .unwrap();
    imported_turns(&host, &root, &["turn-0", "outside", "turn-2"]).await;
}

#[tokio::test]
async fn unreadable_native_bodies_stay_deferred_without_blocking_their_import() {
    let directory = tempfile::tempdir().unwrap();
    let directory = dunce::canonicalize(directory.path()).unwrap();
    let mut codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    Arc::get_mut(&mut codex).unwrap().deferred_history = true;
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let root = codex
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    // Native history the Host never ran; its unreadable answer stays deferred.
    let item = |id: &str, body| Arc::new(Item::new(id.into(), ItemStatus::Completed, body));
    codex
        .records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .turns = Some(vec![Arc::new(Turn {
        id: "turn-0".into(),
        status: TurnStatus::Completed,
        items: Some(vec![
            item(
                "user-0",
                ItemBody::UserMessage {
                    text: Some("hello".into()),
                    content: vec![],
                },
            ),
            item(
                "answer-0",
                ItemBody::AssistantText {
                    text: "answer".into(),
                    phase: AssistantPhase::Final,
                },
            ),
        ]),
        ..Default::default()
    })]);
    codex.unreadable.lock().unwrap().insert("answer-0".into());
    history(&host, client.id(), &root).await;
    imported_turns(&host, &root, &["turn-0"]).await;
    let stored = host
        .inner
        .transcripts
        .turn(&root, &"turn-0".into())
        .unwrap()
        .unwrap()
        .items
        .unwrap();
    let deferred = |id: &str| {
        stored
            .iter()
            .find(|item| item.id.as_str() == id)
            .unwrap()
            .is_deferred()
    };
    assert!(!deferred("user-0"));
    assert!(deferred("answer-0"));
    // Reading the unreadable body reports the provider's failure, as before.
    let failure = call::<op::ItemResponse>(
        &host,
        client.id(),
        Call::ReadItem(op::ReadItem {
            thread_id: root.clone(),
            turn_id: "turn-0".into(),
            item_id: "answer-0".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "unreadable");
    let error = send(
        &host,
        client.id(),
        &root,
        ProviderKind::Claude,
        "handoff",
        "Continue",
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "incomplete_history");
    assert!(claude.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recorded_items_with_reused_turn_ids_do_not_need_the_native_provider() {
    let directory = tempfile::tempdir().unwrap();
    let directory = dunce::canonicalize(directory.path()).unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let target = SessionRef::new(ProviderKind::Codex, "recorded".into()).unwrap();
    let turns = ["older", "latest"].map(|id| {
        Arc::new(Turn {
            id: "repeat".into(),
            status: TurnStatus::Completed,
            items: Some(vec![Arc::new(Item::new(
                id.into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: id.into(),
                    phase: AssistantPhase::Final,
                },
            ))]),
            ..Default::default()
        })
    });
    host.inner
        .transcripts
        .import(
            &target,
            host.inner.transcripts.version(&target).unwrap(),
            ThreadResponse {
                thread: Thread {
                    id: Some(target.clone()),
                    turns: Some(turns.into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    for item_id in ["older", "latest"] {
        let output = call::<op::ItemResponse>(
            &host,
            client.id(),
            Call::ReadItem(op::ReadItem {
                thread_id: target.clone(),
                turn_id: "repeat".into(),
                item_id: item_id.into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(output.item.id.as_str(), item_id);
        assert!(
            matches!(output.item.body(), ItemBody::AssistantText { text, .. } if text == item_id)
        );
    }
    assert!(codex.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn imports_copy_message_text_while_host_turns_keep_activity() {
    let directory = tempfile::tempdir().unwrap();
    let directory = dunce::canonicalize(directory.path()).unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let root = codex
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let turn = |id: &str| Turn {
        id: id.into(),
        status: TurnStatus::Completed,
        items: Some(
            [
                ItemBody::UserMessage {
                    text: Some(format!("{id} request")),
                    content: vec![],
                },
                ItemBody::Reasoning {
                    content: vec![format!("{id} activity")],
                    summary: vec![],
                },
                ItemBody::AssistantText {
                    text: format!("{id} answer"),
                    phase: AssistantPhase::Final,
                },
            ]
            .into_iter()
            .enumerate()
            .map(|(index, body)| {
                Arc::new(Item::new(
                    format!("{id}-{index}").into(),
                    ItemStatus::Completed,
                    body,
                ))
            })
            .collect(),
        ),
        ..Default::default()
    };
    codex
        .records
        .lock()
        .unwrap()
        .get_mut(&root.id)
        .unwrap()
        .thread
        .turns = Some(vec![Arc::new(turn("past"))]);
    emit(
        &codex.events,
        AgentChange::Session {
            session: root.clone(),
            change: SessionChange::Turn {
                turn: turn("live"),
                completed: true,
            },
        },
    )
    .await
    .unwrap();
    history(&host, client.id(), &root).await;
    imported_turns(&host, &root, &["past", "live"]).await;
    let items = |id: &str| {
        host.inner
            .transcripts
            .turn(&root, &id.into())
            .unwrap()
            .unwrap()
            .items
            .unwrap()
            .iter()
            .map(|item| item.id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(items("past"), ["past-0", "past-2"]);
    assert_eq!(items("live"), ["live-0", "live-1", "live-2"]);
}

#[tokio::test]
async fn a_client_holding_the_current_transcript_receives_no_turns() {
    let directory = tempfile::tempdir().unwrap();
    let directory = dunce::canonicalize(directory.path()).unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.join("claude"));
    let host = service(&directory, &codex, &claude);
    let client = host.open_session();
    let root = codex
        .create(directory.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "first",
        "hello",
    )
    .await
    .unwrap();
    history(&host, client.id(), &root).await;
    imported_turns(&host, &root, &["turn-0"]).await;
    let open = |known_version| {
        call::<agent_protocol::session::OpenedSession>(
            &host,
            client.id(),
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: root.clone(),
                limit: 10,
                include_activity: true,
                known_version,
            }),
        )
    };
    let current = open(None).await.unwrap();
    let version = current.response.thread.transcript_version;
    assert!(version.is_some() && !current.unchanged);
    let held = open(version).await.unwrap();
    assert!(held.unchanged);
    assert!(held.response.thread.turns.is_none());
    assert_eq!(held.response.thread.name, current.response.thread.name);

    send(
        &host,
        client.id(),
        &root,
        ProviderKind::Codex,
        "second",
        "again",
    )
    .await
    .unwrap();
    let changed = open(version).await.unwrap();
    assert!(!changed.unchanged);
    assert!(changed.response.thread.transcript_version > version);
    assert_eq!(changed.response.thread.turns.unwrap().len(), 2);
}
