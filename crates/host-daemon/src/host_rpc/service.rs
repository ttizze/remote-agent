use agent_protocol::session::ProviderKind;
use std::sync::{Arc, OnceLock};

use agent_protocol::operations as op;

use agent_protocol::models::ListQuery;

use agent_protocol::models::Thread;

use agent_protocol::models::ThreadResponse;

use agent_protocol::protocol::{Body, Call, Response};
use agent_transport::peer::RpcMessageError;
use codex_app_server::CodexAppServer;
use serde::Serialize;

use super::agent::{Agent, Identity, session_pages};
use super::conversations::Conversations;
use futures_util::{StreamExt, TryStreamExt};
use std::collections::HashMap;

use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) delivery: agent_protocol::error::Delivery,
    pub(crate) execution: Option<Box<agent_protocol::execution::ExecutionError>>,
}
impl From<Failure> for agent_protocol::error::RpcFailure {
    fn from(error: Failure) -> Self {
        Self {
            code: error.code.into(),
            message: error.message,
            delivery: error.delivery,
            execution: error.execution,
        }
    }
}
impl From<RpcMessageError> for Failure {
    fn from(error: RpcMessageError) -> Self {
        Self::new("invalid_params", error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new("invalid_params", error)
    }
}

impl Failure {
    pub(crate) fn before_submission(mut self) -> Self {
        self.delivery = agent_protocol::error::Delivery::NotSent;
        self
    }
    pub(crate) fn unknown(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: format!("{error:#}"),
            delivery: agent_protocol::error::Delivery::Unknown,
            execution: None,
        }
    }
    pub(crate) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: format!("{error:#}"),
            delivery: agent_protocol::error::Delivery::NotSent,
            execution: None,
        }
    }
}

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    browser: OnceLock<Arc<crate::browser::Browser>>,
    agents: std::sync::RwLock<HashMap<ProviderKind, Arc<dyn Agent>>>,
    startup_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    codex: Arc<super::codex::Codex>,
    projects: ProjectStore,
    router: SessionRouter,
    conversations: Arc<Conversations>,
    initial_import: tokio::sync::OnceCell<()>,
    catalog_import: tokio::sync::Mutex<()>,
    event_pumps: std::sync::Mutex<std::collections::HashSet<ProviderKind>>,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
}

impl HostRpcService {
    pub fn new(
        codex: Result<Arc<CodexAppServer>, String>,
        projects: ProjectStore,
    ) -> anyhow::Result<Self> {
        let conversations = Arc::new(Conversations::open(
            &projects.path().with_file_name("bex-conversations.sqlite"),
        )?);
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let adapter = Arc::new(super::codex::Codex::new(codex.clone()));
        let agents = HashMap::from([(ProviderKind::Codex, adapter.clone() as Arc<dyn Agent>)]);
        Ok(Self {
            inner: Arc::new(ServiceInner {
                browser: OnceLock::new(),
                agents: std::sync::RwLock::new(agents),
                startup_errors: Default::default(),
                dictation: crate::dictation::Dictation::new(codex),
                codex: adapter,
                worktrees: crate::worktrees::Worktrees::new(projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                permission_settings_access: Default::default(),
                terminals: Default::default(),
                projects,
                router: SessionRouter::with_conversations(conversations.clone()),
                conversations,
                initial_import: Default::default(),
                catalog_import: Default::default(),
                event_pumps: Default::default(),
                files,
            }),
        })
    }

    fn storage_scope(&self, provider: ProviderKind) -> Result<String, Failure> {
        provider_storage_scope([(
            provider,
            canonical_storage_path(self.agent(provider)?.storage_directory()),
        )])
        .map_err(|error| Failure::new("provider_storage_unavailable", error))
    }

    fn native_session(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<agent_protocol::session::SessionRef, Failure> {
        self.inner
            .conversations
            .native(target, &self.storage_scope(target.provider)?)
            .map_err(|error| Failure::new("invalid_session", error))
    }

    fn normalize_item(&self, item: &mut agent_protocol::models::Item) -> Result<(), Failure> {
        if let agent_protocol::models::ItemBody::Subagent {
            sender,
            receivers,
            states,
            ..
        } = item.body_mut()
        {
            for native in sender
                .iter_mut()
                .chain(receivers.iter_mut())
                .chain(states.iter_mut().map(|state| &mut state.session))
            {
                *native = self
                    .inner
                    .conversations
                    .bind(native, &self.storage_scope(native.provider)?)
                    .map_err(|error| Failure::new("invalid_session", error))?;
            }
        }
        Ok(())
    }

    async fn hydrate_history(
        &self,
        agent: &dyn Agent,
        native: &agent_protocol::session::SessionRef,
        mut turns: Vec<Arc<agent_protocol::models::Turn>>,
    ) -> Result<Vec<Arc<agent_protocol::models::Turn>>, Failure> {
        for turn in &mut turns {
            let turn = Arc::make_mut(turn);
            if turn.items_summary || turn.items.is_none() {
                turn.items = Some(agent.read_turn_items(&native.id, &turn.id).await?);
                turn.items_summary = false;
            }
            for item in turn.items.iter_mut().flatten() {
                if item.is_deferred() {
                    let params = op::ReadItem {
                        thread_id: native.clone(),
                        turn_id: turn.id.clone(),
                        item_id: item.id.clone(),
                    };
                    *item = Arc::new(agent.read_item(&params).await?.item);
                }
                self.normalize_item(Arc::make_mut(item))?;
            }
        }
        Ok(turns)
    }

    async fn import_conversation(
        &self,
        target: &agent_protocol::session::SessionRef,
        max_pages: usize,
    ) -> Result<(), Failure> {
        let _lease = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let native = self.native_session(target)?;
        let agent = self.agent(target.provider)?;
        for _ in 0..max_pages {
            let _serial = self.inner.router.submission_lock(target).lock_owned().await;
            let (complete, started, cursor) = self
                .inner
                .conversations
                .import_state(target)
                .map_err(|error| Failure::new("history_import_failed", error))?;
            if complete {
                return Ok(());
            }
            if started && max_pages == 1 {
                return Ok(());
            }
            let (response, mut page) = if started {
                let cursor = cursor.ok_or_else(|| {
                    Failure::new("invalid_history", "unfinished import has no cursor")
                })?;
                (None, agent.read_history(&native.id, &cursor, true).await?)
            } else {
                // Claude's native reader has no page cursor; its existing bounded
                // file parser supplies the full timeline. Codex reads bounded pages.
                let mut response = agent.open(&native.id, 100, true).await?;
                if response
                    .thread
                    .history_read_state
                    .as_ref()
                    .is_some_and(|state| {
                        state.kind == agent_protocol::session::HistoryReadKind::Unavailable
                    })
                {
                    return Err(Failure::new(
                        "history_import_failed",
                        "provider history is unavailable",
                    ));
                }
                if response.thread.id.as_ref() != Some(&native) {
                    return Err(Failure::new(
                        "invalid_history",
                        "provider history identity changed",
                    ));
                }
                let page = agent_protocol::session::HistoryPage {
                    turns: response.thread.turns.take().unwrap_or_default(),
                    next_cursor: response.thread.history_cursor.take(),
                };
                (Some(response), page)
            };
            page.turns = self
                .hydrate_history(agent.as_ref(), &native, page.turns)
                .await?;
            self.inner
                .conversations
                .import_page(target, response.as_ref(), &page)
                .map_err(|error| Failure::new("history_import_failed", error))?;
        }
        Ok(())
    }

    async fn import_provider(
        &self,
        provider: ProviderKind,
        agent: &dyn Agent,
    ) -> (Vec<agent_protocol::session::SessionRef>, Option<Failure>) {
        let mut targets = Vec::new();
        let result = async {
            let scope = self.storage_scope(provider)?;
            let pages = session_pages(agent, "");
            futures_util::pin_mut!(pages);
            while let Some(page) = pages.try_next().await? {
                for summary in page {
                    if summary.thread.id.is_none() {
                        continue;
                    }
                    let target = self
                        .inner
                        .conversations
                        .discover(&summary.thread, &scope, summary.branch.as_deref())
                        .map_err(|error| Failure::new("history_import_failed", error))?;
                    self.inner.router.broadcast(
                        agent_protocol::protocol::Notification::SessionRenamed {
                            session: target.clone(),
                        },
                    );
                    targets.push(target);
                }
            }
            Ok::<_, Failure>(())
        }
        .await;
        (targets, result.err())
    }

    async fn initial_import(&self) {
        self.inner.initial_import.get_or_init(|| async {
            if let Err(error) = self.refresh_import().await {
                tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
            }
        }).await;
    }

    async fn refresh_import(&self) -> Result<(), Failure> {
        let _catalog = self.inner.catalog_import.lock().await;
        let mut targets = Vec::new();
        let mut failure = None;
        for (provider, agent) in self.agents() {
            let (discovered, error) = self.import_provider(provider, agent.as_ref()).await;
            targets.extend(discovered);
            if failure.is_none() {
                failure = error;
            }
        }
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            for target in targets {
                let Some(inner) = inner.upgrade() else { break };
                let service = Self { inner };
                if let Err(error) = service.import_conversation(&target, usize::MAX).await {
                    if let Err(commit) = service
                        .inner
                        .conversations
                        .import_failed(&target, &error.to_string())
                    {
                        tracing::error!(target: "bex", operation = "history.import_commit", message = %commit);
                    }
                    tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
                }
                service.inner.router.broadcast(
                    agent_protocol::protocol::Notification::HistoryChanged { session: target },
                );
            }
        });
        failure.map_or(Ok(()), Err)
    }

    fn agent(&self, provider: ProviderKind) -> Result<Arc<dyn Agent>, Failure> {
        self.inner
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&provider)
            .cloned()
            .ok_or_else(|| {
                self.inner
                    .startup_errors
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&provider)
                    .cloned()
                    .unwrap_or_else(|| {
                        Failure::new(
                            "provider_unavailable",
                            format!("{provider:?} is unavailable"),
                        )
                    })
            })
    }

    fn agents(&self) -> Vec<(ProviderKind, Arc<dyn Agent>)> {
        let mut agents: Vec<_> = self
            .inner
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(p, a)| (*p, a.clone()))
            .collect();
        agents.sort_by_key(|(p, _)| *p);
        agents
    }
    async fn account_request(&self, request: Call) -> Result<Body, Failure> {
        if matches!(request, Call::ListAccounts(_)) {
            let mut combined = op::Accounts {
                accounts: Vec::new(),
                selected: HashMap::new(),
                error: None,
            };
            let mut errors = self
                .inner
                .startup_errors
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            for (_, agent) in self.agents() {
                match Identity::list(agent.as_ref()).await {
                    Ok(accounts) => {
                        combined.accounts.extend(accounts.accounts);
                        combined.selected.extend(accounts.selected);
                        errors.extend(accounts.error);
                    }
                    Err(error) => errors.push(error.to_string()),
                }
            }
            combined.error = (!errors.is_empty()).then(|| errors.join("\n"));
            return Ok(combined.into());
        }
        use super::agent::{AccountCommand as Command, AccountReply};
        let (provider, command) = match request {
            Call::StartAccountLogin(p) => (p.provider, Command::StartLogin),
            Call::SelectAccount(p) => (p.provider, Command::Select { id: p.id }),
            Call::LogoutAccount(p) => (p.provider, Command::Logout { id: p.id }),
            Call::ReadAccountLogin(p) => (p.provider, Command::ReadLogin { id: p.id }),
            Call::CancelAccountLogin(p) => (p.provider, Command::CancelLogin { id: p.id }),
            Call::SubmitAccountLogin(p) => (
                p.provider,
                Command::SubmitLogin {
                    id: p.id,
                    code: p.code,
                },
            ),
            _ => return Err(Failure::new("invalid_params", "not an account request")),
        };
        Ok(match self.agent(provider)?.account(command).await? {
            AccountReply::Selection(value) => value.into(),
            AccountReply::Login(value) => value.into(),
            AccountReply::Status(value) => value.into(),
            AccountReply::Complete => agent_protocol::models::Empty {}.into(),
        })
    }

    pub async fn enable_browser(&self, profile: std::path::PathBuf) -> Result<(), String> {
        let browser = crate::browser::Browser::start(profile).await?;
        self.inner
            .browser
            .set(browser)
            .map_err(|_| "BEX browser is already configured".to_owned())
    }

    fn browser_config(&self, thread: &str) -> Result<Option<serde_json::Value>, Failure> {
        self.inner
            .browser
            .get()
            .map(|browser| {
                browser
                    .provider_config(thread)
                    .map_err(|e| Failure::new("browser_unavailable", e))
            })
            .transpose()
    }

    pub async fn enable_accounts(
        &self,
        directory: std::path::PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        self.inner.codex.enable_accounts(directory, config).await
    }

    pub async fn enable_claude(
        &self,
        program: std::path::PathBuf,
        directory: std::path::PathBuf,
        native_home: Option<std::path::PathBuf>,
    ) -> anyhow::Result<()> {
        let claude = crate::claude::Claude::load(program, directory, native_home)
            .await
            .inspect_err(|error| {
                self.inner
                    .startup_errors
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        ProviderKind::Claude,
                        Failure::new("provider_unavailable", error),
                    );
            })?;
        {
            let mut agents = self.inner.agents.write().unwrap_or_else(|e| e.into_inner());
            anyhow::ensure!(
                !agents.contains_key(&ProviderKind::Claude),
                "Claude Code is already configured"
            );
            agents.insert(ProviderKind::Claude, Arc::new(claude));
            self.inner
                .startup_errors
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&ProviderKind::Claude);
        }
        self.start_event_pumps();
        Ok(())
    }

    pub(crate) async fn shutdown_owned_processes(&self) {
        if let Some(browser) = self.inner.browser.get() {
            browser.shutdown().await;
        }
        self.inner.terminals.shutdown().await;
        for (_, agent) in self.agents() {
            agent.shutdown().await;
        }
    }

    pub(crate) fn revoke_device(&self, principal: &str) {
        self.inner.terminals.revoke_device(principal);
    }
    pub fn open_session(&self) -> HostSession {
        self.start_event_pumps();
        self.inner.router.open_session()
    }

    pub(crate) fn open_authenticated_session(&self, principal: String) -> HostSession {
        self.start_event_pumps();
        self.inner
            .router
            .open_authenticated_session(Some(principal))
    }

    pub fn close_session(&self, session: SessionId) {
        self.inner.router.close_session(session);
        self.inner.terminals.close_session(session);
        self.inner.files.clear_session(session);
        self.inner.dictation.close_session(session);
    }

    pub(crate) fn data_recipients(&self) -> (Vec<String>, Option<String>) {
        let ai = self
            .agents()
            .into_iter()
            .filter(|(_, a)| a.availability().is_ok())
            .map(|(p, _)| match p {
                ProviderKind::Codex => "OpenAI".into(),
                ProviderKind::Claude => "Anthropic".into(),
            })
            .collect();
        (
            ai,
            self.agent(ProviderKind::Codex)
                .ok()
                .filter(|a| a.availability().is_ok())
                .map(|_| "OpenAI".into()),
        )
    }
    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        let mut errors: serde_json::Map<_, _> = self
            .agents()
            .into_iter()
            .filter_map(|(p, a)| {
                a.availability().err().map(|e| {
                    (
                        provider_key(p),
                        serde_json::to_value(e).expect("failure serializes"),
                    )
                })
            })
            .collect();
        errors.extend(
            self.inner
                .startup_errors
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(provider, error)| {
                    (
                        provider_key(*provider),
                        serde_json::to_value(error).expect("failure serializes"),
                    )
                }),
        );
        serde_json::Value::Object(errors)
    }

    pub fn start(&self) {
        self.start_event_pumps();
        let service = self.clone();
        tokio::spawn(async move {
            service.initial_import().await;
        });
    }

    /// Dispatch a classified message from an authenticated session.
    pub async fn dispatch(&self, session: SessionId, message: &Call) -> Result<HostReply, String> {
        self.inner.router.ensure_session(session)?;
        if let Call::OpenSession(params) = message {
            return self.session_open(session, params).await;
        }
        if let Call::CreateSession(params) = message {
            let _workspace = self.inner.worktree_access.read().await;
            let result = async {
                let response = self.create_session(params.clone()).await?;
                let target = response.thread.id.clone().ok_or_else(|| {
                    Failure::new("invalid_thread", "created session ID is missing")
                })?;
                let read = self
                    .inner
                    .router
                    .retain_execution(target)
                    .map_err(|error| Failure::new("invalid_thread", error))?;
                self.inner
                    .router
                    .finish_session_read(read, session, response)
                    .map_err(|error| Failure::new("session_create_failed", error))
            }
            .await;
            return match result {
                Ok(reply) => Ok(reply),
                Err(error) => Ok(Response::from_result::<(), _>(Err(error)).into()),
            };
        }
        let result = match message {
            Call::Submit(input) => {
                // The Host owns execution after dispatch starts. Closing the RPC
                // drops only the reply waiter, never an admitted provider write.
                let service = self.clone();
                let input = input.clone();
                tokio::spawn(async move { service.execute_submission(&input).await })
                    .await
                    .map_err(|error| Failure::unknown("submission_outcome_unknown", error))
                    .and_then(|result| result)
                    .map(Into::into)
            }
            _ => {
                let _workspace_read = if matches!(
                    message,
                    Call::ForkSession(_)
                        | Call::AnswerSession(_)
                        | Call::StartTerminal(_)
                        | Call::WriteFile(_)
                        | Call::Upload(_)
                        | Call::ReviewWorkspace(_)
                ) {
                    Some(self.inner.worktree_access.read().await)
                } else {
                    None
                };
                self.request(session, message, session_target(message).0)
                    .await
            }
        };
        if let Err(error) = &result {
            agent_transport::diagnostics::rpc_error(
                message.method(),
                None,
                &serde_json::value::to_raw_value(error).map_err(invalid_message)?,
            );
        }
        Ok(Response::from_result(result).into())
    }

    async fn execute_submission(
        &self,
        input: &op::Submission,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use agent_protocol::session::SubmissionDelivery;
        let target = &input.thread_id;
        let id = input.client_user_message_id.as_str();
        let _execution = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        if let Some(delivery) = self
            .inner
            .conversations
            .previous_command(input)
            .map_err(|error| Failure::new("invalid_input", error))?
        {
            return replay_submission(delivery);
        }
        self.native_session(target)?;
        self.agent(target.provider)?.availability()?;
        let _workspace = self.inner.worktree_access.read().await;
        // Finish the source snapshot before a provider write can move its cursors.
        self.import_conversation(target, usize::MAX).await?;
        let _serial = self.inner.router.submission_lock(target).lock_owned().await;
        if let Some(delivery) = self
            .inner
            .conversations
            .admit(input)
            .map_err(|error| Failure::new("input_admission_failed", error))?
        {
            return replay_submission(delivery);
        }
        let result = match self.inner.router.begin_submission(target, id) {
            Ok(()) => self.submit_input(target, input).await,
            Err(error) => Err(error),
        };
        let delivery = match &result {
            Ok(receipt) => SubmissionDelivery::Accepted {
                turn_id: receipt.turn_id.clone(),
            },
            Err(error) if error.delivery == agent_transport::peer::Delivery::NotSent => {
                SubmissionDelivery::Rejected
            }
            Err(_) => SubmissionDelivery::Unknown,
        };
        self.inner
            .router
            .finish_submission(target, id, delivery)
            .map_err(|error| Failure::unknown("submission_outcome_unknown", error))?;
        result
    }

    async fn submit_input(
        &self,
        target: &agent_protocol::session::SessionRef,
        input: &op::Submission,
    ) -> Result<op::SubmissionReceipt, Failure> {
        if input
            .model
            .as_ref()
            .is_some_and(|model| model.provider != target.provider || model.id.trim().is_empty())
        {
            return Err(Failure::new(
                "provider_mismatch",
                "別のプロバイダーのモデルを使う場合は新しい会話を作成してください。",
            ));
        }
        let agent = self.agent(target.provider)?;
        agent.availability()?;
        let native = self.native_session(target)?;
        let state = agent.state(&native.id).await?;
        let mut response = state.response;
        if response.thread.id.as_ref() != Some(&native) {
            return Err(Failure::new(
                "invalid_session",
                "native session identity changed",
            ));
        }
        response.thread.id = Some(target.clone());
        response.thread = self.inner.router.overlay_execution(target, response.thread);
        let running_turn = response
            .thread
            .turns
            .iter()
            .flatten()
            .rev()
            .find(|t| {
                t.status == agent_protocol::models::TurnStatus::Running && !t.id.trim().is_empty()
            })
            .map(|t| t.id.as_str());
        let route = super::submission::submission_target(
            response.thread.status,
            running_turn,
            agent.running_input(),
            response.thread.cwd.as_deref(),
        )
        .map_err(|e| Failure::new("submission_unavailable", e))?;
        let mut reload = state.needs_reload;
        if let super::submission::SubmissionTarget::Start { cwd } = route
            && let Some(directory) = self
                .inner
                .worktrees
                .ensure_available(cwd)
                .await
                .map_err(|e| Failure::new("worktree_creation_failed", e))?
        {
            for (_, adapter) in self.agents() {
                adapter.discard_workspace_processes(&directory).await?;
            }
            reload = true;
        }
        let mut native_input = input.clone();
        native_input.thread_id = native;
        agent
            .submit(
                &native_input,
                route,
                reload,
                self.browser_config(&target.to_string())?,
            )
            .await
    }

    async fn answer_request(
        &self,
        session: SessionId,
        id: agent_protocol::ids::RequestId,
        answer: agent_protocol::requests::Answer,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        use agent_protocol::session::RequestDelivery;
        let (origin, body) = self
            .inner
            .router
            .claim_response(session, &id, &answer)
            .map_err(|error| Failure::new("invalid_answer", error))?;
        // Until a native command is admitted, cancellation is proven not sent.
        let mut delivery = scopeguard::guard((id, RequestDelivery::Awaiting), |(id, state)| {
            if let Err(error) = self.inner.router.response_delivery(&id, state) {
                tracing::error!(target: "bex", operation = "history.answer_commit", message = %error);
            }
        });
        let write = origin
            .source
            .prepare(&origin.native_id, &body, &answer)
            .await?;
        delivery.1 = RequestDelivery::Unknown;
        write.await?;
        let (id, _) = scopeguard::ScopeGuard::into_inner(delivery);
        self.inner
            .router
            .response_delivery(&id, RequestDelivery::Sent)
            .map_err(|error| Failure::unknown("answer_outcome_unknown", error))?;
        Ok(agent_protocol::models::Empty {})
    }

    async fn session_open(
        &self,
        session: SessionId,
        params: &agent_protocol::session::OpenSession,
    ) -> Result<HostReply, String> {
        let result: anyhow::Result<HostReply> = async {
            if params.limit == 0 {
                anyhow::bail!("invalid session reference or zero history limit");
            }
            let target = params.session.clone();
            let agent = self.agent(target.provider)?;
            let limit = params.limit;
            let read = self
                .inner
                .router
                .retain_execution(target.clone())
                .map_err(anyhow::Error::msg)?;
            let started = std::time::Instant::now();
            if let Err(error) = self.import_conversation(&target, 1).await {
                self.inner.conversations.import_failed(&target, &error.to_string())?;
            }
            let mut response = self.inner.conversations.open_thread(&target, limit, params.include_activity)?;
            let native_ms = started.elapsed().as_millis();
            if response.thread.id.as_ref() != Some(&target) {
                return Err(anyhow::anyhow!("conversation identity changed"));
            }
            describe_thread(
                &mut response.thread,
                agent.capabilities(),
                &self.project_snapshot().await?,
            );
            let project_ms = started.elapsed().as_millis() - native_ms;
            let more = response.thread.history_has_more == Some(true);
            response.thread.history_has_more = Some(more);
            response.thread.history_limit = Some(limit as u64);
            response.thread.history_read_state.get_or_insert_with(|| {
                agent_protocol::session::HistoryReadState::new(
                    if more {
                        agent_protocol::session::HistoryReadKind::Partial
                    } else {
                        agent_protocol::session::HistoryReadKind::Complete
                    },
                    Vec::new(),
                )
            });
            let reply = self.inner
                .router
                .finish_session_read(read, session, response)
                .map_err(anyhow::Error::msg)?;
            tracing::info!(target: "bex", operation = "history.open",
                message = %format_args!("native_ms={native_ms} project_ms={project_ms} total_ms={} bytes={} limit={limit}",
                    started.elapsed().as_millis(), reply.initial.len()));
            Ok(reply)
        }
        .await;
        match result {
            Ok(reply) => Ok(reply),
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(target: "bex", operation = "history.open", message);
                Ok(Response::error("session_open_failed", &message).into())
            }
        }
    }

    async fn read_item(
        &self,
        session: SessionId,
        params: &op::ReadItem,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<agent_protocol::operations::ItemResponse, Failure> {
        let live = self
            .inner
            .router
            .current_turn(target, &params.turn_id)
            .and_then(|turn| {
                turn.items
                    .as_ref()?
                    .iter()
                    .find(|item| item.id == params.item_id)
                    .filter(|item| !item.is_deferred())
                    .cloned()
            });
        let mut response = if let Some(item) = live {
            agent_protocol::operations::ItemResponse {
                item: Arc::unwrap_or_clone(item),
                transfer: None,
            }
        } else {
            let turn = self
                .inner
                .conversations
                .turn(target, &params.turn_id)
                .map_err(|error| Failure::new("item_unavailable", error))?;
            let item = turn
                .items
                .into_iter()
                .flatten()
                .find(|item| item.id == params.item_id)
                .ok_or_else(|| Failure::new("item_unavailable", "item is not available"))?;
            op::ItemResponse {
                item: Arc::unwrap_or_clone(item),
                transfer: None,
            }
        };
        let bytes = agent_protocol::protocol::encode(&response.item).expect("item serializes");
        if bytes.len() > agent_protocol::models::MAX_INLINE_ITEM_BYTES {
            response.transfer = Some(
                self.inner
                    .files
                    .download_bytes(session, bytes)
                    .await
                    .map_err(|error| Failure::new("item_transfer_failed", error))?,
            );
            response.item.defer();
        }
        Ok(response)
    }

    async fn request(
        &self,
        session: SessionId,
        request: &Call,
        target_session: Option<&agent_protocol::session::SessionRef>,
    ) -> Result<Body, Failure> {
        if let Call::Browser(params) = request {
            let browser = self.inner.browser.get().ok_or_else(|| {
                Failure::new(
                    "browser_unavailable",
                    "このHostではBEXブラウザが有効になっていません。",
                )
            })?;
            return browser
                .request(params)
                .await
                .map(Into::into)
                .map_err(|e| Failure::new("browser_failed", e));
        }
        let method = request.method();
        if let Call::ReadItem(params) = request {
            let Some(target) = target_session else {
                return Err(Failure::new("invalid_params", "session ID is required"));
            };
            return self
                .read_item(session, params, target)
                .await
                .map(Into::into);
        }
        if let Some(target) = target_session {
            let capabilities = self.agent(target.provider)?.capabilities();
            if matches!(request, Call::ForkSession(_)) && !capabilities.fork {
                return Err(Failure::new(
                    "unsupported_operation",
                    format!("{method} is unsupported by this provider"),
                ));
            }
        }
        let response = match request {
            Call::ReadTurnItems(params) => {
                let read = self
                    .inner
                    .router
                    .retain_execution(params.session.clone())
                    .map_err(|error| Failure::new("invalid_params", error))?;
                let items = self
                    .inner
                    .conversations
                    .turn(&params.session, &params.turn_id)
                    .map_err(|error| Failure::new("turn_details_failed", error))?
                    .items
                    .unwrap_or_default();
                self.inner
                    .router
                    .finish_turn_read(read, session, params.turn_id.clone(), items)
                    .map_err(|error| Failure::new("turn_details_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadHistory(params) => {
                self.native_session(&params.session)?;
                let mut page = self
                    .inner
                    .conversations
                    .history(&params.session, &params.cursor, params.include_activity)
                    .map_err(|error| Failure::new("history_read_failed", error))?;
                if page.turns.is_empty() && page.next_cursor.is_some() {
                    if let Err(error) = self.import_conversation(&params.session, 2).await {
                        self.inner
                            .conversations
                            .import_failed(&params.session, &error.to_string())
                            .map_err(|error| Failure::new("history_read_failed", error))?;
                        return Err(error);
                    }
                    page = self
                        .inner
                        .conversations
                        .history(&params.session, &params.cursor, params.include_activity)
                        .map_err(|error| Failure::new("history_read_failed", error))?;
                }
                agent_protocol::models::defer_item_details(
                    &mut page.turns,
                    agent_protocol::models::MAX_INLINE_ITEM_BYTES,
                );
                if agent_protocol::protocol::encode(Response::Success { result: &page })
                    .map_err(|error| Failure::new("invalid_thread_history", error))?
                    .len()
                    > agent_protocol::protocol::MAX_FRAME_BYTES
                {
                    agent_protocol::models::defer_item_details(&mut page.turns, 0);
                }
                page.into()
            }
            Call::ReadPermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.agent(params.provider)?
                    .read_permissions()
                    .await?
                    .into()
            }
            Call::UpdatePermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.agent(params.provider)?
                    .update_permissions(params.mode, &params.version)
                    .await?
                    .into()
            }
            Call::ComposerCatalog(params) => {
                let cwd = params.cwd.as_str();
                let results = futures_util::future::join_all(
                    self.agents()
                        .into_iter()
                        .map(|(_, agent)| async move { agent.catalog(cwd).await }),
                )
                .await;
                let mut catalog = agent_protocol::composer::ComposerCatalog {
                    cwd: params.cwd.clone(),
                    ..Default::default()
                };
                for (provider, error) in self.inner.startup_errors.read().unwrap().iter() {
                    catalog.errors.insert(*provider, vec![error.to_string()]);
                }
                for result in results {
                    catalog.candidates.extend(result.candidates);
                    catalog.errors.extend(result.errors);
                }
                catalog.candidates.sort_by(|a, b| {
                    a.invocation
                        .name
                        .to_lowercase()
                        .cmp(&b.invocation.name.to_lowercase())
                        .then(a.invocation.path.cmp(&b.invocation.path))
                        .then(a.invocation.provider.cmp(&b.invocation.provider))
                });
                catalog
                    .candidates
                    .dedup_by(|a, b| a.invocation == b.invocation);
                catalog.into()
            }
            Call::ReadAccountUsage(params) => {
                self.agent(params.provider)?.usage(&params.id).await?.into()
            }
            Call::ListAccounts(_)
            | Call::SelectAccount(_)
            | Call::LogoutAccount(_)
            | Call::StartAccountLogin(_)
            | Call::ReadAccountLogin(_)
            | Call::SubmitAccountLogin(_)
            | Call::CancelAccountLogin(_) => self.account_request(request.clone()).await?,

            Call::ListModels(params) => {
                let cursors: std::collections::BTreeMap<ProviderKind, Option<String>> = params
                    .cursor
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_else(|| {
                        self.agents()
                            .into_iter()
                            .map(|(provider, _)| provider)
                            .chain(self.inner.startup_errors.read().unwrap().keys().copied())
                            .map(|provider| (provider, None))
                            .collect()
                    });
                let mut page = op::ModelPage {
                    data: Vec::new(),
                    next_cursor: None,
                    provider_errors: None,
                };
                let mut next = std::collections::BTreeMap::new();
                for (provider, cursor) in cursors {
                    let result = match self.agent(provider) {
                        Ok(agent) => {
                            agent
                                .models(&op::ListModels {
                                    cursor,
                                    ..params.clone()
                                })
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(result) => {
                            page.data.extend(result.data);
                            if let Some(cursor) = result.next_cursor {
                                next.insert(provider, Some(cursor));
                            }
                        }
                        Err(error) => {
                            page.provider_errors
                                .get_or_insert_default()
                                .insert(provider_key(provider), serde_json::to_value(error)?);
                        }
                    }
                }
                if !next.is_empty() {
                    page.next_cursor = Some(serde_json::to_string(&next)?);
                }
                if params.cursor.is_none() && page.data.is_empty() && page.provider_errors.is_some()
                {
                    return Err(Failure::new(
                        "models_unavailable",
                        serde_json::to_value(&page.provider_errors)?,
                    ));
                }
                page.into()
            }

            Call::RequestSession(params) => (self
                .inner
                .router
                .request_session(&params.request_id.to_string())
                .ok_or_else(|| {
                    Failure::new("request_unavailable", "request is no longer pending")
                })?)
            .into(),
            Call::SessionScope(_) => {
                let started = std::time::Instant::now();
                let scope = provider_storage_scope(
                    self.agents()
                        .into_iter()
                        .map(|(p, a)| (p, canonical_storage_path(a.storage_directory()))),
                )?;
                tracing::info!(target: "bex", operation = "host.connection.scope",
                    message = %format_args!("elapsed_ms={}", started.elapsed().as_millis()));
                scope.into()
            }
            Call::ConnectionPerformance(performance) => {
                let performance = performance.clone();
                tokio::task::spawn_blocking(move || {
                    agent_transport::diagnostics::connection_performance(&performance)
                })
                .await
                .map_err(|error| Failure::new("diagnostic_write_failed", error.to_string()))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::AnswerSession(params) => (self
                .answer_request(session, params.request_id.clone(), params.answer.clone())
                .await?)
                .into(),
            Call::AddProject(params) => (self
                .inner
                .projects
                .register(std::path::Path::new(&params.cwd))
                .await
                .map_err(|error| Failure::new("project_add_failed", error))?)
            .into(),
            Call::ListSessions(params) => {
                let started = std::time::Instant::now();
                let result = self.host_title_list(params.query.clone()).await;
                tracing::info!(target: "bex", operation = "host.thread.list.performance",
                    message = %format_args!("elapsed_ms={} success={}", started.elapsed().as_millis(), result.is_ok()));
                result?.into()
            }
            Call::ImportHistory(_) => {
                self.refresh_import().await?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadWorktreeSettings(_) | Call::UpdateWorktreeSettings(_) => {
                let update = if let Call::UpdateWorktreeSettings(settings) = request {
                    Some(settings.clone())
                } else {
                    None
                };
                (self
                    .inner
                    .worktrees
                    .settings(update)
                    .await
                    .map_err(|error| Failure::new("worktree_settings_failed", error))?)
                .into()
            }
            Call::ListWorktrees(_) => (self.worktree_list().await?).into(),
            Call::RemoveWorktree(params) => {
                let _exclusive = self.inner.worktree_access.write().await;
                (self.remove_worktree(params.path.clone()).await?).into()
            }

            Call::CreateSession(_) => unreachable!("creation owns its subscription"),
            Call::StartTerminal(params) => (self
                .inner
                .terminals
                .start(
                    self.inner.router.clone(),
                    session,
                    params.handle.clone(),
                    params.cwd.clone(),
                    params.size,
                )
                .await
                .map_err(|error| Failure::new("terminal_start_failed", error))?)
            .into(),
            Call::WriteTerminal(_)
            | Call::ResizeTerminal(_)
            | Call::KillTerminal(_)
            | Call::DetachTerminal(_) => (self
                .inner
                .terminals
                .request(session, request)
                .await
                .map_err(|error| Failure::new("terminal_operation_failed", error))?)
            .into(),
            Call::PrepareDictation(params) => {
                self.inner
                    .dictation
                    .prepare(session, params.id.clone())
                    .map_err(|error| Failure::new("dictation_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::CancelDictation(params) => {
                self.inner.dictation.cancel(session, &params.id);
                agent_protocol::models::Empty {}.into()
            }
            Call::Transcribe(params) => (self
                .inner
                .dictation
                .transcribe(session, params.preparation.as_deref(), &params.audio)
                .await
                .map_err(|error| Failure::new("dictation_failed", error))?)
            .into(),
            Call::ReviewWorkspace(params) => {
                (crate::inspect_workspace(params.cwd.clone())
                    .await
                    .map_err(|error| Failure::new("workspace_review_failed", error))?)
                .into()
            }
            Call::ListFiles(_)
            | Call::ReadFile(_)
            | Call::WriteFile(_)
            | Call::Upload(_)
            | Call::Download(_)
            | Call::ReadVisualization(_) => self
                .inner
                .files
                .request(session, request.clone())
                .await
                .map_err(|error| Failure::new("file_operation_failed", error))?,
            Call::ForkSession(params) => {
                let target = target_session.expect("session-scoped fork");
                let agent = self.agent(target.provider)?;
                let native = self.native_session(target)?;
                self.start_thread(agent.as_ref(), |browser| {
                    agent.fork(&native.id, &params.last_turn_id, browser)
                })
                .await?
                .into()
            }
            Call::Interrupt(params) => {
                let target = target_session.expect("session-scoped interrupt");
                let native = self.native_session(target)?;
                self.agent(target.provider)?
                    .interrupt(&native.id, &params.turn_id)
                    .await?
                    .into()
            }
            Call::RenameSession(params) => {
                let target = target_session.expect("session-scoped rename");
                self.native_session(target)?;
                self.inner
                    .conversations
                    .rename(target, &params.name, true)
                    .map_err(|error| Failure::new("rename_failed", error))?;
                self.inner.router.broadcast(
                    agent_protocol::protocol::Notification::SessionRenamed {
                        session: target.clone(),
                    },
                );
                agent_protocol::models::Empty {}.into()
            }
            _ => {
                return Err(Failure::new(
                    "method_not_found",
                    format!("unregistered method: {method}"),
                ));
            }
        };
        Ok(response)
    }

    pub(crate) fn files(&self) -> &crate::workspace_files::WorkspaceFiles {
        &self.inner.files
    }

    async fn worktree_list(&self) -> Result<Vec<agent_protocol::models::Worktree>, Failure> {
        let mut worktrees = self
            .inner
            .worktrees
            .list()
            .await
            .map_err(|error| Failure::new("worktree_list_failed", error))?;
        if worktrees.is_empty() {
            return Ok(worktrees);
        }
        for error in self
            .inner
            .startup_errors
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            for worktree in &mut worktrees {
                worktree.blocked_reason =
                    Some(format!("稼働状況を確認できないため削除できません: {error}"));
            }
        }
        let mut threads = Vec::new();
        let agents = self.agents();
        for (_, agent) in &agents {
            let pages = session_pages(agent.as_ref(), "");
            futures_util::pin_mut!(pages);
            while let Some(result) = pages.next().await {
                match result {
                    Ok(page) => {
                        for summary in page {
                            let mut thread = summary.thread;
                            if let Some(native) = &thread.id {
                                thread.id = Some(
                                    self.inner
                                        .conversations
                                        .bind(native, &self.storage_scope(native.provider)?)
                                        .map_err(|error| Failure::new("invalid_thread", error))?,
                                );
                            }
                            threads.push(thread);
                        }
                    }
                    Err(error) if error.code == "invalid_session_list" => return Err(error),
                    Err(error) => {
                        for w in &mut worktrees {
                            w.blocked_reason =
                                Some(format!("稼働状況を確認できないため削除できません: {error}"));
                        }
                        break;
                    }
                }
            }
        }
        let mut active_sessions = std::collections::HashSet::new();
        for worktree in &mut worktrees {
            let directory = std::path::Path::new(&worktree.path);
            let mut active = std::collections::HashSet::new();
            for (_, agent) in &agents {
                match agent.active_sessions_in(directory).await {
                    Ok(sessions) => {
                        for native in sessions {
                            active.insert(
                                self.inner
                                    .conversations
                                    .bind(&native, &self.storage_scope(native.provider)?)
                                    .map_err(|error| Failure::new("invalid_thread", error))?,
                            );
                        }
                    }
                    Err(error) => {
                        worktree.blocked_reason =
                            Some(format!("稼働状況を確認できないため削除できません: {error}"))
                    }
                }
            }
            if !active.is_empty() {
                worktree.blocked_reason = Some(
                    "このワークツリーで作業を実行中です。完了または停止してから削除してください。"
                        .into(),
                );
            }
            active_sessions.extend(active);
        }
        // Providers can omit a first, still-running turn from their history list.
        // Resolve every retained execution before allowing any checkout removal.
        for target in self.inner.router.execution_targets() {
            if threads
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&target))
            {
                continue;
            }
            let native = self.native_session(&target)?;
            let mut response = self
                .agent(target.provider)?
                .state(&native.id)
                .await?
                .response;
            if response.thread.id.as_ref() != Some(&native) {
                return Err(Failure::new(
                    "invalid_thread",
                    "native session identity changed",
                ));
            }
            response.thread.id = Some(target);
            threads.push(response.thread);
        }
        for thread in threads {
            let thread = match thread.id.clone() {
                Some(id) => self.inner.router.overlay_execution(&id, thread),
                None => thread,
            };
            let active = thread
                .id
                .as_ref()
                .is_some_and(|id| active_sessions.contains(id))
                || worktree_active(
                    thread.status,
                    thread.turns.as_deref().unwrap_or_default(),
                    !thread.requests.is_empty(),
                    thread.submissions.values(),
                );
            let Some(cwd) = thread.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) else {
                if active {
                    return Err(Failure::new(
                        "worktree_activity_unknown",
                        "実行中の会話の作業場所を確認できないため削除できません。",
                    ));
                }
                continue;
            };
            let cwd = tokio::fs::canonicalize(cwd)
                .await
                .unwrap_or_else(|_| std::path::PathBuf::from(cwd));
            let cwd = dunce::simplified(&cwd);
            for worktree in &mut worktrees {
                if !cwd.starts_with(&worktree.path) {
                    continue;
                }
                if active {
                    worktree.blocked_reason = Some("このワークツリーで作業を実行中です。完了または停止してから削除してください。".into());
                }
                if let Some(id) = &thread.id {
                    worktree
                        .threads
                        .push(agent_protocol::models::WorktreeThread {
                            id: id.clone(),
                            name: thread
                                .name
                                .clone()
                                .filter(|name| !name.is_empty())
                                .or_else(|| {
                                    thread
                                        .preview
                                        .as_deref()
                                        .map(|preview| preview.chars().take(120).collect())
                                })
                                .unwrap_or_else(|| "新しいチャット".into()),
                            active,
                        });
                }
            }
        }
        for worktree in &mut worktrees {
            if self
                .inner
                .terminals
                .in_use(std::path::Path::new(&worktree.path))
            {
                worktree.blocked_reason =
                    Some("このワークツリーのターミナルを閉じてから削除してください。".into());
            }
        }
        Ok(worktrees)
    }

    async fn remove_worktree(&self, path: String) -> Result<(), Failure> {
        let entries = self.worktree_list().await?;
        let entry = entries
            .iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                Failure::new(
                    "worktree_remove_failed",
                    "Bexが作成したワークツリーではありません。",
                )
            })?;
        if let Some(reason) = &entry.blocked_reason {
            return Err(Failure::new("worktree_remove_failed", reason));
        }
        self.inner
            .worktrees
            .remove(path, false)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }

    pub(crate) async fn cleanup_merged_worktrees(&self) -> anyhow::Result<()> {
        if !self.inner.worktrees.settings(None).await?.delete_merged {
            return Ok(());
        }
        let _exclusive = self.inner.worktree_access.write().await;
        let entries = self.worktree_list().await?;
        let statuses = crate::worktrees::directory_statuses(
            entries
                .iter()
                .filter(|entry| entry.blocked_reason.is_none())
                .map(|entry| crate::worktrees::StatusSource {
                    cwd: entry.path.clone(),
                    checkout: None,
                    repository: None,
                    branch: None,
                })
                .collect(),
        )
        .await?;
        for (source, status) in statuses {
            if status == agent_protocol::models::WorktreeStatus::Merged
                && let Err(error) = self.inner.worktrees.remove(source.cwd, true).await
            {
                tracing::warn!(target: "bex", operation = "host.worktree.cleanup", message = %error);
            }
        }
        Ok(())
    }

    async fn project_snapshot(&self) -> Result<crate::projects::state::Snapshot, Failure> {
        self.inner
            .projects
            .load()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))
    }

    async fn host_title_list(
        &self,
        query: ListQuery,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        self.initial_import().await;
        let snapshot = self.project_snapshot().await?;
        let mut titles = crate::projects::titles::TitleList::new(&snapshot.projects, &query);
        let mut threads = Vec::new();
        let mut branches = HashMap::new();
        for (provider, agent) in self.agents() {
            let scope = self.storage_scope(provider)?;
            for summary in self
                .inner
                .conversations
                .titles(provider, &scope, &query.search_term)
                .map_err(|error| Failure::new("sessions_unavailable", error))?
            {
                let mut thread = summary.thread;
                if let (Some(id), Some(branch)) = (&thread.id, summary.branch) {
                    branches.insert(id.clone(), branch);
                }
                describe_thread(&mut thread, agent.capabilities(), &snapshot);
                threads.push(thread);
            }
        }
        threads.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then(a.id.cmp(&b.id))
        });
        for thread in threads {
            titles.push(thread);
            if titles.complete() {
                break;
            }
        }
        let provider_errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut page = titles.finish();
        let sources: Vec<_> = page
            .data
            .iter()
            .map(|thread| {
                let cwd = thread.cwd.as_ref()?;
                let mapping = snapshot.worktree_mapping(std::path::Path::new(cwd));
                Some(crate::worktrees::StatusSource {
                    cwd: cwd.clone(),
                    checkout: mapping.map(|(checkout, _)| checkout.to_owned()),
                    repository: mapping.map(|(_, repository)| repository.to_owned()),
                    branch: thread.id.as_ref().and_then(|id| branches.remove(id)),
                })
            })
            .collect();
        let statuses =
            crate::worktrees::directory_statuses(sources.iter().flatten().cloned().collect())
                .await
                .map_err(|error| Failure::new("worktree_status_failed", error))?;
        for (thread, source) in page.data.iter_mut().zip(sources) {
            thread.worktree_status = source
                .as_ref()
                .and_then(|source| statuses.get(source))
                .copied();
        }
        if !provider_errors.is_empty() {
            page.provider_errors = Some(provider_errors);
        }
        Ok(page)
    }

    async fn create_session(
        &self,
        mut params: agent_protocol::operations::CreateSession,
    ) -> Result<ThreadResponse, Failure> {
        let provider = params.provider;
        if params
            .model
            .as_ref()
            .is_some_and(|model| model.provider != provider || model.id.trim().is_empty())
        {
            return Err(Failure::new(
                "provider_mismatch",
                "model must name a model of the requested provider",
            ));
        }
        let agent = self.agent(provider)?;
        agent.validate_create()?;
        // A missing selection must not inherit the App Server's checkout.
        // Keep the real cwd on the thread; project enrichment identifies
        // this persisted location as a chat even after a Host restart.
        if params
            .cwd
            .as_deref()
            .is_none_or(|cwd| cwd.trim().is_empty())
        {
            let directory = self.inner.projects.chat_directory();
            tokio::fs::create_dir_all(&directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            let directory = tokio::fs::canonicalize(directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            let directory = dunce::simplified(&directory).to_owned();
            params.cwd = Some(directory.into_os_string().into_string().map_err(|_| {
                Failure::new("chat_directory_unavailable", "chat path is not UTF-8")
            })?);
        } else {
            match self.inner.worktrees.prepare(params.cwd.as_deref()).await {
                Ok(Some(cwd)) => {
                    params.cwd = Some(cwd.into_os_string().into_string().map_err(|_| {
                        Failure::new("worktree_creation_failed", "worktree path is not UTF-8")
                    })?)
                }
                Ok(None) => {}
                Err(error) => return Err(Failure::new("worktree_creation_failed", error)),
            }
        }
        self.start_thread(agent.as_ref(), |browser| {
            agent.create(
                params.cwd.as_deref().unwrap_or_default(),
                params.model.as_ref().map(|m| m.id.as_str()),
                browser,
            )
        })
        .await
    }

    /// New threads browse under a provisional scope until their native ID exists.
    async fn start_thread<F>(
        &self,
        agent: &dyn Agent,
        start: impl FnOnce(Option<serde_json::Value>) -> F,
    ) -> Result<ThreadResponse, Failure>
    where
        F: std::future::Future<Output = Result<ThreadResponse, Failure>>,
    {
        let scope = uuid::Uuid::new_v4().to_string();
        let mut response = start(self.browser_config(&scope)?).await?;
        let native =
            response.thread.id.clone().ok_or_else(|| {
                Failure::new("invalid_thread", "created thread identity is missing")
            })?;
        let target = self
            .inner
            .conversations
            .bind(&native, &self.storage_scope(native.provider)?)
            .map_err(|error| Failure::new("session_create_failed", error))?;
        let page = agent_protocol::session::HistoryPage {
            turns: self
                .hydrate_history(
                    agent,
                    &native,
                    response.thread.turns.take().unwrap_or_default(),
                )
                .await?,
            next_cursor: response.thread.history_cursor.take(),
        };
        self.inner
            .conversations
            .import_page(&target, Some(&response), &page)
            .map_err(|error| Failure::new("session_create_failed", error))?;
        if page.next_cursor.is_some() {
            self.import_conversation(&target, usize::MAX).await?;
        }
        response = self
            .inner
            .conversations
            .open_thread(&target, 5, false)
            .map_err(|error| Failure::new("session_create_failed", error))?;
        if let Some(browser) = self.inner.browser.get()
            && let Some(id) = &response.thread.id
        {
            browser.bind_scope(scope, id.to_string()).await;
        }
        describe_thread(
            &mut response.thread,
            agent.capabilities(),
            &self.project_snapshot().await?,
        );
        Ok(response)
    }

    fn start_event_pumps(&self) {
        let mut started = self
            .inner
            .event_pumps
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (provider, agent) in self.agents() {
            if !started.insert(provider) {
                continue;
            }
            if let Some(mut events) = agent.event_stream() {
                let inner = Arc::downgrade(&self.inner);
                let router = self.inner.router.clone();
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        let Some(inner) = inner.upgrade() else { break };
                        let service = Self { inner };
                        let mut change = event.change;
                        let result = async {
                            let renamed = matches!(&change, super::agent::AgentChange::Renamed(_));
                            let native = match &mut change {
                                super::agent::AgentChange::Session { session, .. }
                                | super::agent::AgentChange::Request { session, .. }
                                | super::agent::AgentChange::Renamed(session) => Some(session),
                                _ => None,
                            };
                            if let Some(native) = native {
                                let target = service
                                    .inner
                                    .conversations
                                    .bind(
                                        native,
                                        &service
                                            .storage_scope(provider)
                                            .map_err(|error| error.to_string())?,
                                    )
                                    .map_err(|error| error.to_string())?;
                                if renamed {
                                    let response = agent
                                        .state(&native.id)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                    if let Some(name) = response.response.thread.name {
                                        service
                                            .inner
                                            .conversations
                                            .rename(&target, &name, false)
                                            .map_err(|error| error.to_string())?;
                                    }
                                }
                                *native = target;
                            }
                            if let super::agent::AgentChange::Session { change, .. } = &mut change {
                                let items = match change {
                                    agent_protocol::session::SessionChange::Turn {
                                        turn, ..
                                    } => turn.items.as_deref_mut(),
                                    agent_protocol::session::SessionChange::TurnItems {
                                        items,
                                        ..
                                    } => Some(items.as_mut_slice()),
                                    agent_protocol::session::SessionChange::Item {
                                        item, ..
                                    } => Some(std::slice::from_mut(item)),
                                    _ => None,
                                };
                                for item in items.into_iter().flatten() {
                                    service
                                        .normalize_item(Arc::make_mut(item))
                                        .map_err(|error| error.to_string())?;
                                }
                            }
                            change.apply(&router)
                        }
                        .await;
                        if let Err(error) = &result {
                            tracing::error!(target: "bex", operation = "history.event_commit", message = %error);
                        }
                        if let Some(applied) = event.applied {
                            let _ = applied.send(result);
                        }
                    }
                });
            }
        }
    }
}

fn replay_submission(
    delivery: agent_protocol::session::SubmissionDelivery,
) -> Result<op::SubmissionReceipt, Failure> {
    use agent_protocol::session::SubmissionDelivery;
    match delivery {
        SubmissionDelivery::Accepted { turn_id } => Ok(op::SubmissionReceipt { turn_id }),
        SubmissionDelivery::Rejected => Err(Failure::new(
            "submission_rejected",
            "input was previously rejected",
        )),
        _ => Err(Failure::unknown(
            "submission_outcome_unknown",
            "input was already admitted; inspect the conversation before retrying",
        )),
    }
}

fn worktree_active<'a>(
    status: agent_protocol::models::SessionStatus,
    turns: &[Arc<agent_protocol::models::Turn>],
    has_requests: bool,
    mut submissions: impl Iterator<Item = &'a agent_protocol::session::SubmissionDelivery>,
) -> bool {
    use agent_protocol::{
        execution::TurnStatus, models::SessionStatus, session::SubmissionDelivery,
    };
    status == SessionStatus::Running
        || has_requests
        || turns.iter().any(|turn| turn.status == TurnStatus::Running)
        || submissions.any(|delivery| match delivery {
            SubmissionDelivery::Rejected => false,
            SubmissionDelivery::Accepted { turn_id: Some(id) } => {
                // Completed receipts can remain for replay after interruption.
                // Only a known finished turn proves that input no longer owns work.
                !turns.iter().any(|turn| {
                    &turn.id == id
                        && matches!(
                            turn.status,
                            TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                        )
                })
            }
            _ => true,
        })
}

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid request: {error}")
}

// Serialize the sorted map directly: a JSON Value's object ordering can vary
// with serde_json features unified by unrelated client dependencies.
fn provider_storage_scope(
    areas: impl IntoIterator<Item = (ProviderKind, std::path::PathBuf)>,
) -> Result<String, serde_json::Error> {
    let areas: std::collections::BTreeMap<_, _> = areas
        .into_iter()
        .map(|(provider, path)| (provider_key(provider), path))
        .collect();
    let bytes = serde_json::to_vec(&areas)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// Resolve existing ancestors too: a newly created native directory must not
// change a scope merely because /var is a symlink to /private/var on macOS.
fn canonical_storage_path(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(path) = dunce::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            canonical_storage_path(parent).join(name)
        }
        _ => std::env::current_dir().unwrap_or_default().join(path),
    }
}

// Every session-scoped operation provides its target here. Submission IDs
// additionally identify the operations whose delivery must be tracked.
fn session_target(request: &Call) -> (Option<&agent_protocol::session::SessionRef>, Option<&str>) {
    match request {
        Call::Submit(p) => (Some(&p.thread_id), Some(p.client_user_message_id.as_str())),
        Call::ForkSession(p) => (Some(&p.thread_id), None),
        Call::Interrupt(p) => (Some(&p.thread_id), None),
        Call::ReadItem(p) => (Some(&p.thread_id), None),
        Call::ReadTurnItems(p) => (Some(&p.session), None),
        Call::RenameSession(p) => (Some(&p.thread_id), None),
        _ => (None, None),
    }
}

fn provider_key(provider: ProviderKind) -> String {
    serde_json::to_value(provider)
        .expect("provider serializes")
        .as_str()
        .unwrap()
        .to_owned()
}

fn describe_thread(
    thread: &mut Thread,
    mut capabilities: agent_protocol::session::Capabilities,
    projects: &crate::projects::state::Snapshot,
) {
    capabilities.rename = true;
    thread.project_id = projects.project_membership(thread.cwd.as_deref());
    thread.capabilities = Some(capabilities);
}

#[cfg(test)]
mod tests {
    #[test]
    fn worktree_activity_requires_finished_delivery_and_no_other_live_work() {
        use super::worktree_active;
        use agent_protocol::{
            execution::TurnStatus, models::SessionStatus, session::SubmissionDelivery,
        };
        let receipt = SubmissionDelivery::Accepted {
            turn_id: Some("accepted".into()),
        };
        for status in [
            TurnStatus::Running,
            TurnStatus::Unknown,
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let turns = [std::sync::Arc::new(agent_protocol::models::Turn {
                id: "accepted".into(),
                status,
                ..Default::default()
            })];
            let busy = |session, requests, deliveries: &[SubmissionDelivery]| {
                worktree_active(session, &turns, requests, deliveries.iter())
            };
            assert_eq!(
                busy(SessionStatus::Idle, false, &[]),
                status == TurnStatus::Running
            );
            assert_eq!(
                busy(SessionStatus::Idle, false, std::slice::from_ref(&receipt)),
                matches!(status, TurnStatus::Running | TurnStatus::Unknown)
            );
            assert!(busy(SessionStatus::Running, false, &[]));
            assert!(busy(SessionStatus::Idle, true, &[]));
            for unresolved in [
                SubmissionDelivery::Sending,
                SubmissionDelivery::Unknown,
                SubmissionDelivery::Accepted { turn_id: None },
                SubmissionDelivery::Accepted {
                    turn_id: Some("other".into()),
                },
            ] {
                assert!(busy(
                    SessionStatus::Idle,
                    false,
                    &[receipt.clone(), unresolved]
                ));
            }
        }
        assert!(!worktree_active(
            SessionStatus::Idle,
            &[],
            false,
            [SubmissionDelivery::Rejected].iter()
        ));
        assert!(worktree_active(
            SessionStatus::Idle,
            &[],
            false,
            [receipt].iter()
        ));
    }

    use agent_protocol::session::ProviderKind;

    #[tokio::test]
    async fn successful_configuration_clears_the_provider_startup_error() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("claude");
        let registry = directory.join("accounts/accounts.json");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        std::fs::write(&registry, "invalid registry").unwrap();
        let program = root.path().join("native-cli.exe");
        std::fs::write(&program, []).unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
        )
        .unwrap();
        let configure = || {
            service.enable_claude(
                program.clone(),
                directory.clone(),
                Some(root.path().join("native")),
            )
        };
        assert!(configure().await.is_err());
        assert_eq!(
            service.provider_errors()["claude"]["code"],
            "provider_unavailable"
        );
        std::fs::write(&registry, r#"{"accounts":[],"selectedId":null}"#).unwrap();
        configure().await.unwrap();
        assert!(service.provider_errors().get("claude").is_none());
        let session = service.open_session();
        let response = service
            .dispatch(
                session.id(),
                &Call::CreateSession(op::CreateSession {
                    provider: ProviderKind::Claude,
                    cwd: Some(root.path().to_string_lossy().into_owned()),
                    model: None,
                }),
            )
            .await
            .unwrap();
        let Response::Success { result: response } = agent_protocol::protocol::decode::<
            Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap() else {
            panic!("provider did not recover");
        };
        assert_eq!(
            response.response.thread.id.unwrap().provider,
            ProviderKind::Claude
        );
    }

    #[tokio::test]
    async fn answers_keep_delivery_evidence_until_the_source_resolves_them() {
        use super::*;
        use agent_protocol::{
            requests::{Answer, ElicitationAnswer},
            session::{RequestDelivery, SessionChange},
        };
        use futures_util::FutureExt;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("not used".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
        )
        .unwrap();
        service
            .enable_claude(
                root.path().join("unused-cli"),
                root.path().join("claude"),
                Some(root.path().join("native")),
            )
            .await
            .unwrap();
        let connection = service.open_session();
        let router = &service.inner.router;
        let target =
            agent_protocol::session::SessionRef::new(ProviderKind::Claude, "native".into())
                .unwrap();
        let target = service
            .inner
            .conversations
            .bind(&target, &service.storage_scope(target.provider).unwrap())
            .unwrap();
        let (input, mut receiver) = tokio::sync::mpsc::channel(1);
        let instance = uuid::Uuid::new_v4();
        let answer = Answer::Elicitation {
            action: ElicitationAnswer::Accept {
                values: serde_json::json!({}),
            },
        };
        let delivery = |id: &agent_protocol::ids::RequestId| {
            let thread = router.overlay_execution(&target, Thread::default());
            thread.requests.get(id).map(|request| request.delivery)
        };
        for native in ["cancelled", "interrupted", "written"] {
            let adapted = super::super::requests::claude(uuid::Uuid::new_v4().to_string().into(), &"unrelated".into(), &serde_json::json!({"subtype":"elicitation","mcp_server_name":"server","requested_schema":{"type":"object","properties":{}}})).unwrap();
            let id = adapted.request.id.clone();
            router
                .request(
                    target.clone(),
                    crate::claude::request_origin(
                        instance,
                        serde_json::json!(native),
                        input.clone(),
                        adapted.answers,
                    ),
                    adapted.request,
                )
                .unwrap();
            // Session-scoped elicitation works without a live turn and survives another turn's completion.
            router
                .session_change(
                    &target,
                    SessionChange::Turn {
                        turn: agent_protocol::models::Turn {
                            id: "unrelated".into(),
                            ..Default::default()
                        },
                        completed: true,
                    },
                )
                .unwrap();
            assert_eq!(delivery(&id), Some(RequestDelivery::Awaiting));
            if native == "cancelled" {
                assert!(
                    input
                        .try_send(crate::claude::Command {
                            value: serde_json::Value::Null,
                            user: None,
                            delivered: None,
                        })
                        .is_ok()
                );
            }
            let mut operation =
                Box::pin(service.answer_request(connection.id(), id.clone(), answer.clone()));
            assert!(operation.as_mut().now_or_never().is_none());
            assert_eq!(delivery(&id), Some(RequestDelivery::Sending));
            assert!(
                router
                    .claim_response(connection.id(), &id, &answer)
                    .is_err()
            );
            let command = receiver.try_recv().unwrap();
            if native == "cancelled" {
                drop(operation);
                assert_eq!(delivery(&id), Some(RequestDelivery::Awaiting));
                assert!(command.delivered.is_none());
            } else if native == "interrupted" {
                drop(operation);
                assert_eq!(delivery(&id), Some(RequestDelivery::Unknown));
                assert!(command.delivered.is_some());
            } else {
                assert_eq!(command.value["response"]["request_id"], native);
                command.delivered.unwrap().send(Ok(())).unwrap();
                operation.await.unwrap();
                assert_eq!(delivery(&id), Some(RequestDelivery::Sent));
            }
            if native != "cancelled" {
                assert!(
                    router
                        .claim_response(connection.id(), &id, &answer)
                        .is_err()
                );
            }
            router
                .resolve_native_request(instance, &serde_json::json!(native))
                .unwrap();
            assert_eq!(delivery(&id), None);
        }
    }

    #[tokio::test]
    async fn answer_preflight_failures_keep_awaiting_and_prove_non_delivery() {
        use super::*;
        use agent_protocol::requests::{Answer, ElicitationAnswer};
        for case in [
            "invalidClaudeId",
            "closedClaude",
            "unavailableCodex",
            "stoppedCodex",
        ] {
            let root = tempfile::tempdir().unwrap();
            let service = HostRpcService::new(
                Err("unavailable".into()),
                ProjectStore::new(root.path().join("worktrees.json")),
            )
            .unwrap();
            let connection = service.open_session();
            let claude = case.ends_with("Claude") || case == "invalidClaudeId";
            let provider = if claude {
                ProviderKind::Claude
            } else {
                ProviderKind::Codex
            };
            if claude {
                service
                    .enable_claude(
                        root.path().join("unused-cli"),
                        root.path().join("claude"),
                        Some(root.path().join("native")),
                    )
                    .await
                    .unwrap();
            }
            let target =
                agent_protocol::session::SessionRef::new(provider, "native".into()).unwrap();
            let target = service
                .inner
                .conversations
                .bind(&target, &service.storage_scope(target.provider).unwrap())
                .unwrap();
            let (input, mut receiver) = tokio::sync::mpsc::channel(1);
            let stopped = tokio_util::sync::CancellationToken::new();
            let adapted = if claude {
                super::super::requests::claude("request".into(), &"turn".into(), &serde_json::json!({"subtype":"elicitation","requested_schema":{"type":"object","properties":{}}}))
            } else {
                super::super::requests::codex("request".into(),"mcpServer/elicitation/request",&serde_json::json!({"mode":"form","requestedSchema":{"type":"object","properties":{}}}))
            }.unwrap();
            let id = adapted.request.id.clone();
            let instance = uuid::Uuid::new_v4();
            let native_id = if case == "invalidClaudeId" {
                serde_json::json!(1)
            } else {
                serde_json::json!("native-request")
            };
            let origin = if claude {
                crate::claude::request_origin(instance, native_id.clone(), input, adapted.answers)
            } else {
                super::super::requests::unavailable_origin(
                    instance,
                    native_id.clone(),
                    stopped.clone(),
                )
            };
            service
                .inner
                .router
                .request(target.clone(), origin, adapted.request)
                .unwrap();
            if case == "closedClaude" {
                receiver.close();
            }
            if case == "stoppedCodex" {
                stopped.cancel();
            }
            for _ in 0..2 {
                let failure = service
                    .answer_request(
                        connection.id(),
                        id.clone(),
                        Answer::Elicitation {
                            action: ElicitationAnswer::Accept {
                                values: serde_json::json!({}),
                            },
                        },
                    )
                    .await
                    .unwrap_err();
                assert_eq!(
                    failure.delivery,
                    agent_transport::peer::Delivery::NotSent,
                    "{case}"
                );
                let thread = service
                    .inner
                    .router
                    .overlay_execution(&target, Thread::default());
                assert_eq!(
                    thread.requests[&id].delivery,
                    agent_protocol::session::RequestDelivery::Awaiting,
                    "{case}"
                );
                assert!(receiver.try_recv().is_err());
            }
            service
                .inner
                .router
                .resolve_native_request(instance, &native_id)
                .unwrap();
        }
    }
    #[test]
    fn unknown_and_native_methods_are_rejected_before_dispatch() {
        for method in [
            "not/public",
            "provider",
            "thread/list",
            "thread/read",
            "thread/resume",
            "turn/start",
            "turn/steer",
            "thread/queue/add",
            "account/read",
        ] {
            for params in [
                serde_json::json!({}),
                serde_json::json!({"threadId":{"provider":"claude","id":"native"}}),
            ] {
                assert!(
                    agent_protocol::protocol::json_boundary::call(method, params).is_err(),
                    "{method}"
                );
            }
        }
    }
    #[tokio::test]
    async fn provider_capabilities_are_checked_before_provider_availability() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("not available".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        )
        .unwrap();
        service
            .enable_claude(
                root.path().join("missing-claude"),
                root.path().join("claude"),
                Some(root.path().join("native")),
            )
            .await
            .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &agent_protocol::session::SessionRef {
                    provider: ProviderKind::Claude,
                    id: "native".into(),
                },
                &service.storage_scope(ProviderKind::Claude).unwrap(),
            )
            .unwrap();
        let session = service.open_session();
        let renamed = service
            .dispatch(
                session.id(),
                &Call::RenameSession(op::RenameSession {
                    thread_id: target.clone(),
                    name: "Renamed".into(),
                }),
            )
            .await
            .unwrap();
        assert!(
            agent_protocol::protocol::decode::<Response<agent_protocol::models::Empty>>(
                &renamed.initial
            )
            .unwrap()
            .into_value()
            .get("error")
            .is_none()
        );
        for (method, expected) in [
            ("host/session/fork", "unsupported_operation"),
            ("host/session/submit", "provider_unavailable"),
        ] {
            let call = agent_protocol::protocol::json_boundary::call(method, serde_json::json!({"threadId":target,"clientUserMessageId":method,"lastTurnId":"turn","name":"Renamed","input":[],"expectedTurnId":"turn"})).unwrap();
            let response = service.dispatch(session.id(), &call).await.unwrap();
            let response = agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
            >(&response.initial)
            .unwrap()
            .into_value();
            assert_eq!(response["error"]["code"], expected, "{method}");
        }
    }

    #[tokio::test]
    async fn unavailable_provider_does_not_retain_a_submission_as_in_flight() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &agent_protocol::session::SessionRef {
                    provider: ProviderKind::Codex,
                    id: "native".into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        let session = service.open_session();
        for method in ["host/session/submit"] {
            let call = agent_protocol::protocol::json_boundary::call(
                method,
                serde_json::json!({"threadId":target,"clientUserMessageId":"input","input":[],"expectedTurnId":"turn"}),
            ).unwrap();
            for _ in 0..2 {
                let response = service.dispatch(session.id(), &call).await.unwrap();
                let response = agent_protocol::protocol::decode::<Response<()>>(&response.initial)
                    .unwrap()
                    .into_value();
                assert_eq!(response["error"]["code"], "provider_unavailable");
                assert_eq!(response["error"]["delivery"], "notSent");
            }
        }
    }

    #[tokio::test]
    async fn closing_the_submission_waiter_does_not_cancel_host_owned_execution() {
        use super::*;
        use futures_util::FutureExt;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("not-executable.test");
        std::fs::write(&program, []).unwrap();
        let service = HostRpcService::new(
            Err("not used".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
        )
        .unwrap();
        service
            .enable_claude(
                program,
                root.path().join("claude-state"),
                Some(root.path().join("native")),
            )
            .await
            .unwrap();
        let created = service
            .create_session(op::CreateSession {
                provider: ProviderKind::Claude,
                cwd: Some(root.path().to_string_lossy().into_owned()),
                model: None,
            })
            .await
            .unwrap();
        let target = created.thread.id.unwrap();
        let input = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "detached".into(),
            input: vec![op::Input::Text {
                text: "hello".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        let connection = service.open_session();
        let held = service
            .inner
            .router
            .submission_lock(&target)
            .lock_owned()
            .await;
        let call = Call::Submit(input.clone());
        let mut waiting = Box::pin(service.dispatch(connection.id(), &call));
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(waiting);
        drop(connection);
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if service
                    .inner
                    .conversations
                    .previous_command(&input)
                    .unwrap()
                    == Some(agent_protocol::session::SubmissionDelivery::Rejected)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached execution did not commit its provider preflight failure");
        let response = service
            .inner
            .conversations
            .open_thread(&target, 5, false)
            .unwrap();
        assert_eq!(
            response.thread.submissions.get("detached"),
            Some(&agent_protocol::session::SubmissionDelivery::Rejected)
        );
    }

    #[tokio::test]
    async fn first_import_preserves_claude_history_without_starting_a_cli() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native");
        let project = native.join("projects/example");
        std::fs::create_dir_all(&project).unwrap();
        let id = "12345678-1234-4234-8234-123456789abc";
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            include_str!("../../tests/fixtures/claude-2.1.266.jsonl"),
        )
        .unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        )
        .unwrap();
        service
            .enable_claude(
                root.path().join("does-not-exist"),
                root.path().join("state"),
                Some(native),
            )
            .await
            .unwrap();
        service.initial_import().await;
        let target = service
            .inner
            .conversations
            .bind(
                &agent_protocol::session::SessionRef {
                    provider: ProviderKind::Claude,
                    id: id.into(),
                },
                &service.storage_scope(ProviderKind::Claude).unwrap(),
            )
            .unwrap();
        let session = service.open_session();
        let call =
            agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                include_activity: false,
                session: target,
                limit: 5,
            });
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert!(reply.get("error").is_none(), "{reply}");
        let thread = &reply["result"]["response"]["thread"];
        assert!(!thread["turns"].as_array().unwrap().is_empty());
        assert_ne!(thread["historyReadState"]["type"], "unavailable");
        let transcript = project.join(format!("{id}.jsonl"));
        let changed = std::fs::read_to_string(&transcript)
            .unwrap()
            .replace("Fixture user input", "Changed outside Bex");
        std::fs::write(transcript, changed).unwrap();
        // The imported Host history is authoritative after the first import.
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value()
        .to_string();
        assert!(!reply.contains("Changed outside Bex"), "{reply}");
        assert!(reply.contains("Fixture user input"));
    }

    #[test]
    fn storage_scope_has_a_fixed_encoding() {
        let areas = [
            (ProviderKind::Codex, "/fixtures/codex".into()),
            (ProviderKind::Claude, "/fixtures/claude".into()),
        ];
        assert_eq!(
            super::provider_storage_scope(areas).unwrap(),
            "8170d6192a00957e1b3b4a3da16963137015f8d36f1efaa8b8b347090f78476b",
        );
    }

    proptest::proptest! {
        #[test]
        fn storage_scope_ignores_input_order_but_tracks_provider_paths(
            codex in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
            claude in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
        ) {
            let codex = std::path::PathBuf::from(codex);
            let claude = std::path::PathBuf::from(claude);
            let original = super::provider_storage_scope([
                (ProviderKind::Codex, codex.clone()),
                (ProviderKind::Claude, claude.clone()),
            ]).unwrap();
            proptest::prop_assert_eq!(&original, &super::provider_storage_scope([
                (ProviderKind::Claude, claude.clone()),
                (ProviderKind::Codex, codex.clone()),
            ]).unwrap());
            for areas in [
                [(ProviderKind::Codex, codex.join("different")), (ProviderKind::Claude, claude.clone())],
                [(ProviderKind::Codex, codex.clone()), (ProviderKind::Claude, claude.join("different"))],
            ] {
                proptest::prop_assert_ne!(&original, &super::provider_storage_scope(areas).unwrap());
            }
            proptest::prop_assert_ne!(&original, &super::provider_storage_scope([
                (ProviderKind::Codex, codex),
            ]).unwrap());
        }
    }

    #[test]
    fn creating_native_storage_does_not_change_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native").join("projects");
        let before = super::canonical_storage_path(&path);
        std::fs::create_dir_all(&path).unwrap();
        let scope = super::provider_storage_scope([(ProviderKind::Codex, before)]).unwrap();
        assert_eq!(
            scope,
            super::provider_storage_scope([(
                ProviderKind::Codex,
                super::canonical_storage_path(&path)
            ),])
            .unwrap()
        );
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            assert_eq!(
                scope,
                super::provider_storage_scope([(
                    ProviderKind::Codex,
                    super::canonical_storage_path(&alias.join("native/projects"))
                ),])
                .unwrap()
            );
        }
    }
}
