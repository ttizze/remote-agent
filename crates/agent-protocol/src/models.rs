//! Shared typed models. Unknown provider fields are ignored.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};

pub const MAX_INLINE_ITEM_BYTES: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    pub endpoint: String,
    pub invitation: uuid::Uuid,
    pub expires_at: u64,
    pub host_name: String,
    pub ai_recipients: Vec<String>,
    pub transcription_recipient: Option<String>,
}
impl std::fmt::Debug for Invitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invitation")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteHost {
    pub id: String,
    pub name: String,
    pub ticket: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub node_id: String,
    pub name: String,
    pub devices: Vec<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub provider_errors: Option<Map<String, Value>>,
}

/// Provider membership before Host enrichment; null alone does not rule out cwd membership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectMembership {
    Unknown {},
    Unassigned {},
    Assigned(String),
}
impl Default for ProjectMembership {
    fn default() -> Self {
        Self::Unknown {}
    }
}
impl ProjectMembership {
    pub fn as_ref(&self) -> Option<&String> {
        if let Self::Assigned(id) = self {
            Some(id)
        } else {
            None
        }
    }
    pub fn as_deref(&self) -> Option<&str> {
        self.as_ref().map(String::as_str)
    }
    pub fn is_none(&self) -> bool {
        self.as_ref().is_none()
    }
}
fn project_membership<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<ProjectMembership, D::Error> {
    if !d.is_human_readable() {
        return ProjectMembership::deserialize(d);
    }
    let value = Value::deserialize(d)?;
    match value {
        Value::Null => Ok(ProjectMembership::Unassigned {}),
        Value::String(id) => Ok(ProjectMembership::Assigned(id)),
        value => serde_json::from_value(value).map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub history_read_state: Option<crate::session::HistoryReadState>,
    pub capabilities: Option<crate::session::Capabilities>,
    #[serde(default)]
    pub requests: BTreeMap<crate::ids::RequestId, Arc<crate::requests::Request>>,
    #[serde(default)]
    pub submissions: BTreeMap<crate::ids::ClientInputId, crate::session::SubmissionDelivery>,
    pub id: Option<crate::session::SessionRef>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub worktree_status: Option<WorktreeStatus>,
    #[serde(default)]
    pub status: SessionStatus,
    pub turns: Option<Vec<Arc<Turn>>>,
    #[serde(default, deserialize_with = "project_membership")]
    pub project_id: ProjectMembership,
    pub preview: Option<String>,
    pub updated_at: Option<f64>,
    pub history_has_more: Option<bool>,
    pub history_cursor: Option<String>,
    pub history_limit: Option<u64>,
    pub list_stale: Option<bool>,
    pub agent_id: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: crate::ids::TurnId,
    #[serde(default)]
    pub status: TurnStatus,
    pub items: Option<Vec<Arc<Item>>>,
    #[serde(default)]
    pub items_summary: bool,
    pub started_at: Option<f64>,
    pub duration_ms: Option<u64>,
    pub error: Option<ExecutionError>,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
}
pub use crate::{execution::*, items::*};

impl Thread {
    pub fn active_turn_id(&self) -> Option<crate::ids::TurnId> {
        self.turns
            .as_ref()?
            .iter()
            .rev()
            .find(|turn| turn.status == TurnStatus::Running)
            .map(|turn| turn.id.clone())
    }
}

impl Thread {
    /// Keep RPC snapshots small; item reads recover every deferred body.
    pub fn defer_item_details(&mut self, max_inline_bytes: usize) {
        if let Some(turns) = &mut self.turns {
            defer_item_details(turns, max_inline_bytes);
        }
    }
}

pub fn defer_item_details(turns: &mut [Arc<Turn>], max_inline_bytes: usize) {
    for turn in turns {
        let turn = Arc::make_mut(turn);
        if let Some(items) = &mut turn.items {
            defer_items(items, max_inline_bytes);
        }
    }
}

pub fn defer_items(items: &mut [Arc<Item>], max_inline_bytes: usize) {
    for item in items {
        let limit = if matches!(
            item.body(),
            ItemBody::UserMessage { .. }
                | ItemBody::AssistantText { .. }
                | ItemBody::ImageGeneration { .. }
        ) {
            max_inline_bytes
        } else {
            512.min(max_inline_bytes)
        };
        if !item.id.is_empty() && !fits_inline(item, limit) {
            Arc::make_mut(item).defer();
        }
    }
}

fn fits_inline(value: &impl Serialize, limit: usize) -> bool {
    postcard::serialize_with_flavor::<_, postcard::ser_flavors::Size, usize>(
        value,
        Default::default(),
    )
    .is_ok_and(|size| size <= limit)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread: Thread,
    pub model: Option<ModelRef>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadList {
    pub data: Vec<Thread>,
    pub projects: Vec<Project>,
    pub more_project_ids: Vec<String>,
    pub has_more_chats: bool,
    pub has_more_projects: bool,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub provider_errors: Option<Map<String, Value>>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRoot {
    pub path: String,
}
/// Native model identity scoped by provider; neither field is encoded in the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: crate::session::ProviderKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: ModelRef,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceTier {
    pub id: String,
    pub name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffort {
    pub reasoning_effort: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListQuery {
    pub project_limit: u32,
    pub chat_limit: u32,
    pub project_thread_limits: BTreeMap<String, u32>,
    pub search_term: String,
}
impl Default for ListQuery {
    fn default() -> Self {
        Self {
            project_limit: 5,
            chat_limit: 5,
            project_thread_limits: BTreeMap::new(),
            search_term: String::new(),
        }
    }
}
impl ListQuery {
    pub fn for_connection(mut self) -> Self {
        if self.project_limit == 0 {
            self.project_limit = 5;
        }
        if self.chat_limit == 0 {
            self.chat_limit = 5;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub size: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    pub delete_merged: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub path: String,
    pub project_path: String,
    pub branch: String,
    pub blocked_reason: Option<String>,
    pub threads: Vec<WorktreeThread>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeThread {
    pub id: crate::session::SessionRef,
    pub name: String,
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceReview {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<ChangedFile>,
    pub diff: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    proptest! {
        #[test]
        fn worktree_status_distinguishes_pending_work_from_integrated_history(
            dirty in any::<bool>(),
            unmerged_changes in any::<bool>(),
            merged_history in any::<bool>(),
        ) {
            let status = worktree_branch_status(dirty, unmerged_changes, merged_history);
            let expected = match (dirty, unmerged_changes, merged_history) {
                (true, _, _) | (_, true, _) => Some(WorktreeStatus::Unmerged),
                (false, false, true) => Some(WorktreeStatus::Merged),
                _ => None,
            };
            prop_assert_eq!(status, expected);
        }
    }

    #[test]
    fn deferred_read_keeps_conversation_and_activity_headers() {
        let text = "会話".repeat(4096);
        let result = json!({"thread":{"turns":[{"id":"turn","items":[{"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":text}}]}}}}},{"id":"agent","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":text,"phase":"unknown"}}}}},{"id":"command","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"日本語".repeat(1000),"cwd":null,"output":text,"exitCode":null}}}}},{"id":"files","status":"completed","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"a.txt","kind":{"update":{"movePath":null}},"diff":text,"proposal":null}],"output":""}}}}},{"id":"future","status":"completed","clientInputId":null,"body":{"inline":{"body":{"custom":{"provider":"codex","kind":"futureTool","value":{"id":"future","type":"futureTool","tool":"inspect","status":"completed","result":{"content":text}}}}}}},{"id":"small","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":["short"]}}}}}],"status":"unknown"}]}});
        let mut typed: ThreadResponse = serde_json::from_value(result).unwrap();
        typed.thread.defer_item_details(MAX_INLINE_ITEM_BYTES);
        let items = typed.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap();
        assert!(
            matches!(items[0].body(),ItemBody::UserMessage {content,..} if content.first() == Some(&MessagePart::Text {text:text.clone()}))
        );
        assert!(
            matches!(items[1].body(),ItemBody::AssistantText {text:actual,..} if actual == &text)
        );
        assert!(
            matches!(items[2].body(),ItemBody::CommandExecution {command,..} if command.starts_with("日本語"))
        );
        assert_eq!(items[2].status, ItemStatus::Completed);
        assert!(
            matches!(items[3].body(),ItemBody::FileChange {changes,..} if changes[0].path == "a.txt" && matches!(changes[0].kind,FileChangeKind::Update {..}))
        );
        assert!(matches!(items[4].body(),ItemBody::Custom {kind,..} if kind == "futureTool"));
        assert!(
            matches!(items[5].body(),ItemBody::Reasoning {summary,..} if summary == &vec!["short"])
        );
        assert_eq!(
            items
                .iter()
                .filter(|item| item.is_deferred())
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["command", "files", "future"]
        );
    }

    #[test]
    fn generated_image_output_is_not_truncated_as_an_activity_detail() {
        let image = json!({"id":"image","status":"completed","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":format!("/{} image.png", "directory/".repeat(40)),"data":"A".repeat(8192),"revisedPrompt":null}}}}});
        let result = json!({"thread":{"turns":[{"id":"turn","items":[image],"status":"unknown"}]}});
        let mut typed: ThreadResponse = serde_json::from_value(result).unwrap();
        typed.thread.defer_item_details(MAX_INLINE_ITEM_BYTES);
        let item = &typed.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0];
        assert_eq!(serde_json::to_value(item.as_ref()).unwrap(), image);
        assert!(!item.is_deferred());
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Empty {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferGrant {
    pub token: [u8; 32],
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadedFile {
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

pub fn compact_title(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

pub fn task_active(observed: Option<bool>, status: SessionStatus) -> bool {
    observed.unwrap_or(status == SessionStatus::Running)
}
pub fn task_title<'a>(name: Option<&'a str>, preview: Option<&'a str>) -> &'a str {
    name.filter(|name| !name.is_empty())
        .or_else(|| preview.filter(|preview| !preview.is_empty()))
        .unwrap_or("無題のタスク")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeStatus {
    Unmerged,
    Merged,
}

/// Pending file changes take priority over previously integrated branch work.
pub fn worktree_branch_status(
    has_uncommitted_changes: bool,
    has_unmerged_changes: bool,
    has_merged_history: bool,
) -> Option<WorktreeStatus> {
    if has_uncommitted_changes || has_unmerged_changes {
        Some(WorktreeStatus::Unmerged)
    } else if has_merged_history {
        Some(WorktreeStatus::Merged)
    } else {
        None
    }
}
