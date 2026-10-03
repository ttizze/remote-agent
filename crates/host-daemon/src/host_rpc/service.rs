use agent_protocol::session::ProviderKind;
use anyhow::Context;
use std::sync::{Arc, OnceLock};

use agent_protocol::operations as op;

use agent_protocol::models::ListQuery;

use agent_protocol::models::Thread;

use agent_protocol::models::ThreadResponse;

use agent_transport::peer::PeerEvent;

use agent_transport::peer::RpcMessage;

use agent_protocol::protocol::{Body, Call, Response};
use agent_transport::peer::{RpcMessageError, RpcMessageKind};
use codex_app_server::CodexAppServer;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::codex::ThreadListParams;

use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;

#[derive(Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub(super) struct Failure {
    code: &'static str,
    message: String,
    delivery: agent_protocol::error::Delivery,
    pub(super) execution: Option<Box<agent_protocol::execution::ExecutionError>>,
}
impl From<Failure> for agent_protocol::error::RpcFailure {
    fn from(error: Failure) -> Self {
        Self {
            code: error.code.into(),
            message: error.message,
            delivery: error.delivery,
            execution: error.execution.map(|execution| *execution),
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

impl From<crate::claude::OperationError> for Failure {
    fn from(error: crate::claude::OperationError) -> Self {
        Self {
            code: "claude_failed",
            message: error.message,
            delivery: error.delivery,
            execution: None,
        }
    }
}

impl Failure {
    pub(super) fn before_submission(mut self) -> Self {
        self.delivery = agent_protocol::error::Delivery::NotSent;
        self
    }
    pub(super) fn unknown(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: format!("{error:#}"),
            delivery: agent_protocol::error::Delivery::Unknown,
            execution: None,
        }
    }
    pub(super) fn new(code: &'static str, error: impl std::fmt::Display) -> Self {
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
    claude: OnceLock<crate::claude::Claude>,
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    codex: super::codex::Codex,
    projects: ProjectStore,
    project_creation: tokio::sync::Mutex<()>,
    router: SessionRouter,
    event_pump_started: OnceLock<()>,
    files: crate::workspace_files::WorkspaceFiles,
    worktrees: crate::worktrees::Worktrees,
    worktree_access: tokio::sync::RwLock<()>,
    permission_settings_access: tokio::sync::Mutex<()>,
    terminals: crate::terminals::Terminals,
    dictation: crate::dictation::Dictation,
}

impl HostRpcService {
    pub fn new(codex: Result<Arc<CodexAppServer>, String>, projects: ProjectStore) -> Self {
        let files = crate::workspace_files::WorkspaceFiles::new(
            projects.path().with_file_name("bex-attachments"),
        );
        Self {
            inner: Arc::new(ServiceInner {
                browser: OnceLock::new(),
                claude: OnceLock::new(),
                accounts: tokio::sync::Mutex::new(None),
                restoration_error: tokio::sync::watch::channel(None).0,
                dictation: crate::dictation::Dictation::new(codex.clone()),
                codex: super::codex::Codex::new(codex),
                worktrees: crate::worktrees::Worktrees::new(projects.path()),
                worktree_access: tokio::sync::RwLock::new(()),
                permission_settings_access: Default::default(),
                terminals: Default::default(),
                projects,
                project_creation: Default::default(),
                router: SessionRouter::new(),
                event_pump_started: OnceLock::new(),
                files,
            }),
        }
    }

    async fn account_request(
        &self,
        request: Call,
    ) -> Result<agent_protocol::protocol::Body, Failure> {
        use agent_protocol::protocol::Body;
        use agent_protocol::session::ProviderKind;
        let claude_request = match &request {
            Call::StartAccountLogin(params) => params.provider == ProviderKind::Claude,
            Call::SelectAccount(params) => params.id.starts_with("claude:"),
            Call::LogoutAccount(params) => params.id.starts_with("claude:"),
            Call::ReadAccountLogin(params) => params.id.starts_with("claude:"),
            Call::CancelAccountLogin(params) => params.id.starts_with("claude:"),
            Call::SubmitAccountLogin(params) => params.id.starts_with("claude:"),
            _ => false,
        };
        if claude_request {
            let claude = self.inner.claude.get().ok_or_else(|| {
                Failure::new("account_unavailable", "Claude が設定されていません。")
            })?;
            return claude
                .accounts
                .lock()
                .await
                .request(request)
                .await
                .map_err(|error| Failure::new("account_operation_failed", error));
        }
        let listing = matches!(request, Call::ListAccounts(_));
        let mut accounts = self.inner.accounts.lock().await;
        let result = match accounts.as_mut() {
            Some(accounts) => match self.inner.codex.server() {
                Ok(server) => accounts.request(server, request).await,
                Err(error) => Err(error.to_string()),
            },
            None => Err("Codex のアカウント管理が利用できません。".into()),
        };
        drop(accounts);
        if !listing {
            return result.map_err(|error| Failure::new("account_operation_failed", error));
        }
        let mut result = match result {
            Ok(Body::Accounts(accounts)) => *accounts,
            Ok(_) => unreachable!("account listing returns Accounts"),
            Err(error) => op::Accounts {
                accounts: Vec::new(),
                selected_id: None,
                selected_claude_id: None,
                error: Some(error),
            },
        };
        if let Some(claude) = self.inner.claude.get() {
            match claude.accounts.lock().await.list().await {
                Ok((entries, selected)) => {
                    result.accounts.extend(entries);
                    result.selected_claude_id = selected;
                }
                Err(error) => {
                    result.error = Some(match result.error {
                        Some(codex) => format!("{codex}\n{error}"),
                        None => error,
                    });
                }
            }
        }
        Ok(result.into())
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
        let accounts = crate::codex_accounts::Accounts::load(
            directory,
            config,
            self.inner
                .codex
                .server()
                .map_err(|error| error.to_string())?,
            self.inner.restoration_error.clone(),
        )
        .await?;
        *self.inner.accounts.lock().await = Some(accounts);
        Ok(())
    }

    pub async fn enable_claude(
        &self,
        program: std::path::PathBuf,
        directory: std::path::PathBuf,
        native_home: Option<std::path::PathBuf>,
    ) -> anyhow::Result<()> {
        let claude = crate::claude::Claude::load(
            program,
            directory,
            native_home,
            self.inner.router.clone(),
            self.inner.browser.get().cloned(),
        )
        .await?;
        self.inner
            .claude
            .set(claude)
            .map_err(|_| anyhow::anyhow!("Claude Code is already configured"))
    }

    pub(crate) async fn shutdown_owned_processes(&self) {
        if let Some(browser) = self.inner.browser.get() {
            browser.shutdown().await;
        }
        self.inner.terminals.shutdown().await;
        if let Some(claude) = self.inner.claude.get() {
            claude.shutdown().await;
        }
    }

    pub(crate) fn revoke_device(&self, principal: &str) {
        self.inner.terminals.revoke_device(principal);
    }
    pub fn open_session(&self) -> HostSession {
        self.start_codex_event_pump();
        self.inner.router.open_session()
    }

    pub(crate) fn open_authenticated_session(&self, principal: String) -> HostSession {
        self.start_codex_event_pump();
        self.inner
            .router
            .open_authenticated_session(Some(principal))
    }

    pub fn close_session(&self, session: SessionId) {
        self.inner.router.close_session(session);
        self.inner.terminals.close_session(session);
        self.inner.files.clear_session(session);
    }

    pub(crate) fn data_recipients(&self) -> (Vec<String>, Option<String>) {
        let mut ai = Vec::new();
        let transcription = if self.inner.codex.server().is_ok() {
            ai.push("OpenAI".into());
            Some("OpenAI".into())
        } else {
            None
        };
        if self.inner.claude.get().is_some() {
            ai.push("Anthropic".into());
        }
        (ai, transcription)
    }

    pub(crate) fn provider_errors(&self) -> serde_json::Value {
        match self.inner.codex.server() {
            Ok(_) => serde_json::json!({}),
            Err(error) => serde_json::json!({"codex":error}),
        }
    }

    pub fn start(&self) {
        self.start_codex_event_pump();
    }

    /// Dispatch a classified message from an authenticated session.
    pub async fn dispatch(&self, session: SessionId, message: &Call) -> Result<HostReply, String> {
        self.inner.router.ensure_session(session)?;
        if let Call::OpenSession(params) = message {
            return self.session_open(session, params).await;
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
                    | Call::CreateSession(_)
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
        Ok(Response::from_result(result)
            .map_err(invalid_message)?
            .into())
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
        if target.provider == ProviderKind::Codex
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return Err(Failure::new("account_unavailable", error));
        }
        let claude = if target.provider == ProviderKind::Claude {
            Some(
                self.inner
                    .claude
                    .get()
                    .ok_or_else(|| Failure::new("claude_unavailable", "Claude is unavailable"))?,
            )
        } else {
            None
        };
        // Loaded state is native routing evidence; it is not a BEX session status.
        let (mut response, requires_resume) = match claude {
            Some(claude) => (
                claude
                    .read(&target.id, 1)
                    .await
                    .map_err(|e| Failure::new("session_read_failed", e))?,
                false,
            ),
            None => self.inner.codex.submission_state(&target.id).await?,
        };
        if response.thread.id.as_ref() != Some(target) {
            return Err(Failure::new(
                "invalid_thread",
                "native session identity changed",
            ));
        }
        self.inner.router.overlay_execution(target, &mut response);
        use super::submission::{SubmissionTarget, submission_target};
        let route = submission_target(
            response.thread.status,
            response.thread.turns.as_deref().unwrap_or_default(),
            response.thread.cwd.as_deref(),
        )
        .map_err(|error| Failure::new("submission_unavailable", error))?;
        let turn_id = match route {
            SubmissionTarget::Steer(turn_id) => {
                if let Some(claude) = claude {
                    claude
                        .additional_input(
                            &target.id,
                            Some(turn_id),
                            &input.input,
                            &input.client_user_message_id,
                        )
                        .await
                        .map_err(Failure::from)?;
                } else {
                    self.inner
                        .codex
                        .steer(
                            &target.id,
                            turn_id,
                            &input.client_user_message_id,
                            &input.input,
                        )
                        .await?;
                }
                Some(turn_id.into())
            }
            SubmissionTarget::Queue => {
                if let Some(claude) = claude {
                    claude
                        .additional_input(
                            &target.id,
                            None,
                            &input.input,
                            &input.client_user_message_id,
                        )
                        .await
                        .map_err(Failure::from)?;
                } else {
                    self.inner
                        .codex
                        .queue_input(&target.id, &input.client_user_message_id, &input.input)
                        .await?;
                }
                None
            }
            SubmissionTarget::Start { cwd } => {
                let recreated = self
                    .inner
                    .worktrees
                    .ensure_available(cwd)
                    .await
                    .map_err(|error| Failure::new("worktree_creation_failed", error))?;
                if let Some(directory) = &recreated
                    && let Some(claude) = self.inner.claude.get()
                {
                    claude
                        .discard_workspace_processes(directory)
                        .await
                        .map_err(|error| Failure::new("workspace_resume_failed", error))?;
                }
                Some(if let Some(claude) = claude {
                    claude
                        .start_turn(&target.id, input)
                        .await
                        .map_err(Failure::from)?
                } else {
                    self.inner
                        .codex
                        .start_turn(
                            input,
                            cwd,
                            requires_resume || recreated.is_some(),
                            self.browser_config(&target.to_string())?,
                        )
                        .await?
                })
            }
        };
        Ok(op::SubmissionReceipt { turn_id })
    }

    async fn answer_request(
        &self,
        session: SessionId,
        id: agent_protocol::ids::RequestId,
        answer: agent_protocol::requests::Answer,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        use super::requests::RequestDestination;
        use agent_protocol::session::RequestDelivery;
        let (origin, result) = self
            .inner
            .router
            .claim_response(session, &id, &answer)
            .map_err(|error| Failure::new("invalid_answer", error))?;
        // Until a native command is admitted, cancellation is proven not sent.
        let mut delivery = scopeguard::guard((id, RequestDelivery::Awaiting), |(id, state)| {
            self.inner.router.response_delivery(&id, state)
        });
        match origin.destination {
            RequestDestination::Claude { input } => {
                let request_id = origin.native_id.as_str().ok_or_else(|| {
                    Failure::new("invalid_answer", "Claude request ID must be a string")
                })?;
                let permit = input
                    .reserve()
                    .await
                    .map_err(|_| Failure::new("answer_not_sent", "Claude Code input is closed"))?;
                let (delivered, receipt) = tokio::sync::oneshot::channel();
                delivery.1 = RequestDelivery::Unknown;
                permit.send(crate::claude::Command {
                    value: serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":result}}),
                    user: None,
                    delivered: Some(delivered),
                });
                tokio::time::timeout(std::time::Duration::from_secs(15), receipt)
                    .await
                    .map_err(|_| "Claude answer delivery timed out".to_owned())
                    .and_then(|receipt| {
                        receipt.map_err(|_| {
                            "Claude exited before confirming the answer write".to_owned()
                        })
                    })
                    .flatten()
                    .map_err(|error| Failure::unknown("answer_delivery_unknown", error))?;
            }
            RequestDestination::Codex { .. } => {
                let codex = self
                    .inner
                    .codex
                    .server()
                    .map_err(|error| Failure::new("answer_not_sent", error))?;
                if origin.instance != self.inner.codex.instance {
                    return Err(Failure::new(
                        "answer_not_sent",
                        "request source has changed",
                    ));
                }
                delivery.1 = RequestDelivery::Unknown;
                codex
                    .send_raw(
                        &serde_json::json!({"id":origin.native_id,"result":result}).to_string(),
                    )
                    .await
                    .map_err(|error| Failure::unknown("answer_delivery_unknown", error))?;
            }
        }
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
            if target.provider == agent_protocol::session::ProviderKind::Codex {
                self.inner.codex.server()?;
            }
            let limit = params.limit;
            let read = self
                .inner
                .router
                .retain_execution(target.clone())
                .map_err(anyhow::Error::msg)?;
            let started = std::time::Instant::now();
            let mut response = match target.provider {
                agent_protocol::session::ProviderKind::Claude => {
                    self.inner
                        .claude
                        .get()
                        .context("Claude is unavailable")?
                        .read(&target.id, limit)
                        .await?
                }
                agent_protocol::session::ProviderKind::Codex => {
                    self.inner.codex.read(&target.id, limit).await?
                }
            };
            let native_ms = started.elapsed().as_millis();
            if response.thread.id.as_ref() != Some(&target) {
                return Err(anyhow::anyhow!("native session identity changed"));
            }
            describe_thread(
                &mut response.thread,
                target.provider,
                &self.project_snapshot().await?,
            );
            let project_ms = started.elapsed().as_millis() - native_ms;
            let more = response.thread.history_has_more == Some(true)
                || response
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.items_has_more == Some(true));
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
                Ok(Response::error("session_open_failed", &message)
                    .map_err(invalid_message)?
                    .into())
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
                turn.items?
                    .into_iter()
                    .find(|item| item.id == params.item_id)
            });
        let mut response = if let Some(item) = live {
            agent_protocol::operations::ItemResponse {
                item: Arc::unwrap_or_clone(item),
                transfer: None,
            }
        } else if target.provider == agent_protocol::session::ProviderKind::Claude {
            let claude = self
                .inner
                .claude
                .get()
                .ok_or_else(|| Failure::new("claude_unavailable", "Claude is unavailable"))?;
            claude
                .read_item(&target.id, params)
                .await
                .map_err(|error| Failure::new("item_read_failed", error))?
        } else {
            let params = op::ReadItem {
                thread_id: target.clone(),
                turn_id: params.turn_id.clone(),
                item_id: params.item_id.clone(),
            };
            self.inner.codex.item_read(params).await?
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
            let capabilities = provider_capabilities(target.provider);
            if matches!(request, Call::ForkSession(_)) && !capabilities.fork
                || matches!(request, Call::RenameSession(_)) && !capabilities.rename
            {
                return Err(Failure::new(
                    "unsupported_operation",
                    format!("{method} is unsupported by this provider"),
                ));
            }
            if target.provider == ProviderKind::Claude {
                let claude =
                    self.inner.claude.get().ok_or_else(|| {
                        Failure::new("claude_unavailable", "Claude is unavailable")
                    })?;
                return match request {
                    Call::Interrupt(params) => claude
                        .interrupt(&target.id, &params.turn_id)
                        .await
                        .map(|()| agent_protocol::models::Empty {}.into())
                        .map_err(Failure::from),
                    _ => Err(Failure::new(
                        "unsupported_operation",
                        format!("{method} is unsupported by this provider"),
                    )),
                };
            }
        }
        let response = match request {
            Call::ReadPermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                match params.provider {
                    ProviderKind::Codex => self.inner.codex.read_permissions().await?,
                    ProviderKind::Claude => super::permissions::read_claude_permissions(
                        self.inner
                            .claude
                            .get()
                            .ok_or_else(|| {
                                Failure::new("claude_unavailable", "Claude が設定されていません。")
                            })?
                            .storage_directory(),
                    )?,
                }
                .into()
            }
            Call::UpdatePermissionSettings(params) => {
                let _guard = self.inner.permission_settings_access.lock().await;
                match params.provider {
                    ProviderKind::Codex => {
                        self.inner
                            .codex
                            .update_permissions(params.mode, &params.version)
                            .await?
                    }
                    ProviderKind::Claude => super::permissions::update_claude_permissions(
                        self.inner
                            .claude
                            .get()
                            .ok_or_else(|| {
                                Failure::new("claude_unavailable", "Claude が設定されていません。")
                            })?
                            .storage_directory(),
                        params.mode,
                        &params.version,
                    )?,
                }
                .into()
            }
            Call::ComposerCatalog(params) => {
                self.inner.codex.composer_catalog(&params.cwd).await.into()
            }
            Call::ReadAccountUsage(params) => {
                let usage = if params.id.starts_with("claude:") {
                    let claude = self.inner.claude.get().ok_or_else(|| {
                        Failure::new("account_unavailable", "Claude が設定されていません。")
                    })?;
                    let fetch = claude
                        .accounts
                        .lock()
                        .await
                        .usage_request(&params.id)
                        .map_err(|error| Failure::new("account_operation_failed", error))?;
                    fetch.await
                } else {
                    let fetch = self
                        .inner
                        .accounts
                        .lock()
                        .await
                        .as_mut()
                        .ok_or_else(|| {
                            Failure::new(
                                "account_unavailable",
                                "Codex のアカウント管理が利用できません。",
                            )
                        })?
                        .usage_request(&params.id)
                        .map_err(|error| Failure::new("account_operation_failed", error))?;
                    fetch.await
                };
                usage.into()
            }
            Call::ListAccounts(_)
            | Call::SelectAccount(_)
            | Call::LogoutAccount(_)
            | Call::StartAccountLogin(_)
            | Call::ReadAccountLogin(_)
            | Call::SubmitAccountLogin(_)
            | Call::CancelAccountLogin(_) => self.account_request(request.clone()).await?,

            Call::ListModels(params) => {
                let first_page = params.cursor.is_none();
                let (codex, claude) = tokio::join!(self.inner.codex.models(params), async {
                    if first_page && let Some(claude) = self.inner.claude.get() {
                        claude.models().await
                    } else {
                        Ok(Vec::new())
                    }
                });
                let mut page = codex.unwrap_or_else(|error| op::ModelPage {
                    data: Vec::new(),
                    next_cursor: None,
                    provider_errors: Some(serde_json::Map::from_iter([(
                        "codex".into(),
                        serde_json::to_value(error).expect("Failure serializes"),
                    )])),
                });
                match claude {
                    Ok(models) => page.data.extend(models),
                    Err(error) => {
                        let errors = page.provider_errors.get_or_insert_default();
                        errors.insert(
                            "claude".into(),
                            serde_json::json!({"message":format!("{error:#}")}),
                        );
                    }
                }
                if first_page && page.data.is_empty() && page.provider_errors.is_some() {
                    return Err(Failure::new(
                        "models_unavailable",
                        serde_json::to_value(&page.provider_errors)?,
                    ));
                } else {
                    page.into()
                }
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
                let codex = self
                    .inner
                    .projects
                    .path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let path = canonical_storage_path;
                let areas = serde_json::json!({"codex":path(codex),"claude":self.inner.claude.get().map(|claude| path(claude.storage_directory()))});
                let digest =
                    ring::digest::digest(&ring::digest::SHA256, areas.to_string().as_bytes());
                let scope: String = digest
                    .as_ref()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
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
                .add_project(&params.cwd)
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
                (self.remove_worktree(params.clone()).await?).into()
            }

            Call::CreateSession(params) => (self.create_session(params.clone()).await?).into(),
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
            Call::Transcribe(params) => (self
                .inner
                .dictation
                .transcribe(&params.audio)
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
                let scope = uuid::Uuid::new_v4().to_string();
                let mut response = self
                    .inner
                    .codex
                    .fork(
                        &target.id,
                        &params.last_turn_id,
                        params.exclude_turns,
                        self.browser_config(&scope)?,
                    )
                    .await?;
                if let Some(browser) = self.inner.browser.get()
                    && let Some(id) = &response.thread.id
                {
                    browser.bind_scope(scope, id.to_string()).await;
                }
                describe_thread(
                    &mut response.thread,
                    ProviderKind::Codex,
                    &self.project_snapshot().await?,
                );
                response.into()
            }
            Call::Interrupt(params) => self
                .inner
                .codex
                .interrupt(
                    &target_session.expect("session-scoped interrupt").id,
                    &params.turn_id,
                )
                .await?
                .into(),
            Call::RenameSession(params) => self
                .inner
                .codex
                .rename(
                    &target_session.expect("session-scoped rename").id,
                    &params.name,
                )
                .await?
                .into(),
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
        let mut params = ThreadListParams {
            limit: 100,
            sort_key: "updated_at",
            sort_direction: "desc",
            use_state_db_only: true,
            search_term: None,
            cursor: None,
        };
        let mut cursors = std::collections::HashSet::new();
        let mut threads = match self.inner.claude.get() {
            Some(claude) => claude
                .list("")
                .await
                .map_err(|error| Failure::new("claude_history_unavailable", error))?,
            None => Vec::new(),
        };
        loop {
            let page = match self.inner.codex.thread_page(&params).await {
                Ok(page) => page,
                Err(error) => {
                    for worktree in &mut worktrees {
                        worktree.blocked_reason = Some(format!(
                            "Codexの稼働状況を確認できないため削除できません: {error}"
                        ));
                    }
                    break;
                }
            };
            threads.extend(page.data);
            params.cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            let Some(cursor) = &params.cursor else {
                break;
            };
            if !cursors.insert(cursor.clone()) {
                return Err(Failure::new(
                    "invalid_thread_list",
                    "thread list cursor repeated",
                ));
            }
        }
        for thread in threads {
            let Some(cwd) = thread.cwd.as_deref() else {
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
                let active = thread.status == agent_protocol::models::SessionStatus::Running;
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

    async fn remove_worktree(&self, params: op::RemoveWorktree) -> Result<(), Failure> {
        let entries = self.worktree_list().await?;
        let entry = entries
            .iter()
            .find(|entry| entry.path == params.path)
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
            .remove(params.path)
            .await
            .map_err(|error| Failure::new("worktree_remove_failed", error))
    }

    async fn project_snapshot(&self) -> Result<crate::projects::state::Snapshot, Failure> {
        let projects = if self.inner.codex.server().is_ok() {
            Some(self.inner.codex.projects().await?)
        } else {
            None
        };
        self.inner
            .projects
            .load(projects)
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))
    }

    async fn add_project(&self, cwd: &str) -> anyhow::Result<String> {
        anyhow::ensure!(
            std::path::Path::new(cwd).is_absolute(),
            "project directory must be absolute"
        );
        let root = dunce::simplified(&tokio::fs::canonicalize(cwd).await?).to_owned();
        anyhow::ensure!(
            tokio::fs::metadata(&root).await?.is_dir(),
            "project path must be a directory"
        );
        let _registration = self.inner.project_creation.lock().await;
        if !self.project_snapshot().await?.has_root(&root) {
            self.inner.codex.create_project(&root).await?;
        }
        root.into_os_string()
            .into_string()
            .map_err(|_| anyhow::anyhow!("project path is not UTF-8"))
    }

    async fn host_title_list(
        &self,
        query: ListQuery,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let mut params = ThreadListParams {
            limit: 100,
            sort_key: "updated_at",
            sort_direction: "desc",
            use_state_db_only: true,
            search_term: (!query.search_term.trim().is_empty())
                .then_some(query.search_term.as_str()),
            cursor: None,
        };
        let claude_listing = async {
            match self.inner.claude.get() {
                Some(claude) => claude.list(&query.search_term).await,
                None => Ok(Vec::new()),
            }
        };
        // Membership is needed to assemble the list, not to fetch provider metadata.
        // Start all reads together so opening the app does not pay their latency in series.
        let (snapshot, (claude_result, codex_result)) =
            tokio::try_join!(self.project_snapshot(), async {
                Ok(tokio::join!(
                    tokio::time::timeout(std::time::Duration::from_secs(5), claude_listing),
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        self.inner.codex.thread_page(&params)
                    ),
                ))
            })?;
        let mut titles = crate::projects::titles::TitleList::new(&snapshot.projects, &query);
        let mut provider_errors = serde_json::Map::new();
        let mut claude_threads = match claude_result.unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "Claude listing timed out; results are partial"
            ))
        }) {
            Ok(threads) => threads,
            Err(error) => {
                provider_errors.insert(
                    "claude".into(),
                    serde_json::json!({"message":format!("{error:#}")}),
                );
                Vec::new()
            }
        }
        .into_iter()
        .peekable();
        let mut first_page = Some(codex_result.unwrap_or_else(|_| {
            Err(Failure::new(
                "provider_timeout",
                "Codex listing timed out; results are partial",
            ))
        }));
        let mut cursors = std::collections::HashSet::new();
        loop {
            // DB metadata avoids scanning or repairing the rollout. Every page
            // uses one Desktop snapshot and the same membership decisions.
            let result = match first_page.take() {
                Some(result) => result,
                None => self.inner.codex.thread_page(&params).await,
            };
            let page = match result {
                Ok(page) => page,
                Err(error) if self.inner.claude.get().is_some() => {
                    provider_errors.insert(
                        "codex".into(),
                        serde_json::to_value(error).expect("Failure serializes"),
                    );
                    break;
                }
                Err(error) => return Err(error),
            };
            for mut thread in page.data {
                describe_thread(
                    &mut thread,
                    agent_protocol::session::ProviderKind::Codex,
                    &snapshot,
                );
                while claude_threads.peek().is_some_and(|claude| {
                    crate::claude::updated_at(claude) >= crate::claude::updated_at(&thread)
                }) {
                    let mut claude = claude_threads.next().unwrap();
                    describe_thread(
                        &mut claude,
                        agent_protocol::session::ProviderKind::Claude,
                        &snapshot,
                    );
                    titles.push(claude);
                }
                titles.push(thread);
            }
            params.cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            if titles.complete() || params.cursor.is_none() {
                break;
            }
            if !cursors.insert(params.cursor.as_ref().unwrap().clone()) {
                return Err(Failure::new(
                    "invalid_thread_list",
                    "thread list cursor repeated",
                ));
            }
        }
        for mut thread in claude_threads {
            describe_thread(
                &mut thread,
                agent_protocol::session::ProviderKind::Claude,
                &snapshot,
            );
            titles.push(thread);
        }
        let mut page = titles.finish();
        let statuses = crate::worktrees::directory_statuses(
            page.data
                .iter()
                .filter_map(|thread| thread.cwd.clone())
                .collect(),
        )
        .await
        .map_err(|error| Failure::new("worktree_status_failed", error))?;
        for thread in &mut page.data {
            thread.worktree_status = thread
                .cwd
                .as_ref()
                .and_then(|cwd| statuses.get(cwd))
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
        if provider == agent_protocol::session::ProviderKind::Codex {
            self.inner.codex.server()?;
        }
        let project_id = if let Some(cwd) = params.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            self.project_snapshot()
                .await?
                .project_for_workspace(cwd)
                .map(str::to_owned)
        } else {
            None
        };
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
        let mut response = if provider == agent_protocol::session::ProviderKind::Claude {
            let claude = self.inner.claude.get().ok_or_else(|| {
                Failure::new(
                    "claude_unavailable",
                    "このHostではClaude Codeが有効になっていません。",
                )
            })?;
            let model = params
                .model
                .as_ref()
                .map_or("default", |model| model.id.as_str());
            claude
                .create(params.cwd.as_deref().unwrap_or_default(), model)
                .await
                .map_err(|error| Failure::new("claude_unavailable", error))?
        } else {
            let scope = uuid::Uuid::new_v4().to_string();
            let response = self
                .inner
                .codex
                .create(
                    params.cwd.as_deref(),
                    params.model.as_ref().map(|model| model.id.as_str()),
                    project_id.as_deref(),
                    self.browser_config(&scope)?,
                )
                .await?;
            if let Some(browser) = self.inner.browser.get()
                && let Some(id) = &response.thread.id
            {
                browser.bind_scope(scope, id.to_string()).await;
            }
            response
        };
        describe_thread(
            &mut response.thread,
            provider,
            &self.project_snapshot().await?,
        );
        Ok(response)
    }

    fn start_codex_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Subscribe before spawning the pump. Otherwise Codex can emit a
        // server request in the scheduling gap and it would be lost before
        // there is a receiver to retain it for the next phone.
        let Ok(codex) = &self.inner.codex.process else {
            return;
        };
        let mut events = codex.subscribe();
        let router = self.inner.router.clone();
        let stopped = self.inner.codex.stopped.clone();
        let instance = self.inner.codex.instance;
        let processed = self.inner.codex.processed.clone();
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(PeerEvent::Message(message)) => {
                        let sequence = message.sequence;
                        let _processed = scopeguard::guard(sequence, |sequence| {
                            processed.send_replace(sequence);
                        });
                        let line = message.value;
                        let Ok(request) = RpcMessage::parse(&line) else {
                            continue;
                        };
                        if request.kind() == RpcMessageKind::Request
                            && request.method() == Some("account/chatgptAuthTokens/refresh")
                        {
                            let Some(inner) = inner.upgrade() else {
                                return;
                            };
                            tokio::spawn(async move {
                                #[derive(Deserialize)]
                                #[serde(rename_all = "camelCase")]
                                struct Refresh<'a> {
                                    previous_account_id: Option<&'a str>,
                                }
                                let Ok(request) = RpcMessage::parse(&line) else {
                                    return;
                                };
                                let Ok(params) = request.params::<Refresh>() else {
                                    return;
                                };
                                let mut accounts = inner.accounts.lock().await;
                                let result = match accounts.as_mut() {
                                    Some(accounts) => {
                                        accounts.refresh(params.previous_account_id).await
                                    }
                                    None => Err("アカウントを選択してください。".into()),
                                };
                                let response = match result {
                                    Ok(result) => request.response::<_, ()>(Ok(result)),
                                    Err(error) => request.error(-32000, &error),
                                };
                                let Ok(response) = response else {
                                    return;
                                };
                                let line = zeroize::Zeroizing::new(response);
                                if let Ok(codex) = &inner.codex.process {
                                    let _ = codex.send_raw(&line).await;
                                }
                            });
                        } else if request.kind() == RpcMessageKind::Request {
                            let admission = serde_json::from_str(&line)
                                .map_err(|error| error.to_string())
                                .and_then(|request| {
                                    super::codex::request(
                                        &router,
                                        instance,
                                        stopped.clone(),
                                        request,
                                    )
                                });
                            if let Err(error) = &admission {
                                tracing::warn!(target: "bex", operation = "host.codex.request_rejected", message = %error);
                            }
                            if let Err(error) = admission
                                && let Some(inner) = inner.upgrade()
                                && let Ok(codex) = &inner.codex.process
                                && let Ok(response) = request.error(-32000, &error)
                            {
                                let _ = codex.send_raw(&response).await;
                            }
                        } else if let Err(error) = super::codex::event(&router, instance, &request)
                        {
                            tracing::error!(target: "bex", operation = "host.codex.event", message = %error);
                            break;
                        }
                    }
                    Ok(PeerEvent::Response { sequence, .. }) => {
                        processed.send_replace(sequence);
                    }
                    Ok(PeerEvent::Closed(_))
                    | Err(broadcast::error::RecvError::Closed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => break,
                }
            }
            stopped.cancel();
            if let Some(inner) = inner.upgrade()
                && let Ok(codex) = &inner.codex.process
                && let Err(error) = codex.shutdown().await
            {
                tracing::error!(target: "bex", operation = "host.codex.shutdown", message = %error);
            }
            router.fail_provider(agent_protocol::session::ProviderKind::Codex,
                            "Codexとの接続が終了したため、この実行は継続できません。Hostを再起動してから再送信してください。");
        });
    }
}

fn invalid_message(error: impl std::fmt::Display) -> String {
    format!("invalid request: {error}")
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
        Call::RenameSession(p) => (Some(&p.thread_id), None),
        _ => (None, None),
    }
}

fn provider_capabilities(
    provider: agent_protocol::session::ProviderKind,
) -> agent_protocol::session::Capabilities {
    match provider {
        agent_protocol::session::ProviderKind::Codex => super::codex::Codex::capabilities(),
        agent_protocol::session::ProviderKind::Claude => crate::claude::Claude::capabilities(),
    }
}

fn describe_thread(
    thread: &mut Thread,
    provider: agent_protocol::session::ProviderKind,
    projects: &crate::projects::state::Snapshot,
) {
    thread.project_id = projects.project_membership(thread.cwd.as_deref(), &thread.project_id);
    thread.capabilities = Some(provider_capabilities(provider));
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn answers_keep_delivery_evidence_until_the_source_resolves_them() {
        use super::super::requests::{RequestDestination, RequestOrigin};
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
            let mut response = ThreadResponse {
                thread: Thread::default(),
                model: None,
            };
            router.overlay_execution(&target, &mut response);
            response
                .thread
                .requests
                .get(id)
                .map(|request| request.delivery)
        };
        for native in ["cancelled", "interrupted", "written"] {
            let adapted = super::super::requests::claude(uuid::Uuid::new_v4().to_string().into(), &"unrelated".into(), &serde_json::json!({"subtype":"elicitation","mcp_server_name":"server","requested_schema":{"type":"object","properties":{}}})).unwrap();
            let id = adapted.request.id.clone();
            router
                .request(
                    target.clone(),
                    RequestOrigin {
                        instance,
                        native_id: serde_json::json!(native),
                        destination: RequestDestination::Claude {
                            input: input.clone(),
                        },
                    },
                    adapted,
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
            router.resolve_native_request(instance, &serde_json::json!(native));
            assert_eq!(delivery(&id), None);
        }
    }

    #[tokio::test]
    async fn answer_preflight_failures_keep_awaiting_and_prove_non_delivery() {
        use super::super::requests::{RequestDestination, RequestOrigin};
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
            );
            let connection = service.open_session();
            let claude = case.ends_with("Claude") || case == "invalidClaudeId";
            let provider = if claude {
                ProviderKind::Claude
            } else {
                ProviderKind::Codex
            };
            let target =
                agent_protocol::session::SessionRef::new(provider, "native".into()).unwrap();
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
            let destination = if claude {
                RequestDestination::Claude { input }
            } else {
                RequestDestination::Codex {
                    stopped: stopped.clone(),
                }
            };
            service
                .inner
                .router
                .request(
                    target.clone(),
                    RequestOrigin {
                        instance,
                        native_id: native_id.clone(),
                        destination,
                    },
                    adapted,
                )
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
                let mut response = ThreadResponse {
                    thread: Thread::default(),
                    model: None,
                };
                service
                    .inner
                    .router
                    .overlay_execution(&target, &mut response);
                assert_eq!(
                    response.thread.requests[&id].delivery,
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
        let service = HostRpcService::new(
            Err("not available".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session();
        for (method, expected) in [
            ("host/session/fork", "unsupported_operation"),
            ("host/session/rename", "unsupported_operation"),
            ("host/session/submit", "claude_unavailable"),
        ] {
            let call = agent_protocol::protocol::json_boundary::call(method, serde_json::json!({"threadId":{"provider":"claude","id":"native"},"clientUserMessageId":method,"lastTurnId":"turn","excludeTurns":false,"name":"Renamed","input":[],"expectedTurnId":"turn"})).unwrap();
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
                assert_eq!(response["error"]["code"], "codex_unavailable");
                assert_eq!(response["error"]["delivery"], "notSent");
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
        let service = HostRpcService::new(
            Err("unavailable".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        service
            .enable_claude(
                root.path().join("does-not-exist"),
                root.path().join("state"),
                Some(native),
            )
            .await
            .unwrap();
        let session = service.open_session();
        let call =
            agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
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
    fn creating_native_storage_does_not_change_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native").join("projects");
        let before = super::canonical_storage_path(&path);
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(before, super::canonical_storage_path(&path));
    }
}
