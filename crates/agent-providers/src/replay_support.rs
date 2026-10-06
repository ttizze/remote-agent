//! Reference transcript helpers shared by the provider replays and the
//! runtime replays of the same transcripts.
use crate::*;
use serde_json::json;
use std::path::Path;

/// One recorded reference transcript of `scenario`.
pub fn transcript(scenario: &str, driver: Driver) -> Vec<Value> {
    let file = match driver {
        Driver::Codex => "codex",
        Driver::Claude => "claude",
    };
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/fixtures/{scenario}/{file}_transcript.ndjson")),
    )
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}
/// T3 replay.ts normalization: values that vary per machine or recorder.
pub fn normalized_frame(frame: &Value, ignored_config: &[String]) -> Value {
    fn walk(value: &Value) -> Value {
        match value {
            Value::String(text) if text.starts_with("Context handoff (") => {
                match (text.find(":\n"), text.find("\n\nUser message:\n")) {
                    (Some(header), Some(user)) if header < user => Value::String(format!(
                        "{}<dynamic-summary>{}",
                        &text[..header + 2],
                        &text[user..]
                    )),
                    _ => value.clone(),
                }
            }
            Value::Array(values) => Value::Array(values.iter().map(walk).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), walk(v))).collect())
            }
            _ => value.clone(),
        }
    }
    let mut frame = walk(frame);
    let method = string(&frame, "method");
    let params = &mut frame["params"];
    match method.as_str() {
        "initialize" => {
            if params["clientInfo"].is_object() {
                params["clientInfo"]["version"] = json!("<ignored>");
            }
        }
        "turn/start" => {
            if let Some(params) = params.as_object_mut() {
                if params.get("approvalPolicy") == Some(&json!("never")) {
                    params.remove("approvalPolicy");
                }
                if params["sandboxPolicy"]["type"] == "dangerFullAccess" {
                    params.remove("sandboxPolicy");
                }
                if params
                    .get("collaborationMode")
                    .is_some_and(|mode| mode["settings"].is_object())
                {
                    params["collaborationMode"]["settings"]["developer_instructions"] =
                        json!("<ignored>");
                }
            }
        }
        "thread/start" | "thread/resume" | "thread/fork" => {
            if let Some(params) = params.as_object_mut() {
                params.remove("cwd");
                params.remove("model");
                if let Some(config) = params.get_mut("config").and_then(Value::as_object_mut) {
                    config.remove("mcp_servers");
                    for key in ignored_config {
                        config.remove(key);
                    }
                }
            }
        }
        _ => {}
    }
    frame
}
/// Claude CLI frames in the SDK vocabulary the reference transcripts record.
pub fn sdk_frame(frame: &Value) -> Option<Value> {
    match string(frame, "type").as_str() {
        "user" => Some(json!({"type":"prompt.offer","message":frame})),
        "control_request" if frame["request"]["subtype"] == "interrupt" => {
            Some(json!({"type":"query.interrupt"}))
        }
        "control_request" if frame["request"]["subtype"] == "initialize" => None,
        "control_response" if frame["response"]["subtype"] == "success" => {
            Some(json!({"type":"permission.response","result":frame["response"]["response"]}))
        }
        "control_response" => None,
        _ => Some(frame.clone()),
    }
}
/// The kind a replay expectation names a request by.
pub fn request_kind(request: &Request) -> &str {
    match &request.body {
        RequestBody::Approval { kind, .. } => kind,
        RequestBody::Questions { .. } => "user_input",
    }
}
/// Whether `state` holds an item of the matching kind.
pub fn has_item(state: &State, kind: fn(&ItemKind) -> bool) -> bool {
    state.items.iter().any(|item| kind(&item.kind))
}
