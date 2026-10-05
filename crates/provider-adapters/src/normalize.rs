//! Provider messages become complete records. This module performs no I/O.
use orchestration::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct TurnState {
    pub run: Run,
    pub attempt: RunAttempt,
    pub session: ProviderSession,
    pub provider_thread: ProviderThread,
    pub turn: ProviderTurn,
    pub items: BTreeMap<String, TurnItem>,
    pub requests: BTreeMap<RuntimeRequestId, NativeRequest>,
    pub interrupted: bool,
    pub terminal: bool,
    pub sequence: u64,
    pub claude_message_id: String,
    pub claude_blocks: BTreeMap<u64, String>,
}
#[derive(Debug, Clone)]
pub struct NativeRequest {
    pub id: Value,
    pub method: String,
    pub input: Value,
    pub record: RuntimeRequest,
}
pub struct Translation {
    pub state: TurnState,
    pub payloads: Vec<EventPayload>,
    pub immediate_responses: Vec<(Value, Value)>,
}

impl TurnState {
    pub fn prepare(
        run: Run,
        attempt: RunAttempt,
        provider_thread: ProviderThread,
        session: ProviderSession,
        turn_ordinal: u64,
        now: &Timestamp,
    ) -> Self {
        let turn = ProviderTurn {
            id: ProviderTurnId::new(format!("provider-turn:{}", attempt.id)).expect("derived id"),
            provider_thread_id: provider_thread.id.clone(),
            node_id: attempt.root_node_id.clone(),
            run_attempt_id: Some(attempt.id.clone()),
            native_turn_ref: None,
            ordinal: turn_ordinal,
            status: TurnStatus::Pending,
            started_at: Some(now.clone()),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        };
        Self {
            run,
            attempt,
            session,
            provider_thread,
            turn,
            items: BTreeMap::new(),
            requests: BTreeMap::new(),
            interrupted: false,
            terminal: false,
            sequence: 0,
            claude_message_id: String::new(),
            claude_blocks: BTreeMap::new(),
        }
    }
    pub fn batch(&mut self, payloads: Vec<EventPayload>, now: &Timestamp) -> crate::ProviderBatch {
        let events = payloads
            .into_iter()
            .map(|payload| {
                self.sequence += 1;
                DomainEvent {
                    id: EventId::new(format!(
                        "event:adapter:{}:{}",
                        self.attempt.id, self.sequence
                    ))
                    .expect("derived id"),
                    thread_id: self.run.thread_id.clone(),
                    occurred_at: now.clone(),
                    payload,
                }
            })
            .collect();
        crate::ProviderBatch {
            thread_id: self.run.thread_id.clone(),
            run_id: self.run.id.clone(),
            attempt_id: self.attempt.id.clone(),
            events,
            occurred_at: now.clone(),
            acknowledged: None,
        }
    }
    pub fn take_request(
        &mut self,
        id: &RuntimeRequestId,
        now: &Timestamp,
    ) -> Option<NativeRequest> {
        let request = self.requests.remove(id)?;
        for item in self
            .items
            .values_mut()
            .filter(|item| item.node_id.as_ref() == Some(&request.record.node_id))
        {
            item.status = ItemStatus::Completed;
            item.completed_at = Some(now.clone());
            item.updated_at = now.clone();
        }
        Some(request)
    }
    pub fn initial_payloads(&self) -> Vec<EventPayload> {
        vec![
            EventPayload::ProviderSessionAttached(self.session.clone()),
            EventPayload::ProviderThreadUpdated(self.provider_thread.clone()),
        ]
    }
    fn item(&self, key: &str, body: TurnItemBody, status: ItemStatus, now: &Timestamp) -> TurnItem {
        let old = self.items.get(key);
        TurnItem {
            id: TurnItemId::new(format!("turn-item:{}:{key}", self.attempt.id))
                .expect("derived id"),
            thread_id: self.run.thread_id.clone(),
            run_id: Some(self.run.id.clone()),
            node_id: Some(
                NodeId::new(format!("node:{}:{key}", self.attempt.id)).expect("derived id"),
            ),
            provider_thread_id: Some(self.provider_thread.id.clone()),
            provider_turn_id: Some(self.turn.id.clone()),
            native_item_ref: Some(ProviderRef {
                driver: self.session.driver,
                native_id: Some(key.into()),
                strength: Strength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            parent_item_id: None,
            ordinal: old.map_or(
                self.run.ordinal * 1_000_000 + self.items.len() as u64 + 1,
                |old| old.ordinal,
            ),
            status,
            title: old.and_then(|item| item.title.clone()),
            started_at: old
                .and_then(|item| item.started_at.clone())
                .or_else(|| Some(now.clone())),
            completed_at: if matches!(
                status,
                ItemStatus::Completed
                    | ItemStatus::Interrupted
                    | ItemStatus::Failed
                    | ItemStatus::Cancelled
            ) {
                Some(now.clone())
            } else {
                None
            },
            updated_at: now.clone(),
            body,
        }
    }
    fn record_item(
        &mut self,
        key: &str,
        mut item: TurnItem,
        now: &Timestamp,
        payloads: &mut Vec<EventPayload>,
    ) {
        if item.title.is_none() {
            item.title = title(&item.body);
        }
        let node_id = item.node_id.clone().expect("adapter item has node");
        let kind = match item.body {
            TurnItemBody::AssistantMessage { .. } => NodeKind::AssistantMessage,
            TurnItemBody::Reasoning { .. } => NodeKind::Reasoning,
            TurnItemBody::ProposedPlan { .. } => NodeKind::Plan,
            TurnItemBody::TodoList { .. } => NodeKind::TodoList,
            TurnItemBody::ApprovalRequest { .. } => NodeKind::ApprovalRequest,
            TurnItemBody::UserInputRequest { .. } => NodeKind::UserInputRequest,
            _ => NodeKind::ToolCall,
        };
        let request_id = match &item.body {
            TurnItemBody::ApprovalRequest { request_id, .. }
            | TurnItemBody::UserInputRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        };
        payloads.push(EventPayload::NodeUpdated(ExecutionNode {
            id: node_id,
            thread_id: self.run.thread_id.clone(),
            run_id: Some(self.run.id.clone()),
            parent_node_id: Some(self.attempt.root_node_id.clone()),
            root_node_id: self.attempt.root_node_id.clone(),
            kind,
            status: node_status(item.status),
            counts_for_run: false,
            provider_thread_id: Some(self.provider_thread.id.clone()),
            provider_turn_id: Some(self.turn.id.clone()),
            native_item_ref: item.native_item_ref.clone(),
            runtime_request_id: request_id,
            checkpoint_scope_id: None,
            started_at: item.started_at.clone(),
            completed_at: item.completed_at.clone(),
        }));
        if let TurnItemBody::AssistantMessage {
            message_id,
            text,
            attachments,
            streaming,
        } = &item.body
        {
            let message = ConversationMessage {
                created_by: CreatedBy::Agent,
                creation_source: CreationSource::Provider,
                id: message_id.clone(),
                thread_id: self.run.thread_id.clone(),
                run_id: Some(self.run.id.clone()),
                node_id: item.node_id.clone(),
                role: Role::Assistant,
                text: text.clone(),
                context: None,
                attachments: attachments.clone(),
                streaming: *streaming,
                created_at: item.started_at.clone().unwrap_or_else(|| now.clone()),
                updated_at: now.clone(),
            };
            payloads.push(EventPayload::MessageUpdated(message));
        }
        if let TurnItemBody::ProposedPlan {
            plan_id,
            markdown,
            streaming,
        } = &item.body
        {
            payloads.push(EventPayload::PlanUpdated(PlanArtifact {
                id: plan_id.clone(),
                thread_id: self.run.thread_id.clone(),
                run_id: Some(self.run.id.clone()),
                node_id: item.node_id.clone().unwrap(),
                status: if *streaming {
                    PlanStatus::Draft
                } else {
                    PlanStatus::Active
                },
                detail_in_turn_item: true,
                body: PlanBody::ProposedPlan {
                    markdown: markdown.clone(),
                },
            }));
        }
        if let TurnItemBody::TodoList {
            plan_id,
            steps,
            explanation,
        } = &item.body
        {
            payloads.push(EventPayload::PlanUpdated(PlanArtifact {
                id: plan_id.clone(),
                thread_id: self.run.thread_id.clone(),
                run_id: Some(self.run.id.clone()),
                node_id: item.node_id.clone().unwrap(),
                status: if steps
                    .iter()
                    .all(|step| step.status == StepStatus::Completed)
                {
                    PlanStatus::Completed
                } else {
                    PlanStatus::Active
                },
                detail_in_turn_item: true,
                body: PlanBody::TodoList {
                    steps: steps.clone(),
                    explanation: explanation.clone(),
                },
            }));
        }
        self.items.insert(key.into(), item.clone());
        payloads.push(EventPayload::TurnItemUpdated(item));
    }
    fn started(
        &mut self,
        native_turn_id: Option<&str>,
        now: &Timestamp,
        payloads: &mut Vec<EventPayload>,
    ) {
        if self.terminal {
            return;
        }
        self.turn.status = TurnStatus::Running;
        if let Some(id) = native_turn_id {
            self.turn.native_turn_ref = Some(ProviderRef {
                driver: self.session.driver,
                native_id: Some(id.into()),
                strength: Strength::Strong,
                fingerprint: None,
                ordinal: None,
            });
        }
        self.attempt.status = AttemptStatus::Running;
        self.attempt.provider_turn_id = Some(self.turn.id.clone());
        self.attempt.started_at.get_or_insert_with(|| now.clone());
        self.run.status = RunStatus::Running;
        self.run.started_at.get_or_insert_with(|| now.clone());
        self.session.status = SessionStatus::Running;
        self.session.updated_at = now.clone();
        self.provider_thread.status = ProviderThreadStatus::Active;
        self.provider_thread.last_run_ordinal = Some(self.run.ordinal);
        self.provider_thread.updated_at = now.clone();
        payloads.extend([
            EventPayload::ProviderTurnUpdated(self.turn.clone()),
            EventPayload::RunAttemptUpdated(self.attempt.clone()),
            EventPayload::RunUpdated(self.run.clone()),
            EventPayload::NodeUpdated(self.root_node(NodeStatus::Running, now)),
            EventPayload::ProviderSessionUpdated(self.session.clone()),
            EventPayload::ProviderThreadUpdated(self.provider_thread.clone()),
        ]);
    }
    fn root_node(&self, status: NodeStatus, now: &Timestamp) -> ExecutionNode {
        ExecutionNode {
            id: self.attempt.root_node_id.clone(),
            thread_id: self.run.thread_id.clone(),
            run_id: Some(self.run.id.clone()),
            parent_node_id: None,
            root_node_id: self.attempt.root_node_id.clone(),
            kind: NodeKind::RootTurn,
            status,
            counts_for_run: true,
            provider_thread_id: Some(self.provider_thread.id.clone()),
            provider_turn_id: Some(self.turn.id.clone()),
            native_item_ref: None,
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: self.attempt.started_at.clone(),
            completed_at: self.terminal.then(|| now.clone()),
        }
    }
    fn finish(
        &mut self,
        status: TurnStatus,
        failure: Option<ProviderFailure>,
        now: &Timestamp,
        payloads: &mut Vec<EventPayload>,
    ) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let status = if self.interrupted && status == TurnStatus::Completed {
            TurnStatus::Interrupted
        } else {
            status
        };
        self.turn.status = status;
        self.turn.completed_at = Some(now.clone());
        self.run.status = match status {
            TurnStatus::Completed => RunStatus::Completed,
            TurnStatus::Interrupted => RunStatus::Interrupted,
            TurnStatus::Cancelled => RunStatus::Cancelled,
            _ => RunStatus::Failed,
        };
        self.run.completed_at = Some(now.clone());
        self.attempt.status = match status {
            TurnStatus::Completed => AttemptStatus::Completed,
            TurnStatus::Interrupted => AttemptStatus::Interrupted,
            TurnStatus::Cancelled => AttemptStatus::Cancelled,
            _ => AttemptStatus::Failed,
        };
        self.attempt.completed_at = Some(now.clone());
        for (_, native) in std::mem::take(&mut self.requests) {
            let mut record = native.record;
            record.status = RequestStatus::Cancelled;
            record.resolved_at = Some(now.clone());
            let waiting: Vec<_> = self
                .items
                .iter()
                .filter(|(_, item)| item.node_id.as_ref() == Some(&record.node_id))
                .map(|(key, item)| (key.clone(), item.clone()))
                .collect();
            for (key, mut item) in waiting {
                item.status = ItemStatus::Cancelled;
                item.completed_at = Some(now.clone());
                item.updated_at = now.clone();
                self.record_item(&key, item, now, payloads);
            }
            payloads.push(EventPayload::RuntimeRequestUpdated(record));
        }
        for (key, old) in self.items.clone() {
            if matches!(
                old.status,
                ItemStatus::Running | ItemStatus::Pending | ItemStatus::Waiting
            ) {
                let item = orchestration::projector::finished_item(
                    &old,
                    match status {
                        TurnStatus::Completed => ItemStatus::Completed,
                        TurnStatus::Interrupted => ItemStatus::Interrupted,
                        TurnStatus::Cancelled => ItemStatus::Cancelled,
                        _ => ItemStatus::Failed,
                    },
                    now,
                );
                self.record_item(&key, item, now, payloads);
            }
        }
        if let Some(failure) = failure {
            let item = self.item(
                "terminal-error",
                TurnItemBody::Error {
                    failure,
                    retry: None,
                },
                ItemStatus::Failed,
                now,
            );
            self.record_item("terminal-error", item, now, payloads);
        }
        self.session.status = SessionStatus::Ready;
        self.session.updated_at = now.clone();
        self.provider_thread.status = ProviderThreadStatus::Idle;
        self.provider_thread.updated_at = now.clone();
        payloads.extend([
            EventPayload::ProviderTurnUpdated(self.turn.clone()),
            EventPayload::RunAttemptUpdated(self.attempt.clone()),
            EventPayload::NodeUpdated(self.root_node(
                match status {
                    TurnStatus::Completed => NodeStatus::Completed,
                    TurnStatus::Interrupted => NodeStatus::Interrupted,
                    TurnStatus::Cancelled => NodeStatus::Cancelled,
                    _ => NodeStatus::Failed,
                },
                now,
            )),
            EventPayload::ProviderSessionUpdated(self.session.clone()),
            EventPayload::ProviderThreadUpdated(self.provider_thread.clone()),
            EventPayload::RunUpdated(self.run.clone()),
        ]);
    }
}
fn node_status(status: ItemStatus) -> NodeStatus {
    match status {
        ItemStatus::Idle => NodeStatus::Idle,
        ItemStatus::Pending => NodeStatus::Pending,
        ItemStatus::Running => NodeStatus::Running,
        ItemStatus::Waiting => NodeStatus::Waiting,
        ItemStatus::Completed => NodeStatus::Completed,
        ItemStatus::Failed => NodeStatus::Failed,
        ItemStatus::Cancelled => NodeStatus::Cancelled,
        ItemStatus::Interrupted => NodeStatus::Interrupted,
    }
}
fn title(body: &TurnItemBody) -> Option<String> {
    Some(match body {
        TurnItemBody::Reasoning { .. } => "Thinking".into(),
        TurnItemBody::CommandExecution { input, .. } => input.clone(),
        TurnItemBody::FileChange { file_name, .. } => file_name.clone(),
        TurnItemBody::DynamicTool { tool_name, .. } => {
            tool_name.clone().unwrap_or_else(|| "Tool".into())
        }
        TurnItemBody::ApprovalRequest { request_kind, .. } => format!("{request_kind:?} approval"),
        TurnItemBody::UserInputRequest { .. } => "Input needed".into(),
        TurnItemBody::ProposedPlan { .. } | TurnItemBody::TodoList { .. } => "Plan".into(),
        _ => return None,
    })
}
fn string(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            String::new()
        } else {
            value.to_string()
        }
    })
}
fn native_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.as_str().or_else(|| part["text"].as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
fn failure(message: &str, class: FailureClass) -> ProviderFailure {
    ProviderFailure {
        class,
        message: message.chars().take(4096).collect(),
        code: None,
        retryable: Some(false),
        reset_at: None,
    }
}
fn questions(value: &Value) -> Vec<UserInputQuestion> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| UserInputQuestion {
            id: q["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| q["question"].as_str().unwrap_or("question").to_string()),
            header: q["header"].as_str().unwrap_or("Question").into(),
            question: string(&q["question"]),
            options: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| QuestionOption {
                    label: string(&o["label"]),
                    description: string(&o["description"]),
                    value: o["value"].as_str().map(str::to_owned),
                })
                .collect(),
            multi_select: q["multiSelect"].as_bool().unwrap_or(false),
            allow_custom_answer: q["allowCustomAnswer"].as_bool().unwrap_or(true),
            required: q["required"].as_bool().unwrap_or(true),
        })
        .collect()
}

pub fn codex(
    state: &TurnState,
    method: &str,
    params: &Value,
    request_id: Option<&Value>,
    now: &Timestamp,
) -> Translation {
    let mut next = state.clone();
    let mut payloads = vec![];
    let immediate_responses = vec![];
    if next.terminal {
        return Translation {
            state: next,
            payloads,
            immediate_responses,
        };
    }
    match method {
        "turn/started" => next.started(
            params["turn"]["id"]
                .as_str()
                .or_else(|| params["turnId"].as_str()),
            now,
            &mut payloads,
        ),
        "turn/completed" => {
            let turn = &params["turn"];
            let status = match turn["status"].as_str() {
                Some("completed") => TurnStatus::Completed,
                Some("interrupted") => TurnStatus::Interrupted,
                Some("cancelled") => TurnStatus::Cancelled,
                _ => TurnStatus::Failed,
            };
            let failure = (status == TurnStatus::Failed).then(|| {
                failure(
                    turn["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex turn failed"),
                    FailureClass::ProviderError,
                )
            });
            next.finish(status, failure, now, &mut payloads);
        }
        "item/agentMessage/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/textDelta"
        | "item/plan/delta" => {
            let key = params["itemId"].as_str().unwrap_or("stream");
            let delta = string(&params["delta"]);
            let previous = next
                .items
                .get(key)
                .map(|item| match &item.body {
                    TurnItemBody::AssistantMessage { text, .. }
                    | TurnItemBody::Reasoning { text, .. } => text.clone(),
                    TurnItemBody::ProposedPlan { markdown, .. } => markdown.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let text = previous + &delta;
            let body = match method {
                "item/agentMessage/delta" => TurnItemBody::AssistantMessage {
                    message_id: MessageId::new(format!("message:{}:{key}", next.attempt.id))
                        .expect("derived id"),
                    text,
                    attachments: vec![],
                    streaming: true,
                },
                "item/plan/delta" => TurnItemBody::ProposedPlan {
                    plan_id: PlanId::new(format!("plan:{}:{key}", next.attempt.id))
                        .expect("derived id"),
                    markdown: text,
                    streaming: true,
                },
                _ => TurnItemBody::Reasoning {
                    text,
                    streaming: true,
                },
            };
            let item = next.item(key, body, ItemStatus::Running, now);
            next.record_item(key, item, now, &mut payloads);
        }
        "item/started" | "item/completed" => {
            let value = &params["item"];
            let key = value["id"].as_str().unwrap_or("item");
            let completed = method == "item/completed";
            let status = if completed {
                if value["status"] == "failed" {
                    ItemStatus::Failed
                } else {
                    ItemStatus::Completed
                }
            } else {
                ItemStatus::Running
            };
            let body = match value["type"].as_str().unwrap_or("") {
                "agentMessage" => TurnItemBody::AssistantMessage {
                    message_id: MessageId::new(format!("message:{}:{key}", next.attempt.id))
                        .expect("derived id"),
                    text: string(&value["text"]),
                    attachments: vec![],
                    streaming: !completed,
                },
                "reasoning" => TurnItemBody::Reasoning {
                    text: native_text(&value["summary"]) + &native_text(&value["content"]),
                    streaming: !completed,
                },
                "plan" => TurnItemBody::ProposedPlan {
                    plan_id: PlanId::new(format!("plan:{}:{key}", next.attempt.id))
                        .expect("derived id"),
                    markdown: string(&value["text"]),
                    streaming: !completed,
                },
                "commandExecution" => TurnItemBody::CommandExecution {
                    input: string(&value["command"]),
                    output: value["aggregatedOutput"].as_str().map(str::to_owned),
                    output_omitted: false,
                    output_indicates_failure: value["exitCode"]
                        .as_i64()
                        .is_some_and(|code| code != 0),
                    exit_code: value["exitCode"]
                        .as_i64()
                        .and_then(|code| i32::try_from(code).ok()),
                },
                "fileChange" => {
                    let changes: Vec<_> = value["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|change| FileChange {
                            operation: change["kind"]["type"]
                                .as_str()
                                .or_else(|| change["kind"].as_str())
                                .unwrap_or("update")
                                .into(),
                            path: string(&change["path"]),
                            old_path: change["kind"]["movePath"].as_str().map(str::to_owned),
                            file_type: None,
                            mime_type: None,
                        })
                        .collect();
                    let diff = value["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|change| change["diff"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    TurnItemBody::FileChange {
                        file_name: changes
                            .first()
                            .map(|change| change.path.clone())
                            .unwrap_or_default(),
                        additions: None,
                        deletions: None,
                        diff_str: Some(diff),
                        old_str: None,
                        new_str: None,
                        changes,
                    }
                }
                "webSearch" => TurnItemBody::WebSearch {
                    patterns: vec![string(&value["query"])],
                    results: vec![],
                },
                "contextCompaction" => TurnItemBody::Compaction {
                    driver: Some(Driver::Codex),
                    summary: None,
                    before_token_count: None,
                    after_token_count: None,
                },
                _ => TurnItemBody::DynamicTool {
                    tool_name: value["tool"]
                        .as_str()
                        .or_else(|| value["type"].as_str())
                        .map(str::to_owned),
                    viewed_image_path: None,
                    input: Json(value["arguments"].clone()),
                    output: completed.then(|| Json(value["result"].clone())),
                    output_omitted: false,
                },
            };
            let item = next.item(key, body, status, now);
            next.record_item(key, item, now, &mut payloads);
        }
        "item/commandExecution/outputDelta" => {
            if let Some(item) = next
                .items
                .get(params["itemId"].as_str().unwrap_or(""))
                .cloned()
            {
                let key = params["itemId"].as_str().unwrap();
                let mut item = item;
                if let TurnItemBody::CommandExecution { output, .. } = &mut item.body {
                    output
                        .get_or_insert_default()
                        .push_str(params["delta"].as_str().unwrap_or(""));
                }
                item.updated_at = now.clone();
                next.record_item(key, item, now, &mut payloads);
            }
        }
        "turn/plan/updated" => {
            let steps = params["plan"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, step)| PlanStep {
                    id: format!("step-{index}"),
                    text: string(&step["step"]),
                    status: match step["status"].as_str() {
                        Some("completed") => StepStatus::Completed,
                        Some("inProgress") => StepStatus::Running,
                        _ => StepStatus::Pending,
                    },
                    duration_anchor_at: None,
                    duration_ms: None,
                })
                .collect();
            let item = next.item(
                "todo",
                TurnItemBody::TodoList {
                    plan_id: PlanId::new(format!("plan:{}:todo", next.attempt.id))
                        .expect("derived id"),
                    steps,
                    explanation: params["explanation"].as_str().map(str::to_owned),
                },
                ItemStatus::Running,
                now,
            );
            next.record_item("todo", item, now, &mut payloads);
        }
        "thread/tokenUsage/updated" => {
            let usage = &params["tokenUsage"]["total"];
            next.turn.token_usage = Some(TokenUsage {
                used_tokens: usage["totalTokens"].as_u64().unwrap_or(0),
                max_tokens: params["tokenUsage"]["modelContextWindow"].as_u64(),
                input_tokens: usage["inputTokens"].as_u64(),
                cached_input_tokens: usage["cachedInputTokens"].as_u64(),
                output_tokens: usage["outputTokens"].as_u64(),
                reasoning_output_tokens: usage["reasoningOutputTokens"].as_u64(),
                updated_at: now.clone(),
            });
            payloads.push(EventPayload::ProviderTurnUpdated(next.turn.clone()));
        }
        "error" => {
            let failure = failure(
                params["error"]["message"].as_str().unwrap_or("Codex error"),
                if params["error"]["codexErrorInfo"] == "usageLimitExceeded" {
                    FailureClass::UsageLimit
                } else {
                    FailureClass::ProviderError
                },
            );
            if params["willRetry"].as_bool() == Some(true) {
                let item = next.item(
                    "retry",
                    TurnItemBody::Error {
                        failure,
                        retry: Some(Retry {
                            attempt: params["attempt"].as_u64().unwrap_or(1) as u32,
                            max_attempts: None,
                            retry_delay_ms: None,
                        }),
                    },
                    ItemStatus::Running,
                    now,
                );
                next.record_item("retry", item, now, &mut payloads);
            } else {
                let item = next.item(
                    "error",
                    TurnItemBody::Error {
                        failure,
                        retry: None,
                    },
                    ItemStatus::Failed,
                    now,
                );
                next.record_item("error", item, now, &mut payloads);
            }
        }
        "item/tool/requestUserInput"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "item/commandExecution/requestApproval"
        | "execCommandApproval"
        | "applyPatchApproval"
        | "mcpServer/elicitation/request"
            if request_id.is_some() =>
        {
            let id = request_id.unwrap();
            let native_id = id.to_string();
            let request_id =
                RuntimeRequestId::new(format!("request:{}:{native_id}", next.attempt.id))
                    .expect("derived id");
            let key = format!("request:{native_id}");
            let kind = match method {
                "item/tool/requestUserInput" => RequestKind::UserInput,
                "item/fileChange/requestApproval" | "applyPatchApproval" => RequestKind::FileChange,
                "item/permissions/requestApproval" => RequestKind::Permission,
                "mcpServer/elicitation/request" => RequestKind::McpElicitation,
                _ => RequestKind::Command,
            };
            let body = if kind == RequestKind::UserInput {
                TurnItemBody::UserInputRequest {
                    request_id: request_id.clone(),
                    questions: questions(&params["questions"]),
                    question_answer: None,
                    response_mode_message: false,
                }
            } else {
                TurnItemBody::ApprovalRequest {
                    request_id: request_id.clone(),
                    request_kind: kind,
                    prompt: Some(
                        params["command"]
                            .as_str()
                            .or_else(|| params["reason"].as_str())
                            .or_else(|| params["message"].as_str())
                            .unwrap_or("Permission requested")
                            .into(),
                    ),
                    app_name: params["serverName"].as_str().map(str::to_owned),
                    options: vec![
                        ApprovalOption {
                            decision: ApprovalDecision::Accept,
                            label: "Allow once".into(),
                            warning: None,
                        },
                        ApprovalOption {
                            decision: ApprovalDecision::AcceptForSession,
                            label: "Allow session".into(),
                            warning: None,
                        },
                        ApprovalOption {
                            decision: ApprovalDecision::Decline,
                            label: "Decline".into(),
                            warning: None,
                        },
                    ],
                }
            };
            let item = next.item(&key, body, ItemStatus::Waiting, now);
            let record = RuntimeRequest {
                id: request_id.clone(),
                node_id: item.node_id.clone().unwrap(),
                provider_turn_id: Some(next.turn.id.clone()),
                native_request_ref: Some(ProviderRef {
                    driver: Driver::Codex,
                    native_id: Some(native_id),
                    strength: Strength::Strong,
                    fingerprint: None,
                    ordinal: None,
                }),
                kind,
                status: RequestStatus::Pending,
                response_capability: ResponseCapability::Live {
                    provider_session_id: next.session.id.clone(),
                },
                created_at: now.clone(),
                resolved_at: None,
                decision: None,
                answers: None,
            };
            payloads.push(EventPayload::RuntimeRequestUpdated(record.clone()));
            next.requests.insert(
                request_id,
                NativeRequest {
                    id: id.clone(),
                    method: method.into(),
                    input: params.clone(),
                    record,
                },
            );
            next.record_item(&key, item, now, &mut payloads);
        }
        _ => {}
    }
    Translation {
        state: next,
        payloads,
        immediate_responses,
    }
}

pub fn codex_response(
    request: &NativeRequest,
    decision: Option<ApprovalDecision>,
    answers: Option<&Answers>,
) -> Value {
    match request.method.as_str() {
        "item/tool/requestUserInput" => {
            json!({"answers": answers.map(|answers| answers.iter().map(|(key,value)|(key.clone(),json!({"answers": if let Some(values)=value.0.as_array(){values.clone()}else{vec![value.0.clone()]} }))).collect::<BTreeMap<_,_>>()).unwrap_or_default()})
        }
        "mcpServer/elicitation/request" => {
            json!({"action":match decision {Some(ApprovalDecision::Accept|ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways)=>"accept",Some(ApprovalDecision::Decline)=>"decline",_=>"cancel"},"content":answers})
        }
        "item/permissions/requestApproval" => {
            json!({"permissions":if matches!(decision,Some(ApprovalDecision::Accept|ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways)){request.input["permissions"].clone()}else{json!({})},"scope":if matches!(decision,Some(ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways)){"session"}else{"turn"}})
        }
        _ => {
            json!({"decision":match decision{Some(ApprovalDecision::Accept)=>"accept",Some(ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways)=>"acceptForSession",Some(ApprovalDecision::Decline)=>"decline",_=>"cancel"}})
        }
    }
}

pub fn claude(state: &TurnState, frame: &Value, now: &Timestamp) -> Translation {
    let mut next = state.clone();
    let mut payloads = vec![];
    let mut immediate_responses = vec![];
    if next.terminal {
        return Translation {
            state: next,
            payloads,
            immediate_responses,
        };
    }
    match frame["type"].as_str().unwrap_or("") {
        "system" if frame["subtype"] == "init" => {}
        "system" if frame["subtype"] == "compact_boundary" => {
            if next.turn.status == TurnStatus::Pending {
                next.started(None, now, &mut payloads);
            }
            let item = next.item(
                "compaction",
                TurnItemBody::Compaction {
                    driver: Some(Driver::Claude),
                    summary: frame["message"].as_str().map(str::to_owned),
                    before_token_count: frame["compact_metadata"]["pre_tokens"].as_u64(),
                    after_token_count: None,
                },
                ItemStatus::Completed,
                now,
            );
            next.record_item("compaction", item, now, &mut payloads);
        }
        "stream_event" => {
            let event = &frame["event"];
            let index = event["index"].as_u64().unwrap_or(0);
            match event["type"].as_str().unwrap_or("") {
                "message_start" => {
                    next.claude_message_id =
                        event["message"]["id"].as_str().unwrap_or("stream").into();
                    if next.turn.status == TurnStatus::Pending {
                        next.started(None, now, &mut payloads);
                    }
                }
                "content_block_start" => {
                    let block = &event["content_block"];
                    let key = block["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{}:block-{index}", next.claude_message_id));
                    next.claude_blocks.insert(index, key.clone());
                    let body = match block["type"].as_str() {
                        Some("thinking") => TurnItemBody::Reasoning {
                            text: string(&block["thinking"]),
                            streaming: true,
                        },
                        Some("tool_use") => claude_tool(
                            &next,
                            &key,
                            block["name"].as_str().unwrap_or("Tool"),
                            &block["input"],
                        ),
                        _ => TurnItemBody::AssistantMessage {
                            message_id: MessageId::new(format!(
                                "message:{}:{key}",
                                next.attempt.id
                            ))
                            .expect("derived id"),
                            text: string(&block["text"]),
                            attachments: vec![],
                            streaming: true,
                        },
                    };
                    let item = next.item(&key, body, ItemStatus::Running, now);
                    next.record_item(&key, item, now, &mut payloads);
                }
                "content_block_delta" => {
                    if let Some(key) = next.claude_blocks.get(&index).cloned()
                        && let Some(mut item) = next.items.get(&key).cloned()
                    {
                        match &mut item.body {
                            TurnItemBody::AssistantMessage { text, .. } => {
                                text.push_str(event["delta"]["text"].as_str().unwrap_or(""))
                            }
                            TurnItemBody::Reasoning { text, .. } => {
                                text.push_str(event["delta"]["thinking"].as_str().unwrap_or(""))
                            }
                            _ => {}
                        }
                        item.updated_at = now.clone();
                        next.record_item(&key, item, now, &mut payloads);
                    }
                }
                _ => {}
            }
        }
        "assistant" => {
            if let Some(cursor) = frame["uuid"]
                .as_str()
                .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            {
                next.turn.native_turn_ref = Some(ProviderRef {
                    driver: Driver::Claude,
                    native_id: Some(cursor.into()),
                    strength: Strength::Weak,
                    fingerprint: None,
                    ordinal: None,
                });
                payloads.push(EventPayload::ProviderTurnUpdated(next.turn.clone()));
            }
            let message = &frame["message"];
            let native = message["id"]
                .as_str()
                .or_else(|| frame["uuid"].as_str())
                .unwrap_or("assistant");
            for (index, block) in message["content"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let key = block["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{native}:block-{index}"));
                let body = match block["type"].as_str() {
                    Some("tool_use") => claude_tool(
                        &next,
                        &key,
                        block["name"].as_str().unwrap_or("Tool"),
                        &block["input"],
                    ),
                    Some("thinking") => TurnItemBody::Reasoning {
                        text: string(&block["thinking"]),
                        streaming: false,
                    },
                    Some("text") => TurnItemBody::AssistantMessage {
                        message_id: MessageId::new(format!("message:{}:{key}", next.attempt.id))
                            .expect("derived id"),
                        text: string(&block["text"]),
                        attachments: vec![],
                        streaming: false,
                    },
                    _ => continue,
                };
                let status = if block["type"] == "tool_use" {
                    ItemStatus::Running
                } else {
                    ItemStatus::Completed
                };
                let item = next.item(&key, body, status, now);
                next.record_item(&key, item, now, &mut payloads);
            }
            let usage = &message["usage"];
            if usage.is_object() {
                let input = usage["input_tokens"].as_u64().unwrap_or(0);
                let output = usage["output_tokens"].as_u64().unwrap_or(0);
                let cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                next.turn.token_usage = Some(TokenUsage {
                    used_tokens: input + output + cached,
                    max_tokens: None,
                    input_tokens: Some(input),
                    cached_input_tokens: Some(cached),
                    output_tokens: Some(output),
                    reasoning_output_tokens: None,
                    updated_at: now.clone(),
                });
                payloads.push(EventPayload::ProviderTurnUpdated(next.turn.clone()));
            }
        }
        "user" => {
            for block in frame["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] == "tool_result" {
                    let key = block["tool_use_id"].as_str().unwrap_or("");
                    if let Some(mut item) = next.items.get(key).cloned() {
                        let text = native_text(&block["content"]);
                        match &mut item.body {
                            TurnItemBody::CommandExecution {
                                output,
                                exit_code,
                                output_indicates_failure,
                                ..
                            } => {
                                *output = Some(text);
                                *output_indicates_failure =
                                    block["is_error"].as_bool().unwrap_or(false);
                                *exit_code = None;
                            }
                            TurnItemBody::DynamicTool { output, .. } => {
                                *output = Some(Json(block["content"].clone()))
                            }
                            _ => {}
                        }
                        item.status = if block["is_error"].as_bool().unwrap_or(false) {
                            ItemStatus::Failed
                        } else {
                            ItemStatus::Completed
                        };
                        item.completed_at = Some(now.clone());
                        item.updated_at = now.clone();
                        next.record_item(key, item, now, &mut payloads);
                    }
                }
            }
        }
        "result" => {
            let failed =
                frame["is_error"].as_bool().unwrap_or(false) || frame["subtype"] != "success";
            let detail = frame["result"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| native_text(&frame["errors"]));
            next.finish(
                if failed {
                    TurnStatus::Failed
                } else {
                    TurnStatus::Completed
                },
                failed.then(|| failure(&detail, FailureClass::ProviderError)),
                now,
                &mut payloads,
            );
        }
        "control_request" if frame["request"]["subtype"] == "can_use_tool" => {
            let id = frame["request_id"].clone();
            let params = &frame["request"];
            let name = params["tool_name"].as_str().unwrap_or("Tool");
            let input = &params["input"];
            if name == "ExitPlanMode" {
                let value = if input["type"] == "record" {
                    &input["value"]
                } else {
                    input
                };
                if let Some(markdown) = value["plan"]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    let key = "proposed-plan";
                    let item = next.item(
                        key,
                        TurnItemBody::ProposedPlan {
                            plan_id: PlanId::new(format!("plan:{}:proposal", next.attempt.id))
                                .expect("derived id"),
                            markdown: markdown.into(),
                            streaming: false,
                        },
                        ItemStatus::Completed,
                        now,
                    );
                    next.record_item(key, item, now, &mut payloads);
                }
                immediate_responses.push((id, json!({"behavior":"deny","message":"The client captured your proposed plan. Stop here and wait for the user's feedback or implementation request in a later turn."})));
            } else {
                let native_id = string(&id);
                let request_id =
                    RuntimeRequestId::new(format!("request:{}:{native_id}", next.attempt.id))
                        .expect("derived id");
                let key = format!("request:{native_id}");
                let kind = match name {
                    "AskUserQuestion" => RequestKind::UserInput,
                    "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => RequestKind::FileChange,
                    "Read" | "Grep" | "Glob" | "LS" => RequestKind::FileRead,
                    _ => RequestKind::Command,
                };
                let body = if kind == RequestKind::UserInput {
                    TurnItemBody::UserInputRequest {
                        request_id: request_id.clone(),
                        questions: questions(&input["questions"]),
                        question_answer: None,
                        response_mode_message: false,
                    }
                } else {
                    TurnItemBody::ApprovalRequest {
                        request_id: request_id.clone(),
                        request_kind: kind,
                        prompt: Some(format!("{name}\n{}", input)),
                        app_name: None,
                        options: vec![],
                    }
                };
                let item = next.item(&key, body, ItemStatus::Waiting, now);
                let record = RuntimeRequest {
                    id: request_id.clone(),
                    node_id: item.node_id.clone().unwrap(),
                    provider_turn_id: Some(next.turn.id.clone()),
                    native_request_ref: Some(ProviderRef {
                        driver: Driver::Claude,
                        native_id: Some(native_id),
                        strength: Strength::Strong,
                        fingerprint: None,
                        ordinal: None,
                    }),
                    kind,
                    status: RequestStatus::Pending,
                    response_capability: ResponseCapability::Live {
                        provider_session_id: next.session.id.clone(),
                    },
                    created_at: now.clone(),
                    resolved_at: None,
                    decision: None,
                    answers: None,
                };
                payloads.push(EventPayload::RuntimeRequestUpdated(record.clone()));
                next.requests.insert(
                    request_id,
                    NativeRequest {
                        id,
                        method: name.into(),
                        input: input.clone(),
                        record,
                    },
                );
                next.record_item(&key, item, now, &mut payloads);
            }
        }
        "rate_limit_event" => {
            let item = next.item(
                "rate-limit",
                TurnItemBody::SystemNotice {
                    message: "Provider usage limit".into(),
                },
                ItemStatus::Completed,
                now,
            );
            next.record_item("rate-limit", item, now, &mut payloads);
        }
        _ => {}
    }
    Translation {
        state: next,
        payloads,
        immediate_responses,
    }
}
fn claude_tool(state: &TurnState, key: &str, name: &str, input: &Value) -> TurnItemBody {
    match name {
        "Bash" => TurnItemBody::CommandExecution {
            input: string(&input["command"]),
            output: None,
            output_omitted: false,
            output_indicates_failure: false,
            exit_code: None,
        },
        "Read" | "Grep" | "Glob" | "LS" => TurnItemBody::FileSearch {
            pattern: input["pattern"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .map(str::to_owned),
            results: vec![],
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => TurnItemBody::FileChange {
            file_name: input["file_path"]
                .as_str()
                .or_else(|| input["notebook_path"].as_str())
                .unwrap_or("")
                .into(),
            additions: None,
            deletions: None,
            diff_str: None,
            old_str: input["old_string"].as_str().map(str::to_owned),
            new_str: input["new_string"]
                .as_str()
                .or_else(|| input["content"].as_str())
                .map(str::to_owned),
            changes: vec![],
        },
        "WebSearch" | "WebFetch" => TurnItemBody::WebSearch {
            patterns: vec![
                input["query"]
                    .as_str()
                    .or_else(|| input["url"].as_str())
                    .unwrap_or("")
                    .into(),
            ],
            results: vec![],
        },
        "TodoWrite" => TurnItemBody::TodoList {
            plan_id: PlanId::new(format!("plan:{}:{key}", state.attempt.id)).expect("derived id"),
            steps: input["todos"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, todo)| PlanStep {
                    id: format!("todo-{index}"),
                    text: string(&todo["content"]),
                    status: match todo["status"].as_str() {
                        Some("completed") => StepStatus::Completed,
                        Some("in_progress") => StepStatus::Running,
                        _ => StepStatus::Pending,
                    },
                    duration_anchor_at: None,
                    duration_ms: None,
                })
                .collect(),
            explanation: None,
        },
        _ => TurnItemBody::DynamicTool {
            tool_name: Some(name.into()),
            viewed_image_path: None,
            input: Json(input.clone()),
            output: None,
            output_omitted: false,
        },
    }
}
pub fn claude_response(
    request: &NativeRequest,
    decision: Option<ApprovalDecision>,
    answers: Option<&Answers>,
) -> Value {
    if matches!(
        decision,
        Some(ApprovalDecision::Decline | ApprovalDecision::Cancel)
    ) {
        return json!({"behavior":"deny","message":"Declined by user","interrupt":decision==Some(ApprovalDecision::Cancel)});
    }
    let mut input = request.input.clone();
    if let Some(answers) = answers {
        input["answers"] = json!(
            answers
                .iter()
                .map(|(key, value)| {
                    let text = match &value.0 {
                        Value::Array(values) => values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        value => value.to_string(),
                    };
                    (key.clone(), text)
                })
                .collect::<BTreeMap<_, _>>()
        );
    }
    let mut response = json!({"behavior":"allow","updatedInput":input});
    if matches!(
        decision,
        Some(ApprovalDecision::AcceptForSession | ApprovalDecision::AcceptAlways)
    ) {
        response["updatedPermissions"] = json!([{"type":"addRules","rules":[{"toolName":request.method}],"behavior":"allow","destination":"session"}]);
    }
    response
}

pub fn disconnected(state: &TurnState, message: &str, now: &Timestamp) -> Translation {
    let mut next = state.clone();
    let mut payloads = vec![];
    next.finish(
        if state.interrupted {
            TurnStatus::Interrupted
        } else {
            TurnStatus::Failed
        },
        (!state.interrupted).then(|| failure(message, FailureClass::TransportError)),
        now,
        &mut payloads,
    );
    Translation {
        state: next,
        payloads,
        immediate_responses: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::capabilities;
    fn timestamp() -> Timestamp {
        Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
    }
    fn state(driver: Driver) -> TurnState {
        let now = timestamp();
        let command = Command {
            command_id: CommandId::new("create").unwrap(),
            thread_id: ThreadId::new("thread").unwrap(),
            body: CommandBody::ThreadCreate {
                created_by: CreatedBy::User,
                creation_source: CreationSource::Desktop,
                project_id: ProjectId::new("project").unwrap(),
                title: "Test".into(),
                model_selection: ModelSelection {
                    instance_id: ProviderInstanceId::new("provider").unwrap(),
                    model: "test-model".into(),
                    options: Default::default(),
                },
                runtime_mode: RuntimeMode::ApprovalRequired,
                interaction_mode: InteractionMode::Default,
                branch: None,
                worktree_path: None,
            },
        };
        let decisions =
            decider::decide(&command, None, &now, &capabilities(driver).turns, driver).unwrap();
        let mut projection = None;
        for event in decisions.events {
            projection = projector::apply(projection.as_ref(), &event, Default::default());
        }
        let command = Command {
            command_id: CommandId::new("send").unwrap(),
            thread_id: command.thread_id,
            body: CommandBody::MessageDispatch(MessageDispatch {
                created_by: CreatedBy::User,
                creation_source: CreationSource::Desktop,
                message_id: MessageId::new("user-message").unwrap(),
                text: "Test".into(),
                context: None,
                attachments: vec![],
                model_selection: None,
                delivery_intent: None,
                dispatch_mode: DispatchMode::StartImmediately,
            }),
        };
        for event in decider::decide(
            &command,
            projection.as_ref(),
            &now,
            &capabilities(driver).turns,
            driver,
        )
        .unwrap()
        .events
        {
            projection = projector::apply(projection.as_ref(), &event, Default::default());
        }
        let p = projection.unwrap();
        let session = ProviderSession {
            id: ProviderSessionId::new("session").unwrap(),
            driver,
            provider_instance_id: p.runs[0].provider_instance_id.clone(),
            status: SessionStatus::Ready,
            cwd: "/tmp".into(),
            model: Some("test-model".into()),
            capabilities: capabilities(driver),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_error: None,
        };
        TurnState::prepare(
            p.runs[0].clone(),
            p.attempts[0].clone(),
            p.provider_threads[0].clone(),
            session,
            1,
            &now,
        )
    }
    #[test]
    fn codex_deltas_finalize_one_message_and_ignore_late_events() {
        let state = state(Driver::Codex);
        let now = timestamp();
        let delta = codex(
            &state,
            "item/agentMessage/delta",
            &json!({"itemId":"text","delta":"Hi"}),
            None,
            &now,
        );
        assert!(state.items.is_empty());
        let result = codex(
            &delta.state,
            "item/completed",
            &json!({"item":{"type":"agentMessage","id":"text","text":"Hi!"}}),
            None,
            &now,
        );
        assert_eq!(result.state.items.len(), 1);
        assert!(
            matches!(&result.state.items["text"].body,TurnItemBody::AssistantMessage {text,streaming:false,..} if text=="Hi!")
        );
        let done = codex(
            &result.state,
            "turn/completed",
            &json!({"turn":{"status":"completed"}}),
            None,
            &now,
        );
        assert_eq!(done.state.run.status, RunStatus::Completed);
        assert!(
            codex(
                &done.state,
                "item/agentMessage/delta",
                &json!({"itemId":"late","delta":"late"}),
                None,
                &now
            )
            .payloads
            .is_empty()
        );
        assert!(
            matches!(done.payloads.last(),Some(EventPayload::RunUpdated(run)) if run.status==RunStatus::Completed)
        );
    }
    #[test]
    fn pending_approval_is_cancelled_on_disconnect_but_resolved_callback_is_not() {
        let initial = state(Driver::Codex);
        let now = timestamp();
        let approval = codex(
            &initial,
            "item/commandExecution/requestApproval",
            &json!({"command":"pwd"}),
            Some(&json!(4)),
            &now,
        );
        let id = approval.state.requests.keys().next().unwrap().clone();
        assert_eq!(
            codex_response(
                &approval.state.requests[&id],
                Some(ApprovalDecision::AcceptForSession),
                None
            ),
            json!({"decision":"acceptForSession"})
        );
        let failure = disconnected(&approval.state, "closed", &now);
        assert!(failure.payloads.iter().any(|event| matches!(event,EventPayload::RuntimeRequestUpdated(request) if request.status==RequestStatus::Cancelled)));
        assert!(failure.state.requests.is_empty());
        let mut resolved = approval.state;
        resolved.take_request(&id, &now).unwrap();
        assert!(
            !disconnected(&resolved, "closed", &now)
                .payloads
                .iter()
                .any(|event| matches!(event, EventPayload::RuntimeRequestUpdated(_)))
        );
        assert!(
            codex(
                &initial,
                "account/chatgptAuthTokens/refresh",
                &json!({}),
                Some(&json!(5)),
                &now
            )
            .payloads
            .is_empty()
        );
    }
    #[test]
    fn claude_initialization_does_not_consume_pending_context() {
        let initial = state(Driver::Claude);
        let result = claude(
            &initial,
            &json!({"type":"system","subtype":"init"}),
            &timestamp(),
        );
        assert_eq!(result.state.turn.status, TurnStatus::Pending);
        assert!(result.payloads.is_empty());
        let started = claude(
            &result.state,
            &json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"message"}}}),
            &timestamp(),
        );
        assert_eq!(started.state.turn.status, TurnStatus::Running);
    }
    #[test]
    fn claude_stream_and_final_frame_share_identity() {
        let mut state = state(Driver::Claude);
        let now = timestamp();
        for event in [
            json!({"type":"message_start","message":{"id":"msg-1"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}),
        ] {
            state = claude(&state, &json!({"type":"stream_event","event":event}), &now).state;
        }
        state=claude(&state,&json!({"type":"assistant","message":{"id":"msg-1","content":[{"type":"text","text":"Hi!"}]}}),&now).state;
        assert_eq!(state.items.len(), 1);

        assert!(
            matches!(&state.items["msg-1:block-0"].body,TurnItemBody::AssistantMessage {text,streaming:false,..} if text=="Hi!")
        );
        state.interrupted = true;
        let result = claude(
            &state,
            &json!({"type":"result","subtype":"success","result":"Hi!"}),
            &now,
        );
        assert_eq!(result.state.run.status, RunStatus::Interrupted);
    }
    #[test]
    fn claude_plan_is_captured_and_tool_denied() {
        let initial = state(Driver::Claude);
        let now = timestamp();
        let result = claude(
            &initial,
            &json!({"type":"control_request","request_id":"plan","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"  # Plan\nDo it  "}}}),
            &now,
        );
        assert!(result.state.requests.is_empty());
        assert_eq!(result.immediate_responses[0].1["behavior"], "deny");
        assert!(
            matches!(&result.state.items["proposed-plan"].body,TurnItemBody::ProposedPlan{markdown,..} if markdown=="# Plan\nDo it")
        );
        let empty = claude(
            &initial,
            &json!({"type":"control_request","request_id":"plan","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{}}}),
            &now,
        );
        assert!(empty.state.items.is_empty());
        assert_eq!(empty.immediate_responses[0].1["behavior"], "deny");
    }
    #[test]
    fn structured_claude_answers_are_joined_and_required_is_preserved() {
        let result = claude(
            &state(Driver::Claude),
            &json!({"type":"control_request","request_id":"q","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Which?","required":false,"multiSelect":true}]}}}),
            &timestamp(),
        );
        let request = result.state.requests.values().next().unwrap();
        let answers = BTreeMap::from([("Which?".into(), Json(json!(["A", "B"])))]);
        assert_eq!(
            claude_response(request, None, Some(&answers))["updatedInput"]["answers"],
            json!({"Which?":"A, B"})
        );
        assert!(
            matches!(&result.state.items.values().next().unwrap().body,TurnItemBody::UserInputRequest{questions,..} if !questions[0].required)
        );
    }
    proptest::proptest! {
        #[test]
        fn codex_streaming_is_pure_and_accumulates_exactly(parts in proptest::collection::vec("[a-z]{0,20}",0..30)) {
            let original=state(Driver::Codex);let now=timestamp();let mut current=original.clone();
            for part in &parts {current=codex(&current,"item/agentMessage/delta",&json!({"itemId":"text","delta":part}),None,&now).state;}
            proptest::prop_assert!(original.items.is_empty());
            if let Some(item)=current.items.get("text") {let TurnItemBody::AssistantMessage{text,..}=&item.body else {unreachable!()};proptest::prop_assert_eq!(text, &parts.concat());}
        }
    }
}
