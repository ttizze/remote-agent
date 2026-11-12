//! Shared typed models. Unknown provider fields are ignored.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

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
    pub devices: Vec<HostDevice>,
    #[serde(default)]
    pub provider_errors: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostDevice {
    pub id: String,
    pub name: Option<String>,
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
    /// Host list revision; newer notices survive an older in-flight read.
    #[serde(default)]
    pub list_revision: u64,
    pub history_read_state: Option<crate::session::HistoryReadState>,
    pub capabilities: Option<crate::session::Capabilities>,
    #[serde(default)]
    pub requests: BTreeMap<crate::ids::RequestId, Arc<crate::requests::Request>>,
    #[serde(default)]
    pub submissions: BTreeMap<crate::ids::ClientInputId, crate::session::SubmissionDelivery>,
    /// Input waiting in the Host's queue for the running turn, in order.
    #[serde(default)]
    pub queue: crate::operations::InputQueue,
    pub preparation: Option<crate::session::SessionPreparation>,
    pub workspace_setup: Option<crate::workspace::WorkspaceSetup>,
    pub id: Option<crate::session::SessionRef>,
    /// The public identity remains stable across provider handoffs.
    pub active_provider: Option<crate::session::ProviderKind>,
    pub parent_id: Option<crate::session::SessionRef>,
    pub can_accept_direct_input: Option<bool>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub has_changes: Option<bool>,
    pub git_branch: Option<String>,
    #[serde(default)]
    pub status: SessionStatus,
    pub turns: Option<Vec<Arc<Turn>>>,
    /// Work the agent left running after its running turn answered.
    #[serde(default)]
    pub background: Vec<crate::execution::BackgroundTask>,
    #[serde(default, deserialize_with = "project_membership")]
    pub project_id: ProjectMembership,
    pub preview: Option<String>,
    pub updated_at: Option<f64>,
    pub history_has_more: Option<bool>,
    pub history_cursor: Option<String>,
    pub history_limit: Option<u64>,
    pub list_stale: Option<bool>,
    pub agent_id: Option<String>,
    /// Host transcript version of these turns; absent for native reads.
    pub transcript_version: Option<u64>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: crate::ids::TurnId,
    pub provider: Option<crate::session::ProviderKind>,
    pub supports_fork: Option<bool>,
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
    /// Started by input that waited in the Host's queue behind another turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}
impl Turn {
    /// Reused turn IDs describe the same occurrence only when their items overlap.
    /// An observation without items remains compatible with a matching turn ID.
    pub fn matches_occurrence(&self, other: &Self) -> bool {
        if self.id != other.id {
            return false;
        }
        let items = self.items.as_deref().unwrap_or_default();
        let other_items = other.items.as_deref().unwrap_or_default();
        if items.is_empty() || other_items.is_empty() {
            return true;
        }
        self.shares_items(other)
    }

    /// Known item identities distinguish occurrences even beside an empty turn.
    pub fn shares_items(&self, other: &Self) -> bool {
        let ids: HashSet<_> = self.items.iter().flatten().map(|item| &item.id).collect();
        other
            .items
            .iter()
            .flatten()
            .any(|item| ids.contains(&item.id))
    }
}
pub use crate::{execution::*, items::*};

impl Thread {
    /// The turn a stop request targets. The Host routes a preparing task's
    /// submission ID to its preparation, so the same stop control covers both.
    pub fn active_turn_id(&self) -> Option<crate::ids::TurnId> {
        if let Some(preparation) = self.preparation.as_ref().filter(|preparation| {
            preparation.status == crate::session::PreparationStatus::Preparing
        }) {
            return Some(
                preparation
                    .submission
                    .client_user_message_id
                    .to_string()
                    .into(),
            );
        }
        let turns = self.turns.as_ref()?;
        turns
            .iter()
            .rev()
            .find(|turn| turn.status == TurnStatus::Running)
            // As T3 Code, Stop also ends the background work an answered
            // turn left running.
            .or_else(|| turns.last().filter(|_| !self.background.is_empty()))
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

/// Collapsed history rows need only activity headers, so each activity body
/// above 512 bytes loads on expansion.
pub fn defer_item_details(turns: &mut [Arc<Turn>], max_inline_bytes: usize) {
    for turn in turns {
        for index in 0..turn.items.as_ref().map_or(0, Vec::len) {
            let item = &turn.items.as_ref().expect("turn items are present")[index];
            let limit = if is_conversation(item) {
                max_inline_bytes
            } else {
                512.min(max_inline_bytes)
            };
            if !item.id.is_empty() && inline_size(item, limit).is_none() {
                let items = Arc::make_mut(turn)
                    .items
                    .as_mut()
                    .expect("turn items are present");
                Arc::make_mut(&mut items[index]).defer();
            }
        }
    }
}

/// A turn whose activity a client opened. Activity bodies stay inline,
/// smallest first, until they total `activity_bytes`, so most rows expand
/// without another Host request; the rest load on expansion.
pub fn defer_turn_items(items: &mut [Arc<Item>], max_inline_bytes: usize, activity_bytes: usize) {
    let mut activity = Vec::new();
    for (index, item) in items.iter_mut().enumerate() {
        if item.id.is_empty() || item.is_deferred() {
            continue;
        }
        let conversation = is_conversation(item);
        let limit = if conversation {
            max_inline_bytes
        } else {
            activity_bytes.min(max_inline_bytes)
        };
        match inline_size(item, limit) {
            Some(size) if !conversation => activity.push((size, index)),
            Some(_) => {}
            None => Arc::make_mut(item).defer(),
        }
    }
    activity.sort_unstable();
    let mut remaining = activity_bytes;
    for (size, index) in activity {
        match remaining.checked_sub(size) {
            Some(left) => remaining = left,
            None => Arc::make_mut(&mut items[index]).defer(),
        }
    }
}

fn is_conversation(item: &Item) -> bool {
    matches!(
        item.body(),
        ItemBody::UserMessage { .. }
            | ItemBody::AssistantText { .. }
            | ItemBody::ImageGeneration { .. }
            | ItemBody::ImageView { .. }
            | ItemBody::Notification { .. }
    )
}

/// The encoded size of `value`, when it is at most `limit` bytes.
fn inline_size(value: &impl Serialize, limit: usize) -> Option<usize> {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("inline item exceeds its byte budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut budget = Budget(limit);
    crate::protocol::encode_into(&mut budget, value).ok()?;
    Some(limit - budget.0)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread: Thread,
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPage {
    pub limit: u32,
    pub has_more: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadList {
    /// Revision captured before the Host starts reading native metadata.
    #[serde(default)]
    pub revision: u64,
    pub limit: u32,
    pub data: Vec<Thread>,
    pub projects: Vec<Project>,
    pub has_more: bool,
    pub has_more_projects: bool,
    /// Project pages included in this response, with their own lookahead result.
    pub project_pages: BTreeMap<String, ListPage>,
    #[serde(default)]
    pub provider_errors: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObservation {
    pub id: crate::session::SessionRef,
    pub parent_id: crate::session::SessionRef,
    pub name: Option<String>,
    pub status: crate::execution::SessionStatus,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(default)]
    pub list_revision: u64,
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    pub icon: Option<ProjectIcon>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Base64 PNG thumbnails; a single source image is shared across appearances.
pub struct ProjectIcon {
    pub light_png: String,
    pub dark_png: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectRoot {
    pub path: String,
}
/// Native model identity scoped by provider; neither field is encoded in the other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    pub credential_route: Option<crate::dsh::KeyProvider>,
    /// None means capabilities have not been resolved, not that they are unsupported.
    pub settings: Option<ModelSettings>,
    #[serde(default)]
    pub settings_discovery: bool,
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceTier {
    pub id: String,
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub fast: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffort {
    pub reasoning_effort: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ListPart {
    Projects,
    Chats,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListQuery {
    pub part: Option<ListPart>,
    pub limit: u32,
    pub project_limit: u32,
    pub search_term: String,
    /// None requests the first five recent project pages; an explicit map reads
    /// only those projects, leaving closed pages and root pagination untouched.
    pub project_limits: Option<std::collections::HashMap<String, u32>>,
}
impl Default for ListQuery {
    fn default() -> Self {
        Self {
            limit: 5,
            project_limit: 5,
            search_term: String::new(),
            project_limits: None,
            part: None,
        }
    }
}
impl ListQuery {
    pub fn for_connection(mut self) -> Self {
        if self.limit == 0 {
            self.limit = 5;
        }
        if self.project_limit == 0 {
            self.project_limit = 5;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Only a prefix is present; it must never be edited or written back.
    pub truncated: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    pub init_submodules: bool,
    pub setup_script: Option<crate::workspace::SetupScript>,
    pub project_setup_scripts: std::collections::HashMap<String, crate::workspace::SetupScript>,
    /// Remove idle checkouts whose HEAD the default branch already contains.
    pub delete_unchanged: bool,
    /// Project IDs that override `delete_unchanged`; other projects inherit it.
    pub project_delete_unchanged: std::collections::HashMap<String, bool>,
}
impl WorktreeSettings {
    pub fn deletes_unchanged(&self, project: Option<&str>) -> bool {
        project
            .and_then(|id| self.project_delete_unchanged.get(id))
            .copied()
            .unwrap_or(self.delete_unchanged)
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReview {
    pub controls: WorkspaceControls,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<crate::workspace::WorkspaceOptions>,
    pub branch: String,
    /// The reference whose merge base produced the diff, for example `origin/main`.
    pub base_ref: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<ChangedFile>,
    pub diff: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceControls {
    pub branches: Vec<String>,
    pub references: Vec<String>,
    pub remotes: Vec<String>,
    pub can_push: bool,
    pub scripts: Vec<ProjectScript>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectScript {
    pub name: String,
    pub command: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    #[test]
    fn inline_budget_matches_the_actual_named_frame_at_the_boundary() {
        let item: Item = serde_json::from_value(json!({
                "id":"item", "body":{"inline":{"body":{"assistantText":{"text":"名前と本文","phase":"final"}}}}
            })).unwrap();
        let bytes = crate::protocol::encode(&item).unwrap();
        assert_eq!(inline_size(&item, bytes.len()), Some(bytes.len()));
        assert_eq!(inline_size(&item, bytes.len() + 7), Some(bytes.len()));
        assert_eq!(inline_size(&item, bytes.len() - 1), None);
        assert_eq!(inline_size(&item, 0), None);
    }

    fn command(id: usize, output: usize) -> Arc<Item> {
        Arc::new(Item::new(
            format!("command-{id}").into(),
            ItemStatus::Completed,
            ItemBody::CommandExecution {
                command: "cargo test".into(),
                cwd: None,
                output: "x".repeat(output),
                exit_code: Some(0),
            },
        ))
    }

    #[test]
    fn deferred_history_shares_unchanged_turns_and_items() {
        let mut anonymous = (*command(2, 1024)).clone();
        anonymous.id = "".into();
        let original = vec![
            Arc::new(Turn::default()),
            Arc::new(Turn {
                items: Some(Vec::new()),
                ..Default::default()
            }),
            Arc::new(Turn {
                items: Some(vec![command(0, 16)]),
                ..Default::default()
            }),
            Arc::new(Turn {
                items: Some(vec![command(0, 16), command(1, 1024), Arc::new(anonymous)]),
                ..Default::default()
            }),
        ];
        let mut sent = original.clone();
        defer_item_details(&mut sent, MAX_INLINE_ITEM_BYTES);
        for (source, sent) in original.iter().zip(&sent).take(3) {
            assert!(Arc::ptr_eq(source, sent));
        }
        assert!(!Arc::ptr_eq(&original[3], &sent[3]));
        let source = original[3].items.as_ref().unwrap();
        let sent = sent[3].items.as_ref().unwrap();
        assert!(Arc::ptr_eq(&source[0], &sent[0]));
        assert!(sent[1].is_deferred());
        assert!(Arc::ptr_eq(&source[2], &sent[2]));
        assert!(
            matches!(source[1].body(), ItemBody::CommandExecution { output, .. } if output.len() == 1024)
        );
        assert!(!source[1].is_deferred());
    }

    #[test]
    fn deferred_history_keeps_the_inclusive_wire_budget() {
        let conversation = Arc::new(Item::new(
            "answer".into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "回答".into(),
                phase: Default::default(),
            },
        ));
        for item in [command(0, 16), conversation] {
            let size = crate::protocol::encode(&item).unwrap().len();
            assert!(size < 512);
            let original = Arc::new(Turn {
                items: Some(vec![item]),
                ..Default::default()
            });
            for budget in [size, size - 1] {
                let mut sent = vec![original.clone()];
                defer_item_details(&mut sent, budget);
                assert_eq!(
                    sent[0].items.as_ref().unwrap()[0].is_deferred(),
                    budget < size
                );
                assert_eq!(Arc::ptr_eq(&original, &sent[0]), budget == size);
            }
        }
    }

    #[test]
    fn deferred_history_caps_activity_at_512_wire_bytes() {
        for size in [512, 513] {
            let item = (0..512)
                .map(|output| command(0, output))
                .find(|item| crate::protocol::encode(item).unwrap().len() == size)
                .expect("an ASCII output can fill the wire budget exactly");
            let original = Arc::new(Turn {
                items: Some(vec![item]),
                ..Default::default()
            });
            let mut sent = vec![original.clone()];
            defer_item_details(&mut sent, MAX_INLINE_ITEM_BYTES);
            assert_eq!(sent[0].items.as_ref().unwrap()[0].is_deferred(), size > 512);
            assert_eq!(Arc::ptr_eq(&original, &sent[0]), size == 512);
        }
    }

    proptest! {
        #[test]
        fn deferred_history_obeys_each_activity_wire_budget(
            outputs in proptest::collection::vec(0usize..2048, 0..24),
            budget in 0usize..4096,
        ) {
            let items: Vec<_> = outputs.iter().enumerate().map(|(id, output)| command(id, *output)).collect();
            let source = Arc::new(Turn { items: Some(items), ..Default::default() });
            let mut sent = vec![source.clone()];
            defer_item_details(&mut sent, budget);
            for (source, sent) in source.items.as_ref().unwrap().iter().zip(sent[0].items.as_ref().unwrap()) {
                let deferred = crate::protocol::encode(source).unwrap().len() > budget.min(512);
                prop_assert_eq!(sent.is_deferred(), deferred);
                prop_assert_eq!(&sent.id, &source.id);
                prop_assert_eq!(sent.status, source.status);
                if !deferred {
                    prop_assert!(Arc::ptr_eq(source, sent));
                }
            }
        }
    }

    proptest! {
        #[test]
        fn opened_turn_inlines_the_most_activity_within_its_budget(
            outputs in proptest::collection::vec(0usize..4096, 0..24),
            budget in 0usize..32 * 1024,
        ) {
            let original = outputs.iter().enumerate().map(|(id, output)| command(id, *output)).collect::<Vec<_>>();
            let size = |item: &Arc<Item>| crate::protocol::encode(item).unwrap().len();
            let mut items = original.clone();
            defer_turn_items(&mut items, MAX_INLINE_ITEM_BYTES, budget);
            let (inline, deferred): (Vec<_>, Vec<_>) =
                original.iter().zip(&items).partition(|(_, sent)| !sent.is_deferred());
            for (source, sent) in &inline {
                prop_assert_eq!(source, sent);
            }
            let used = inline.iter().map(|(source, _)| size(source)).sum::<usize>();
            prop_assert!(used <= budget);
            // Keeping a smaller body inline instead would fit more rows, and
            // any deferred body that still fits should have been sent.
            for (source, _) in &deferred {
                prop_assert!(used + size(source) > budget);
                prop_assert!(inline.iter().all(|(kept, _)| size(kept) <= size(source)));
            }
        }
    }

    #[test]
    fn opened_turn_keeps_conversation_inline_beside_its_activity_budget() {
        let answer = Arc::new(Item::new(
            "answer".into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "回答".repeat(4096),
                phase: Default::default(),
            },
        ));
        let mut items = vec![answer.clone(), command(0, 64), command(1, 64)];
        let one = crate::protocol::encode(&items[1]).unwrap().len();
        defer_turn_items(&mut items, MAX_INLINE_ITEM_BYTES, one);
        assert_eq!(items[0], answer);
        assert_eq!(
            items
                .iter()
                .map(|item| item.is_deferred())
                .collect::<Vec<_>>(),
            [false, false, true]
        );
        defer_turn_items(&mut items, 0, 0);
        assert!(items.iter().all(|item| item.is_deferred()));
    }

    proptest! {
        #[test]
        fn project_cleanup_overrides_only_its_own_project(
            global in any::<bool>(),
            overridden in any::<bool>(),
            other in "[a-z]{1,4}",
        ) {
            let settings = WorktreeSettings {
                delete_unchanged: global,
                project_delete_unchanged: [("project".to_owned(), overridden)].into(),
                ..Default::default()
            };
            prop_assert_eq!(settings.deletes_unchanged(Some("project")), overridden);
            prop_assert_eq!(settings.deletes_unchanged(None), global);
            if other != "project" {
                prop_assert_eq!(settings.deletes_unchanged(Some(&other)), global);
            }
        }
    }

    #[test]
    fn turn_occurrences_require_matching_ids_and_overlapping_items_when_known() {
        let turn = |id: &str, items: &[&str]| Turn {
            id: id.into(),
            items: Some(
                items
                    .iter()
                    .map(|id| {
                        Arc::new(Item::new(
                            (*id).into(),
                            ItemStatus::Completed,
                            ItemBody::Sleep {},
                        ))
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        for (left, right, matches) in [
            (turn("turn", &["a"]), turn("turn", &["b", "a"]), true),
            (turn("turn", &["a"]), turn("turn", &["b"]), false),
            (turn("turn", &["a"]), turn("other", &["a"]), false),
            (turn("turn", &[]), turn("turn", &["a"]), true),
            (turn("turn", &[]), turn("other", &[]), false),
        ] {
            assert_eq!(left.matches_occurrence(&right), matches);
            assert_eq!(right.matches_occurrence(&left), matches);
        }
    }

    #[test]
    fn stop_targets_a_preparing_submission_until_the_host_sends_it() {
        use crate::session::{PreparationStatus, ProviderKind, SessionPreparation, SessionRef};
        let id = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        let mut thread = Thread {
            preparation: Some(SessionPreparation {
                submission: crate::operations::Submission {
                    thread_id: id,
                    client_user_message_id: "input".into(),
                    input: Vec::new(),
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                status: PreparationStatus::Preparing,
                stage: crate::session::PreparationStage::Workspace,
            }),
            ..Default::default()
        };
        assert_eq!(thread.active_turn_id(), Some("input".into()));
        thread.turns = Some(vec![Arc::new(Turn {
            id: "native".into(),
            status: TurnStatus::Running,
            ..Default::default()
        })]);
        for status in [
            PreparationStatus::Sending,
            PreparationStatus::Interrupted,
            PreparationStatus::Unknown,
        ] {
            thread.preparation.as_mut().unwrap().status = status;
            assert_eq!(thread.active_turn_id(), Some("native".into()));
        }
        thread.turns = None;
        assert_eq!(thread.active_turn_id(), None);
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

    #[test]
    fn viewed_image_paths_stay_inline_without_an_activity_budget() {
        let image = Arc::new(Item::new(
            "screenshot".into(),
            ItemStatus::Completed,
            ItemBody::ImageView {
                path: format!("/{} screenshot.png", "directory/".repeat(80)),
            },
        ));
        let mut turns = vec![Arc::new(Turn {
            items: Some(vec![image.clone()]),
            ..Default::default()
        })];
        defer_item_details(&mut turns, MAX_INLINE_ITEM_BYTES);
        let sent = turns[0].items.as_ref().unwrap();
        assert!(!sent[0].is_deferred());
        assert_eq!(sent[0], image);

        let mut opened = vec![image.clone()];
        defer_turn_items(&mut opened, MAX_INLINE_ITEM_BYTES, 0);
        assert!(!opened[0].is_deferred());
        assert_eq!(opened[0], image);
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
    observed.unwrap_or(matches!(
        status,
        SessionStatus::Preparing | SessionStatus::Running
    ))
}
pub fn task_title<'a>(name: Option<&'a str>, preview: Option<&'a str>) -> Option<&'a str> {
    name.filter(|name| !name.is_empty())
        .or_else(|| preview.filter(|preview| !preview.is_empty()))
}

#[cfg(test)]
mod title_tests {
    use super::*;
    #[test]
    fn task_titles_return_only_names_or_previews_without_display_copy() {
        for (name, preview, expected) in [
            (None, None, None),
            (Some(""), Some(""), None),
            (Some(""), Some("preview"), Some("preview")),
            (Some("設定{0}"), Some("preview"), Some("設定{0}")),
            (Some(" "), None, Some(" ")),
        ] {
            assert_eq!(task_title(name, preview), expected);
        }
    }
}
