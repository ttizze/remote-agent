//! Pull request and source-control RPC records.
//!
//! These records carry provider data and user intents across the native
//! boundary.  The provider remains at the Host boundary; clients consume the
//! shared domain values.
use agent_domain::{
    PullRequestAction, PullRequestDetail, PullRequestKey, PullRequestLink, PullRequestLinkSource,
    PullRequestSummary,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestListState {
    All,
    #[default]
    Open,
    Closed,
    Merged,
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
        if self
            .query
            .as_deref()
            .is_some_and(|query| query.chars().count() > 256)
        {
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
    pub patch: String,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub omitted_file_stats: Option<Vec<PullRequestOmittedFileStat>>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestOmittedFileStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequestDiff {
    pub reference: PullRequestRef,
    pub cursor: Option<String>,
    pub commit: Option<String>,
    pub fresh: bool,
}

impl GetPullRequestDiff {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self
            .cursor
            .as_deref()
            .is_some_and(|cursor| !cursor.chars().all(|character| character.is_ascii_digit()))
        {
            return Err("diff cursor must be a decimal page number".into());
        }
        if self
            .cursor
            .as_deref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 7 || cursor == "0")
        {
            return Err("diff cursor must be a bounded positive page number".into());
        }
        if self.commit.as_deref().is_some_and(|commit| {
            !(7..=64).contains(&commit.len())
                || !commit
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        }) {
            return Err("diff commit must be a hexadecimal revision".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestDiffChangeType {
    Change,
    RenamePure,
    RenameChanged,
    New,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPullRequestDiffFileContents {
    pub reference: PullRequestRef,
    pub commit: Option<String>,
    pub change_type: PullRequestDiffChangeType,
    pub old_path: String,
    pub new_path: String,
}

impl GetPullRequestDiffFileContents {
    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        if self.commit.as_deref().is_some_and(|commit| {
            !(7..=64).contains(&commit.len())
                || !commit
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        }) {
            return Err("diff commit must be a hexadecimal revision".into());
        }
        validate_file_path(&self.old_path)?;
        validate_file_path(&self.new_path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

fn validate_file_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.contains('\0')
        || path.contains('\\')
        || path.split('/').any(|segment| segment == "..")
    {
        return Err("file path is required and must be at most 4096 bytes".into());
    }
    Ok(())
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
        validate_file_path(&self.path)?;
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
        if self
            .files
            .iter()
            .any(|file| validate_file_path(&file.path).is_err())
        {
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
        if self.thread_id.trim().is_empty()
            || self.project_id.trim().is_empty()
            || self.host.trim().is_empty()
            || self.repository.trim().is_empty()
            || self.number == 0
        {
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
        if self.thread_id.trim().is_empty()
            || self.project_id.trim().is_empty()
            || self.link.number == 0
        {
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
        if self
            .host
            .as_deref()
            .is_some_and(|host| host.trim().is_empty())
        {
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
        assert!(
            ListPullRequests {
                limit: 101,
                ..request
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn diff_requests_page_without_dropping_legal_url_delimiters_in_paths() {
        let reference = PullRequestRef {
            project_id: "project".into(),
            repository: "owner/repository".into(),
            number: 7,
            host: Some("github.example".into()),
            allow_stale: false,
        };
        let request = GetPullRequestDiff {
            reference: reference.clone(),
            cursor: Some("2".into()),
            commit: Some("abcdef1".into()),
            fresh: true,
        };
        assert!(request.validate().is_ok());
        assert!(
            GetPullRequestFile {
                reference: reference.clone(),
                path: "docs/a?b#c.txt".into(),
                max_bytes: 1024,
            }
            .validate()
            .is_ok()
        );
        assert!(
            SetPullRequestFilesViewed {
                reference,
                files: vec![PullRequestViewedFile {
                    path: "docs/a?b#c.txt".into(),
                    viewed: true,
                }],
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn rejects_unbounded_diff_cursors_and_path_traversal() {
        let reference = PullRequestRef {
            project_id: "project".into(),
            repository: "owner/repository".into(),
            number: 7,
            host: None,
            allow_stale: false,
        };
        assert!(
            GetPullRequestDiff {
                reference: reference.clone(),
                cursor: Some("0".into()),
                commit: None,
                fresh: false,
            }
            .validate()
            .is_err()
        );
        assert!(
            GetPullRequestDiffFileContents {
                reference,
                commit: None,
                change_type: PullRequestDiffChangeType::Change,
                old_path: "../secret".into(),
                new_path: "safe".into(),
            }
            .validate()
            .is_err()
        );
    }
}
