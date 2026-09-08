//! Wire contracts owned by the Host. Codex payloads remain lossless JSON.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub const ACCOUNT_LIST: &str = "host/account/list";
pub const ACCOUNT_SELECT: &str = "host/account/select";
pub const ACCOUNT_LOGIN_START: &str = "host/account/login/start";
pub const ACCOUNT_LOGIN_STATUS: &str = "host/account/login/status";
pub const ACCOUNT_LOGIN_CANCEL: &str = "host/account/login/cancel";
pub const WORKTREE_SETTINGS_READ: &str = "host/worktree/settings/read";
pub const WORKTREE_SETTINGS_UPDATE: &str = "host/worktree/settings/update";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub plan_type: String,
    pub chatgpt_account_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelectParams<'a> {
    #[serde(borrow)]
    pub account_id: &'a str,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginParams<'a> {
    #[serde(borrow)]
    pub login_id: &'a str,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountList<'a> {
    pub accounts: Cow<'a, [Account]>,
    pub selected_id: Option<Cow<'a, str>>,
    pub error: Option<Cow<'a, str>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection<'a> {
    pub selected_id: Cow<'a, str>,
    pub persistence_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLogin<'a> {
    pub login_id: Cow<'a, str>,
    pub user_code: Cow<'a, str>,
    pub verification_url: Cow<'a, str>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStatus<'a> {
    pub completed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Cow<'a, str>>,
}

#[derive(Serialize, Deserialize)]
pub struct HostError<'a> {
    #[serde(borrow)]
    pub code: &'a str,
    #[serde(borrow)]
    pub message: &'a str,
}

pub const FILE_LIST: &str = "host/file/list";
pub const FILE_READ: &str = "host/file/read";
pub const FILE_WRITE: &str = "host/file/write";
pub const WORKSPACE_REVIEW: &str = "host/workspace/review";

#[derive(Serialize, Deserialize)]
pub struct FilePathParams<'a> {
    pub path: Cow<'a, std::path::Path>,
}

#[derive(Serialize, Deserialize)]
pub struct FileWriteParams<'a> {
    pub path: Cow<'a, std::path::Path>,
    pub revision: Cow<'a, str>,
    pub text: Cow<'a, str>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceReviewParams<'a> {
    pub cwd: Cow<'a, str>,
}

#[derive(Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: std::path::PathBuf,
    pub directory: bool,
    pub size: u64,
}

#[derive(Serialize, Deserialize)]
pub struct FileList {
    pub path: std::path::PathBuf,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDocument<'a> {
    pub path: Cow<'a, std::path::Path>,
    pub revision: String,
    pub text: Cow<'a, str>,
    pub bom: bool,
    pub line_ending: Cow<'a, str>,
    pub size: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReview {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<WorkspaceFileChange>,
    pub diff: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileChange {
    pub path: String,
    pub status: Cow<'static, str>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}
