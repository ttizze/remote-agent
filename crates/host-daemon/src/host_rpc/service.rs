//! The Host's RPC entry point. Dispatch admits each call, then routes it to the
//! owner of its resource; workflows that span owners live in the domain files.
mod activity;
mod project_import;
mod sessions;
mod titles;
mod update;
mod workspace;
mod workspace_setup;
mod worktrees;

use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;
use crate::providers::{Agent, Backend, Failure, SessionListScope, native_pages};
use agent_protocol::{
    models::Thread,
    operations as op,
    protocol::{Body, Call, Response},
    session::ProviderKind,
};
use futures_util::{StreamExt, TryStreamExt};
use sessions::describe_thread;
use std::sync::{Arc, OnceLock};

const SESSION_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub struct HostRpcService {
    pub(super) inner: Arc<ServiceInner>,
}

pub(super) struct ServiceInner {
    browser: OnceLock<Arc<crate::browser::Browser>>,
    pub(super) providers: crate::providers::Registry,
    provider_settings: crate::providers::Settings,
    pub(super) projects: ProjectStore,
    pub(super) conversation: super::runtime::Runtime,
    pub(super) router: SessionRouter,
    event_pumps: std::sync::Once,
    files: crate::workspace_files::WorkspaceFiles,
    editors: crate::editors::Editors,
    pub(super) worktrees: crate::worktrees::Worktrees,
    pub(super) worktree_access: tokio::sync::RwLock<()>,
    workspace_access: tokio::sync::Mutex<()>,
    worktree_cleanup: tokio::sync::Notify,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
    history_scope: OnceLock<(std::path::PathBuf, bool)>,
    updating: std::sync::atomic::AtomicBool,
}

impl HostRpcService {
    pub(in crate::host_rpc) fn require_not_updating(&self) -> Result<(), Failure> {
        if self
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Err(Failure::new(
                "host_updating",
                "Bexを更新しています。再接続後にもう一度お試しください。",
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "test-support")]
    pub fn fixture_projects(&self) -> ProjectStore {
        self.inner.projects.clone()
    }

    pub fn new(
        backends: impl IntoIterator<Item = Backend>,
        project_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        let project_path = project_path.into();
        let storage = Arc::new(super::storage::Storage::open(
            &project_path.with_file_name(crate::host_rpc::storage::CONVERSATION_DATABASE_FILE),
        ));
        let updating_recovery = update::consume_restart_marker(
            &project_path.with_file_name(agent_local_host::update::RECOVERY_FILE),
        )
        .unwrap_or_else(|error| {
            tracing::warn!(target:"agent",operation="host.update.marker",%error);
            false
        });
        let projects = ProjectStore::new(project_path, storage.clone());
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let router = SessionRouter::new();
        let (providers, dictation) = crate::providers::Registry::new(backends);
        let conversation = super::runtime::Runtime::new(
            storage,
            providers.all().into_iter().map(|(provider, _)| provider),
            updating_recovery,
        );
        Self {
            inner: Arc::new(ServiceInner {
                history_scope: OnceLock::new(),
                conversation,
                updating: std::sync::atomic::AtomicBool::new(false),
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
    pub(crate) fn storage_scope(&self) -> anyhow::Result<String> {
        self.inner
            .conversation
            .storage
            .scope()
            .map_err(anyhow::Error::msg)
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
            match self
                .inner
                .providers
                .identity(provider)?
                .account(command)
                .await?
            {
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

    pub(super) fn browser_config(
        &self,
        thread: &str,
    ) -> Result<Option<serde_json::Value>, Failure> {
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
        self.inner.conversation.cancel_preparations();
        if let Some(browser) = self.inner.browser.get() {
            browser.shutdown().await;
        }
        self.inner.terminals.shutdown().await;
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
        self.inner.conversation.storage.check_ready()?;
        self.start_event_pumps();
        Ok(())
    }

    /// Dispatch a classified message from an authenticated session.
    pub async fn dispatch(&self, session: SessionId, message: &Call) -> Result<HostReply, String> {
        self.inner.router.ensure_session(session)?;
        let _workspace = if matches!(
            message,
            Call::StartTerminal(_)
                | Call::StartAccountTerminal(_)
                | Call::WriteFile(_)
                | Call::WriteWorkspaceFile(_)
                | Call::Upload(_)
                | Call::ReviewWorkspace(_)
                | Call::ChangeWorkspace(_)
        ) {
            Some(self.inner.worktree_access.read().await)
        } else {
            None
        };
        let result = async {
            if _workspace.is_some() {
                self.require_not_updating()?;
            }
            match super::runtime::Runtime::dispatch(self, session, message).await? {
                Some(reply) => Ok(reply),
                None => self
                    .request(session, message)
                    .await
                    .map(|body| Response::Success { result: body }.into()),
            }
        }
        .await;
        if let Err(error) = &result {
            agent_transport::diagnostics::rpc_error(
                message.method(),
                None,
                &serde_json::value::to_raw_value(error).map_err(invalid_message)?,
            );
        }
        Ok(result.unwrap_or_else(|error| {
            Response::Failure {
                error: error.into(),
            }
            .into()
        }))
    }

    async fn request(&self, session: SessionId, request: &Call) -> Result<Body, Failure> {
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
        let response = match request {
            Call::SearchList(params) => self.search_list(params).await?.into(),
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
                self.inner
                    .conversation
                    .storage
                    .mark_read(&params.session, &params.turn_id)
                    .map_err(super::runtime::storage_failure)?;
                agent_protocol::models::Empty {}.into()
            }
            Call::RegisterLiveActivity(params) => {
                self.register_live_activity(session, params)?.into()
            }
            Call::UnregisterLiveActivity(params) => {
                self.unregister_live_activity(session, params)?.into()
            }

            Call::RequestSession(params) => self
                .inner
                .conversation
                .storage
                .request_owner(&params.request_id)
                .map_err(super::runtime::storage_failure)?
                .ok_or_else(|| Failure::new("request_unavailable", "request is no longer pending"))?
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
                    .identity(params.provider)?
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
            _ => self.provider_request(request).await?,
        };
        Ok(response)
    }

    /// Dispatch remaining provider requests; unknown requests return method_not_found.
    async fn provider_request(&self, request: &Call) -> Result<Body, Failure> {
        Ok(match request {
            Call::ReadPermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.inner
                    .providers
                    .configuration(params.provider)?
                    .read_permissions()
                    .await?
                    .into()
            }
            Call::UpdatePermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                self.inner
                    .providers
                    .configuration(params.provider)?
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
                .identity(params.provider)?
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
                .configuration(ProviderKind::Codex)?
                .codex_cli(params.update)
                .await?
                .into(),
            Call::ReadModelSettings(params) => self
                .inner
                .providers
                .configuration(params.model.provider)?
                .model_settings(&params.model.id)
                .await?
                .into(),
            Call::ReadDshCredentials(_) => self
                .inner
                .providers
                .identity(ProviderKind::DeepSeek)?
                .dsh_credentials()
                .await?
                .into(),
            Call::UpdateDshCredential(update) => self
                .inner
                .providers
                .identity(ProviderKind::DeepSeek)?
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

    pub(in crate::host_rpc) fn require_provider_enabled(
        &self,
        provider: ProviderKind,
    ) -> Result<(), Failure> {
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
            super::runtime::Runtime::start(self);
            for (_, agent) in self.inner.providers.all() {
                if let Some(mut events) = agent.event_stream() {
                    let owner = Arc::downgrade(&self.inner);
                    tokio::spawn(async move {
                        while let Some(event) = events.recv().await {
                            let Some(inner) = owner.upgrade() else {
                                break;
                            };
                            let result = super::runtime::Runtime::observe(
                                &Self { inner },
                                event.change,
                                event.execution,
                            )
                            .await;
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

#[cfg(test)]
mod tests;
