//! Provider approval/question requests and client answers.
use crate::error::PeerError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    #[serde(default, rename = "deliveryState")]
    pub delivery_state: Option<crate::session::RequestDelivery>,
    #[serde(default, rename = "nativeRequestId")]
    #[serde(with = "crate::protocol::json")]
    pub native_request_id: Option<Value>,
    #[serde(with = "crate::protocol::json")]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub params: Map<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Answer {
    Decision {
        index: u32,
    },
    Permissions {
        allow: bool,
    },
    Questions {
        answers: std::collections::BTreeMap<String, String>,
    },
    Raw {
        value: Value,
    },
}
/// The UI and response validator use the same ordered choices.
pub fn approval_decisions(request: &ServerRequest) -> &[Value] {
    static DEFAULTS: std::sync::LazyLock<[Value; 4]> = std::sync::LazyLock::new(|| {
        ["accept", "acceptForSession", "decline", "cancel"].map(|value| Value::String(value.into()))
    });
    request
        .params
        .get("availableDecisions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(DEFAULTS.as_slice())
}
/// Validates a response before any bytes are queued. Unknown payloads remain intact.
pub fn answer_result(request: &ServerRequest, answer: &Answer) -> Result<Value, PeerError> {
    use serde_json::json;
    match answer {
        Answer::Decision { index } => {
            let choices = approval_decisions(request);
            let decision = choices
                .get(*index as usize)
                .ok_or_else(|| PeerError::InvalidMessage("invalid approval choice".into()))?;
            Ok(json!({"decision":decision}))
        }
        Answer::Permissions { allow } => {
            let permissions = if *allow {
                request
                    .params
                    .get("permissions")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("permissions object is missing".into())
                    })?
            } else {
                json!({})
            };
            Ok(json!({"permissions":permissions,"scope":"turn"}))
        }
        Answer::Questions { answers } => {
            #[derive(Deserialize)]
            struct Question {
                id: String,
            }
            let questions: Vec<Question> = serde_json::from_value(
                request
                    .params
                    .get("questions")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            let mut result = Map::new();
            for question in questions {
                let text = answers
                    .get(&question.id)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("every question requires an answer".into())
                    })?;
                result.insert(question.id, json!({"answers":[text]}));
            }
            Ok(json!({"answers":result}))
        }
        Answer::Raw { value } => Ok(value.clone()),
    }
}
/// Validate the complete wire answer before claiming a shared pending request.
pub fn validate_answer(request: &ServerRequest, result: &Value) -> Result<(), String> {
    if request.method == "item/tool/requestUserInput" {
        let questions = request
            .params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or("questions are unavailable")?;
        let answers = result["answers"]
            .as_object()
            .ok_or("answers are required")?;
        if answers.len() != questions.len() {
            return Err("every question requires one answer".into());
        }
        for question in questions {
            let id = question["id"].as_str().ok_or("question ID is missing")?;
            let values = answers
                .get(id)
                .and_then(|answer| answer["answers"].as_array())
                .ok_or("question answer is missing")?;
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
            {
                return Err("question answer is invalid".into());
            }
        }
    } else if request.method == "item/permissions/requestApproval" {
        let requested = request
            .params
            .get("permissions")
            .and_then(Value::as_object)
            .ok_or("permissions are unavailable")?;
        let granted = result["permissions"]
            .as_object()
            .ok_or("permissions are required")?;
        if result["scope"] != "turn"
            || granted
                .iter()
                .any(|(key, value)| requested.get(key) != Some(value))
        {
            return Err("answer grants unrequested permissions".into());
        }
    } else if matches!(
        request.method.as_str(),
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "claude/tool/requestApproval"
    ) {
        if !approval_decisions(request).contains(&result["decision"]) {
            return Err("invalid approval decision".into());
        }
    } else if request.method == "mcpServer/elicitation/request" {
        match result["action"].as_str() {
            Some("decline" | "cancel") if result["content"].is_null() => {}
            Some("accept")
                if matches!(
                    request.params.get("mode").and_then(Value::as_str),
                    Some("form" | "openai/form")
                ) && result["content"].is_object() => {}
            Some("accept")
                if request.params.get("mode").and_then(Value::as_str) == Some("url")
                    && result["content"].is_null() => {}
            _ => return Err("invalid MCP elicitation answer".into()),
        }
    } else if request.method == "item/tool/call" {
        if !result["success"].is_boolean()
            || result["contentItems"].as_array().is_none_or(|items| {
                items.iter().any(|item| match item["type"].as_str() {
                    Some("inputText") => !item["text"].is_string(),
                    Some("inputImage") => item["imageUrl"]
                        .as_str()
                        .is_none_or(|url| !url.starts_with("data:image/")),
                    _ => true,
                })
            })
        {
            return Err("invalid dynamic tool response".into());
        }
    } else {
        return Err("unsupported provider request cannot be approved".into());
    }
    Ok(())
}
