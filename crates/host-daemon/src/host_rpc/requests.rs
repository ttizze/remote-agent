//! Native request/answer conversion. Native identities and effects stay private.
use agent_protocol::{ids::RequestId, requests::*, session::RequestDelivery};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The Host keeps normalized requests and source identity. The adapter owns
/// answer mappings and the resource to which an answer can be written.
#[async_trait::async_trait]
pub(crate) trait AnswerSource: Send + Sync {
    fn is_alive(&self) -> bool;
    async fn prepare(
        &self,
        native_id: &Value,
        body: &RequestBody,
        answer: &Answer,
    ) -> Result<super::agent::AnswerWrite, super::service::Failure>;
}
#[derive(Clone)]
pub(crate) struct RequestOrigin {
    pub instance: uuid::Uuid,
    pub native_id: Value,
    pub provider: agent_protocol::session::ProviderKind,
    pub source: std::sync::Arc<dyn AnswerSource>,
}
#[cfg(test)]
pub(crate) fn unavailable_origin(
    instance: uuid::Uuid,
    native_id: Value,
    stopped: tokio_util::sync::CancellationToken,
) -> RequestOrigin {
    struct UnavailableSource(tokio_util::sync::CancellationToken);
    #[async_trait::async_trait]
    impl AnswerSource for UnavailableSource {
        fn is_alive(&self) -> bool {
            !self.0.is_cancelled()
        }
        async fn prepare(
            &self,
            _: &Value,
            _: &RequestBody,
            _: &Answer,
        ) -> Result<super::agent::AnswerWrite, super::service::Failure> {
            Err(super::service::Failure::new(
                "answer_not_sent",
                "request source has changed",
            ))
        }
    }
    RequestOrigin {
        instance,
        native_id,
        provider: agent_protocol::session::ProviderKind::Codex,
        source: std::sync::Arc::new(UnavailableSource(stopped)),
    }
}

pub(crate) struct AdaptedRequest {
    pub request: Request,
    pub answers: NativeAnswers,
}

pub(crate) enum NativeAnswers {
    Choices(BTreeMap<String, Value>),
    Questions {
        questions: Vec<NativeQuestion>,
        claude_input: Option<Value>,
    },
    Elicitation,
    Tool,
}

pub(crate) struct NativeQuestion {
    id: String,
    native_id: String,
    choices: BTreeMap<String, String>,
}

impl NativeAnswers {
    pub(crate) fn translate(&self, body: &RequestBody, answer: &Answer) -> Result<Value, String> {
        validate_answer(body, answer)?;
        Ok(match (self, answer) {
            (
                Self::Choices(choices),
                Answer::Approval { choice_id } | Answer::Permission { choice_id },
            ) => choices
                .get(choice_id)
                .ok_or("request choice mapping is missing")?
                .clone(),
            (
                Self::Questions {
                    questions,
                    claude_input,
                },
                Answer::Questions { answers },
            ) => {
                let mut result = serde_json::Map::new();
                for question in questions {
                    let labels = match &answers[&question.id] {
                        QuestionAnswer::FreeText { text } => vec![text.clone()],
                        QuestionAnswer::SingleChoice { choice_id } => vec![
                            question
                                .choices
                                .get(choice_id)
                                .ok_or("question choice mapping is missing")?
                                .clone(),
                        ],
                        QuestionAnswer::MultipleChoices { choice_ids } => choice_ids
                            .iter()
                            .map(|id| {
                                question
                                    .choices
                                    .get(id)
                                    .cloned()
                                    .ok_or("question choice mapping is missing")
                            })
                            .collect::<Result<_, _>>()?,
                    };
                    result.insert(
                        question.native_id.clone(),
                        if claude_input.is_some() {
                            json!(labels.join(", "))
                        } else {
                            json!({"answers":labels})
                        },
                    );
                }
                if let Some(input) = claude_input {
                    let mut input = input.clone();
                    input["answers"] = result.into();
                    json!({"behavior":"allow","updatedInput":input})
                } else {
                    json!({"answers":result})
                }
            }
            (Self::Elicitation, Answer::Elicitation { action }) => match action {
                ElicitationAnswer::Accept { values } => json!({"action":"accept","content":values}),
                ElicitationAnswer::Decline => json!({"action":"decline","content":null}),
                ElicitationAnswer::Cancel => json!({"action":"cancel","content":null}),
            },
            (Self::Tool, Answer::ToolExecution { success, content }) => {
                json!({"success":success,"contentItems":content.iter().map(|item| match item {
                ToolContent::Text { text } => json!({"type":"inputText","text":text}),
                ToolContent::Image { data_url } => json!({"type":"inputImage","imageUrl":data_url}),
            }).collect::<Vec<_>>()})
            }
            _ => return Err("answer mapping does not match request".into()),
        })
    }
}

fn string(value: &Value, name: &str) -> String {
    value[name].as_str().unwrap_or_default().into()
}
fn detail(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("native value serializes")
}
fn choice(id: &RequestId, index: usize, label: &str, description: String) -> Choice {
    Choice {
        id: format!("{id}/choice/{index}"),
        label: label.into(),
        description,
    }
}

pub(crate) fn codex(id: RequestId, method: &str, params: &Value) -> Result<AdaptedRequest, String> {
    let target = match params["turnId"].as_str().filter(|id| !id.is_empty()) {
        Some(turn) => RequestTarget::Turn {
            turn_id: turn.into(),
            item_id: params["itemId"].as_str().map(Into::into),
        },
        None if method == "mcpServer/elicitation/request" => RequestTarget::Session,
        _ => return Err("native request turn ID is missing".into()),
    };
    let (body, answers) = match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let defaults =
                ["accept", "acceptForSession", "decline", "cancel"].map(|value| json!(value));
            let decisions = if method == "item/fileChange/requestApproval" {
                defaults.as_slice()
            } else {
                match &params["availableDecisions"] {
                    Value::Null => defaults.as_slice(),
                    Value::Array(values) => values.as_slice(),
                    _ => return Err("native approval choices must be an array".into()),
                }
            };
            let mut choices = Vec::new();
            let mut mapping = BTreeMap::new();
            for (index, decision) in decisions.iter().enumerate() {
                let (label, allows, description) = approval_choice(decision)?;
                if allows
                    && params
                        .get("additionalPermissions")
                        .is_some_and(|permissions| {
                            !permissions.is_null()
                                && permissions
                                    .as_object()
                                    .and_then(permission_description)
                                    .is_none()
                        })
                {
                    continue;
                }
                let choice = choice(&id, index, label, description);
                mapping.insert(choice.id.clone(), json!({"decision":decision}));
                choices.push(choice);
            }
            if choices.is_empty() {
                return Err("native request has no supported choices".into());
            }
            let kind = if method == "item/fileChange/requestApproval" {
                ApprovalKind::FileChange
            } else {
                ApprovalKind::Command
            };
            (
                RequestBody::Approval {
                    kind,
                    description: string(params, "reason"),
                    details: [
                        ("command", "コマンド"),
                        ("cwd", "作業ディレクトリ"),
                        ("grantRoot", "書込み対象"),
                    ]
                    .iter()
                    .filter_map(|(key, label)| {
                        params[*key]
                            .as_str()
                            .map(|value| format!("{label}: {value}"))
                    })
                    .chain(
                        params["additionalPermissions"]
                            .as_object()
                            .map(|permissions| {
                                permission_description(permissions).unwrap_or_else(|| {
                                    "未対応の追加権限が含まれるため、許可できません。".into()
                                })
                            }),
                    )
                    .collect::<Vec<_>>()
                    .join("\n"),
                    choices,
                },
                NativeAnswers::Choices(mapping),
            )
        }
        "item/permissions/requestApproval" => {
            let permissions = params["permissions"]
                .as_object()
                .ok_or("native permissions are missing")?;
            let mut choices = Vec::new();
            let mut mapping = BTreeMap::new();
            let description = permission_description(permissions);
            if let Some(description) = &description {
                for (index, scope, label) in [
                    (0, "turn", "このターンで許可"),
                    (1, "session", "このセッションで許可"),
                ] {
                    let choice = choice(&id, index, label, description.clone());
                    mapping.insert(
                        choice.id.clone(),
                        json!({"permissions":permissions,"scope":scope}),
                    );
                    choices.push(choice);
                }
            }
            let deny = choice(&id, 2, "拒否", String::new());
            mapping.insert(deny.id.clone(), json!({"permissions":{},"scope":"turn"}));
            choices.push(deny);
            (
                RequestBody::Permission {
                    description: string(params, "reason"),
                    details: description.unwrap_or_else(|| {
                        "未対応の権限形式が含まれるため、許可できません。".into()
                    }),
                    choices,
                },
                NativeAnswers::Choices(mapping),
            )
        }
        "item/tool/requestUserInput" => questions(&id, &params["questions"], None)?,
        "mcpServer/elicitation/request" => (
            RequestBody::Elicitation {
                server: string(params, "serverName"),
                message: string(params, "message"),
                input: elicitation_input(params, "requestedSchema")?,
            },
            NativeAnswers::Elicitation,
        ),
        "item/tool/call" => (
            RequestBody::ToolExecution {
                tool: string(params, "tool"),
                namespace: params["namespace"].as_str().map(Into::into),
                arguments: params["arguments"].clone(),
            },
            NativeAnswers::Tool,
        ),
        _ => return Err("unsupported native request".into()),
    };
    Ok(AdaptedRequest {
        request: Request {
            id,
            target,
            delivery: RequestDelivery::Awaiting,
            body,
        },
        answers,
    })
}

pub(crate) fn claude(
    id: RequestId,
    turn_id: &agent_protocol::ids::TurnId,
    params: &Value,
) -> Result<AdaptedRequest, String> {
    let (target, body, answers) = match params["subtype"].as_str() {
        Some("can_use_tool") => {
            let target = RequestTarget::Turn {
                turn_id: turn_id.clone(),
                item_id: params["tool_use_id"].as_str().map(Into::into),
            };
            let input = &params["input"];
            if params["tool_name"] == "AskUserQuestion" {
                let (body, answers) = questions(&id, &input["questions"], Some(input.clone()))?;
                (target, body, answers)
            } else {
                let allow = choice(&id, 0, "承認", String::new());
                let deny = choice(&id, 1, "拒否", String::new());
                let mapping = [
                    (
                        allow.id.clone(),
                        json!({"behavior":"allow","updatedInput":input}),
                    ),
                    (
                        deny.id.clone(),
                        json!({"behavior":"deny","message":"ユーザーがこの操作を拒否しました。"}),
                    ),
                ]
                .into();
                let kind = match params["tool_name"].as_str() {
                    Some("Bash") => ApprovalKind::Command,
                    Some("Write" | "Edit" | "NotebookEdit") => ApprovalKind::FileChange,
                    _ => ApprovalKind::Tool,
                };
                (
                    target,
                    RequestBody::Approval {
                        kind,
                        description: string(params, "tool_name"),
                        details: detail(input),
                        choices: vec![allow, deny],
                    },
                    NativeAnswers::Choices(mapping),
                )
            }
        }
        Some("elicitation") => (
            RequestTarget::Session,
            RequestBody::Elicitation {
                server: string(params, "mcp_server_name"),
                message: string(params, "message"),
                input: elicitation_input(params, "requested_schema")?,
            },
            NativeAnswers::Elicitation,
        ),
        _ => return Err("unsupported Claude control request".into()),
    };
    Ok(AdaptedRequest {
        request: Request {
            id,
            target,
            delivery: RequestDelivery::Awaiting,
            body,
        },
        answers,
    })
}

fn questions(
    id: &RequestId,
    value: &Value,
    claude_input: Option<Value>,
) -> Result<(RequestBody, NativeAnswers), String> {
    let native = value.as_array().ok_or("native questions are missing")?;
    if native.is_empty() {
        return Err("native questions are empty".into());
    }
    let mut seen = BTreeSet::new();
    let mut questions = Vec::new();
    let mut mapping = Vec::new();
    for (index, question) in native.iter().enumerate() {
        let native_id = string(
            question,
            if claude_input.is_some() {
                "question"
            } else {
                "id"
            },
        );
        if native_id.is_empty() || !seen.insert(native_id.clone()) {
            return Err("native question identity is ambiguous".into());
        }
        let question_id = format!("{id}/question/{index}");
        let mut choices = Vec::new();
        let mut labels = BTreeMap::new();
        let mut seen_labels = BTreeSet::new();
        for (index, option) in question["options"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let label = string(option, "label");
            if label.is_empty() || !seen_labels.insert(label.clone()) {
                return Err("native question choices are ambiguous".into());
            }
            let choice_id = format!("{question_id}/choice/{index}");
            labels.insert(choice_id.clone(), label.clone());
            choices.push(QuestionChoice {
                id: choice_id,
                label,
                description: string(option, "description"),
            });
        }
        questions.push(Question {
            id: question_id.clone(),
            header: string(question, "header"),
            prompt: string(question, "question"),
            secret: question["isSecret"] == true,
            allow_free_text: choices.is_empty()
                || claude_input.is_some()
                || question["isOther"] == true,
            multiple: question["multiSelect"] == true,
            choices,
        });
        mapping.push(NativeQuestion {
            id: question_id,
            native_id,
            choices: labels,
        });
    }
    Ok((
        RequestBody::Question { questions },
        NativeAnswers::Questions {
            questions: mapping,
            claude_input,
        },
    ))
}

/// Returns the label, whether the choice grants the request, and its description.
fn approval_choice(decision: &Value) -> Result<(&'static str, bool, String), String> {
    let (label, allows) = match decision.as_str() {
        Some("accept") => ("承認", true),
        Some("acceptForSession") => ("このセッションで承認", true),
        Some("decline") => ("拒否", false),
        Some("cancel") => ("キャンセル", false),
        _ => {
            let fields = decision
                .as_object()
                .filter(|fields| fields.len() == 1)
                .ok_or("unsupported native approval choice")?;
            if let Some(amendment) = fields.get("acceptWithExecpolicyAmendment") {
                let fields = amendment
                    .as_object()
                    .filter(|fields| fields.len() == 1)
                    .ok_or("unsupported execution rule amendment")?;
                let arguments = fields
                    .get("execpolicy_amendment")
                    .and_then(Value::as_array)
                    .filter(|values| values.iter().all(Value::is_string))
                    .ok_or("execution rule arguments are missing")?;
                return Ok((
                    "実行ルールを追加して承認",
                    true,
                    format!(
                        "今後、次の引数のコマンドを許可します: {}",
                        detail(&json!(arguments))
                    ),
                ));
            }
            if let Some(amendment) = fields.get("applyNetworkPolicyAmendment") {
                let fields = amendment
                    .as_object()
                    .filter(|fields| fields.len() == 1)
                    .ok_or("unsupported network rule amendment")?;
                let rule = fields
                    .get("network_policy_amendment")
                    .and_then(Value::as_object)
                    .filter(|fields| fields.len() == 2)
                    .ok_or("unsupported network policy choice")?;
                let host = rule
                    .get("host")
                    .and_then(Value::as_str)
                    .ok_or("network policy host is missing")?;
                return match rule.get("action").and_then(Value::as_str) {
                    Some("allow") => Ok((
                        "ネットワーク許可ルールを追加",
                        true,
                        format!(
                            "今後、次のホストへの接続を許可します: {}",
                            detail(&json!(host))
                        ),
                    )),
                    Some("deny") => Ok((
                        "ネットワーク拒否ルールを追加",
                        false,
                        format!(
                            "今後、次のホストへの接続を拒否します: {}",
                            detail(&json!(host))
                        ),
                    )),
                    _ => Err("unsupported network policy action".into()),
                };
            }
            return Err("unsupported native approval choice".into());
        }
    };
    Ok((label, allows, String::new()))
}

fn permission_description(permissions: &serde_json::Map<String, Value>) -> Option<String> {
    let mut lines = Vec::new();
    for (key, value) in permissions {
        if !matches!(key.as_str(), "network" | "fileSystem") {
            return None;
        }
        if value.is_null() {
            continue;
        }
        let fields = value.as_object()?;
        match key.as_str() {
            "network" => {
                if fields.keys().any(|key| key != "enabled") {
                    return None;
                }
                if let Some(enabled) = fields.get("enabled").filter(|value| !value.is_null()) {
                    lines.push(
                        if enabled.as_bool()? {
                            "ネットワーク接続を許可"
                        } else {
                            "ネットワーク接続を拒否"
                        }
                        .into(),
                    );
                }
            }
            "fileSystem" => {
                for (key, value) in fields {
                    if !matches!(
                        key.as_str(),
                        "read" | "write" | "globScanMaxDepth" | "entries"
                    ) {
                        return None;
                    }
                    if value.is_null() {
                        continue;
                    }
                    match key.as_str() {
                        "read" | "write" => {
                            for path in value.as_array()? {
                                let path = path.as_str()?;
                                if !std::path::Path::new(path).is_absolute() {
                                    return None;
                                }
                                lines.push(format!(
                                    "{}: {path}",
                                    if key == "read" {
                                        "ファイルの読取り"
                                    } else {
                                        "ファイルの書込み"
                                    }
                                ));
                            }
                        }
                        "globScanMaxDepth" => {
                            let depth = value.as_u64().filter(|depth| *depth > 0)?;
                            lines.push(format!("ファイル検索の最大深さ: {depth}"));
                        }
                        "entries" => {
                            for entry in value.as_array()? {
                                let entry = entry.as_object().filter(|fields| fields.len() == 2)?;
                                let access = match entry.get("access")?.as_str()? {
                                    "read" => "読取り",
                                    "write" => "書込み",
                                    "deny" => "アクセス拒否",
                                    _ => return None,
                                };
                                let path = entry
                                    .get("path")?
                                    .as_object()
                                    .filter(|fields| fields.len() == 2)?;
                                let target = match path.get("type")?.as_str()? {
                                    "path" => {
                                        let path = path.get("path")?.as_str()?;
                                        if !std::path::Path::new(path).is_absolute() {
                                            return None;
                                        }
                                        path.to_owned()
                                    }
                                    "glob_pattern" => format!(
                                        "一致するファイル: {}",
                                        path.get("pattern")?.as_str()?
                                    ),
                                    "special" => {
                                        let special = path.get("value")?.as_object()?;
                                        let label = match special.get("kind")?.as_str()? {
                                            "root" => "全ファイル",
                                            "minimal" => "最小限のファイル",
                                            "project_roots" => "プロジェクトのファイル",
                                            "tmpdir" => "一時ディレクトリ",
                                            "slash_tmp" => "/tmp",
                                            _ => return None,
                                        };
                                        if special
                                            .keys()
                                            .any(|key| !matches!(key.as_str(), "kind" | "subpath"))
                                        {
                                            return None;
                                        }
                                        match special
                                            .get("subpath")
                                            .filter(|value| !value.is_null())
                                        {
                                            Some(path) => format!("{label}/{}", path.as_str()?),
                                            None => label.into(),
                                        }
                                    }
                                    _ => return None,
                                };
                                lines.push(format!("ファイルの{access}: {target}"));
                            }
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
    }
    Some(lines.join("\n"))
}

fn elicitation_input(params: &Value, schema_key: &str) -> Result<ElicitationInput, String> {
    match params["mode"].as_str().unwrap_or("form") {
        "url" => {
            let url = params["url"].as_str().ok_or("elicitation URL is missing")?;
            let parsed = reqwest::Url::parse(url).map_err(|_| "elicitation URL is invalid")?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err("unsupported elicitation URL scheme".into());
            }
            Ok(ElicitationInput::Url { url: url.into() })
        }
        "form" | "openai/form" => Ok(ElicitationInput::Form {
            fields: form_fields(&params[schema_key])?,
        }),
        _ => Err("unsupported elicitation mode".into()),
    }
}

fn form_fields(schema: &Value) -> Result<Vec<FormField>, String> {
    if schema["type"] != "object"
        || schema.as_object().is_none_or(|schema| {
            schema.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "type"
                        | "properties"
                        | "required"
                        | "title"
                        | "description"
                        | "$schema"
                        | "additionalProperties"
                )
            })
        })
    {
        return Err("unsupported elicitation form schema".into());
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("elicitation form properties are missing")?;
    if schema
        .get("required")
        .is_some_and(|value| !value.is_array())
    {
        return Err("required form fields must be an array".into());
    }
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|value| value.as_str().ok_or("invalid required field name"))
        .collect::<Result<_, _>>()?;
    if required.iter().any(|name| !properties.contains_key(*name)) {
        return Err("required form field is missing".into());
    }
    properties
        .iter()
        .map(|(name, property)| {
            let input = form_input(property)?;
            if let Some(default) = property.get("default")
                && !input.accepts(default)
            {
                return Err("invalid form default".into());
            }
            Ok(FormField {
                name: name.clone(),
                title: property["title"].as_str().unwrap_or(name).into(),
                description: string(property, "description"),
                required: required.contains(name.as_str()),
                input,
            })
        })
        .collect()
}

fn form_input(schema: &Value) -> Result<FormInput, String> {
    let properties = schema
        .as_object()
        .ok_or("form field schema must be an object")?;
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if schema
            .get(key)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err("form bounds must be nonnegative integers".into());
        }
    }
    for key in ["minimum", "maximum"] {
        if schema
            .get(key)
            .is_some_and(|value| value.as_f64().is_none_or(|value| !value.is_finite()))
        {
            return Err("form numeric bounds must be finite numbers".into());
        }
    }
    for (minimum, maximum) in [
        ("minLength", "maxLength"),
        ("minItems", "maxItems"),
        ("minimum", "maximum"),
    ] {
        if let (Some(minimum), Some(maximum)) = (schema[minimum].as_f64(), schema[maximum].as_f64())
            && minimum > maximum
        {
            return Err("form bounds are reversed".into());
        }
    }
    let field_type = schema["type"]
        .as_str()
        .ok_or("form field type is missing")?;
    if properties.keys().any(|key| {
        !matches!(key.as_str(), "type" | "title" | "description" | "default")
            && !match field_type {
                "string" => matches!(
                    key.as_str(),
                    "minLength" | "maxLength" | "format" | "enum" | "enumNames" | "oneOf" | "anyOf"
                ),
                "number" | "integer" => matches!(key.as_str(), "minimum" | "maximum"),
                "boolean" => false,
                "array" => matches!(
                    key.as_str(),
                    "items" | "minItems" | "maxItems" | "uniqueItems"
                ),
                _ => false,
            }
    }) {
        return Err("form constraint does not match field type".into());
    }
    if schema
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("uniqueItems must be a boolean".into());
    }
    if schema.get("format").is_some_and(|value| !value.is_string()) {
        return Err("string format must be a string".into());
    }
    let choice = form_choices(schema)?;
    if !choice.is_empty()
        && ["minLength", "maxLength", "format"]
            .iter()
            .any(|key| schema.get(key).is_some())
    {
        return Err("combined string and enum constraints are unsupported".into());
    }
    Ok(match schema["type"].as_str() {
        Some("string") if !choice.is_empty() => FormInput::Choice {
            choices: choice,
            default: schema["default"].as_str().map(Into::into),
        },
        Some("string") => {
            let format = match schema["format"].as_str() {
                None => None,
                Some("email") => Some(StringFormat::Email),
                Some("uri") => Some(StringFormat::Uri),
                Some("date") => Some(StringFormat::Date),
                Some("date-time") => Some(StringFormat::DateTime),
                _ => return Err("unsupported string format".into()),
            };
            FormInput::String {
                min_length: schema["minLength"].as_u64(),
                max_length: schema["maxLength"].as_u64(),
                format,
                default: schema["default"].as_str().map(Into::into),
            }
        }
        Some("number" | "integer") => FormInput::Number {
            integer: schema["type"] == "integer",
            minimum: schema["minimum"].as_f64(),
            maximum: schema["maximum"].as_f64(),
            default: schema["default"].as_f64(),
        },
        Some("boolean") => FormInput::Boolean {
            default: schema["default"].as_bool(),
        },
        Some("array") => {
            let FormInput::Choice { choices, .. } = form_input(&schema["items"])? else {
                return Err("form arrays must contain enumerated strings".into());
            };
            FormInput::Multiple {
                choices,
                min_items: schema["minItems"].as_u64(),
                max_items: schema["maxItems"].as_u64(),
                default: schema["default"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|value| {
                        value
                            .as_str()
                            .map(Into::into)
                            .ok_or("invalid multi-choice default")
                    })
                    .collect::<Result<_, _>>()?,
            }
        }
        _ => return Err("unsupported form field type".into()),
    })
}

fn form_choices(schema: &Value) -> Result<Vec<FormChoice>, String> {
    if schema.get("enumNames").is_some() && schema.get("enum").is_none() {
        return Err("enum display names require enum values".into());
    }
    if ["enum", "oneOf", "anyOf"]
        .iter()
        .filter(|key| schema.get(**key).is_some())
        .count()
        > 1
    {
        return Err("multiple enum representations are unsupported".into());
    }
    for key in ["enum", "oneOf", "anyOf", "enumNames"] {
        if schema
            .get(key)
            .is_some_and(|value| !value.is_array() || value.as_array().is_some_and(Vec::is_empty))
        {
            return Err("form choices must be a nonempty array".into());
        }
    }
    let choices: Vec<FormChoice> = if let Some(values) = schema["enum"].as_array() {
        if let Some(names) = schema["enumNames"].as_array()
            && names.len() != values.len()
        {
            return Err("invalid enum display names".into());
        }
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.as_str().ok_or("form enum values must be strings")?;
                Ok(FormChoice {
                    value: value.into(),
                    title: match schema.get("enumNames") {
                        Some(names) => names[index]
                            .as_str()
                            .ok_or("enum display names must be strings")?,
                        None => value,
                    }
                    .into(),
                })
            })
            .collect::<Result<_, String>>()?
    } else if let Some(values) = schema["oneOf"]
        .as_array()
        .or_else(|| schema["anyOf"].as_array())
    {
        values
            .iter()
            .map(|value| {
                if value.as_object().is_none_or(|fields| {
                    fields
                        .keys()
                        .any(|key| !matches!(key.as_str(), "const" | "title"))
                }) {
                    return Err("unsupported titled enum constraint".into());
                }
                let text = value["const"]
                    .as_str()
                    .ok_or("titled enum must contain string constants")?;
                Ok(FormChoice {
                    value: text.into(),
                    title: value["title"].as_str().unwrap_or(text).into(),
                })
            })
            .collect::<Result<_, String>>()?
    } else {
        Vec::new()
    };
    if choices
        .iter()
        .map(|choice| &choice.value)
        .collect::<BTreeSet<_>>()
        .len()
        != choices.len()
    {
        return Err("form choices must be unique".into());
    }
    Ok(choices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_choices_preserve_native_effects_and_describe_their_lifetime() {
        let decisions = json!(["accept", "acceptForSession", "decline", "cancel",
            {"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["git","status"]}},
            {"applyNetworkPolicyAmendment":{"network_policy_amendment":{"host":"example.com","action":"allow"}}},
            {"applyNetworkPolicyAmendment":{"network_policy_amendment":{"host":"example.com","action":"deny"}}}
        ]);
        let adapted = codex(
            "public-request".into(),
            "item/commandExecution/requestApproval",
            &json!({"turnId":"turn","availableDecisions":decisions}),
        )
        .unwrap();
        assert_eq!(adapted.request.body.choices().len(), 7);
        for (index, choice) in adapted.request.body.choices().iter().enumerate() {
            assert_eq!(
                adapted
                    .answers
                    .translate(
                        &adapted.request.body,
                        &Answer::Approval {
                            choice_id: choice.id.clone()
                        }
                    )
                    .unwrap(),
                json!({"decision":decisions[index]})
            );
            assert_ne!(choice.id, choice.label);
        }
        for choice in &adapted.request.body.choices()[4..] {
            assert!(!choice.description.contains("Amendment"));
        }
        let default = codex(
            "request".into(),
            "item/fileChange/requestApproval",
            &json!({"turnId":"turn"}),
        )
        .unwrap();
        assert_eq!(default.request.body.choices().len(), 4);
        for invalid in [
            json!([]),
            json!(["future-decision"]),
            json!({"not":"choices"}),
            json!([{"acceptWithExecpolicyAmendment":{"future":true}}]),
        ] {
            assert!(
                codex(
                    "request".into(),
                    "item/commandExecution/requestApproval",
                    &json!({"turnId":"turn","availableDecisions":invalid})
                )
                .is_err()
            );
        }
        assert!(
            codex(
                "request".into(),
                "future/approval",
                &json!({"turnId":"turn"})
            )
            .is_err()
        );
    }

    #[test]
    fn permissions_keep_turn_and_session_scope_without_granting_unknown_permissions() {
        let path = std::env::temp_dir().to_string_lossy().into_owned();
        let permissions = json!({"network":{"enabled":true},"fileSystem":{"entries":[{"access":"write","path":{"type":"path","path":path}}]}});
        let adapted = codex(
            "request".into(),
            "item/permissions/requestApproval",
            &json!({"turnId":"turn","permissions":permissions}),
        )
        .unwrap();
        let choices = adapted.request.body.choices();
        assert_eq!(choices.len(), 3);
        for (choice, scope) in [(&choices[0], "turn"), (&choices[1], "session")] {
            assert_eq!(
                adapted
                    .answers
                    .translate(
                        &adapted.request.body,
                        &Answer::Permission {
                            choice_id: choice.id.clone()
                        }
                    )
                    .unwrap(),
                json!({"permissions":permissions,"scope":scope})
            );
            assert!(choice.description.contains(&path));
        }
        for permissions in [
            json!({"future":true}),
            json!({"future":null}),
            json!({"fileSystem":{"future":null}}),
            json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"future","path":"/workspace"}}]}}),
        ] {
            let adapted = codex(
                "request".into(),
                "item/permissions/requestApproval",
                &json!({"turnId":"turn","permissions":permissions}),
            )
            .unwrap();
            let choices = adapted.request.body.choices();
            assert_eq!(choices.len(), 1);
            assert_eq!(
                adapted
                    .answers
                    .translate(
                        &adapted.request.body,
                        &Answer::Permission {
                            choice_id: choices[0].id.clone()
                        }
                    )
                    .unwrap(),
                json!({"permissions":{},"scope":"turn"})
            );
        }
    }

    #[test]
    fn claude_questions_restore_native_labels_only_at_the_answer_boundary() {
        let input = json!({"questions":[{"question":"Choose colors","header":"colors","multiSelect":true,"options":[{"label":"Red"},{"label":"Blue"}]}],"otherInput":"retained"});
        let adapted = claude(
            "public-request".into(),
            &"turn".into(),
            &json!({"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":input}),
        )
        .unwrap();
        let RequestBody::Question { questions } = &adapted.request.body else {
            panic!("question")
        };
        let question = &questions[0];
        assert_ne!(question.id, question.prompt);
        assert!(question.multiple && question.allow_free_text);
        let answers = [(
            question.id.clone(),
            QuestionAnswer::MultipleChoices {
                choice_ids: question
                    .choices
                    .iter()
                    .map(|choice| choice.id.clone())
                    .collect(),
            },
        )]
        .into();
        let result = adapted
            .answers
            .translate(&adapted.request.body, &Answer::Questions { answers })
            .unwrap();
        let mut expected = input.clone();
        expected["answers"] = json!({"Choose colors":"Red, Blue"});
        assert_eq!(result, json!({"behavior":"allow","updatedInput":expected}));
        assert!(claude("request".into(), &"turn".into(), &json!({"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"same"},{"question":"same"}]}})).is_err());
        assert!(
            claude(
                "request".into(),
                &"turn".into(),
                &json!({"subtype":"request_user_dialog","dialog_kind":"future"})
            )
            .is_err()
        );
    }

    #[test]
    fn elicitation_retains_titled_choices_defaults_and_constraints_without_an_active_turn() {
        let schema = json!({"type":"object","required":["name","count","flag","one","many"],"properties":{
            "name":{"type":"string","minLength":1,"maxLength":20,"default":"name"},
            "count":{"type":"integer","minimum":1,"maximum":5,"default":3},
            "flag":{"type":"boolean","default":false},
            "one":{"type":"string","oneOf":[{"const":"a","title":"Alpha"},{"const":"b","title":"Beta"}],"default":"a"},
            "many":{"type":"array","items":{"type":"string","enum":["a","b"],"enumNames":["Alpha","Beta"]},"minItems":1,"maxItems":2,"default":["a"]}
        }});
        let adapted = codex(
            "request".into(),
            "mcpServer/elicitation/request",
            &json!({"turnId":null,"serverName":"mcp","mode":"form","requestedSchema":schema}),
        )
        .unwrap();
        assert_eq!(adapted.request.target, RequestTarget::Session);
        let RequestBody::Elicitation {
            input: ElicitationInput::Form { fields },
            ..
        } = &adapted.request.body
        else {
            panic!("form")
        };
        assert_eq!(fields.len(), 5);
        assert!(fields.iter().all(|field| field.required));
        let values = json!({"name":"chosen","count":4,"flag":true,"one":"b","many":["a","b"]});
        assert_eq!(
            adapted
                .answers
                .translate(
                    &adapted.request.body,
                    &Answer::Elicitation {
                        action: ElicitationAnswer::Accept {
                            values: values.clone()
                        }
                    }
                )
                .unwrap(),
            json!({"action":"accept","content":values})
        );
        for property in [
            json!({"type":"object","properties":{}}),
            json!({"type":"string","pattern":".*"}),
            json!({"type":"integer","minimum":4,"maximum":1}),
            json!({"type":"string","minLength":"one"}),
            json!({"type":"array","items":{"type":"string","pattern":"a","enum":["a"]}}),
        ] {
            assert!(
                form_fields(&json!({"type":"object","properties":{"field":property}})).is_err()
            );
        }
        let url = claude("request".into(), &"turn".into(), &json!({"subtype":"elicitation","mcp_server_name":"mcp","mode":"url","url":"https://example.com/login"})).unwrap();
        assert_eq!(url.request.target, RequestTarget::Session);
        assert_eq!(
            url.answers
                .translate(
                    &url.request.body,
                    &Answer::Elicitation {
                        action: ElicitationAnswer::Cancel
                    }
                )
                .unwrap(),
            json!({"action":"cancel","content":null})
        );
    }
}
