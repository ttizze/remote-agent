//! Bex's native provider boundary.
use super::{requests::RequestOrigin, routing::SessionRouter, service::Failure};
use agent_protocol::{
    composer::ComposerCatalog,
    ids::TurnId,
    models::{Empty, Item, Thread, ThreadResponse},
    operations as op,
    permissions::{PermissionMode, PermissionSettings},
    session::{Capabilities, SessionRef},
};
use futures_util::{Stream, StreamExt, future::BoxFuture};
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

#[derive(Clone, Copy)]
pub(crate) enum SessionListScope<'a> {
    Roots,
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

pub(super) async fn read_session_page(
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
    fn running_input(&self) -> super::submission::RunningInput;
    /// Cloud conversations own execution without a checkout on this Host.
    fn manages_workspace(&self, _id: &str) -> bool {
        true
    }
    fn capabilities(&self) -> Capabilities;
    fn availability(&self) -> Result<(), Failure>;
    /// Reject creation before the Host creates a workspace. Some adapters can
    /// create a conversation locally and defer starting an executable.
    fn validate_create(&self) -> Result<(), Failure>;
    fn storage_directory(&self) -> &Path;
    /// Provider-owned cloud projects do not scan native history folders.
    async fn cloud_projects(&self) -> Result<Vec<agent_protocol::models::Project>, Failure> {
        Ok(Vec::new())
    }
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
    async fn submit(
        &self,
        input: &op::Submission,
        route: super::submission::SubmissionTarget<'_>,
        reload: bool,
        browser: Option<Value>,
    ) -> Result<op::SubmissionReceipt, Failure>;
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
    /// External conversations are polled only while a client observes them.
    Observed {
        session: SessionRef,
        changes: Vec<agent_protocol::session::SessionChange>,
    },
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
impl AgentChange {
    pub fn apply(self, router: &SessionRouter) -> Result<(), String> {
        match self {
            Self::Observed { session, changes } => {
                if !router.is_observed(&session) {
                    return Err("conversation subscription closed".into());
                }
                for change in changes {
                    router.session_change(&session, change);
                }
            }
            Self::TaskParent { session, parent } => router.task_parent(session, parent),
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
            Self::Renamed { session, name } => {
                router.broadcast(agent_protocol::protocol::Notification::SessionRenamed {
                    session,
                    name,
                    revision: 0,
                })
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

    async fn two_session_history() -> (tempfile::TempDir, crate::adapters::Claude, Vec<String>) {
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
        let agent = crate::adapters::Claude::load(
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
