//! The Host's RPC entry point. Dispatch admits each call, then routes it to the
//! owner of its resource; workflows that span owners live in the domain files.
use super::owners::Write;
mod activity;
mod automations;
mod feedback;
mod project_import;
mod pull_requests;
mod sessions;
mod titles;
mod update;
mod workspace;
mod workspace_setup;
mod worktrees;

use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;
use crate::providers::{Agent, Backend, Failure, SessionListScope, session_pages};
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
    automations: crate::automations::Automations,
    automation_stop: tokio_util::sync::CancellationToken,
    browser: OnceLock<Arc<crate::browser::Browser>>,
    pub(super) providers: crate::providers::Registry,
    provider_settings: crate::providers::Settings,
    pub(super) projects: ProjectStore,
    project_clones: crate::project_clone::Clones,
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
            &project_path.with_file_name("bex-conversations.sqlite"),
        ));
        let projects = ProjectStore::new(project_path, storage.clone());
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        let router = SessionRouter::new();
        let (providers, dictation) = crate::providers::Registry::new(backends);
        let conversation = super::runtime::Runtime::new(
            storage,
            providers.all().into_iter().map(|(provider, _)| provider),
        );
        Self {
            inner: Arc::new(ServiceInner {
                automations: crate::automations::Automations::open(
                    &projects.path().with_file_name("bex-automations.sqlite"),
                ),
                automation_stop: Default::default(),
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
                project_clones: crate::project_clone::Clones::new(projects.clone()),
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
        Ok(self.inner.providers.storage_scope()?)
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
        self.inner.automation_stop.cancel();
        self.inner.project_clones.shutdown().await;
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
        self.inner.conversation.owners.disconnect(session);
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
        if let Some(response) = self.project_request(request).await? {
            return Ok(response);
        }
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
            Call::Automations(params) => self.automations(params).await?.into(),
            Call::PrepareTriage(params) => {
                params
                    .validate()
                    .map_err(|e| Failure::new("invalid_params", e))?;
                if let Some(source) = &params.source {
                    self.inner
                        .conversation
                        .storage
                        .header(source)
                        .map_err(super::runtime::storage_failure)?;
                }
                let state = self
                    .inner
                    .projects
                    .path()
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_owned();
                let params = params.clone();
                tokio::task::spawn_blocking(move || {
                    crate::triage::prepare(&state.join("triage"), &state, &params, Some(true))
                })
                .await
                .map_err(|e| Failure::new("triage_failed", e.to_string()))?
                .map_err(|e| Failure::new("triage_failed", e.to_string()))?
                .into()
            }
            Call::PullRequests(params) => self.pull_requests(params).await?.into(),
            Call::UploadFeedback(params) => self.upload_feedback(params).await?.into(),
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
                    .owners
                    .write(&params.session, Write::Read(params.turn_id.clone()))
                    .await
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
            self.start_automations();
            let mut clones = self.inner.project_clones.subscribe();
            let owner = Arc::downgrade(&self.inner);
            tokio::spawn(async move {
                while clones.changed().await.is_ok() {
                    let state = clones.borrow_and_update().clone();
                    let Some(inner) = owner.upgrade() else { break };
                    if let Some(state) = state {
                        inner.router.broadcast(
                            agent_protocol::protocol::Notification::ProjectClones { state },
                        );
                    }
                }
            });
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
mod tests {
    #[tokio::test]
    async fn provider_switches_broadcast_and_gate_new_work_while_retaining_history_access() {
        use super::super::runtime::native_fixture::{NativeAgent, call};
        use super::*;
        use agent_protocol::{
            protocol::Notification,
            providers::{ProviderSettings, SetProviderEnabled},
        };
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let host = HostRpcService::new([agent.backend()], directory.path().join("worktrees.json"));
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
        use super::super::runtime::native_fixture::call;
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            std::iter::empty::<Backend>(),
            root.path().join("worktrees.json"),
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
        use super::super::runtime::native_fixture::{NativeAgent, call};
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        a.accounts_gate.forget_permits(1);
        b.accounts_gate.forget_permits(1);
        let host = HostRpcService::new(
            [a.backend(), b.backend()],
            directory.path().join("worktrees.json"),
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
            call::<agent_protocol::conversation::records::OpenedList>(
                &host,
                client.id(),
                Call::SubscribeList(agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None,
                }),
            ),
        )
        .await
        .expect("conversation lists must not wait for accounts")
        .unwrap();
        agent_protocol::conversation::records::validate_list(&list.initial).unwrap();
        a.accounts_gate.add_permits(1);
        b.accounts_gate.add_permits(1);
        assert!(accounts.await.unwrap().errors.is_empty());
    }

    #[tokio::test]
    async fn account_read_failures_keep_their_provider_alongside_startup_errors() {
        use super::super::runtime::native_fixture::{NativeAgent, call};
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
            directory.path().join("worktrees.json"),
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
            root.path().join("worktrees.json"),
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
        use super::*;
        use crate::host_rpc::runtime::native_fixture::NativeAgent;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [Backend::unavailable(
                ProviderKind::Claude,
                "invalid provider configuration",
            )],
            root.path().join("worktrees.json"),
        );
        assert_eq!(
            service.provider_errors()["claude"]["code"],
            "provider_unavailable"
        );
        let agent = NativeAgent::new(ProviderKind::Claude, root.path().join("claude"));
        let service = HostRpcService::new([agent.backend()], root.path().join("worktrees.json"));
        assert!(service.provider_errors().get("claude").is_none());
        let session = service.open_session();
        let response = service
            .dispatch(
                session.id(),
                &Call::CreateConversation(agent_protocol::conversation::records::Create {
                    workspace: None,
                    provider: agent_protocol::session::ProviderKind::Claude,
                    session: agent_protocol::session::SessionRef::new(
                        None,
                        uuid::Uuid::new_v4().to_string(),
                    )
                    .unwrap(),
                    subscription: uuid::Uuid::new_v4(),
                    cwd: Some(root.path().to_string_lossy().into_owned()),
                    selection: Default::default(),
                    initial: None,
                }),
            )
            .await
            .unwrap();
        let Response::Success { result: response } = agent_protocol::protocol::decode::<
            Response<agent_protocol::conversation::records::Opened>,
        >(&response.initial)
        .unwrap() else {
            panic!("provider did not recover");
        };
        assert_eq!(response.session.provider, None);
        let header = service
            .inner
            .conversation
            .storage
            .header(&response.session)
            .unwrap();
        assert_eq!(header.state.provider, ProviderKind::Claude);
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
    fn stored_header(
        provider: agent_protocol::session::ProviderKind,
        cwd: &std::path::Path,
    ) -> agent_protocol::conversation::records::Header {
        agent_protocol::conversation::records::Header {
            state: agent_protocol::conversation::Thread {
                context_start: 0,
                name: String::new(),
                workspace: agent_protocol::conversation::Workspace::Ready(
                    cwd.to_string_lossy().into_owned(),
                ),
                provider,
                queue_held: false,
            },
            details: Default::default(),
        }
    }
    #[tokio::test]
    async fn host_names_do_not_depend_on_provider_availability_and_failed_input_is_not_in_flight() {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::call;
        use agent_protocol::conversation::{Command, InputContent, records};
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [Backend::unavailable(ProviderKind::Codex, "unavailable")],
            root.path().join("projects.json"),
        );
        let target = agent_protocol::session::SessionRef::new(
            ProviderKind::Codex,
            uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        service
            .inner
            .conversation
            .storage
            .create(&target, &stored_header(ProviderKind::Codex, root.path()))
            .unwrap();
        let session = service.open_session();
        let renamed: records::Acknowledged = call(
            &service,
            session.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation: uuid::Uuid::new_v4(),
                command: Command::Rename("Renamed".into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            renamed.outcome,
            agent_protocol::conversation::Outcome::Accepted
        );
        assert_eq!(
            service
                .inner
                .conversation
                .storage
                .header(&target)
                .unwrap()
                .state
                .name,
            "Renamed"
        );
        let input = uuid::Uuid::new_v4();
        let request = Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: input,
            command: Command::Submit {
                id: input.to_string().into(),
                content: InputContent {
                    author: None,
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: "unsent".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        });
        for _ in 0..2 {
            let error = call::<records::Acknowledged>(&service, session.id(), request.clone())
                .await
                .unwrap()
                .accepted()
                .unwrap_err();
            assert_eq!(error.code, "command_rejected");
            assert_eq!(error.delivery, agent_protocol::error::Delivery::NotSent);
            assert!(
                service
                    .inner
                    .conversation
                    .storage
                    .state(&target)
                    .unwrap()
                    .inputs
                    .is_empty()
            );
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
        let service =
            super::HostRpcService::new([claude.into()], root.path().join("projects.json"));
        let before = service.storage_scope().unwrap();
        assert_eq!(before, service.inner.providers.storage_scope().unwrap());
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
            root.path().join("projects.json"),
        );
        let after = service.storage_scope().unwrap();
        assert_eq!(after, service.inner.providers.storage_scope().unwrap());
        assert_eq!(
            before, after,
            "Adding Harness must not archive existing drafts or navigation"
        );
    }
    #[tokio::test]
    async fn conversation_rpc_failures_are_logged_once_without_request_payloads() {
        use crate::HostRpcService;
        use agent_protocol::protocol::{Call, Response};
        use tracing::instrument::WithSubscriber;
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        service.set_history_scope(directory.path().to_owned(), true);
        service.prepare_update().await.unwrap();
        let log = tempfile::NamedTempFile::new().unwrap();
        let file = log.reopen().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || file.try_clone().unwrap())
            .finish();
        let client = service.open_session();
        let input = uuid::Uuid::new_v4();
        let request = Call::ChangeConversation(agent_protocol::conversation::records::Change {
            session: agent_protocol::session::SessionRef::new(
                None,
                uuid::Uuid::new_v4().to_string(),
            )
            .unwrap(),
            operation: input,
            command: agent_protocol::conversation::Command::Submit {
                id: input.to_string().into(),
                content: agent_protocol::conversation::InputContent {
                    author: None,
                    fresh_context: false,
                    parts: vec![agent_protocol::operations::Input::Text {
                        text: "private request title".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        });
        let reply = service
            .dispatch(client.id(), &request)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        let Response::Failure { error } = agent_protocol::protocol::decode::<
            Response<agent_protocol::models::Empty>,
        >(&reply.initial)
        .unwrap() else {
            panic!("updating must reject the write");
        };
        assert_eq!(error.code, "host_updating");
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(text.matches("host/conversation/change").count(), 1);
        assert!(text.contains("host_updating"));
        assert!(!text.contains("private request title"));
    }
}
