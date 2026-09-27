use anyhow::Context;
use std::sync::{Arc, OnceLock};

use agent_protocol::operations as op;

use agent_protocol::models::ListQuery;

use agent_protocol::models::Thread;

use agent_protocol::models::ThreadResponse;

use agent_protocol::protocol::{Body, Call, Response};
use agent_transport::peer::{PeerEvent, RpcMessage, RpcMessageKind};
use codex_app_server::CodexAppServer;
use serde::Deserialize;
use tokio::sync::broadcast;

use super::codex::ThreadListParams;
use super::failure::Failure;
use super::routing::{HostReply, HostSession, SessionId, SessionRouter};
use crate::ProjectStore;

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
                let mut config = browser
                    .provider_config(thread)
                    .map_err(|e| Failure::new("browser_unavailable", e))?;
                config["tool_timeout_sec"] = 1800.into();
                Ok(serde_json::json!({"mcp_servers.bex_browser":config}))
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

    pub(crate) async fn revoke_device(&self, principal: &str) {
        self.inner.terminals.revoke_device(principal);
        if let Some(browser) = self.inner.browser.get() {
            browser.revoke_device(principal).await;
        }
    }
    pub fn open_session(&self, capacity: usize) -> HostSession {
        self.start_codex_event_pump();
        self.inner.router.open_session(capacity)
    }

    pub(crate) fn open_authenticated_session(
        &self,
        capacity: usize,
        principal: String,
    ) -> HostSession {
        self.start_codex_event_pump();
        self.inner
            .router
            .open_authenticated_session(capacity, Some(principal))
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
            let (thread_id, input_id) = match message.session_scope() {
                Some((thread, input)) => (Some(thread), input),
                None => (None, None),
            };
            let target = thread_id
                .map(agent_protocol::session::SessionRef::from_thread_id)
                .transpose()
                .map_err(|error| Failure::new("invalid_params", error))?;
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
                    | Call::QueueTurn(_)
                    | Call::StartThread(_)
                    | Call::ResumeThread(_)
                    | Call::StartTurn(_)
                    | Call::SteerTurn(_)
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
                self.submit_input(session, target.as_ref().expect("submission target"), input)
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
                    Ok(Body::Started(receipt)) => SubmissionDelivery::Accepted {
                        turn_id: Some(receipt.turn.id.clone()),
                    },
                    Ok(_) => SubmissionDelivery::Accepted {
                        turn_id: match message {
                            Call::SteerTurn(input) => Some(input.expected_turn_id.clone()),
                            _ => None,
                        },
                    },
                    Err(error) if error.delivery() == agent_transport::peer::Delivery::NotSent => {
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
        session: SessionId,
        target: &agent_protocol::session::SessionRef,
        input: &agent_protocol::operations::Submission,
    ) -> Result<agent_protocol::operations::SubmissionReceipt, Failure> {
        use agent_protocol::operations::{
            QueueTurn, ResumeThread, StartTurn, SteerTurn, SubmissionReceipt,
        };
        let mut response = match target.provider {
            agent_protocol::session::ProviderKind::Codex => self
                .inner
                .codex
                .request(
                    "thread/read",
                    &serde_json::json!({"threadId":target.id,"includeTurns":false}),
                )
                .await
                .map_err(Failure::before_submission)?,
            agent_protocol::session::ProviderKind::Claude => self
                .inner
                .claude
                .get()
                .ok_or_else(|| Failure::new("claude_unavailable", "Claude is unavailable"))?
                .read(&target.id, 1)
                .await
                .map_err(|e| Failure::new("session_read_failed", e))?,
        };
        let expected = match target.provider {
            agent_protocol::session::ProviderKind::Codex => target.id.as_str(),
            agent_protocol::session::ProviderKind::Claude => input.thread_id.as_str(),
        };
        if response.thread.id.as_deref() != Some(expected) {
            return Err(Failure::new(
                "invalid_thread",
                "native session identity changed",
            ));
        }
        self.inner.router.overlay_execution(target, &mut response);
        let thread = &response.thread;
        use agent_protocol::session::{SubmissionTarget, submission_target};
        let route = submission_target(
            thread.status.as_ref(),
            thread.turns.as_deref().unwrap_or_default(),
            thread.cwd.as_deref(),
        )
        .map_err(|error| Failure::new("submission_unavailable", error))?;
        let call = match route {
            SubmissionTarget::Steer(turn_id) => Call::SteerTurn(SteerTurn {
                thread_id: input.thread_id.clone(),
                client_user_message_id: input.client_user_message_id.clone(),
                input: input.input.clone(),
                expected_turn_id: turn_id.into(),
            }),
            SubmissionTarget::Queue => Call::QueueTurn(QueueTurn {
                thread_id: input.thread_id.clone(),
                client_user_message_id: input.client_user_message_id.clone(),
                input: input.input.clone(),
            }),
            SubmissionTarget::Start { cwd, resume } => {
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
                if resume || recreated.is_some() {
                    self.request(
                        session,
                        &Call::ResumeThread(ResumeThread {
                            thread_id: input.thread_id.clone(),
                            cwd: Some(cwd.into()),
                        }),
                        Some(target),
                    )
                    .await
                    .map_err(Failure::before_submission)?;
                }
                Call::StartTurn(StartTurn {
                    thread_id: input.thread_id.clone(),
                    client_user_message_id: input.client_user_message_id.clone(),
                    input: input.input.clone(),
                    model: input.model.clone(),
                    effort: input.effort.clone(),
                    service_tier: input.service_tier.clone(),
                })
            }
        };
        let reply = self.request(session, &call, Some(target)).await?;
        let turn_id = match (&call, reply) {
            (Call::StartTurn(params), Body::Started(reply)) => {
                agent_protocol::operations::RpcMethod::validate(params, &reply)
                    .map_err(|error| Failure::unknown("invalid_submission_reply", error))?;
                Some(reply.turn.id)
            }
            (Call::SteerTurn(params), Body::Empty(_)) => Some(params.expected_turn_id.clone()),
            (Call::QueueTurn(params), Body::Queued(reply)) => {
                agent_protocol::operations::RpcMethod::validate(params, &reply)
                    .map_err(|error| Failure::unknown("invalid_submission_reply", error))?;
                None
            }
            _ => {
                return Err(Failure::unknown(
                    "invalid_submission_reply",
                    "provider returned an invalid submission result",
                ));
            }
        };
        Ok(SubmissionReceipt { turn_id })
    }

    async fn answer_request(
        &self,
        session: SessionId,
        id: serde_json::Value,
        result: serde_json::Value,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let id = id.to_string();
        let (provider, native) = self
            .inner
            .router
            .claim_response(session, &id, &result)
            .map_err(|error| Failure::new("invalid_answer", error))?;
        let sent = if provider == agent_protocol::session::ProviderKind::Claude {
            match self.inner.claude.get() {
                Some(claude) => {
                    claude
                        .respond(
                            native.as_str().ok_or_else(|| {
                                Failure::new("invalid_answer", "Claude request ID must be a string")
                            })?,
                            &result,
                        )
                        .await
                }
                None => Err("Claude is unavailable".into()),
            }
        } else {
            match self.inner.codex.server() {
                Ok(codex) => codex
                    .send_raw(&serde_json::json!({"id":native,"result":result}).to_string())
                    .await
                    .map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            }
        };
        if let Err(error) = sent {
            self.inner.router.response_unknown(&id);
            return Err(Failure::unknown("answer_delivery_unknown", error));
        }
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
            let expected = match target.provider {
                agent_protocol::session::ProviderKind::Codex => target.id.clone(),
                agent_protocol::session::ProviderKind::Claude => target.thread_id(),
            };
            if response.thread.id.as_deref() != Some(expected.as_str()) {
                return Err(anyhow::anyhow!("native session identity changed"));
            }
            response.thread.session = Some(target.clone());
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
                thread_id: target.id.clone(),
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
            response.item.retain_header();
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
            let principal = self
                .inner
                .router
                .principal(session)
                .map_err(|e| Failure::new("invalid_session", e))?;
            return browser
                .request(&principal, params)
                .await
                .map(Into::into)
                .map_err(|e| Failure::new("browser_failed", e));
        }
        let method = request.method();
        if matches!(request, Call::Provider(_))
            && !matches!(
                method,
                "thread/list"
                    | "account/read"
                    | "account/login/start"
                    | "account/login/cancel"
                    | "account/logout"
            )
        {
            return Err(Failure::new(
                "method_not_found",
                format!("unregistered method: {method}"),
            ));
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
        let claude_target = target_session
            .filter(|target| target.provider == agent_protocol::session::ProviderKind::Claude);
        if claude_target.is_none()
            && matches!(
                request,
                Call::StartTurn(_) | Call::SteerTurn(_) | Call::QueueTurn(_)
            )
            && let Err(error) = self.inner.codex.server()
        {
            return Err(error);
        }
        if matches!(request, Call::StartTurn(_))
            && claude_target.is_none()
            && let Some(error) = self.inner.restoration_error.borrow().as_ref()
        {
            return Err(Failure::new("account_unavailable", error));
        }
        if let Some(target) = target_session {
            let capabilities = provider_capabilities(target.provider);
            let supported = match request {
                Call::ForkThread(_) => capabilities.fork,
                Call::RenameThread(_) => capabilities.rename,
                Call::SteerTurn(_) | Call::QueueTurn(_) => capabilities.additional_input,
                _ => true,
            };
            if !supported {
                return Err(Failure::new(
                    "unsupported_operation",
                    format!("{method} is unsupported by this provider"),
                ));
            }
        }
        if let Some(target) = claude_target {
            return match self.inner.claude.get() {
                Some(claude) => {
                    claude
                        .request(&target.id, request)
                        .await
                        .map_err(|error| Failure::Host {
                            code: "claude_failed",
                            message: error.message,
                            delivery: error.delivery,
                        })
                }
                None => Err(Failure::new(
                    "claude_unavailable",
                    "このHostではClaude Codeが有効になっていません。",
                )),
            };
        }
        if let Call::StartTurn(params) = request
            && params
                .model
                .as_deref()
                .is_some_and(|model| model.starts_with(crate::claude::MODEL_PREFIX))
        {
            return Err(Failure::new(
                "provider_mismatch",
                "Claudeへ切り替える場合は新しい会話を作成してください。",
            ));
        }
        let response = match request {
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

            Call::ListModels(params) if self.inner.claude.get().is_some() => {
                let first_page = params.cursor.is_none();
                let (codex, claude) = tokio::join!(
                    self.inner
                        .codex
                        .request::<_, op::ModelPage>(method, &params),
                    async {
                        if first_page {
                            self.inner.claude.get().unwrap().models().await
                        } else {
                            Ok(Vec::new())
                        }
                    }
                );
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
                .answer_request(session, params.request_id.clone(), params.result.clone())
                .await?)
                .into(),
            Call::AddProject(params) => (self
                .add_project(&params.cwd)
                .await
                .map_err(|error| Failure::new("project_add_failed", error))?)
            .into(),
            Call::ListThreads(params) => {
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

            Call::StartThread(params) => (self.start_thread(params.clone()).await?).into(),
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
            Call::ListModels(_)
            | Call::ResumeThread(_)
            | Call::ForkThread(_)
            | Call::StartTurn(_)
            | Call::SteerTurn(_)
            | Call::QueueTurn(_)
            | Call::Interrupt(_)
            | Call::RenameThread(_)
            | Call::Provider(_) => {
                let browser_scope = matches!(request, Call::ForkThread(_))
                    .then(|| uuid::Uuid::new_v4().to_string());
                let response = self
                    .inner
                    .codex
                    .request_raw(&{
                        let mut params = request.params_json()?;
                        if let Some(target) = target_session {
                            params["threadId"] = target.id.clone().into();
                            if matches!(request, Call::ResumeThread(_))
                                && let Some(config) = self.browser_config(&target.id)?
                            {
                                params["config"] = config;
                            }
                        }
                        if let Some(scope) = &browser_scope
                            && let Some(config) = self.browser_config(scope)?
                        {
                            params["config"] = config;
                        }
                        agent_transport::peer::request_line(method, &params)
                            .expect("provider request serializes")
                    })
                    .await?;
                let mut body = codex_response(method, response)?;
                if let Body::Thread(response) = &mut body {
                    if let Some(scope) = browser_scope
                        && let Some(browser) = self.inner.browser.get()
                        && let Some(id) = &response.thread.id
                    {
                        browser.bind_scope(scope, id.clone()).await;
                    }
                    describe_thread(
                        &mut response.thread,
                        agent_protocol::session::ProviderKind::Codex,
                        &self.project_snapshot().await?,
                    );
                }
                body
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
            for worktree in &mut worktrees {
                if !cwd.starts_with(&worktree.path) {
                    continue;
                }
                let active = thread.status.as_ref().is_some_and(|status| {
                    status.kind == agent_protocol::models::ThreadStatusKind::Active
                });
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
        let root = tokio::fs::canonicalize(cwd).await?;
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
        let merged = crate::worktrees::merged_directories(
            page.data
                .iter()
                .filter_map(|thread| thread.cwd.clone())
                .collect(),
        )
        .await
        .map_err(|error| Failure::new("worktree_status_failed", error))?;
        for thread in &mut page.data {
            thread.worktree_merged =
                Some(thread.cwd.as_ref().is_some_and(|cwd| merged.contains(cwd)));
        }
        if !provider_errors.is_empty() {
            page.provider_errors = Some(provider_errors);
        }
        Ok(page)
    }

    async fn start_thread(
        &self,
        mut params: agent_protocol::operations::StartThread,
    ) -> Result<ThreadResponse, Failure> {
        let provider = if params
            .model
            .as_deref()
            .is_some_and(|model| model.starts_with(crate::claude::MODEL_PREFIX))
        {
            agent_protocol::session::ProviderKind::Claude
        } else {
            agent_protocol::session::ProviderKind::Codex
        };
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
        let mut response = if let Some(model) = params.model.as_deref()
            && provider == agent_protocol::session::ProviderKind::Claude
        {
            let claude = self.inner.claude.get().ok_or_else(|| {
                Failure::new(
                    "claude_unavailable",
                    "このHostではClaude Codeが有効になっていません。",
                )
            })?;
            claude
                .create(params.cwd.as_deref().unwrap_or_default(), model)
                .await
                .map_err(|error| Failure::new("claude_unavailable", error))?
        } else {
            let scope = uuid::Uuid::new_v4().to_string();
            let mut request =
                serde_json::json!({"cwd":params.cwd,"model":params.model,"projectId":project_id});
            if let Some(config) = self.browser_config(&scope)? {
                request["config"] = config;
            }
            let response: ThreadResponse =
                self.inner.codex.request("thread/start", &request).await?;
            if let Some(browser) = self.inner.browser.get()
                && let Some(id) = &response.thread.id
            {
                browser.bind_scope(scope, id.clone()).await;
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
                                .and_then(|request| super::codex::request(&router, request));
                            if let Err(error) = admission
                                && let Some(inner) = inner.upgrade()
                                && let Ok(codex) = &inner.codex.process
                                && let Ok(response) = request.error(-32000, &error)
                            {
                                let _ = codex.send_raw(&response).await;
                            }
                        } else if let Err(error) = super::codex::event(&router, &request) {
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
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            canonical_storage_path(parent).join(name)
        }
        _ => std::env::current_dir().unwrap_or_default().join(path),
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
    let session = thread.session.take().or_else(|| {
        thread
            .id
            .as_ref()
            .map(|id| agent_protocol::session::SessionRef {
                provider,
                id: id.clone(),
            })
    });
    if let Some(session) = session {
        thread.id = Some(session.thread_id());
        thread.session = Some(session);
    }
}

fn codex_response(method: &str, line: String) -> Result<Body, Failure> {
    match agent_protocol::protocol::json_boundary::response(method, &line)
        .map_err(|error| Failure::unknown("invalid_params", error))?
    {
        Response::Failure { error } => {
            // Provider fields are not Bex delivery evidence.
            Err(Failure::upstream(serde_json::value::to_raw_value(&error)?))
        }
        Response::Success { result } => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_error_fields_never_prove_non_delivery() {
        let native = serde_json::json!({"code":123,"message":"not sent","delivery":"notSent","details":{"kept":true}});
        let response = super::codex_response(
            "turn/start",
            serde_json::json!({"id":1,"error":native}).to_string(),
        )
        .unwrap_err();

        assert_eq!(
            response.delivery(),
            agent_transport::peer::Delivery::Unknown
        );
        let response = super::Response::from_result::<(), _>(Err(response))
            .unwrap()
            .into_value();
        assert_eq!(response["error"]["delivery"], "unknown");
        assert_eq!(response["error"]["providerError"], native);
        assert_eq!(response["error"]["message"], "not sent");
    }
    #[tokio::test]
    async fn unknown_methods_do_not_require_or_reach_codex() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            Err("must not be consulted".into()),
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session(16);
        for params in [
            serde_json::json!({}),
            serde_json::json!({"threadId":"claude:native"}),
        ] {
            let call = agent_protocol::protocol::json_boundary::call("not/public", params).unwrap();
            let response = service.dispatch(session.id(), &call).await.unwrap();
            let response = agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
            >(&response.initial)
            .unwrap()
            .into_value();
            assert_eq!(response["error"]["code"], "method_not_found");
            assert_eq!(response["error"]["delivery"], "notSent");
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
        let session = service.open_session(16);
        for (method, expected) in [
            ("thread/fork", "unsupported_operation"),
            ("thread/name/set", "unsupported_operation"),
            ("turn/steer", "claude_unavailable"),
            ("thread/queue/add", "claude_unavailable"),
        ] {
            let call = agent_protocol::protocol::json_boundary::call(method, serde_json::json!({"threadId":"claude:native","clientUserMessageId":method,"lastTurnId":"turn","excludeTurns":false,"name":"Renamed","input":[],"expectedTurnId":"turn"})).unwrap();
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
        let session = service.open_session(16);
        for method in ["turn/start", "turn/steer", "thread/queue/add"] {
            let call = agent_protocol::protocol::json_boundary::call(
                method,
                serde_json::json!({"threadId":"native","clientUserMessageId":"input","input":[],"expectedTurnId":"turn"}),
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

    #[test]
    fn malformed_provider_reply_does_not_prove_non_delivery() {
        let error =
            super::codex_response("turn/start", r#"{"id":1,"result":{}}"#.into()).unwrap_err();
        assert_eq!(error.delivery(), agent_transport::peer::Delivery::Unknown);
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
        let session = service.open_session(16);
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
