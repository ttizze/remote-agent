use super::*;
use crate::host_rpc::routing::SessionId;
use crate::providers::Discovery as _;
use crate::providers::{
    AccountCommand, AccountReply, AgentChange, AgentEvent, Identity, SessionPage, SubmissionState,
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
    models::{Thread, ThreadResponse},
    session::HistoryPage,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;

pub(in crate::host_rpc) fn bound_execution(
    service: &HostRpcService,
    target: &SessionRef,
) -> (SessionRef, Uuid) {
    let state = service
        .inner
        .conversation
        .storage
        .thread_state(&target.id, &[])
        .unwrap();
    let attempt = state
        .run_attempts
        .values()
        .max_by_key(|attempt| (state.runs[&attempt.run_id].ordinal, attempt.attempt_ordinal))
        .unwrap();
    let thread = &state.provider_threads[&attempt.provider_thread_id];
    let native = thread
        .native_thread_ref
        .as_ref()
        .and_then(|reference| reference.native_id.clone())
        .unwrap();
    (
        SessionRef::new(
            ProviderKind::from_instance_id(&attempt.provider_instance_id).unwrap(),
            native,
        )
        .unwrap(),
        Uuid::parse_str(&attempt.id).unwrap(),
    )
}

pub(in crate::host_rpc) fn canonical_snapshot(
    service: &HostRpcService,
    target: &SessionRef,
) -> Result<agent_protocol::orchestration::projection::ThreadSnapshot, String> {
    use agent_protocol::orchestration::projection::ThreadStreamItem;
    let storage = &service.inner.conversation.storage;
    let mut snapshot = storage
        .thread_stream(target, None)?
        .into_iter()
        .find_map(|item| match item {
            ThreadStreamItem::Snapshot(snapshot) => Some(snapshot),
            _ => None,
        })
        .ok_or("thread snapshot is missing")?;
    let mut cursors = std::collections::BTreeSet::new();
    while let Some(cursor) = snapshot.history_cursor.clone() {
        if !cursors.insert(cursor.clone()) {
            return Err("history cursor repeated".into());
        }
        let page = storage.thread_history(target, &cursor)?;
        snapshot = agent_core::orchestration::receive_history(&snapshot, &cursor, page)
            .ok_or("history cursor changed")?;
    }
    Ok(snapshot)
}

/// Inspect the same canonical snapshots, history pages and full-item queries as
/// clients. This intentionally has no fallback to the replaced row storage.
pub(in crate::host_rpc) fn history(
    service: &HostRpcService,
    target: &SessionRef,
) -> Result<ThreadResponse, String> {
    let storage = &service.inner.conversation.storage;
    let snapshot = canonical_snapshot(service, target)?;
    let provider =
        ProviderKind::from_instance_id(&snapshot.projection.thread.provider_instance_id)?;
    let caps = service
        .inner
        .providers
        .get(provider)
        .map(|agent| agent.capabilities())
        .unwrap_or_default();
    let mut details = std::collections::BTreeMap::new();
    for row in &snapshot.projection.visible_turn_items {
        let source = SessionRef::new(None, row.source_thread_id.clone()).map_err(str::to_owned)?;
        if let Some(item) = storage.thread_item(&source, &row.source_item_id)? {
            details.insert(
                (row.source_thread_id.clone(), row.source_item_id.clone()),
                Arc::new(item),
            );
        }
    }
    agent_core::orchestration::project(
        &snapshot,
        target,
        provider,
        records::capabilities(caps),
        storage.thread_project_root(target)?.as_deref(),
        &details,
    )
}

#[derive(Clone)]
pub(in crate::host_rpc) struct Transcript {
    pub snapshot: agent_protocol::orchestration::projection::ThreadSnapshot,
    session: SessionRef,
    provider: ProviderKind,
    capabilities: Capabilities,
    project_root: Option<String>,
}
impl Transcript {
    pub(in crate::host_rpc) fn opened(opened: records::Opened) -> Self {
        let mut snapshot = None;
        for item in opened.initial {
            snapshot = agent_core::orchestration::receive(
                &opened.session.application_id(),
                snapshot.as_ref(),
                item,
            )
            .unwrap();
        }
        Self {
            snapshot: snapshot.expect("opening includes a snapshot"),
            session: opened.session,
            provider: opened.provider,
            capabilities: opened.capabilities,
            project_root: opened.project_root,
        }
    }
    pub(in crate::host_rpc) fn apply(
        mut self,
        item: agent_protocol::orchestration::projection::ThreadStreamItem,
    ) -> Result<Self, String> {
        self.snapshot = agent_core::orchestration::receive(
            &self.session.application_id(),
            Some(&self.snapshot),
            item,
        )?
        .ok_or("snapshot disappeared")?;
        Ok(self)
    }
    pub(in crate::host_rpc) fn project(&self) -> Result<ThreadResponse, String> {
        agent_core::orchestration::project(
            &self.snapshot,
            &self.session,
            self.provider,
            records::capabilities(self.capabilities),
            self.project_root.as_deref(),
            &Default::default(),
        )
    }
}

type HistoryInjection = (String, Vec<Value>);

/// Independent native sessions, including persisted history and provider
/// acknowledgements. Assertions observe the Host boundary, not a reimplementation
/// of its handoff/routing decisions.
pub(in crate::host_rpc) struct NativeAgent {
    pub(in crate::host_rpc) generated_title: Mutex<Option<String>>,
    pub(in crate::host_rpc) title_prompts: Mutex<Vec<String>>,
    pub(in crate::host_rpc) title_started: tokio::sync::Notify,
    pub(in crate::host_rpc) title_gate: tokio::sync::Semaphore,
    provider: ProviderKind,
    pub(in crate::host_rpc) discovery_reads: Arc<AtomicUsize>,
    pub(in crate::host_rpc) body_reads: Arc<AtomicUsize>,
    pub(in crate::host_rpc) history_activity: Mutex<Vec<bool>>,
    pub(in crate::host_rpc) records: Arc<Mutex<HashMap<String, ThreadResponse>>>,
    pub(in crate::host_rpc) idle_warning: Option<&'static str>,
    pub(in crate::host_rpc) deferred_history: bool,
    pub(in crate::host_rpc) metadata_only_state: bool,
    pub(in crate::host_rpc) needs_reload: bool,
    /// Item IDs whose native bodies cannot be read.
    pub(in crate::host_rpc) unreadable: Mutex<std::collections::HashSet<String>>,
    pub(in crate::host_rpc) inputs: Arc<Mutex<Vec<op::Submission>>>,
    pub(in crate::host_rpc) feedback_reports: Mutex<Vec<(String, Option<String>, bool)>>,
    pub(in crate::host_rpc) interrupts: Mutex<Vec<(String, TurnId)>>,
    pub(in crate::host_rpc) injected: Arc<Mutex<Vec<HistoryInjection>>>,
    pub(in crate::host_rpc) released: Arc<Mutex<Vec<String>>>,
    pub(in crate::host_rpc) unload_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) next: Arc<AtomicUsize>,
    pub(in crate::host_rpc) fail_submit: AtomicUsize,
    pub(in crate::host_rpc) submit_gate: Option<tokio::sync::Semaphore>,
    pub(in crate::host_rpc) submit_started: tokio::sync::Notify,
    pub(in crate::host_rpc) create_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) create_started: tokio::sync::Notify,
    pub(in crate::host_rpc) created_in: Mutex<Vec<bool>>,
    pub(in crate::host_rpc) create_response_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) created: tokio::sync::Notify,
    pub(in crate::host_rpc) fail_inject: AtomicUsize,
    pub(in crate::host_rpc) open_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) state_gate: tokio::sync::Semaphore,
    pub(in crate::host_rpc) state_error: Mutex<Option<Failure>>,
    pub(in crate::host_rpc) rollback_error: Mutex<Option<String>>,
    pub(in crate::host_rpc) rollback_calls: AtomicUsize,
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
    pub(in crate::host_rpc) fn new(provider: ProviderKind) -> Arc<Self> {
        let (events, receiver) = mpsc::channel(32);
        Arc::new(Self {
            generated_title: Default::default(),
            title_prompts: Default::default(),
            title_started: Default::default(),
            title_gate: tokio::sync::Semaphore::new(1),
            provider,
            discovery_reads: Arc::default(),
            body_reads: Default::default(),
            history_activity: Default::default(),
            records: Default::default(),
            idle_warning: None,
            deferred_history: false,
            metadata_only_state: false,
            needs_reload: false,
            unreadable: Default::default(),
            inputs: Default::default(),
            feedback_reports: Default::default(),
            interrupts: Default::default(),
            injected: Default::default(),
            released: Default::default(),
            unload_gate: tokio::sync::Semaphore::new(1),
            next: Default::default(),
            fail_submit: AtomicUsize::new(0),
            submit_gate: None,
            submit_started: tokio::sync::Notify::new(),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            created_in: Mutex::default(),
            create_response_gate: tokio::sync::Semaphore::new(1),
            created: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
            open_gate: tokio::sync::Semaphore::new(1),
            state_gate: tokio::sync::Semaphore::new(1),
            state_error: Mutex::new(None),
            rollback_error: Mutex::new(None),
            rollback_calls: AtomicUsize::new(0),
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
            generated_title: Mutex::new(self.generated_title.lock().unwrap().clone()),
            title_prompts: Default::default(),
            title_started: Default::default(),
            title_gate: tokio::sync::Semaphore::new(1),
            provider: self.provider,
            discovery_reads: self.discovery_reads.clone(),
            body_reads: self.body_reads.clone(),
            history_activity: Default::default(),
            records: self.records.clone(),
            idle_warning: self.idle_warning,
            deferred_history: self.deferred_history,
            metadata_only_state: self.metadata_only_state,
            needs_reload: self.needs_reload,
            unreadable: Default::default(),
            inputs: self.inputs.clone(),
            feedback_reports: Mutex::new(self.feedback_reports.lock().unwrap().clone()),
            interrupts: Default::default(),
            injected: self.injected.clone(),
            released: self.released.clone(),
            unload_gate: tokio::sync::Semaphore::new(1),
            next: self.next.clone(),
            fail_submit: AtomicUsize::new(0),
            submit_gate: None,
            submit_started: tokio::sync::Notify::new(),
            create_gate: tokio::sync::Semaphore::new(1),
            create_started: tokio::sync::Notify::new(),
            created_in: Mutex::default(),
            create_response_gate: tokio::sync::Semaphore::new(1),
            created: tokio::sync::Notify::new(),
            fail_inject: AtomicUsize::new(0),
            open_gate: tokio::sync::Semaphore::new(1),
            state_gate: tokio::sync::Semaphore::new(1),
            state_error: Mutex::new(None),
            rollback_error: Mutex::new(None),
            rollback_calls: AtomicUsize::new(0),
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
    async fn generate_title(&self, prompt: &str) -> Result<Option<String>, Failure> {
        let title = self.generated_title.lock().unwrap().clone();
        if title.is_some() {
            self.title_prompts.lock().unwrap().push(prompt.into());
            self.title_started.notify_one();
            let _permit = self.title_gate.acquire().await.unwrap();
        }
        Ok(title)
    }
    fn data_recipient(&self) -> &'static str {
        "fixture"
    }
    async fn models(&self, _: &op::ListModels) -> Result<op::ModelPage, Failure> {
        Ok(op::ModelPage {
            data: vec![agent_protocol::models::Model {
                id: "fixture-model".into(),
                model: ModelRef {
                    provider: self.provider,
                    id: "fixture-model".into(),
                },
                display_name: "Fixture model".into(),
                credential_route: None,
                settings: None,
                settings_discovery: false,
                is_default: Some(true),
            }],
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
    async fn catalog_page(
        &self,
        cursor: Option<String>,
        scope: SessionListScope<'_>,
        limit: u32,
    ) -> Result<SessionPage, Failure> {
        self.discovery_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_list.swap(0, Ordering::SeqCst) != 0 {
            return Err(Failure::new(
                "fixture_list_failed",
                format!("{} list failed", self.provider.key()),
            ));
        }
        let mut page = SessionPage {
            data: self
                .records
                .lock()
                .unwrap()
                .values()
                .filter(|_| !matches!(scope, SessionListScope::Descendants(_)))
                .map(|record| {
                    let mut thread = record.thread.clone();
                    thread.turns = None;
                    thread
                })
                .collect(),
            next_cursor: None,
        };
        page.data
            .sort_by(|a, b| b.updated_at.partial_cmp(&a.updated_at).unwrap());
        let start = cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| Failure::new("invalid_cursor", "invalid fixture catalog cursor"))?;
        let end = start.saturating_add(limit as usize).min(page.data.len());
        page.next_cursor = (end < page.data.len()).then(|| end.to_string());
        page.data = page
            .data
            .into_iter()
            .skip(start)
            .take(limit as usize)
            .collect();
        Ok(page)
    }
    async fn open(
        &self,
        id: &str,
        limit: usize,
        include_activity: bool,
    ) -> Result<ThreadResponse, Failure> {
        self.history_activity.lock().unwrap().push(include_activity);
        self.discovery_reads.fetch_add(1, Ordering::SeqCst);
        let _permit = self.open_gate.acquire().await.unwrap();
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
    async fn read_history(
        &self,
        id: &str,
        cursor: &str,
        include_activity: bool,
    ) -> Result<HistoryPage, Failure> {
        self.history_activity.lock().unwrap().push(include_activity);
        self.discovery_reads.fetch_add(1, Ordering::SeqCst);
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
    async fn read_item(
        &self,
        params: &op::ReadItem,
    ) -> Result<agent_protocol::models::Item, Failure> {
        self.body_reads.fetch_add(1, Ordering::SeqCst);
        self.discovery_reads.fetch_add(1, Ordering::SeqCst);
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
        Ok((*item).clone())
    }
}

#[async_trait::async_trait]
impl Agent for NativeAgent {
    async fn upload_feedback(
        &self,
        id: &str,
        reason: Option<&str>,
        include_logs: bool,
    ) -> Result<agent_protocol::feedback::FeedbackReceipt, Failure> {
        self.feedback_reports.lock().unwrap().push((
            id.into(),
            reason.map(str::to_owned),
            include_logs,
        ));
        Ok(agent_protocol::feedback::FeedbackReceipt {
            feedback_id: format!("feedback:{id}"),
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            feedback: self.provider == ProviderKind::Codex,
            steer: true,
            model_change: true,
            rename: true,
            fork: self.provider == ProviderKind::Codex,
            rollback: self.provider == ProviderKind::Codex,
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
        let _permit = self.state_gate.acquire().await.unwrap();
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
    async fn inject_history(&self, id: &str, items: &[Value]) -> Result<bool, Failure> {
        if self.provider != ProviderKind::Codex {
            return Ok(false);
        }
        self.injected
            .lock()
            .unwrap()
            .push((id.into(), items.to_vec()));
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
        let _permit = self.unload_gate.acquire().await.unwrap();
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
        self.submit_started.notify_one();
        let _permit = match &self.submit_gate {
            Some(gate) => Some(gate.acquire().await.unwrap()),
            None => None,
        };
        let failure = self.fail_submit.swap(0, Ordering::SeqCst);
        match failure {
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
        let mut user = Item::new(
            format!("user-{execution}").into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: op::Input::message_parts(&input.input),
            },
        );
        user.client_input_id = Some(input.client_user_message_id.clone());
        let turn_id: TurnId = format!("turn-{execution}").into();
        let turn = Turn {
            id: turn_id.clone(),
            status: TurnStatus::Completed,
            items: Some(vec![
                Arc::new(user),
                Arc::new(Item::new(
                    format!("answer-{execution}").into(),
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
        if self.provider == ProviderKind::Claude {
            crate::providers::emit_for(
                &self.events,
                AgentChange::Head {
                    session: input.thread_id.clone(),
                    turn: turn_id.clone(),
                    head: turn_id.to_string(),
                },
                Some(execution),
            )
            .await
            .map_err(|error| Failure::new("event_failed", error))?;
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
        if failure == 3 {
            return Err(Failure::unknown(
                "submission_unknown",
                "native callback preceded a lost acknowledgement",
            ));
        }
        Ok(op::SubmissionReceipt {
            turn_id: Some(turn_id),
        })
    }
    async fn interrupt(&self, id: &str, turn: &TurnId) -> Result<Empty, Failure> {
        self.interrupts
            .lock()
            .unwrap()
            .push((id.into(), turn.clone()));
        Ok(Empty {})
    }
    async fn discard_workspace_processes(&self, _: &Path) -> Result<(), Failure> {
        Ok(())
    }
    async fn rollback(
        &self,
        id: &str,
        retained: Option<&str>,
        first_removed: Option<&str>,
        _cwd: &str,
        _browser: Option<Value>,
    ) -> Result<crate::providers::RollbackResult, Failure> {
        self.rollback_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.rollback_error.lock().unwrap().as_ref() {
            return Err(Failure::new("rollback_refused", error));
        }
        let mut records = self.records.lock().unwrap();
        let response = records.get_mut(id).ok_or_else(unsupported)?;
        if let Some(first_removed) = first_removed {
            let turns = response.thread.turns.as_mut().unwrap();
            if let Some(index) = turns
                .iter()
                .position(|turn| turn.id.as_str() == first_removed)
            {
                turns.truncate(index);
            } else if retained
                .is_some_and(|retained| !turns.iter().any(|turn| turn.id.as_str() == retained))
            {
                return Err(unsupported());
            }
        }
        // A confirmed SDK rollback returns a fresh snapshot. The artificial
        // read failure used before the operation no longer describes it.
        response.thread.history_read_state = None;
        Ok(crate::providers::RollbackResult {
            native_id: id.into(),
            head_id: retained.map(str::to_owned),
        })
    }
    async fn fork(
        &self,
        id: &str,
        turn: crate::providers::NativeForkPoint<'_>,
        _: Option<serde_json::Value>,
    ) -> Result<crate::providers::ForkResult, Failure> {
        let source = self.open(id, usize::MAX, true).await?;
        let mut response = self
            .create(source.thread.cwd.as_deref().unwrap(), None, None)
            .await?;
        let mut turns = source.thread.turns.unwrap();
        if let Some(turn) = turn {
            turns.truncate(
                turns
                    .iter()
                    .rposition(|candidate| candidate.id.as_str() == turn)
                    .unwrap()
                    + 1,
            );
        }
        response.thread.turns = Some(turns);
        self.records.lock().unwrap().insert(
            response.thread.id.as_ref().unwrap().id.clone(),
            response.clone(),
        );
        Ok(response.into())
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

pub(in crate::host_rpc) async fn own(
    host: &HostRpcService,
    public: &SessionRef,
    response: &ThreadResponse,
) {
    let native = response.thread.id.as_ref().unwrap();
    let (thread, native) =
        Runtime::native_header_records(host, public, native, response, &super::commands::at())
            .await
            .unwrap();
    host.inner
        .conversation
        .storage
        .seed_native_records(&thread, &native, Default::default())
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
