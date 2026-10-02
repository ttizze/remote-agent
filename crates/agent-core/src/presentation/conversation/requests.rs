//! Present provider-neutral requests and construct validated native-editor answers.
use super::Request;
use agent_protocol::requests::Request as WireRequest;
use serde_json::Value;

pub(super) fn request(source: &WireRequest) -> Request {
    use agent_protocol::requests::{ApprovalKind, RequestBody};
    let (title, body, details) = match &source.body {
        RequestBody::Approval {
            kind,
            description,
            details,
            ..
        } => {
            let title = match kind {
                ApprovalKind::Command => "コマンドの承認待ち",
                ApprovalKind::FileChange => "ファイル変更の承認待ち",
                ApprovalKind::Tool => "ツールの承認待ち",
            };
            (title, description.clone(), details.clone())
        }
        RequestBody::Permission {
            description,
            details,
            ..
        } => ("権限の承認待ち", description.clone(), details.clone()),
        RequestBody::Question { questions } => (
            "回答待ち",
            questions
                .first()
                .map(|question| question.prompt.clone())
                .unwrap_or_default(),
            String::new(),
        ),
        RequestBody::Elicitation {
            server, message, ..
        } => (
            "MCPからの入力待ち",
            format!("{server}\n{message}"),
            String::new(),
        ),
        RequestBody::ToolExecution {
            tool, arguments, ..
        } => (
            "ツールの入力待ち",
            tool.clone(),
            serde_json::to_string_pretty(arguments).expect("arguments serialize"),
        ),
    };
    Request {
        id: source.id.clone(),
        title: match source.delivery {
            crate::session::RequestDelivery::Awaiting => title,
            crate::session::RequestDelivery::Sending => "回答を送信中",
            crate::session::RequestDelivery::Sent => "回答を送信しました",
            crate::session::RequestDelivery::Unknown => "回答の配送結果が不明です",
        }
        .into(),
        body,
        details,
        can_respond: source.delivery == crate::session::RequestDelivery::Awaiting,
        request_body: source.body.clone(),
    }
}

pub(super) fn build_question_answer(
    multiple: bool,
    text: String,
    choice_ids: Vec<String>,
) -> agent_protocol::requests::QuestionAnswer {
    use agent_protocol::requests::QuestionAnswer;
    if !text.is_empty() {
        QuestionAnswer::FreeText { text }
    } else if multiple {
        QuestionAnswer::MultipleChoices { choice_ids }
    } else {
        QuestionAnswer::SingleChoice {
            choice_id: choice_ids.into_iter().next().unwrap_or_default(),
        }
    }
}

pub(super) fn answer_from_json(
    body: &agent_protocol::requests::RequestBody,
    text: &str,
) -> Result<agent_protocol::requests::Answer, String> {
    use agent_protocol::requests::{Answer, ElicitationAnswer, RequestBody, ToolContent};
    let answer = match body {
        RequestBody::Elicitation { .. } => Answer::Elicitation {
            action: ElicitationAnswer::Accept {
                values: serde_json::from_str(text).map_err(|error| error.to_string())?,
            },
        },
        RequestBody::ToolExecution { .. } => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ResultInput {
                success: bool,
                content: Vec<ToolContent>,
            }
            let value: ResultInput =
                serde_json::from_str(text).map_err(|error| error.to_string())?;
            Answer::ToolExecution {
                success: value.success,
                content: value.content,
            }
        }
        _ => return Err("this request requires a typed choice or question answer".into()),
    };
    agent_protocol::requests::validate_answer(body, &answer)?;
    Ok(answer)
}

pub(super) fn request_input_default(body: agent_protocol::requests::RequestBody) -> String {
    use agent_protocol::requests::{ElicitationInput, FormInput, RequestBody};
    let value = match body {
        RequestBody::Elicitation {
            input: ElicitationInput::Form { fields },
            ..
        } => Value::Object(
            fields
                .into_iter()
                .filter_map(|field| {
                    let default = match field.input {
                        FormInput::String { default, .. } | FormInput::Choice { default, .. } => {
                            default.map(Value::from)
                        }
                        FormInput::Number { default, .. } => default.map(Value::from),
                        FormInput::Boolean { default } => default.map(Value::from),
                        FormInput::Multiple { default, .. } if !default.is_empty() => {
                            Some(serde_json::json!(default))
                        }
                        _ => None,
                    };
                    default.map(|value| (field.name, value))
                })
                .collect(),
        ),
        RequestBody::Elicitation {
            input: ElicitationInput::Url { .. },
            ..
        } => Value::Null,
        RequestBody::ToolExecution { .. } => serde_json::json!({"success":true,"content":[]}),
        _ => Value::Null,
    };
    serde_json::to_string_pretty(&value).expect("request defaults serialize")
}
