//! Snapshot getters for native views. Conversation views are built from
//! `sync` and `commands` results.
use super::{AgentError, error};
use crate::{
    models::{FileContent, FileList, Project, WorktreeSettings},
    state::{Draft, RefScope, Snapshot},
};
use agent_protocol::{
    models::AgentActivityPhase,
    operations::{AccountLogin, Accounts},
    vcs::{ActionProgressEvent, ActionProgressKind},
};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AwarenessActivityView {
    pub environment_id: String,
    pub thread_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub phase: String,
    pub headline: String,
    pub detail: Option<String>,
    pub model_title: Option<String>,
    pub updated_at_ms: i64,
}

fn awareness_phase_name(phase: &AgentActivityPhase) -> String {
    match phase {
        AgentActivityPhase::Starting => "starting",
        AgentActivityPhase::Running => "running",
        AgentActivityPhase::WaitingApproval => "waitingApproval",
        AgentActivityPhase::WaitingInput => "waitingInput",
        AgentActivityPhase::Completed => "completed",
        AgentActivityPhase::Failed => "failed",
        AgentActivityPhase::Stale => "stale",
    }
    .into()
}

#[uniffi::export]
impl Snapshot {
    #[uniffi::constructor]
    pub fn empty() -> Arc<Self> {
        Arc::default()
    }
    #[uniffi::constructor]
    pub fn restore(bytes: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        Ok(Arc::new(crate::persistence::decode(&bytes).map_err(error)?))
    }
    pub fn serialize_model_preferences(&self) -> Result<Vec<u8>, AgentError> {
        crate::persistence::encode_model_preferences(self).map_err(error)
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn host_name(&self) -> Option<String> {
        self.host_name.clone()
    }
    pub fn environment_id(&self) -> Option<String> {
        self.environment
            .as_ref()
            .map(|environment| environment.environment_id.clone())
    }
    pub fn environment_label(&self) -> Option<String> {
        self.environment
            .as_ref()
            .map(|environment| environment.label.clone())
    }
    pub fn environment_platform(&self) -> Option<String> {
        self.environment
            .as_ref()
            .map(|environment| format!("{}:{}", environment.platform.os, environment.platform.arch))
    }
    pub fn environment_machine(&self) -> Option<String> {
        self.environment
            .as_ref()
            .and_then(|environment| environment.platform.machine.clone())
    }
    pub fn environment_server_version(&self) -> Option<String> {
        self.environment
            .as_ref()
            .map(|environment| environment.server_version.clone())
    }
    pub fn environment_protocol_version(&self) -> Option<u32> {
        self.environment
            .as_ref()
            .and_then(|environment| environment.orchestration_protocol_version)
    }
    pub fn environment_connection_state(&self) -> Option<String> {
        crate::environment::summarize_snapshot(self).map(|summary| {
            match summary.connection {
                crate::environment::EnvironmentConnectionState::Connected => "connected",
                crate::environment::EnvironmentConnectionState::Connecting => "connecting",
                crate::environment::EnvironmentConnectionState::Disconnected => "disconnected",
            }
            .into()
        })
    }
    pub fn environment_reconnect_reason(&self) -> Option<String> {
        crate::environment::summarize_snapshot(self).and_then(|summary| summary.reconnect_reason)
    }
    pub fn environment_can_upload_attachments(&self) -> bool {
        self.environment.as_ref().is_some_and(|environment| {
            crate::environment::supports_capability(
                &environment.capabilities,
                crate::environment::EnvironmentCapability::AttachmentUploads,
            )
        })
    }
    pub fn environment_supports_inline_context(&self) -> bool {
        self.environment
            .as_ref()
            .is_some_and(|environment| environment.capabilities.inline_message_context)
    }
    pub fn environment_can_publish_activity(&self) -> bool {
        self.environment
            .as_ref()
            .is_some_and(|environment| environment.capabilities.agent_activity_publishing)
    }
    pub fn environment_capabilities(&self) -> Vec<String> {
        self.environment
            .as_ref()
            .map_or_else(Vec::new, |environment| {
                crate::environment::capability_names(&environment.capabilities)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
    }
    pub fn scoped_thread_id(&self, thread_id: String) -> Option<String> {
        self.environment.as_ref().and_then(|environment| {
            crate::environment::scoped_key(&environment.environment_id, &thread_id)
        })
    }
    pub fn scoped_project_id(&self, project_id: String) -> Option<String> {
        self.environment.as_ref().and_then(|environment| {
            crate::environment::scoped_key(&environment.environment_id, &project_id)
        })
    }
    pub fn awareness_activity_count(&self) -> u64 {
        self.awareness
            .as_ref()
            .map_or(0, |awareness| awareness.activities.len() as u64)
    }
    pub fn awareness_updated_at_ms(&self) -> i64 {
        self.awareness
            .as_ref()
            .map_or(0, |awareness| awareness.updated_at_ms)
    }
    pub fn awareness_activities(&self) -> Vec<AwarenessActivityView> {
        self.awareness.as_ref().map_or_else(Vec::new, |awareness| {
            awareness
                .activities
                .iter()
                .map(|activity| AwarenessActivityView {
                    environment_id: activity.environment_id.clone(),
                    thread_id: activity.thread_id.clone(),
                    project_title: activity.project_title.clone(),
                    thread_title: activity.thread_title.clone(),
                    phase: awareness_phase_name(&activity.phase),
                    headline: activity.headline.clone(),
                    detail: activity.detail.clone(),
                    model_title: activity.model_title.clone(),
                    updated_at_ms: activity.updated_at_ms,
                })
                .collect()
        })
    }
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
    pub fn supersedes(&self, previous: Arc<Self>) -> bool {
        self.accepts_after(&previous)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn selected_thread_id(&self) -> Option<String> {
        self.selected_thread.as_ref().map(ToString::to_string)
    }
    pub fn selected_project_id(&self) -> Option<String> {
        self.selected_project.clone()
    }
    pub fn selected_pull_request_label(&self) -> Option<String> {
        self.selected_thread
            .as_ref()
            .and_then(|thread| self.thread_row(thread))
            .and_then(|row| row.pull_request_label.clone())
    }
    pub fn selected_pull_request_url(&self) -> Option<String> {
        self.selected_thread
            .as_ref()
            .and_then(|thread| self.thread_row(thread))
            .and_then(|row| row.pull_request_url.clone())
    }
    pub fn current_directory(&self) -> String {
        self.cwd()
    }
    pub fn draft(&self) -> Draft {
        self.current_draft()
    }
    pub fn search_query(&self) -> String {
        self.search.clone()
    }
    pub fn can_open_terminal(&self) -> bool {
        self.terminal_available()
    }
    pub fn review_revision(&self) -> Option<String> {
        self.workspace
            .review
            .as_ref()
            .map(|_| format!("{}:{}", self.cwd(), self.workspace.review_generation))
    }
    pub fn current_draft_key(&self) -> String {
        self.draft_key()
    }
    pub fn projects(&self) -> Vec<Project> {
        self.shell_projects().to_vec()
    }
    pub fn directory(&self) -> Option<FileList> {
        self.workspace.directory.clone()
    }
    pub fn file(&self) -> Option<FileContent> {
        self.workspace
            .file
            .as_ref()
            .map(|file| file.as_ref().clone())
    }
    pub fn file_draft(&self, path: String) -> Option<String> {
        self.workspace
            .file_drafts
            .get(&path)
            .map(|draft| draft.text.clone())
    }
    pub fn review(&self) -> Option<Arc<WorkspaceReview>> {
        self.workspace
            .review
            .as_ref()
            .map(|r| Arc::new(WorkspaceReview(r.clone())))
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.workspace.worktree_settings.clone()
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.accounts.clone()
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.account_login.clone()
    }
    pub fn git_status(&self, cwd: String) -> Option<GitStatus> {
        self.git.status.get(&cwd).map(GitStatus::from)
    }
    pub fn git_refs(&self, cwd: String) -> Vec<GitRef> {
        self.sources
            .refs(&cwd, RefScope::All)
            .and_then(|entry| entry.list.as_ref())
            .map(|list| list.refs.iter().map(GitRef::from).collect())
            .unwrap_or_default()
    }
    pub fn git_action(&self, action_id: String) -> Option<GitActionProgress> {
        self.git
            .actions
            .get(&action_id)
            .map(GitActionProgress::from)
    }
    pub fn git_menu(&self, cwd: String) -> Vec<crate::view::git::GitActionMenuItem> {
        let status = self.git.status.get(&cwd);
        let busy = self
            .git
            .actions
            .values()
            .any(|event| event.cwd == cwd && !action_finished(&event.kind));
        crate::view::git::build_menu_items(
            status,
            busy,
            status.is_some_and(|status| status.has_primary_remote),
        )
    }
    pub fn git_quick_action(&self, cwd: String) -> Option<crate::view::git::GitQuickAction> {
        let status = self.git.status.get(&cwd)?;
        let busy = self
            .git
            .actions
            .values()
            .any(|event| event.cwd == cwd && !action_finished(&event.kind));
        Some(crate::view::git::resolve_quick_action(
            Some(status),
            busy,
            status.is_default_ref,
            status.has_primary_remote,
        ))
    }
    pub fn git_requires_default_branch_confirmation(&self, cwd: String, action: String) -> bool {
        let Some(action) = parse_git_action(&action) else {
            return false;
        };
        let Some(status) = self.git.status.get(&cwd) else {
            return false;
        };
        crate::view::git::requires_default_branch_confirmation(action, status.is_default_ref)
    }
    pub fn git_default_branch_action_copy(
        &self,
        cwd: String,
        action: String,
        includes_commit: bool,
    ) -> Option<crate::view::git::DefaultBranchActionCopy> {
        let action = parse_git_action(&action)?;
        let status = self.git.status.get(&cwd)?;
        let branch = status.ref_name.as_deref()?.to_owned();
        let terminology = status
            .source_control_provider
            .as_ref()
            .map(|provider| crate::view::git::change_request_terminology(Some(provider.kind)));
        Some(crate::view::git::default_branch_action_copy(
            action,
            &branch,
            includes_commit,
            terminology,
        ))
    }
}

fn parse_git_action(action: &str) -> Option<crate::view::git::GitAction> {
    Some(match action {
        "commit" => crate::view::git::GitAction::Commit,
        "push" => crate::view::git::GitAction::Push,
        "create_pr" => crate::view::git::GitAction::CreatePr,
        "commit_push" => crate::view::git::GitAction::CommitPush,
        "commit_push_pr" => crate::view::git::GitAction::CommitPushPr,
        "open_pr" => crate::view::git::GitAction::OpenPr,
        _ => return None,
    })
}

fn action_finished(kind: &ActionProgressKind) -> bool {
    matches!(
        kind,
        ActionProgressKind::ActionFinished { .. } | ActionProgressKind::ActionFailed { .. }
    )
}

#[derive(Clone, uniffi::Record)]
pub struct GitStatus {
    pub is_repo: bool,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: Option<u64>,
    pub working_tree: Vec<GitFileChange>,
    pub pull_request_url: Option<String>,
}

#[derive(Clone, uniffi::Record)]
pub struct GitFileChange {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, uniffi::Record)]
pub struct GitRef {
    pub name: String,
    pub is_remote: bool,
    pub remote_name: Option<String>,
    pub current: bool,
    pub is_default: bool,
    pub worktree_path: Option<String>,
}
impl From<&agent_protocol::workspace::VcsRef> for GitRef {
    fn from(reference: &agent_protocol::workspace::VcsRef) -> Self {
        Self {
            name: reference.name.clone(),
            is_remote: reference.is_remote,
            remote_name: reference.remote_name.clone(),
            current: reference.current,
            is_default: reference.is_default,
            worktree_path: reference.worktree_path.clone(),
        }
    }
}

impl From<&agent_protocol::workspace::VcsStatus> for GitStatus {
    fn from(status: &agent_protocol::workspace::VcsStatus) -> Self {
        Self {
            is_repo: status.is_repo,
            has_primary_remote: status.has_primary_remote,
            is_default_ref: status.is_default_ref,
            ref_name: status.ref_name.clone(),
            has_working_tree_changes: status.has_working_tree_changes,
            has_upstream: status.has_upstream,
            ahead_count: status.ahead_count,
            behind_count: status.behind_count,
            ahead_of_default_count: status.ahead_of_default_count,
            working_tree: status
                .working_tree
                .files
                .iter()
                .map(|file| GitFileChange {
                    path: file.path.clone(),
                    insertions: file.insertions,
                    deletions: file.deletions,
                })
                .collect(),
            pull_request_url: status.pr.as_ref().map(|pr| pr.url.clone()),
        }
    }
}

#[derive(Clone, uniffi::Record)]
pub struct GitActionProgress {
    pub action_id: String,
    pub action: String,
    pub phase: Option<String>,
    pub label: Option<String>,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
}
impl From<&ActionProgressEvent> for GitActionProgress {
    fn from(event: &ActionProgressEvent) -> Self {
        let (phase, label, status, output, error) = match &event.kind {
            ActionProgressKind::ActionStarted { phases } => (
                phases.first().map(|phase| format!("{phase:?}")),
                None,
                "started".into(),
                None,
                None,
            ),
            ActionProgressKind::PhaseStarted { phase, label } => (
                Some(format!("{phase:?}")),
                Some(label.clone()),
                "phase_started".into(),
                None,
                None,
            ),
            ActionProgressKind::HookStarted { hook_name } => (
                None,
                Some(hook_name.clone()),
                "hook_started".into(),
                None,
                None,
            ),
            ActionProgressKind::HookOutput { stream, text, .. } => (
                None,
                None,
                format!("output_{stream:?}"),
                Some(text.clone()),
                None,
            ),
            ActionProgressKind::HookFinished { hook_name, .. } => (
                None,
                Some(hook_name.clone()),
                "hook_finished".into(),
                None,
                None,
            ),
            ActionProgressKind::ActionFinished { .. } => {
                (None, None, "finished".into(), None, None)
            }
            ActionProgressKind::ActionFailed { phase, message } => (
                phase.map(|phase| format!("{phase:?}")),
                None,
                "failed".into(),
                None,
                Some(message.clone()),
            ),
        };
        Self {
            action_id: event.action_id.clone(),
            action: event.action.as_str().into(),
            phase,
            label,
            status,
            output,
            error,
        }
    }
}
#[derive(uniffi::Object)]
pub struct WorkspaceReview(pub(crate) Arc<crate::models::WorkspaceReview>);
#[uniffi::export]
impl WorkspaceReview {
    pub fn diff_files(&self) -> Vec<crate::presentation::diff::WorkspaceDiffFile> {
        crate::presentation::diff::diff_files(&self.0)
    }
    pub fn file_count(&self) -> u64 {
        self.0.files.len() as u64
    }
    pub fn branch(&self) -> String {
        self.0.branch.clone()
    }
    pub fn additions(&self) -> u64 {
        self.0.additions
    }
    pub fn deletions(&self) -> u64 {
        self.0.deletions
    }
}
