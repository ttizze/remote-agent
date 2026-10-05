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
    selected_context_window: Option<u64>,
    messages: BTreeMap<String, MessageCursor>,
    current: BTreeMap<String, String>,
    tools: BTreeMap<String, (String, Value)>,
    presentations: BTreeMap<String, ToolPresentation>,
    parents: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    observed_models: BTreeMap<String, String>,
    background_tasks: BTreeSet<String>,
    /// SDK task ID -> native tool-use ID. Local Bash tasks have no child thread.
    tasks: BTreeMap<String, (String, bool)>,
    text_seen: BTreeSet<String>,
    authentication_failed: BTreeSet<String>,
    usage_limited: BTreeSet<String>,
}
impl ClaudeProtocol {
    /// Restore only native correlation after a process restart. The actor owns
    /// the durable task and child projections that supply these values.
    pub fn restore_task_route(
        &mut self,
        task: &str,
        tool: &str,
        parent_tool: Option<&str>,
        agent: bool,
    ) {
        self.tasks.insert(task.into(), (tool.into(), agent));
        if let Some(parent) = parent_tool {
            self.parents.insert(tool.into(), parent.into());
        }
        if !agent {
            self.background_tasks.insert(task.into());
        }
    }
    pub fn command(
        &mut self,
        command: &ProviderCommand,
        user_uuid: &str,
        images: &[PreparedImage],
    ) -> Result<Translation, ProtocolError> {
        let mut result = Translation::default();
        match command {
            ProviderCommand::Start {
                selection,
                text,
                attachments,
                context,
                ..
            } => {
                self.selected_context_window = Some(claude_context_window(&selection.model));
                self.text_seen.remove("");
                self.authentication_failed.remove("");
                self.usage_limited.remove("");
                result.events.push(ProviderEvent::PromptOffered {
                    key: user_uuid.into(),
                });
                result
                    .events
                    .push(ProviderEvent::TurnStarted { native_turn: None });
                result.outbound.push(claude_user_message(
                    &if context.is_none() || text.trim() == "/compact" {
                        text.clone()
                    } else {
                        format!("{}\n\n{text}", render_history(context.as_ref().unwrap()))
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
            ProviderCommand::Interrupt { .. } => result
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
            ProviderCommand::Rollback {
                native_thread,
                absolute_head,
            } => {
                result.process = Some(if absolute_head.is_none() {
                    ProcessDirective::Reset {
                        native_thread: native_thread.clone(),
                    }
                } else {
                    ProcessDirective::Resume {
                        native_thread: native_thread.clone(),
                        absolute_head: absolute_head.clone(),
                    }
                })
            }
            ProviderCommand::Fork {
                native_thread,
                through_turn,
            } => {
                result.process = Some(ProcessDirective::Fork {
                    native_thread: native_thread.clone(),
                    through_head: through_turn.clone(),
                })
            }
        }
        Ok(result)
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Translation, ProtocolError> {
        if let Some(control) = self.control.receive(frame)? {
            return Ok(control);
        }
        let native_route = optional(frame, "parent_tool_use_id").unwrap_or_default();
        let mut route = self
            .aliases
            .get(&native_route)
            .cloned()
            .unwrap_or(native_route);
        if frame["type"] == "system"
            && matches!(
                string(frame, "subtype").as_str(),
                "task_started" | "task_progress" | "task_notification"
            )
        {
            let tool = optional(frame, "tool_use_id").or_else(|| {
                optional(frame, "task_id")
                    .and_then(|id| self.tasks.get(&id).map(|(tool, _)| tool.clone()))
            });
            if let Some(tool) = tool {
                let tool = self.aliases.get(&tool).unwrap_or(&tool);
                if let Some(parent) = self.parents.get(tool) {
                    route = parent.clone();
                }
            }
            if frame["task_type"] == "local_bash"
                || optional(frame, "task_id")
                    .is_some_and(|id| self.tasks.get(&id).is_some_and(|(_, agent)| !agent))
            {
                route.clear();
            }
        }
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
                        message: format!(
                            "Claude API {}.",
                            string(frame, "error").replace('_', " ")
                        ),
                        retrying: true,
                        code: frame["error_status"]
                            .as_i64()
                            .map(|status| format!("api_error_{status}"))
                            .or_else(|| optional(frame, "error")),
                        class: Some(
                            if frame["error_status"] == 429 {
                                "usage_limit"
                            } else if frame["error_status"].is_null() {
                                "transport_error"
                            } else {
                                "provider_error"
                            }
                            .into(),
                        ),
                        retryable: Some(true),
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
                    let offered_tool =
                        optional(frame, "tool_use_id").unwrap_or_else(|| task.clone());
                    let tool = self
                        .tasks
                        .get(&task)
                        .map(|(tool, _)| tool.clone())
                        .unwrap_or_else(|| offered_tool.clone());
                    self.aliases.insert(offered_tool.clone(), tool.clone());
                    let agent = string(frame, "task_type") == "local_agent"
                        || self
                            .tools
                            .get(&tool)
                            .is_some_and(|(name, _)| name == "Agent" || name == "Task");
                    self.tasks.insert(task.clone(), (tool.clone(), agent));
                    if agent {
                        events.push(ProviderEvent::SubagentStarted {
                            background: frame["is_backgrounded"] == true,
                            native_thread: None,
                            key: tool.clone(),
                            parent: (!route.is_empty()).then(|| route.clone()),
                            prompt: optional(frame, "prompt")
                                .or_else(|| {
                                    self.tools
                                        .get(&tool)
                                        .and_then(|(_, input)| optional(input, "prompt"))
                                })
                                .unwrap_or_default(),
                            model: optional(frame, "model")
                                .or_else(|| self.observed_models.get(&route).cloned()),
                        });
                        events.push(ProviderEvent::SubagentNativeBound {
                            key: tool.clone(),
                            native_task: task.clone(),
                        });
                        events.push(ProviderEvent::SubagentNamed {
                            key: tool,
                            title: string(frame, "description"),
                        });
                    } else if frame["owned_by_subagent"] != true || frame["is_backgrounded"] == true
                    {
                        self.background_tasks.insert(task.clone());
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
                    if let Some((tool, agent)) = self.tasks.get(&task).cloned() {
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
                        } else if self.background_tasks.remove(&task) {
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
                if !route.is_empty()
                    && let Some(model) = optional(message, "model")
                {
                    self.observed_models.insert(route.clone(), model.clone());
                    events.push(ProviderEvent::ModelObserved { model });
                }
                let id = required(message, "id")?;
                if let Some(key) = optional(frame, "uuid") {
                    events.push(ProviderEvent::AssistantCursor { key });
                }
                if frame["error"] == "authentication_failed" {
                    self.authentication_failed.insert(route.clone());
                }
                if frame["error"] == "rate_limit" {
                    self.usage_limited.insert(route.clone());
                } else {
                    self.usage_limited.remove(&route);
                }
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
                            let meta = frame["tool_use_meta"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .find(|meta| {
                                    meta["id"].as_str().is_some_and(|id| id.trim() == key)
                                });
                            if let Some(presentation) = meta.and_then(claude_tool_presentation) {
                                self.presentations.insert(key.clone(), presentation);
                            }
                            self.parents.insert(key.clone(), route.clone());
                            if name == "Agent" || name == "Task" {
                                events.push(ProviderEvent::SubagentStarted {
                                    background: input["run_in_background"] == true,
                                    native_thread: None,
                                    key,
                                    parent: if route.is_empty() {
                                        None
                                    } else {
                                        Some(route.clone())
                                    },
                                    prompt: string(&input, "prompt"),
                                    model: optional(&input, "model")
                                        .or_else(|| self.observed_models.get(&route).cloned()),
                                });
                            } else if name == "TodoWrite" {
                                events.push(ProviderEvent::Plan {
                                    kind: PlanKind::Todo,
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
                                    kind: claude_tool(
                                        &name,
                                        &input,
                                        None,
                                        self.presentations.get(&key),
                                    ),
                                    key,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(usage) = message.get("usage") {
                    let window = self
                        .observed_models
                        .get(&route)
                        .map(|model| claude_context_window(model))
                        .or(self.selected_context_window)
                        .unwrap_or(200_000);
                    let usage = claude_usage(usage, window);
                    events.push(ProviderEvent::ContextUsage(ContextUsage {
                        used_tokens: usage.total,
                        max_tokens: usage.max,
                        auto_compact_threshold: None,
                    }));
                    events.push(ProviderEvent::Usage(usage));
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
                                    self.background_tasks.insert(task.clone());
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
                                    let native = &frame["tool_use_result"];
                                    if native["isAsync"] == true
                                        || native["status"] == "async_launched"
                                        || text.starts_with("Async agent launched successfully.")
                                    {
                                        continue;
                                    }
                                    events.push(ProviderEvent::SubagentFinished {
                                        key,
                                        status,
                                        result: if native["content"].is_array() {
                                            claude_result_text(&native["content"])
                                        } else {
                                            text
                                        },
                                    });
                                } else {
                                    events.push(ProviderEvent::ItemFinished {
                                        kind: claude_tool(
                                            name,
                                            input,
                                            Some(&frame["tool_use_result"]),
                                            self.presentations.get(&key),
                                        ),
                                        key,
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
                let hint = self.authentication_failed.contains(&route).then_some("Claude could not authenticate. For subscription login, run `claude auth login` on this environment's machine, then start a new thread. For API-key authentication, check this instance's configured credentials.");
                let status = claude_terminal_status(frame, hint);
                let success = status == RunStatus::Completed;
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
                if !aborted
                    && (frame["subtype"] != "success" || frame["is_error"] == true || !success)
                {
                    let message = frame["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .find(|error| !error.starts_with("[ede_diagnostic]"))
                        .map(str::to_owned)
                        .or_else(|| claude_result_error(frame, hint))
                        .or_else(|| hint.map(str::to_owned))
                        .or_else(|| optional(frame, "result"))
                        .unwrap_or_default();
                    events.push(ProviderEvent::ItemFinished {
                        key: optional(frame, "uuid").unwrap_or_else(|| "result-error".into()),
                        kind: ProviderItem::Error {
                            message,
                            retrying: false,
                            code: Some(if frame["subtype"] != "success" {
                                string(frame, "subtype")
                            } else {
                                frame["api_error_status"]
                                    .as_i64()
                                    .map(|status| format!("api_error_{status}"))
                                    .unwrap_or_else(|| {
                                        optional(frame, "terminal_reason")
                                            .unwrap_or_else(|| "sdk_result_error".into())
                                    })
                            }),
                            class: Some(
                                if frame["terminal_reason"] == "blocking_limit"
                                    || frame["api_error_status"] == 429
                                    || self.usage_limited.contains(&route)
                                {
                                    "usage_limit"
                                } else {
                                    "provider_error"
                                }
                                .into(),
                            ),
                            retryable: matches!(
                                frame["api_error_status"].as_i64(),
                                Some(429 | 529)
                            )
                            .then_some(true),
                        },
                        text: None,
                        status: ItemStatus::Failed,
                    });
                }
                events.push(ProviderEvent::TurnUsage(normalize_claude_turn_usage(
                    &string(frame, "subtype"),
                    frame.get("usage"),
                    status,
                )));
                if aborted {
                    events.push(ProviderEvent::TurnAborted {
                        reason: string(frame, "terminal_reason"),
                    });
                } else {
                    events.push(ProviderEvent::TurnFinished {
                        status,
                        native_head: None,
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
            child_events(events, &route, &self.parents)?
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
fn claude_result_error(frame: &Value, hint: Option<&str>) -> Option<String> {
    if frame["api_error_status"] == 529 {
        return Some("Claude API is overloaded (529). Try again shortly.".into());
    }
    if frame["api_error_status"] == 429 {
        return Some("Claude API rate limit reached. Try again later.".into());
    }
    Some(
        match string(frame, "terminal_reason").as_str() {
            "api_error" => hint.unwrap_or("Claude gave up after repeated API errors."),
            "malformed_tool_use_exhausted" => "Claude gave up after repeated malformed tool calls.",
            "budget_exhausted" => "Claude stopped: the turn's token budget was exhausted.",
            "structured_output_retry_exhausted" => {
                "Claude could not produce the requested structured output."
            }
            "tool_deferred_unavailable" => {
                "Claude could not resume a deferred tool call: the tool is no longer available."
            }
            "turn_setup_failed" => "Claude could not start the turn.",
            "blocking_limit" => "Claude stopped: a usage limit blocked the request.",
            "rapid_refill_breaker" => {
                "Claude stopped: the context refilled too quickly after compaction."
            }
            "prompt_too_long" => "Claude stopped: the prompt exceeds the model's context window.",
            "image_error" => "Claude stopped: an image in the conversation could not be processed.",
            "model_error" => "Claude stopped: the model returned an error.",
            _ => return None,
        }
        .into(),
    )
}
fn claude_terminal_status(frame: &Value, hint: Option<&str>) -> RunStatus {
    if matches!(
        string(frame, "terminal_reason").as_str(),
        "aborted_tools" | "aborted_streaming"
    ) {
        return RunStatus::Interrupted;
    }
    if frame["subtype"] == "success" {
        return if claude_result_error(frame, hint).is_some()
            || frame["is_error"] == true && hint.is_some()
        {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        };
    }
    let errors = frame["errors"].to_string().to_lowercase();
    if errors.contains("interrupt") {
        RunStatus::Interrupted
    } else if errors.contains("cancel") {
        RunStatus::Cancelled
    } else {
        RunStatus::Failed
    }
}
fn claude_tool_presentation(meta: &Value) -> Option<ToolPresentation> {
    let bounded_text = |key, limit| {
        let text = meta.get(key)?.as_str()?.trim();
        (!text.is_empty() && text.encode_utf16().count() <= limit).then(|| text.to_owned())
    };
    bounded_text("id", 512)?;
    let title = bounded_text("display_name", 160)?;
    let source=bounded_text("server_display_name",160).map(|server| {
        let mut source=json!({"key":format!("mcp:{}",server.to_lowercase()),"name":server,"kind":"integration"});
        if let Some(raw)=meta["icon_url"].as_str().filter(|raw|raw.encode_utf16().count()<=4096)
            && let Ok(url)=url::Url::parse(raw)
            && matches!(url.scheme(),"http"|"https")
            && url.as_str().encode_utf16().count()<=4096
        { source["icon"]=json!({"_tag":"themed-logo","logoUrl":url.as_str()}); }
        Json(source)
    });
    Some(ToolPresentation {
        title: Some(title),
        source,
    })
}
fn claude_tool(
    name: &str,
    input: &Value,
    output: Option<&Value>,
    presentation: Option<&ToolPresentation>,
) -> ProviderItem {
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
            results: output.and_then(|v| v.get("results")).cloned().map(Json),
        },
        _ => ProviderItem::Tool {
            presentation: presentation.cloned().unwrap_or_default(),
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
pub fn claude_context_window(model: &str) -> u64 {
    if matches!(model, "claude-opus-4-6" | "claude-opus-4-7") {
        1_000_000
    } else {
        200_000
    }
}
fn claude_usage(usage: &Value, window: u64) -> TokenUsage {
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached_input = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_creation = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let output = usage["output_tokens"].as_u64().unwrap_or(0);
    TokenUsage {
        input: input + cached_input + cache_creation,
        cached_input,
        output,
        reasoning_output: 0,
        total: input + cached_input + cache_creation + output,
        max: Some(window),
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
        if native_image(file) {
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
