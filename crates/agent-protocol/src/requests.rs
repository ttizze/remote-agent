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
    pub effect: ChoiceEffect,
}

/// What answering with a choice does, so clients can rank the choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChoiceEffect {
    /// Grants this request only.
    Allow,
    /// Grants this request and matching later ones, for a scope or by a rule.
    AllowLasting,
    Deny,
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
    SkipQuestions,
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
    Accept { values: Value },
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

impl Question {
    pub fn accepts(&self, answer: &QuestionAnswer) -> bool {
        let valid_choice = |id: &str| self.choices.iter().any(|choice| choice.id == id);
        match answer {
            QuestionAnswer::FreeText { text } => self.allow_free_text && !text.trim().is_empty(),
            QuestionAnswer::SingleChoice { choice_id } => !self.multiple && valid_choice(choice_id),
            QuestionAnswer::MultipleChoices { choice_ids } => {
                self.multiple
                    && !choice_ids.is_empty()
                    && choice_ids.iter().all(|id| valid_choice(id))
                    && choice_ids.iter().collect::<BTreeSet<_>>().len() == choice_ids.len()
            }
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
            if !questions.iter().all(|question| {
                answers
                    .get(&question.id)
                    .is_some_and(|answer| question.accepts(answer))
            }) {
                return Err("invalid question answer".into());
            }
        }
        (RequestBody::Question { .. }, Answer::SkipQuestions) => {}
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
                let length_valid = (min_length.is_none() && max_length.is_none()) || {
                    let length = value.chars().count() as u64;
                    min_length.is_none_or(|min| length >= min)
                        && max_length.is_none_or(|max| length <= max)
                };
                length_valid && format.is_none_or(|format| format.accepts(value))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_forms_keep_scalar_bounds_formats_and_types_when_limits_are_absent() {
        for (min_length, max_length, format, text, accepted) in [
            (None, None, None, "", true),
            (None, None, None, "🦀éa", true),
            (Some(3), Some(3), None, "🦀éa", true),
            (Some(4), None, None, "🦀éa", false),
            (None, Some(2), None, "🦀éa", false),
            (Some(0), None, None, "", true),
            (Some(1), None, None, "", false),
            (Some(2), Some(1), None, "aa", false),
            (None, Some(0), None, "", true),
            (None, Some(0), None, "a", false),
            (Some(u64::MAX), None, None, "a", false),
            (None, Some(u64::MAX), None, "🦀éa", true),
            (None, None, Some(StringFormat::Date), "2026-11-05", true),
            (None, None, Some(StringFormat::Date), "2026-02-29", false),
            (None, None, Some(StringFormat::Email), "not-an-email", false),
        ] {
            let input = FormInput::String {
                min_length,
                max_length,
                format,
                default: None,
            };
            assert_eq!(input.accepts(&json!(text)), accepted, "{input:?}: {text}");
        }
        let input = FormInput::String {
            min_length: None,
            max_length: None,
            format: None,
            default: None,
        };
        for value in [Value::Null, json!(42), json!(true), json!([]), json!({})] {
            assert!(!input.accepts(&value), "{value}");
        }
    }

    #[test]
    fn every_question_needs_its_own_text_or_offered_choices() {
        let question = |id: &str, multiple| Question {
            id: id.into(),
            header: String::new(),
            prompt: "Choose".into(),
            secret: false,
            allow_free_text: true,
            multiple,
            choices: ["a", "b"]
                .map(|choice| QuestionChoice {
                    id: choice.into(),
                    label: choice.into(),
                    description: String::new(),
                })
                .into(),
        };
        let text = |text: &str| QuestionAnswer::FreeText { text: text.into() };
        let single = |id: &str| QuestionAnswer::SingleChoice {
            choice_id: id.into(),
        };
        let multiple = |ids: &[&str]| QuestionAnswer::MultipleChoices {
            choice_ids: ids.iter().map(|id| (*id).into()).collect(),
        };
        for (multiple_choice, answer, accepted) in [
            (false, text("Custom answer"), true),
            (false, text(" "), false),
            (false, single("a"), true),
            (false, single("unknown"), false),
            (false, multiple(&["a"]), false),
            (true, multiple(&["a", "b"]), true),
            (true, multiple(&[]), false),
            (true, multiple(&["a", "a"]), false),
            (true, single("a"), false),
        ] {
            let first = question("first", multiple_choice);
            assert_eq!(first.accepts(&answer), accepted, "{answer:?}");
            let answers = Answer::Questions {
                answers: [("first".into(), answer)].into(),
            };
            assert_eq!(
                validate_answer(
                    &RequestBody::Question {
                        questions: vec![first.clone()]
                    },
                    &answers
                )
                .is_ok(),
                accepted
            );
            assert!(
                validate_answer(
                    &RequestBody::Question {
                        questions: vec![first, question("second", false)]
                    },
                    &answers
                )
                .is_err(),
                "an unanswered question rejects the answers"
            );
        }
        let mut choice_only = question("first", false);
        choice_only.allow_free_text = false;
        assert!(!choice_only.accepts(&text("Custom answer")));
    }

    #[test]
    fn skipping_questions_is_explicit_and_cannot_answer_an_approval() {
        let question = RequestBody::Question {
            questions: vec![Question {
                id: "question".into(),
                header: String::new(),
                prompt: "Choose one".into(),
                secret: false,
                allow_free_text: false,
                multiple: false,
                choices: vec![QuestionChoice {
                    id: "choice".into(),
                    label: "Continue".into(),
                    description: String::new(),
                }],
            }],
        };
        assert!(validate_answer(&question, &Answer::SkipQuestions).is_ok());
        assert!(
            validate_answer(
                &question,
                &Answer::Questions {
                    answers: BTreeMap::new()
                }
            )
            .is_err()
        );
        assert!(
            validate_answer(
                &question,
                &Answer::Questions {
                    answers: [(
                        "question".into(),
                        QuestionAnswer::SingleChoice {
                            choice_id: String::new()
                        }
                    )]
                    .into(),
                }
            )
            .is_err()
        );
        for body in [
            RequestBody::Approval {
                kind: ApprovalKind::Command,
                description: String::new(),
                details: String::new(),
                choices: vec![],
            },
            RequestBody::Permission {
                description: String::new(),
                details: String::new(),
                choices: vec![],
            },
            RequestBody::Elicitation {
                server: "server".into(),
                message: String::new(),
                input: ElicitationInput::Url {
                    url: "https://example.com".into(),
                },
            },
            RequestBody::ToolExecution {
                tool: "tool".into(),
                namespace: None,
                arguments: Value::Null,
            },
        ] {
            assert!(validate_answer(&body, &Answer::SkipQuestions).is_err());
        }
        assert_eq!(
            serde_json::to_value(Answer::SkipQuestions).unwrap(),
            json!("skipQuestions")
        );
        assert_eq!(
            serde_json::from_value::<Answer>(json!("skipQuestions")).unwrap(),
            Answer::SkipQuestions
        );
    }
}
