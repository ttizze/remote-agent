use crate::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
        history: Option<HistoricalContext>,
    },
    Inject {
        start: Value,
        history: HistoricalContext,
    },
    RevertRead {
        thread: String,
        head: Option<String>,
    },
    RevertResume {
        thread: String,
        head: Option<String>,
    },
    RevertPage {
        thread: String,
        head: Option<String>,
        before: Option<String>,
        visited: BTreeSet<Option<String>>,
    },
    Operation(String),
}
/// One native app-server session's RPC correlation, with no application entities.
#[derive(Debug, Default)]
pub struct CodexProtocol {
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    thread: Option<String>,
    turns: BTreeMap<String, String>,
    reasoning_parts: BTreeMap<String, BTreeMap<(String, u64), String>>,
    processes: BTreeMap<String, BTreeSet<String>>,
    interrupt_pending: BTreeSet<String>,
    stop_before_thread: bool,
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
                    if native_image(file) {
                        input.push(json!({"type":"localImage","path":file.path}));
                    }
                }
                let text = attachment_text(text, attachments);
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
                    .filter(|id| native_thread.as_ref() == Some(id))
                {
                    start["threadId"] = json!(thread);
                    self.start_or_inject(start, handoff.clone())
                } else if let Some(thread) = native_thread {
                    self.request("thread/resume",json!({"threadId":thread,"excludeTurns":true,"model":selection.model,"cwd":context.cwd}),Pending::Thread { start, history: handoff.clone() })
                } else {
                    self.request("thread/start",json!({"model":selection.model,"cwd":context.cwd,"config":{"tools.update_plan.enabled":true}}),Pending::Thread { start, history: handoff.clone() })
                }
            }
            ProviderCommand::Steer { text, attachments } => {
                let thread = self
                    .thread
                    .as_ref()
                    .ok_or_else(|| ProtocolError::Invalid("no native thread".into()))?;
                let turn = self
                    .turns
                    .get(thread)
                    .ok_or_else(|| ProtocolError::Remote {
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
            ProviderCommand::Interrupt {
                native_thread,
                native_turn,
            } => {
                let thread = native_thread.as_ref().or(self.thread.as_ref()).cloned();
                if let Some(thread) = thread {
                    let turn = native_turn.as_ref().or(self.turns.get(&thread)).cloned();
                    if self.pending.values().any(|pending| matches!(pending,Pending::Inject {start,..} if start["threadId"] == thread)) {
                        self.stop_before_thread = true;
                    }
                    if turn.is_none()
                        && self
                            .pending
                            .values()
                            .any(|p| matches!(p, Pending::Operation(op) if op == "turn/start"))
                    {
                        self.interrupt_pending.insert(thread.clone());
                    }
                    return Ok(self.interrupt(&thread, turn.as_deref()));
                }
                if self
                    .pending
                    .values()
                    .any(|p| matches!(p, Pending::Thread { .. }))
                {
                    self.stop_before_thread = true;
                }
                return Ok(vec![]);
            }
            ProviderCommand::Respond {
                native_key,
                decision,
                answers,
                input,
            } => {
                let id: Value = serde_json::from_str(native_key)
                    .map_err(|_| ProtocolError::Invalid("invalid native request id".into()))?;
                let result = if let Some(answers) = answers {
                    json!({"answers":answers.iter().map(|(k,v)| (k.clone(),json!({"answers":v.choices()}))).collect::<BTreeMap<_,_>>()})
                } else if let Some(payload) = input
                    .as_ref()
                    .filter(|input| input.0.get("permissions").is_some())
                {
                    let decision = decision.unwrap_or(ApprovalDecision::Cancel);
                    if matches!(
                        decision,
                        ApprovalDecision::Accept | ApprovalDecision::AcceptForSession
                    ) {
                        json!({"permissions":payload.0["permissions"],"scope":if decision==ApprovalDecision::AcceptForSession {"session"} else {"turn"}})
                    } else {
                        json!({"permissions":{},"scope":"turn"})
                    }
                } else if let Some(payload) = input.as_ref().filter(|input| {
                    input.0.get("serverName").is_some()
                        && (input.0.get("requestedSchema").is_some()
                            || input.0.get("mode").is_some())
                }) {
                    mcp_elicitation_response(
                        &payload.0,
                        decision.unwrap_or(ApprovalDecision::Cancel),
                    )
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
                json!({"threadId":native_thread,"includeTurns":false}),
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
    fn revert_page(
        &mut self,
        thread: String,
        head: Option<String>,
        before: Option<String>,
        cursor: Option<String>,
        mut visited: BTreeSet<Option<String>>,
    ) -> Result<Value, ProtocolError> {
        if !visited.insert(cursor.clone()) {
            return Err(ProtocolError::Invalid(
                "Thread history pagination repeated a cursor.".into(),
            ));
        }
        Ok(self.request("thread/turns/list",json!({"threadId":thread,"cursor":cursor,"limit":100,"sortDirection":"desc","itemsView":"summary"}),Pending::RevertPage {thread,head,before,visited}))
    }
    fn start_or_inject(&mut self, start: Value, history: Option<HistoricalContext>) -> Value {
        if let Some(history) = history {
            self.request("thread/inject_items",json!({"threadId":start["threadId"],"items":history_response_items(&history.messages,&history.context)}),Pending::Inject {start,history})
        } else {
            self.request("turn/start", start, Pending::Operation("turn/start".into()))
        }
    }
    fn interrupt(&mut self, thread: &str, turn: Option<&str>) -> Vec<Value> {
        let mut outbound = vec![];
        if let Some(turn) = turn {
            outbound.push(self.request(
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                Pending::Operation("turn/interrupt".into()),
            ));
        }
        let processes = self.processes.get(thread).cloned().unwrap_or_default();
        for process in processes {
            outbound.push(self.request(
                "thread/backgroundTerminals/terminate",
                json!({"threadId":thread,"processId":process}),
                Pending::Operation("thread/backgroundTerminals/terminate".into()),
            ));
        }
        outbound
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
                if error["code"] == -32601
                    && let Pending::Inject { mut start, history } = pending
                {
                    if std::mem::take(&mut self.stop_before_thread) {
                        return Ok(output);
                    }
                    prepend_inline_history(&mut start, &history);
                    output.outbound.push(self.request(
                        "turn/start",
                        start,
                        Pending::Operation("turn/start".into()),
                    ));
                    return Ok(output);
                }
                let message = string(error, "message");
                let operation = match pending {
                    Pending::Initialize => "initialize".into(),
                    Pending::Thread { .. } => "thread/start".into(),
                    Pending::Inject { .. } => "thread/inject_items".into(),
                    Pending::RevertRead { .. } => "thread/read".into(),
                    Pending::RevertResume { .. } => "thread/resume".into(),
                    Pending::RevertPage { .. } => "thread/turns/list".into(),
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
                    Pending::Inject { .. } => "thread/inject_items".into(),
                    Pending::RevertRead { .. } => "thread/read".into(),
                    Pending::RevertResume { .. } => "thread/resume".into(),
                    Pending::RevertPage { .. } => "thread/turns/list".into(),
                    Pending::Operation(operation) => operation.clone(),
                },
                result: Json(result.clone()),
            });
            match pending {
                Pending::Initialize => output.outbound.push(json!({"method":"initialized"})),
                Pending::Thread { mut start, history } => {
                    let thread = required(&result["thread"], "id")?;
                    self.thread = Some(thread.clone());
                    output.events.push(ProviderEvent::SessionReady {
                        native_thread: thread.clone(),
                    });
                    let stopped = std::mem::take(&mut self.stop_before_thread);
                    if !start.is_null() && !stopped {
                        start["threadId"] = json!(thread);
                        output.outbound.push(self.start_or_inject(start, history));
                    }
                }
                Pending::Inject { start, .. } => {
                    output.events.push(ProviderEvent::ContextInjected);
                    if !std::mem::take(&mut self.stop_before_thread) {
                        output.outbound.push(self.request(
                            "turn/start",
                            start,
                            Pending::Operation("turn/start".into()),
                        ));
                    }
                }
                Pending::RevertRead { thread, head } => {
                    if result["thread"]["historyMode"] != "paginated" {
                        return Err(ProtocolError::Invalid(format!(
                            "Cannot roll back Codex thread {thread}: the thread uses legacy history, which Codex 0.156 cannot revert."
                        )));
                    }
                    if result["thread"]["status"]["type"] == "notLoaded" {
                        output.outbound.push(self.request(
                            "thread/resume",
                            json!({"threadId":thread,"excludeTurns":true}),
                            Pending::RevertResume { thread, head },
                        ));
                    } else {
                        output.outbound.push(self.revert_page(
                            thread,
                            head,
                            None,
                            None,
                            BTreeSet::new(),
                        )?);
                    }
                }
                Pending::RevertResume { thread, head } => {
                    output.outbound.push(self.revert_page(
                        thread,
                        head,
                        None,
                        None,
                        BTreeSet::new(),
                    )?);
                }
                Pending::RevertPage {
                    thread,
                    head,
                    mut before,
                    visited,
                } => {
                    let turns = result["data"].as_array().ok_or_else(|| {
                        ProtocolError::Invalid("missing native turn history".into())
                    })?;
                    for turn in turns {
                        let id = required(turn, "id")?;
                        if head.as_ref() == Some(&id) {
                            if let Some(before) = before {
                                output.outbound.push(self.request(
                                    "thread/revert",
                                    json!({"threadId":thread,"beforeTurnId":before}),
                                    Pending::Operation("thread/revert".into()),
                                ));
                            }
                            return Ok(output);
                        }
                        before = Some(id);
                    }
                    if let Some(cursor) = optional(result, "nextCursor") {
                        output.outbound.push(self.revert_page(
                            thread,
                            head,
                            before,
                            Some(cursor),
                            visited,
                        )?);
                    } else if let Some(head) = head {
                        return Err(ProtocolError::MissingBoundary(head));
                    } else if let Some(before) = before {
                        output.outbound.push(self.request(
                            "thread/revert",
                            json!({"threadId":thread,"beforeTurnId":before}),
                            Pending::Operation("thread/revert".into()),
                        ));
                    }
                }
                Pending::Operation(operation) => {
                    if operation == "turn/start"
                        && let Some(turn) = optional(&result["turn"], "id")
                        && let Some(thread) = self.thread.clone()
                    {
                        self.turns.insert(thread.clone(), turn.clone());
                        if self.interrupt_pending.remove(&thread) {
                            output.outbound.extend(self.interrupt(&thread, Some(&turn)));
                        }
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
                if let Some(thread) = &native_thread {
                    self.turns.insert(thread.clone(), turn.clone());
                    if self.interrupt_pending.remove(thread) {
                        output.outbound.extend(self.interrupt(thread, Some(&turn)));
                    }
                }
                events.push(ProviderEvent::TurnStarted {
                    native_turn: Some(turn),
                });
            }
            "turn/completed" => {
                let turn = &p["turn"];
                if let Some(thread) = &native_thread {
                    self.turns.remove(thread);
                    self.interrupt_pending.remove(thread);
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
                let delta = string(p, "delta");
                if !delta.is_empty() {
                    let native = required(p, "itemId")?;
                    let (stream, index) = if method.ends_with("summaryTextDelta") {
                        ("summary", p["summaryIndex"].as_u64().unwrap_or(0))
                    } else {
                        ("content", p["contentIndex"].as_u64().unwrap_or(0))
                    };
                    let key = json!([string(p, "turnId"), native, stream, index]).to_string();
                    self.reasoning_parts
                        .entry(native)
                        .or_default()
                        .insert((stream.into(), index), key.clone());
                    events.push(ProviderEvent::TextDelta {
                        key,
                        kind: ProviderItem::Reasoning,
                        text: delta,
                    });
                }
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
                kind: PlanKind::Todo,
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
                let counters = |usage: &Value| UsageCounters {
                    input: usage["inputTokens"].as_u64().unwrap_or(0),
                    cached_input: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                    cache_creation: usage["cacheWriteInputTokens"].as_u64(),
                    output: usage["outputTokens"].as_u64().unwrap_or(0),
                    reasoning: usage["reasoningOutputTokens"].as_u64().unwrap_or(0),
                };
                events.push(ProviderEvent::UsageTotals {
                    native_thread: string(p, "threadId"),
                    native_turn: string(p, "turnId"),
                    total: counters(&p["tokenUsage"]["total"]),
                    last: counters(&p["tokenUsage"]["last"]),
                });
                if !self
                    .turns
                    .get(&string(p, "threadId"))
                    .is_some_and(|turn| turn != &string(p, "turnId"))
                {
                    let usage = &p["tokenUsage"]["last"];
                    events.push(ProviderEvent::ContextUsage(ContextUsage {
                        used_tokens: usage["totalTokens"].as_u64().unwrap_or(0),
                        max_tokens: p["tokenUsage"]["modelContextWindow"].as_u64(),
                        auto_compact_threshold: None,
                    }));
                    events.push(ProviderEvent::Usage(TokenUsage {
                        input: usage["inputTokens"].as_u64().unwrap_or(0),
                        cached_input: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                        output: usage["outputTokens"].as_u64().unwrap_or(0),
                        reasoning_output: usage["reasoningOutputTokens"].as_u64().unwrap_or(0),
                        total: usage["totalTokens"].as_u64().unwrap_or(0),
                        max: p["tokenUsage"]["modelContextWindow"].as_u64(),
                    }));
                }
            }
            "item/started" | "item/completed" => {
                let item = &p["item"];
                let key = required(item, "id")?;
                let completed = method == "item/completed";
                if string(item, "type") == "commandExecution"
                    && let (Some(thread), Some(process)) =
                        (&native_thread, optional(item, "processId"))
                {
                    if completed {
                        if let Some(processes) = self.processes.get_mut(thread) {
                            processes.remove(&process);
                        }
                    } else {
                        self.processes
                            .entry(thread.clone())
                            .or_default()
                            .insert(process);
                    }
                }
                match string(item, "type").as_str() {
                    "reasoning" => {
                        if completed {
                            let mut parts = self.reasoning_parts.remove(&key).unwrap_or_default();
                            for stream in ["summary", "content"] {
                                for (index, text) in
                                    item[stream].as_array().into_iter().flatten().enumerate()
                                {
                                    if text.as_str().is_some_and(|text| !text.is_empty()) {
                                        parts.entry((stream.into(), index as u64)).or_insert_with(
                                            || {
                                                json!([string(p, "turnId"), key, stream, index])
                                                    .to_string()
                                            },
                                        );
                                    }
                                }
                            }
                            for ((stream, index), key) in parts {
                                events.push(ProviderEvent::ItemFinished {
                                    key,
                                    kind: ProviderItem::Reasoning,
                                    text: item[stream][index as usize]
                                        .as_str()
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned),
                                    status: ItemStatus::Completed,
                                });
                            }
                        }
                    }

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
                                background: false,
                                native_thread: Some(child.clone()),
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
                                    background: false,
                                    native_thread: Some(child.clone()),
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
                        kind: PlanKind::Proposed,
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
                owner_path: vec![],
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
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "mcpServer/elicitation/request" => {
                if method == "mcpServer/elicitation/request"
                    && (mcp_elicitation_response(p, ApprovalDecision::Accept)["action"] != "accept"
                        || !p["turnId"]
                            .as_str()
                            .is_some_and(|turn| self.turns.values().any(|active| active == turn)))
                {
                    output
                        .outbound
                        .push(json!({"id":frame["id"],"result":{"action":"decline"}}));
                    return Ok(output);
                }
                let kind = if method.contains("fileChange")
                    || p["permissions"]["fileSystem"]["write"]
                        .as_array()
                        .is_some_and(|paths| !paths.is_empty())
                {
                    "file-change"
                } else if p["permissions"]["fileSystem"]["read"]
                    .as_array()
                    .is_some_and(|paths| !paths.is_empty())
                {
                    "file-read"
                } else if method.starts_with("mcp") {
                    "mcp-elicitation"
                } else {
                    "command"
                };
                let (title, options) = if method.starts_with("mcp") {
                    describe_mcp_elicitation(p)
                } else {
                    (
                        optional(p, "command").unwrap_or_else(|| kind.into()),
                        vec![
                            ApprovalOption {
                                decision: ApprovalDecision::Accept,
                                label: "Approve".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::AcceptForSession,
                                label: "Approve for session".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::Decline,
                                label: "Decline".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::Cancel,
                                label: "Cancel".into(),
                            },
                        ],
                    )
                };
                events.push(ProviderEvent::RequestOpened {
                    owner_path: vec![],
                    key: frame["id"].to_string(),
                    body: RequestBody::Approval {
                        kind: kind.into(),
                        title,
                        detail: optional(p, "reason"),
                        options,
                        input: Json(p.clone()),
                    },
                    capability: ResponseCapability::Live,
                });
            }
            "error" => {
                let retrying = p["willRetry"].as_bool().unwrap_or(false);
                let message = string(&p["error"], "message");
                events.push(ProviderEvent::ItemFinished {
                    key: String::new(),
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
            let path = native_path(&thread, &self.children)?;
            let mut routed = vec![];
            for mut event in events {
                match &mut event {
                    ProviderEvent::RequestOpened { owner_path, .. } => {
                        *owner_path = path.iter().rev().map(|s| (*s).to_owned()).collect();
                        output.events.push(event);
                    }
                    ProviderEvent::RequestClosed { .. } => output.events.push(event),
                    _ => routed.push(event),
                }
            }
            output
                .events
                .extend(child_events(routed, &thread, &self.children)?);
        } else {
            output.events = events;
        }
        Ok(output)
    }
}
fn prepend_inline_history(start: &mut Value, history: &HistoricalContext) {
    if let Some(inputs) = start["input"].as_array_mut() {
        for input in inputs {
            if input["type"] == "text" {
                input["text"] = json!(format!(
                    "{}\n\n{}",
                    render_history(history),
                    string(input, "text")
                ));
                break;
            }
        }
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
                query: optional(item, "query")
                    .or_else(|| optional(&item["action"], "query"))
                    .unwrap_or_default(),
                results: item.get("results").cloned().map(Json),
            },
            None,
        ),
        "dynamicToolCall" | "mcpToolCall" => (
            ProviderItem::Tool {
                presentation: ToolPresentation::default(),
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
        if frame["method"] == "turn/start" {
            self.thread = optional(&frame["params"], "threadId");
        }
        if let Some(id) = frame["id"].as_u64() {
            let method = string(frame, "method");
            let pending = match method.as_str() {
                "initialize" => Pending::Initialize,
                "thread/start" | "thread/resume" => Pending::Thread {
                    start: Value::Null,
                    history: None,
                },
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
pub fn native_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}
