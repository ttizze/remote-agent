use crate::*;
use serde_json::{Value, json};

fn persistence(value: &str) -> Option<ApprovalDecision> {
    let value = value.to_lowercase();
    if value.contains("session") {
        Some(ApprovalDecision::AcceptForSession)
    } else if ["always", "permanent", "forever", "persistent"]
        .iter()
        .any(|word| value.contains(word))
    {
        Some(ApprovalDecision::AcceptAlways)
    } else {
        None
    }
}
fn nullable_string(value: Option<&Value>) -> bool {
    value.is_none_or(|value| value.is_null() || value.is_string())
}
fn string_array(value: Option<&Value>) -> bool {
    value.is_none_or(|value| {
        value.is_null()
            || value
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string))
    })
}
fn form(payload: &Value) -> Option<&Value> {
    let form = payload
        .get("requestedSchema")
        .filter(|form| payload["mode"] != "url" && form.is_object())?;
    if !string_array(form.get("required")) {
        return None;
    }
    if let Some(properties) = form.get("properties") {
        let properties = properties.as_object()?;
        for field in properties.values() {
            if !field.is_object()
                || ["type", "title", "description"]
                    .iter()
                    .any(|key| !nullable_string(field.get(key)))
                || !string_array(field.get("enum"))
                || !string_array(field.get("enumNames"))
            {
                return None;
            }
            if let Some(options) = field.get("oneOf").filter(|value| !value.is_null())
                && !options.as_array().is_some_and(|options| {
                    options.iter().all(|option| {
                        option["const"].is_string() && nullable_string(option.get("title"))
                    })
                })
            {
                return None;
            }
        }
    }
    Some(form)
}
fn metadata(payload: &Value) -> Option<&Value> {
    let meta = payload.get("_meta").filter(|meta| meta.is_object())?;
    if [
        "app",
        "app_name",
        "appName",
        "connector_name",
        "connectorName",
    ]
    .iter()
    .any(|key| !nullable_string(meta.get(key)))
        || meta
            .get("allowPersistentApproval")
            .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return None;
    }
    if meta
        .get("persist")
        .is_some_and(|value| !value.is_string() && !value.is_null() && !string_array(Some(value)))
    {
        return None;
    }
    for (key, fields) in [
        ("target", ["app", "name"]),
        ("tool_params", ["app", "app_name"]),
    ] {
        if let Some(nested) = meta.get(key).filter(|value| !value.is_null())
            && (!nested.is_object() || fields.iter().any(|key| !nullable_string(nested.get(key))))
        {
            return None;
        }
    }
    Some(meta)
}
fn field_options(field: &Value) -> Vec<(String, Option<String>)> {
    if let Some(options) = field["oneOf"].as_array() {
        options
            .iter()
            .filter_map(|option| {
                option["const"]
                    .as_str()
                    .map(|value| (value.into(), optional(option, "title")))
            })
            .collect()
    } else {
        field["enum"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, value)| {
                value.as_str().map(|value| {
                    (
                        value.into(),
                        field["enumNames"][index].as_str().map(str::to_owned),
                    )
                })
            })
            .collect()
    }
}
fn persistence_field(key: &str, field: &Value) -> bool {
    persistence(key).is_some()
        || key.eq_ignore_ascii_case("persist")
        || persistence(&string(field, "title")).is_some()
        || persistence(&string(field, "description")).is_some()
}
pub fn mcp_elicitation_response(payload: &Value, decision: ApprovalDecision) -> Value {
    if matches!(
        decision,
        ApprovalDecision::Decline | ApprovalDecision::Cancel
    ) {
        return json!({"action":if decision==ApprovalDecision::Decline {"decline"} else {"cancel"}});
    }
    if payload["mode"] == "url" {
        return json!({"action":"decline"});
    }
    let persist = match decision {
        ApprovalDecision::AcceptForSession => Some("session"),
        ApprovalDecision::AcceptAlways => Some("always"),
        _ => None,
    };
    let form = form(payload);
    let mut content = serde_json::Map::new();
    for (key, field) in form
        .and_then(|form| form["properties"].as_object())
        .into_iter()
        .flatten()
    {
        let chosen = field_options(field).into_iter().find(|(value, _)| {
            if persist.is_some() {
                persistence(value) == Some(decision)
            } else {
                persistence(value).is_none()
                    && ["once", "accept", "approve", "allow"]
                        .iter()
                        .any(|word| value.to_lowercase().contains(word))
            }
        });
        if let Some((value, _)) = chosen {
            content.insert(key.clone(), json!(value));
        } else if field["type"] == "boolean" && persistence_field(key, field) {
            content.insert(
                key.clone(),
                json!(decision == ApprovalDecision::AcceptAlways),
            );
        } else if let Some(default) = field.get("default").filter(|value| !value.is_null()) {
            content.insert(key.clone(), default.clone());
        }
    }
    if form
        .and_then(|form| form["required"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|key| !content.contains_key(key))
    {
        return json!({"action":"decline"});
    }
    let mut response = json!({"action":"accept"});
    if let Some(persist) = persist {
        response["_meta"] = json!({"persist":persist});
    }
    if form.is_some() {
        response["content"] = json!(content);
    }
    response
}
pub fn describe_mcp_elicitation(payload: &Value) -> (String, Vec<ApprovalOption>) {
    let empty = Value::Null;
    let meta = metadata(payload).unwrap_or(&empty);
    let app_name = [
        meta.get("app_name"),
        meta.get("appName"),
        meta.get("app"),
        meta["target"].get("app"),
        meta["target"].get("name"),
        meta["tool_params"].get("app_name"),
        meta["tool_params"].get("app"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .map(str::to_owned)
    .or_else(|| {
        let message = string(payload, "message");
        let prefix = "Allow ChatGPT to use ";
        (message.to_lowercase().starts_with(&prefix.to_lowercase()) && message.ends_with('?'))
            .then(|| message[prefix.len()..message.len() - 1].to_owned())
    })
    .or_else(|| optional(meta, "connector_name"))
    .or_else(|| optional(meta, "connectorName"))
    .unwrap_or_else(|| string(payload, "serverName"));
    let mut choices = std::collections::BTreeMap::new();
    let persist = meta.get("persist").cloned().unwrap_or(Value::Null);
    for value in persist.as_str().into_iter().chain(
        persist
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    ) {
        if let Some(decision) = persistence(value) {
            choices.insert(
                if decision == ApprovalDecision::AcceptForSession {
                    0
                } else {
                    1
                },
                String::new(),
            );
        }
    }
    if meta["allowPersistentApproval"] == true {
        choices.insert(1, String::new());
    }
    for (key, field) in form(payload)
        .and_then(|form| form["properties"].as_object())
        .into_iter()
        .flatten()
    {
        for (value, label) in field_options(field) {
            if let Some(decision) = persistence(&value) {
                choices.insert(
                    if decision == ApprovalDecision::AcceptForSession {
                        0
                    } else {
                        1
                    },
                    label.unwrap_or_default(),
                );
            }
        }
        if field["type"] == "boolean" && persistence_field(key, field) {
            choices.insert(1, string(field, "title"));
        }
    }
    let mut options = vec![
        ApprovalOption {
            decision: ApprovalDecision::Cancel,
            label: "Cancel".into(),
        },
        ApprovalOption {
            decision: ApprovalDecision::Decline,
            label: "Decline".into(),
        },
    ];
    for (key, decision, fallback) in [
        (
            0,
            ApprovalDecision::AcceptForSession,
            "Always allow this session",
        ),
        (1, ApprovalDecision::AcceptAlways, "Always allow"),
    ] {
        if let Some(label) = choices.get(&key)
            && mcp_elicitation_response(payload, decision)["action"] == "accept"
        {
            options.push(ApprovalOption {
                decision,
                label: if label.is_empty() {
                    fallback.into()
                } else {
                    label.clone()
                },
            });
        }
    }
    options.push(ApprovalOption {
        decision: ApprovalDecision::Accept,
        label: "Approve".into(),
    });
    (app_name, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"mode":"form","message":"Allow ChatGPT to use Safari?","serverName":"computer-use","threadId":"provider-thread-1","turnId":"turn-1","_meta":{"app_name":"Safari","persist":["session","always"]},"requestedSchema":{"type":"object","properties":{"approval":{"type":"string","oneOf":[{"const":"once","title":"Allow once"},{"const":"session","title":"Allow for this session"},{"const":"always","title":"Always allow Safari"}]}},"required":["approval"]}})
    }
    #[test]
    fn advertised_app_and_approval_choices_keep_the_original_responses() {
        let request = request();
        let (app, options) = describe_mcp_elicitation(&request);
        assert_eq!(app, "Safari");
        assert_eq!(
            options,
            vec![
                ApprovalOption {
                    decision: ApprovalDecision::Cancel,
                    label: "Cancel".into()
                },
                ApprovalOption {
                    decision: ApprovalDecision::Decline,
                    label: "Decline".into()
                },
                ApprovalOption {
                    decision: ApprovalDecision::AcceptForSession,
                    label: "Allow for this session".into()
                },
                ApprovalOption {
                    decision: ApprovalDecision::AcceptAlways,
                    label: "Always allow Safari".into()
                },
                ApprovalOption {
                    decision: ApprovalDecision::Accept,
                    label: "Approve".into()
                }
            ]
        );
        let mut without_meta = request.clone();
        without_meta.as_object_mut().unwrap().remove("_meta");
        assert_eq!(describe_mcp_elicitation(&without_meta).0, "Safari");
        for (decision, expected) in [
            (
                ApprovalDecision::Accept,
                json!({"action":"accept","content":{"approval":"once"}}),
            ),
            (
                ApprovalDecision::AcceptForSession,
                json!({"action":"accept","_meta":{"persist":"session"},"content":{"approval":"session"}}),
            ),
            (
                ApprovalDecision::AcceptAlways,
                json!({"action":"accept","_meta":{"persist":"always"},"content":{"approval":"always"}}),
            ),
            (ApprovalDecision::Decline, json!({"action":"decline"})),
            (ApprovalDecision::Cancel, json!({"action":"cancel"})),
        ] {
            assert_eq!(mcp_elicitation_response(&request, decision), expected);
        }
    }
    #[test]
    fn boolean_and_nullable_forms_keep_the_reference_persistence_choices() {
        let mut boolean = request();
        boolean["_meta"] = json!({"app_name":"Safari"});
        boolean["requestedSchema"] = json!({"type":"object","properties":{"always":{"type":"boolean","title":"Always allow Safari"}}});
        assert!(
            describe_mcp_elicitation(&boolean)
                .1
                .iter()
                .any(|option| option.decision == ApprovalDecision::AcceptAlways)
        );
        assert_eq!(
            mcp_elicitation_response(&boolean, ApprovalDecision::AcceptAlways),
            json!({"action":"accept","_meta":{"persist":"always"},"content":{"always":true}})
        );
        let mut nullable = request();
        nullable["_meta"] = json!({"app_name":null,"appName":"Safari","connector_name":null,"persist":null,"target":null,"tool_params":null});
        nullable["requestedSchema"] = json!({"type":"object","properties":{"approval":{"type":"string","title":null,"description":null,"default":null,"enum":["once","always"],"enumNames":null}},"required":["approval"]});
        assert_eq!(describe_mcp_elicitation(&nullable).0, "Safari");
        assert!(
            describe_mcp_elicitation(&nullable)
                .1
                .iter()
                .any(|option| option.decision == ApprovalDecision::AcceptAlways)
        );
        assert_eq!(
            mcp_elicitation_response(&nullable, ApprovalDecision::AcceptAlways),
            json!({"action":"accept","_meta":{"persist":"always"},"content":{"approval":"always"}})
        );
    }
    #[test]
    fn unsupported_required_answers_and_url_flows_are_declined() {
        let mut request = request();
        request["requestedSchema"] = json!({"type":"object","properties":{"email":{"type":"string","format":"email"}},"required":["email"]});
        assert_eq!(
            mcp_elicitation_response(&request, ApprovalDecision::Accept),
            json!({"action":"decline"})
        );
        assert_eq!(
            mcp_elicitation_response(
                &json!({"mode":"url","url":"https://example.com/authorize"}),
                ApprovalDecision::Accept
            ),
            json!({"action":"decline"})
        );
        request["requestedSchema"] = json!({"type":"object","properties":{"approval":{"type":"string","enum":["once"]}},"required":["approval"]});
        assert_eq!(
            describe_mcp_elicitation(&request)
                .1
                .iter()
                .map(|option| option.decision)
                .collect::<Vec<_>>(),
            vec![
                ApprovalDecision::Cancel,
                ApprovalDecision::Decline,
                ApprovalDecision::Accept
            ]
        );
    }
}
