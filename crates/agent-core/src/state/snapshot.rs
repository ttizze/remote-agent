//! Client snapshot types. Reduce logic stays in `state`.
pub use crate::models::{
    FileContent, FileList, HostStatus, Invitation, Item, ListQuery, Model, RemoteHost, Thread,
    ThreadList, WorkspaceReview, WorktreeSettings,
};
pub use agent_protocol::operations::{Answer, ServerRequest};
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::{Map, Value};
pub(crate) use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub invocations: Vec<agent_protocol::composer::Invocation>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Attachment {
    pub path: String,
    pub name: String,
    pub is_image: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileDraft {
    pub revision: String,
    pub text: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub directory: Option<Arc<FileList>>,
    pub file: Option<Arc<FileContent>>,
    pub review_cwd: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<crate::models::Worktree>>>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Navigation {
    pub thread_id: Option<String>,
    pub cwd: String,
    pub draft_key: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(skip)]
    pub active: BTreeMap<String, bool>,
    pub unread: BTreeSet<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostManagement {
    pub status: Option<Arc<HostStatus>>,
    pub remotes: Vec<RemoteHost>,
    #[serde(skip)]
    pub invitation: Option<Arc<Invitation>>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    pub accounts: Option<Arc<agent_protocol::operations::Accounts>>,
    #[serde(skip)]
    pub login: Option<Arc<agent_protocol::operations::AccountLogin>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmission {
    pub sequence: u64,
    pub draft_key: String,
    pub draft: Arc<Draft>,
    pub turn_id: Option<String>,
    pub after_item_id: Option<String>,
    pub accepted: bool,
    pub delivery_unknown: bool,
}
impl PendingSubmission {
    pub fn delivery_label(&self) -> &'static str {
        if self.delivery_unknown {
            "送信結果不明（自動再送しません）"
        } else if self.accepted {
            "送信済み"
        } else {
            "送信中…"
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TerminalPhase {
    Suspended,
    Detached,
    Starting,
    Running,
    Exited(i32),
    Failed(String),
}
impl TerminalPhase {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => "起動中".into(),
            Self::Running => "実行中".into(),
            Self::Suspended => "再接続を待っています".into(),
            Self::Detached => "切断済み".into(),
            Self::Exited(code) => format!("終了 · {code}"),
            Self::Failed(error) => error.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutput {
    pub sequence: u64,
    #[serde(with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
    pub reset_size: Option<agent_protocol::operations::TerminalSize>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub cwd: String,
    pub size: agent_protocol::operations::TerminalSize,
    pub phase: TerminalPhase,
    pub output: VecDeque<Arc<TerminalOutput>>,
    pub sequence: u64,
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalView {
    pub status: Option<String>,
    pub loading: bool,
    pub accepts_input: bool,
    pub output: Vec<TerminalOutput>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    #[serde(skip)]
    pub composer_catalog: Option<Arc<agent_protocol::composer::ComposerCatalog>>,
    #[serde(skip)]
    pub host_name: Option<String>,
    pub storage_scope: String,
    pub archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    #[serde(default)]
    pub account: Arc<AccountState>,
    #[serde(skip)]
    pub terminals: Arc<BTreeMap<String, Arc<Terminal>>>,
    #[serde(default)]
    pub conversations: Arc<BTreeMap<String, Arc<Thread>>>,
    #[serde(default)]
    pub threads: Option<Arc<ThreadList>>,
    #[serde(default)]
    pub models: Arc<Vec<Model>>,
    #[serde(default)]
    pub model_errors: Arc<Map<String, Value>>,
    #[serde(skip)]
    pub requests: Arc<BTreeMap<String, Arc<ServerRequest>>>,
    pub drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    pub pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    #[serde(default)]
    pub workspace: Arc<Workspace>,
    pub navigation: Arc<Navigation>,
    pub activity: Arc<Activity>,
    #[serde(default)]
    pub management: Arc<HostManagement>,
    #[serde(skip)]
    pub list_query: Arc<ListQuery>,
    #[serde(default)]
    pub epoch: u64,
    #[serde(skip)]
    pub connected: bool,
    #[serde(skip)]
    pub subscriptions: Arc<BTreeMap<String, uuid::Uuid>>,
    #[serde(skip)]
    pub error: Option<String>,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn terminal_view(&self, handle: String) -> Option<TerminalView> {
        self.terminals.get(&handle).map(|terminal| TerminalView {
            status: match terminal.phase {
                TerminalPhase::Starting | TerminalPhase::Running => None,
                _ => Some(terminal.phase.label()),
            },
            loading: matches!(terminal.phase, TerminalPhase::Starting),
            accepts_input: matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running
            ),
            output: terminal
                .output
                .iter()
                .map(|chunk| (**chunk).clone())
                .collect(),
        })
    }

    pub fn model_error_messages(&self) -> Vec<String> {
        self.model_errors
            .iter()
            .map(|(provider, error)| {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string());
                format!("{provider}: {message}")
            })
            .collect()
    }
}

/// Client-owned data from a previously configured Host storage area. Keeping
/// it separate prevents storage switches from deleting or mixing user drafts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopedData {
    pub drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    pub pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    pub navigation: Arc<Navigation>,
    pub activity: Arc<Activity>,
}
