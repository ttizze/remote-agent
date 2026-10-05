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
    pub provider: Option<crate::providers::ProviderRef>,
    pub history_read_state: Option<crate::session::HistoryReadState>,
    pub capabilities: Option<crate::session::Capabilities>,
    #[serde(default)]
    pub requests: BTreeMap<crate::ids::RequestId, Arc<crate::requests::Request>>,
    #[serde(default)]
    pub submissions: BTreeMap<crate::ids::ClientInputId, crate::session::SubmissionDelivery>,
    #[serde(default)]
    pub queued_inputs: Vec<crate::queue::QueueEntry>,
    #[serde(default)]
    pub queue_held: bool,
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
    pub importing: bool,
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
/// Native model identity scoped by configured instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    #[serde(rename = "instanceId")]
    pub instance_id: crate::providers::ProviderInstanceId,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: ModelRef,
    pub display_name: String,
    #[serde(with = "crate::protocol::json")]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub is_custom: bool,
    pub is_default: Option<bool>,
}
pub fn provider_models(
    models: &[Model],
    instance_id: &crate::providers::ProviderInstanceId,
) -> Vec<Model> {
    models
        .iter()
        .filter(|model| &model.model.instance_id == instance_id)
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
    #[serde(
        default,
        deserialize_with = "present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

/// Canonical selections retain their authored order and distinguish false from absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelOptionValue {
    String(#[serde(deserialize_with = "option_string")] String),
    Boolean(bool),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOptionSelection {
    #[serde(deserialize_with = "option_string")]
    pub id: String,
    #[serde(with = "crate::protocol::json")]
    pub value: ModelOptionValue,
}
fn option_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    let value = value.trim();
    if value.is_empty() {
        Err(serde::de::Error::custom("model option must not be empty"))
    } else {
        Ok(value.to_owned())
    }
}

pub fn model_option_value<'a>(
    options: &'a [ModelOptionSelection],
    id: &str,
) -> Option<&'a ModelOptionValue> {
    options
        .iter()
        .find(|option| option.id == id)
        .map(|option| &option.value)
}
pub fn model_option_string<'a>(options: &'a [ModelOptionSelection], id: &str) -> Option<&'a str> {
    match model_option_value(options, id) {
        Some(ModelOptionValue::String(value)) => Some(value),
        _ => None,
    }
}
pub fn model_option_boolean(options: &[ModelOptionSelection], id: &str) -> Option<bool> {
    match model_option_value(options, id) {
        Some(ModelOptionValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

/// Editing preserves the first position and removes duplicates for this ID.
pub fn with_model_option(
    options: &[ModelOptionSelection],
    id: &str,
    value: Option<ModelOptionValue>,
) -> Vec<ModelOptionSelection> {
    let id = id.trim();
    if id.is_empty() {
        return options.to_vec();
    }
    let value = match value {
        Some(ModelOptionValue::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then(|| ModelOptionValue::String(value.to_owned()))
        }
        value => value,
    };
    let mut replacement = value.map(|value| ModelOptionSelection {
        id: id.into(),
        value,
    });
    let mut next = Vec::with_capacity(options.len());
    for option in options {
        if option.id == id {
            if let Some(replacement) = replacement.take() {
                next.push(replacement);
            }
        } else {
            next.push(option.clone());
        }
    }
    if let Some(replacement) = replacement {
        next.push(replacement);
    }
    next
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ModelOptionKind {
    Select {
        options: Vec<ModelOptionChoice>,
        #[serde(
            default,
            rename = "currentValue",
            deserialize_with = "present_option",
            skip_serializing_if = "Option::is_none"
        )]
        current_value: Option<String>,
        #[serde(default, rename = "promptInjectedValues")]
        prompt_injected_values: Vec<String>,
    },
    Boolean {
        #[serde(
            default,
            rename = "currentValue",
            deserialize_with = "present_option",
            skip_serializing_if = "Option::is_none"
        )]
        current_value: Option<bool>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(
        default,
        deserialize_with = "present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,
    #[serde(flatten)]
    pub kind: ModelOptionKind,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default)]
    pub option_descriptors: Vec<ModelOptionDescriptor>,
}

// Optional capability fields can be absent; an authored null is invalid in T3.
fn present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl ModelCapabilities {
    pub fn select(&self, ids: &[&str]) -> Option<&ModelOptionDescriptor> {
        self.option_descriptors.iter().find(|descriptor| {
            ids.contains(&descriptor.id.as_str())
                && matches!(descriptor.kind, ModelOptionKind::Select { .. })
        })
    }

    /// Store supported raw choices, including prompt-injected choices, until the driver compiles them.
    pub fn normalize_options(
        &self,
        selections: &[ModelOptionSelection],
        include_defaults: bool,
    ) -> Vec<ModelOptionSelection> {
        self.option_descriptors
            .iter()
            .filter_map(|descriptor| {
                let raw = model_option_value(selections, &descriptor.id);
                if raw.is_none() && (!include_defaults || descriptor.id == "variant") {
                    return None;
                }
                let value = match &descriptor.kind {
                    ModelOptionKind::Select { options, .. } => {
                        let saved = match raw {
                            Some(ModelOptionValue::String(value)) => {
                                Some(value.trim()).filter(|value| !value.is_empty())
                            }
                            _ => None,
                        };
                        saved
                            .filter(|value| {
                                options.is_empty()
                                    || options.iter().any(|choice| choice.id == *value)
                            })
                            .or_else(|| descriptor.selected(None))
                            .map(|value| ModelOptionValue::String(value.into()))
                    }
                    ModelOptionKind::Boolean { current_value } => match raw {
                        Some(ModelOptionValue::Boolean(value)) => Some(*value),
                        _ => *current_value,
                    }
                    .map(ModelOptionValue::Boolean),
                }?;
                Some(ModelOptionSelection {
                    id: descriptor.id.clone(),
                    value,
                })
            })
            .collect()
    }
}

impl ModelOptionDescriptor {
    pub fn value(&self, selections: &[ModelOptionSelection]) -> Option<ModelOptionValue> {
        match &self.kind {
            ModelOptionKind::Select { .. } => self
                .selected(model_option_string(selections, &self.id))
                .map(|value| ModelOptionValue::String(value.into())),
            ModelOptionKind::Boolean { current_value } => {
                model_option_boolean(selections, &self.id)
                    .or(*current_value)
                    .map(ModelOptionValue::Boolean)
            }
        }
    }
    pub fn choices(&self) -> &[ModelOptionChoice] {
        match &self.kind {
            ModelOptionKind::Select { options, .. } => options,
            ModelOptionKind::Boolean { .. } => &[],
        }
    }

    pub fn selected<'a>(&'a self, value: Option<&'a str>) -> Option<&'a str> {
        let ModelOptionKind::Select {
            options,
            current_value,
            prompt_injected_values,
        } = &self.kind
        else {
            return None;
        };
        let default = || {
            options
                .iter()
                .find(|choice| choice.is_default)
                .map(|choice| choice.id.as_str())
        };
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            if options.is_empty() {
                return Some(value);
            }
            if options.iter().any(|choice| choice.id == value) {
                return if prompt_injected_values.iter().any(|prompt| prompt == value) {
                    default()
                } else {
                    Some(value)
                };
            }
        }
        current_value.as_deref().or_else(default)
    }
}

/// Resolved custom entries; malformed rows do not hide valid neighboring models.
// Custom-model decoding follows T3 Tools Inc.'s MIT implementation; license
// and copyright notice: third-party/T3-Code-LICENSE.
pub struct CustomModel {
    pub slug: String,
    pub name: String,
    pub capabilities: Option<ModelCapabilities>,
}

pub fn read_custom_models(value: Option<&Value>) -> Vec<CustomModel> {
    let Some(entries) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    entries
        .iter()
        .filter_map(|entry| {
            let slug = entry
                .as_str()
                .or_else(|| entry.get("slug").and_then(Value::as_str))?
                .trim();
            if slug.is_empty() || !seen.insert(slug) {
                return None;
            }
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(slug);
            let capabilities = entry
                .get("capabilities")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .and_then(normalize_custom_capabilities);
            Some(CustomModel {
                slug: slug.into(),
                name: name.into(),
                capabilities,
            })
        })
        .collect()
}

fn normalize_custom_capabilities(mut capabilities: ModelCapabilities) -> Option<ModelCapabilities> {
    fn trim(value: &mut String) -> Option<()> {
        *value = value.trim().to_owned();
        (!value.is_empty()).then_some(())
    }
    fn optional(value: &mut Option<String>) -> Option<()> {
        if let Some(value) = value {
            trim(value)?;
        }
        Some(())
    }
    for descriptor in &mut capabilities.option_descriptors {
        trim(&mut descriptor.id)?;
        trim(&mut descriptor.label)?;
        optional(&mut descriptor.description)?;
        if let ModelOptionKind::Select {
            options,
            current_value,
            prompt_injected_values,
        } = &mut descriptor.kind
        {
            optional(current_value)?;
            for choice in options {
                trim(&mut choice.id)?;
                trim(&mut choice.label)?;
                optional(&mut choice.description)?;
            }
            for value in prompt_injected_values {
                trim(value)?;
            }
        }
    }
    Some(capabilities)
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

    proptest::proptest! {
        #[test]
        fn canonical_options_preserve_types_order_and_false_across_json_and_binary(
            id in "[a-z]{1,16}", text in "[a-z]{1,32}", flag in proptest::bool::ANY,
        ) {
            let options = vec![
                ModelOptionSelection { id: id.clone(), value: ModelOptionValue::Boolean(flag) },
                ModelOptionSelection { id: id.clone(), value: ModelOptionValue::String(text.clone()) },
            ];
            let json = serde_json::to_value(&options).unwrap();
            proptest::prop_assert_eq!(&json, &serde_json::json!([
                {"id":id,"value":flag}, {"id":id,"value":text}
            ]));
            let from_json: Vec<ModelOptionSelection> = serde_json::from_value(json).unwrap();
            proptest::prop_assert_eq!(&from_json, &options);
            let binary = crate::protocol::encode(&options).unwrap();
            let from_binary: Vec<ModelOptionSelection> = crate::protocol::decode(&binary).unwrap();
            proptest::prop_assert_eq!(&from_binary, &options);
            proptest::prop_assert_eq!(model_option_boolean(&options, &id), Some(flag));
            proptest::prop_assert_eq!(model_option_string(&options, &id), None);
            proptest::prop_assert_eq!(model_option_boolean(&options[1..], &id), None);
            proptest::prop_assert_eq!(model_option_string(&options[1..], &id), Some(text.as_str()));
            let changed = with_model_option(&options, &id, Some(ModelOptionValue::Boolean(!flag)));
            proptest::prop_assert_eq!(changed, vec![ModelOptionSelection { id: id.clone(), value: ModelOptionValue::Boolean(!flag) }]);
            proptest::prop_assert_eq!(options.len(), 2);
            proptest::prop_assert!(with_model_option(&options, &id, None).is_empty());
            let other = ModelOptionSelection {id: format!("{id}_other"), value: ModelOptionValue::Boolean(false)};
            let authored = vec![options[0].clone(),other.clone(),options[1].clone()];
            let edited = with_model_option(&authored, &id, Some(ModelOptionValue::String(format!(" {text} "))));
            proptest::prop_assert_eq!(edited, vec![options[1].clone(),other.clone()]);
            proptest::prop_assert_eq!(with_model_option(std::slice::from_ref(&other), &id, Some(ModelOptionValue::String(text.clone()))),
                vec![other.clone(),options[1].clone()]);
            proptest::prop_assert_eq!(with_model_option(std::slice::from_ref(&other), &id, None), vec![other]);
        }
    }

    #[test]
    fn authored_option_values_are_trimmed_and_only_canonical_scalar_arrays_are_accepted() {
        let options: Vec<ModelOptionSelection> = serde_json::from_value(serde_json::json!([
            {"id":" effort ","value":" high "}, {"id":"fastMode","value":false}
        ]))
        .unwrap();
        assert_eq!(model_option_string(&options, "effort"), Some("high"));
        assert_eq!(model_option_boolean(&options, "fastMode"), Some(false));
        for invalid in [
            serde_json::json!({"effort":"high"}),
            serde_json::json!([{"id":" ","value":false}]),
            serde_json::json!([{"id":"effort","value":" "}]),
            serde_json::json!([{"id":"effort","value":null}]),
            serde_json::json!([{"id":"effort","value":42}]),
            serde_json::json!([{"id":"effort","value":{}}]),
        ] {
            assert!(serde_json::from_value::<Vec<ModelOptionSelection>>(invalid).is_err());
        }
        assert_eq!(
            with_model_option(&options, " ", Some(ModelOptionValue::Boolean(true))),
            options
        );
        assert_eq!(
            with_model_option(
                &options,
                " effort ",
                Some(ModelOptionValue::String("  ".into()))
            ),
            vec![options[1].clone()]
        );
    }

    #[test]
    fn option_normalization_retains_prompt_choices_and_false_without_inventing_variant_overrides() {
        let caps: ModelCapabilities = serde_json::from_value(serde_json::json!({"optionDescriptors":[
            {"id":"effort","label":"Reasoning","type":"select","options":[{"id":"high","label":"High","isDefault":true},{"id":"ultrathink","label":"Ultra"}],"promptInjectedValues":["ultrathink"]},
            {"id":"fastMode","label":"Fast","type":"boolean","currentValue":true},
            {"id":"variant","label":"Variant","type":"select","options":[{"id":"default","label":"Default","isDefault":true}]},
            {"id":"opaque","label":"Opaque","type":"select","options":[]}
        ]})).unwrap();
        let saved: Vec<ModelOptionSelection> = serde_json::from_value(serde_json::json!([
            {"id":"effort","value":"ultrathink"},{"id":"fastMode","value":false},
            {"id":"opaque","value":" custom "},{"id":"unknown","value":true}
        ]))
        .unwrap();
        let original = saved.clone();
        let normalized = caps.normalize_options(&saved, true);
        assert_eq!(
            model_option_string(&normalized, "effort"),
            Some("ultrathink")
        );
        assert_eq!(
            caps.option_descriptors[0].value(&normalized),
            Some(ModelOptionValue::String("high".into()))
        );
        assert_eq!(model_option_boolean(&normalized, "fastMode"), Some(false));
        assert_eq!(model_option_string(&normalized, "opaque"), Some("custom"));
        assert!(model_option_value(&normalized, "unknown").is_none());
        assert!(model_option_value(&normalized, "variant").is_none());
        assert_eq!(caps.normalize_options(&normalized, true), normalized);
        assert_eq!(saved, original);
        assert!(caps.normalize_options(&[], false).is_empty());
        let defaults = caps.normalize_options(&[], true);
        assert_eq!(model_option_string(&defaults, "effort"), Some("high"));
        assert_eq!(model_option_boolean(&defaults, "fastMode"), Some(true));
        assert!(model_option_value(&defaults, "opaque").is_none());
        assert!(model_option_value(&defaults, "variant").is_none());
        let invalid = vec![
            ModelOptionSelection {
                id: "effort".into(),
                value: ModelOptionValue::String("unsupported".into()),
            },
            ModelOptionSelection {
                id: "fastMode".into(),
                value: ModelOptionValue::String("false".into()),
            },
            ModelOptionSelection {
                id: "variant".into(),
                value: ModelOptionValue::Boolean(false),
            },
            ModelOptionSelection {
                id: "opaque".into(),
                value: ModelOptionValue::String("  ".into()),
            },
        ];
        let corrected = caps.normalize_options(&invalid, false);
        assert_eq!(model_option_string(&corrected, "effort"), Some("high"));
        assert_eq!(model_option_boolean(&corrected, "fastMode"), Some(true));
        assert_eq!(model_option_string(&corrected, "variant"), Some("default"));
        assert!(model_option_value(&corrected, "opaque").is_none());
        assert_eq!(caps.normalize_options(&corrected, false), corrected);
    }

    #[test]
    fn custom_model_rows_keep_first_authored_identity_and_drop_only_bad_capabilities() {
        let source = serde_json::json!([
            null, 42, {}, "  ", " custom ", {"slug":"custom", "name":"ignored"},
            {"slug":"Custom", "name":" Named ", "capabilities":{"optionDescriptors":[
                {"id":" effort ","label":" Effort ","description":" Details ","type":"select","currentValue":" max ","options":[{"id":" max ","label":" Maximum ","description":" Choice ","isDefault":true}],"promptInjectedValues":[" max "]},
                {"id":"fastMode","label":"Fast","type":"boolean","currentValue":false}
            ]}},
            {"slug":"bad-caps", "name":" ", "capabilities":{"optionDescriptors":[{"id":" ","label":"Empty","type":"boolean"}]}},
            {"slug":"wrong-caps", "capabilities":{"optionDescriptors":"bad"}},
            {"slug":"empty-caps", "capabilities":{}}
        ]);
        let original = source.clone();
        let entries = read_custom_models(Some(&source));
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.slug.as_str(), entry.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("custom", "custom"),
                ("Custom", "Named"),
                ("bad-caps", "bad-caps"),
                ("wrong-caps", "wrong-caps"),
                ("empty-caps", "empty-caps")
            ]
        );
        assert!(entries[0].capabilities.is_none());
        let caps = entries[1].capabilities.as_ref().unwrap();
        assert_eq!(caps.option_descriptors[0].id, "effort");
        assert_eq!(caps.option_descriptors[0].choices()[0].label, "Maximum");
        assert_eq!(
            caps.option_descriptors[0].description.as_deref(),
            Some("Details")
        );
        assert_eq!(
            caps.option_descriptors[0].choices()[0]
                .description
                .as_deref(),
            Some("Choice")
        );
        assert_eq!(caps.option_descriptors[0].selected(None), Some("max"));
        assert_eq!(
            caps.option_descriptors[0].selected(Some("max")),
            Some("max")
        );
        assert!(matches!(
            caps.option_descriptors[1].kind,
            ModelOptionKind::Boolean {
                current_value: Some(false)
            }
        ));
        assert!(entries[2].capabilities.is_none());
        assert!(entries[3].capabilities.is_none());
        assert_eq!(
            entries[4].capabilities.as_ref().unwrap().option_descriptors,
            []
        );
        assert_eq!(source, original);
        assert!(read_custom_models(Some(&serde_json::json!({"slug":"not-an-array"}))).is_empty());
        for descriptor in [
            serde_json::json!({"id":"fast","label":"Fast","type":"boolean","currentValue":null}),
            serde_json::json!({"id":"fast","label":"Fast","type":"boolean","description":null}),
            serde_json::json!({"id":"effort","label":"Effort","type":"select","currentValue":null,"options":[]}),
            serde_json::json!({"id":"effort","label":"Effort","type":"select","options":[{"id":"low","label":"Low","description":null}]}),
            serde_json::json!({"id":"fast","label":"Fast","type":"boolean","description":" "}),
            serde_json::json!({"id":"effort","label":"Effort","type":"select","currentValue":" ","options":[]}),
            serde_json::json!({"id":"effort","label":"Effort","type":"select","options":[{"id":"low","label":"Low","description":" "}]}),
        ] {
            let rows = serde_json::json!([{"slug":"kept","capabilities":{"optionDescriptors":[descriptor]}}]);
            let kept = read_custom_models(Some(&rows));
            assert_eq!(kept[0].slug, "kept");
            assert!(kept[0].capabilities.is_none());
        }
    }

    #[test]
    fn descriptor_selections_respect_current_default_prompt_values_and_opaque_choices() {
        let descriptor: ModelOptionDescriptor = serde_json::from_value(serde_json::json!({
            "id":"effort","label":"Effort","type":"select","currentValue":"low",
            "options":[{"id":"low","label":"Low"},{"id":"high","label":"High","isDefault":true},{"id":"ultrathink","label":"Ultra"}],
            "promptInjectedValues":["ultrathink"]
        })).unwrap();
        for (saved, expected) in [
            (None, "low"),
            (Some(" "), "low"),
            (Some(" high "), "high"),
            (Some("unsupported"), "low"),
            (Some("ultrathink"), "high"),
        ] {
            assert_eq!(descriptor.selected(saved), Some(expected));
        }
        let opaque: ModelOptionDescriptor = serde_json::from_value(
            serde_json::json!({"id":"custom","label":"Custom","type":"select","options":[]}),
        )
        .unwrap();
        assert_eq!(opaque.selected(Some(" arbitrary ")), Some("arbitrary"));
        assert_eq!(opaque.selected(None), None);
        let boolean: ModelOptionDescriptor = serde_json::from_value(
            serde_json::json!({"id":"fast","label":"Fast","type":"boolean","currentValue":true}),
        )
        .unwrap();
        assert_eq!(boolean.selected(Some("true")), None);
    }

    proptest::proptest! {
        #[test]
        fn custom_slugs_are_unique_case_sensitive_and_keep_the_first_name(slug in "[a-zA-Z][a-zA-Z0-9/_-]{0,30}") {
            let source = serde_json::json!([
                {"slug":format!(" {slug} "),"name":"First"},null,
                {"slug":slug,"name":"Second"}, " ", {"slug":format!("other/{slug}"),"name":"Other"}
            ]);
            let entries = read_custom_models(Some(&source));
            proptest::prop_assert_eq!(entries.len(),2);
            proptest::prop_assert_eq!(&entries[0].slug,&slug);
            proptest::prop_assert_eq!(&entries[0].name,"First");
            proptest::prop_assert_eq!(&entries[1].slug,&format!("other/{slug}"));
        }
    }
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
        let result = json!({"thread":{"turns":[{"id":"turn","items":[{"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":text}}]}}}}},{"id":"agent","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":text,"phase":"unknown"}}}}},{"id":"command","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"日本語".repeat(1000),"cwd":null,"output":text,"exitCode":null}}}}},{"id":"files","status":"completed","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"a.txt","kind":{"update":{"movePath":null}},"diff":text,"proposal":null}],"output":""}}}}},{"id":"future","status":"completed","clientInputId":null,"body":{"inline":{"body":{"custom":{"driver":"codex","kind":"futureTool","value":{"id":"future","type":"futureTool","tool":"inspect","status":"completed","result":{"content":text}}}}}}},{"id":"small","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":["short"]}}}}}],"status":"unknown"}]}});
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
