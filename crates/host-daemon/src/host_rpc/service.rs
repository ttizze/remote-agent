use agent_protocol::session::ProviderKind;
use std::sync::{Arc, OnceLock};

use agent_protocol::operations as op;

use agent_protocol::models::ListQuery;

use agent_protocol::models::Thread;

use agent_protocol::models::ThreadResponse;

use crate::adapters::Backend;
use agent_protocol::protocol::{Body, Call, Response};
use agent_transport::peer::RpcMessageError;
use serde::Serialize;

use super::agent::{Agent, Identity, SessionSummary, session_pages};
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
    agents: HashMap<ProviderKind, Arc<dyn Agent>>,
    startup_errors: HashMap<ProviderKind, Failure>,
    projects: ProjectStore,
    router: SessionRouter,
    event_pumps: std::sync::Once,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
}

impl HostRpcService {
    pub async fn enable_apns(&self, path: &std::path::Path, host_name: &str) -> anyhow::Result<()> {
        self.inner
            .router
            .set_apns(crate::apns::Apns::load(path, host_name).await?);
        Ok(())
    }
    pub fn new(backends: impl IntoIterator<Item = Backend>, projects: ProjectStore) -> Self {
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let mut agents = HashMap::new();
        let mut startup_errors = HashMap::new();
        let mut dictation = crate::dictation::Dictation::default();
        for backend in backends {
            match backend.agent {
                Ok(agent) => {
                    agents.insert(backend.provider, agent);
                }
                Err(error) => {
                    startup_errors.insert(backend.provider, error);
                }
            }
            if let Some(backend) = backend.dictation {
                dictation = backend;
            }
        }
        Self {
            inner: Arc::new(ServiceInner {
                browser: OnceLock::new(),
                agents,
                startup_errors,
                dictation,
                worktrees: crate::worktrees::Worktrees::new(projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                permission_settings_access: Default::default(),
                terminals: Default::default(),
                projects,
                router: SessionRouter::new(),
                event_pumps: std::sync::Once::new(),
                files,
            }),
        }
    }

    fn agent(&self, provider: ProviderKind) -> Result<Arc<dyn Agent>, Failure> {
        self.inner.agents.get(&provider).cloned().ok_or_else(|| {
            self.inner
                .startup_errors
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
        self.inner.router.revoke_device(principal);
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
            .map(|(_, agent)| agent.data_recipient().into())
            .collect();
        (ai, self.inner.dictation.data_recipient())
    }
    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        let mut errors: serde_json::Map<_, _> = self
            .agents()
            .into_iter()
            .filter_map(|(p, a)| {
                a.availability().err().map(|e| {
                    (
                        p.key().to_owned(),
                        serde_json::to_value(e).expect("failure serializes"),
                    )
                })
            })
            .collect();
        errors.extend(self.inner.startup_errors.iter().map(|(provider, error)| {
            (
                provider.key().to_owned(),
                serde_json::to_value(error).expect("failure serializes"),
            )
        }));
        serde_json::Value::Object(errors)
    }

    pub fn start(&self) {
        self.start_event_pumps();
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
        let result = async {
            let (target, input_id) = session_target(message);
            let target = target.cloned();
            let submission = target.as_ref().zip(input_id);
            // Keep the execution alive and serialize decisions through the
            // provider acknowledgement, including concurrent client inputs.
            let _read = submission
                .map(|(target, _)| self.inner.router.retain_execution(target.clone()))
                .transpose()
                .map_err(|error| Failure::new("invalid_params", error))?;
            let _serial = if let Some((target, _)) = submission {
                Some(self.inner.router.submission_lock(target).lock_owned().await)
            } else {
                None
            };
            let _workspace_read = if matches!(
                message,
                Call::Submit(_)
                    | Call::ForkSession(_)
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
            if let Some((target, id)) = submission {
                if matches!(message, Call::Submit(_))
                    && let Some(receipt) = self.inner.router.submission_receipt(target, id)
                {
                    return Ok(receipt.into());
                }
                self.inner.router.begin_submission(target, id)?;
            }
            let result = if let Call::Submit(input) = message {
                self.submit_input(target.as_ref().expect("submission target"), input)
                    .await
                    .map(Into::into)
            } else {
                self.request(session, message, target.as_ref()).await
            };
            if let Some((target, id)) = submission {
                use agent_protocol::session::SubmissionDelivery;
                let delivery = match &result {
                    Ok(Body::Submission(receipt)) => SubmissionDelivery::Accepted {
                        turn_id: receipt.turn_id.clone(),
                    },
                    Ok(_) => unreachable!("only submission returns delivery evidence"),
                    Err(error) if error.delivery == agent_transport::peer::Delivery::NotSent => {
                        SubmissionDelivery::Rejected
                    }
                    Err(_) => SubmissionDelivery::Unknown,
                };
                self.inner.router.finish_submission(target, id, delivery);
            }
            result
        }
        .await;
        if let Err(error) = &result {
            agent_transport::diagnostics::rpc_error(
                message.method(),
                None,
                &serde_json::value::to_raw_value(error).map_err(invalid_message)?,
            );
        }
        Ok(Response::from_result(result).into())
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
        let state = agent.state(&target.id).await?;
        let mut response = state.response;
        if response.thread.id.as_ref() != Some(target) {
            return Err(Failure::new(
                "invalid_session",
                "native session identity changed",
            ));
        }
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
        agent
            .submit(
                input,
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
            self.inner.router.response_delivery(&id, state)
        });
        let write = origin
            .source
            .prepare(&origin.native_id, &body, &answer)
            .await?;
        delivery.1 = RequestDelivery::Unknown;
        write.await?;
        delivery.1 = RequestDelivery::Sent;
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
            let mut response = agent.open(&target.id, limit, params.include_activity).await?;
            let native_ms = started.elapsed().as_millis();
            if response.thread.id.as_ref() != Some(&target) {
                return Err(anyhow::anyhow!("native session identity changed"));
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
            self.agent(target.provider)?.read_item(params).await?
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
            if matches!(request, Call::ForkSession(_)) && !capabilities.fork
                || matches!(request, Call::RenameSession(_)) && !capabilities.rename
            {
                return Err(Failure::new(
                    "unsupported_operation",
                    format!("{method} is unsupported by this provider"),
                ));
            }
        }
        let response = match request {
            Call::RegisterLiveActivity(params) => {
                params
                    .validate()
                    .map_err(|error| Failure::new("invalid_params", error))?;
                let enabled = self
                    .inner
                    .router
                    .apns()
                    .is_some_and(|apns| apns.environment() == params.environment);
                if enabled {
                    let _lease = self
                        .inner
                        .router
                        .retain_execution(params.session.clone())
                        .map_err(|error| Failure::new("invalid_params", error))?;
                    let thread = self
                        .agent(params.session.provider)?
                        .open(&params.session.id, 1, false)
                        .await?
                        .thread;
                    self.inner
                        .router
                        .register_live_activity(session, params, thread)
                        .map_err(|error| Failure::new("live_activity_failed", error))?;
                }
                agent_protocol::live_activity::LiveActivityRegistration { enabled }.into()
            }
            Call::UnregisterLiveActivity(params) => {
                let principal = self
                    .inner
                    .router
                    .principal(session)
                    .map_err(|error| Failure::new("connection_closed", error))?;
                if let Some(apns) = self.inner.router.apns() {
                    apns.unregister(&principal, &params.activity_id);
                }
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadTurnItems(params) => {
                let read = self
                    .inner
                    .router
                    .retain_execution(params.session.clone())
                    .map_err(|error| Failure::new("invalid_params", error))?;
                let items = self
                    .agent(params.session.provider)?
                    .read_turn_items(&params.session.id, &params.turn_id)
                    .await?;
                self.inner
                    .router
                    .finish_turn_read(read, session, params.turn_id.clone(), items)
                    .map_err(|error| Failure::new("turn_details_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadHistory(params) => {
                let mut page = self
                    .agent(params.session.provider)?
                    .read_history(&params.session.id, &params.cursor, params.include_activity)
                    .await?;
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
                for (provider, error) in self.inner.startup_errors.iter() {
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
                            .chain(self.inner.startup_errors.keys().copied())
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
                                .insert(provider.key().to_owned(), serde_json::to_value(error)?);
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
                self.start_thread(agent.as_ref(), |browser| {
                    agent.fork(&target.id, &params.last_turn_id, browser)
                })
                .await?
                .into()
            }
            Call::Interrupt(params) => {
                let target = target_session.expect("session-scoped interrupt");
                self.agent(target.provider)?
                    .interrupt(&target.id, &params.turn_id)
                    .await?
                    .into()
            }
            Call::RenameSession(params) => {
                let target = target_session.expect("session-scoped rename");
                let result = self
                    .agent(target.provider)?
                    .rename(&target.id, &params.name)
                    .await?;
                if let Some(apns) = self.inner.router.apns() {
                    apns.rename(target, &params.name);
                }
                result.into()
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
        for error in self.inner.startup_errors.values() {
            for worktree in &mut worktrees {
                worktree.blocked_reason =
                    Some(format!("稼働状況を確認できないため削除できません: {error}"));
            }
        }
        let mut threads = Vec::new();
        let agents = self.agents();
        for (_, agent) in &agents {
            let pages = session_pages(agent.as_ref(), "", None);
            futures_util::pin_mut!(pages);
            while let Some(result) = pages.next().await {
                match result {
                    Ok(page) => threads.extend(page.into_iter().map(|summary| summary.thread)),
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
                    Ok(sessions) => active.extend(sessions),
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
            let response = self
                .agent(target.provider)?
                .state(&target.id)
                .await?
                .response;
            if response.thread.id.as_ref() != Some(&target) {
                return Err(Failure::new(
                    "invalid_thread",
                    "native session identity changed",
                ));
            }
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
        let search = query.search_term.as_str();
        let agents = self.agents();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let (snapshot, mut listings) = tokio::join!(
            self.project_snapshot(),
            futures_util::future::join_all(agents.iter().map(|(provider, agent)| async move {
                let mut threads = session_pages(agent.as_ref(), search, None)
                    .map_ok(|page| futures_util::stream::iter(page.into_iter().map(Ok)))
                    .try_flatten()
                    .boxed();
                let head = next_title(&mut threads, deadline).await;
                (*provider, agent.capabilities(), threads, head)
            }))
        );
        let snapshot = snapshot?;
        let mut titles = crate::projects::titles::TitleList::new(&snapshot.projects, &query);
        let mut provider_errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut branches = std::collections::HashMap::new();
        let successful = listings.iter().any(|(_, _, _, head)| head.is_ok());
        while let Some(newest) = listings
            .iter()
            .filter_map(|(_, _, _, head)| head.as_ref().ok()?.as_ref())
            .map(|summary| summary.thread.updated_at.unwrap_or_default())
            .max_by(f64::total_cmp)
        {
            // Native pages guarantee descending timestamps, but equal timestamps
            // can span pages. Finish each tie before applying the stable ID order.
            let mut group = Vec::new();
            for (_, capabilities, threads, head) in &mut listings {
                while head.as_ref().is_ok_and(|thread| {
                    thread.as_ref().is_some_and(|summary| {
                        summary
                            .thread
                            .updated_at
                            .unwrap_or_default()
                            .total_cmp(&newest)
                            == std::cmp::Ordering::Equal
                    })
                }) {
                    group.push((head.as_mut().unwrap().take().unwrap(), *capabilities));
                    *head = next_title(threads, deadline).await;
                }
            }
            group.sort_by(|(a, _), (b, _)| a.thread.id.cmp(&b.thread.id));
            for (summary, capabilities) in group {
                let mut thread = summary.thread;
                if let (Some(id), Some(branch)) = (&thread.id, summary.branch) {
                    branches.insert(id.clone(), branch);
                }
                describe_thread(&mut thread, capabilities, &snapshot);
                titles.push(thread);
                if titles.complete() {
                    break;
                }
            }
            if titles.complete() {
                break;
            }
        }
        for (provider, _, _, head) in listings {
            if let Err(error) = head {
                provider_errors.insert(provider.key().to_owned(), serde_json::to_value(error)?);
            }
        }
        if !successful && !provider_errors.is_empty() {
            return Err(Failure::new(
                "sessions_unavailable",
                serde_json::to_value(provider_errors)?,
            ));
        }
        let mut page = titles.finish();
        // The recent page can end before a visible root's older descendants.
        let descendants = futures_util::future::join_all(page.data.iter().filter_map(|thread| {
            let id = thread.id.as_ref()?;
            if thread.parent_id.is_some() || id.provider != ProviderKind::Codex {
                return None;
            }
            let (_, agent) = agents
                .iter()
                .find(|(provider, _)| *provider == id.provider)?;
            Some(async move {
                let mut threads = session_pages(agent.as_ref(), "", Some(&id.id))
                    .map_ok(|page| futures_util::stream::iter(page.into_iter().map(Ok)))
                    .try_flatten()
                    .boxed();
                let mut children = Vec::new();
                loop {
                    match next_title(&mut threads, deadline).await {
                        Ok(Some(summary)) => children.push(summary),
                        Ok(None) => return (children, agent.capabilities(), None),
                        Err(error) => return (children, agent.capabilities(), Some(error)),
                    }
                }
            })
        }))
        .await;
        let mut children = Vec::new();
        for (summaries, capabilities, error) in descendants {
            if let Some(error) = error {
                provider_errors.insert("codex".into(), serde_json::to_value(error)?);
            }
            for summary in summaries {
                let mut thread = summary.thread;
                if let (Some(id), Some(branch)) = (&thread.id, summary.branch) {
                    branches.insert(id.clone(), branch);
                }
                describe_thread(&mut thread, capabilities, &snapshot);
                children.push(crate::projects::titles::summary(thread));
            }
        }
        children.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        page.data = crate::projects::titles::append_descendants(page.data, children);
        page.projects = tokio::task::spawn_blocking(move || {
            let mut projects = page.projects;
            for project in &mut projects {
                project.favicon_png = project.roots.iter().find_map(|root| {
                    crate::projects::icons::resolve(std::path::Path::new(&root.path))
                });
            }
            projects
        })
        .await
        .map_err(|error| Failure::new("project_icons_unavailable", error))?;
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
        self.inner.event_pumps.call_once(|| {
            for (_, agent) in self.agents() {
                if let Some(mut events) = agent.event_stream() {
                    let router = self.inner.router.clone();
                    tokio::spawn(async move {
                        while let Some(event) = events.recv().await {
                            let result = event.change.apply(&router);
                            if let Some(applied) = event.applied {
                                let _ = applied.send(result);
                            }
                        }
                    });
                }
            }
        });
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
        .map(|(provider, path)| (provider.key().to_owned(), path))
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
        Call::RegisterLiveActivity(p) => (Some(&p.session), None),
        _ => (None, None),
    }
}

async fn next_title(
    threads: &mut futures_util::stream::BoxStream<'_, Result<SessionSummary, Failure>>,
    deadline: tokio::time::Instant,
) -> Result<Option<SessionSummary>, Failure> {
    tokio::time::timeout_at(deadline, threads.try_next())
        .await
        .unwrap_or_else(|_| {
            Err(Failure::new(
                "provider_timeout",
                "session listing timed out; results are partial",
            ))
        })
}

fn describe_thread(
    thread: &mut Thread,
    capabilities: agent_protocol::session::Capabilities,
    projects: &crate::projects::state::Snapshot,
) {
    thread.project_id = projects.project_membership(thread.cwd.as_deref());
    thread.capabilities = Some(capabilities);
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn live_activity_registration_falls_back_without_apns_and_rejects_malformed_tokens() {
        use super::*;
        use agent_protocol::{
            live_activity::{LiveActivityRegistration, PushEnvironment, RegisterLiveActivity},
            session::SessionRef,
        };
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [Backend::unavailable(
                ProviderKind::Codex,
                "provider is offline",
            )],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let connection = service.open_session();
        let mut params = RegisterLiveActivity {
            session: SessionRef {
                provider: ProviderKind::Codex,
                id: "task".into(),
            },
            activity_id: "activity".into(),
            token: vec![1; 32],
            environment: PushEnvironment::Sandbox,
        };
        for configured in [false, true] {
            if configured {
                service
                    .inner
                    .router
                    .set_apns(Some(crate::apns::Apns::testing()));
                params.environment = PushEnvironment::Production;
            }
            let response = service
                .dispatch(connection.id(), &Call::RegisterLiveActivity(params.clone()))
                .await
                .unwrap();
            let Response::Success { result } = agent_protocol::protocol::decode::<
                Response<LiveActivityRegistration>,
            >(&response.initial)
            .unwrap() else {
                panic!("unconfigured or mismatched APNs must keep local updates available");
            };
            assert!(!result.enabled);
        }
        params.token.clear();
        let response = service
            .dispatch(connection.id(), &Call::RegisterLiveActivity(params))
            .await
            .unwrap();
        assert!(matches!(
            agent_protocol::protocol::decode::<Response<LiveActivityRegistration>>(
                &response.initial
            )
            .unwrap(),
            Response::Failure { .. }
        ));
    }
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

    #[tokio::test(start_paused = true)]
    async fn title_reads_share_one_deadline_across_pages() {
        use super::*;
        let start = tokio::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(5);
        let mut threads = futures_util::stream::iter([4, 2])
            .then(|seconds| async move {
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                Ok(SessionSummary {
                    thread: Thread::default(),
                    branch: None,
                })
            })
            .boxed();
        assert!(next_title(&mut threads, deadline).await.unwrap().is_some());
        assert_eq!(
            next_title(&mut threads, deadline).await.err().unwrap().code,
            "provider_timeout"
        );
        assert_eq!(
            tokio::time::Instant::now() - start,
            std::time::Duration::from_secs(5)
        );
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
        let configure = || {
            crate::adapters::Claude::load(
                program.clone(),
                directory.clone(),
                Some(root.path().join("native")),
            )
        };
        let error = match configure().await {
            Err(error) => error,
            Ok(_) => panic!("invalid registry was accepted"),
        };
        let service = HostRpcService::new(
            [Backend::unavailable(ProviderKind::Claude, error)],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        assert_eq!(
            service.provider_errors()["claude"]["code"],
            "provider_unavailable"
        );
        std::fs::write(&registry, r#"{"accounts":[],"selectedId":null}"#).unwrap();
        let service = HostRpcService::new(
            [configure().await.unwrap().into()],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
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
        let claude = crate::adapters::Claude::load(
            root.path().join("unused-cli"),
            root.path().join("claude"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        let connection = service.open_session();
        let router = &service.inner.router;
        let target =
            agent_protocol::session::SessionRef::new(ProviderKind::Claude, "native".into())
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
            let adapted = crate::adapters::requests::claude(
                uuid::Uuid::new_v4().to_string().into(),
                &"unrelated".into(),
                &crate::claude::SdkRequest::Elicitation {
                    server_name: "server".into(),
                    message: String::new(),
                    mode: None,
                    url: None,
                    requested_schema: Some(serde_json::json!({"type":"object","properties":{}})),
                },
            )
            .unwrap();
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
            router.session_change(
                &target,
                SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "unrelated".into(),
                        ..Default::default()
                    },
                    completed: true,
                },
            );
            assert_eq!(delivery(&id), Some(RequestDelivery::Awaiting));
            if native == "cancelled" {
                assert!(
                    input
                        .try_send(crate::claude::Command {
                            value: crate::claude::SdkInput::Interrupt {
                                request_id: "unused".into()
                            },
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
                assert!(
                    matches!(command.value, crate::claude::SdkInput::Answer { request_id, .. } if request_id == native)
                );
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
            router.resolve_native_request(instance, &serde_json::json!(native));
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
            let claude = case.ends_with("Claude") || case == "invalidClaudeId";
            let provider = if claude {
                ProviderKind::Claude
            } else {
                ProviderKind::Codex
            };
            let backend = if claude {
                crate::adapters::Claude::load(
                    root.path().join("unused-cli"),
                    root.path().join("claude"),
                    Some(root.path().join("native")),
                )
                .await
                .unwrap()
                .into()
            } else {
                Backend::unavailable(provider, "unavailable")
            };
            let service = HostRpcService::new(
                [backend],
                ProjectStore::new(root.path().join("worktrees.json")),
            );
            let connection = service.open_session();
            let target =
                agent_protocol::session::SessionRef::new(provider, "native".into()).unwrap();
            let (input, mut receiver) = tokio::sync::mpsc::channel(1);
            let stopped = tokio_util::sync::CancellationToken::new();
            let adapted = if claude {
                crate::adapters::requests::claude("request".into(), &"turn".into(), &crate::claude::SdkRequest::Elicitation { server_name: String::new(), message: String::new(), mode: None, url: None, requested_schema: Some(serde_json::json!({"type":"object","properties":{}})) })
            } else {
                crate::adapters::requests::codex("request".into(),"mcpServer/elicitation/request",&serde_json::json!({"mode":"form","requestedSchema":{"type":"object","properties":{}}}))
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
                .resolve_native_request(instance, &native_id);
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
        let claude = crate::adapters::Claude::load(
            root.path().join("missing-claude"),
            root.path().join("claude"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session();
        for (method, expected) in [
            ("host/session/fork", "unsupported_operation"),
            ("host/session/rename", "unsupported_operation"),
            ("host/session/submit", "provider_unavailable"),
        ] {
            let call = agent_protocol::protocol::json_boundary::call(method, serde_json::json!({"threadId":{"provider":"claude","id":"native"},"clientUserMessageId":method,"lastTurnId":"turn","name":"Renamed","input":[],"expectedTurnId":"turn"})).unwrap();
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
            [Backend::unavailable(ProviderKind::Codex, "unavailable")],
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session();
        for method in ["host/session/submit"] {
            let call = agent_protocol::protocol::json_boundary::call(
                method,
                serde_json::json!({"threadId":{"provider":"codex","id":"native"},"clientUserMessageId":"input","input":[],"expectedTurnId":"turn"}),
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
    async fn task_list_delivers_project_branding_to_core_and_refreshes_removed_icons() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("remote-agent");
        let native = root.path().join("native");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir_all(native.join("projects")).unwrap();
        std::fs::write(workspace.join("favicon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="#ff0000"/></svg>"##).unwrap();
        let projects = ProjectStore::new(root.path().join("bex-worktrees.json"));
        projects.register(&workspace).await.unwrap();
        let claude = crate::adapters::Claude::load(
            root.path().join("does-not-exist"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [
                Backend::unavailable(ProviderKind::Codex, "unavailable"),
                claude.into(),
            ],
            projects,
        );
        let session = service.open_session();
        let call = agent_protocol::protocol::Call::ListSessions(
            agent_protocol::operations::ListSessions {
                query: Default::default(),
            },
        );
        for has_icon in [true, false] {
            if !has_icon {
                std::fs::remove_file(workspace.join("favicon.svg")).unwrap();
            }
            let response = service.dispatch(session.id(), &call).await.unwrap();
            let reply = agent_protocol::protocol::decode::<
                Response<agent_protocol::models::ThreadList>,
            >(&response.initial)
            .unwrap();
            let Response::Success { result } = reply else {
                panic!("task list failed: {reply:?}")
            };
            let snapshot = agent_core::state::Snapshot {
                threads: Some(Arc::new(result)),
                ..Default::default()
            };
            let list = snapshot.thread_list().unwrap();
            assert_eq!(list.projects.len(), 1);
            let project = &list.projects[0];
            assert_eq!(project.monogram, "RA");
            assert_eq!(project.icon_png.is_some(), has_icon);
            if let Some(png) = &project.icon_png {
                let image = image::load_from_memory(png).unwrap().into_rgba8();
                assert_eq!(image.dimensions(), (64, 64));
                assert_eq!(image.get_pixel(32, 32).0, [255, 0, 0, 255]);
            }
        }
    }

    #[tokio::test]
    async fn opening_claude_history_reads_native_files_without_starting_a_cli() {
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
        let claude = crate::adapters::Claude::load(
            root.path().join("does-not-exist"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session();
        let call =
            agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                include_activity: false,
                session: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: id.into(),
                },
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
        // Even with the original subscription open, every open reads native data.
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value()
        .to_string();
        assert!(reply.contains("Changed outside Bex"), "{reply}");
        assert!(!reply.contains("Fixture user input"));
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
