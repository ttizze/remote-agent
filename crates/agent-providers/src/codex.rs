use crate::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct WireContext {
    pub cwd: String,
    pub client_name: String,
    pub client_version: String,
}
#[derive(Debug, Clone)]
enum Pending {
    Initialize,
    Thread {
        start: Value,
    },
    RevertRead {
        thread: String,
        head: Option<String>,
    },
    Operation(String),
}
/// One native app-server session's RPC correlation, with no application entities.
#[derive(Debug, Default)]
pub struct CodexProtocol {
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    thread: Option<String>,
    turn: Option<String>,
    /// Native child thread -> native parent, used solely to route notifications.
    children: BTreeMap<String, String>,
}
impl CodexProtocol {
    fn request(&mut self, method: &str, params: Value, pending: Pending) -> Value {
        self.next_id += 1;
        self.pending.insert(self.next_id, pending);
        json!({"id":self.next_id,"method":method,"params":params})
    }
    pub fn initialize(&mut self, context: &WireContext) -> Value {
        self.request("initialize",json!({"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["turn/diff/updated"]},"clientInfo":{"name":context.client_name,"title":context.client_name,"version":context.client_version}}),Pending::Initialize)
    }
    pub fn command(
        &mut self,
        command: &ProviderCommand,
        context: &WireContext,
    ) -> Result<Vec<Value>, ProtocolError> {
        let frame = match command {
            ProviderCommand::Start {
                selection,
                runtime_mode,
                interaction_mode,
                text,
                attachments,
                native_thread,
                context: handoff,
                ..
            } => {
                let mut input = vec![];
                for file in attachments {
                    if native_image_mime(&file.mime_type) {
                        input.push(json!({"type":"localImage","path":file.path}));
                    }
                }
                let text = if handoff.is_empty() {
                    text.clone()
                } else {
                    format!("{handoff}\n\n{text}")
                };
                let text = attachment_text(&text, attachments);
                input.push(json!({"type":"text","text":text}));
                let (approval, reviewer, sandbox) = codex_runtime(*runtime_mode);
                let mut start = json!({"input":input,"cwd":context.cwd,"model":selection.model,"summary":"detailed","approvalPolicy":approval,"approvalsReviewer":reviewer,"sandboxPolicy":{"type":sandbox}});
                if let Some(effort) = selection.options.get("reasoningEffort") {
                    start["effort"] = json!(effort);
                }
                if let Some(tier) = selection.options.get("serviceTier") {
                    start["serviceTier"] = json!(tier);
                }
                if *interaction_mode == InteractionMode::Plan {
                    start["collaborationMode"] = json!({"mode":"plan","settings":{"model":selection.model,"reasoning_effort":selection.options.get("reasoningEffort").map(String::as_str).unwrap_or("medium")}});
                }
                if let Some(thread) = self
                    .thread
                    .clone()
                    .filter(|id| native_thread.as_ref().is_none_or(|native| native == id))
                {
                    start["threadId"] = json!(thread);
                    self.request("turn/start", start, Pending::Operation("turn/start".into()))
                } else if let Some(thread) = native_thread {
                    self.request("thread/resume",json!({"threadId":thread,"excludeTurns":true,"model":selection.model,"cwd":context.cwd}),Pending::Thread { start })
                } else {
                    self.request("thread/start",json!({"model":selection.model,"cwd":context.cwd,"config":{"tools.update_plan.enabled":true}}),Pending::Thread { start })
                }
            }
            ProviderCommand::Steer { text, attachments } => {
                let thread = self
                    .thread
                    .as_ref()
                    .ok_or_else(|| ProtocolError::Invalid("no native thread".into()))?;
                let turn = self.turn.as_ref().ok_or_else(|| ProtocolError::Remote {
                    operation: "turn/steer".into(),
                    message: "No active turn".into(),
                    turn_completed: true,
                })?;
                let input = json!([{"type":"text","text":attachment_text(text,attachments)}]);
                self.request(
                    "turn/steer",
                    json!({"threadId":thread,"expectedTurnId":turn,"input":input}),
                    Pending::Operation("turn/steer".into()),
                )
            }
            ProviderCommand::Interrupt => {
                if let (Some(thread), Some(turn)) = (&self.thread, &self.turn) {
                    self.request(
                        "turn/interrupt",
                        json!({"threadId":thread,"turnId":turn}),
                        Pending::Operation("turn/interrupt".into()),
                    )
                } else {
                    return Ok(vec![]);
                }
            }
            ProviderCommand::Respond {
                native_key,
                decision,
                answers,
                ..
            } => {
                let id: Value = serde_json::from_str(native_key)
                    .map_err(|_| ProtocolError::Invalid("invalid native request id".into()))?;
                let result = if let Some(answers) = answers {
                    json!({"answers":answers.iter().map(|(k,v)| (k.clone(),json!({"answers":v}))).collect::<BTreeMap<_,_>>()})
                } else {
                    json!({"decision":match decision.unwrap_or(ApprovalDecision::Cancel) { ApprovalDecision::Accept=>"accept",ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways=>"acceptForSession",ApprovalDecision::Decline=>"decline",ApprovalDecision::Cancel=>"cancel" }})
                };
                json!({"id":id,"result":result})
            }
            ProviderCommand::Rollback {
                native_thread,
                absolute_head,
            } => self.request(
                "thread/read",
                json!({"threadId":native_thread,"includeTurns":true}),
                Pending::RevertRead {
                    thread: native_thread.clone(),
                    head: absolute_head.clone(),
                },
            ),
            ProviderCommand::Fork {
                native_thread,
                through_turn,
            } => {
                let mut params = json!({"threadId":native_thread,"cwd":context.cwd});
                if let Some(turn) = through_turn {
                    params["lastTurnId"] = json!(turn);
                }
                self.request(
                    "thread/fork",
                    params,
                    Pending::Operation("thread/fork".into()),
                )
            }
            ProviderCommand::Compact { native_thread } => {
                let thread = native_thread
                    .as_ref()
                    .or(self.thread.as_ref())
                    .ok_or_else(|| {
                        ProtocolError::Invalid("compact requires native thread".into())
                    })?;
                self.request(
                    "thread/compact/start",
                    json!({"threadId":thread}),
                    Pending::Operation("thread/compact/start".into()),
                )
            }
            // Codex selection/runtime parameters are authoritative on turn/start.
            ProviderCommand::SetModel { .. } | ProviderCommand::SetRuntimeMode { .. } => {
                return Ok(vec![]);
            }
        };
        Ok(vec![frame])
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Translation, ProtocolError> {
        let mut output = Translation::default();
        if frame.get("method").is_none() {
            let Some(id) = frame.get("id").and_then(Value::as_u64) else {
                return Ok(output);
            };
            let Some(pending) = self.pending.remove(&id) else {
                return Ok(output);
            };
            if let Some(error) = frame.get("error") {
                let message = string(error, "message");
                let operation = match pending {
                    Pending::Initialize => "initialize".into(),
                    Pending::Thread { .. } => "thread/start".into(),
                    Pending::RevertRead { .. } => "thread/read".into(),
                    Pending::Operation(operation) => operation,
                };
                return Err(ProtocolError::Remote {
                    turn_completed: completed_steer_error(&message),
                    operation,
                    message,
                });
            }
            let result = &frame["result"];
            output.replies.push(NativeReply {
                request: id.to_string(),
                operation: match &pending {
                    Pending::Initialize => "initialize".into(),
                    Pending::Thread { .. } => "thread/start".into(),
                    Pending::RevertRead { .. } => "thread/read".into(),
                    Pending::Operation(operation) => operation.clone(),
                },
                result: Json(result.clone()),
            });
            match pending {
                Pending::Initialize => output.outbound.push(json!({"method":"initialized"})),
                Pending::Thread { mut start } => {
                    let thread = required(&result["thread"], "id")?;
                    self.thread = Some(thread.clone());
                    output.events.push(ProviderEvent::SessionReady {
                        native_thread: thread.clone(),
                    });
                    if !start.is_null() {
                        start["threadId"] = json!(thread);
                        output.outbound.push(self.request(
                            "turn/start",
                            start,
                            Pending::Operation("turn/start".into()),
                        ));
                    }
                }
                Pending::RevertRead { thread, head } => {
                    let turns = result["thread"]["turns"].as_array().ok_or_else(|| {
                        ProtocolError::Invalid("missing native turn history".into())
                    })?;
                    let after = absolute_revert_count(turns, head.as_deref())?;
                    if after > 0 {
                        output.outbound.push(self.request(
                            "thread/revert",
                            json!({"threadId":thread,"numTurns":after}),
                            Pending::Operation("thread/revert".into()),
                        ));
                    }
                }
                Pending::Operation(operation) => {
                    if operation == "turn/start"
                        && let Some(turn) = optional(&result["turn"], "id")
                    {
                        self.turn = Some(turn);
                    }
                }
            }
            return Ok(output);
        }
        let method = required(frame, "method")?;
        let p = &frame["params"];
        let native_thread = optional(p, "threadId");
        let mut events = vec![];
        match method.as_str() {
            "thread/started" => {
                let thread = required(&p["thread"], "id")?;
                if self.thread.is_none() {
                    self.thread = Some(thread.clone());
                }
                if self.thread.as_ref() == Some(&thread) {
                    events.push(ProviderEvent::SessionReady {
                        native_thread: thread,
                    });
                }
            }
            "turn/started" => {
                let turn = required(&p["turn"], "id")?;
                if native_thread == self.thread {
                    self.turn = Some(turn.clone());
                }
                events.push(ProviderEvent::TurnStarted {
                    native_turn: Some(turn),
                });
            }
            "turn/completed" => {
                let turn = &p["turn"];
                if native_thread == self.thread {
                    self.turn = None;
                }
                events.push(ProviderEvent::TurnFinished {
                    status: terminal(&string(turn, "status")),
                    native_head: optional(turn, "id"),
                });
            }
            "item/agentMessage/delta" => events.push(ProviderEvent::TextDelta {
                key: required(p, "itemId")?,
                kind: ProviderItem::Text,
                text: string(p, "delta"),
            }),
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                events.push(ProviderEvent::TextDelta {
                    key: required(p, "itemId")?,
                    kind: ProviderItem::Reasoning,
                    text: string(p, "delta"),
                })
            }
            "item/commandExecution/outputDelta" => events.push(ProviderEvent::TextDelta {
                key: required(p, "itemId")?,
                kind: ProviderItem::Command {
                    command: String::new(),
                    cwd: None,
                    exit_code: None,
                },
                text: string(p, "delta"),
            }),
            "item/plan/delta" => events.push(ProviderEvent::PlanDelta {
                key: required(p, "itemId")?,
                text: string(p, "delta"),
            }),
            "turn/plan/updated" => events.push(ProviderEvent::Plan {
                key: "todo".into(),
                markdown: optional(p, "explanation").unwrap_or_default(),
                steps: p["plan"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| PlanStep {
                        text: string(s, "step"),
                        status: string(s, "status"),
                    })
                    .collect(),
            }),
            "thread/tokenUsage/updated" => {
                let usage = &p["tokenUsage"]["total"];
                events.push(ProviderEvent::Usage(TokenUsage {
                    input: usage["inputTokens"].as_u64().unwrap_or(0),
                    cached_input: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                    output: usage["outputTokens"].as_u64().unwrap_or(0),
                    reasoning_output: usage["reasoningOutputTokens"].as_u64().unwrap_or(0),
                    total: usage["totalTokens"].as_u64().unwrap_or(0),
                    max: p["tokenUsage"]["modelContextWindow"].as_u64(),
                }));
            }
            "item/started" | "item/completed" => {
                let item = &p["item"];
                let key = required(item, "id")?;
                let completed = method == "item/completed";
                match string(item, "type").as_str() {
                    "userMessage" => {
                        if completed {
                            events.push(ProviderEvent::UserMessage {
                                key,
                                text: item["content"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|b| b["text"].as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            });
                        }
                    }
                    "subAgentActivity" => {
                        let child = required(item, "agentThreadId")?;
                        self.children.insert(
                            child.clone(),
                            native_thread
                                .clone()
                                .filter(|p| self.thread.as_ref() != Some(p))
                                .unwrap_or_default(),
                        );
                        match string(item, "kind").as_str() {
                            "started" => events.push(ProviderEvent::SubagentStarted {
                                key: child,
                                parent: native_thread
                                    .clone()
                                    .filter(|id| self.children.contains_key(id)),
                                prompt: String::new(),
                                model: optional(item, "model"),
                            }),
                            "completed" | "closed" => {
                                events.push(ProviderEvent::SubagentFinished {
                                    key: child,
                                    status: ItemStatus::Completed,
                                    result: string(item, "message"),
                                })
                            }
                            _ => {}
                        }
                        events.push(ProviderEvent::SubagentNamed {
                            key: required(item, "agentThreadId")?,
                            title: string(item, "agentPath"),
                        });
                    }
                    "collabAgentToolCall" => {
                        let tool = string(item, "tool");
                        let receivers = item["receiverThreadIds"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        for child in receivers {
                            self.children.insert(
                                child.clone(),
                                native_thread
                                    .clone()
                                    .filter(|p| self.thread.as_ref() != Some(p))
                                    .unwrap_or_default(),
                            );
                            if matches!(tool.as_str(), "spawnAgent" | "sendInput" | "resumeAgent") {
                                events.push(ProviderEvent::SubagentStarted {
                                    key: child.clone(),
                                    parent: native_thread
                                        .clone()
                                        .filter(|id| self.children.contains_key(id)),
                                    prompt: string(item, "prompt"),
                                    model: optional(item, "model"),
                                });
                            }
                            let status = &item["agentsStates"][&child];
                            if completed
                                && status
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .is_some_and(|s| {
                                        matches!(s, "completed" | "errored" | "shutdown")
                                    })
                            {
                                events.push(ProviderEvent::SubagentFinished {
                                    key: child,
                                    status: item_status(&string(status, "status")),
                                    result: string(status, "message"),
                                });
                            }
                        }
                    }
                    "plan" => events.push(ProviderEvent::Plan {
                        key,
                        markdown: string(item, "text"),
                        steps: vec![],
                    }),
                    kind => {
                        if let Some((kind, text)) = codex_item(kind, item) {
                            events.push(if completed {
                                ProviderEvent::ItemFinished {
                                    key,
                                    kind,
                                    text,
                                    status: item_status(&string(item, "status")),
                                }
                            } else {
                                ProviderEvent::ItemStarted { key, kind }
                            });
                        }
                    }
                }
            }
            "item/tool/requestUserInput" => events.push(ProviderEvent::RequestOpened {
                key: frame["id"].to_string(),
                body: RequestBody::Questions {
                    questions: questions(&p["questions"]),
                },
                capability: ResponseCapability::Live,
            }),
            "serverRequest/resolved" => events.push(ProviderEvent::RequestClosed {
                key: p["requestId"].to_string(),
            }),
            "item/commandExecution/requestApproval"
            | "execCommandApproval"
            | "item/fileChange/requestApproval"
            | "applyPatchApproval"
            | "item/permissions/requestApproval"
            | "mcpServer/elicitation/request" => {
                let kind = if method.contains("fileChange") || method == "applyPatchApproval" {
                    "file-change"
                } else if method.contains("permission") {
                    "permission"
                } else if method.starts_with("mcp") {
                    "mcp-elicitation"
                } else {
                    "command"
                };
                let decisions = p["availableDecisions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| match v.as_str() {
                                Some("accept") => Some(ApprovalDecision::Accept),
                                Some("acceptForSession") => {
                                    Some(ApprovalDecision::AcceptForSession)
                                }
                                Some("decline") => Some(ApprovalDecision::Decline),
                                Some("cancel") => Some(ApprovalDecision::Cancel),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| {
                        vec![
                            ApprovalDecision::Accept,
                            ApprovalDecision::AcceptForSession,
                            ApprovalDecision::Decline,
                            ApprovalDecision::Cancel,
                        ]
                    });
                events.push(ProviderEvent::RequestOpened {
                    key: frame["id"].to_string(),
                    body: RequestBody::Approval {
                        kind: kind.into(),
                        title: optional(p, "command").unwrap_or_else(|| kind.into()),
                        detail: optional(p, "reason"),
                        options: decisions
                            .into_iter()
                            .map(|decision| ApprovalOption {
                                label: format!("{decision:?}"),
                                decision,
                            })
                            .collect(),
                        input: Json(p.clone()),
                    },
                    capability: ResponseCapability::Live,
                });
            }
            "error" => {
                let retrying = p["willRetry"].as_bool().unwrap_or(false);
                let message = string(&p["error"], "message");
                events.push(ProviderEvent::ItemFinished {
                    key: format!("error:{}", self.next_id),
                    kind: ProviderItem::Error {
                        message,
                        retrying,
                        code: optional(&p["error"], "codexErrorInfo"),
                        class: Some("provider_error".into()),
                        retryable: retrying.then_some(true),
                    },
                    text: None,
                    status: ItemStatus::Failed,
                });
            }
            // Account, process and diagnostic notifications do not alter a conversation.
            _ => {}
        }
        if let Some(thread) = native_thread.filter(|t| self.thread.as_ref() != Some(t)) {
            output.events = child_events(events, &thread, &self.children)?;
        } else {
            output.events = events;
        }
        Ok(output)
    }
}
fn codex_runtime(mode: RuntimeMode) -> (&'static str, &'static str, &'static str) {
    match mode {
        RuntimeMode::ApprovalRequired => ("untrusted", "user", "readOnly"),
        RuntimeMode::AutoAcceptEdits => ("on-request", "user", "workspaceWrite"),
        RuntimeMode::Auto => ("on-request", "auto_review", "workspaceWrite"),
        RuntimeMode::FullAccess => ("never", "user", "dangerFullAccess"),
    }
}
fn codex_item(kind: &str, item: &Value) -> Option<(ProviderItem, Option<String>)> {
    Some(match kind {
        "agentMessage" => (ProviderItem::Text, optional(item, "text")),
        "reasoning" => (
            ProviderItem::Reasoning,
            item["summary"].as_array().map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        ),
        "commandExecution" => (
            ProviderItem::Command {
                command: string(item, "command"),
                cwd: optional(item, "cwd"),
                exit_code: item["exitCode"].as_i64(),
            },
            optional(item, "aggregatedOutput"),
        ),
        "fileChange" => (
            ProviderItem::FileChange {
                changes: Json(item["changes"].clone()),
            },
            None,
        ),
        "webSearch" => (
            ProviderItem::WebSearch {
                query: string(item, "query"),
            },
            None,
        ),
        "dynamicToolCall" | "mcpToolCall" => (
            ProviderItem::Tool {
                name: optional(item, "tool")
                    .or_else(|| optional(item, "toolName"))
                    .unwrap_or_default(),
                input: Json(item["arguments"].clone()),
                output: item.get("result").cloned().map(Json),
            },
            None,
        ),
        "contextCompaction" => (
            ProviderItem::Compaction {
                before: item["beforeTokenCount"].as_u64(),
                after: item["afterTokenCount"].as_u64(),
            },
            None,
        ),
        _ => return None,
    })
}
#[cfg(test)]
impl CodexProtocol {
    pub(crate) fn replay_outbound(&mut self, frame: &Value) {
        if let Some(id) = frame["id"].as_u64() {
            let method = string(frame, "method");
            let pending = match method.as_str() {
                "initialize" => Pending::Initialize,
                "thread/start" | "thread/resume" | "thread/fork" => {
                    Pending::Thread { start: Value::Null }
                }
                _ => Pending::Operation(method),
            };
            self.next_id = self.next_id.max(id);
            self.pending.insert(id, pending);
        }
    }
}
pub fn completed_steer_error(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "turn completed",
        "no active turn",
        "cannot steer a compact turn",
        "expected active turn id",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}
/// Compute a relative wire operation from an absolute desired head and a fresh
/// native read. Never skip a missing head or count application-only turns.
pub fn absolute_revert_count(turns: &[Value], head: Option<&str>) -> Result<usize, ProtocolError> {
    if let Some(head) = head {
        let index = turns
            .iter()
            .position(|turn| turn["id"].as_str() == Some(head))
            .ok_or_else(|| ProtocolError::MissingBoundary(head.into()))?;
        Ok(turns.len() - index - 1)
    } else {
        Ok(turns.len())
    }
}
pub fn attachment_text(text: &str, attachments: &[Attachment]) -> String {
    let mut text = text.to_owned();
    for file in attachments {
        let kind = if file.mime_type.starts_with("image/") {
            "image"
        } else {
            "file"
        };
        if file.path.is_empty() {
            continue;
        }
        let context = format!(
            "[Attached {kind} \"{}\" is saved at: {}]",
            file.name, file.path
        );
        let candidate = if text.is_empty() {
            context
        } else {
            format!("{text}\n\n{context}")
        };
        if candidate.encode_utf16().count() <= 120_000 {
            text = candidate;
        }
    }
    text
}
pub fn native_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}
