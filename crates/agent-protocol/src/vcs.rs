//! Git operations: the live status stream with its remote half, pull, the
//! stacked commit / push / create-PR actions with their progress, `git init`,
//! worktrees from the branch picker, pull requests checked out as threads and
//! publishing a repository.
use agent_domain::ThreadId;
use serde::{Deserialize, Serialize};

use crate::workspace::{BranchChanges, VcsStatus, WorkingTreeChanges};

/// The hosting provider a remote belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceControlProviderKind {
    Github,
    Gitlab,
    Forgejo,
    AzureDevops,
    Bitbucket,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderInfo {
    pub kind: SourceControlProviderKind,
    /// "GitHub", "GitLab Self-Hosted", or the host for an unknown provider.
    pub name: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestState {
    Open,
    Closed,
    Merged,
}

/// The pull request of the checked-out branch, as the status reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref: String,
    pub head_ref: String,
    pub state: ChangeRequestState,
    pub is_draft: bool,
    /// The provider's last activity (ISO 8601), comments included.
    pub updated_at: Option<String>,
}

/// What `git status` and the local refs say; read on every refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusLocal {
    pub is_repo: bool,
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: WorkingTreeChanges,
    pub branch_changes: Option<BranchChanges>,
}

/// What the remote says: the upstream's divergence and the branch's pull
/// request; fetched on its own cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusRemote {
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: Option<u64>,
    pub pr: Option<VcsStatusChangeRequest>,
}

impl VcsStatusRemote {
    /// What a checkout without a remote reading shows.
    pub fn empty() -> Self {
        Self {
            has_upstream: false,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: Some(0),
            pr: None,
        }
    }
}

impl VcsStatusLocal {
    pub fn not_repository() -> Self {
        Self {
            is_repo: false,
            source_control_provider: None,
            has_primary_remote: false,
            is_default_ref: false,
            ref_name: None,
            has_working_tree_changes: false,
            working_tree: WorkingTreeChanges {
                files: vec![],
                insertions: 0,
                deletions: 0,
            },
            branch_changes: None,
        }
    }
}

impl VcsStatus {
    /// The local half with the remote half, or without one.
    pub fn merge(local: VcsStatusLocal, remote: Option<VcsStatusRemote>) -> Self {
        let remote = remote.unwrap_or_else(VcsStatusRemote::empty);
        Self {
            is_repo: local.is_repo,
            source_control_provider: local.source_control_provider,
            has_primary_remote: local.has_primary_remote,
            is_default_ref: local.is_default_ref,
            ref_name: local.ref_name,
            has_working_tree_changes: local.has_working_tree_changes,
            working_tree: local.working_tree,
            branch_changes: local.branch_changes,
            has_upstream: remote.has_upstream,
            ahead_count: remote.ahead_count,
            behind_count: remote.behind_count,
            ahead_of_default_count: remote.ahead_of_default_count,
            pr: remote.pr,
        }
    }
    pub fn local(&self) -> VcsStatusLocal {
        VcsStatusLocal {
            is_repo: self.is_repo,
            source_control_provider: self.source_control_provider.clone(),
            has_primary_remote: self.has_primary_remote,
            is_default_ref: self.is_default_ref,
            ref_name: self.ref_name.clone(),
            has_working_tree_changes: self.has_working_tree_changes,
            working_tree: self.working_tree.clone(),
            branch_changes: self.branch_changes.clone(),
        }
    }
    pub fn remote(&self) -> VcsStatusRemote {
        VcsStatusRemote {
            has_upstream: self.has_upstream,
            ahead_count: self.ahead_count,
            behind_count: self.behind_count,
            ahead_of_default_count: self.ahead_of_default_count,
            pr: self.pr.clone(),
        }
    }
}

/// `host/vcs/subscribeStatus`: the status of a checkout, then every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeVcsStatus {
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VcsStatusStreamEvent {
    Snapshot {
        local: VcsStatusLocal,
        remote: Option<VcsStatusRemote>,
    },
    LocalUpdated {
        local: VcsStatusLocal,
    },
    RemoteUpdated {
        remote: Option<VcsStatusRemote>,
    },
}

/// `host/vcs/refreshStatus`: reads the checkout again, remote included, and
/// answers with the merged status; subscribers get the changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshVcsStatus {
    pub cwd: String,
}

/// `host/vcs/pull`: `git pull --ff-only` on the checked-out branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pull {
    pub cwd: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullStatus {
    Pulled,
    SkippedUpToDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub status: PullStatus,
    pub ref_name: String,
    pub upstream_ref: Option<String>,
}

/// The stacked Git actions the controls run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackedAction {
    Commit,
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
}

impl StackedAction {
    pub fn commits(self) -> bool {
        matches!(self, Self::Commit | Self::CommitPush | Self::CommitPushPr)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Push => "push",
            Self::CreatePr => "create_pr",
            Self::CommitPush => "commit_push",
            Self::CommitPushPr => "commit_push_pr",
        }
    }
}

/// `host/vcs/runStackedAction`: runs the action and streams its progress; the
/// stream ends with `ActionFinished` or `ActionFailed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStackedAction {
    /// The client's id for the action; every event carries it.
    pub action_id: String,
    pub cwd: String,
    pub action: StackedAction,
    /// Used instead of a generated message; at most 10,000 characters.
    pub commit_message: Option<String>,
    /// Commit on a new `feature/…` branch first.
    pub feature_branch: bool,
    /// Only these paths are committed; `None` commits everything.
    pub file_paths: Option<Vec<String>>,
    /// The thread the action runs beside; a pull request it creates is
    /// linked to it.
    pub thread_id: Option<ThreadId>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Branch,
    Commit,
    Push,
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStepStatus {
    Created,
    SkippedNotRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchStep {
    pub status: BranchStepStatus,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStepStatus {
    Created,
    SkippedNoChanges,
    SkippedNotRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitStep {
    pub status: CommitStepStatus,
    pub commit_sha: Option<String>,
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushStepStatus {
    Pushed,
    SkippedNotRequested,
    SkippedUpToDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushStep {
    pub status: PushStepStatus,
    pub branch: Option<String>,
    pub upstream_branch: Option<String>,
    pub set_upstream: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrStepStatus {
    Created,
    OpenedExisting,
    SkippedNotRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrStep {
    pub status: PrStepStatus,
    pub url: Option<String>,
    pub number: Option<u64>,
    pub base_branch: Option<String>,
    pub head_branch: Option<String>,
    pub title: Option<String>,
}

/// What the completion toast offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ActionToastCta {
    None,
    OpenPr {
        label: String,
        url: String,
    },
    RunAction {
        label: String,
        action: StackedAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionToast {
    pub title: String,
    pub description: Option<String>,
    pub cta: ActionToastCta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackedActionResult {
    pub action: StackedAction,
    pub branch: BranchStep,
    pub commit: CommitStep,
    pub push: PushStep,
    pub pr: PrStep,
    pub toast: ActionToast,
}

/// One progress event of a stacked action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProgressEvent {
    pub action_id: String,
    pub cwd: String,
    pub action: StackedAction,
    pub kind: ActionProgressKind,
}

// Keep the terminal result inline: this public wire event is constructed and
// matched by the Host and every client, and boxing it would add an allocation
// and migration to every caller without a meaningful runtime ownership role.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ActionProgressKind {
    ActionStarted {
        phases: Vec<ActionPhase>,
    },
    PhaseStarted {
        phase: ActionPhase,
        label: String,
    },
    HookStarted {
        hook_name: String,
    },
    HookOutput {
        hook_name: Option<String>,
        stream: OutputStream,
        text: String,
    },
    HookFinished {
        hook_name: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    ActionFinished {
        result: StackedActionResult,
    },
    ActionFailed {
        phase: Option<ActionPhase>,
        message: String,
    },
}

/// `host/vcs/init`: `git init` in a folder without a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitRepository {
    pub cwd: String,
}

/// `host/vcs/createWorktree`: a worktree of `ref_name`, or of a new branch
/// `new_ref_name` started at `ref_name`; `base_ref_name` records the base the
/// Changes view compares with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktree {
    pub cwd: String,
    pub ref_name: String,
    pub new_ref_name: Option<String>,
    pub base_ref_name: Option<String>,
    /// Absent: the Host's worktree directory.
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCheckout {
    pub path: String,
    pub ref_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub worktree: WorktreeCheckout,
}

/// `host/vcs/removeWorktree`: `git worktree remove`; one already gone is
/// pruned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorktreeCheckout {
    pub cwd: String,
    pub path: String,
    pub force: bool,
}

/// `host/git/resolvePullRequest`: the pull request `reference` names
/// (a number, `#number` or a URL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvePullRequest {
    pub cwd: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_branch: String,
    pub head_branch: String,
    pub state: ChangeRequestState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPullRequestResult {
    pub pull_request: ResolvedPullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestThreadMode {
    /// Checks the pull request out in the project's checkout.
    Local,
    /// Checks it out in a worktree of its own.
    Worktree,
}

/// `host/git/preparePullRequestThread`: checks a pull request out for a
/// thread to work on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparePullRequestThread {
    pub cwd: String,
    pub reference: String,
    pub mode: PullRequestThreadMode,
    pub thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPullRequestThread {
    pub pull_request: ResolvedPullRequest,
    pub branch: String,
    pub worktree_path: Option<String>,
    /// False when a reused worktree kept its own commits or changes, so the
    /// checkout is older than the pull request.
    pub is_on_pull_request_head: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RepositoryVisibility {
    Private,
    Public,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloneProtocol {
    Auto,
    Ssh,
    Https,
}

/// `host/sourceControl/publishRepository`: creates `repository` on the
/// provider, adds it as a remote and pushes the checked-out branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRepository {
    pub cwd: String,
    pub provider: SourceControlProviderKind,
    /// `owner/name`.
    pub repository: String,
    pub visibility: RepositoryVisibility,
    /// `origin` when absent.
    pub remote_name: Option<String>,
    pub protocol: Option<CloneProtocol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryInfo {
    pub provider: SourceControlProviderKind,
    pub name_with_owner: String,
    pub url: String,
    pub ssh_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    Pushed,
    /// The repository has no commit yet; the remote was added.
    RemoteAdded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedRepository {
    pub repository: RepositoryInfo,
    pub remote_name: String,
    pub remote_url: String,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub status: PublishStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> VcsStatusLocal {
        VcsStatusLocal {
            is_repo: true,
            source_control_provider: Some(SourceControlProviderInfo {
                kind: SourceControlProviderKind::Github,
                name: "GitHub".into(),
                base_url: "https://github.com".into(),
            }),
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some("feature/demo".into()),
            has_working_tree_changes: true,
            working_tree: WorkingTreeChanges {
                files: vec![],
                insertions: 1,
                deletions: 0,
            },
            branch_changes: None,
        }
    }

    // shared/git.ts mergeGitStatusParts: the halves round-trip through the
    // merged status, and a missing remote half reads as no remote.
    #[test]
    fn the_merged_status_splits_back_into_its_halves() {
        let remote = VcsStatusRemote {
            has_upstream: true,
            ahead_count: 2,
            behind_count: 1,
            ahead_of_default_count: Some(3),
            pr: None,
        };
        let merged = VcsStatus::merge(local(), Some(remote.clone()));
        assert_eq!(merged.local(), local());
        assert_eq!(merged.remote(), remote);
        let without = VcsStatus::merge(local(), None);
        assert_eq!(without.remote(), VcsStatusRemote::empty());
        assert!(!without.has_upstream);
        assert_eq!(without.ahead_of_default_count, Some(0));
    }

    // contracts git.test.ts "decodes a server-authored completion toast".
    #[test]
    fn a_stacked_action_result_decodes_its_toast_action() {
        let result: StackedActionResult = serde_json::from_value(serde_json::json!({
            "action": "commit_push",
            "branch": {"status": "created", "name": "feature/server-owned-toast"},
            "commit": {"status": "created", "commitSha": "89abcdef01234567", "subject": "feat: move toast state into git manager"},
            "push": {"status": "pushed", "branch": "feature/server-owned-toast", "upstreamBranch": "origin/feature/server-owned-toast"},
            "pr": {"status": "skipped_not_requested"},
            "toast": {
                "title": "Pushed 89abcde to origin/feature/server-owned-toast",
                "description": "feat: move toast state into git manager",
                "cta": {"run_action": {"label": "Create PR", "action": "create_pr"}}
            }
        }))
        .unwrap();
        assert_eq!(
            result.toast.cta,
            ActionToastCta::RunAction {
                label: "Create PR".into(),
                action: StackedAction::CreatePr
            }
        );
        assert_eq!(result.push.set_upstream, None);
    }
}
