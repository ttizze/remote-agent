//! Wire data. Missing fields stay missing; null cursors remain explicit nulls.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Invitation {
    pub endpoint: String,
    pub invitation: uuid::Uuid,
    pub expires_at: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub node_id: String,
    pub name: String,
    pub devices: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Thread {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ThreadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<Arc<Turn>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub history_cursor: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub project_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_mode: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadStatus {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Arc<Item>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_has_more: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub items_next_cursor: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_item_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_user_message: Option<Arc<Item>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub started_at: Option<Option<serde_json::Number>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub completed_at: Option<Option<serde_json::Number>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub duration_ms: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregated_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_changes"
    )]
    pub changes: Option<ItemChanges>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
/// A file activity keeps its affected paths and change kinds while diff bodies
/// may be fetched separately. Unknown upstream metadata survives full reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Known file bodies are typed; unrecognized wire shapes remain inspectable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemChanges {
    Files(Vec<ItemChange>),
    Unknown(Value),
}
fn present_changes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ItemChanges>, D::Error> {
    ItemChanges::deserialize(deserializer).map(Some)
}
impl ItemChanges {
    pub fn len(&self) -> usize {
        match self {
            Self::Files(files) => files.len(),
            Self::Unknown(value) => value.as_array().map_or(0, Vec::len),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn accepts_delta(&self) -> bool {
        match self {
            Self::Files(_) => true,
            Self::Unknown(value) => value
                .as_array()
                .is_some_and(|entries| entries.last().is_none_or(Value::is_object)),
        }
    }
    pub(crate) fn append_delta(&mut self, delta: &str) {
        match self {
            Self::Files(files) => {
                if files.is_empty() {
                    files.push(ItemChange {
                        path: Some(String::new()),
                        kind: Some(Value::String("update".into())),
                        diff: None,
                        extra: Map::new(),
                    });
                }
                files
                    .last_mut()
                    .unwrap()
                    .diff
                    .get_or_insert_with(String::new)
                    .push_str(delta);
            }
            Self::Unknown(value) => {
                let entries = value.as_array_mut().expect("validated file delta target");
                let entry = entries
                    .last_mut()
                    .and_then(Value::as_object_mut)
                    .expect("an empty array decodes as typed files");
                append_text(entry.entry("diff").or_insert(Value::Null), delta);
            }
        }
    }
    fn retain_headers(&mut self) {
        match self {
            Self::Files(files) => {
                for file in files {
                    file.diff = None;
                }
            }
            Self::Unknown(value) => {
                if let Some(entries) = value.as_array_mut() {
                    for entry in entries {
                        if let Some(fields) = entry.as_object_mut() {
                            fields.remove("diff");
                        }
                    }
                }
            }
        }
    }
}

impl Thread {
    /// Keep visible messages and generated images complete; mark large activity
    /// bodies for explicit item reads without copying their serialized output.
    pub fn defer_item_details(&mut self) {
        for turn in self.turns.iter_mut().flatten() {
            let turn = Arc::make_mut(turn);
            let mut deferred = Vec::new();
            for item in turn.items.iter_mut().flatten() {
                if item.id.is_empty()
                    || matches!(
                        item.kind.as_deref(),
                        Some("userMessage" | "agentMessage" | "imageGeneration")
                    )
                    || fits_inline(item)
                {
                    continue;
                }
                let item = Arc::make_mut(item);
                deferred.push(item.id.clone());
                item.retain_header();
            }
            if !deferred.is_empty() {
                turn.deferred_item_ids = Some(deferred);
            }
        }
    }
}
impl Item {
    fn retain_header(&mut self) {
        for text in [
            &mut self.text,
            &mut self.status,
            &mut self.command,
            &mut self.aggregated_output,
            &mut self.saved_path,
            &mut self.client_id,
        ]
        .into_iter()
        .flatten()
        {
            truncate_detail(text);
        }
        if let Some(result) = &mut self.result
            && !retain_scalar(result)
        {
            self.result = None;
        }
        if let Some(changes) = &mut self.changes {
            changes.retain_headers();
        }
        self.extra.retain(|_, value| retain_scalar(value));
    }
}

// Stop counting when the budget is exceeded; never allocate another large body.
fn fits_inline(value: &impl Serialize) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("inline budget exceeded"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(4096), value).is_ok()
}
fn truncate_detail(text: &mut String) {
    let mut end = text.len().min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
// Extension fields have no known schema. Preserve only their scalar headers.
fn retain_scalar(value: &mut Value) -> bool {
    if let Value::String(text) = value {
        truncate_detail(text);
    }
    !value.is_array() && !value.is_object()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread: Thread,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadList {
    pub data: Vec<Thread>,
    pub projects: Vec<Project>,
    pub more_project_ids: Vec<String>,
    pub has_more_chats: bool,
    pub has_more_projects: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectRoot {
    pub path: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tiers: Option<Vec<ServiceTier>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ServiceTier {
    pub id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReasoningEffort {
    pub reasoning_effort: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileContent {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub bom: bool,
    pub line_ending: String,
    pub size: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct WorkspaceReview {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<ChangedFile>,
    pub diff: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nullable_update_fields_distinguish_absence_null_and_value() {
        for field in ["historyCursor", "projectId"] {
            for value in [serde_json::json!(null), serde_json::json!("value")] {
                let source = serde_json::json!({field:value});
                let thread: Thread = serde_json::from_value(source.clone()).unwrap();
                let encoded = serde_json::to_value(thread).unwrap();
                assert_eq!(encoded[field], value);
                assert!(encoded.get(field).is_some());
            }
            assert!(
                serde_json::to_value(Thread::default())
                    .unwrap()
                    .get(field)
                    .is_none()
            );
        }
        for field in ["itemsNextCursor", "startedAt", "completedAt", "durationMs"] {
            let source = serde_json::json!({"id":"turn",field:null});
            let turn: Turn = serde_json::from_value(source).unwrap();
            assert!(
                serde_json::to_value(turn)
                    .unwrap()
                    .get(field)
                    .is_some_and(serde_json::Value::is_null)
            );
            let absent: Turn = serde_json::from_value(serde_json::json!({"id":"turn"})).unwrap();
            assert!(serde_json::to_value(absent).unwrap().get(field).is_none());
        }
    }

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
        typed.thread.defer_item_details();
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
        typed.thread.defer_item_details();
        let result = serde_json::to_value(typed).unwrap();
        assert_eq!(result["thread"]["turns"][0]["items"][0], image);
        assert!(result["thread"]["turns"][0]["deferredItemIds"].is_null());
    }

    #[test]
    fn history_page_preserves_the_opaque_cursor_and_chronological_order() {
        let mut result: ThreadResponse = serde_json::from_value(json!({"thread":{}})).unwrap();
        result.thread.apply_history_page(
            serde_json::from_value(
                json!({"data":[{"id":"new"},{"id":"old"}],"nextCursor":"opaque:token"}),
            )
            .unwrap(),
        );
        let result = serde_json::to_value(result).unwrap();
        assert_eq!(result["thread"]["turns"][0]["id"], "old");
        assert_eq!(result["thread"]["historyCursor"], "opaque:token");
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Empty {}

/// Host-only history policy is consumed before forwarding the remaining params.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThreadParams {
    #[serde(skip_serializing)]
    pub paginate_history: bool,
    #[serde(skip_serializing)]
    pub defer_item_details: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_turns: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams<'a> {
    pub limit: usize,
    pub sort_key: &'a str,
    pub sort_direction: &'a str,
    pub use_state_db_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
    pub limit: usize,
    pub sort_direction: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_view: Option<&'a str>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub item: Arc<Item>,
    pub turn_id: Option<String>,
}
impl HistoryPage<HistoryItem> {
    pub fn into_items(self) -> Result<(Vec<Arc<Item>>, Option<String>), &'static str> {
        if self.data.len() > 100 {
            return Err("item page exceeds requested size");
        }
        let items = self
            .data
            .into_iter()
            .map(|entry| {
                if entry.item.id.is_empty() {
                    Err("history item ID is missing")
                } else {
                    Ok(entry.item)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok((items, self.next_cursor))
    }
}
impl Thread {
    pub fn apply_history_page(&mut self, mut page: HistoryPage<Arc<Turn>>) {
        page.data.reverse();
        self.turns = Some(page.data);
        self.history_cursor = Some(page.next_cursor.filter(|cursor| !cursor.is_empty()));
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransferGrant {
    pub token: String,
    pub size: u64,
    pub sha256: String,
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

// Preserve omitted fields separately from explicit null in partial updates.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }
}
