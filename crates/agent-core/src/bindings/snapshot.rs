//! Snapshot getters for native views. Conversation views are built from
//! `sync` and `commands` results.
use super::{AgentError, error};
use crate::{
    models::{FileContent, FileList, Project, WorktreeSettings},
    state::{Draft, Snapshot},
};
use agent_protocol::{
    models::AgentActivityPhase,
    operations::{AccountLogin, Accounts},
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
