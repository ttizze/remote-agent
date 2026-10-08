//! What the composer and the diff panel read from a workspace: provider skills
//! and slash commands, path search for `@` mentions, Git status, refs and the
//! uncommitted and branch-range diffs.
use serde::{Deserialize, Serialize};

/// `host/provider/commands`: the skills and slash commands an instance offers
/// in a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProviderCommands {
    pub instance: String,
    pub cwd: String,
    /// Scan again instead of answering from the Host's cache.
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCommands {
    pub instance: String,
    pub cwd: String,
    pub slash_commands: Vec<SlashCommand>,
    /// The provider has not reported its commands yet; ask again later.
    pub slash_commands_pending: bool,
    pub skills: Vec<ProviderSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    /// What the command's argument means.
    pub input_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSkill {
    pub name: String,
    pub path: String,
    pub enabled: bool,
    pub description: Option<String>,
    /// `user`, `project`, `repo`, `system`…
    pub scope: Option<String>,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    /// Only the user can run it, so it is offered under `/`.
    pub user_invocation_only: bool,
    /// False when the provider reserves it for the agent.
    pub user_invocable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Directory,
}

/// `host/workspace/searchEntries`: files and directories under `cwd` matching
/// `query` (at most 256 characters); an empty query lists entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEntries {
    pub cwd: String,
    pub query: String,
    /// 1 to 200.
    pub limit: u32,
    pub kind: Option<EntryKind>,
    /// Only files a client can preview as images.
    pub image_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    /// Relative to the searched directory, with `/` separators.
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySearch {
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
}

pub const CONTENT_SEARCH_MAX_QUERY: usize = 256;
pub const CONTENT_SEARCH_MAX_LIMIT: u32 = 500;

/// `host/workspace/searchContents`: searches text files under `cwd` while
/// preserving the query exactly as entered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchContents {
    pub cwd: String,
    pub query: String,
    pub limit: u32,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}
impl SearchContents {
    pub fn validate(&self) -> Result<(), String> {
        if self.cwd.trim().is_empty() {
            return Err("workspace directory is required".into());
        }
        if self.query.is_empty() {
            return Err("content search query is required".into());
        }
        if self.query.chars().count() > CONTENT_SEARCH_MAX_QUERY {
            return Err(format!(
                "content search query exceeds {CONTENT_SEARCH_MAX_QUERY} characters"
            ));
        }
        if !(1..=CONTENT_SEARCH_MAX_LIMIT).contains(&self.limit) {
            return Err(format!(
                "content search limit must be 1 to {CONTENT_SEARCH_MAX_LIMIT}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatchRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    pub path: String,
    pub line_number: u32,
    pub line_content: String,
    pub match_ranges: Vec<ContentMatchRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearch {
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
    pub regex_fallback_error: Option<String>,
}

/// `host/vcs/status`: the checkout's branch and its uncommitted and branch
/// changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadVcsStatus {
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeTotals {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeChanges {
    pub files: Vec<FileChangeTotals>,
    pub insertions: u64,
    pub deletions: u64,
}

/// The `Changes` totals: the working tree against the merge base with `base_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchChanges {
    pub base_ref: Option<String>,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatus {
    pub is_repo: bool,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    /// `None` on a detached HEAD.
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: WorkingTreeChanges,
    pub branch_changes: Option<BranchChanges>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefKind {
    All,
    Local,
    Remote,
}

/// `host/vcs/listRefs`: branches for the base picker, newest commit first with
/// the current and default branches leading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRefs {
    pub cwd: String,
    /// Case-insensitive name filter, at most 256 characters.
    pub query: Option<String>,
    pub cursor: Option<u32>,
    /// Keeps remote branches that a local branch tracks under the same name.
    pub include_matching_remote_refs: bool,
    pub ref_kind: RefKind,
    /// 1 to 200; 100 when absent.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRef {
    pub name: String,
    pub is_remote: bool,
    pub remote_name: Option<String>,
    pub current: bool,
    pub is_default: bool,
    /// The worktree that has this branch checked out.
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefList {
    pub refs: Vec<VcsRef>,
    pub is_repo: bool,
    pub has_primary_remote: bool,
    pub next_cursor: Option<u32>,
    pub total_count: u32,
}

/// `host/vcs/switchRef`: checks out a branch in a checkout, tracking a remote
/// branch locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchRef {
    pub cwd: String,
    pub ref_name: String,
}

/// `host/vcs/createRef`: creates a branch in a checkout at its HEAD, and
/// checks it out when `switch_ref` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRef {
    pub cwd: String,
    pub ref_name: String,
    pub switch_ref: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchedRef {
    /// The branch checked out afterwards; `None` on a detached HEAD.
    pub ref_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffSourceKind {
    /// `Uncommitted`: the working tree against HEAD.
    WorkingTree,
    /// `Changes`: the working tree against the merge base with the base ref.
    BranchRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreviewFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub source: DiffSourceKind,
}

/// `host/review/diffPreview`: both diffs of a checkout, or one file's patch of
/// one of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreview {
    pub cwd: String,
    /// The `Changes` base; chosen automatically when absent.
    pub base_ref: Option<String>,
    pub ignore_whitespace: bool,
    pub file: Option<DiffPreviewFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSource {
    pub id: String,
    pub kind: DiffSourceKind,
    pub title: String,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub diff: String,
    /// Changes whenever the diff or its file list changes.
    pub diff_hash: String,
    pub truncated: bool,
    /// `None` when the untracked file listing overflowed.
    pub files: Option<Vec<DiffFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreviewResult {
    pub cwd: String,
    pub generated_at: agent_domain::Timestamp,
    /// Empty outside a Git checkout.
    pub sources: Vec<DiffSource>,
}
