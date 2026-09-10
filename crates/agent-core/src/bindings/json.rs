use super::{AgentError, error};
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
impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Boolean { value },
            Value::Number(value) => Self::Number {
                value: value.to_string(),
            },
            Value::String(value) => Self::String { value },
            Value::Array(values) => Self::Array {
                values: values.into_iter().map(Into::into).collect(),
            },
            Value::Object(fields) => Self::Object {
                fields: fields.into_iter().map(|(k, v)| (k, v.into())).collect(),
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
pub fn parse_json_value(text: String) -> Result<Value, AgentError> {
    serde_json::from_str::<Value>(&text).map_err(error)
}
#[uniffi::export]
pub fn format_json_value(value: Value) -> Result<String, AgentError> {
    serde_json::to_string_pretty(&value).map_err(error)
}

// Foreign types need one converter, independent of how many records contain them.
type JsonObject = serde_json::Map<String, Value>;
type ThreadLimits = std::collections::BTreeMap<String, u32>;
use uuid::Uuid;
uniffi::custom_type!(Value, JsonValue, { remote, lower: |value| value.into(), try_lift: |value| Ok(value.try_into()?) });
uniffi::custom_type!(JsonObject, HashMap<String, Value>, {
    remote, lower: |fields| fields.into_iter().collect(), try_lift: |fields| Ok(fields.into_iter().collect())
});
uniffi::custom_type!(ThreadLimits, HashMap<String, u32>, {
    remote, lower: |limits| limits.into_iter().collect(), try_lift: |limits| Ok(limits.into_iter().collect())
});
uniffi::custom_type!(Uuid, String, { remote, lower: |value| value.to_string(), try_lift: |value| Ok(value.parse()?) });

type QuestionAnswers = std::collections::BTreeMap<String, String>;
uniffi::custom_type!(QuestionAnswers, HashMap<String, String>, {
    remote, lower: |answers| answers.into_iter().collect(), try_lift: |answers| Ok(answers.into_iter().collect())
});
