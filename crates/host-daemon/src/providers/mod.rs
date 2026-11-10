//! Native provider boundary. Providers translate their protocols into these
//! values; the Host owns routing, conversations and client-facing state.
pub(crate) mod claude;
pub mod codex;
pub mod deepseek;
mod refreshed;
mod registry;
pub(crate) mod requests;
mod settings;
pub use claude::Claude;
pub(crate) use registry::Registry;
pub(crate) use settings::Settings;

use crate::dictation::Dictation;
use agent_protocol::session::ProviderKind;
use agent_protocol::{
    composer::ComposerCatalog,
    ids::TurnId,
    models::{Empty, Item, Thread, ThreadResponse},
    operations as op,
    permissions::{PermissionMode, PermissionSettings},
    requests::{Answer, RequestBody},
    session::{Capabilities, SessionRef},
};
use futures_util::{Stream, StreamExt, future::BoxFuture};
use serde_json::Value;
use std::{path::Path, sync::Arc};

#[derive(Debug, Clone, serde::Serialize, thiserror::Error)]
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
impl From<agent_transport::peer::RpcMessageError> for Failure {
    fn from(error: agent_transport::peer::RpcMessageError) -> Self {
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

pub struct Backend {
    pub(crate) provider: ProviderKind,
    pub(crate) agent: Result<Arc<dyn Agent>, Failure>,
    pub(crate) dictation: Option<Dictation>,
}

impl Backend {
    pub fn unavailable(provider: ProviderKind, error: impl std::fmt::Display) -> Self {
        Self {
            provider,
            agent: Err(Failure::new("provider_unavailable", error)),
            dictation: None,
        }
    }
}

impl From<Claude> for Backend {
    fn from(agent: Claude) -> Self {
        Self {
            provider: ProviderKind::Claude,
            agent: Ok(Arc::new(agent)),
            dictation: None,
        }
    }
}

/// The Host keeps normalized requests and source identity. The adapter owns
/// answer mappings and the resource to which an answer can be written.
#[async_trait::async_trait]
pub(crate) trait AnswerSource: Send + Sync {
    fn is_alive(&self) -> bool;
    async fn prepare(
        &self,
        native_id: &Value,
        body: &RequestBody,
        answer: &Answer,
    ) -> Result<AnswerWrite, Failure>;
}
#[derive(Clone)]
pub(crate) struct RequestOrigin {
    pub instance: uuid::Uuid,
    pub native_id: Value,
    pub provider: ProviderKind,
    pub source: Arc<dyn AnswerSource>,
}

/// What a provider does with input: steer its running turn, or start a turn.
/// The Host chooses after reading native state and overlaying its live
/// execution, and keeps input queued behind a running turn itself.
#[derive(Debug, PartialEq)]
pub(crate) enum SubmissionTarget<'a> {
    Steer(&'a str),
    Start { cwd: &'a str },
}

pub(crate) struct SessionSummary {
    pub thread: Thread,
    pub branch: Option<String>,
}

pub(crate) struct SessionPage {
    pub data: Vec<SessionSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum SessionListScope<'a> {
    /// Tasks a person opened, including these Host-owned native sessions.
    /// Unowned SDK and noninteractive CLI runs belong to other agents or scripts.
    Roots(&'a [SessionRef]),
    /// Tasks and their subagents, so a subagent match can surface its task.
    WithSubagents(&'a [SessionRef]),
    /// Every native session, including agent-started runs whose activity
    /// still occupies a workspace.
    All,
    Loaded,
    Descendants(&'a str),
}

/// Submission evidence from one read, including whether starting needs the
/// adapter to reload its session. The Host may also require a reload after
/// recreating the workspace.
pub(crate) struct SubmissionState {
    pub response: ThreadResponse,
    pub needs_reload: bool,
}

/// Preserve healthy pages on a later failure and reject repeated cursors.
/// Filtered lists can widen follow-up reads without enlarging the first page.
pub(crate) fn session_pages<'a>(
    agent: &'a dyn Agent,
    search: &'a str,
    scope: SessionListScope<'a>,
    first_limit: u32,
    later_limit: u32,
    deadline: Option<tokio::time::Instant>,
    first_page: Option<Result<SessionPage, Failure>>,
) -> impl Stream<Item = Result<Vec<SessionSummary>, Failure>> + 'a {
    futures_util::stream::try_unfold(
        Some((None, std::collections::HashSet::new(), first_page)),
        move |state| async move {
            let Some((cursor, mut seen, first_page)) = state else {
                return Ok(None);
            };
            let page = if let Some(page) = first_page {
                page?
            } else {
                let limit = if cursor.is_none() {
                    first_limit
                } else {
                    later_limit
                };
                let read = Agent::list(agent, search, cursor, scope, limit);
                if let Some(deadline) = deadline {
                    read_session_page(read, deadline).await?
                } else {
                    read.await?
                }
            };
            let next = page.next_cursor.filter(|cursor| !cursor.is_empty());
            if let Some(cursor) = &next
                && !seen.insert(cursor.clone())
            {
                return Err(Failure::new(
                    "invalid_session_list",
                    "session list cursor repeated",
                ));
            }
            Ok(Some((
                page.data,
                next.map(|cursor| (Some(cursor), seen, None)),
            )))
        },
    )
    .fuse()
}

pub(crate) async fn read_session_page(
    read: impl Future<Output = Result<SessionPage, Failure>>,
    deadline: tokio::time::Instant,
) -> Result<SessionPage, Failure> {
    // Enforce the deadline only when reading a new native page. Buffered
    // titles and an exhausted provider remain readable after the deadline.
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => Err(Failure::new(
            "provider_timeout",
            "session listing timed out; results are partial",
        )),
        result = read => result,
    }
}

/// A prepared write. Preparing cannot send an answer; polling this future can.
pub(crate) type AnswerWrite = BoxFuture<'static, Result<(), Failure>>;

/// Only account workflow values cross the provider boundary.
pub(crate) enum AccountCommand {
    Select { id: String },
    Logout { id: String },
    StartLogin,
    SubmitLogin { id: String, code: String },
    ReadLogin { id: String },
    CancelLogin { id: String },
}
pub(crate) enum AccountReply {
    Selection(op::AccountSelection),
    Login(op::AccountLogin),
    Status(op::AccountLoginStatus),
    Complete,
}
impl From<op::AccountSelection> for AccountReply {
    fn from(value: op::AccountSelection) -> Self {
        Self::Selection(value)
    }
}
impl From<op::AccountLogin> for AccountReply {
    fn from(value: op::AccountLogin) -> Self {
        Self::Login(value)
    }
}
impl From<op::AccountLoginStatus> for AccountReply {
    fn from(value: op::AccountLoginStatus) -> Self {
        Self::Status(value)
    }
}
impl From<Empty> for AccountReply {
    fn from(_: Empty) -> Self {
        Self::Complete
    }
}

#[async_trait::async_trait]
pub(crate) trait Identity: Send + Sync {
    async fn list(&self) -> Result<op::Accounts, Failure>;
    async fn account(&self, command: AccountCommand) -> Result<AccountReply, Failure>;
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, Failure>;
    async fn account_terminal(&self, _id: &str) -> Result<crate::terminals::Launch, Failure> {
        Err(Failure::new(
            "unsupported",
            "This provider has no interactive account setup",
        ))
    }
    async fn dsh_credentials(&self) -> Result<agent_protocol::dsh::Credentials, Failure> {
        Err(Failure::new(
            "unsupported",
            "This provider does not manage DSH credentials",
        ))
    }
    async fn update_dsh_credential(
        &self,
        _update: agent_protocol::dsh::KeyUpdate,
    ) -> Result<agent_protocol::dsh::Credentials, Failure> {
        Err(Failure::new(
            "unsupported",
            "This provider does not manage DSH credentials",
        ))
    }
}

#[async_trait::async_trait]
pub(crate) trait Agent: Identity {
    async fn codex_cli(&self, _: bool) -> Result<op::CodexCliStatus, Failure> {
        Err(Failure::new(
            "unsupported",
            "Codex CLI management is unavailable",
        ))
    }
    fn data_recipient(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// The items an opened turn shows before its details are read. Providers
    /// whose opened histories include activity keep every item.
    fn summary_items(&self, _items: &[Arc<Item>]) -> Option<Vec<Arc<Item>>> {
        None
    }
    fn availability(&self) -> Result<(), Failure>;
    /// An adapter that never started its process cannot own interrupted work.
    /// Read failures after startup still require a complete restart snapshot.
    fn requires_restart_snapshot(&self) -> bool {
        true
    }
    /// Reject creation before the Host creates a workspace. Some adapters can
    /// create a conversation locally and defer starting an executable.
    fn validate_create(&self) -> Result<(), Failure>;
    fn storage_directory(&self) -> &Path;
    /// External native histories distinguish client draft namespaces. Provider
    /// checkpoints owned by this Host already share its identity namespace.
    fn history_scope_directory(&self) -> Option<&Path> {
        Some(self.storage_directory())
    }
    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        scope: SessionListScope<'_>,
        limit: u32,
    ) -> Result<SessionPage, Failure>;
    async fn open(
        &self,
        id: &str,
        limit: usize,
        include_activity: bool,
    ) -> Result<ThreadResponse, Failure>;
    async fn read_history(
        &self,
        id: &str,
        cursor: &str,
        include_activity: bool,
    ) -> Result<agent_protocol::session::HistoryPage, Failure>;
    async fn read_item(&self, params: &op::ReadItem) -> Result<op::ItemResponse, Failure>;
    async fn read_turn_items(&self, id: &str, turn_id: &TurnId) -> Result<Vec<Arc<Item>>, Failure>;
    async fn create(
        &self,
        cwd: &str,
        model: Option<&str>,
        browser: Option<Value>,
    ) -> Result<ThreadResponse, Failure>;
    async fn state(&self, id: &str) -> Result<SubmissionState, Failure>;
    /// False is an explicit unsupported operation, before any history mutation.
    /// Other failures may have delivered history and must not be blindly retried.
    async fn inject_history(
        &self,
        _id: &str,
        _context: &agent_protocol::handoff::Context,
    ) -> Result<bool, Failure> {
        Ok(false)
    }
    /// Release an idle runtime while preserving its native conversation.
    async fn unload(&self, _id: &str) -> Result<(), Failure> {
        Ok(())
    }
    async fn submit(
        &self,
        input: &op::Submission,
        route: SubmissionTarget<'_>,
        reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure>;
    /// Continue persisted native history without replaying a user message.
    async fn continue_turn(
        &self,
        _id: &str,
        _model: Option<&str>,
        _effort: Option<&str>,
        _service_tier: Option<&str>,
    ) -> Result<op::SubmissionReceipt, Failure> {
        Err(Failure::new(
            "unsupported_operation",
            "Native continuation is unavailable",
        ))
    }
    async fn interrupt(&self, id: &str, turn: &TurnId) -> Result<Empty, Failure>;
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure>;
    async fn model_settings(
        &self,
        _id: &str,
    ) -> Result<agent_protocol::models::ModelSettings, Failure> {
        Err(Failure::new(
            "unsupported",
            "Model settings cannot be discovered",
        ))
    }
    async fn catalog(&self, cwd: &str) -> ComposerCatalog;
    /// Explain uncertainty when native history cannot prove inactivity. Providers
    /// with authoritative listed status need no additional check.
    async fn workspace_idle_warning(&self, _id: &str) -> Option<&'static str> {
        None
    }
    async fn discard_workspace_processes(&self, dir: &Path) -> Result<(), Failure>;
    async fn read_permissions(&self) -> Result<PermissionSettings, Failure>;
    async fn update_permissions(
        &self,
        mode: PermissionMode,
        version: &str,
    ) -> Result<PermissionSettings, Failure>;
    async fn fork(
        &self,
        id: &str,
        turn: &str,
        browser: Option<Value>,
    ) -> Result<ThreadResponse, Failure>;
    async fn rename(&self, id: &str, name: &str) -> Result<Empty, Failure>;
    fn event_stream(&self) -> Option<tokio::sync::mpsc::Receiver<AgentEvent>>;
    async fn shutdown(&self);
}

pub(crate) enum AgentChange {
    TaskParent {
        session: SessionRef,
        parent: SessionRef,
    },
    Session {
        session: SessionRef,
        change: agent_protocol::session::SessionChange,
    },
    Request {
        session: SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
    },
    Resolved {
        instance: uuid::Uuid,
        native_id: Value,
    },
    SourceClosed(uuid::Uuid),
    Renamed {
        session: SessionRef,
        name: Option<String>,
    },
    Stopped {
        provider: agent_protocol::session::ProviderKind,
        reason: String,
    },
}
pub(crate) struct AgentEvent {
    pub change: AgentChange,
    pub applied: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}
#[cfg(test)]
pub(crate) fn unavailable_origin(
    instance: uuid::Uuid,
    native_id: Value,
    stopped: tokio_util::sync::CancellationToken,
) -> RequestOrigin {
    struct UnavailableSource(tokio_util::sync::CancellationToken);
    #[async_trait::async_trait]
    impl AnswerSource for UnavailableSource {
        fn is_alive(&self) -> bool {
            !self.0.is_cancelled()
        }
        async fn prepare(
            &self,
            _: &Value,
            _: &RequestBody,
            _: &Answer,
        ) -> Result<AnswerWrite, Failure> {
            Err(Failure::new(
                "answer_not_sent",
                "request source has changed",
            ))
        }
    }
    RequestOrigin {
        instance,
        native_id,
        provider: agent_protocol::session::ProviderKind::Codex,
        source: std::sync::Arc::new(UnavailableSource(stopped)),
    }
}
pub(crate) async fn emit(
    events: &tokio::sync::mpsc::Sender<AgentEvent>,
    change: AgentChange,
) -> Result<(), String> {
    let (applied, receipt) = tokio::sync::oneshot::channel();
    events
        .send(AgentEvent {
            change,
            applied: Some(applied),
        })
        .await
        .map_err(|_| "Host event stream closed".to_owned())?;
    receipt
        .await
        .map_err(|_| "Host event processing stopped".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::TryStreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn empty_page() -> SessionPage {
        SessionPage {
            data: Vec::new(),
            next_cursor: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_page_deadlines_do_not_poll_the_provider() {
        let now = tokio::time::Instant::now();
        for deadline in [
            now,
            now - Duration::from_secs(1),
            now + Duration::from_secs(1),
        ] {
            let polls = AtomicUsize::new(0);
            let read = async {
                if deadline > now {
                    tokio::time::sleep_until(deadline).await;
                }
                polls.fetch_add(1, Ordering::SeqCst);
                Ok(empty_page())
            };
            assert_eq!(
                read_session_page(read, deadline).await.err().unwrap().code,
                "provider_timeout"
            );
            assert_eq!(polls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn native_page_reads_share_one_deadline() {
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_secs(5);
        let read = |seconds| async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            Ok(empty_page())
        };
        assert!(read_session_page(read(4), deadline).await.is_ok());
        assert_eq!(
            read_session_page(read(2), deadline)
                .await
                .err()
                .unwrap()
                .code,
            "provider_timeout"
        );
        assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn buffered_titles_and_exhaustion_do_not_spend_the_native_read_budget() {
        let (_root, agent, _) = two_session_history().await;
        for first_limit in [2, 1] {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut titles = session_pages(
                &agent,
                "",
                SessionListScope::All,
                first_limit,
                2,
                Some(deadline),
                None,
            )
            .map_ok(|page| futures_util::stream::iter(page.into_iter().map(Ok::<_, Failure>)))
            .try_flatten()
            .boxed();
            assert!(titles.try_next().await.unwrap().is_some());
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(6)).await;
            if first_limit == 2 {
                assert!(
                    titles.try_next().await.unwrap().is_some(),
                    "keep the already received second title"
                );
                for _ in 0..2 {
                    assert!(
                        titles.try_next().await.unwrap().is_none(),
                        "an exhausted provider must remain complete"
                    );
                }
            } else {
                assert_eq!(
                    titles.try_next().await.err().unwrap().code,
                    "provider_timeout",
                    "another native page still requires budget"
                );
            }
            tokio::time::resume();
        }
    }

    async fn two_session_history() -> (tempfile::TempDir, Claude, Vec<String>) {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native");
        let transcripts = native.join("projects/shared");
        std::fs::create_dir_all(&transcripts).unwrap();
        let mut expected = Vec::new();
        for _ in 0..2 {
            let id = uuid::Uuid::new_v4();
            expected.push(id.to_string());
            let user = serde_json::json!({"type":"user","sessionId":id,"cwd":root.path(),"message":{"content":"hello"}});
            std::fs::write(transcripts.join(format!("{id}.jsonl")), format!("{user}\n")).unwrap();
        }
        expected.sort();
        let agent = Claude::load(
            root.path().join("missing-cli"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        (root, agent, expected)
    }

    #[tokio::test]
    async fn prefetched_pages_survive_project_waits_and_continue_from_their_cursor() {
        let (_root, agent, expected) = two_session_history().await;
        for first_limit in [1, 2] {
            let first_page =
                Agent::list(&agent, "", None, SessionListScope::All, first_limit).await;
            let first_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(6)).await;
            // A complete prefetched page needs no budget. A continuation gets
            // the remaining native-read budget after project discovery ends.
            let deadline = if first_limit == 2 {
                first_deadline
            } else {
                tokio::time::Instant::now() + Duration::from_secs(1)
            };
            let pages = session_pages(
                &agent,
                "",
                SessionListScope::All,
                first_limit,
                2,
                Some(deadline),
                Some(first_page),
            );
            // Native filesystem work uses real blocking threads.
            tokio::time::resume();
            let pages: Vec<_> = pages.try_collect().await.unwrap();
            let mut actual: Vec<_> = pages
                .into_iter()
                .flatten()
                .map(|summary| summary.thread.id.unwrap().id)
                .collect();
            actual.sort();
            assert_eq!(actual, expected);
        }
        let mut failed = session_pages(
            &agent,
            "",
            SessionListScope::All,
            2,
            2,
            None,
            Some(Err(Failure::new(
                "provider_timeout",
                "first read timed out",
            ))),
        )
        .boxed();
        assert_eq!(
            failed.try_next().await.err().unwrap().code,
            "provider_timeout"
        );
        assert!(failed.try_next().await.unwrap().is_none());
    }
}
