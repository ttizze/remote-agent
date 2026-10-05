use agent_protocol::session::ProviderKind;
use std::sync::{Arc, OnceLock};

use agent_protocol::operations as op;

use agent_protocol::models::ListQuery;

use agent_protocol::models::ThreadResponse;

use agent_protocol::protocol::{Body, Call, Response};
use agent_transport::peer::RpcMessageError;
use codex_app_server::CodexAppServer;
use serde::Serialize;

use super::agent::{Agent, Identity, session_pages};
use super::conversations::{Conversations, NativeIdentity};
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
    initial_import: OnceLock<()>,
    catalog_import: Arc<tokio::sync::Mutex<()>>,
    catalog_errors: std::sync::RwLock<HashMap<ProviderKind, Failure>>,
    body_imports: std::sync::Mutex<HashMap<ProviderKind, bool>>,
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
        codex_home: Option<std::path::PathBuf>,
    ) -> anyhow::Result<Self> {
        let conversations = Arc::new(Conversations::open(
            &projects.path().with_file_name("bex-conversations.sqlite"),
        )?);
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let adapter = Arc::new(super::codex::Codex::new(codex.clone(), codex_home));
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
                catalog_errors: Default::default(),
                body_imports: Default::default(),
                event_pumps: Default::default(),
                files,
            }),
        })
    }

    fn storage_scope(&self, provider: ProviderKind) -> Result<String, Failure> {
        native_storage_scope(&canonical_storage_path(
            self.agent(provider)?.storage_directory(),
        ))
        .map_err(|error| Failure::new("provider_storage_unavailable", error))
    }

    fn native_session(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<NativeIdentity, Failure> {
        let provider = self
            .inner
            .conversations
            .provider(target)
            .map_err(|error| Failure::new("invalid_session", error))?;
        self.inner
            .conversations
            .native(target, &self.storage_scope(provider)?)
            .map_err(|error| Failure::new("invalid_session", error))
    }

    fn normalize_item(
        &self,
        provider: ProviderKind,
        item: &mut agent_protocol::models::Item,
    ) -> Result<(), Failure> {
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
                    .bind(
                        &NativeIdentity {
                            provider,
                            id: native.id.clone(),
                        },
                        &self.storage_scope(provider)?,
                    )
                    .map_err(|error| Failure::new("invalid_session", error))?;
            }
        }
        Ok(())
    }

    async fn hydrate_history(
        &self,
        agent: &dyn Agent,
        native: &NativeIdentity,
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
                    *item = Arc::new(agent.read_item(&native.id, &turn.id, &item.id).await?.item);
                }
                self.normalize_item(native.provider, Arc::make_mut(item))?;
            }
        }
        Ok(turns)
    }

    async fn import_conversation(
        &self,
        target: &agent_protocol::session::SessionRef,
        max_pages: usize,
    ) -> Result<(), Failure> {
        let lease = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let importing = self.inner.router.import_lock(target).lock_owned().await;
        self.import_locked(target, max_pages, lease, importing)
            .await
    }

    async fn import_locked(
        &self,
        target: &agent_protocol::session::SessionRef,
        max_pages: usize,
        _lease: super::routing::SessionLease,
        _importing: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<(), Failure> {
        let native = self.native_session(target)?;
        let agent = self.agent(native.provider)?;
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
                if response.thread.id.as_ref().map(|id| id.id.as_str()) != Some(native.id.as_str())
                {
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
            self.history_changed(target);
        }
        Ok(())
    }

    fn start_history_import(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<(), Failure> {
        let lease = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let Ok(importing) = self.inner.router.import_lock(target).try_lock_owned() else {
            return Ok(());
        };
        let service = self.clone();
        let target = target.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .import_locked(&target, usize::MAX, lease, importing)
                .await
            {
                service.record_import_failure(&target, &error);
            }
        });
        Ok(())
    }

    fn record_import_failure(&self, target: &agent_protocol::session::SessionRef, error: &Failure) {
        if let Err(commit) = self
            .inner
            .conversations
            .import_failed(target, &error.to_string())
        {
            tracing::error!(target: "bex", operation = "history.import_commit", message = %commit);
        }
        tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
        self.history_changed(target);
    }

    async fn import_provider(&self, provider: ProviderKind, agent: &dyn Agent) -> Option<Failure> {
        let result = async {
            let scope = self.storage_scope(provider)?;
            let pages = session_pages(agent, "");
            futures_util::pin_mut!(pages);
            while let Some(page) = pages.try_next().await? {
                self.inner
                    .conversations
                    .discover_page(provider, &page, &scope)
                    .map_err(|error| Failure::new("history_import_failed", error))?;
                self.inner
                    .router
                    .broadcast(agent_protocol::protocol::Notification::CatalogChanged {});
            }
            Ok::<_, Failure>(())
        }
        .await;
        let mut errors = self
            .inner
            .catalog_errors
            .write()
            .unwrap_or_else(|error| error.into_inner());
        match &result {
            Ok(()) => {
                errors.remove(&provider);
            }
            Err(error) => {
                errors.insert(provider, error.clone());
            }
        }
        self.inner
            .router
            .broadcast(agent_protocol::protocol::Notification::CatalogChanged {});
        self.import_bodies(provider);
        result.err()
    }

    fn initial_import(&self) {
        self.inner.initial_import.get_or_init(|| {
            // A manual scan already owns the first import if it acquired the
            // lock before startup. Acquire before spawning so the first list
            // response accurately reports import progress.
            if let Ok(catalog) = self.inner.catalog_import.clone().try_lock_owned() {
                let service = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = service.scan_import().await {
                        tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
                    }
                    drop(catalog);
                    service.inner.router.broadcast(
                        agent_protocol::protocol::Notification::CatalogChanged {},
                    );
                });
            }
        });
    }

    async fn refresh_import(&self) -> Result<(), Failure> {
        let catalog = self.inner.catalog_import.lock().await;
        let result = self.scan_import().await;
        drop(catalog);
        self.inner
            .router
            .broadcast(agent_protocol::protocol::Notification::CatalogChanged {});
        result
    }

    async fn scan_import(&self) -> Result<(), Failure> {
        let providers = self.agents();
        let failures = futures_util::future::join_all(
            providers
                .iter()
                .map(|(provider, agent)| self.import_provider(*provider, agent.as_ref())),
        )
        .await;
        failures.into_iter().flatten().next().map_or(Ok(()), Err)
    }

    fn import_bodies(&self, provider: ProviderKind) {
        {
            let mut workers = self
                .inner
                .body_imports
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(refresh) = workers.get_mut(&provider) {
                *refresh = true;
                return;
            }
            workers.insert(provider, false);
        }
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut after = 0;
            loop {
                let Some(current) = inner.upgrade() else {
                    return;
                };
                let service = Self { inner: current };
                let pending = service.storage_scope(provider).and_then(|scope| {
                    service
                        .inner
                        .conversations
                        .pending_imports(provider, &scope, after)
                        .map_err(|error| Failure::new("history_import_failed", error))
                });
                let pending = match pending {
                    Ok(pending) => pending,
                    Err(error) => {
                        service
                            .inner
                            .body_imports
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&provider);
                        tracing::warn!(target: "bex", operation = "history.import", code = error.code, message = %error);
                        return;
                    }
                };
                if pending.is_empty() {
                    let mut workers = service
                        .inner
                        .body_imports
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if workers.get_mut(&provider).is_some_and(std::mem::take) {
                        // Catalog refreshes during this pass require one more
                        // pass from the beginning, including earlier failures.
                        after = 0;
                        continue;
                    }
                    workers.remove(&provider);
                    return;
                }
                drop(service);
                for (position, target) in pending {
                    let Some(current) = inner.upgrade() else {
                        return;
                    };
                    let service = Self { inner: current };
                    if let Err(error) = service.import_conversation(&target, usize::MAX).await {
                        service.record_import_failure(&target, &error);
                    }
                    after = position;
                }
            }
        });
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
            .inner
            .catalog_errors
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(provider, error)| {
                (
                    provider_key(*provider),
                    serde_json::to_value(error).expect("failure serializes"),
                )
            })
            .collect();
        errors.extend(self.agents().into_iter().filter_map(|(p, a)| {
            a.availability().err().map(|e| {
                (
                    provider_key(p),
                    serde_json::to_value(e).expect("failure serializes"),
                )
            })
        }));
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
        self.initial_import();
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
            Call::Submit(input) | Call::QueueInput(input) => {
                // The Host owns execution after dispatch starts. Closing the RPC
                // drops only the reply waiter, never an admitted provider write.
                let service = self.clone();
                let force_queue = matches!(message, Call::QueueInput(_));
                let input = input.clone();
                tokio::spawn(async move { service.execute_submission(&input, force_queue).await })
                    .await
                    .map_err(|error| Failure::unknown("submission_outcome_unknown", error))
                    .and_then(|result| result)
                    .map(Into::into)
            }
            Call::SteerQueued(params) => {
                let service = self.clone();
                let params = params.clone();
                tokio::spawn(async move { service.steer_queued(&params).await })
                    .await
                    .map_err(|error| Failure::unknown("submission_outcome_unknown", error))
                    .and_then(|result| result)
                    .map(Into::into)
            }
            Call::Interrupt(params) => {
                let service = self.clone();
                let params = params.clone();
                tokio::spawn(async move { service.interrupt_session(&params).await })
                    .await
                    .map_err(|error| Failure::unknown("interrupt_outcome_unknown", error))
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
        force_queue: bool,
    ) -> Result<op::SubmissionReceipt, Failure> {
        use agent_protocol::session::SubmissionDelivery;
        let target = &input.thread_id;
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
            && delivery != SubmissionDelivery::Sending
        {
            return replay_submission(delivery);
        }
        let native = self.native_session(target)?;
        if input
            .model
            .as_ref()
            .is_some_and(|model| model.provider != native.provider || model.id.trim().is_empty())
        {
            return Err(Failure::new(
                "provider_mismatch",
                "別のプロバイダーのモデルを使う場合は新しい会話を作成してください。",
            ));
        }
        let _workspace = self.inner.worktree_access.read().await;
        let serial = self.inner.router.submission_lock(target);
        let mut _serial_guard = serial.clone().lock_owned().await;
        // A concurrent sender can observe the durable Sending receipt before
        // native IO completes. Wait for its session owner, then replay the
        // settled receipt instead of reporting its in-flight result as Unknown.
        if let Some(delivery) = self
            .inner
            .conversations
            .previous_command(input)
            .map_err(|error| Failure::new("invalid_input", error))?
        {
            return replay_submission(delivery);
        }
        let held = self
            .inner
            .conversations
            .queue_held(target)
            .map_err(|error| Failure::new("conversation_unavailable", error))?;
        let prepared = if force_queue || held {
            None
        } else {
            self.agent(native.provider)?.availability()?;
            // Complete the source snapshot before native execution can move its
            // cursors. Admission to a held queue requires no provider process.
            drop(_serial_guard);
            self.import_conversation(target, usize::MAX).await?;
            _serial_guard = serial.lock_owned().await;
            Some(self.prepare_input(target).await?)
        };
        let held = self
            .inner
            .conversations
            .queue_held(target)
            .map_err(|error| Failure::new("conversation_unavailable", error))?;
        let queued = held
            || prepared
                .as_ref()
                .is_none_or(|(route, _)| *route == super::submission::SubmissionTarget::Queue);
        if let Some(delivery) = self
            .inner
            .conversations
            .admit(
                input,
                if queued {
                    SubmissionDelivery::Queued
                } else {
                    SubmissionDelivery::Sending
                },
            )
            .map_err(|error| Failure::new("input_admission_failed", error))?
        {
            return replay_submission(delivery);
        }
        if queued {
            self.inner
                .router
                .publish_submission(
                    target,
                    input.client_user_message_id.clone(),
                    SubmissionDelivery::Queued,
                )
                .map_err(|error| Failure::unknown("input_admission_unknown", error))?;
            self.history_changed(target);
            self.wake_queue(target.clone());
            return Ok(op::SubmissionReceipt { turn_id: None });
        }
        let (route, reload) = prepared.expect("immediate input has an execution route");
        self.deliver_input(target, input, route, reload).await
    }

    async fn deliver_input(
        &self,
        target: &agent_protocol::session::SessionRef,
        input: &op::Submission,
        route: super::submission::SubmissionTarget,
        reload: bool,
    ) -> Result<op::SubmissionReceipt, Failure> {
        self.history_changed(target);
        let result = match self.inner.router.publish_submission(
            target,
            input.client_user_message_id.clone(),
            agent_protocol::session::SubmissionDelivery::Sending,
        ) {
            Ok(()) => self.submit_input(target, input, route, reload).await,
            Err(error) => Err(Failure::unknown("submission_outcome_unknown", error)),
        };
        self.finish_input(target, input.client_user_message_id.as_str(), &result)?;
        self.history_changed(target);
        result
    }

    async fn steer_queued(
        &self,
        params: &agent_protocol::queue::SteerQueued,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        use agent_protocol::session::SubmissionDelivery;
        let target = &params.session;
        let _execution = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_queue", error))?;
        self.native_session(target)?;
        let _workspace = self.inner.worktree_access.read().await;
        let _serial = self.inner.router.submission_lock(target).lock_owned().await;
        match self
            .inner
            .conversations
            .queue_receipt(target, &params.id)
            .map_err(|error| Failure::new("queue_read_failed", error))?
        {
            Some(SubmissionDelivery::Queued) => {}
            Some(SubmissionDelivery::Accepted { turn_id })
                if turn_id.as_ref() == Some(&params.turn_id) =>
            {
                return Ok(agent_protocol::models::Empty {});
            }
            Some(SubmissionDelivery::Sending | SubmissionDelivery::Unknown) => {
                return Err(Failure::unknown(
                    "submission_outcome_unknown",
                    "queued input delivery is uncertain; it cannot be resent",
                ));
            }
            _ => {
                return Err(Failure::new(
                    "invalid_queue",
                    "queued input is no longer available",
                ));
            }
        }
        let (route, reload) = self.prepare_input(target).await?;
        if !matches!(&route, super::submission::SubmissionTarget::Steer(turn) if turn == params.turn_id.as_str())
        {
            return Err(Failure::new(
                "steer_unavailable",
                "the observed turn is no longer available for steering",
            ));
        }
        let input = self
            .inner
            .conversations
            .claim_queued(target, Some(&params.id))
            .map_err(|error| Failure::new("queue_claim_failed", error))?
            .ok_or_else(|| Failure::new("invalid_queue", "queued input is no longer available"))?;
        self.deliver_input(target, &input, route, reload).await?;
        Ok(agent_protocol::models::Empty {})
    }

    fn finish_input(
        &self,
        target: &agent_protocol::session::SessionRef,
        id: &str,
        result: &Result<op::SubmissionReceipt, Failure>,
    ) -> Result<(), Failure> {
        use agent_protocol::session::SubmissionDelivery;
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
        Ok(())
    }

    async fn prepare_input(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<(super::submission::SubmissionTarget, bool), Failure> {
        let native = self.native_session(target)?;
        let agent = self.agent(native.provider)?;
        agent.availability()?;
        let state = agent.state(&native.id).await?;
        let mut response = state.response;
        if response.thread.id.as_ref().map(|id| id.id.as_str()) != Some(native.id.as_str()) {
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
            agent.capabilities().active_steering,
            response.thread.cwd.as_deref(),
        )
        .map_err(|e| Failure::new("submission_unavailable", e))?;
        Ok((route, state.needs_reload))
    }

    async fn submit_input(
        &self,
        target: &agent_protocol::session::SessionRef,
        input: &op::Submission,
        route: super::submission::SubmissionTarget,
        mut reload: bool,
    ) -> Result<op::SubmissionReceipt, Failure> {
        if let super::submission::SubmissionTarget::Start { cwd } = &route
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
        let native = self.native_session(target)?;
        self.agent(native.provider)?
            .submit(
                input,
                &native.id,
                route,
                reload,
                self.browser_config(&target.to_string())?,
            )
            .await
    }

    fn history_changed(&self, target: &agent_protocol::session::SessionRef) {
        self.inner
            .router
            .broadcast(agent_protocol::protocol::Notification::HistoryChanged {
                session: target.clone(),
            });
    }

    async fn interrupt_session(
        &self,
        params: &op::Interrupt,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let target = &params.thread_id;
        let _execution = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let _serial = self.inner.router.submission_lock(target).lock_owned().await;
        let native = self.native_session(target)?;
        let response = self
            .agent(native.provider)?
            .interrupt(&native.id, &params.turn_id)
            .await?;
        // Completion wakes a queue worker, but the serial guard keeps it from
        // claiming another input before this hold is committed.
        if self
            .inner
            .conversations
            .has_queued(target)
            .map_err(|error| Failure::new("queue_read_failed", error))?
        {
            self.inner
                .conversations
                .queue_control(target, &agent_protocol::queue::QueueAction::Pause)
                .map_err(|error| Failure::new("queue_pause_failed", error))?;
            self.history_changed(target);
        }
        Ok(response)
    }

    fn wake_queue(&self, target: agent_protocol::session::SessionRef) {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let Some(inner) = inner.upgrade() else { return };
            let service = Self { inner };
            if let Err(error) = service.deliver_queued(&target).await {
                let _serial = service
                    .inner
                    .router
                    .submission_lock(&target)
                    .lock_owned()
                    .await;
                if let Err(commit) = service
                    .inner
                    .conversations
                    .queue_control(&target, &agent_protocol::queue::QueueAction::Pause)
                {
                    tracing::error!(target: "bex", operation = "queue.pause_failed", message = %commit);
                }
                service.history_changed(&target);
                tracing::warn!(target: "bex", operation = "queue.delivery_failed", code = error.code, message = %error);
            }
        });
    }

    async fn deliver_queued(
        &self,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<(), Failure> {
        let _execution = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_queue", error))?;
        if self
            .inner
            .conversations
            .queue_held(target)
            .map_err(|error| Failure::new("queue_read_failed", error))?
            || !self
                .inner
                .conversations
                .has_queued(target)
                .map_err(|error| Failure::new("queue_read_failed", error))?
        {
            return Ok(());
        }
        let _workspace = self.inner.worktree_access.read().await;
        self.import_conversation(target, usize::MAX).await?;
        let _serial = self.inner.router.submission_lock(target).lock_owned().await;
        if !self
            .inner
            .conversations
            .has_queued(target)
            .map_err(|error| Failure::new("queue_read_failed", error))?
        {
            return Ok(());
        }
        let (route, reload) = self.prepare_input(target).await?;
        if !matches!(route, super::submission::SubmissionTarget::Start { .. }) {
            return Ok(());
        }
        let Some(input) = self
            .inner
            .conversations
            .claim_queued(target, None)
            .map_err(|error| Failure::new("queue_claim_failed", error))?
        else {
            return Ok(());
        };
        self.deliver_input(target, &input, route, reload)
            .await
            .map(|_| ())
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
            let provider = self.inner.conversations.provider(&target)?;
            let capabilities = self.agent(provider).map(|agent| host_capabilities(agent.capabilities())).unwrap_or_default();
            let limit = params.limit;
            let read = self
                .inner
                .router
                .retain_execution(target.clone())
                .map_err(anyhow::Error::msg)?;
            let started = std::time::Instant::now();
            let mut response = self.inner.conversations.open_thread(&target, limit, params.include_activity)?;
            if response.thread.history_read_state.as_ref().is_some_and(|state| state.kind == agent_protocol::session::HistoryReadKind::Importing) {
                self.start_history_import(&target)?;
            }
            let native_ms = started.elapsed().as_millis();
            if response.thread.id.as_ref() != Some(&target) {
                return Err(anyhow::anyhow!("conversation identity changed"));
            }
            response.thread.project_id = self.project_snapshot().await?.project_membership(response.thread.cwd.as_deref());
            response.thread.capabilities = Some(capabilities);
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
        if response.item.is_deferred() {
            let previous = response.item.clone();
            let native = self.native_session(target)?;
            let mut hydrated = self
                .agent(native.provider)?
                .read_item(&native.id, &params.turn_id, &params.item_id)
                .await?;
            self.normalize_item(native.provider, &mut hydrated.item)?;
            if !self
                .inner
                .router
                .hydrate_item(target, &params.turn_id, &previous, &hydrated.item)
                .map_err(|error| Failure::new("item_import_failed", error))?
            {
                return Err(Failure::new(
                    "item_changed",
                    "item changed while importing its body; read it again",
                ));
            }
            response = hydrated;
        }
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
        if let Some(target) = target_session
            && matches!(request, Call::ForkSession(_))
        {
            let native = self.native_session(target)?;
            let capabilities = self.agent(native.provider)?.capabilities();
            if !capabilities.fork {
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
            Call::SessionScope(_) => self
                .inner
                .conversations
                .storage_identity()
                .to_owned()
                .into(),
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
            Call::QueueControl(params) => {
                let _serial = self
                    .inner
                    .router
                    .submission_lock(&params.session)
                    .lock_owned()
                    .await;
                self.inner
                    .conversations
                    .queue_control(&params.session, &params.action)
                    .map_err(|error| Failure::new("queue_update_failed", error))?;
                self.history_changed(&params.session);
                self.wake_queue(params.session.clone());
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
            Call::SteerQueued(_) => unreachable!("the Host owns queued steering"),
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
                let native = self.native_session(target)?;
                let agent = self.agent(native.provider)?;
                self.start_thread(native.provider, agent.as_ref(), |browser| {
                    agent.fork(&native.id, &params.last_turn_id, browser)
                })
                .await?
                .into()
            }
            Call::RenameSession(params) => {
                let target = target_session.expect("session-scoped rename");
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
        for (provider, agent) in &agents {
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
                                        .bind(
                                            &NativeIdentity {
                                                provider: *provider,
                                                id: native.id.clone(),
                                            },
                                            &self.storage_scope(*provider)?,
                                        )
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
            for (provider, agent) in &agents {
                match agent.active_sessions_in(directory).await {
                    Ok(sessions) => {
                        for native in sessions {
                            active.insert(
                                self.inner
                                    .conversations
                                    .bind(
                                        &NativeIdentity {
                                            provider: *provider,
                                            id: native.id.clone(),
                                        },
                                        &self.storage_scope(*provider)?,
                                    )
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
                .agent(native.provider)?
                .state(&native.id)
                .await?
                .response;
            if response.thread.id.as_ref().map(|id| id.id.as_str()) != Some(native.id.as_str()) {
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
        self.initial_import();
        let snapshot = self.project_snapshot().await?;
        let providers = self.agents();
        let (mut page, mut branches) = self
            .inner
            .conversations
            .title_list(&snapshot, &query)
            .map_err(|error| Failure::new("sessions_unavailable", error))?;
        for thread in &mut page.data {
            if let Some(kind) = thread.provider
                && let Some((_, agent)) = providers.iter().find(|(provider, _)| *provider == kind)
            {
                thread.capabilities = Some(host_capabilities(agent.capabilities()));
            }
        }
        let provider_errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        page.importing = self.inner.catalog_import.try_lock().is_err();
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
        self.start_thread(provider, agent.as_ref(), |browser| {
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
        provider: ProviderKind,
        agent: &dyn Agent,
        start: impl FnOnce(Option<serde_json::Value>) -> F,
    ) -> Result<ThreadResponse, Failure>
    where
        F: std::future::Future<Output = Result<ThreadResponse, Failure>>,
    {
        let scope = uuid::Uuid::new_v4().to_string();
        let mut response = start(self.browser_config(&scope)?).await?;
        let native = NativeIdentity {
            provider,
            id: response
                .thread
                .id
                .as_ref()
                .ok_or_else(|| {
                    Failure::new("invalid_thread", "created thread identity is missing")
                })?
                .id
                .clone(),
        };
        let target = self
            .inner
            .conversations
            .bind(&native, &self.storage_scope(provider)?)
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
        response.thread.project_id = self
            .project_snapshot()
            .await?
            .project_membership(response.thread.cwd.as_deref());
        response.thread.capabilities = Some(host_capabilities(agent.capabilities()));
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
                            if let super::agent::AgentChange::Request { origin, .. } = &change
                                && origin.provider != provider
                            {
                                return Err("request source provider does not match adapter".into());
                            }
                            if let super::agent::AgentChange::Stopped {
                                provider: stopped, ..
                            } = &change
                                && *stopped != provider
                            {
                                return Err("stopped provider does not match adapter".into());
                            }
                            let renamed = match &change {
                                super::agent::AgentChange::Renamed { name, .. } => {
                                    Some(name.clone())
                                }
                                _ => None,
                            };
                            let native = match &mut change {
                                super::agent::AgentChange::Session { session, .. }
                                | super::agent::AgentChange::Request { session, .. }
                                | super::agent::AgentChange::Renamed { session, .. } => {
                                    Some(session)
                                }
                                _ => None,
                            };
                            if let Some(native) = native {
                                let target = service
                                    .inner
                                    .conversations
                                    .bind(
                                        &NativeIdentity {
                                            provider,
                                            id: native.id.clone(),
                                        },
                                        &service
                                            .storage_scope(provider)
                                            .map_err(|error| error.to_string())?,
                                    )
                                    .map_err(|error| error.to_string())?;
                                if let Some(name) = renamed {
                                    service
                                        .inner
                                        .conversations
                                        .rename(&target, &name, false)
                                        .map_err(|error| error.to_string())?;
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
                                        .normalize_item(provider, Arc::make_mut(item))
                                        .map_err(|error| error.to_string())?;
                                }
                            }
                            let finished = match &change {
                                super::agent::AgentChange::Session {
                                    session,
                                    change:
                                        agent_protocol::session::SessionChange::Turn {
                                            completed: true,
                                            ..
                                        }
                                        | agent_protocol::session::SessionChange::Status {
                                            status: agent_protocol::execution::SessionStatus::Idle,
                                        },
                                } => Some(session.clone()),
                                _ => None,
                            };
                            change.apply(&router)?;
                            if let Some(target) = finished {
                                service.wake_queue(target);
                            }
                            Ok(())
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
        SubmissionDelivery::Queued => Ok(op::SubmissionReceipt { turn_id: None }),
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

// Native source identity is separate from the Host database's identity.
// Provider/instance selection belongs to the binding, not this path hash.
fn native_storage_scope(directory: &std::path::Path) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(directory)?;
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
        Call::Submit(p) | Call::QueueInput(p) => {
            (Some(&p.thread_id), Some(p.client_user_message_id.as_str()))
        }
        Call::QueueControl(p) => (Some(&p.session), None),
        Call::SteerQueued(p) => (Some(&p.session), None),
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

fn host_capabilities(
    mut capabilities: agent_protocol::session::Capabilities,
) -> agent_protocol::session::Capabilities {
    capabilities.rename = true;
    capabilities
}

#[cfg(test)]
mod tests {
    use agent_protocol::models::Thread;
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

    #[tokio::test]
    async fn client_storage_scope_is_owned_by_the_database_not_provider_configuration() {
        use super::*;
        async fn scope(service: &HostRpcService) -> String {
            let session = service.open_session();
            let response = service
                .dispatch(
                    session.id(),
                    &Call::SessionScope(agent_protocol::models::Empty {}),
                )
                .await
                .unwrap();
            let Response::Success { result } =
                agent_protocol::protocol::decode::<Response<String>>(&response.initial).unwrap()
            else {
                panic!("storage identity request failed")
            };
            result
        }
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("worktrees.json");
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(projects.clone()),
            Some(root.path().join("codex-home")),
        )
        .unwrap();
        let original = scope(&service).await;
        service
            .enable_claude(
                root.path().join("missing-cli"),
                root.path().join("claude-state"),
                Some(root.path().join("claude-home")),
            )
            .await
            .unwrap();
        assert_eq!(scope(&service).await, original);
        drop(service);
        let restarted = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(projects.clone()),
            Some(root.path().join("different-codex-home")),
        )
        .unwrap();
        assert_eq!(scope(&restarted).await, original);
        drop(restarted);
        std::fs::remove_file(projects.with_file_name("bex-conversations.sqlite")).unwrap();
        let replaced = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(projects),
            Some(root.path().join("different-codex-home")),
        )
        .unwrap();
        assert_ne!(scope(&replaced).await, original);
    }

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
            Some(root.path().join("codex-native")),
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
            response.response.thread.provider,
            Some(ProviderKind::Claude)
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
            Some(root.path().join("codex-native")),
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
        let target = NativeIdentity {
            provider: ProviderKind::Claude,
            id: "native".into(),
        };
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
                            user: None,
                            value: serde_json::Value::Null,
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
                Some(root.path().join("codex-native")),
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
            let target = NativeIdentity {
                provider,
                id: "native".into(),
            };
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
                serde_json::json!({"threadId":{"id":"native"}}),
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
            Some(root.path().join("codex-native")),
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
                &NativeIdentity {
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
    async fn rejected_interrupt_keeps_waiting_inputs_and_queue_state_unchanged() {
        use super::*;
        use agent_protocol::session::SubmissionDelivery;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("not available".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
            Some(root.path().join("codex-native")),
        )
        .unwrap();
        service
            .enable_claude(
                root.path().join("missing-cli"),
                root.path().join("claude"),
                Some(root.path().join("native")),
            )
            .await
            .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Claude,
                    id: "source".into(),
                },
                &service.storage_scope(ProviderKind::Claude).unwrap(),
            )
            .unwrap();
        let input = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "waiting".into(),
            input: vec![op::Input::Text {
                text: "waiting input".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        service
            .inner
            .conversations
            .admit(&input, SubmissionDelivery::Queued)
            .unwrap();
        assert!(
            service
                .interrupt_session(&op::Interrupt {
                    thread_id: target.clone(),
                    turn_id: "stale".into(),
                })
                .await
                .is_err()
        );
        let thread = service
            .inner
            .conversations
            .open_thread(&target, 5, false)
            .unwrap()
            .thread;
        assert!(!thread.queue_held);
        assert_eq!(thread.queued_inputs.len(), 1);
        assert_eq!(thread.queued_inputs[0].submission, input);
        assert_eq!(thread.queued_inputs[0].delivery, SubmissionDelivery::Queued);
    }

    #[tokio::test]
    async fn concurrent_duplicate_waits_for_the_owner_and_replays_its_settled_receipt() {
        use super::*;
        use agent_protocol::session::SubmissionDelivery;
        use futures_util::FutureExt;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
            Some(root.path().join("codex-native")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Codex,
                    id: "source".into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        let input = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "concurrent".into(),
            input: vec![op::Input::Text {
                text: "send once".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        let owner = service
            .inner
            .router
            .submission_lock(&target)
            .lock_owned()
            .await;
        service
            .inner
            .conversations
            .admit(&input, SubmissionDelivery::Sending)
            .unwrap();
        let mut duplicate = Box::pin(service.execute_submission(&input, false));
        assert!(duplicate.as_mut().now_or_never().is_none());
        service
            .inner
            .router
            .finish_submission(
                &target,
                "concurrent",
                SubmissionDelivery::Accepted {
                    turn_id: Some("native-turn".into()),
                },
            )
            .unwrap();
        drop(owner);
        let replay = tokio::time::timeout(std::time::Duration::from_secs(1), duplicate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay.turn_id.as_deref(), Some("native-turn"));
        assert_eq!(
            service.execute_submission(&input, false).await.unwrap(),
            replay
        );
    }

    #[tokio::test]
    async fn queued_steering_replays_only_its_completed_target_without_redelivering_uncertain_input()
     {
        use super::*;
        use agent_protocol::{
            queue::{QueueAction, SteerQueued},
            session::SubmissionDelivery,
        };
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("not available".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
            Some(root.path().join("native")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Codex,
                    id: "source".into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        let _execution = service
            .inner
            .router
            .retain_execution(target.clone())
            .unwrap();
        service
            .inner
            .conversations
            .queue_control(&target, &QueueAction::Pause)
            .unwrap();
        let submission = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "queued".into(),
            input: vec![op::Input::Text {
                text: "preserve this input".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        service
            .inner
            .conversations
            .admit(&submission, SubmissionDelivery::Queued)
            .unwrap();
        let mut command = SteerQueued {
            session: target.clone(),
            id: "queued".into(),
            turn_id: "active".into(),
        };
        assert_eq!(
            service.steer_queued(&command).await.unwrap_err().code,
            "provider_unavailable"
        );
        assert_eq!(
            service.inner.conversations.queued(&target).unwrap(),
            std::slice::from_ref(&submission)
        );
        for delivery in [SubmissionDelivery::Sending, SubmissionDelivery::Unknown] {
            service
                .inner
                .router
                .finish_submission(&target, "queued", delivery)
                .unwrap();
            assert_eq!(
                service.steer_queued(&command).await.unwrap_err().delivery,
                agent_protocol::error::Delivery::Unknown
            );
        }
        service
            .inner
            .router
            .finish_submission(
                &target,
                "queued",
                SubmissionDelivery::Accepted {
                    turn_id: Some("active".into()),
                },
            )
            .unwrap();
        service.steer_queued(&command).await.unwrap();
        command.turn_id = "different".into();
        assert_eq!(
            service.steer_queued(&command).await.unwrap_err().code,
            "invalid_queue"
        );
        command.id = "missing".into();
        assert_eq!(
            service.steer_queued(&command).await.unwrap_err().code,
            "invalid_queue"
        );
        assert!(service.inner.conversations.queue_held(&target).unwrap());
    }

    #[tokio::test]
    async fn held_queue_admits_once_without_a_provider_process() {
        use super::*;
        use agent_protocol::{queue::QueueAction, session::SubmissionDelivery};
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("not available".into()),
            ProjectStore::new(root.path().join("worktrees.json")),
            Some(root.path().join("codex-native")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Codex,
                    id: "source".into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        service
            .inner
            .conversations
            .queue_control(&target, &QueueAction::Pause)
            .unwrap();
        let input = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "held".into(),
            input: vec![op::Input::Text {
                text: "save until the provider recovers".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        for _ in 0..2 {
            assert!(
                service
                    .execute_submission(&input, false)
                    .await
                    .unwrap()
                    .turn_id
                    .is_none()
            );
        }
        let thread = service
            .inner
            .conversations
            .open_thread(&target, 5, false)
            .unwrap()
            .thread;
        assert!(thread.queue_held);
        assert_eq!(thread.queued_inputs.len(), 1);
        assert_eq!(thread.queued_inputs[0].submission, input);
        assert_eq!(thread.queued_inputs[0].delivery, SubmissionDelivery::Queued);
        assert!(thread.turns.unwrap_or_default().is_empty());
        let mut mismatched = input;
        mismatched.client_user_message_id = "mismatched".into();
        mismatched.model = Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Claude,
            id: "default".into(),
        });
        assert_eq!(
            service
                .execute_submission(&mismatched, true)
                .await
                .unwrap_err()
                .code,
            "provider_mismatch"
        );
    }

    #[tokio::test]
    async fn unavailable_provider_does_not_retain_a_submission_as_in_flight() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
            Some(root.path().join("codex-native")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
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
            Some(root.path().join("codex-native")),
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
    async fn persisted_conversation_reads_edits_and_receipts_do_not_require_its_native_home() {
        use super::*;
        use agent_protocol::{
            models::{Turn, TurnStatus},
            session::{HistoryPage, SubmissionDelivery},
        };
        async fn call<T: serde::de::DeserializeOwned>(
            service: &HostRpcService,
            connection: &HostSession,
            request: Call,
        ) -> T {
            let reply = service.dispatch(connection.id(), &request).await.unwrap();
            let Response::Success { result } =
                agent_protocol::protocol::decode::<Response<T>>(&reply.initial).unwrap()
            else {
                panic!("stored conversation operation failed");
            };
            result
        }
        let root = tempfile::tempdir().unwrap();
        let projects = ProjectStore::new(root.path().join("worktrees.json"));
        let service = HostRpcService::new(
            Err("unavailable".into()),
            projects.clone(),
            Some(root.path().join("first-home")),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Codex,
                    id: "native".into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        service
            .inner
            .conversations
            .import_page(
                &target,
                Some(&ThreadResponse {
                    thread: Thread {
                        id: Some(agent_protocol::session::SessionRef {
                            id: "native".into(),
                        }),
                        provider: Some(ProviderKind::Claude),
                        ..Default::default()
                    },
                    model: None,
                }),
                &HistoryPage {
                    turns: (0..8)
                        .map(|index| {
                            Arc::new(Turn {
                                id: format!("turn-{index}").into(),
                                status: TurnStatus::Completed,
                                items: Some(Vec::new()),
                                ..Default::default()
                            })
                        })
                        .collect(),
                    next_cursor: None,
                },
            )
            .unwrap();
        let completed = op::Submission {
            thread_id: target.clone(),
            client_user_message_id: "completed".into(),
            input: vec![op::Input::Text {
                text: "once".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        service
            .inner
            .conversations
            .admit(
                &completed,
                SubmissionDelivery::Accepted {
                    turn_id: Some("turn-7".into()),
                },
            )
            .unwrap();
        let waiting = op::Submission {
            client_user_message_id: "waiting".into(),
            ..completed.clone()
        };
        service
            .inner
            .conversations
            .admit(&waiting, SubmissionDelivery::Queued)
            .unwrap();
        drop(service);
        let service = HostRpcService::new(
            Err("unavailable".into()),
            projects,
            Some(root.path().join("different-home")),
        )
        .unwrap();
        assert!(service.native_session(&target).is_err());
        let connection = service.open_session();
        let list: agent_protocol::models::ThreadList = call(
            &service,
            &connection,
            Call::ListSessions(op::ListSessions::new(Default::default())),
        )
        .await;
        assert_eq!(list.data[0].id.as_ref(), Some(&target));
        assert_eq!(list.data[0].provider, Some(ProviderKind::Codex));
        let opened: agent_protocol::session::OpenedSession = call(
            &service,
            &connection,
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: target.clone(),
                limit: 2,
                include_activity: true,
            }),
        )
        .await;
        assert_eq!(opened.response.thread.turns.as_ref().unwrap().len(), 2);
        let page: HistoryPage = call(
            &service,
            &connection,
            Call::ReadHistory(agent_protocol::session::ReadHistory {
                session: target.clone(),
                cursor: opened.response.thread.history_cursor.unwrap(),
                include_activity: true,
            }),
        )
        .await;
        assert!(!page.turns.is_empty());
        let _: agent_protocol::models::Empty = call(
            &service,
            &connection,
            Call::RenameSession(op::RenameSession {
                thread_id: target.clone(),
                name: "Saved title".into(),
            }),
        )
        .await;
        let _: agent_protocol::models::Empty = call(
            &service,
            &connection,
            Call::QueueControl(agent_protocol::queue::QueueControl {
                session: target.clone(),
                action: agent_protocol::queue::QueueAction::Pause,
            }),
        )
        .await;
        assert!(service.inner.conversations.queue_held(&target).unwrap());
        let receipt: op::SubmissionReceipt =
            call(&service, &connection, Call::Submit(completed)).await;
        assert_eq!(receipt.turn_id.as_deref(), Some("turn-7"));
        assert_eq!(
            service
                .inner
                .conversations
                .open_thread(&target, 2, false)
                .unwrap()
                .thread
                .name
                .as_deref(),
            Some("Saved title")
        );
    }

    #[tokio::test]
    async fn stored_lists_do_not_wait_for_a_catalog_scan_and_expose_its_progress() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("codex-native"), "not a storage directory").unwrap();
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
            Some(root.path().join("codex-native")),
        )
        .unwrap();
        let source = agent_protocol::session::SessionRef { id: "saved".into() };
        service
            .inner
            .conversations
            .discover_page(
                ProviderKind::Codex,
                &[super::super::agent::SessionSummary {
                    thread: Thread {
                        id: Some(source),
                        name: Some("Saved before startup".into()),
                        ..Default::default()
                    },
                    branch: None,
                }],
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        let catalog = service.inner.catalog_import.lock().await;
        let page = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.host_title_list(Default::default()),
        )
        .await
        .expect("a list must not await a blocked catalog scan")
        .unwrap();
        assert!(page.importing);
        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].name.as_deref(), Some("Saved before startup"));
        drop(catalog);
        assert!(service.refresh_import().await.is_err());
        let page = service.host_title_list(Default::default()).await.unwrap();
        assert!(!page.importing);
        assert_eq!(page.data.len(), 1);
        assert!(page.provider_errors.unwrap().contains_key("codex"));
    }

    #[tokio::test]
    async fn first_import_reads_codex_without_app_server_and_keeps_the_host_history_authoritative()
    {
        use super::*;
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("codex-native");
        let sessions = native.join("sessions/2026/10/05");
        std::fs::create_dir_all(&sessions).unwrap();
        let native_id = "12345678-1234-4234-8234-123456789abc";
        let source = sessions.join(format!("rollout-2026-10-05T00-00-00-{native_id}.jsonl"));
        let bytes = [
            json!({"type":"session_meta","payload":{"id":native_id,"cwd":"/fixture/project","history_mode":"legacy"}}),
            json!({"type":"turn_context","payload":{"model":"fixture-codex"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"native-turn"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"Saved Codex input"}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"Saved Codex answer","phase":"final_answer"}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"native-turn"}}),
        ].into_iter().map(|record| format!("{record}\n")).collect::<String>();
        std::fs::write(&source, &bytes).unwrap();
        let service = HostRpcService::new(
            Err("missing app-server".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
            Some(native),
        )
        .unwrap();
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Codex,
                    id: native_id.into(),
                },
                &service.storage_scope(ProviderKind::Codex).unwrap(),
            )
            .unwrap();
        let serial = service
            .inner
            .router
            .submission_lock(&target)
            .lock_owned()
            .await;
        service.initial_import();
        drop(service.inner.catalog_import.lock().await);
        assert!(service.inner.catalog_errors.read().unwrap().is_empty());
        let titles = service.host_title_list(Default::default()).await.unwrap();
        assert_eq!(titles.data.len(), 1);
        assert_eq!(titles.data[0].name.as_deref(), Some("Saved Codex input"));
        let session = service.open_session();
        let call = Call::OpenSession(agent_protocol::session::OpenSession {
            include_activity: true,
            session: target.clone(),
            limit: 5,
        });
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.dispatch(session.id(), &call),
        )
        .await
        .expect("metadata opens without waiting for source IO")
        .unwrap();
        let reply = agent_protocol::protocol::decode::<
            Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert_eq!(
            reply["result"]["response"]["thread"]["historyReadState"]["type"],
            "importing"
        );
        drop(serial);
        service
            .import_conversation(&target, usize::MAX)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&source).unwrap(), bytes);
        std::fs::remove_file(source).unwrap();
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert!(reply.get("error").is_none(), "{reply}");
        assert_eq!(reply["result"]["response"]["model"]["id"], "fixture-codex");
        assert_eq!(
            reply["result"]["response"]["thread"]["turns"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(reply.to_string().contains("Saved Codex answer"));
        assert!(service.provider_errors().get("codex").is_some());
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
            Some(root.path().join("codex-native")),
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
        let target = service
            .inner
            .conversations
            .bind(
                &NativeIdentity {
                    provider: ProviderKind::Claude,
                    id: id.into(),
                },
                &service.storage_scope(ProviderKind::Claude).unwrap(),
            )
            .unwrap();
        let serial = service
            .inner
            .router
            .submission_lock(&target)
            .lock_owned()
            .await;
        service.initial_import();
        drop(service.inner.catalog_import.lock().await);
        assert!(service.inner.catalog_errors.read().unwrap().is_empty());
        let session = service.open_session();
        let call =
            agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                include_activity: false,
                session: target.clone(),
                limit: 5,
            });
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.dispatch(session.id(), &call),
        )
        .await
        .expect("opening committed metadata must not wait for source history")
        .unwrap();
        let opened = agent_protocol::protocol::decode::<
            Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert_eq!(
            opened["result"]["response"]["thread"]["historyReadState"]["type"],
            "importing"
        );
        drop(serial);
        service
            .import_conversation(&target, usize::MAX)
            .await
            .unwrap();
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
    fn creating_native_storage_does_not_change_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native").join("projects");
        let before = super::canonical_storage_path(&path);
        std::fs::create_dir_all(&path).unwrap();
        let scope = super::native_storage_scope(&before).unwrap();
        assert_eq!(
            scope,
            super::native_storage_scope(&super::canonical_storage_path(&path)).unwrap()
        );
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            assert_eq!(
                scope,
                super::native_storage_scope(&super::canonical_storage_path(
                    &alias.join("native/projects")
                ))
                .unwrap()
            );
        }
    }
}
