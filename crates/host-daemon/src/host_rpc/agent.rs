//! Bex's boundary for the two native agent implementations.
use super::{requests::RequestOrigin, routing::SessionRouter, service::Failure};
use agent_protocol::{
    composer::ComposerCatalog,
    ids::TurnId,
    models::{Empty, Item, Thread, ThreadResponse},
    operations as op,
    permissions::{PermissionMode, PermissionSettings},
    session::{Capabilities, SessionRef},
};
use futures_util::{Stream, future::BoxFuture};
use serde_json::Value;
use std::{path::Path, sync::Arc};

pub(crate) struct SessionSummary {
    pub thread: Thread,
    pub branch: Option<String>,
}

pub(crate) struct SessionPage {
    pub data: Vec<SessionSummary>,
    pub next_cursor: Option<String>,
}

/// Submission evidence from one read, including whether starting needs the
/// adapter to reload its session. The Host may also require a reload after
/// recreating the workspace.
pub(crate) struct SubmissionState {
    pub response: ThreadResponse,
    pub needs_reload: bool,
}

/// Consume native pages with one cursor policy, preserving earlier pages if a
/// later read fails. Callers decide whether partial results are useful.
pub(crate) fn session_pages<'a>(
    agent: &'a dyn Agent,
    search: &'a str,
    ancestor: Option<&'a str>,
) -> impl Stream<Item = Result<Vec<SessionSummary>, Failure>> + 'a {
    futures_util::stream::try_unfold(
        Some((None, std::collections::HashSet::new())),
        move |state| async move {
            let Some((cursor, mut seen)) = state else {
                return Ok(None);
            };
            let page = Agent::list(agent, search, cursor, ancestor).await?;
            let next = page.next_cursor.filter(|cursor| !cursor.is_empty());
            if let Some(cursor) = &next
                && !seen.insert(cursor.clone())
            {
                return Err(Failure::new(
                    "invalid_session_list",
                    "session list cursor repeated",
                ));
            }
            Ok(Some((page.data, next.map(|cursor| (Some(cursor), seen)))))
        },
    )
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
}

#[async_trait::async_trait]
pub(crate) trait Agent: Identity {
    fn data_recipient(&self) -> &'static str;
    fn running_input(&self) -> super::submission::RunningInput;
    fn capabilities(&self) -> Capabilities;
    fn availability(&self) -> Result<(), Failure>;
    /// Reject creation before the Host creates a workspace. Some adapters can
    /// create a conversation locally and defer starting an executable.
    fn validate_create(&self) -> Result<(), Failure>;
    fn storage_directory(&self) -> &Path;
    async fn list(
        &self,
        search: &str,
        cursor: Option<String>,
        ancestor: Option<&str>,
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
    async fn submit(
        &self,
        input: &op::Submission,
        route: super::submission::SubmissionTarget<'_>,
        reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure>;
    async fn interrupt(&self, id: &str, turn: &TurnId) -> Result<Empty, Failure>;
    async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure>;
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
    Renamed(SessionRef),
    Stopped {
        provider: agent_protocol::session::ProviderKind,
        reason: String,
    },
}
pub(crate) struct AgentEvent {
    pub change: AgentChange,
    pub applied: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}
impl AgentChange {
    pub fn apply(self, router: &SessionRouter) -> Result<(), String> {
        match self {
            Self::Session { session, change } => router.session_change(&session, change),
            Self::Request {
                session,
                origin,
                request,
            } => return router.request(session, origin, request),
            Self::Resolved {
                instance,
                native_id,
            } => router.resolve_native_request(instance, &native_id),
            Self::SourceClosed(instance) => router.close_request_source(instance),
            Self::Renamed(session) => {
                router.broadcast(agent_protocol::protocol::Notification::SessionRenamed { session })
            }
            Self::Stopped { provider, reason } => router.fail_provider(provider, &reason),
        }
        Ok(())
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
