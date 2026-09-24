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
    pub session: Option<crate::session::SessionRef>,
    pub capabilities: Option<crate::session::Capabilities>,
    #[serde(default)]
    pub requests: BTreeMap<String, Arc<crate::operations::ServerRequest>>,
    #[serde(default)]
    pub submissions: BTreeMap<String, crate::session::SubmissionDelivery>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub worktree_merged: Option<bool>,
    pub status: Option<ThreadStatus>,
    pub turns: Option<Vec<Arc<Turn>>>,
    #[serde(default, deserialize_with = "project_membership")]
    pub project_id: ProjectMembership,
    pub path: Option<String>,
    pub preview: Option<String>,
    pub created_at: Option<f64>,
    pub updated_at: Option<f64>,
    pub history_mode: Option<String>,
    pub history_has_more: Option<bool>,
    pub history_limit: Option<u64>,
    pub list_stale: Option<bool>,
    pub agent_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadStatusKind {
    Active,
    Idle,
    NotLoaded,
    SystemError,
    #[serde(other)]
    Other,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadStatus {
    #[serde(rename = "type")]
    pub kind: ThreadStatusKind,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub status: Option<String>,
    pub items: Option<Vec<Arc<Item>>>,
    pub items_view: Option<String>,
    pub items_has_more: Option<bool>,
    pub deferred_item_ids: Option<Vec<String>>,
    pub opening_user_message: Option<Arc<Item>>,
    pub started_at: Option<f64>,
    pub completed_at: Option<f64>,
    pub duration_ms: Option<u64>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub error: Option<Value>,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    pub text: Option<String>,
    pub status: Option<String>,
    pub command: Option<String>,
    pub aggregated_output: Option<String>,
    pub saved_path: Option<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub result: Option<Value>,
    pub client_id: Option<String>,
    #[serde(default, deserialize_with = "present_changes")]
    pub changes: Option<ItemChanges>,
    pub phase: Option<String>,
    pub tool: Option<String>,
    pub server: Option<String>,
    pub query: Option<String>,
    pub path: Option<String>,
    pub cwd: Option<String>,
    pub detail_file: Option<String>,
    pub agent_id: Option<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub content: Option<Value>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub summary: Option<Value>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub arguments: Option<Value>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub review: Option<Value>,
    pub exit_code: Option<i32>,
}
/// A file activity keeps its affected paths and change kinds while diff bodies
/// may be fetched separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemChange {
    pub path: Option<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub kind: Option<Value>,
    pub diff: Option<String>,
}

/// Only supported file-change fields enter the application model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemChanges(pub Vec<ItemChange>);
fn present_changes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ItemChanges>, D::Error> {
    if deserializer.is_human_readable() {
        let value = Value::deserialize(deserializer)?;
        Ok(serde_json::from_value(value).ok())
    } else {
        Option::<ItemChanges>::deserialize(deserializer)
    }
}
impl ItemChanges {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn append_delta(&mut self, delta: &str) {
        if self.0.is_empty() {
            self.0.push(ItemChange {
                path: Some(String::new()),
                kind: Some(Value::String("update".into())),
                diff: None,
            });
        }
        self.0
            .last_mut()
            .unwrap()
            .diff
            .get_or_insert_with(String::new)
            .push_str(delta);
    }
    fn retain_headers(&mut self) {
        for file in &mut self.0 {
            file.diff = None;
        }
    }
}

impl Thread {
    pub fn active_turn_id(&self) -> Option<String> {
        self.turns
            .as_ref()?
            .iter()
            .rev()
            .find(|turn| turn.status.as_deref() == Some("inProgress"))
            .map(|turn| turn.id.clone())
    }
}

impl Thread {
    /// Keep RPC snapshots small; item reads recover every deferred body.
    pub fn defer_item_details(&mut self, max_inline_bytes: usize) {
        for turn in self.turns.iter_mut().flatten() {
            let turn = Arc::make_mut(turn);
            for item in turn.items.iter_mut().flatten() {
                if item.id.is_empty()
                    || fits_inline(
                        item,
                        if matches!(
                            item.kind.as_deref(),
                            Some("userMessage" | "agentMessage" | "imageGeneration")
                        ) {
                            max_inline_bytes
                        } else {
                            // Hundreds of individually small, collapsed tool
                            // bodies otherwise dominate the initial history page.
                            512.min(max_inline_bytes)
                        },
                    )
                {
                    continue;
                }
                let item = Arc::make_mut(item);
                item.retain_header();
                let ids = turn.deferred_item_ids.get_or_insert_default();
                if !ids.contains(&item.id) {
                    ids.push(item.id.clone());
                }
            }
            if let Some(item) = &mut turn.opening_user_message
                && !fits_inline(item, max_inline_bytes)
            {
                Arc::make_mut(item).retain_header();
            }
        }
    }
}
impl Item {
    pub fn retain_header(&mut self) {
        if let Some(text) = &mut self.text {
            truncate_detail(text);
        }
        if let Some(command) = &mut self.command {
            *command = compact_title(command);
        }
        // Command output is only displayed after expansion, which reads the
        // original item. A truncated output is not part of its activity header.
        self.aggregated_output = None;
        if self.kind.as_deref() == Some("imageGeneration")
            || self
                .result
                .as_mut()
                .is_some_and(|result| !retain_scalar(result))
        {
            self.result = None;
        }
        if let Some(changes) = &mut self.changes {
            changes.retain_headers();
        }
        for value in [
            &mut self.content,
            &mut self.summary,
            &mut self.arguments,
            &mut self.review,
        ] {
            if value.as_mut().is_some_and(|value| !retain_scalar(value)) {
                *value = None;
            }
        }
    }
}

// Stop counting when the budget is exceeded; never allocate another large body.
fn fits_inline(value: &impl Serialize, limit: usize) -> bool {
    postcard::serialize_with_flavor::<_, postcard::ser_flavors::Size, usize>(
        value,
        Default::default(),
    )
    .is_ok_and(|size| size <= limit)
}

fn truncate_detail(text: &mut String) {
    let mut end = text.len().min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
// Tool results keep only a short scalar preview until their details are read.
fn retain_scalar(value: &mut Value) -> bool {
    if let Value::String(text) = value {
        truncate_detail(text);
    }
    !value.is_array() && !value.is_object()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread: Thread,
    pub model: Option<String>,
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
    pub position: Option<u64>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRoot {
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
    pub is_default: Option<bool>,
}
pub fn provider_models(models: &[Model], provider: crate::session::ProviderKind) -> Vec<Model> {
    models
        .iter()
        .filter(|model| model_provider(&model.model) == provider)
        .cloned()
        .collect()
}

pub fn model_provider(model: &str) -> crate::session::ProviderKind {
    if model.starts_with("claude:") {
        crate::session::ProviderKind::Claude
    } else {
        crate::session::ProviderKind::Codex
    }
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
    pub bom: bool,
    pub line_ending: String,
    pub size: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
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
    pub id: String,
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
    use serde_json::json;
    #[test]
    fn deferred_read_keeps_conversation_and_activity_headers() {
        let text = "会話".repeat(4096);
        let result = json!({"thread":{"turns":[{"id":"turn","items":[
            {"id":"user","type":"userMessage","content":[{"type":"text","text":text}]},
            {"id":"agent","type":"agentMessage","text":text},
            {"id":"command","type":"commandExecution","command":"日本語".repeat(1000),"status":"completed","aggregatedOutput":text},
            {"id":"files","type":"fileChange","status":"completed","changes":[{"path":"a.txt","kind":{"type":"update"},"diff":text}]},
            {"id":"future","type":"futureTool","tool":"inspect","status":"completed","result":{"content":text}},
            {"id":"small","type":"reasoning","summary":["short"]}
        ]}]}});
        let mut typed: ThreadResponse = serde_json::from_value(result).unwrap();
        typed.thread.defer_item_details(MAX_INLINE_ITEM_BYTES);
        let result = serde_json::to_value(typed).unwrap();
        let turn = &result["thread"]["turns"][0];
        let items = &turn["items"];
        assert_eq!(items[0]["content"][0]["text"], text);
        assert_eq!(items[1]["text"], text);
        assert!(items[2]["command"].as_str().unwrap().starts_with("日本語"));
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(items[3]["changes"][0]["path"], "a.txt");
        assert_eq!(items[3]["changes"][0]["kind"]["type"], "update");
        assert_eq!(items[4]["tool"], "inspect");
        assert_eq!(items[5]["summary"], json!(["short"]));
        assert_eq!(
            turn["deferredItemIds"],
            json!(["command", "files", "future"])
        );
    }

    #[test]
    fn generated_image_output_is_not_truncated_as_an_activity_detail() {
        let image = json!({"id":"image","type":"imageGeneration","status":"completed",
            "result":"A".repeat(8192),"savedPath":format!("/{} image.png", "directory/".repeat(40))});
        let result = json!({"thread":{"turns":[{"id":"turn","items":[image]}]}});
        let mut typed: ThreadResponse = serde_json::from_value(result).unwrap();
        typed.thread.defer_item_details(MAX_INLINE_ITEM_BYTES);
        let result = serde_json::to_value(typed).unwrap();
        for field in ["result", "savedPath"] {
            assert_eq!(
                result["thread"]["turns"][0]["items"][0][field],
                image[field]
            );
        }
        assert!(result["thread"]["turns"][0]["deferredItemIds"].is_null());
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

pub(crate) fn append_text(value: &mut Value, delta: &str) {
    if !value.is_string() {
        let text = match value.take() {
            Value::Array(parts) => parts
                .into_iter()
                .filter_map(|part| match part {
                    Value::String(text) => Some(text),
                    Value::Object(mut object) => object
                        .remove("text")
                        .and_then(|text| text.as_str().map(str::to_owned)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        *value = Value::String(text);
    }
    if let Value::String(text) = value {
        text.push_str(delta);
    }
}

pub fn compact_title(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

pub fn worktree_branch_merged(head: &str, initial: Option<&str>, contained_in_main: bool) -> bool {
    contained_in_main && initial.is_some_and(|initial| initial != head)
}
