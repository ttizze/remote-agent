use crate::{AgentError, error};
use serde_json::Value;
use std::collections::HashMap;

/// Unknown wire fields remain typed values; no native JSON dictionaries or commands.
#[derive(Clone, uniffi::Enum)]
pub enum JsonValue {
    Null,
    Boolean { value: bool },
    Number { value: String },
    String { value: String },
    Array { values: Vec<JsonValue> },
    Object { fields: HashMap<String, JsonValue> },
}
impl From<&Value> for JsonValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Boolean { value: *value },
            Value::Number(value) => Self::Number {
                value: value.to_string(),
            },
            Value::String(value) => Self::String {
                value: value.clone(),
            },
            Value::Array(values) => Self::Array {
                values: values.iter().map(Into::into).collect(),
            },
            Value::Object(fields) => Self::Object {
                fields: fields.iter().map(|(k, v)| (k.clone(), v.into())).collect(),
            },
        }
    }
}
impl TryFrom<JsonValue> for Value {
    type Error = AgentError;
    fn try_from(value: JsonValue) -> Result<Self, Self::Error> {
        Ok(match value {
            JsonValue::Null => Value::Null,
            JsonValue::Boolean { value } => value.into(),
            JsonValue::Number { value } => Value::Number(value.parse().map_err(error)?),
            JsonValue::String { value } => value.into(),
            JsonValue::Array { values } => Value::Array(
                values
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()?,
            ),
            JsonValue::Object { fields } => Value::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| Ok((k, v.try_into()?)))
                    .collect::<Result<_, AgentError>>()?,
            ),
        })
    }
}
#[uniffi::export]
pub fn parse_json_value(text: String) -> Result<JsonValue, AgentError> {
    serde_json::from_str::<Value>(&text)
        .map(|value| (&value).into())
        .map_err(error)
}
#[uniffi::export]
pub fn format_json_value(value: JsonValue) -> Result<String, AgentError> {
    serde_json::to_string_pretty(&Value::try_from(value)?).map_err(error)
}

#[derive(Clone, uniffi::Record)]
pub struct Attachment {
    pub path: String,
    pub name: String,
    pub is_image: bool,
}
impl From<&agent_core::state::Attachment> for Attachment {
    fn from(a: &agent_core::state::Attachment) -> Self {
        Self {
            path: a.path.clone(),
            name: a.name.clone(),
            is_image: a.is_image,
        }
    }
}
impl From<Attachment> for agent_core::state::Attachment {
    fn from(a: Attachment) -> Self {
        Self {
            path: a.path,
            name: a.name,
            is_image: a.is_image,
        }
    }
}
#[derive(uniffi::Record)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}
impl From<&agent_core::state::Draft> for Draft {
    fn from(d: &agent_core::state::Draft) -> Self {
        Self {
            text: d.text.clone(),
            attachments: d.attachments.iter().map(Into::into).collect(),
            model: d.model.clone(),
            effort: d.effort.clone(),
            service_tier: d.service_tier.clone(),
        }
    }
}
#[derive(uniffi::Record)]
pub struct ListQuery {
    pub project_limit: u32,
    pub chat_limit: u32,
    pub project_thread_limits: HashMap<String, u32>,
    pub search_term: String,
}
impl From<ListQuery> for agent_core::models::ListQuery {
    fn from(q: ListQuery) -> Self {
        Self {
            project_limit: q.project_limit as usize,
            chat_limit: q.chat_limit as usize,
            project_thread_limits: q
                .project_thread_limits
                .into_iter()
                .map(|(k, v)| (k, v as usize))
                .collect(),
            search_term: q.search_term,
        }
    }
}
#[derive(Clone, uniffi::Record)]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
}
impl From<WorktreeSettings> for agent_core::models::WorktreeSettings {
    fn from(s: WorktreeSettings) -> Self {
        Self {
            create_on_new_session: s.create_on_new_session,
            copy_on_create: s.copy_on_create,
            copy_paths: s.copy_paths,
            worktree_directory: s.worktree_directory,
            extra: Default::default(),
        }
    }
}
#[derive(uniffi::Record)]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}
#[derive(uniffi::Enum)]
pub enum Outcome {
    Applied,
    StartedThread { id: String },
    Submitted { turn_id: Option<String> },
    RemoteHostPaired { id: String },
    SessionImages { images: Vec<SessionImage> },
}
impl From<agent_core::store::Outcome> for Outcome {
    fn from(o: agent_core::store::Outcome) -> Self {
        use agent_core::store::Outcome as O;
        match o {
            O::Applied => Self::Applied,
            O::StartedThread(id) => Self::StartedThread { id },
            O::Submitted(turn_id) => Self::Submitted { turn_id },
            O::RemoteHostPaired(id) => Self::RemoteHostPaired { id },
            O::SessionImages(images) => Self::SessionImages {
                images: images
                    .into_iter()
                    .map(|i| SessionImage {
                        source: i.source,
                        encoded: i.encoded,
                    })
                    .collect(),
            },
        }
    }
}
