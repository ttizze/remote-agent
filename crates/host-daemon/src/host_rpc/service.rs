//! The Host's RPC entry point. Dispatch admits each call, then routes it to the
//! owner of its resource; workflows that span owners live in the domain files.
mod activity;
mod capacity;
mod conversations;
pub(super) mod model_selections;
mod preparation;
mod project_import;
mod queue;
mod sessions;
mod titles;
mod transcripts;
mod update;
mod workspace;
mod worktrees;

use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;
use crate::providers::{
    Agent, Backend, Failure, SessionListScope, read_session_page, session_pages,
};
use agent_protocol::{
    models::{Thread, ThreadResponse},
    operations as op,
    protocol::{Body, Call, Response},
    session::ProviderKind,
};
use futures_util::{StreamExt, TryStreamExt};
use sessions::describe_thread;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};
use worktrees::worktree_active;

const SESSION_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub struct HostRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    browser: OnceLock<Arc<crate::browser::Browser>>,
    providers: crate::providers::Registry,
    provider_settings: crate::providers::Settings,
    projects: ProjectStore,
    router: SessionRouter,
    event_pumps: std::sync::Once,
    files: crate::workspace_files::WorkspaceFiles,
    editors: crate::editors::Editors,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    workspace_access: tokio::sync::Mutex<()>,
    worktree_cleanup: tokio::sync::Notify,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
    conversations: super::conversations::Conversations,
    store: Arc<super::store::ConversationStore>,
    imports: std::sync::Mutex<HashMap<agent_protocol::session::SessionRef, transcripts::Import>>,
    model_selections: model_selections::ModelSelections,
    capacity: std::sync::Mutex<HashMap<agent_protocol::session::SessionRef, capacity::Run>>,
    preparations: std::sync::Mutex<
        HashMap<agent_protocol::session::SessionRef, tokio_util::sync::CancellationToken>,
    >,
    history_scope: OnceLock<(std::path::PathBuf, bool)>,
    updating: std::sync::atomic::AtomicBool,
}

impl HostRpcService {
    pub fn new(backends: impl IntoIterator<Item = Backend>, projects: ProjectStore) -> Self {
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let store = Arc::new(super::store::ConversationStore::open(
            &projects.path().with_file_name("bex-transcripts.sqlite"),
        ));
        let conversations = super::conversations::Conversations::new(store.clone());
        let router = SessionRouter::new().with_store(store.clone());
        let (providers, dictation) = crate::providers::Registry::new(backends);
        Self {
            inner: Arc::new(ServiceInner {
                history_scope: OnceLock::new(),
                updating: std::sync::atomic::AtomicBool::new(false),
                conversations,
                store: store.clone(),
                imports: Default::default(),
                capacity: Default::default(),
                preparations: Default::default(),
                model_selections: model_selections::ModelSelections::new(store),
                browser: OnceLock::new(),
                provider_settings: crate::providers::Settings::new(
                    projects.path().with_file_name("bex-providers.json"),
                ),
                providers,
                dictation,
                worktrees: crate::worktrees::Worktrees::new(projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                workspace_access: Default::default(),
                worktree_cleanup: tokio::sync::Notify::new(),
                permission_settings_access: Default::default(),
                terminals: Default::default(),
                projects,
                router,
                event_pumps: std::sync::Once::new(),
                files,
                editors: Default::default(),
            }),
        }
    }

    /// Clients compare this with cached state at the start of every connection.
    pub(crate) fn storage_scope(&self) -> Result<String, serde_json::Error> {
        self.inner.providers.storage_scope()
    }

    async fn account_request(&self, request: Call) -> Result<Body, Failure> {
        if matches!(request, Call::ListAccounts(_)) {
            return Ok(self.inner.providers.accounts().await.into());
        }
        use crate::providers::{AccountCommand as Command, AccountReply};
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
        Ok(
            match self.inner.providers.get(provider)?.account(command).await? {
                AccountReply::Selection(value) => value.into(),
                AccountReply::Login(value) => value.into(),
                AccountReply::Status(value) => value.into(),
                AccountReply::Complete => agent_protocol::models::Empty {}.into(),
            },
        )
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
        for cancel in self.inner.preparations.lock().unwrap().values() {
            cancel.cancel();
        }
        if let Some(browser) = self.inner.browser.get() {
            browser.shutdown().await;
        }
        self.inner.terminals.shutdown().await;
        self.inner.router.record_running();
        self.inner.providers.shutdown().await;
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
        (
            self.inner.providers.data_recipients(),
            self.inner.dictation.data_recipient(),
        )
    }
    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        self.inner.providers.errors()
    }

    pub fn start(&self) -> Result<(), String> {
        self.inner.store.check_ready()?;
        self.start_event_pumps();
        Ok(())
    }

    /// Dispatch a classified message from an authenticated session.
    pub async fn dispatch(&self, session: SessionId, message: &Call) -> Result<HostReply, String> {
        self.inner.router.ensure_session(session)?;
        if let Call::OpenSession(params) = message {
            return self.session_open(session, params).await;
        }
        if let Call::CreateSession(params) = message {
            let result = async {
                let (response, prepare) = if params.initial_input.is_some() {
                    self.admit_task(params).await?
                } else {
                    let _workspace = self.inner.worktree_access.read().await;
                    (self.create_session(params.clone()).await?, false)
                };
                let target = response.thread.id.clone().ok_or_else(|| {
                    Failure::new("invalid_thread", "created session ID is missing")
                })?;
                let read = self
                    .inner
                    .router
                    .retain_execution(target.clone())
                    .map_err(|error| Failure::new("invalid_thread", error))?;
                let reply = self
                    .inner
                    .router
                    .finish_session_read(read, session, response, false)
                    .map_err(|error| Failure::new("session_create_failed", error));
                // Attach the creating client before work can complete. A
                // disconnected caller still owns an admitted durable task.
                if prepare {
                    self.schedule_preparation(target);
                }
                reply
            }
            .await;
            return match result {
                Ok(reply) => Ok(reply),
                Err(error) => Ok(Response::from_result::<(), _>(Err(error)).into()),
            };
        }
        let result = async {
            if let Call::RetryPreparation(params) = message
                && params.work_locally
            {
                self.preempt_setup(&params.thread_id).await?;
            }
            let (target, input_id) = session_target(message);
            let target = target.cloned();
            let submission = target.as_ref().zip(input_id);
            let serialized = target.as_ref().filter(|_| {
                matches!(
                    message,
                    Call::Submit(_)
                        | Call::ChangeQueue(_)
                        | Call::RenameSession(_)
                        | Call::ForkSession(_)
                        | Call::RewindSession(_)
                        | Call::RetryPreparation(_)
                )
            });
            // Retain the actor before locking: idle reads must not prune it
            // and let a concurrent operation acquire a different mutex.
            let _execution = serialized
                .map(|target| self.inner.router.retain_execution(target.clone()))
                .transpose()
                .map_err(|error| Failure::new("invalid_params", error))?;
            let _serial = if let Some(target) = serialized {
                Some(self.inner.router.submission_lock(target).lock_owned().await)
            } else {
                None
            };
            let _workspace_read = if matches!(
                message,
                Call::Submit(_)
                    | Call::ChangeQueue(_)
                    | Call::ForkSession(_)
                    | Call::RewindSession(_)
                    | Call::AnswerSession(_)
                    | Call::StartTerminal(_)
                    | Call::StartAccountTerminal(_)
                    | Call::WriteFile(_)
                    | Call::WriteWorkspaceFile(_)
                    | Call::Upload(_)
                    | Call::ChangeWorkspace(_)
                    | Call::ReviewWorkspace(_)
            ) {
                Some(self.inner.worktree_access.read().await)
            } else {
                None
            };
            if _workspace_read.is_some()
                && self
                    .inner
                    .updating
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(Failure::new(
                    "host_updating",
                    "Bexを更新しています。再接続後にもう一度お試しください。",
                ));
            }
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
                    "このホストではBexブラウザが有効になっていません。",
                )
            })?;
            return browser
                .request(params)
                .await
                .map(Into::into)
                .map_err(|e| Failure::new("browser_failed", e));
        }
        if let Call::ReadItem(params) = request {
            let Some(target) = target_session else {
                return Err(Failure::new("invalid_params", "session ID is required"));
            };
            return self
                .read_item(session, params, target)
                .await
                .map(Into::into);
        }
        let response = match request {
            Call::ReadFileOpenCapabilities(_) => self
                .inner
                .editors
                .capabilities()
                .await
                .map_err(|error| Failure::new("editor_discovery_failed", error))?
                .into(),
            Call::OpenInEditor(params) => {
                self.inner
                    .editors
                    .open(params.clone())
                    .await
                    .map_err(|error| Failure::new("editor_open_failed", error))?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadHandoffHistory(params) => self.read_handoff_history(params).await?.into(),
            Call::Unsupported => {
                return Err(Failure::new(
                    "method_not_found",
                    "このホストはこの操作に対応していません。PC側のBexを更新してください。",
                ));
            }
            Call::MarkSessionRead(params) => {
                self.inner.router.mark_session_read(params)?;
                agent_protocol::models::Empty {}.into()
            }
            Call::ReadTaskActivity(_) => self.read_task_activity(false).await?.into(),
            Call::RegisterLiveActivity(params) => {
                self.register_live_activity(session, params)?.into()
            }
            Call::UnregisterLiveActivity(params) => {
                self.unregister_live_activity(session, params)?.into()
            }
            Call::ReadTurnItems(params) => self.read_turn_items(session, params).await?.into(),
            Call::ReadHistory(params) => self.read_history(params).await?.into(),

            Call::RequestSession(params) => (self
                .inner
                .router
                .request_session(&params.request_id.to_string())
                .ok_or_else(|| {
                    Failure::new("request_unavailable", "request is no longer pending")
                })?)
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
            Call::ScanProjects(_) => self.scan_projects().await?.into(),
            Call::ImportProjects(params) => self.import_projects(&params.paths).await?.into(),
            Call::ReadListDecorations(params) => self.list_decorations(params).await?.into(),
            Call::ListSessions(params) => self.list_sessions(params).await?.into(),
            Call::ListProjectSessions(params) => self.list_project_sessions(params).await?.into(),
            Call::ListAgents(params) => self.list_agents(params).await?.into(),
            Call::ReadWorktreeSettings(_) => (self
                .inner
                .worktrees
                .settings(None)
                .await
                .map_err(|error| Failure::new("worktree_settings_failed", error))?)
            .into(),
            Call::UpdateWorktreeSettings(settings) => (self
                .update_worktree_settings(settings.clone())
                .await
                .map_err(|error| Failure::new("worktree_settings_failed", error))?)
            .into(),
            Call::ListWorktrees(_) => (self.worktree_list().await?).into(),
            Call::RemoveWorktree(params) => {
                (self.remove_worktree(params.path.clone()).await?).into()
            }

            Call::CreateSession(_) => unreachable!("creation owns its subscription"),
            Call::RetryPreparation(params) => self
                .retry_preparation(&params.thread_id, params.work_locally)
                .await?
                .into(),
            Call::StartTerminal(params) => {
                if self.inner.terminals.is_setup(&params.handle) {
                    self.inner
                        .terminals
                        .attach_setup(&self.inner.router, session, &params.handle, params.size)
                        .await
                        .map_err(|error| Failure::new("terminal_start_failed", error))?;
                    return Ok(agent_protocol::models::Empty {}.into());
                }
                let launch = crate::terminals::Launch::command(
                    params.cwd.clone(),
                    params.command.as_deref(),
                )
                .map_err(|error| Failure::new("terminal_start_failed", error))?;
                (self
                    .inner
                    .terminals
                    .start(
                        self.inner.router.clone(),
                        session,
                        params.handle.clone(),
                        launch,
                        params.size,
                    )
                    .await
                    .map_err(|error| Failure::new("terminal_start_failed", error))?)
                .into()
            }
            Call::StartAccountTerminal(params) => {
                let launch = self
                    .inner
                    .providers
                    .get(params.provider)?
                    .account_terminal(&params.id)
                    .await?;
                self.inner
                    .terminals
                    .start(
                        self.inner.router.clone(),
                        session,
                        params.handle.clone(),
                        launch,
                        params.size,
                    )
                    .await
                    .map_err(|error| Failure::new("terminal_start_failed", error))?
                    .into()
            }
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
            Call::ReviewWorkspace(params) => self.review_workspace(params).await?.into(),
            Call::ChangeWorkspace(params) => self.change_workspace(params).await?.into(),
            Call::ListFiles(_)
            | Call::ListTreeEntries(_)
            | Call::SearchTreeEntries(_)
            | Call::ResolveFile(_)
            | Call::ReadFileMetadata(_)
            | Call::ReadFile(_)
            | Call::WriteFile(_)
            | Call::WriteWorkspaceFile(_)
            | Call::Upload(_)
            | Call::Download(_)
            | Call::ReadVisualization(_) => {
                self.inner.files.request(session, request.clone()).await?
            }
            Call::ForkSession(_)
            | Call::RewindSession(_)
            | Call::Interrupt(_)
            | Call::ChangeQueue(_)
            | Call::RenameSession(_) => {
                let target = target_session.expect("session-scoped conversation change");
                self.change_conversation(target, request).await?
            }
            _ => self.provider_request(request).await?,
        };
        Ok(response)
    }

    async fn change_conversation(
        &self,
        target: &agent_protocol::session::SessionRef,
        request: &Call,
    ) -> Result<Body, Failure> {
        Ok(match request {
            Call::ForkSession(params) => self.fork_session(target, params).await?.into(),
            Call::RewindSession(params) => self.rewind_session(target, params).await?.into(),
            Call::Interrupt(params) => self.interrupt(target, params).await?.into(),
            Call::ChangeQueue(params) => self.change_queue(target, &params.change).await?.into(),
            Call::RenameSession(params) => self.rename_session(target, params).await?.into(),
            _ => unreachable!("only conversation changes are routed here"),
        })
    }

    /// Dispatch remaining provider requests; unknown requests return method_not_found.
    async fn provider_request(&self, request: &Call) -> Result<Body, Failure> {
        Ok(match request {
            Call::ReadPermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.inner
                    .providers
                    .get(params.provider)?
                    .read_permissions()
                    .await?
                    .into()
            }
            Call::UpdatePermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.inner
                    .providers
                    .get(params.provider)?
                    .update_permissions(params.mode, &params.version)
                    .await?
                    .into()
            }
            Call::ComposerCatalog(params) => {
                let settings = self.inner.provider_settings.read()?;
                let mut catalog = self.inner.providers.catalog(&params.cwd).await;
                catalog
                    .candidates
                    .retain(|candidate| settings.enabled(candidate.invocation.provider));
                catalog
                    .errors
                    .retain(|provider, _| settings.enabled(*provider));
                catalog.into()
            }
            Call::ReadAccountUsage(params) => self
                .inner
                .providers
                .get(params.provider)?
                .usage(&params.id)
                .await?
                .into(),
            Call::ListAccounts(_)
            | Call::SelectAccount(_)
            | Call::LogoutAccount(_)
            | Call::StartAccountLogin(_)
            | Call::ReadAccountLogin(_)
            | Call::SubmitAccountLogin(_)
            | Call::CancelAccountLogin(_) => self.account_request(request.clone()).await?,

            Call::CodexCli(params) => self
                .inner
                .providers
                .get(ProviderKind::Codex)?
                .codex_cli(params.update)
                .await?
                .into(),
            Call::ReadModelSettings(params) => self
                .inner
                .providers
                .get(params.model.provider)?
                .model_settings(&params.model.id)
                .await?
                .into(),
            Call::ReadDshCredentials(_) => self
                .inner
                .providers
                .get(ProviderKind::DeepSeek)?
                .dsh_credentials()
                .await?
                .into(),
            Call::UpdateDshCredential(update) => self
                .inner
                .providers
                .get(ProviderKind::DeepSeek)?
                .update_dsh_credential(update.clone())
                .await?
                .into(),
            Call::ReadProviderSettings(_) => self.inner.provider_settings.read()?.into(),
            Call::ReadHostSoftware(_) => op::HostSoftware {
                version: env!("CARGO_PKG_VERSION").into(),
                revision: option_env!("AGENT_BUILD_REVISION")
                    .unwrap_or("development")
                    .into(),
                build: option_env!("AGENT_BUILD_NUMBER")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            }
            .into(),
            Call::SetProviderEnabled(params) => {
                let settings = self
                    .inner
                    .provider_settings
                    .set_enabled(params.provider, params.enabled)?;
                self.inner.router.broadcast(
                    agent_protocol::protocol::Notification::ProviderSettings {
                        settings: settings.clone(),
                    },
                );
                settings.into()
            }
            Call::ListModels(params) => {
                if self
                    .inner
                    .provider_settings
                    .read()?
                    .enabled(params.provider)
                {
                    self.inner.providers.models(params).await?.into()
                } else {
                    op::ModelPage::default().into()
                }
            }
            _ => {
                return Err(Failure::new(
                    "method_not_found",
                    format!("unregistered method: {}", request.method()),
                ));
            }
        })
    }

    pub(crate) fn files(&self) -> &crate::workspace_files::WorkspaceFiles {
        &self.inner.files
    }

    fn require_provider_enabled(&self, provider: ProviderKind) -> Result<(), Failure> {
        if !self.inner.provider_settings.read()?.enabled(provider) {
            return Err(Failure::new(
                "provider_disabled",
                "この環境でプロバイダーがオフになっています。設定からオンにしてください。",
            ));
        }
        Ok(())
    }

    fn start_event_pumps(&self) {
        self.inner.event_pumps.call_once(|| {
            if let Err(message) = self.inner.store.interrupt_running() {
                tracing::error!(target: "agent", operation = "host.transcript.write", message);
            }
            self.restore_queues();
            if self.inner.router.apns().is_some() {
                let owner = Arc::downgrade(&self.inner);
                tokio::spawn(async move {
                    let mut delay = 1;
                    loop {
                        let Some(inner) = owner.upgrade() else { break };
                        let service = Self { inner };
                        let Some(apns) = service.inner.router.apns() else {
                            break;
                        };
                        if apns.is_ready() {
                            break;
                        }
                        let _ = service.read_task_activity(false).await;
                        if apns.is_ready() {
                            break;
                        }
                        drop(apns);
                        drop(service);
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(60);
                    }
                });
            }

            for (_, agent) in self.inner.providers.all() {
                if let Some(mut events) = agent.event_stream() {
                    let supports_fork = agent.capabilities().fork;
                    let owner = Arc::downgrade(&self.inner);
                    tokio::spawn(async move {
                        while let Some(event) = events.recv().await {
                            let Some(inner) = owner.upgrade() else {
                                break;
                            };
                            let bindings = if matches!(
                                event.change,
                                crate::providers::AgentChange::Stopped { .. }
                            ) {
                                inner.conversations.active_bindings()
                            } else {
                                Ok(Vec::new())
                            };
                            let result = bindings
                                .and_then(|bindings| {
                                    inner
                                        .conversations
                                        .route(event.change, supports_fork)
                                        .map(|event| (bindings, event))
                                })
                                .map_err(|error| error.to_string())
                                .and_then(|(bindings, event)| {
                                    event.map_or(Ok(()), |mut event| {
                                        let service = Self {
                                            inner: inner.clone(),
                                        };
                                        service.capacity_event(&mut event);
                                        service.apply_change(event, &bindings)
                                    })
                                });
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

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid request: {error}")
}

// Serialize the sorted map directly: a JSON Value's object ordering can vary
// with serde_json features unified by unrelated client dependencies.
// Every session-scoped operation provides its target here. Submission IDs
// additionally identify the operations whose delivery must be tracked.
fn session_target(request: &Call) -> (Option<&agent_protocol::session::SessionRef>, Option<&str>) {
    match request {
        Call::Submit(p) => (Some(&p.thread_id), Some(p.client_user_message_id.as_str())),
        Call::ForkSession(p) => (Some(&p.thread_id), None),
        Call::RewindSession(p) => (Some(&p.thread_id), None),
        Call::Interrupt(p) => (Some(&p.thread_id), None),
        Call::ChangeQueue(p) => (Some(&p.thread_id), None),
        Call::RetryPreparation(p) => (Some(&p.thread_id), None),
        Call::ReadItem(p) => (Some(&p.thread_id), None),
        Call::ReadTurnItems(p) => (Some(&p.session), None),
        Call::RenameSession(p) => (Some(&p.thread_id), None),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn provider_switches_broadcast_and_gate_new_work_while_retaining_history_access() {
        use super::conversations::tests::{NativeAgent, call};
        use super::*;
        use agent_protocol::{
            protocol::Notification,
            providers::{ProviderSettings, SetProviderEnabled},
        };
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let host = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let mut other = host.open_session();
        let settings: ProviderSettings = call(
            &host,
            client.id(),
            Call::SetProviderEnabled(SetProviderEnabled {
                provider: ProviderKind::Codex,
                enabled: false,
            }),
        )
        .await
        .unwrap();
        assert!(!settings.enabled(ProviderKind::Codex));
        let notified = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let message =
                    agent_protocol::protocol::decode::<Notification>(&other.recv().await.unwrap())
                        .unwrap();
                if let Notification::ProviderSettings { settings } = message {
                    break settings;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(notified, settings);
        let read: ProviderSettings = call(
            &host,
            other.id(),
            Call::ReadProviderSettings(agent_protocol::models::Empty {}),
        )
        .await
        .unwrap();
        assert_eq!(read, settings);
        assert_eq!(
            host.creation_agent(ProviderKind::Codex, None)
                .err()
                .unwrap()
                .code,
            "provider_disabled"
        );
        assert!(
            host.inner.providers.get(ProviderKind::Codex).is_ok(),
            "history and running work keep their provider"
        );
        let models: op::ModelPage = call(
            &host,
            other.id(),
            Call::ListModels(op::ListModels {
                provider: ProviderKind::Codex,
                limit: 100,
                cursor: None,
            }),
        )
        .await
        .unwrap();
        assert!(models.data.is_empty());
        assert!(models.provider_errors.is_none());
        let enabled: ProviderSettings = call(
            &host,
            other.id(),
            Call::SetProviderEnabled(SetProviderEnabled {
                provider: ProviderKind::Codex,
                enabled: true,
            }),
        )
        .await
        .unwrap();
        assert!(enabled.enabled(ProviderKind::Codex));
        assert!(host.creation_agent(ProviderKind::Codex, None).is_ok());
    }

    #[tokio::test]
    async fn file_failures_are_classified_without_losing_visualization_recovery() {
        use super::conversations::tests::call;
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            std::iter::empty::<Backend>(),
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        let session = service.open_session();
        let missing = root.path().join("missing.mp4").to_str().unwrap().to_owned();
        for request in [
            Call::ResolveFile(op::ResolveFile {
                path: missing.clone(),
            }),
            Call::ReadFile(op::ListFiles {
                path: missing.clone(),
            }),
            Call::Download(op::ListFiles { path: missing }),
        ] {
            let failure = call::<serde_json::Value>(&service, session.id(), request)
                .await
                .unwrap_err();
            assert_eq!(failure.code, "file_not_found");
        }
        let failure = call::<String>(
            &service,
            session.id(),
            Call::ReadVisualization(op::LoadVisualization {
                path: "missing.html".into(),
                cwd: root.path().to_str().unwrap().into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, "file_operation_failed");
        assert!(
            failure
                .message
                .contains("表示ファイルが見つかりません。ホストでHTMLを再作成してください。")
        );
    }

    #[tokio::test]
    async fn account_providers_are_read_in_parallel_without_blocking_conversation_lists() {
        use super::conversations::tests::{NativeAgent, call};
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        a.accounts_gate.forget_permits(1);
        b.accounts_gate.forget_permits(1);
        let host = HostRpcService::new(
            [a.backend(), b.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let mut accounts = Box::pin(call::<op::Accounts>(
            &host,
            client.id(),
            Call::ListAccounts(agent_protocol::models::Empty {}),
        ));
        tokio::select! {
            result = &mut accounts => panic!("withheld accounts returned early: {result:?}"),
            result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                a.accounts_started.notified().await;
                b.accounts_started.notified().await;
            }) => result.expect("both providers must start before either one finishes"),
        }
        let list = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            call::<agent_protocol::models::ThreadList>(
                &host,
                client.id(),
                Call::ListSessions(op::ListSessions::new(Default::default())),
            ),
        )
        .await
        .expect("conversation lists must not wait for accounts")
        .unwrap();
        assert!(list.provider_errors.is_none());
        a.accounts_gate.add_permits(1);
        b.accounts_gate.add_permits(1);
        assert!(accounts.await.unwrap().errors.is_empty());
    }

    #[tokio::test]
    async fn account_read_failures_keep_their_provider_alongside_startup_errors() {
        use super::conversations::tests::{NativeAgent, call};
        use super::*;
        use std::sync::atomic::Ordering;
        let directory = tempfile::tempdir().unwrap();
        let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let host = HostRpcService::new(
            [
                Backend::unavailable(ProviderKind::DeepSeek, "deepseek startup failed"),
                Backend::unavailable(ProviderKind::Claude, "claude startup failed"),
                a.backend(),
            ],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();

        a.fail_accounts.store(1, Ordering::SeqCst);
        let accounts: op::Accounts = call(
            &host,
            client.id(),
            Call::ListAccounts(agent_protocol::models::Empty {}),
        )
        .await
        .unwrap();
        assert_eq!(
            accounts.errors,
            [
                (ProviderKind::Codex, "codex accounts failed".into()),
                (ProviderKind::Claude, "claude startup failed".into()),
                (ProviderKind::DeepSeek, "deepseek startup failed".into()),
            ]
            .into()
        );
    }

    #[tokio::test]
    async fn failed_model_providers_remain_identifiable_when_no_catalog_is_available() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let providers = [
            ProviderKind::Codex,
            ProviderKind::Claude,
            ProviderKind::DeepSeek,
        ];
        let service = HostRpcService::new(
            providers.map(|provider| Backend::unavailable(provider, "offline")),
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        let session = service.open_session();
        for provider in providers {
            let response = service
                .dispatch(
                    session.id(),
                    &Call::ListModels(op::ListModels {
                        provider,
                        cursor: None,
                        limit: 100,
                    }),
                )
                .await
                .unwrap();
            let Response::Success { result: page } =
                agent_protocol::protocol::decode::<Response<op::ModelPage>>(&response.initial)
                    .unwrap()
            else {
                panic!("catalog failures must retain each provider's error");
            };
            assert!(page.data.is_empty());
            assert!(page.next_cursor.is_none());
            let errors = page.provider_errors.unwrap();
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[provider.key()]["code"], "provider_unavailable");
        }
    }

    #[tokio::test]
    async fn successful_configuration_clears_the_provider_startup_error() {
        use super::conversations::tests::NativeAgent;
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [Backend::unavailable(
                ProviderKind::Claude,
                "invalid provider configuration",
            )],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        assert_eq!(
            service.provider_errors()["claude"]["code"],
            "provider_unavailable"
        );
        let agent = NativeAgent::new(ProviderKind::Claude, root.path().join("claude"));
        let service = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        assert!(service.provider_errors().get("claude").is_none());
        let session = service.open_session();
        let response = service
            .dispatch(
                session.id(),
                &Call::CreateSession(op::CreateSession {
                    initial_input: None,
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
        let claude = crate::providers::Claude::load(
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
            if expected == "unsupported_operation" {
                assert_eq!(
                    response["error"]["message"],
                    format!("{method} is unsupported by this provider")
                );
            }
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
    async fn enabling_host_owned_harness_keeps_the_existing_draft_namespace() {
        let root = tempfile::tempdir().unwrap();
        let claude = crate::providers::claude::Claude::load(
            "claude".into(),
            root.path().join("accounts"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let service = super::HostRpcService::new(
            [claude.into()],
            crate::ProjectStore::new(root.path().join("projects.json")),
        );
        let before = service.storage_scope().unwrap();
        let claude = crate::providers::claude::Claude::load(
            "claude".into(),
            root.path().join("accounts"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let harness = crate::providers::deepseek::DeepSeek::load(
            "dsh".into(),
            root.path().join("harness"),
            Some(root.path().join("home")),
            None,
        )
        .await
        .unwrap();
        let service = super::HostRpcService::new(
            [claude.into(), harness.into()],
            crate::ProjectStore::new(root.path().join("projects.json")),
        );
        let after = service.storage_scope().unwrap();
        assert_eq!(
            before, after,
            "Adding Harness must not archive existing drafts or navigation"
        );
    }
}
