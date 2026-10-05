use crate::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    pub attachment_id: String,
    pub mime_type: String,
    pub base64: String,
}

#[derive(Debug, Default)]
struct MessageCursor {
    id: String,
    blocks: BTreeMap<u64, String>,
    final_index: u64,
}
/// Native message/block correlation only; no application entity builder or IDs.
#[derive(Debug, Default)]
pub struct ClaudeProtocol {
    pub control: ClaudeControl,
    messages: BTreeMap<String, MessageCursor>,
    current: BTreeMap<String, String>,
    tools: BTreeMap<String, (String, Value)>,
    /// SDK task ID -> native tool-use ID. Local Bash tasks have no child thread.
    tasks: BTreeMap<String, (String, bool)>,
    last_head: Option<String>,
    text_seen: BTreeSet<String>,
}
impl ClaudeProtocol {
    pub fn command(
        &mut self,
        command: &ProviderCommand,
        user_uuid: &str,
        images: &[PreparedImage],
    ) -> Result<Translation, ProtocolError> {
        let mut result = Translation::default();
        match command {
            ProviderCommand::Start {
                text,
                attachments,
                context,
                ..
            } => {
                self.text_seen.remove("");
                result.events.push(ProviderEvent::PromptOffered {
                    key: user_uuid.into(),
                });
                result
                    .events
                    .push(ProviderEvent::TurnStarted { native_turn: None });
                result.outbound.push(claude_user_message(
                    &if context.is_empty() || text.trim() == "/compact" {
                        text.clone()
                    } else {
                        format!("{context}\n\n{text}")
                    },
                    attachments,
                    user_uuid,
                    false,
                    images,
                )?);
            }
            ProviderCommand::Steer { text, attachments } => result.outbound.push(
                claude_user_message(text, attachments, user_uuid, true, images)?,
            ),
            ProviderCommand::Interrupt => result
                .outbound
                .push(self.control.request("interrupt", json!({}))),
            ProviderCommand::Respond {
                native_key,
                decision,
                answers,
                ..
            } => result.outbound.push(self.control.respond(
                native_key,
                *decision,
                answers.as_ref(),
            )?),
            ProviderCommand::SetModel { selection } => result.outbound.push(
                self.control
                    .request("set_model", json!({"model":selection.model})),
            ),
            ProviderCommand::SetRuntimeMode {
                runtime_mode,
                interaction_mode,
            } => result.outbound.push(self.control.request(
                "set_permission_mode",
                json!({"mode":claude_permission_mode(*runtime_mode,*interaction_mode)}),
            )),
            ProviderCommand::Compact { .. } => {
                self.text_seen.remove("");
                result.events.push(ProviderEvent::PromptOffered {
                    key: user_uuid.into(),
                });
                result
                    .events
                    .push(ProviderEvent::TurnStarted { native_turn: None });
                result.outbound.push(claude_user_message(
                    "/compact",
                    &[],
                    user_uuid,
                    false,
                    images,
                )?);
            }
            // Resume/fork/absolute rollback are launch options, not relative CLI controls.
            ProviderCommand::Rollback { .. } | ProviderCommand::Fork { .. } => {}
        }
        Ok(result)
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Translation, ProtocolError> {
        if let Some(control) = self.control.receive(frame)? {
            return Ok(control);
        }
        let route = optional(frame, "parent_tool_use_id").unwrap_or_default();
        let mut output = Translation::default();
        let mut events = vec![];
        match string(frame, "type").as_str() {
            "system" => match string(frame, "subtype").as_str() {
                "init" => {
                    let native = required(frame, "session_id")?;
                    events.push(ProviderEvent::SessionReady {
                        native_thread: native,
                    });
                    events.push(ProviderEvent::TurnStarted { native_turn: None });
                }
                "compact_boundary" => events.push(ProviderEvent::ItemFinished {
                    key: optional(frame, "uuid").unwrap_or_else(|| "compact".into()),
                    kind: ProviderItem::Compaction {
                        before: frame["compact_metadata"]["pre_tokens"].as_u64(),
                        after: frame["compact_metadata"]["post_tokens"].as_u64(),
                    },
                    text: None,
                    status: ItemStatus::Completed,
                }),
                "api_retry" => events.push(ProviderEvent::ItemFinished {
                    key: optional(frame, "uuid").unwrap_or_else(|| "retry".into()),
                    kind: ProviderItem::Error {
                        message: optional(frame, "error")
                            .unwrap_or_else(|| string(frame, "error_status")),
                        retrying: true,
                    },
                    text: None,
                    status: ItemStatus::Failed,
                }),
                "model_refusal_fallback" => events.push(ProviderEvent::ItemFinished {
                    key: optional(frame, "uuid").unwrap_or_else(|| "fallback".into()),
                    kind: ProviderItem::Notice {
                        message: string(frame, "message"),
                    },
                    text: None,
                    status: ItemStatus::Completed,
                }),
                "task_started" => {
                    let task = required(frame, "task_id")?;
                    let tool = optional(frame, "tool_use_id").unwrap_or_else(|| task.clone());
                    let agent = string(frame, "task_type") == "local_agent"
                        || self
                            .tools
                            .get(&tool)
                            .is_some_and(|(name, _)| name == "Agent" || name == "Task");
                    self.tasks.insert(task.clone(), (tool.clone(), agent));
                    if agent {
                        events.push(ProviderEvent::SubagentStarted {
                            key: tool,
                            parent: optional(frame, "parent_tool_use_id"),
                            prompt: string(frame, "description"),
                            model: optional(frame, "model"),
                        });
                    } else {
                        events.push(ProviderEvent::BackgroundTask {
                            key: task,
                            tool: tool.clone(),
                            kind: if self
                                .tools
                                .get(&tool)
                                .is_some_and(|(name, _)| name == "Monitor")
                            {
                                BackgroundKind::Monitor
                            } else {
                                BackgroundKind::Command
                            },
                            description: string(frame, "description"),
                            status: None,
                            summary: None,
                        });
                    }
                }
                "task_progress" => {
                    let task = required(frame, "task_id")?;
                    if let Some((tool, true)) = self.tasks.get(&task) {
                        events.push(ProviderEvent::SubagentProgress {
                            key: tool.clone(),
                            progress: string(frame, "description"),
                            model: optional(frame, "model"),
                        });
                    }
                }
                "task_notification" => {
                    let task = required(frame, "task_id")?;
                    if let Some((tool, agent)) = self.tasks.remove(&task) {
                        if agent {
                            events.push(ProviderEvent::SubagentFinished {
                                key: tool,
                                status: match string(frame, "status").as_str() {
                                    "completed" => ItemStatus::Completed,
                                    "stopped" => ItemStatus::Cancelled,
                                    _ => ItemStatus::Failed,
                                },
                                result: string(frame, "summary"),
                            });
                        } else {
                            events.push(ProviderEvent::BackgroundTask {
                                key: task,
                                tool: tool.clone(),
                                kind: if self
                                    .tools
                                    .get(&tool)
                                    .is_some_and(|(name, _)| name == "Monitor")
                                {
                                    BackgroundKind::Monitor
                                } else {
                                    BackgroundKind::Command
                                },
                                description: string(frame, "summary"),
                                status: Some(match string(frame, "status").as_str() {
                                    "completed" => ItemStatus::Completed,
                                    "stopped" => ItemStatus::Cancelled,
                                    _ => ItemStatus::Failed,
                                }),
                                summary: optional(frame, "summary"),
                            });
                        }
                    }
                }
                _ => {}
            },
            "stream_event" => {
                let event = &frame["event"];
                match string(event, "type").as_str() {
                    "message_start" => {
                        let id = required(&event["message"], "id")?;
                        self.current.insert(route.clone(), id.clone());
                        self.messages.insert(
                            id.clone(),
                            MessageCursor {
                                id,
                                blocks: BTreeMap::new(),
                                final_index: 0,
                            },
                        );
                    }
                    "content_block_start" => {
                        let index = event["index"].as_u64().ok_or_else(|| {
                            ProtocolError::Invalid("missing content block index".into())
                        })?;
                        let id = self.current.get(&route).cloned().ok_or_else(|| {
                            ProtocolError::Invalid("content block without message_start".into())
                        })?;
                        let cursor = self.messages.get_mut(&id).unwrap();
                        let block = &event["content_block"];
                        let kind = string(block, "type");
                        cursor.blocks.insert(index, kind.clone());
                        let key = format!("{}:block:{index}", cursor.id);
                        match kind.as_str() {
                            "thinking" => events.push(ProviderEvent::ItemStarted {
                                key,
                                kind: ProviderItem::Reasoning,
                            }),
                            "text" => {
                                self.text_seen.insert(route.clone());
                                events.push(ProviderEvent::ItemStarted {
                                    key,
                                    kind: ProviderItem::Text,
                                });
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let index = event["index"].as_u64().ok_or_else(|| {
                            ProtocolError::Invalid("missing content block index".into())
                        })?;
                        let id = self.current.get(&route).ok_or_else(|| {
                            ProtocolError::Invalid("delta without message_start".into())
                        })?;
                        let delta = &event["delta"];
                        let key = format!("{id}:block:{index}");
                        match string(delta, "type").as_str() {
                            "text_delta" => {
                                self.text_seen.insert(route.clone());
                                events.push(ProviderEvent::TextDelta {
                                    key,
                                    kind: ProviderItem::Text,
                                    text: string(delta, "text"),
                                });
                            }
                            "thinking_delta" => events.push(ProviderEvent::TextDelta {
                                key,
                                kind: ProviderItem::Reasoning,
                                text: string(delta, "thinking"),
                            }),
                            _ => {}
                        }
                    }
                    // Final block snapshots provide the authoritative content.
                    "content_block_stop" | "message_delta" | "message_stop" => {}
                    _ => {}
                }
            }
            "assistant" => {
                let message = &frame["message"];
                let id = required(message, "id")?;
                self.last_head = optional(frame, "uuid");
                let content = message["content"].as_array().ok_or_else(|| {
                    ProtocolError::Invalid("assistant content is not an array".into())
                })?;
                let cursor = self
                    .messages
                    .entry(id.clone())
                    .or_insert_with(|| MessageCursor {
                        id,
                        blocks: BTreeMap::new(),
                        final_index: 0,
                    });
                for (index, block) in content.iter().enumerate() {
                    let kind = string(block, "type");
                    let block_index = if content.len() > 1 {
                        index as u64
                    } else {
                        cursor
                            .blocks
                            .iter()
                            .find(|(index, t)| **index >= cursor.final_index && **t == kind)
                            .map(|(i, _)| *i)
                            .unwrap_or(cursor.final_index)
                    };
                    cursor.final_index = block_index + 1;
                    let key = format!("{}:block:{block_index}", cursor.id);
                    match kind.as_str() {
                        "text" => {
                            self.text_seen.insert(route.clone());
                            events.push(ProviderEvent::ItemFinished {
                                key,
                                kind: ProviderItem::Text,
                                text: Some(string(block, "text")),
                                status: ItemStatus::Completed,
                            });
                        }
                        "thinking" => events.push(ProviderEvent::ItemFinished {
                            key,
                            kind: ProviderItem::Reasoning,
                            text: Some(string(block, "thinking")),
                            status: ItemStatus::Completed,
                        }),
                        "tool_use" => {
                            let key = required(block, "id")?;
                            let name = required(block, "name")?;
                            let input = block["input"].clone();
                            self.tools
                                .insert(key.clone(), (name.clone(), input.clone()));
                            if name == "Agent" || name == "Task" {
                                events.push(ProviderEvent::SubagentStarted {
                                    key,
                                    parent: if route.is_empty() {
                                        None
                                    } else {
                                        Some(route.clone())
                                    },
                                    prompt: string(&input, "prompt"),
                                    model: optional(&input, "model"),
                                });
                            } else if name == "TodoWrite" {
                                events.push(ProviderEvent::Plan {
                                    key: key.clone(),
                                    markdown: String::new(),
                                    steps: input["todos"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .map(|s| PlanStep {
                                            text: string(s, "content"),
                                            status: string(s, "status"),
                                        })
                                        .collect(),
                                });
                            } else {
                                events.push(ProviderEvent::ItemStarted {
                                    key,
                                    kind: claude_tool(&name, &input, None),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(usage) = message.get("usage") {
                    events.push(ProviderEvent::Usage(claude_usage(usage)));
                }
            }
            "user" => {
                if let Some(content) = frame["message"]["content"].as_array() {
                    for block in content {
                        if block["type"] == "tool_result" {
                            let key = required(block, "tool_use_id")?;
                            let text = claude_result_text(&block["content"]);
                            let status = if block["is_error"].as_bool() == Some(true) {
                                ItemStatus::Failed
                            } else {
                                ItemStatus::Completed
                            };
                            if let Some((name, input)) = self.tools.get(&key) {
                                if let Some(task) =
                                    optional(&frame["tool_use_result"], "backgroundTaskId")
                                        .or_else(|| optional(&frame["tool_use_result"], "taskId"))
                                    && name != "Agent"
                                    && name != "Task"
                                {
                                    self.tasks.insert(task.clone(), (key.clone(), false));
                                    events.push(ProviderEvent::BackgroundTask {
                                        key: task,
                                        tool: key.clone(),
                                        kind: if name == "Monitor" {
                                            BackgroundKind::Monitor
                                        } else {
                                            BackgroundKind::Command
                                        },
                                        description: optional(input, "description")
                                            .unwrap_or_else(|| name.clone()),
                                        status: None,
                                        summary: None,
                                    });
                                }
                                if name == "Agent" || name == "Task" {
                                    events.push(ProviderEvent::SubagentFinished {
                                        key,
                                        status,
                                        result: text,
                                    });
                                } else {
                                    events.push(ProviderEvent::ItemFinished {
                                        key,
                                        kind: claude_tool(name, input, Some(&block["content"])),
                                        text: Some(text),
                                        status,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            "result" => {
                let success =
                    frame["subtype"] == "success" && frame["is_error"].as_bool() != Some(true);
                let aborted = matches!(
                    string(frame, "terminal_reason").as_str(),
                    "aborted_streaming" | "aborted_tools"
                );
                if success
                    && !aborted
                    && !self.text_seen.contains(&route)
                    && frame["result"].as_str().is_some_and(|s| !s.is_empty())
                {
                    events.push(ProviderEvent::ItemFinished {
                        key: optional(frame, "uuid").unwrap_or_else(|| "result".into()),
                        kind: ProviderItem::Text,
                        text: optional(frame, "result"),
                        status: ItemStatus::Completed,
                    });
                }
                if !success && !aborted {
                    let message = frame["errors"]
                        .as_array()
                        .map(|e| {
                            e.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .or_else(|| optional(frame, "result"))
                        .unwrap_or_default();
                    events.push(ProviderEvent::ItemFinished {
                        key: optional(frame, "uuid").unwrap_or_else(|| "result-error".into()),
                        kind: ProviderItem::Error {
                            message,
                            retrying: false,
                        },
                        text: None,
                        status: ItemStatus::Failed,
                    });
                }
                events.push(ProviderEvent::Usage(claude_usage(&frame["usage"])));
                if aborted {
                    events.push(ProviderEvent::TurnAborted {
                        reason: string(frame, "terminal_reason"),
                    });
                } else {
                    events.push(ProviderEvent::TurnFinished {
                        status: if success {
                            RunStatus::Completed
                        } else {
                            RunStatus::Failed
                        },
                        native_head: self.last_head.clone(),
                    });
                }
                self.text_seen.remove(&route);
            }
            "rate_limit_event" if frame["rate_limit_info"]["status"] == "rejected" => {
                events.push(ProviderEvent::ItemFinished { key:optional(frame,"uuid").unwrap_or_else(|| "rate-limit".into()),kind:ProviderItem::Notice { message:"Claude usage limit reached. Send the message again once the limit resets.".into() },text:None,status:ItemStatus::Completed });
            }
            _ => {}
        }
        let events = if route.is_empty() {
            events
        } else {
            events
                .into_iter()
                .map(|event| ProviderEvent::Child {
                    key: route.clone(),
                    event: Box::new(event),
                })
                .collect()
        };
        let echoed_prompts = frame["user_message_uuids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| optional(frame, "user_message_uuid").into_iter().collect());
        let frame_type = string(frame, "type");
        output.events.push(ProviderEvent::NativeOutput {
            echoed_prompts,
            acknowledged_prompt: if frame_type == "command_lifecycle" {
                optional(frame, "command_uuid")
            } else {
                None
            },
            root: route.is_empty()
                && matches!(
                    frame_type.as_str(),
                    "assistant" | "stream_event" | "user" | "result"
                ),
            result: (frame_type == "result").then(|| NativeResult {
                origin: optional(&frame["origin"], "kind"),
                turn_count: frame["num_turns"].as_u64().unwrap_or(1),
            }),
            events,
        });
        Ok(output)
    }
}
fn claude_tool(name: &str, input: &Value, output: Option<&Value>) -> ProviderItem {
    match name {
        "Bash" => ProviderItem::Command {
            command: string(input, "command"),
            cwd: None,
            exit_code: None,
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ProviderItem::FileChange {
            changes: Json(input.clone()),
        },
        "WebSearch" => ProviderItem::WebSearch {
            query: string(input, "query"),
        },
        _ => ProviderItem::Tool {
            name: name.into(),
            input: Json(input.clone()),
            output: output.cloned().map(Json),
        },
    }
}
fn claude_result_text(content: &Value) -> String {
    content.as_str().map(str::to_owned).unwrap_or_else(|| {
        content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    })
}
fn claude_usage(usage: &Value) -> TokenUsage {
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached_input = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_creation = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let output = usage["output_tokens"].as_u64().unwrap_or(0);
    TokenUsage {
        input,
        cached_input,
        output,
        reasoning_output: 0,
        total: input + cached_input + cache_creation + output,
        max: None,
    }
}
pub fn claude_user_message(
    text: &str,
    attachments: &[Attachment],
    uuid: &str,
    steer: bool,
    images: &[PreparedImage],
) -> Result<Value, ProtocolError> {
    let mut content = vec![];
    for file in attachments {
        if native_image_mime(&file.mime_type) {
            let image = images
                .iter()
                .find(|image| image.attachment_id == file.id)
                .ok_or_else(|| {
                    ProtocolError::Invalid(format!("missing prepared image {}", file.id))
                })?;
            content.push(json!({"type":"image","source":{"type":"base64","media_type":image.mime_type,"data":image.base64}}));
        }
    }
    let text = attachment_text(text, attachments);
    let content = if content.is_empty() {
        json!(text)
    } else {
        content.push(json!({"type":"text","text":text}));
        json!(content)
    };
    let mut message = json!({"type":"user","uuid":uuid,"message":{"role":"user","content":content},"parent_tool_use_id":null});
    if steer {
        message["priority"] = json!("now");
    }
    Ok(message)
}
