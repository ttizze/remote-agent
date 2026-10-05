//! Pure projection of persisted Codex records into the same typed values used
//! by the live adapter. Reference: openai/codex 7f892275, thread_history.rs and
//! thread_history_projection.rs. Input JSON values are never changed.
use crate::host_rpc::native;
use agent_protocol::{
    execution::{ItemStatus, TurnStatus},
    items::ItemBody,
    models::{Item, Turn},
};
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
pub(super) struct Projection {
    turns: Vec<Turn>,
    current: Option<usize>,
    explicit: bool,
    paginated: bool,
    next_item: usize,
    next_turn: usize,
    positions: HashMap<(usize, String), usize>,
    model: Option<String>,
}

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap_or_default().to_owned()
}

fn camel_case(value: &str) -> String {
    let mut words = value.split('_');
    let mut output = words.next().unwrap_or_default().to_owned();
    for word in words {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            output.extend(first.to_uppercase());
        }
        output.extend(chars);
    }
    output
}

fn source_error(value: &Value) -> agent_protocol::execution::ExecutionError {
    let mut value = value.clone();
    let info = &value["codex_error_info"];
    let info = if let Some(code) = info.as_str() {
        json!(camel_case(code))
    } else if let Some((code, details)) = info.as_object().and_then(|fields| fields.iter().next()) {
        let mut details = details.clone();
        if let Some(status) = details.get("http_status_code").cloned() {
            details["httpStatusCode"] = status;
        }
        json!({camel_case(code):details})
    } else {
        Value::Null
    };
    value["codexErrorInfo"] = info;
    native::codex_error(&value, false)
}

fn duration(value: &Value) -> Option<u64> {
    let seconds = value["secs"].as_u64()?;
    seconds
        .checked_mul(1000)?
        .checked_add(value["nanos"].as_u64().unwrap_or_default() / 1_000_000)
}

fn command(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|part| {
                if !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_/-.,:@".contains(&byte))
                {
                    part.to_owned()
                } else {
                    format!("'{}'", part.replace('\'', "'\\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn changes(value: &Value) -> Value {
    Value::Array(value.as_object().into_iter().flat_map(|changes| changes.iter()).map(|(path, change)| {
        let kind = change["type"].as_str().unwrap_or("unknown");
        let content = if kind == "update" { &change["unified_diff"] } else { &change["content"] };
        json!({"path":path,"kind":{"type":kind,"movePath":change["move_path"]},"diff":content})
    }).collect())
}

fn user_content(value: &Value) -> Vec<Value> {
    let mut content = vec![json!({"type":"text","text":value["message"]})];
    let images = value["images"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let files = value["file_ids"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let order = value["image_order"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let complete = !order.is_empty()
        && order.iter().filter(|kind| **kind == "inline").count() == images.len()
        && order.iter().filter(|kind| **kind == "file").count() == files.len()
        && order.len() == images.len() + files.len();
    if complete {
        let mut images = images.iter();
        let mut files = files.iter();
        for kind in order {
            content.push(if kind == "inline" {
                json!({"type":"image","url":images.next().unwrap()})
            } else {
                json!({"type":"image","fileId":files.next().unwrap()})
            });
        }
    } else {
        content.extend(
            images
                .iter()
                .map(|image| json!({"type":"image","url":image})),
        );
        content.extend(files.iter().map(|id| json!({"type":"image","fileId":id})));
    }
    for (field, kind, key) in [
        ("local_images", "localImage", "path"),
        ("audio", "audio", "url"),
        ("local_audio", "localAudio", "path"),
    ] {
        content.extend(
            value[field]
                .as_array()
                .into_iter()
                .flatten()
                .map(|source| json!({"type":kind,key:source})),
        );
    }
    content
}

fn status(value: &Value, completed: bool) -> Value {
    match value.as_str() {
        Some("in_progress" | "inProgress") => json!("inProgress"),
        Some(status) => json!(status),
        None => json!(if completed { "completed" } else { "inProgress" }),
    }
}

/// Only schema-owned fields are renamed. Tool arguments, results, and opaque
/// extension content retain their original keys and values.
fn canonical(value: &Value, completed: bool) -> Result<Item> {
    let kind = value["type"]
        .as_str()
        .context("Codex activity type is missing")?;
    let kind = match kind {
        "UserMessage" => "userMessage",
        "AgentMessage" => "agentMessage",
        "Reasoning" => "reasoning",
        "CommandExecution" => "commandExecution",
        "FileChange" => "fileChange",
        "McpToolCall" => "mcpToolCall",
        "DynamicToolCall" => "dynamicToolCall",
        "CollabAgentToolCall" => "collabAgentToolCall",
        "SubAgentActivity" => "subAgentActivity",
        "WebSearch" => "webSearch",
        "ImageView" => "imageView",
        "ImageGeneration" => "imageGeneration",
        "Plan" => "plan",
        "ContextCompaction" => "contextCompaction",
        "HookPrompt" => "hookPrompt",
        "EnteredReviewMode" => "enteredReviewMode",
        "ExitedReviewMode" => "exitedReviewMode",
        other => other,
    };
    let mut item = value.clone();
    item["type"] = json!(kind);
    item["status"] = status(&value["status"], completed);
    for (source, target) in [
        ("client_id", "clientId"),
        ("aggregated_output", "aggregatedOutput"),
        ("exit_code", "exitCode"),
        ("summary_text", "summary"),
        ("raw_content", "content"),
        ("sender_thread_id", "senderThreadId"),
        ("receiver_thread_ids", "receiverThreadIds"),
        ("agents_states", "agentsStates"),
        ("reasoning_effort", "reasoningEffort"),
        ("content_items", "contentItems"),
        ("mcp_app_resource_uri", "mcpAppResourceUri"),
        ("plugin_id", "pluginId"),
        ("saved_path", "savedPath"),
        ("revised_prompt", "revisedPrompt"),
    ] {
        if let Some(field) = value.get(source) {
            item[target] = field.clone();
        }
    }
    if let Some(milliseconds) = duration(&value["duration"]) {
        item["durationMs"] = json!(milliseconds);
    }
    match kind {
        "userMessage" => {
            item["content"] = Value::Array(
                value["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|part| {
                        let mut part = part.clone();
                        if part["type"] == "local_image" {
                            part["type"] = json!("localImage");
                        }
                        if let Some(url) = part.get("image_url").cloned() {
                            part["url"] = url;
                        }
                        if let Some(id) = part.get("file_id").cloned() {
                            part["fileId"] = id;
                        }
                        part
                    })
                    .collect(),
            );
        }
        "agentMessage" => {
            item["text"] = json!(
                value["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            )
        }
        "commandExecution" => item["command"] = json!(command(&value["command"])),
        "fileChange" => {
            item["changes"] = changes(&value["changes"]);
            item["output"] = json!(format!(
                "{}{}",
                text(value, "stdout"),
                text(value, "stderr")
            ));
        }
        "dynamicToolCall" => {
            item["contentItems"] = Value::Array(
                value["content_items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|content| {
                        let mut content = content.clone();
                        content["type"] =
                            json!(match content["type"].as_str().unwrap_or_default() {
                                "input_text" => "inputText",
                                "input_image" => "inputImage",
                                other => other,
                            });
                        if let Some(url) = content.get("image_url").cloned() {
                            content["imageUrl"] = url;
                        }
                        content
                    })
                    .collect(),
            );
        }
        "collabAgentToolCall" => {
            item["tool"] = json!(camel_case(&text(value, "tool")));
            let mut states = serde_json::Map::new();
            for (id, state) in value["agents_states"]
                .as_object()
                .into_iter()
                .flat_map(|states| states.iter())
            {
                let (status, message) = if let Some(status) = state.as_str() {
                    (camel_case(status), Value::Null)
                } else if let Some((status, message)) =
                    state.as_object().and_then(|fields| fields.iter().next())
                {
                    (camel_case(status), message.clone())
                } else {
                    ("unknown".into(), Value::Null)
                };
                states.insert(id.clone(), json!({"status":status,"message":message}));
            }
            item["agentsStates"] = Value::Object(states);
        }
        "subAgentActivity" => {
            item["tool"] = value["kind"].clone();
            item["receiverThreadIds"] = json!([value["agent_thread_id"]]);
        }
        "enteredReviewMode" => item["review"] = value["user_facing_hint"].clone(),
        "exitedReviewMode" => {
            item["review"] = value["review_output"]["overall_correctness_explanation"].clone()
        }
        _ => {}
    }
    native::codex_item(item).map_err(|_| anyhow::anyhow!("Codex activity has invalid fields"))
}

impl Projection {
    pub(super) fn new(paginated: bool) -> Self {
        Self {
            paginated,
            ..Default::default()
        }
    }
    fn open_turn(&mut self, id: String, explicit: bool, started_at: Option<f64>) -> usize {
        let index = self.turns.len();
        self.turns.push(Turn {
            id: id.into(),
            status: if explicit {
                TurnStatus::Running
            } else {
                TurnStatus::Completed
            },
            started_at,
            items: Some(Vec::new()),
            ..Default::default()
        });
        self.current = Some(index);
        self.explicit = explicit;
        index
    }

    fn active(&mut self) -> usize {
        if let Some(current) = self.current {
            return current;
        }
        self.next_turn += 1;
        self.open_turn(format!("codex-import-turn-{}", self.next_turn), false, None)
    }

    fn target(&mut self, id: Option<&str>) -> Result<usize> {
        match id.filter(|id| !id.is_empty()) {
            Some(id) => self
                .turns
                .iter()
                .position(|turn| turn.id.as_str() == id)
                .context("Codex activity references an unknown turn"),
            None => Ok(self.active()),
        }
    }

    fn push(&mut self, turn: usize, item: Item) {
        let items = self.turns[turn].items.as_mut().unwrap();
        let key = (turn, item.id.to_string());
        if let Some(position) = self.positions.get(&key) {
            items[*position] = Arc::new(item);
        } else {
            self.positions.insert(key, items.len());
            items.push(Arc::new(item));
        }
    }

    fn item(&mut self, payload: &Value, mut value: Value, completed: bool) -> Result<()> {
        let id = payload["call_id"]
            .as_str()
            .or_else(|| payload["id"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                self.next_item += 1;
                format!("codex-import-item-{}", self.next_item)
            });
        value["id"] = json!(id);
        value["status"] = status(payload.get("status").unwrap_or(&value["status"]), completed);
        let turn = self.target(payload["turn_id"].as_str())?;
        let item = native::codex_item(value)
            .map_err(|_| anyhow::anyhow!("Codex activity has invalid fields"))?;
        self.push(turn, item);
        Ok(())
    }

    pub(super) fn apply(&mut self, value: &Value) -> Result<()> {
        match value["type"].as_str() {
            Some("session_meta") => {}
            Some("turn_context") => {
                if let Some(model) = value["payload"]["model"].as_str() {
                    self.model = Some(model.into());
                }
            }
            Some("event_msg") => self.event(&value["payload"])?,
            Some("compacted") => {
                let turn = self.active();
                self.next_item += 1;
                self.push(
                    turn,
                    Item::new(
                        format!("codex-import-item-{}", self.next_item).into(),
                        ItemStatus::Completed,
                        ItemBody::Compaction {},
                    ),
                );
            }
            Some(
                "response_item"
                | "token_usage_record"
                | "world_state"
                | "retained_context"
                | "security_risk_score"
                | "realtime_item"
                | "inter_agent_communication"
                | "inter_agent_communication_metadata",
            ) => {}
            _ => bail!("Codex history contains an unsupported record"),
        }
        Ok(())
    }

    fn event(&mut self, payload: &Value) -> Result<()> {
        match payload["type"].as_str() {
            Some("task_started" | "turn_started") => {
                let id = payload["turn_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("Codex turn identity is missing")?;
                ensure!(
                    !self.turns.iter().any(|turn| turn.id.as_str() == id),
                    "Codex turn identity is repeated"
                );
                self.open_turn(id.into(), true, payload["started_at"].as_f64());
            }
            Some("task_complete" | "turn_complete" | "turn_aborted") => {
                let target = payload["turn_id"]
                    .as_str()
                    .and_then(|id| self.turns.iter().position(|turn| turn.id.as_str() == id))
                    .or(self.current);
                if let Some(target) = target {
                    let turn = &mut self.turns[target];
                    if !payload["error"].is_null() {
                        turn.error = Some(source_error(&payload["error"]));
                        turn.status = TurnStatus::Failed;
                    }
                    if payload["type"] == "turn_aborted" {
                        turn.status = TurnStatus::Interrupted;
                    } else if matches!(turn.status, TurnStatus::Running | TurnStatus::Completed) {
                        turn.status = TurnStatus::Completed;
                    }
                    turn.duration_ms = payload["duration_ms"].as_u64();
                    turn.completed_at_ms = payload["completed_at"]
                        .as_u64()
                        .and_then(|seconds| seconds.checked_mul(1000));
                    if self.current == Some(target) {
                        self.current = None;
                        self.explicit = false;
                    }
                }
            }
            Some("thread_rolled_back") => {
                let count = payload["num_turns"]
                    .as_u64()
                    .context("Codex rollback count is invalid")?;
                self.turns.truncate(
                    self.turns
                        .len()
                        .saturating_sub(usize::try_from(count).unwrap_or(usize::MAX)),
                );
                self.positions
                    .retain(|(turn, _), _| *turn < self.turns.len());
                self.current = None;
                self.explicit = false;
            }
            Some("item_started" | "item_completed") => {
                let kind = payload["item"]["type"].as_str().unwrap_or_default();
                if self.paginated
                    || !matches!(
                        kind,
                        "UserMessage"
                            | "AgentMessage"
                            | "Reasoning"
                            | "WebSearch"
                            | "ImageView"
                            | "ImageGeneration"
                            | "FileChange"
                            | "McpToolCall"
                            | "ContextCompaction"
                    )
                {
                    let turn = self.target(payload["turn_id"].as_str())?;
                    self.push(
                        turn,
                        canonical(&payload["item"], payload["type"] == "item_completed")?,
                    );
                }
            }
            Some("error") => {
                let info = &payload["codex_error_info"];
                if let Some(turn) = self.current
                    && info.as_str() != Some("thread_rollback_failed")
                    && info.get("active_turn_not_steerable").is_none()
                {
                    self.turns[turn].error = Some(source_error(payload));
                    self.turns[turn].status = TurnStatus::Failed;
                }
            }
            Some("token_count" | "hook_started" | "hook_completed" | "thread_settings_applied") => {
            }
            Some(_) if !self.paginated => self.legacy_event(payload)?,
            Some(_) => {}
            None => bail!("Codex history event type is missing"),
        }
        Ok(())
    }

    fn legacy_event(&mut self, payload: &Value) -> Result<()> {
        match payload["type"].as_str() {
            Some("user_message") => {
                if !self.explicit { self.current = None; }
                self.item(payload, json!({"type":"userMessage","clientId":payload["client_id"],"content":user_content(payload)}), true)?;
            }
            Some("agent_message") => { if !text(payload, "message").is_empty() { self.item(payload, json!({"type":"agentMessage","text":payload["message"],"phase":payload["phase"]}), true)?; } }
            Some("agent_reasoning" | "agent_reasoning_raw_content") => {
                let raw = payload["type"] == "agent_reasoning_raw_content";
                let turn = self.active();
                if let Some(item) = self.turns[turn].items.as_mut().unwrap().last_mut()
                    && let ItemBody::Reasoning { content, summary } = Arc::make_mut(item).body_mut() {
                    if raw { content.push(text(payload,"text")); } else { summary.push(text(payload,"text")); }
                } else { self.item(payload, json!({"type":"reasoning","content":if raw {vec![text(payload,"text")]} else {vec![]},"summary":if raw {vec![]} else {vec![text(payload,"text")]} }), true)?; }
            }
            Some("exec_command_begin" | "exec_command_end") => self.item(payload, json!({"type":"commandExecution","command":command(&payload["command"]),"cwd":payload["cwd"],"aggregatedOutput":payload["aggregated_output"],"exitCode":payload["exit_code"]}), payload["type"] == "exec_command_end")?,
            Some("patch_apply_begin" | "patch_apply_end" | "apply_patch_approval_request") => self.item(payload, json!({"type":"fileChange","changes":changes(&payload["changes"]),"output":format!("{}{}",text(payload,"stdout"),text(payload,"stderr"))}), payload["type"] == "patch_apply_end")?,
            Some("mcp_tool_call_begin" | "mcp_tool_call_end") => {
                let result = payload["result"].get("Ok").cloned(); let error = payload["result"].get("Err").cloned();
                let mut item = json!({"type":"mcpToolCall","server":payload["invocation"]["server"],"tool":payload["invocation"]["tool"],"arguments":payload["invocation"]["arguments"],"result":result,"error":error,"mcpAppResourceUri":payload["mcp_app_resource_uri"],"pluginId":payload["plugin_id"],"durationMs":duration(&payload["duration"])});
                if error.is_some() { item["status"] = json!("failed"); }
                self.item(payload, item, payload["type"] == "mcp_tool_call_end")?;
            }
            Some("dynamic_tool_call_request" | "dynamic_tool_call_response") => {
                let mut item = payload.clone(); item["type"] = json!("DynamicToolCall"); item["id"] = payload["call_id"].clone();
                if payload["type"] == "dynamic_tool_call_response" && payload["success"] == false { item["status"] = json!("failed"); }
                let turn = self.target(payload["turn_id"].as_str())?;
                self.push(turn, canonical(&item, payload["type"] == "dynamic_tool_call_response")?);
            }
            Some("web_search_begin" | "web_search_end") => self.item(payload, json!({"type":"webSearch","query":payload["query"],"action":payload["action"]}), payload["type"] == "web_search_end")?,
            Some("view_image_tool_call") => self.item(payload, json!({"type":"imageView","path":payload["path"]}), true)?,
            Some("image_generation_begin" | "image_generation_end") => self.item(payload, json!({"type":"imageGeneration","savedPath":payload["saved_path"],"result":payload["result"],"revisedPrompt":payload["revised_prompt"]}), payload["type"] == "image_generation_end")?,
            Some("context_compacted") => self.item(payload, json!({"type":"contextCompaction"}), true)?,
            Some(_) => self.item(payload, payload.clone(), true)?,
            None => bail!("Codex history event type is missing"),
        }
        Ok(())
    }

    pub(super) fn finish(self) -> (Vec<Turn>, Option<String>) {
        (self.turns, self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        execution::ErrorCategory,
        items::{FileChangeKind, MessagePart},
    };

    fn apply(projection: &mut Projection, payload: Value) {
        projection
            .apply(&json!({"type":"event_msg","payload":payload}))
            .unwrap();
    }

    #[test]
    fn late_tool_completion_targets_its_original_turn_and_rollback_removes_item_positions() {
        let mut projection = Projection::new(true);
        for id in ["one", "two"] {
            apply(&mut projection, json!({"type":"task_started","turn_id":id}));
            apply(
                &mut projection,
                json!({"type":"item_started","turn_id":id,"item":{"type":"CommandExecution","id":"same-call","command":["echo",id],"status":"in_progress"}}),
            );
        }
        assert!(
            projection
                .turns
                .iter()
                .all(|turn| turn.status == TurnStatus::Running
                    && turn.items.as_ref().unwrap()[0].status == ItemStatus::Running)
        );
        apply(
            &mut projection,
            json!({"type":"item_completed","turn_id":"one","item":{"type":"CommandExecution","id":"same-call","command":["echo","one"],"aggregated_output":"original output","exit_code":0,"status":"completed"}}),
        );
        apply(
            &mut projection,
            json!({"type":"task_complete","turn_id":"one","duration_ms":30}),
        );
        assert_eq!(projection.current, Some(1));
        assert!(
            matches!(projection.turns[0].items.as_ref().unwrap()[0].body(), ItemBody::CommandExecution{output,..} if output=="original output")
        );
        apply(
            &mut projection,
            json!({"type":"thread_rolled_back","num_turns":1}),
        );
        apply(
            &mut projection,
            json!({"type":"item_completed","turn_id":"one","item":{"type":"CommandExecution","id":"same-call","command":["echo","one"],"aggregated_output":"late output after rollback","status":"completed"}}),
        );
        apply(
            &mut projection,
            json!({"type":"task_started","turn_id":"replacement"}),
        );
        apply(
            &mut projection,
            json!({"type":"item_completed","turn_id":"replacement","item":{"type":"AgentMessage","id":"same-call","content":[{"type":"Text","text":"replacement output"}]}}),
        );
        let (turns, _) = projection.finish();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert_eq!(turns[0].duration_ms, Some(30));
        assert_eq!(turns[0].items.as_ref().unwrap().len(), 1);
        assert!(
            matches!(turns[0].items.as_ref().unwrap()[0].body(),ItemBody::CommandExecution{output,..} if output=="late output after rollback")
        );
        assert!(
            matches!(turns[1].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText{text,..} if text=="replacement output")
        );
    }

    #[test]
    fn native_error_categories_survive_completion_and_nonterminal_errors_leave_the_turn_running() {
        let mut projection = Projection::new(true);
        apply(
            &mut projection,
            json!({"type":"task_started","turn_id":"one"}),
        );
        for info in [
            json!("thread_rollback_failed"),
            json!({"active_turn_not_steerable":{"reason":"not ready"}}),
        ] {
            apply(
                &mut projection,
                json!({"type":"error","message":"control rejected","codex_error_info":info}),
            );
            assert_eq!(projection.turns[0].status, TurnStatus::Running);
            assert!(projection.turns[0].error.is_none());
        }
        apply(
            &mut projection,
            json!({"type":"error","message":"busy","codex_error_info":{"http_connection_failed":{"http_status_code":429}}}),
        );
        apply(
            &mut projection,
            json!({"type":"task_complete","turn_id":"one"}),
        );
        assert_eq!(projection.turns[0].status, TurnStatus::Failed);
        assert_eq!(
            projection.turns[0].error.as_ref().unwrap().category,
            ErrorCategory::RateLimited
        );
        apply(
            &mut projection,
            json!({"type":"task_started","turn_id":"two"}),
        );
        apply(
            &mut projection,
            json!({"type":"turn_aborted","turn_id":"two","error":{"message":"interrupted","codex_error_info":"usage_limit_exceeded"}}),
        );
        let (turns, _) = projection.finish();
        assert_eq!(turns[1].status, TurnStatus::Interrupted);
        assert_eq!(
            turns[1].error.as_ref().unwrap().category,
            ErrorCategory::UsageLimit
        );
    }

    #[test]
    fn legacy_image_order_audio_file_changes_and_failed_dynamic_output_are_preserved() {
        let mut projection = Projection::new(false);
        apply(
            &mut projection,
            json!({"type":"user_message","message":"input","images":["https://fixture.invalid/image"],"file_ids":["file-fixture"],"image_order":["file","inline"],"local_images":["/fixture/image"],"audio":["https://fixture.invalid/audio"],"local_audio":["/fixture/audio"]}),
        );
        apply(
            &mut projection,
            json!({"type":"patch_apply_end","call_id":"patch","status":"completed","changes":{"old.rs":{"type":"update","unified_diff":"@@\n-before\n+after","move_path":"new.rs"},"add.rs":{"type":"add","content":"new file"}},"stdout":"patched","stderr":" warning"}),
        );
        let response = json!({"type":"dynamic_tool_call_response","call_id":"dynamic","tool":"fixture","namespace":"test","arguments":{"keep_key":true},"content_items":[{"type":"input_text","text":"error text"},{"type":"future_audio","original_key":true}],"success":false,"duration":{"secs":1,"nanos":2_000_000}});
        let original = response.clone();
        apply(&mut projection, response);
        let (turns, _) = projection.finish();
        let items = turns[0].items.as_ref().unwrap();
        let ItemBody::UserMessage { content, .. } = items[0].body() else {
            panic!("user message missing")
        };
        assert!(
            matches!(&content[1],MessagePart::Image{source} if source=="codex-file:file-fixture")
        );
        assert!(
            matches!(&content[2],MessagePart::Image{source} if source=="https://fixture.invalid/image")
        );
        assert_eq!(content.len(), 6);
        assert!(
            matches!(&content[4],MessagePart::Attachment{path,..} if path=="https://fixture.invalid/audio")
        );
        assert!(matches!(&content[5],MessagePart::Attachment{path,..} if path=="/fixture/audio"));
        let ItemBody::FileChange { changes, output } = items[1].body() else {
            panic!("file changes missing")
        };
        assert_eq!(output, "patched warning");
        assert!(changes.iter().any(|change| change.path == "old.rs"
            && change.diff.as_deref() == Some("@@\n-before\n+after")
            && change.kind
                == FileChangeKind::Update {
                    move_path: Some("new.rs".into())
                }));
        assert!(
            changes
                .iter()
                .any(|change| change.kind == FileChangeKind::Add
                    && change.diff.as_deref() == Some("new file"))
        );
        assert_eq!(items[2].status, ItemStatus::Failed);
        assert!(
            matches!(items[2].body(),ItemBody::ToolCall{arguments,result,success,duration_ms,..} if arguments==&original["arguments"] && result.as_ref().unwrap()[1]["original_key"]==true && *success==Some(false) && *duration_ms==Some(1002))
        );
    }

    #[test]
    fn legacy_materialized_items_skip_compatibility_duplicates_but_keep_new_activity() {
        let mut projection = Projection::new(false);
        apply(
            &mut projection,
            json!({"type":"task_started","turn_id":"turn"}),
        );
        apply(
            &mut projection,
            json!({"type":"user_message","message":"input"}),
        );
        apply(
            &mut projection,
            json!({"type":"agent_message","message":"answer"}),
        );
        for (id, kind, content) in [
            (
                "duplicate-user",
                "UserMessage",
                json!([{"type":"text","text":"input"}]),
            ),
            (
                "duplicate-answer",
                "AgentMessage",
                json!([{"type":"Text","text":"answer"}]),
            ),
        ] {
            apply(
                &mut projection,
                json!({"type":"item_completed","turn_id":"turn","item":{"id":id,"type":kind,"content":content}}),
            );
        }
        apply(
            &mut projection,
            json!({"type":"item_completed","turn_id":"turn","item":{"id":"plan","type":"Plan","text":"retained plan"}}),
        );
        let (turns, _) = projection.finish();
        let items = turns[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].body(), ItemBody::UserMessage { .. }));
        assert!(matches!(items[1].body(),ItemBody::AssistantText{text,..} if text=="answer"));
        assert!(matches!(items[2].body(),ItemBody::Plan{text} if text=="retained plan"));
    }

    #[test]
    fn malformed_turn_reference_fails_without_exposing_payload() {
        let mut projection = Projection::new(true);
        let error = projection.apply(&json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"missing","item":{"type":"AgentMessage","id":"item","content":[{"type":"Text","text":"private fixture"}]}}})).unwrap_err();
        assert!(error.to_string().contains("unknown turn"));
        assert!(!error.to_string().contains("private fixture"));
    }
}
