//! Pure translation of Claude native content into the shared conversation model.
//! Live execution and transcript reading own state updates and use these results.
use agent_protocol::{
    execution::ItemStatus,
    ids::ItemId,
    items::*,
    models::Item,
    session::{ProviderKind, SessionRef},
};
use serde_json::{Value, json};
use std::borrow::Cow;

pub(super) fn message_blocks(content: &Value) -> Option<Cow<'_, [Value]>> {
    match content {
        Value::String(text) => Some(Cow::Owned(vec![json!({"type":"text","text":text})])),
        Value::Array(blocks) => Some(Cow::Borrowed(blocks)),
        _ => None,
    }
}

fn input_parts(blocks: &[Value]) -> Vec<MessagePart> {
    blocks
        .iter()
        .filter_map(|block| match block["type"].as_str() {
            Some("text") => Some(MessagePart::Text {
                text: block["text"].as_str()?.into(),
            }),
            Some("image") => {
                let source = &block["source"];
                let source = match source["type"].as_str() {
                    Some("base64") => format!(
                        "data:{};base64,{}",
                        source["media_type"].as_str()?,
                        source["data"].as_str()?
                    ),
                    Some("url") => source["url"].as_str()?.into(),
                    _ => return None,
                };
                Some(MessagePart::Image { source })
            }
            _ => None,
        })
        .collect()
}

pub(super) fn input_item(id: &str, blocks: &[Value], is_meta: bool) -> Option<Item> {
    if is_meta {
        return None;
    }
    let content = input_parts(blocks);
    if content.is_empty() {
        return None;
    }
    let mut item = Item::new(
        id.into(),
        ItemStatus::Unknown,
        ItemBody::UserMessage {
            text: None,
            content,
        },
    );
    item.client_input_id = Some(id.into());
    Some(item)
}

pub(super) fn is_human_input(is_meta: bool, origin: Option<&str>, source: Option<&str>) -> bool {
    !is_meta && origin.is_none_or(|kind| kind == "human") && source != Some("system")
}

pub(super) fn user_item(
    id: &str,
    content: &Value,
    is_meta: bool,
    origin: Option<&str>,
    source: Option<&str>,
) -> Option<Item> {
    if is_meta {
        return None;
    }
    if is_human_input(is_meta, origin, source) {
        input_item(id, &message_blocks(content)?, false)
    } else {
        Some(Item::new(
            id.into(),
            ItemStatus::Unknown,
            ItemBody::Attachment {
                kind: AttachmentKind::Other,
                content: json!({"type":origin.unwrap_or("system"),"promptSource":source,"content":content}),
            },
        ))
    }
}

pub(super) fn attachment_item(id: &str, attachment: &Value) -> anyhow::Result<Option<Item>> {
    anyhow::ensure!(
        attachment.is_object(),
        "native attachment content is unavailable"
    );
    let kind = match attachment["type"].as_str() {
        Some(
            "deferred_tools_delta"
            | "agent_listing_delta"
            | "mcp_instructions_delta"
            | "skill_listing"
            | "total_tokens_reminder"
            | "batching_reminder_sent"
            | "silent_turn_reminder"
            | "date_change"
            | "auto_mode"
            | "command_permissions"
            | "environment"
            | "model"
            | "session_context"
            | "date"
            | "prompt_snapshot"
            | "deferred_tools_record"
            | "bash_output_audience_note"
            | "task_reminder",
        ) => return Ok(None),
        Some("queued_command")
            if attachment["commandMode"] == "prompt"
                && attachment["origin"]["kind"] == "human"
                && attachment["isMeta"] != true =>
        {
            if let Some(blocks) = message_blocks(&attachment["prompt"])
                && let Some(item) = input_item(
                    attachment["source_uuid"].as_str().unwrap_or(id),
                    &blocks,
                    false,
                )
            {
                return Ok(Some(item));
            }
            AttachmentKind::Other
        }
        Some("hook_success" | "hook_error" | "hook_non_blocking_error" | "hook_blocking_error") => {
            AttachmentKind::HookResult
        }
        Some("edited_text_file") => AttachmentKind::FileEdit,
        Some("remote_session_change") => AttachmentKind::SessionUpdate,
        _ => AttachmentKind::Other,
    };
    Ok(Some(Item::new(
        id.into(),
        ItemStatus::Unknown,
        ItemBody::Attachment {
            kind,
            content: attachment.clone(),
        },
    )))
}

pub(super) fn tool_result_item(
    id: ItemId,
    body: &ItemBody,
    content: &Value,
    metadata: &Value,
    failed: bool,
) -> Item {
    let body = Box::new(tool_result_body(body, content, metadata));
    Item {
        id,
        client_input_id: None,
        status: if failed {
            ItemStatus::Failed
        } else if metadata["backgroundTaskId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            || metadata["status"] == "async_launched"
        {
            ItemStatus::Running
        } else {
            ItemStatus::Completed
        },
        body: if metadata["persistedOutputPath"]
            .as_str()
            .is_some_and(|path| !path.is_empty())
        {
            ItemContent::Deferred { summary: body }
        } else {
            ItemContent::Inline { body }
        },
    }
}

pub(super) struct TaskOutcome<'a> {
    pub tool_id: &'a str,
    pub output_path: Option<&'a str>,
    status: ItemStatus,
    summary: &'a str,
}

impl TaskOutcome<'_> {
    pub fn item(&self, id: ItemId, body: &ItemBody) -> Option<Item> {
        if !matches!(
            body,
            ItemBody::CommandExecution { .. }
                | ItemBody::Subagent { .. }
                | ItemBody::ToolCall { .. }
        ) {
            return None;
        }
        let body = Box::new(tool_result_body(body, &json!(self.summary), &Value::Null));
        Some(Item {
            id,
            status: self.status,
            client_input_id: None,
            body: if self.output_path.is_some() {
                ItemContent::Deferred { summary: body }
            } else {
                ItemContent::Inline { body }
            },
        })
    }
}

pub(super) fn task_outcome<'a>(
    tool_id: Option<&'a str>,
    status: Option<&str>,
    summary: Option<&'a str>,
    output_path: Option<&'a str>,
) -> Option<TaskOutcome<'a>> {
    let tool_id = tool_id.filter(|id| !id.is_empty())?;
    let status = match status? {
        "completed" => ItemStatus::Completed,
        "failed" => ItemStatus::Failed,
        "stopped" => ItemStatus::Interrupted,
        _ => return None,
    };
    Some(TaskOutcome {
        tool_id,
        status,
        summary: summary?,
        output_path: output_path.filter(|path| !path.is_empty()),
    })
}

pub(super) fn notification_outcome<'a>(
    origin: Option<&str>,
    prompt: &'a Value,
) -> Option<TaskOutcome<'a>> {
    if origin != Some("task-notification") {
        return None;
    }
    let text = prompt
        .as_str()?
        .trim()
        .strip_prefix("<task-notification>")?
        .strip_suffix("</task-notification>")?;
    // Claude's notification envelope contains raw summary text, which can include
    // '&' and '<'. Read its delimited fields without treating that text as XML.
    let (fields, summary) = text.split_once("<summary>")?;
    let (summary, _) = summary.rsplit_once("</summary>")?;
    let field = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let (prefix, value) = fields.split_once(&open)?;
        let (value, suffix) = value.split_once(&close)?;
        if prefix.contains(&close)
            || suffix.contains(&open)
            || suffix.contains(&close)
            || value.contains(['<', '>'])
        {
            return None;
        }
        Some(value.trim())
    };
    task_outcome(
        field("tool-use-id"),
        field("status"),
        Some(summary.trim()),
        field("output-file"),
    )
}

pub(super) fn content_item(
    session: &SessionRef,
    id: String,
    block: &Value,
    cwd: Option<&str>,
    tool_status: ItemStatus,
) -> Result<Item, serde_json::Error> {
    let text = |key: &str| block[key].as_str().unwrap_or_default().to_owned();
    let mut id = id.into();
    let mut status = ItemStatus::Completed;
    let body = match block["type"].as_str() {
        Some("text") => ItemBody::AssistantText {
            text: text("text"),
            phase: AssistantPhase::Unknown,
        },
        Some("thinking") => ItemBody::Reasoning {
            content: vec![text("thinking")],
            summary: vec![],
        },
        Some("tool_use") => {
            id = serde_json::from_value(block["id"].clone())?;
            status = tool_status;
            let input = &block["input"];
            let string = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
            match block["name"].as_str() {
                Some("Bash") => ItemBody::CommandExecution {
                    command: string("command"),
                    cwd: cwd.map(str::to_owned),
                    output: String::new(),
                    exit_code: None,
                },
                Some("Write" | "Edit" | "NotebookEdit") => {
                    let proposal = match block["name"].as_str() {
                        Some("Write") => FileProposal::Write {
                            content: string("content"),
                        },
                        Some("Edit") => FileProposal::Edit {
                            old_text: string("old_string"),
                            new_text: string("new_string"),
                            replace_all: input["replace_all"] == true,
                        },
                        _ => FileProposal::Notebook {
                            cell_id: input["cell_id"].as_str().map(str::to_owned),
                            source: string("new_source"),
                            mode: input["edit_mode"].as_str().map(str::to_owned),
                        },
                    };
                    ItemBody::FileChange {
                        changes: vec![FileChange {
                            path: string(if block["name"] == "NotebookEdit" {
                                "notebook_path"
                            } else {
                                "file_path"
                            }),
                            kind: FileChangeKind::Unknown,
                            diff: None,
                            proposal: Some(proposal),
                        }],
                        output: String::new(),
                    }
                }
                Some("Agent" | "Task") => ItemBody::Subagent {
                    tool: text("name"),
                    prompt: input["prompt"].as_str().map(str::to_owned),
                    model: input["model"].as_str().map(str::to_owned),
                    effort: None,
                    sender: Some(session.clone()),
                    receivers: vec![],
                    states: vec![],
                    agent_id: None,
                    result: None,
                },
                _ => {
                    let name = block["name"].as_str().unwrap_or_default();
                    let (kind, server, tool) = match name
                        .strip_prefix("mcp__")
                        .and_then(|name| name.split_once("__"))
                    {
                        Some((server, tool)) => (ToolKind::Mcp, server, tool),
                        None => (ToolKind::Local, "Claude Code", name),
                    };
                    ItemBody::ToolCall {
                        resource_uri: None,
                        plugin_id: None,
                        kind,
                        tool: tool.into(),
                        server: Some(server.into()),
                        namespace: None,
                        arguments: input.clone(),
                        result: None,
                        error: None,
                        content: vec![],
                        success: None,
                        duration_ms: None,
                    }
                }
            }
        }
        _ => {
            status = ItemStatus::Unknown;
            ItemBody::Custom {
                provider: ProviderKind::Claude,
                kind: text("type"),
                value: block.clone(),
            }
        }
    };
    Ok(Item::new(id, status, body))
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePatchHunk {
    old_start: u64,
    old_lines: u64,
    new_start: u64,
    new_lines: u64,
    lines: Vec<String>,
}
fn structured_patch(value: &Value) -> Option<String> {
    use std::fmt::Write as _;
    let hunks: Vec<NativePatchHunk> = serde_json::from_value(value.clone()).ok()?;
    if hunks.is_empty() {
        return None;
    }
    let mut diff = String::new();
    for hunk in hunks {
        if !hunk
            .lines
            .iter()
            .all(|line| line.starts_with([' ', '+', '-', '\\']))
        {
            return None;
        }
        writeln!(
            diff,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        )
        .ok()?;
        for line in hunk.lines {
            writeln!(diff, "{line}").ok()?;
        }
    }
    Some(diff)
}
pub(super) fn tool_result_body(body: &ItemBody, content: &Value, metadata: &Value) -> ItemBody {
    let mut body = body.clone();
    let output = || {
        content.as_str().map(str::to_owned).unwrap_or_else(|| {
            content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    };
    match &mut body {
        ItemBody::CommandExecution {
            output: result,
            exit_code,
            ..
        } => {
            *result = output();
            if let Some(code) = metadata["exitCode"]
                .as_i64()
                .and_then(|code| code.try_into().ok())
            {
                *exit_code = Some(code);
            }
        }
        ItemBody::FileChange {
            changes,
            output: result,
        } => {
            *result = output();
            if changes.len() == 1 {
                let change = &mut changes[0];
                if let Some(diff) = structured_patch(&metadata["structuredPatch"]) {
                    change.diff = Some(diff);
                }
                match metadata["type"].as_str() {
                    Some("create") => change.kind = FileChangeKind::Add,
                    Some("update") => change.kind = FileChangeKind::Update { move_path: None },
                    _ => {}
                }
            }
        }
        ItemBody::Subagent {
            agent_id, result, ..
        } => {
            if let Some(id) = metadata["agentId"].as_str() {
                *agent_id = Some(id.into());
            }
            *result = Some(content.clone());
        }
        ItemBody::ToolCall { result, .. } => *result = Some(content.clone()),
        _ => {}
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn background_outcomes_match_live_and_saved_notifications(
            status in prop_oneof![Just("completed"), Just("failed"), Just("stopped")],
            summary in "[a-z &<>]{0,80}",
            background in any::<bool>(),
            failed_launch in any::<bool>(),
            tool_kind in 0..3usize,
            output_available in any::<bool>(),
        ) {
            let summary = summary.trim();
            let session = SessionRef::new(ProviderKind::Claude, "session".into()).unwrap();
            let call = content_item(&session, "block".into(),
                &json!({"type":"tool_use","id":"work","name":match tool_kind {0=>"Bash", 1=>"Agent", _=>"mcp__workflow__run"},"input":{"command":"work","prompt":"work"}}),
                None, ItemStatus::Running).unwrap();
            let metadata = if tool_kind != 0 {
                json!({"status":if background {"async_launched"} else {"completed"},"agentId":"agent"})
            } else {
                json!({"backgroundTaskId":if background {"task"} else {""}})
            };
            let launched = tool_result_item(call.id.clone(), call.body(), &json!("launched"), &metadata, failed_launch);
            prop_assert_eq!(launched.status, if failed_launch {ItemStatus::Failed} else if background {ItemStatus::Running} else {ItemStatus::Completed});
            let output = if output_available {"/work/result.output"} else {""};
            let prompt = json!(format!("<task-notification>\n<task-id>task</task-id><tool-use-id>work</tool-use-id><output-file>{output}</output-file>\n<status>{status}</status><summary>{summary}</summary>\n</task-notification>"));
            let live = task_outcome(Some("work"), Some(status), Some(summary), Some(output)).unwrap();
            let saved = notification_outcome(Some("task-notification"), &prompt).unwrap();
            prop_assert_eq!(saved.tool_id, "work");
            prop_assert_eq!(saved.output_path, live.output_path);
            prop_assert_eq!(live.output_path, output_available.then_some("/work/result.output"));
            let expected = match status {"completed"=>ItemStatus::Completed, "failed"=>ItemStatus::Failed, _=>ItemStatus::Interrupted};
            let result = live.item(launched.id.clone(), launched.body()).unwrap();
            prop_assert_eq!(result.is_deferred(), output_available);
            prop_assert_eq!(result.status, expected);
            prop_assert_eq!(saved.item(launched.id.clone(), launched.body()), Some(result.clone()));
            prop_assert_eq!(live.item(result.id.clone(), result.body()), Some(result));
            prop_assert!(notification_outcome(Some("prompt"), &prompt).is_none());
        }

        #[test]
        fn human_inputs_match_codex_while_metadata_and_notifications_stay_out_of_input(
            text in "[^\\x00]{0,80}",
            array_content in any::<bool>(),
            is_meta in any::<bool>(),
            origin in prop_oneof![Just(Value::Null), Just(json!({"kind":"human"})), Just(json!({"kind":"agent"}))],
            mode in prop_oneof![Just(Value::Null), Just(json!("prompt")), Just(json!("task-notification")), "[a-z]{1,12}".prop_map(Value::String)],
        ) {
            let content = if array_content {
                json!([{"type":"text","text":text},
                    {"type":"image","source":{"type":"url","url":"https://example.invalid/image.png"}}])
            } else {
                json!(text)
            };
            let blocks = message_blocks(&content).unwrap();
            let input = input_item("human", &blocks, is_meta);
            if is_meta {
                prop_assert!(input.is_none());
            } else {
                let mut parts = vec![json!({"type":"text","text":text})];
                if array_content {
                    parts.push(json!({"type":"image","url":"https://example.invalid/image.png"}));
                }
                let codex = crate::adapters::codex::native::parse_item(json!({
                    "type":"userMessage","id":"human","clientId":"human","content":parts,
                })).unwrap();
                prop_assert_eq!(input, Some(codex));
            }
            let attachment = json!({"type":"queued_command","commandMode":mode,"origin":origin,"isMeta":is_meta,
                "source_uuid":"human","prompt":content});
            let item = attachment_item("native-row", &attachment).unwrap().unwrap();
            if mode == "prompt" && origin["kind"] == "human" && !is_meta {
                prop_assert_eq!(item, input_item("human", &blocks, false).unwrap());
            } else {
                prop_assert_eq!(item.id.as_str(), "native-row");
                prop_assert!(matches!(item.body(), ItemBody::Attachment {content, ..} if content == &attachment),
                    "non-human queued content must remain activity");
            }
        }
    }

    proptest! {
        #[test]
        fn current_native_user_records_distinguish_humans_from_system_activity(
            (origin, human_origin) in prop_oneof![Just((None,true)),Just((Some("human"),true)),Just((Some("task-notification"),false)),Just((Some("agent"),false))],
            (source, system_source) in prop_oneof![Just((None,false)),Just((Some("sdk"),false)),Just((Some("system"),true))],
            is_meta in any::<bool>(),
        ) {
            let content = json!("<task-notification><tool-use-id>work</tool-use-id><status>completed</status><summary>done</summary></task-notification>");
            let item = user_item("record", &content, is_meta, origin, source);
            if is_meta {
                prop_assert!(item.is_none());
            } else if human_origin && !system_source {
                prop_assert!(matches!(item.unwrap().body(),ItemBody::UserMessage {..}), "human input must remain a user message");
            } else {
                let item = item.unwrap();
                prop_assert!(matches!(item.body(),ItemBody::Attachment {content:value,..} if value["content"] == content), "system activity must retain its content");
                prop_assert!(matches!(agent_core::presentation::ItemMetadata::from(&item).kind,agent_core::presentation::GroupKind::Activity));
            }
        }
    }

    #[test]
    fn invalid_task_notifications_cannot_change_unrelated_content() {
        for fields in [
            "<status>failed</status>",
            "<tool-use-id></tool-use-id><status>failed</status>",
            "<tool-use-id>work</tool-use-id><status>running</status>",
            "<tool-use-id>work</tool-use-id><status>failed</status><status>completed</status>",
            "<tool-use-id>work</tool-use-id></tool-use-id><status>failed</status>",
            "</tool-use-id><tool-use-id>work</tool-use-id><status>failed</status>",
            "<tool-use-id>work<bad></tool-use-id><status>failed</status>",
        ] {
            let prompt = json!(format!(
                "<task-notification>{fields}<summary>result</summary></task-notification>"
            ));
            assert!(
                notification_outcome(Some("task-notification"), &prompt).is_none(),
                "{fields}"
            );
        }
        for prompt in [
            Value::Null,
            json!([]),
            json!(
                "<task-notification><tool-use-id>work</tool-use-id><status>failed</status></task-notification>"
            ),
            json!(
                "<tool-use-id>work</tool-use-id><status>failed</status><summary>result</summary>"
            ),
        ] {
            assert!(notification_outcome(Some("task-notification"), &prompt).is_none());
        }
        for (tool, status, summary) in [
            (None, Some("failed"), Some("result")),
            (Some(""), Some("failed"), Some("result")),
            (Some("work"), None, Some("result")),
            (Some("work"), Some("failed"), None),
        ] {
            assert!(task_outcome(tool, status, summary, None).is_none());
        }
        let outcome = task_outcome(Some("work"), Some("failed"), Some("result"), None).unwrap();
        assert!(
            outcome
                .item(
                    "work".into(),
                    &ItemBody::UserMessage {
                        text: None,
                        content: vec![]
                    }
                )
                .is_none()
        );
    }

    #[test]
    fn commands_and_responses_use_the_same_conversation_projection_as_codex() {
        use agent_protocol::{execution::TurnStatus, models::Turn};
        use std::sync::Arc;

        let session = SessionRef::new(ProviderKind::Claude, "session".into()).unwrap();
        let command = content_item(
            &session,
            "block".into(),
            &json!({"type":"tool_use","id":"command","name":"Bash","input":{"command":"false"}}),
            Some("/work"),
            ItemStatus::Running,
        )
        .unwrap();
        let command = tool_result_item(
            command.id.clone(),
            command.body(),
            &json!("failed"),
            &json!({"exitCode":7}),
            true,
        );
        let answer = content_item(
            &session,
            "answer".into(),
            &json!({"type":"text","text":"finished"}),
            None,
            ItemStatus::Unknown,
        )
        .unwrap();
        let codex_command = crate::adapters::codex::native::parse_item(json!({
            "type":"commandExecution","id":"command","status":"failed","command":"false",
            "cwd":"/work","aggregatedOutput":"failed","exitCode":7,
        }))
        .unwrap();
        let codex_answer = crate::adapters::codex::native::parse_item(json!({
            "type":"agentMessage","id":"answer","status":"completed","text":"finished","phase":"final_answer",
        })).unwrap();
        assert_eq!(command, codex_command);
        for status in [
            TurnStatus::Running,
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let turn = Turn {
                id: "turn".into(),
                status,
                items: Some(vec![Arc::new(command.clone()), Arc::new(answer.clone())]),
                ..Default::default()
            };
            let codex = Turn {
                items: Some(vec![
                    Arc::new(codex_command.clone()),
                    Arc::new(codex_answer.clone()),
                ]),
                ..turn.clone()
            };
            let projection = |turn: &Turn| {
                serde_json::to_value(agent_core::presentation::project(turn).collect::<Vec<_>>())
                    .unwrap()
            };
            assert_eq!(projection(&turn), projection(&codex));
        }
    }
    #[test]
    fn native_tools_preserve_observations_without_inventing_exit_codes_or_diffs() {
        let session = SessionRef::new(ProviderKind::Claude, "session".into()).unwrap();
        let command = content_item(
            &session,
            "stream".into(),
            &json!({"type":"tool_use","id":"bash","name":"Bash","input":{"command":"false"}}),
            Some("/work"),
            ItemStatus::Running,
        )
        .unwrap();
        let unknown = tool_result_item(
            command.id.clone(),
            command.body(),
            &json!("done"),
            &Value::Null,
            false,
        );
        assert_eq!(unknown.status, ItemStatus::Completed);
        assert!(!unknown.is_deferred());
        assert!(matches!(
            unknown.body(),
            ItemBody::CommandExecution {
                exit_code: None,
                ..
            }
        ));
        let known = tool_result_body(command.body(), &json!("failed"), &json!({"exitCode":7}));
        let saved = tool_result_item(
            command.id.clone(),
            command.body(),
            &json!("failed"),
            &json!({"exitCode":7,"persistedOutputPath":"/work/output"}),
            true,
        );
        assert!(saved.is_deferred());
        assert_eq!(saved.body(), &known);
        assert!(matches!(
            tool_result_body(&known, &json!("expanded"), &Value::Null),
            ItemBody::CommandExecution {
                exit_code: Some(7),
                ..
            }
        ));
        let edit = content_item(&session,"stream".into(),&json!({"type":"tool_use","id":"edit","name":"Edit","input":{"file_path":"/work/a","old_string":"old","new_string":"new"}}),Some("/work"),ItemStatus::Running).unwrap();
        assert!(
            matches!(edit.body(),ItemBody::FileChange {changes,..} if changes[0].diff.is_none() && changes[0].proposal.is_some())
        );
        let applied = tool_result_body(
            edit.body(),
            &json!("edited"),
            &json!({"type":"update","structuredPatch":[{"oldStart":5,"oldLines":1,"newStart":5,"newLines":1,"lines":["-actual old","+actual new"]}]}),
        );
        assert!(
            matches!(applied,ItemBody::FileChange {changes,..} if changes[0].diff.as_deref() == Some("@@ -5,1 +5,1 @@\n-actual old\n+actual new\n"))
        );
        let future = json!({"type":"future_block","nested":{"a":[1,2]}});
        assert!(
            matches!(content_item(&session,"unknown".into(),&future,None,ItemStatus::Unknown).unwrap().body(),ItemBody::Custom {provider:ProviderKind::Claude,value,..} if value == &future)
        );
        let mcp = content_item(&session, "stream".into(),
            &json!({"type":"tool_use","id":"mcp","name":"mcp__my_server__search","input":{"query":"example"}}),
            None, ItemStatus::Running).unwrap();
        assert!(
            matches!(mcp.body(), ItemBody::ToolCall {kind: ToolKind::Mcp, server: Some(server), tool, arguments, ..}
            if server == "my_server" && tool == "search" && arguments == &json!({"query":"example"}))
        );
        let agent = content_item(
            &session,
            "stream".into(),
            &json!({"type":"tool_use","id":"agent","name":"Agent","input":{"prompt":"inspect"}}),
            None,
            ItemStatus::Running,
        )
        .unwrap();
        let completed = tool_result_item(
            agent.id.clone(),
            agent.body(),
            &json!("child output"),
            &json!({"agentId":"child"}),
            false,
        );
        assert!(
            matches!(completed.body(), ItemBody::Subagent {agent_id: Some(id), result: Some(result), ..}
            if id == "child" && result == &json!("child output"))
        );
    }
}
