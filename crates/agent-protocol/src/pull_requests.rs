//! Pull request and source-control RPC records.
//!
//! These records carry provider data and user intents across the native
//! boundary.  The provider remains at the Host boundary; clients consume the
//! shared domain values.
use agent_domain::{
    PullRequestAction, PullRequestDetail, PullRequestKey, PullRequestLink,
    PullRequestLinkSource, PullRequestSummary,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub host: Option<String>,
    pub allow_stale: bool,
}

impl PullRequestRef {
    pub fn validate(&self) -> Result<(), String> {
        if self.project_id.trim().is_empty() {
            return Err("project id is required".into());
        }
        if self.repository.trim().is_empty() || self.number == 0 {
            return Err("repository and a positive number are required".into());
        }
        Ok(())
    }

    pub fn key(&self) -> PullRequestKey {
        PullRequestKey::new(
            self.host.as_deref().unwrap_or("github.com"),
            &self.repository,
            self.number,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestListState {
    All,
    Open,
    Closed,
    Merged,
}

impl Default for PullRequestListState {
    fn default() -> Self {
        Self::Open
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPullRequests {
    pub project_id: String,
    pub repository: Option<String>,
    pub host: Option<String>,
    #[serde(default)]
    pub state: PullRequestListState,
    pub query: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
    pub fresh: bool,
}

impl ListPullRequests {
    pub fn validate(&self) -> Result<(), String> {
        if self.project_id.trim().is_empty() {
            return Err("project id is required".into());
        }
        if self.limit == 0 || self.limit > 100 {
            return Err("pull request limit must be between 1 and 100".into());
        }
        if self.query.as_deref().is_some_and(|query| query.chars().count() > 256) {
            return Err("pull request query is too long".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestList {
    pub entries: Vec<PullRequestSummary>,
    pub next_cursor: Option<String>,
    pub observed_at: agent_domain::Timestamp,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequest {
    pub reference: PullRequestRef,
}

impl GetPullRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    pub status: String,
    pub patch: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiff {
    pub reference: PullRequestRef,
    pub files: Vec<PullRequestDiffFile>,
    pub truncated: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequestDiff {
    pub reference: PullRequestRef,
    pub max_files: u32,
    pub max_patch_bytes: u32,
    pub fresh: bool,
}

impl GetPullRequestDiff {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.max_files == 0 || self.max_files > 2000 {
            return Err("diff file limit must be between 1 and 2000".into());
        }
        if self.max_patch_bytes == 0 || self.max_patch_bytes > 8 * 1024 * 1024 {
            return Err("diff patch limit must be between 1 byte and 8 MiB".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequestFile {
    pub reference: PullRequestRef,
    pub path: String,
    pub max_bytes: u32,
}

impl GetPullRequestFile {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.path.trim().is_empty()
            || self.path.len() > 4096
            || self.path.starts_with('/')
            || self.path.chars().any(|character| matches!(character, '?' | '#' | '\\'))
            || self.path.split('/').any(|segment| segment == "..")
        {
            return Err("file path is required and must be at most 4096 bytes".into());
        }
        if self.max_bytes == 0 || self.max_bytes > 8 * 1024 * 1024 {
            return Err("file limit must be between 1 byte and 8 MiB".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestFile {
    pub reference: PullRequestRef,
    pub path: String,
    #[serde(with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestViewedFile {
    pub path: String,
    pub viewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequestViewedFiles {
    pub reference: PullRequestRef,
    pub limit: u32,
}

impl GetPullRequestViewedFiles {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.limit == 0 || self.limit > 1_000 {
            return Err("viewed-file limit must be between 1 and 1000".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPullRequestFilesViewed {
    pub reference: PullRequestRef,
    pub files: Vec<PullRequestViewedFile>,
}

impl SetPullRequestFilesViewed {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.files.len() > 500 {
            return Err("at most 500 viewed files may be changed at once".into());
        }
        if self.files.iter().any(|file| {
            file.path.is_empty()
                || file.path.len() > 4096
                || file.path.starts_with('/')
                || file.path.chars().any(|character| matches!(character, '?' | '#' | '\\'))
                || file.path.split('/').any(|segment| segment == "..")
        }) {
            return Err("viewed-file paths must be relative and at most 4096 bytes".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestViewedFiles {
    pub reference: PullRequestRef,
    pub files: Vec<PullRequestViewedFile>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPullRequest {
    pub thread_id: String,
    pub project_id: String,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    #[serde(default)]
    pub source: PullRequestLinkSource,
    pub refresh: bool,
}

impl LinkPullRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.thread_id.trim().is_empty() || self.project_id.trim().is_empty() {
            return Err("thread and project ids are required".into());
        }
        if self.host.trim().is_empty() || self.repository.trim().is_empty() || self.number == 0 {
            return Err("host, repository and a positive number are required".into());
        }
        if self.url.trim().is_empty() {
            return Err("pull request URL is required".into());
        }
        let url = url::Url::parse(self.url.trim()).map_err(|_| "pull request URL is invalid")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("pull request URL must be a public HTTP URL".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlinkPullRequest {
    pub thread_id: String,
    pub project_id: String,
    pub host: String,
    pub repository: String,
    pub number: u64,
}

impl UnlinkPullRequest {
    pub fn key(&self) -> PullRequestKey {
        PullRequestKey::new(&self.host, &self.repository, self.number)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.thread_id.trim().is_empty() || self.project_id.trim().is_empty() || self.host.trim().is_empty() || self.repository.trim().is_empty() || self.number == 0 {
            return Err("thread, host, repository and a positive number are required".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPullRequestWatch {
    pub thread_id: String,
    pub project_id: String,
    pub link: PullRequestLink,
    pub enabled: bool,
}

impl SetPullRequestWatch {
    pub fn validate(&self) -> Result<(), String> {
        if self.thread_id.trim().is_empty() || self.project_id.trim().is_empty() || self.link.number == 0 {
            return Err("thread id and a positive pull request number are required".into());
        }
        if self.link.host.trim().is_empty() || self.link.repository.trim().is_empty() {
            return Err("pull request host and repository are required".into());
        }
        if self.link.url.trim().is_empty() {
            return Err("pull request URL is required".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackHead {
    pub number: u64,
    pub head_sha: String,
}

impl PullRequestStackHead {
    fn validate(&self) -> Result<(), String> {
        if self.number == 0 || self.head_sha.trim().is_empty() || self.head_sha.len() > 256 {
            return Err("stack heads require a positive number and a bounded SHA".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActionRequest {
    pub reference: PullRequestRef,
    pub action: PullRequestAction,
    #[serde(default)]
    pub stack_number: Option<u64>,
    #[serde(default)]
    pub expected_stack_heads: Option<Vec<PullRequestStackHead>>,
    #[serde(default)]
    pub merge_method: Option<PullRequestMergeMethod>,
}

impl PullRequestActionRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.stack_number.is_some_and(|number| number == 0) {
            return Err("stack number must be positive".into());
        }
        if self
            .expected_stack_heads
            .as_ref()
            .is_some_and(|heads| heads.len() > 100)
        {
            return Err("at most 100 stack heads may be checked".into());
        }
        if let Some(heads) = self.expected_stack_heads.as_ref() {
            for head in heads {
                head.validate()?;
            }
        }
        if self.stack_number.is_some() && self.expected_stack_heads.is_none() {
            return Err("stack actions require expected stack heads".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPullRequestReview {
    pub reference: PullRequestRef,
    pub verdict: PullRequestReviewVerdict,
    pub body: String,
}

impl SubmitPullRequestReview {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.body.chars().count() > 32_000 {
            return Err("review body is too long".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestOperation {
    pub reference: PullRequestRef,
    pub detail: Option<PullRequestDetail>,
    pub linked: Vec<PullRequestLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlAuthRequest {
    pub host: Option<String>,
    pub cwd: Option<String>,
    pub fresh: bool,
}

impl SourceControlAuthRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.host.as_deref().is_some_and(|host| host.trim().is_empty()) {
            return Err("source-control host must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlAuth {
    pub provider: String,
    pub host: String,
    pub authenticated: bool,
    pub account: Option<String>,
    pub message: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlDiscoveryRequest {
    pub cwd: String,
    pub fresh: bool,
}

impl SourceControlDiscoveryRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.cwd.trim().is_empty() {
            return Err("working directory is required".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceControlRepository {
    pub host: String,
    pub repository: String,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub web_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlDiscovery {
    pub provider: Option<String>,
    pub host: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub repository_info: Option<SourceControlRepository>,
    pub auth: Option<SourceControlAuth>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneRepository {
    pub url: String,
    pub destination: String,
    pub branch: Option<String>,
}

impl CloneRepository {
    pub fn validate(&self) -> Result<(), String> {
        if self.url.trim().is_empty() || self.destination.trim().is_empty() {
            return Err("clone URL and destination are required".into());
        }
        if self.destination.contains('\0') {
            return Err("clone destination contains an invalid character".into());
        }
        if let Ok(url) = url::Url::parse(self.url.trim())
            && (!url.username().is_empty() || url.password().is_some())
        {
            return Err("clone URLs must not contain credentials".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bounded_requests() {
        let request = ListPullRequests {
            project_id: "project".into(),
            repository: None,
            host: None,
            state: PullRequestListState::Open,
            query: None,
            limit: 100,
            cursor: None,
            fresh: false,
        };
        assert!(request.validate().is_ok());
        assert!(ListPullRequests { limit: 101, ..request }.validate().is_err());
    }
}
