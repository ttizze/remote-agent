//! Provider-independent requests and the answers each request can accept.
use crate::ids::{ItemId, RequestId, TurnId};
use crate::session::RequestDelivery;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: RequestId,
    pub target: RequestTarget,
    pub delivery: RequestDelivery,
    pub body: RequestBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RequestTarget {
    Session,
    Turn {
        turn_id: TurnId,
        item_id: Option<ItemId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RequestBody {
    Approval {
        kind: ApprovalKind,
        description: String,
        details: String,
        choices: Vec<Choice>,
    },
    Permission {
        description: String,
        details: String,
        choices: Vec<Choice>,
    },
    Question {
        questions: Vec<Question>,
    },
    Elicitation {
        server: String,
        message: String,
        input: ElicitationInput,
    },
    ToolExecution {
        tool: String,
        namespace: Option<String>,
        #[serde(with = "crate::protocol::json")]
        arguments: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ApprovalKind {
    Command,
    FileChange,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub description: String,
    pub meaning: ChoiceMeaning,
    pub scope: ChoiceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChoiceMeaning {
    Allow,
    Deny,
    Cancel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChoiceScope {
    Once,
    Turn,
    Session,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub prompt: String,
    pub secret: bool,
    pub allow_free_text: bool,
    pub multiple: bool,
    pub choices: Vec<QuestionChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionChoice {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ElicitationInput {
    Form { fields: Vec<FormField> },
    Url { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub name: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub input: FormInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FormInput {
    String {
        min_length: Option<u64>,
        max_length: Option<u64>,
        format: Option<StringFormat>,
        default: Option<String>,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
        default: Option<f64>,
    },
    Boolean {
        default: Option<bool>,
    },
    Choice {
        choices: Vec<FormChoice>,
        default: Option<String>,
    },
    Multiple {
        choices: Vec<FormChoice>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        default: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum StringFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormChoice {
    pub value: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Answer {
    Approval {
        choice_id: String,
    },
    Permission {
        choice_id: String,
    },
    Questions {
        answers: BTreeMap<String, QuestionAnswer>,
    },
    Elicitation {
        action: ElicitationAnswer,
    },
    ToolExecution {
        success: bool,
        content: Vec<ToolContent>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum QuestionAnswer {
    FreeText { text: String },
    SingleChoice { choice_id: String },
    MultipleChoices { choice_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ElicitationAnswer {
    Accept {
        #[serde(with = "crate::protocol::json")]
        values: Value,
    },
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ToolContent {
    Text { text: String },
    Image { data_url: String },
}

impl RequestBody {
    pub fn choices(&self) -> &[Choice] {
        match self {
            Self::Approval { choices, .. } | Self::Permission { choices, .. } => choices,
            _ => &[],
        }
    }
}

/// Runs on both clients and the Host, before claiming a request or sending bytes.
pub fn validate_answer(body: &RequestBody, answer: &Answer) -> Result<(), String> {
    match (body, answer) {
        (RequestBody::Approval { choices, .. }, Answer::Approval { choice_id })
        | (RequestBody::Permission { choices, .. }, Answer::Permission { choice_id }) => {
            if !choices.iter().any(|choice| &choice.id == choice_id) {
                return Err("invalid request choice".into());
            }
        }
        (RequestBody::Question { questions }, Answer::Questions { answers }) => {
            if questions.len() != answers.len() {
                return Err("every question requires one answer".into());
            }
            for question in questions {
                let valid_choice = |id: &str| question.choices.iter().any(|choice| choice.id == id);
                match answers.get(&question.id) {
                    Some(QuestionAnswer::FreeText { text }) if question.allow_free_text && !text.trim().is_empty() => {}
                    Some(QuestionAnswer::SingleChoice { choice_id }) if !question.multiple && valid_choice(choice_id) => {}
                    Some(QuestionAnswer::MultipleChoices { choice_ids }) if question.multiple
                        && !choice_ids.is_empty()
                        && choice_ids.iter().all(|id| valid_choice(id))
                        && choice_ids.iter().collect::<BTreeSet<_>>().len() == choice_ids.len() => {}
                    _ => return Err("invalid question answer".into()),
                }
            }
        }
        (RequestBody::Elicitation { input, .. }, Answer::Elicitation { action }) => {
            if let ElicitationAnswer::Accept { values } = action {
                match input {
                    ElicitationInput::Form { fields } => validate_form(fields, values.as_object().ok_or("form values must be an object")?)?,
                    ElicitationInput::Url { .. } if values.is_null() => {}
                    _ => return Err("URL confirmation must not include form values".into()),
                }
            }
        }
        (RequestBody::ToolExecution { .. }, Answer::ToolExecution { content, .. }) => {
            if content.iter().any(|item| matches!(item, ToolContent::Image { data_url } if !data_url.starts_with("data:image/") || !data_url.contains(";base64,"))) {
                return Err("tool images must be embedded image data".into());
            }
        }
        _ => return Err("answer does not match request type".into()),
    }
    Ok(())
}

pub fn validate_form(fields: &[FormField], values: &Map<String, Value>) -> Result<(), String> {
    if values
        .keys()
        .any(|name| !fields.iter().any(|field| &field.name == name))
    {
        return Err("form contains an unknown field".into());
    }
    for field in fields {
        let Some(value) = values.get(&field.name) else {
            if field.required {
                return Err(format!("{} is required", field.title));
            }
            continue;
        };
        if !field.input.accepts(value) {
            return Err(format!("{} has an invalid value", field.title));
        }
    }
    Ok(())
}

impl FormInput {
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::String {
                min_length,
                max_length,
                format,
                ..
            } => value.as_str().is_some_and(|value| {
                let length = value.chars().count() as u64;
                min_length.is_none_or(|min| length >= min)
                    && max_length.is_none_or(|max| length <= max)
                    && format.is_none_or(|format| format.accepts(value))
            }),
            Self::Number {
                integer,
                minimum,
                maximum,
                ..
            } => value.as_f64().is_some_and(|value| {
                value.is_finite()
                    && (!integer || value.fract() == 0.)
                    && minimum.is_none_or(|min| value >= min)
                    && maximum.is_none_or(|max| value <= max)
            }),
            Self::Boolean { .. } => value.is_boolean(),
            Self::Choice { choices, .. } => value
                .as_str()
                .is_some_and(|value| choices.iter().any(|choice| choice.value == value)),
            Self::Multiple {
                choices,
                min_items,
                max_items,
                ..
            } => value.as_array().is_some_and(|values| {
                min_items.is_none_or(|min| values.len() as u64 >= min)
                    && max_items.is_none_or(|max| values.len() as u64 <= max)
                    && values.iter().all(|value| {
                        value
                            .as_str()
                            .is_some_and(|value| choices.iter().any(|choice| choice.value == value))
                    })
                    && values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == values.len()
            }),
        }
    }
}

impl StringFormat {
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Uri => url::Url::parse(value).is_ok(),
            Self::Email => value.split_once('@').is_some_and(|(name, host)| {
                !name.is_empty()
                    && !name.contains('@')
                    && !host.contains('@')
                    && host.contains('.')
                    && !host.starts_with('.')
                    && !host.ends_with('.')
                    && !value.chars().any(char::is_whitespace)
            }),
            Self::Date => {
                value.len() == 10 && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
            }
            Self::DateTime => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
        }
    }
}
