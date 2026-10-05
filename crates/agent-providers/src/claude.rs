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
    /// Rejected usage windows of the current turn and their reset times.
    rejected_limits: BTreeMap<String, Option<i64>>,
    /// Pending API retry per route: progress, message, code and class.
    retries: BTreeMap<String, (RetryProgress, String, Option<String>, String)>,
    /// User-invocable skills the CLI runs as slash commands.
    skills: Vec<String>,
    /// Background task descriptions that name later reports.
    task_descriptions: BTreeMap<String, String>,
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
    /// Skills discovered for the session's workspace.
    pub fn set_skills(&mut self, skills: Vec<String>) {
        self.skills = skills;
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
                note,
                attachments,
                context,
                ..
            } => {
                let options = claude_model_options(selection);
                self.selected_context_window = Some(options.context_window);
                self.text_seen.remove("");
                self.authentication_failed.remove("");
                self.usage_limited.remove("");
                self.rejected_limits.clear();
                self.retries.remove("");
                result.events.push(ProviderEvent::PromptOffered {
                    key: user_uuid.into(),
                });
                result
                    .events
                    .push(ProviderEvent::TurnStarted { native_turn: None });
                let prompt = provider_prompt(
                    text,
                    note.as_deref(),
                    context.as_ref().filter(|_| text.trim() != "/compact"),
                );
                result.outbound.push(claude_user_message(
                    &claude_prompt_effort(&prompt, options.prompt_effort.as_deref()),
                    attachments,
                    user_uuid,
                    false,
                    images,
                    &self.skills,
                )?);
            }
            ProviderCommand::Steer {
                text, attachments, ..
            } => result.outbound.push(claude_user_message(
                text,
                attachments,
                user_uuid,
                true,
                images,
                &self.skills,
            )?),
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
            ProviderCommand::SetModel { selection } => result.outbound.push(self.control.request(
                "set_model",
                json!({"model":claude_model_options(selection).model}),
            )),
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
                    &[],
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
                "api_retry" => {
                    let count = |key: &str| frame[key].as_f64().map(|n| n.max(0.).trunc() as u64);
                    let retry = RetryProgress {
                        attempt: count("attempt").unwrap_or(1).max(1),
                        max_attempts: count("max_retries").map(|n| n.max(1)),
                        delay_ms: count("retry_delay_ms"),
                    };
                    let message =
                        format!("Claude API {}.", string(frame, "error").replace('_', " "));
                    let code = frame["error_status"]
                        .as_f64()
                        .map(|status| format!("api_error_{}", status.trunc() as i64))
                        .or_else(|| optional(frame, "error"));
                    let class = if frame["error_status"] == 429 {
                        "usage_limit"
                    } else if frame["error_status"].is_null() {
                        "transport_error"
                    } else {
                        "provider_error"
                    };
                    self.retries.insert(
                        route.clone(),
                        (retry.clone(), message.clone(), code.clone(), class.into()),
                    );
                    events.push(ProviderEvent::ItemStarted {
                        key: "terminal-failure".into(),
                        kind: ProviderItem::Error {
                            message,
                            retry: Some(retry),
                            code,
                            class: Some(class.into()),
                            retryable: Some(true),
                        },
                    });
                }
                "model_refusal_fallback" => events.push(ProviderEvent::ItemFinished {
                    key: optional(frame, "uuid").unwrap_or_else(|| "fallback".into()),
                    kind: ProviderItem::Notice {
                        message: string(frame, "content"),
                    },
                    text: None,
                    status: ItemStatus::Completed,
                }),
                "background_tasks_changed" => {
                    if let Some(tasks) = frame["tasks"].as_array() {
                        let roster = tasks
                            .iter()
                            .filter(|task| task["task_type"] == "local_bash")
                            .filter_map(|task| {
                                let id = optional(task, "task_id").filter(|id| !id.is_empty())?;
                                let tool = self
                                    .tasks
                                    .get(&id)
                                    .map(|(tool, _)| tool.clone())
                                    .unwrap_or_else(|| id.clone());
                                let monitor = self
                                    .tools
                                    .get(&tool)
                                    .is_some_and(|(name, _)| name == "Monitor");
                                Some(BackgroundEntry {
                                    key: id,
                                    tool,
                                    kind: if monitor {
                                        BackgroundKind::Monitor
                                    } else {
                                        BackgroundKind::Command
                                    },
                                    description: string(task, "description").trim().to_owned(),
                                })
                            })
                            .collect::<Vec<_>>();
                        for entry in &roster {
                            self.tasks
                                .entry(entry.key.clone())
                                .or_insert_with(|| (entry.tool.clone(), false));
                            self.background_tasks.insert(entry.key.clone());
                            if !entry.description.is_empty() {
                                self.task_descriptions
                                    .entry(entry.key.clone())
                                    .or_insert_with(|| entry.description.clone());
                            }
                        }
                        events.push(ProviderEvent::BackgroundRoster { tasks: roster });
                    }
                }
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
                    } else if frame["is_backgrounded"] != false {
                        self.background_tasks.insert(task.clone());
                        self.task_descriptions
                            .insert(task.clone(), string(frame, "description"));
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
                            exit_code: None,
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
                            let description = self
                                .task_descriptions
                                .remove(&task)
                                .unwrap_or_else(|| string(frame, "summary"));
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
                                description,
                                exit_code: None,
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
                if let Some(recovered) = self.resolve_retry(&route, ItemStatus::Completed) {
                    events.push(recovered);
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
                    if block["tool_use_id"].is_string() {
                        continue;
                    }
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
                        _ if block["id"].is_string()
                            && block["name"].is_string()
                            && block.get("input").is_some() =>
                        {
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
                for block in content
                    .iter()
                    .filter(|block| block["tool_use_id"].is_string())
                {
                    self.tool_result(block, None, &route, &mut events)?;
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
                    let results = content
                        .iter()
                        .filter(|block| block["tool_use_id"].is_string())
                        .collect::<Vec<_>>();
                    let structured = (results.len() == 1
                        && results[0]["type"] == "tool_result"
                        && !frame["tool_use_result"].is_null())
                    .then_some(&frame["tool_use_result"]);
                    for block in results {
                        self.tool_result(block, structured, &route, &mut events)?;
                    }
                }
            }
            "result" => {
                let authentication = self.authentication_failed.contains(&route);
                let usage_limited = !authentication
                    && (!self.rejected_limits.is_empty() || self.usage_limited.contains(&route))
                    && (frame["subtype"] != "success"
                        || frame["api_error_status"].is_null()
                        || frame["api_error_status"] == 429)
                    && matches!(
                        frame["terminal_reason"].as_str(),
                        None | Some("api_error" | "blocking_limit")
                    );
                let hint = if authentication {
                    Some(
                        "Claude could not authenticate. For subscription login, run `claude auth login` on this environment's machine, then start a new thread. For API-key authentication, check this instance's configured credentials.",
                    )
                } else if usage_limited {
                    Some(
                        "Claude usage limit reached. Send the message again once the limit resets.",
                    )
                } else {
                    None
                };
                let status = claude_terminal_status(frame, hint);
                let aborted = matches!(
                    string(frame, "terminal_reason").as_str(),
                    "aborted_streaming" | "aborted_tools"
                );
                // Failed result text belongs on the failure, never in an answer.
                if frame["subtype"] == "success"
                    && frame["is_error"] != true
                    && claude_terminal_status(frame, None) != RunStatus::Failed
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
                if !aborted && status == RunStatus::Failed {
                    let listed = frame["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .find(|error| !error.starts_with("[ede_diagnostic]"))
                        .map(str::to_owned)
                        .or_else(|| claude_result_error(frame, hint));
                    let message = if frame["subtype"] != "success" {
                        listed.unwrap_or_else(|| {
                            frame["errors"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                    } else {
                        listed
                            .or_else(|| hint.map(str::to_owned))
                            .or_else(|| optional(frame, "result"))
                            .unwrap_or_default()
                    };
                    events.push(ProviderEvent::ItemFinished {
                        key: "terminal-failure".into(),
                        kind: ProviderItem::Error {
                            message,
                            retry: self.retries.remove(&route).map(|(retry, ..)| retry),
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
                                    || frame["subtype"] == "success"
                                        && frame["api_error_status"] == 429
                                    || usage_limited
                                {
                                    "usage_limit"
                                } else {
                                    "provider_error"
                                }
                                .into(),
                            ),
                            retryable: (frame["subtype"] == "success"
                                && matches!(frame["api_error_status"].as_i64(), Some(429 | 529)))
                            .then_some(true),
                        },
                        text: None,
                        status: ItemStatus::Failed,
                    });
                }
                if let Some(settled) = self.resolve_retry(
                    &route,
                    match status {
                        RunStatus::Completed => ItemStatus::Completed,
                        RunStatus::Cancelled => ItemStatus::Cancelled,
                        _ => ItemStatus::Interrupted,
                    },
                ) {
                    events.push(settled);
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
            "rate_limit_event" if frame["rate_limit_info"].is_object() => {
                let info = &frame["rate_limit_info"];
                let overage = matches!(
                    info["overageStatus"].as_str(),
                    Some("allowed" | "allowed_warning")
                ) || info["isUsingOverage"] == true
                    || info["overageInUse"] == true;
                let limit = optional(info, "rateLimitType").unwrap_or_else(|| "unknown".into());
                let resets_at = info["resetsAt"].as_f64().map(|value| value as i64);
                if info["status"] == "rejected" && !overage {
                    self.rejected_limits.insert(limit.clone(), resets_at);
                    events.push(ProviderEvent::ItemFinished {
                        key: format!(
                            "usage-limit:{limit}:{}",
                            resets_at.map_or("unknown".into(), |at| at.to_string())
                        ),
                        kind: ProviderItem::UsageLimit {
                            limit: Some(limit),
                            resets_at,
                        },
                        text: None,
                        status: ItemStatus::Completed,
                    });
                } else if overage
                    || matches!(info["status"].as_str(), Some("allowed" | "allowed_warning"))
                {
                    self.rejected_limits.remove(&limit);
                }
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
impl ClaudeProtocol {
    fn resolve_retry(&mut self, route: &str, status: ItemStatus) -> Option<ProviderEvent> {
        let (retry, message, code, class) = self.retries.remove(route)?;
        Some(ProviderEvent::ItemFinished {
            key: "terminal-failure".into(),
            kind: ProviderItem::Error {
                message,
                retry: Some(retry),
                code,
                class: Some(class),
                retryable: Some(true),
            },
            text: None,
            status,
        })
    }
    /// A tool result from either an assistant or a user message. A result
    /// whose tool use was never seen starts a tool named for its result type.
    fn tool_result(
        &mut self,
        block: &Value,
        structured: Option<&Value>,
        route: &str,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProtocolError> {
        let key = required(block, "tool_use_id")?;
        let content = &block["content"];
        let text = claude_result_text(content);
        let status = if claude_result_failed(block) {
            ItemStatus::Failed
        } else {
            ItemStatus::Completed
        };
        if !self.tools.contains_key(&key) {
            let name = claude_result_tool_name(&string(block, "type"));
            self.tools.insert(key.clone(), (name.clone(), json!({})));
            self.parents.insert(key.clone(), route.into());
            events.push(ProviderEvent::ItemStarted {
                key: key.clone(),
                kind: claude_tool(&name, &json!({}), None, None),
            });
        }
        let (name, input) = self.tools[&key].clone();
        let native = structured.unwrap_or(&Value::Null);
        if let Some(task) =
            optional(native, "backgroundTaskId").or_else(|| optional(native, "taskId"))
            && name != "Agent"
            && name != "Task"
        {
            self.tasks.insert(task.clone(), (key.clone(), false));
            self.background_tasks.insert(task.clone());
            self.task_descriptions.insert(
                task.clone(),
                optional(&input, "description").unwrap_or_else(|| name.clone()),
            );
            events.push(ProviderEvent::BackgroundTask {
                key: task,
                tool: key.clone(),
                kind: if name == "Monitor" {
                    BackgroundKind::Monitor
                } else {
                    BackgroundKind::Command
                },
                description: optional(&input, "description").unwrap_or_else(|| name.clone()),
                status: None,
                summary: None,
                exit_code: None,
            });
        }
        if name == "Agent" || name == "Task" {
            if native["isAsync"] == true
                || native["status"] == "async_launched"
                || text.starts_with("Async agent launched successfully.")
            {
                return Ok(());
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
            return Ok(());
        }
        let output = structured.unwrap_or(content);
        let kind = claude_tool(&name, &input, Some(output), self.presentations.get(&key));
        let text = match &kind {
            ProviderItem::Command { .. } => claude_command_output(output).unwrap_or(text),
            _ => text,
        };
        events.push(ProviderEvent::ItemFinished {
            kind,
            key,
            text: Some(text),
            status,
        });
        Ok(())
    }
}
/// Bash output joins its non-blank stdout and stderr.
fn claude_command_output(output: &Value) -> Option<String> {
    if !(output["stdout"].is_string() || output["stderr"].is_string()) {
        return None;
    }
    let text = [&output["stdout"], &output["stderr"]]
        .into_iter()
        .filter_map(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}
fn claude_result_failed(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("tool_result" | "mcp_tool_result") => block["is_error"] == true,
        _ => {
            let content = &block["content"];
            content.is_object()
                && matches!(
                    content["type"].as_str(),
                    Some(
                        "bash_code_execution_tool_result_error"
                            | "code_execution_tool_result_error"
                            | "text_editor_code_execution_tool_result_error"
                            | "tool_search_tool_result_error"
                            | "web_fetch_tool_result_error"
                            | "web_search_tool_result_error"
                    )
                )
        }
    }
}
fn claude_result_tool_name(kind: &str) -> String {
    match kind {
        "bash_code_execution_tool_result" => "bash_code_execution",
        "code_execution_tool_result" => "code_execution",
        "advisor_tool_result" => "advisor",
        "mcp_tool_result" => "mcp_tool",
        "text_editor_code_execution_tool_result" => "text_editor_code_execution",
        "tool_search_tool_result" => "tool_search",
        "web_fetch_tool_result" => "web_fetch",
        "web_search_tool_result" => "web_search",
        _ => "tool",
    }
    .into()
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
        let text = meta
            .get(key)?
            .as_str()?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        (!text.is_empty() && text.encode_utf16().count() <= limit).then_some(text)
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
        ..ToolPresentation::default()
    })
}
fn claude_tool(
    name: &str,
    input: &Value,
    output: Option<&Value>,
    presentation: Option<&ToolPresentation>,
) -> ProviderItem {
    let normalized = name
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .collect::<String>();
    match normalized.as_str() {
        "bash" => ProviderItem::Command {
            command: string(input, "command"),
            cwd: None,
            exit_code: None,
        },
        "edit" | "write" | "multiedit" | "notebookedit" => ProviderItem::FileChange {
            changes: Json(input.clone()),
        },
        "websearch" | "webfetch" => ProviderItem::WebSearch {
            query: optional(input, "query")
                .or_else(|| optional(input, "url"))
                .unwrap_or_default(),
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
/// The prompt, with a known skill run as the trailing slash-command block
/// after any images, as the CLI expands only that block.
pub fn claude_user_message(
    text: &str,
    attachments: &[Attachment],
    uuid: &str,
    steer: bool,
    images: &[PreparedImage],
    skills: &[String],
) -> Result<Value, ProtocolError> {
    let text = attachment_text(text, attachments);
    let dispatch = claude_skill_dispatch(&text, skills);
    let content = if attachments.is_empty() && dispatch.is_none() {
        json!(text)
    } else {
        let mut content = vec![];
        if let Some(leading) = dispatch.as_ref().and_then(|d| d.leading_text.as_ref()) {
            content.push(json!({"type":"text","text":leading}));
        }
        for file in attachments
            .iter()
            .filter(|file| file.kind == AttachmentKind::Image)
        {
            if !native_image(file) {
                return Err(ProtocolError::Invalid(format!(
                    "Unsupported Claude image attachment type '{}'",
                    file.mime_type
                )));
            }
            let image = images
                .iter()
                .find(|image| image.attachment_id == file.id)
                .ok_or_else(|| {
                    ProtocolError::Invalid(format!("missing prepared image {}", file.id))
                })?;
            content.push(json!({"type":"image","source":{"type":"base64","media_type":image.mime_type,"data":image.base64}}));
        }
        match dispatch {
            Some(dispatch) => content.push(json!({"type":"text","text":dispatch.command_text})),
            None if !text.is_empty() => content.push(json!({"type":"text","text":text})),
            None => {}
        }
        json!(content)
    };
    let mut message = json!({"type":"user","uuid":uuid,"message":{"role":"user","content":content},"parent_tool_use_id":null});
    if steer {
        message["priority"] = json!("now");
    }
    Ok(message)
}
