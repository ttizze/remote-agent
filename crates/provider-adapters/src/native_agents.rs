//! Provider-owned task registry. Child histories have nodes and provider turns,
//! but never synthetic application runs. Ownership survives the parent's return.
use crate::{
    ProviderBatch,
    normalize::{self, Translation, TurnState},
};
use orchestration::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone)]
pub(crate) struct NativeOutput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub attempt_id: RunAttemptId,
    pub owner: NativeSubagentOwner,
    pub events: Vec<DomainEvent>,
    pub now: Timestamp,
}
impl NativeOutput {
    pub fn batch(self) -> ProviderBatch {
        ProviderBatch {
            thread_id: self.thread_id,
            run_id: self.run_id,
            attempt_id: self.attempt_id,
            events: self.events,
            occurred_at: self.now,
            acknowledged: None,
            native_owner: Some(self.owner),
            native_continuation_offer: None,
        }
    }
}
#[derive(Debug, Clone)]
struct Child {
    controller_thread: ThreadId,
    controller_run: RunId,
    controller_attempt: RunAttemptId,
    task: Subagent,
    node: ExecutionNode,
    item: TurnItem,
    thread: AppThread,
    state: TurnState,
    generation: u64,
    next_ordinal: u64,
}
#[derive(Debug, Clone)]
struct Launch {
    parent: Option<String>,
    prompt: String,
    title: Option<String>,
    model: Option<String>,
}
#[derive(Debug, Clone)]
pub(crate) struct NativeAgents {
    template: AppThread,
    children: BTreeMap<String, Child>,
    tools: BTreeMap<String, Launch>,
    tool_tasks: BTreeMap<String, String>,
    pending: VecDeque<Value>,
    pending_bytes: usize,
    sequence: u64,
    responses: Vec<(Value, Value)>,
    wake_task: Option<NodeId>,
    buffered_wakes: BTreeMap<MessageId, WakeFrames>,
    collecting_wake: Option<MessageId>,
}
#[derive(Debug, Clone, Default)]
struct WakeFrames {
    frames: Vec<Value>,
    bytes: usize,
    truncated: bool,
}
impl WakeFrames {
    fn push(&mut self, mut frame: Value) {
        let terminal = frame["type"] == "result";
        let size = frame.to_string().len();
        if self.frames.len() >= 511 || self.bytes.saturating_add(size) > 2 * 1024 * 1024 {
            self.truncated = true;
        }
        if terminal {
            if self.truncated {
                frame = serde_json::json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Background output exceeded the continuation buffer limit"]});
            }
        } else if self.truncated {
            return;
        }
        self.bytes += frame.to_string().len();
        self.frames.push(frame);
    }
}
fn key(parent: &ThreadId, native: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{parent}\0{native}")))
}
fn native_ref(driver: Driver, native: &str) -> ProviderRef {
    ProviderRef {
        driver,
        native_id: Some(native.into()),
        strength: Strength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}
fn item_status(status: NodeStatus) -> ItemStatus {
    match status {
        NodeStatus::Completed => ItemStatus::Completed,
        NodeStatus::Failed => ItemStatus::Failed,
        NodeStatus::Interrupted => ItemStatus::Interrupted,
        NodeStatus::Cancelled => ItemStatus::Cancelled,
        NodeStatus::Waiting => ItemStatus::Waiting,
        _ => ItemStatus::Running,
    }
}
fn status(value: &str) -> NodeStatus {
    match value {
        "completed" => NodeStatus::Completed,
        "failed" | "errored" | "notFound" => NodeStatus::Failed,
        "interrupted" => NodeStatus::Interrupted,
        "stopped" | "cancelled" | "shutdown" => NodeStatus::Cancelled,
        "pendingInit" => NodeStatus::Pending,
        _ => NodeStatus::Running,
    }
}
fn terminal(value: NodeStatus) -> bool {
    matches!(
        value,
        NodeStatus::Completed
            | NodeStatus::Failed
            | NodeStatus::Cancelled
            | NodeStatus::Interrupted
    )
}
impl NativeAgents {
    pub fn new(template: AppThread) -> Self {
        Self {
            template,
            children: Default::default(),
            tools: Default::default(),
            tool_tasks: Default::default(),
            pending: Default::default(),
            pending_bytes: 0,
            sequence: 0,
            responses: vec![],
            wake_task: None,
            buffered_wakes: Default::default(),
            collecting_wake: None,
        }
    }
    pub fn contains(&self, native: &str) -> bool {
        self.children.contains_key(native)
    }
    pub fn update_template(&mut self, template: AppThread) {
        self.template = template;
    }
    pub fn is_live(&self) -> bool {
        self.children.values().any(|c| !terminal(c.task.status)) || !self.buffered_wakes.is_empty()
    }
    pub fn has_wake(&self) -> bool {
        self.wake_task.is_some()
    }
    pub fn clear_wake_report(&mut self) {
        self.wake_task = None;
    }
    pub fn take_wake(&mut self, message: &MessageId) -> Option<Vec<Value>> {
        if self.collecting_wake.as_ref() == Some(message) {
            self.collecting_wake = None;
        }
        self.buffered_wakes
            .remove(message)
            .map(|buffer| buffer.frames)
    }
    pub fn has_buffered_wake(&self, message: &MessageId) -> bool {
        self.buffered_wakes.contains_key(message)
    }
    pub fn begin_drain(&mut self, message: &MessageId) {
        if self.has_buffered_wake(message) {
            self.collecting_wake = Some(message.clone());
        }
    }
    pub fn discard_wake(&mut self, message: &MessageId) {
        let _ = self.take_wake(message);
    }
    pub fn buffer_wake(
        &mut self,
        root: &TurnState,
        frame: Value,
    ) -> Option<NativeContinuationOffer> {
        if let Some(id) = self.collecting_wake.clone() {
            let frames = self.buffered_wakes.get_mut(&id)?;
            let terminal = frame["type"] == "result";
            frames.push(frame);
            if terminal {
                self.collecting_wake = None;
            }
            return None;
        }
        let task = self.wake_task.take()?;
        let child = self.children.values().find(|c| c.task.id == task)?;
        let source = NativeContinuationRef {
            provider_thread_id: root.provider_thread.id.clone(),
            run_id: child.controller_run.clone(),
            attempt_id: child.controller_attempt.clone(),
            task_id: task,
        };
        self.sequence += 1;
        let message_id = MessageId::new(format!(
            "message:native-wake:{}",
            key(
                &root.run.thread_id,
                &format!("{}:{}", source.task_id, self.sequence)
            )
        ))
        .expect("derived id");
        let mut buffered = WakeFrames::default();
        buffered.push(frame.clone());
        self.buffered_wakes.insert(message_id.clone(), buffered);
        if frame["type"] != "result" {
            self.collecting_wake = Some(message_id.clone());
        }
        Some(NativeContinuationOffer {
            message_id,
            source,
            summary: "Background activity updated".into(),
        })
    }
    pub fn take_request(
        &mut self,
        id: &RuntimeRequestId,
        now: &Timestamp,
    ) -> Option<normalize::NativeRequest> {
        self.children
            .values_mut()
            .find_map(|c| c.state.take_request(id, now))
    }
    pub fn live_codex_turns(&self) -> Vec<(String, String)> {
        self.children
            .iter()
            .filter(|(_, c)| c.task.driver == Driver::Codex && !terminal(c.task.status))
            .filter_map(|(id, c)| {
                c.state
                    .turn
                    .native_turn_ref
                    .as_ref()?
                    .native_id
                    .as_ref()
                    .map(|turn| (id.clone(), turn.clone()))
            })
            .collect()
    }
    fn emit(
        &mut self,
        child: &Child,
        payloads: Vec<(ThreadId, EventPayload)>,
        now: &Timestamp,
    ) -> NativeOutput {
        let events = payloads
            .into_iter()
            .map(|(thread_id, payload)| {
                self.sequence += 1;
                DomainEvent {
                    id: EventId::new(format!("event:native:{}:{}", child.task.id, self.sequence))
                        .expect("derived id"),
                    thread_id,
                    payload,
                    occurred_at: now.clone(),
                }
            })
            .collect();
        NativeOutput {
            thread_id: child.controller_thread.clone(),
            run_id: child.controller_run.clone(),
            attempt_id: child.controller_attempt.clone(),
            owner: NativeSubagentOwner {
                parent_thread_id: child.task.thread_id.clone(),
                task_id: child.task.id.clone(),
            },
            events,
            now: now.clone(),
        }
    }
    fn task_payloads(child: &Child) -> Vec<(ThreadId, EventPayload)> {
        let parent = &child.task.thread_id;
        vec![
            (
                parent.clone(),
                EventPayload::SubagentUpdated(child.task.clone()),
            ),
            (
                parent.clone(),
                EventPayload::NodeUpdated(child.node.clone()),
            ),
            (
                parent.clone(),
                EventPayload::TurnItemUpdated(child.item.clone()),
            ),
        ]
    }
    fn child_payloads(child: &Child, payloads: Vec<EventPayload>) -> Vec<(ThreadId, EventPayload)> {
        payloads
            .into_iter()
            .filter_map(|mut p| {
                match &mut p {
                    EventPayload::RunCreated(_)
                    | EventPayload::RunUpdated(_)
                    | EventPayload::RunAttemptCreated(_)
                    | EventPayload::RunAttemptUpdated(_)
                    | EventPayload::ProviderSessionAttached(_)
                    | EventPayload::ProviderSessionUpdated(_) => return None,
                    EventPayload::NodeUpdated(n) => {
                        n.run_id = None;
                        n.counts_for_run = false;
                    }
                    EventPayload::TurnItemUpdated(i) => {
                        i.run_id = None;
                        i.ordinal += child.next_ordinal;
                    }
                    EventPayload::TurnItemTextDelta(d) => d.run_id = None,
                    EventPayload::MessageUpdated(m) => m.run_id = None,
                    EventPayload::PlanUpdated(p) => p.run_id = None,
                    EventPayload::ProviderTurnUpdated(t) => t.run_attempt_id = None,
                    _ => {}
                }
                Some((child.thread.id.clone(), p))
            })
            .collect()
    }
    fn register(
        &mut self,
        root: &TurnState,
        native: &str,
        launch: Launch,
        reopen: bool,
        now: &Timestamp,
    ) -> Option<NativeOutput> {
        if let Some(mut child) = self.children.remove(native) {
            if reopen && terminal(child.task.status) {
                child.generation += 1;
                child.next_ordinal += child.state.items.len() as u64 + 1;
                child.task.status = NodeStatus::Running;
                child.task.progress = None;
                child.task.result = None;
                child.task.started_at = Some(now.clone());
                child.task.completed_at = None;
                child.task.updated_at = now.clone();
                if child.task.thread_id == root.run.thread_id {
                    child.controller_run = root.run.id.clone();
                    child.controller_attempt = root.attempt.id.clone();
                    child.task.run_id = Some(root.run.id.clone());
                    child.node.run_id = child.task.run_id.clone();
                    child.item.run_id = child.task.run_id.clone();
                }
                child.node.status = NodeStatus::Running;
                child.node.completed_at = None;
                child.item.status = ItemStatus::Running;
                child.item.completed_at = None;
                if let TurnItemBody::Subagent {
                    progress, result, ..
                } = &mut child.item.body
                {
                    *progress = None;
                    *result = None;
                }
                let mut attempt = child.state.attempt.clone();
                attempt.id = RunAttemptId::new(format!(
                    "native-attempt:{}:{}",
                    child.task.id, child.generation
                ))
                .expect("derived id");
                attempt.root_node_id = NodeId::new(format!(
                    "native-root:{}:{}",
                    child.task.id, child.generation
                ))
                .expect("derived id");
                child.state = TurnState::prepare(
                    child.state.run.clone(),
                    attempt,
                    child.state.provider_thread.clone(),
                    child.state.session.clone(),
                    child.generation,
                    now,
                );
                let mut payloads = Self::task_payloads(&child);
                let mut started = child.state.initial_payloads();
                child.state.started(None, now, &mut started);
                payloads.extend(Self::child_payloads(&child, started));
                let output = self.emit(&child, payloads, now);
                self.children.insert(native.into(), child);
                return Some(output);
            }
            self.children.insert(native.into(), child);
            return None;
        }
        let parent_task = match launch.parent.as_deref() {
            Some(value) if value.starts_with("tool:") => {
                Some(self.tool_tasks.get(value.strip_prefix("tool:")?)?.clone())
            }
            value => value.map(str::to_owned),
        };
        let parent = match parent_task.as_ref() {
            Some(id) => self.children.get(id)?.thread.clone(),
            None => self.template.clone(),
        };
        let parent_node = parent_task
            .as_ref()
            .and_then(|p| self.children.get(p))
            .map(|c| c.task.id.clone())
            .unwrap_or_else(|| root.attempt.root_node_id.clone());
        let hash = key(&parent.id, native);
        let task_id = NodeId::new(format!("native-task:{hash}")).expect("derived id");
        let child_id = ThreadId::new(format!("native-child:{hash}")).expect("derived id");
        let mut thread = parent.clone();
        thread.id = child_id.clone();
        thread.created_by = CreatedBy::Agent;
        thread.creation_source = CreationSource::Provider;
        thread.title = launch
            .title
            .clone()
            .unwrap_or_else(|| launch.prompt.chars().take(80).collect());
        if thread.title.is_empty() {
            thread.title = "Subagent".into();
        }
        if let Some(model) = &launch.model
            && model != "inherit"
        {
            thread.model_selection.model = model.clone();
        }
        thread.lineage.parent_thread_id = Some(parent.id.clone());
        thread.lineage.relationship_to_parent = Some(Relationship::Subagent);
        thread.active_provider_thread_id = None;
        thread.forked_from = None;
        thread.imported = false;
        thread.archived_at = None;
        thread.deleted_at = None;
        thread.settled_override = None;
        thread.settled_at = None;
        thread.unsettled_at = None;
        thread.snoozed_until = None;
        thread.snoozed_at = None;
        thread.pinned_at = None;
        thread.pin_order_key = None;
        thread.active_order_key = None;
        thread.last_visited_at = None;
        thread.rollback_request_id = None;
        thread.rollback_failure = None;
        thread.created_at = now.clone();
        thread.updated_at = now.clone();
        let driver = root.session.driver;
        let provider_id =
            ProviderThreadId::new(format!("native-provider:{hash}")).expect("derived id");
        let child_root = NodeId::new(format!("native-root:{task_id}:1")).expect("derived id");
        let mut provider = root.provider_thread.clone();
        provider.id = provider_id.clone();
        provider.provider_session_id = None;
        provider.app_thread_id = Some(child_id.clone());
        provider.owner_node_id = Some(task_id.clone());
        provider.native_thread_ref = (driver == Driver::Codex).then(|| native_ref(driver, native));
        provider.native_conversation_head_ref = None;
        provider.first_run_ordinal = None;
        provider.last_run_ordinal = None;
        provider.handoff_ids.clear();
        provider.forked_from = None;
        provider.pending_background_tasks.clear();
        provider.context_usage = None;
        provider.native_metadata = None;
        provider.created_at = now.clone();
        provider.updated_at = now.clone();
        if driver == Driver::Codex {
            thread.active_provider_thread_id = Some(provider_id.clone());
        }
        let mut attempt = root.attempt.clone();
        attempt.id = RunAttemptId::new(format!("native-attempt:{task_id}:1")).expect("derived id");
        attempt.root_node_id = child_root.clone();
        attempt.provider_thread_id = provider_id.clone();
        attempt.provider_turn_id = None;
        let mut run = root.run.clone();
        run.thread_id = child_id.clone();
        run.root_node_id = Some(child_root);
        run.provider_thread_id = Some(provider_id.clone());
        run.delegated_completion = None;
        let mut state = TurnState::prepare(run, attempt, provider, root.session.clone(), 1, now);
        let mut child_start = state.initial_payloads();
        state.started(None, now, &mut child_start);
        let task = Subagent {
            id: task_id.clone(),
            thread_id: parent.id.clone(),
            run_id: (parent_task.is_none()).then(|| root.run.id.clone()),
            parent_node_id: parent_node.clone(),
            origin: SubagentOrigin::ProviderNative,
            created_by: CreatedBy::Agent,
            driver,
            provider_instance_id: root.run.model_selection.instance_id.clone(),
            provider_thread_id: (driver == Driver::Codex).then_some(provider_id),
            child_thread_id: Some(child_id.clone()),
            native_task_ref: Some(native_ref(driver, native)),
            prompt: launch.prompt,
            title: launch.title,
            model: launch.model,
            completion_wake: CompletionWake::Always,
            completion_delivery: None,
            status: NodeStatus::Running,
            progress: None,
            result: None,
            started_at: Some(now.clone()),
            completed_at: None,
            updated_at: now.clone(),
        };
        let node = ExecutionNode {
            id: task_id.clone(),
            thread_id: parent.id.clone(),
            run_id: task.run_id.clone(),
            parent_node_id: Some(parent_node),
            root_node_id: root.attempt.root_node_id.clone(),
            kind: NodeKind::Subagent,
            status: NodeStatus::Running,
            counts_for_run: false,
            provider_thread_id: task.provider_thread_id.clone(),
            provider_turn_id: None,
            native_item_ref: task.native_task_ref.clone(),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: Some(now.clone()),
            completed_at: None,
        };
        let item = TurnItem {
            id: TurnItemId::new(format!("native-item:{hash}")).expect("derived id"),
            thread_id: parent.id.clone(),
            run_id: task.run_id.clone(),
            node_id: Some(task_id),
            provider_thread_id: root.run.provider_thread_id.clone(),
            provider_turn_id: Some(root.turn.id.clone()),
            native_item_ref: task.native_task_ref.clone(),
            parent_item_id: None,
            ordinal: root.items.len() as u64 + 1 + self.children.len() as u64,
            status: ItemStatus::Running,
            title: Some(thread.title.clone()),
            started_at: Some(now.clone()),
            completed_at: None,
            updated_at: now.clone(),
            body: TurnItemBody::Subagent {
                subagent_id: task.id.clone(),
                origin: task.origin,
                driver,
                provider_instance_id: task.provider_instance_id.clone(),
                child_thread_id: Some(child_id.clone()),
                prompt: task.prompt.clone(),
                progress: None,
                result: None,
            },
        };
        let child = Child {
            controller_thread: parent_task
                .as_ref()
                .and_then(|p| self.children.get(p))
                .map(|c| c.controller_thread.clone())
                .unwrap_or_else(|| root.run.thread_id.clone()),
            controller_run: parent_task
                .as_ref()
                .and_then(|p| self.children.get(p))
                .map(|c| c.controller_run.clone())
                .unwrap_or_else(|| root.run.id.clone()),
            controller_attempt: parent_task
                .as_ref()
                .and_then(|p| self.children.get(p))
                .map(|c| c.controller_attempt.clone())
                .unwrap_or_else(|| root.attempt.id.clone()),
            task,
            node,
            item,
            thread,
            state,
            generation: 1,
            next_ordinal: 100,
        };
        let mut payloads = Self::task_payloads(&child);
        payloads.push((
            child_id.clone(),
            EventPayload::ThreadCreated(child.thread.clone()),
        ));
        payloads.extend(Self::child_payloads(&child, child_start));
        if !child.task.prompt.is_empty() {
            let message_id = MessageId::new(format!("native-prompt:{hash}:1")).expect("derived id");
            payloads.push((
                child_id.clone(),
                EventPayload::MessageUpdated(ConversationMessage {
                    native_continuation: None,
                    delegated_completion: None,
                    created_by: CreatedBy::Agent,
                    creation_source: CreationSource::Provider,
                    id: message_id.clone(),
                    thread_id: child_id.clone(),
                    run_id: None,
                    node_id: Some(child.state.attempt.root_node_id.clone()),
                    role: Role::User,
                    text: child.task.prompt.clone(),
                    context: None,
                    attachments: vec![],
                    streaming: false,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                }),
            ));
            let mut prompt = child.item.clone();
            prompt.id =
                TurnItemId::new(format!("native-prompt-item:{hash}:1")).expect("derived id");
            prompt.thread_id = child_id;
            prompt.run_id = None;
            prompt.node_id = Some(child.state.attempt.root_node_id.clone());
            prompt.ordinal = 1;
            prompt.status = ItemStatus::Completed;
            prompt.body = TurnItemBody::UserMessage {
                created_by: CreatedBy::Agent,
                creation_source: CreationSource::Provider,
                context: None,
                message_id,
                text: child.task.prompt.clone(),
                attachments: vec![],
                input_intent: InputIntent::TurnStart,
            };
            payloads.push((
                prompt.thread_id.clone(),
                EventPayload::TurnItemUpdated(prompt),
            ));
        }
        let output = self.emit(&child, payloads, now);
        self.children.insert(native.into(), child);
        Some(output)
    }
    fn update(
        &mut self,
        native: &str,
        value: NodeStatus,
        progress: Option<String>,
        result: Option<String>,
        now: &Timestamp,
    ) -> Option<NativeOutput> {
        let mut child = self.children.remove(native)?;
        if terminal(child.task.status) {
            self.children.insert(native.into(), child);
            return None;
        }
        child.task.status = value;
        child.task.updated_at = now.clone();
        if progress.is_some() {
            child.task.progress = progress;
        }
        if result.is_some() {
            child.task.result = result;
        }
        if terminal(value) && child.task.result.is_none() {
            child.task.result = child
                .state
                .items
                .values()
                .filter(|i| matches!(i.body, TurnItemBody::AssistantMessage { .. }))
                .max_by_key(|i| i.ordinal)
                .and_then(|i| match &i.body {
                    TurnItemBody::AssistantMessage { text, .. } => Some(text.clone()),
                    _ => None,
                });
        }
        child.node.status = value;
        child.item.status = item_status(value);
        child.item.updated_at = now.clone();
        if let TurnItemBody::Subagent {
            progress, result, ..
        } = &mut child.item.body
        {
            *progress = child.task.progress.clone();
            *result = child.task.result.clone();
        }
        let mut payloads = Self::task_payloads(&child);
        if terminal(value) {
            child.task.completed_at = Some(now.clone());
            child.node.completed_at = Some(now.clone());
            child.item.completed_at = Some(now.clone());
            payloads = Self::task_payloads(&child);
            let mut completed = vec![];
            child.state.finish(
                match value {
                    NodeStatus::Completed => TurnStatus::Completed,
                    NodeStatus::Cancelled => TurnStatus::Cancelled,
                    NodeStatus::Interrupted => TurnStatus::Interrupted,
                    _ => TurnStatus::Failed,
                },
                None,
                now,
                &mut completed,
            );
            payloads.extend(Self::child_payloads(&child, completed));
        }
        let output = self.emit(&child, payloads, now);
        self.children.insert(native.into(), child);
        Some(output)
    }
    fn remember_tools(&mut self, frame: &Value) {
        let parent = frame["parent_tool_use_id"]
            .as_str()
            .map(|t| format!("tool:{t}"));
        if let Some(content) = frame["message"]["content"].as_array() {
            for block in content {
                if block["type"] == "tool_use"
                    && matches!(block["name"].as_str(), Some("Agent" | "Task"))
                    && let Some(id) = block["id"].as_str()
                {
                    self.tools.insert(
                        id.into(),
                        Launch {
                            parent: parent.clone(),
                            prompt: block["input"]["prompt"].as_str().unwrap_or_default().into(),
                            title: block["input"]["description"].as_str().map(str::to_owned),
                            model: block["input"]["model"].as_str().map(str::to_owned),
                        },
                    );
                }
            }
        }
    }
    fn hold(&mut self, frame: &Value) {
        let size = frame.to_string().len();
        // Do not evict earlier text silently: refuse further frames after the
        // bounded admission window. Task lifecycle still reports its result.
        if self.pending.len() < 512 && self.pending_bytes + size <= 1024 * 1024 {
            self.pending.push_back(frame.clone());
            self.pending_bytes += size;
        }
    }
    pub fn claude(
        &mut self,
        root: &TurnState,
        frame: &Value,
        now: &Timestamp,
    ) -> (bool, Vec<NativeOutput>) {
        self.remember_tools(frame);
        let mut outputs = vec![];
        if let Some(tool) = frame["parent_tool_use_id"].as_str() {
            let Some(native) = self.tool_tasks.get(tool).cloned() else {
                self.hold(frame);
                return (true, outputs);
            };
            let Some(mut child) = self.children.remove(&native) else {
                return (true, outputs);
            };
            if !terminal(child.task.status) {
                let mut frame = frame.clone();
                frame["parent_tool_use_id"] = Value::Null;
                let translated = normalize::claude_plain(child.state.take_owned(), &frame, now);
                child.state = translated.state;
                self.responses.extend(translated.immediate_responses);
                let payloads = Self::child_payloads(&child, translated.payloads);
                if !payloads.is_empty() {
                    outputs.push(self.emit(&child, payloads, now));
                }
            }
            self.children.insert(native, child);
            return (true, outputs);
        }
        if frame["type"] == "system"
            && matches!(
                frame["subtype"].as_str(),
                Some("task_started" | "task_progress" | "task_notification")
            )
        {
            let Some(native) = frame["task_id"].as_str() else {
                return (true, outputs);
            };
            if frame["subtype"] == "task_started" {
                let tool = frame["tool_use_id"].as_str();
                let launch = tool.and_then(|t| self.tools.get(t)).cloned().or_else(|| {
                    self.children.get(native).map(|c| Launch {
                        parent: None,
                        prompt: c.task.prompt.clone(),
                        title: c.task.title.clone(),
                        model: c.task.model.clone(),
                    })
                });
                if let Some(launch) = launch
                    && let Some(output) = self.register(root, native, launch, true, now)
                {
                    outputs.push(output);
                }
                if self.children.contains_key(native) {
                    if let Some(tool) = tool {
                        self.tool_tasks.insert(tool.into(), native.into());
                    }
                    let pending = std::mem::take(&mut self.pending);
                    self.pending_bytes = 0;
                    for buffered in pending {
                        let (_, more) = self.claude(root, &buffered, now);
                        outputs.extend(more);
                    }
                } else {
                    self.hold(frame);
                }
            } else {
                let value = if frame["subtype"] == "task_progress" {
                    NodeStatus::Running
                } else {
                    status(frame["status"].as_str().unwrap_or("completed"))
                };
                if let Some(output) = self.update(
                    native,
                    value,
                    frame["description"].as_str().map(str::to_owned),
                    frame["summary"].as_str().map(str::to_owned),
                    now,
                ) {
                    if frame["subtype"] == "task_notification"
                        && self
                            .children
                            .get(native)
                            .is_some_and(|c| c.task.thread_id == root.run.thread_id)
                        && self.children.get(native).is_some_and(|c| {
                            matches!(c.task.status, NodeStatus::Completed | NodeStatus::Failed)
                        })
                    {
                        self.wake_task = self.children.get(native).map(|c| c.task.id.clone());
                    }
                    outputs.push(output);
                }
            }
            return (true, outputs);
        }
        (false, outputs)
    }
    pub fn codex(
        &mut self,
        root: &TurnState,
        method: &str,
        params: &Value,
        request_id: Option<&Value>,
        now: &Timestamp,
    ) -> (bool, Vec<NativeOutput>) {
        let native = params["threadId"].as_str().unwrap_or_default();
        let mut outputs = vec![];
        let item = &params["item"];
        if matches!(method, "item/started" | "item/completed")
            && matches!(
                item["type"].as_str(),
                Some("collabAgentToolCall" | "subAgentActivity")
            )
        {
            if item["type"] == "subAgentActivity" {
                if let Some(id) = item["agentThreadId"].as_str() {
                    if item["kind"] == "started"
                        && let Some(output) = self.register(
                            root,
                            id,
                            Launch {
                                parent: self
                                    .children
                                    .contains_key(native)
                                    .then(|| native.to_owned()),
                                prompt: item["prompt"].as_str().unwrap_or_default().into(),
                                title: item["agentPath"].as_str().map(str::to_owned),
                                model: item["model"].as_str().map(str::to_owned),
                            },
                            false,
                            now,
                        )
                    {
                        outputs.push(output);
                    }
                    if let Some(value) = item["kind"].as_str().filter(|s| *s != "started")
                        && let Some(output) = self.update(
                            id,
                            status(value),
                            None,
                            item["message"].as_str().map(str::to_owned),
                            now,
                        )
                    {
                        outputs.push(output);
                    }
                }
            } else {
                if item["tool"] == "spawnAgent"
                    && let Some(ids) = item["receiverThreadIds"].as_array()
                {
                    for id in ids.iter().filter_map(Value::as_str) {
                        if let Some(output) = self.register(
                            root,
                            id,
                            Launch {
                                parent: self
                                    .children
                                    .contains_key(native)
                                    .then(|| native.to_owned()),
                                prompt: item["prompt"].as_str().unwrap_or_default().into(),
                                title: None,
                                model: item["model"].as_str().map(str::to_owned),
                            },
                            false,
                            now,
                        ) {
                            outputs.push(output);
                        }
                    }
                }
                if let Some(states) = item["agentsStates"].as_object() {
                    for (id, value) in states {
                        if let Some(output) = self.update(
                            id,
                            status(value["status"].as_str().unwrap_or("running")),
                            None,
                            value["message"].as_str().map(str::to_owned),
                            now,
                        ) {
                            outputs.push(output);
                        }
                    }
                }
            }
            return (true, outputs);
        }
        if self.children.contains_key(native) {
            let mut child = self.children.remove(native).expect("known child");
            if !terminal(child.task.status) {
                let translated = normalize::codex_plain(
                    child.state.take_owned(),
                    method,
                    params,
                    request_id,
                    now,
                );
                child.state = translated.state;
                let payloads = Self::child_payloads(&child, translated.payloads);
                if !payloads.is_empty() {
                    outputs.push(self.emit(&child, payloads, now));
                }
            }
            self.children.insert(native.into(), child);
            if method == "turn/completed"
                && let Some(output) = self.update(
                    native,
                    status(params["turn"]["status"].as_str().unwrap_or("failed")),
                    None,
                    params["turn"]["error"]["message"]
                        .as_str()
                        .map(str::to_owned),
                    now,
                )
            {
                outputs.push(output);
            }
            return (true, outputs);
        }
        (false, outputs)
    }
    pub fn stop(&mut self, now: &Timestamp) -> Vec<NativeOutput> {
        self.wake_task = None;
        self.buffered_wakes.clear();
        self.collecting_wake = None;
        self.pending.clear();
        self.pending_bytes = 0;
        let ids: Vec<_> = self.children.keys().cloned().collect();
        ids.into_iter()
            .filter_map(|id| self.update(&id, NodeStatus::Interrupted, None, None, now))
            .collect()
    }
}
pub fn claude(mut state: TurnState, frame: &Value, now: &Timestamp) -> Translation {
    let Some(mut agents) = state.native_agents.take() else {
        return normalize::claude_plain(state, frame, now);
    };
    if !state.interrupted
        && (state.terminal || state.native_wake_drain.is_some())
        && frame["parent_tool_use_id"].is_null()
        && (agents.collecting_wake.is_some() || agents.has_wake())
        && matches!(
            frame["type"].as_str(),
            Some("assistant" | "stream_event" | "result" | "control_request" | "user")
        )
    {
        state.native_continuation_offer = agents.buffer_wake(&state, frame.clone());
        state.native_agents = Some(agents);
        return Translation {
            state,
            payloads: vec![],
            immediate_responses: vec![],
        };
    }
    let (handled, outputs) = agents.claude(&state, frame, now);
    state.native_batches.extend(outputs);
    state.native_agents = Some(agents);
    let responses = std::mem::take(
        &mut state
            .native_agents
            .as_mut()
            .expect("native registry")
            .responses,
    );
    if handled {
        Translation {
            state,
            payloads: vec![],
            immediate_responses: responses,
        }
    } else {
        let mut translated = normalize::claude_plain(state, frame, now);
        translated.immediate_responses.extend(responses);
        translated
    }
}
pub fn codex(
    mut state: TurnState,
    method: &str,
    params: &Value,
    request_id: Option<&Value>,
    now: &Timestamp,
) -> Translation {
    let Some(mut agents) = state.native_agents.take() else {
        return normalize::codex_plain(state, method, params, request_id, now);
    };
    let (handled, outputs) = agents.codex(&state, method, params, request_id, now);
    state.native_batches.extend(outputs);
    state.native_agents = Some(agents);
    if handled {
        Translation {
            state,
            payloads: vec![],
            immediate_responses: vec![],
        }
    } else {
        normalize::codex_plain(state, method, params, request_id, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn setup(driver: Driver) -> (TurnState, store::Store) {
        let mut p = normalize::tests::projection(driver);
        p.provider_threads[0].native_thread_ref = Some(native_ref(driver, "root-native"));
        let store = store::Store::memory().unwrap();
        let mut payloads = vec![EventPayload::ThreadCreated(p.thread.clone())];
        payloads.extend(p.runs.iter().cloned().map(EventPayload::RunCreated));
        payloads.extend(
            p.attempts
                .iter()
                .cloned()
                .map(EventPayload::RunAttemptCreated),
        );
        payloads.extend(p.nodes.iter().cloned().map(EventPayload::NodeUpdated));
        payloads.extend(
            p.provider_threads
                .iter()
                .cloned()
                .map(EventPayload::ProviderThreadUpdated),
        );
        store
            .ingest(
                events(&p.thread.id, "seed", payloads, &crate::now()),
                None,
                &crate::now(),
            )
            .unwrap();
        let mut state = normalize::tests::state(driver);
        state.provider_thread.native_thread_ref = p.provider_threads[0].native_thread_ref.clone();
        state.native_agents = Some(Box::new(NativeAgents::new(p.thread)));
        (state, store)
    }
    fn commit(state: &mut TurnState, store: &store::Store) -> Vec<ThreadId> {
        std::mem::take(&mut state.native_batches)
            .into_iter()
            .flat_map(|batch| {
                store
                    .ingest_native(
                        batch.events,
                        &batch.thread_id,
                        &batch.run_id,
                        &batch.attempt_id,
                        &batch.owner,
                        &batch.now,
                    )
                    .unwrap()
                    .events
                    .into_iter()
                    .filter_map(|e| match e.event.payload {
                        EventPayload::ThreadCreated(t) => Some(t.id),
                        _ => None,
                    })
            })
            .collect()
    }
    fn frame(state: TurnState, value: Value) -> TurnState {
        claude(state, &value, &crate::now()).state
    }
    fn launch(state: TurnState) -> TurnState {
        let state = frame(
            state,
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Agent","input":{"prompt":"Investigate only this task","description":"Check behavior"}}]}}),
        );
        frame(
            state,
            json!({"type":"system","subtype":"task_started","task_id":"native-1","tool_use_id":"tool-1"}),
        )
    }
    #[test]
    fn fallback_and_start_retry_batches_have_distinct_ids_and_ingest() {
        let (mut state, store) = setup(Driver::Claude);
        for _ in 0..3 {
            let batch = state.batch(state.initial_payloads(), &crate::now());
            assert_eq!(
                store
                    .ingest(
                        batch.events,
                        Some((&state.run.id, Some(&state.attempt.id))),
                        &crate::now()
                    )
                    .unwrap()
                    .events
                    .len(),
                2
            );
            state = TurnState::prepare(
                state.run.clone(),
                state.attempt.clone(),
                state.provider_thread.clone(),
                state.session.clone(),
                state.turn.ordinal,
                &crate::now(),
            );
        }
    }
    #[test]
    fn completed_root_cannot_roll_back_until_its_native_child_stops() {
        let (mut state, store) = setup(Driver::Claude);
        state.provider_thread.provider_session_id = Some(state.session.id.clone());
        store
            .ingest(
                state.batch(state.initial_payloads(), &crate::now()).events,
                None,
                &crate::now(),
            )
            .unwrap();
        state = launch(state);
        commit(&mut state, &store);
        let mut p = store.projection(&state.run.thread_id).unwrap();
        p.runs[0].status = RunStatus::Completed;
        p.thread.active_provider_thread_id = Some(state.provider_thread.id.clone());
        let scope = CheckpointScope {
            id: CheckpointScopeId::new("test-root").unwrap(),
            thread_id: p.thread.id.clone(),
            run_id: Some(state.run.id.clone()),
            node_id: state.attempt.root_node_id.clone(),
            parent_scope_id: None,
            provider_thread_id: Some(state.provider_thread.id.clone()),
            kind: ScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: "/workspace".into(),
            created_at: crate::now(),
        };
        let cp = Checkpoint {
            id: CheckpointId::new("test-baseline").unwrap(),
            thread_id: p.thread.id.clone(),
            scope_id: scope.id.clone(),
            run_id: None,
            node_id: scope.node_id.clone(),
            parent_checkpoint_id: None,
            ordinal_within_scope: 0,
            app_run_ordinal: Some(0),
            reference: CheckpointRef::new("refs/t3/test").unwrap(),
            status: CheckpointStatus::Ready,
            files: vec![],
            captured_at: crate::now(),
        };
        p.checkpoint_scopes.push(scope.clone());
        p.checkpoints.push(cp.clone());
        assert!(rollback::target(&p, &scope.id, &cp.id).is_err());
        p.subagents[0].status = NodeStatus::Interrupted;
        assert!(rollback::target(&p, &scope.id, &cp.id).is_ok());
        let mut child_turn = state.turn;
        child_turn.run_attempt_id = None;
        child_turn.status = TurnStatus::Running;
        p.provider_turns.push(child_turn);
        assert!(rollback::target(&p, &scope.id, &cp.id).is_err());
    }
    #[test]
    fn restarting_retires_native_children_once_even_when_restart_is_chosen_by_delivery_intent() {
        for intent in [false, true] {
            let (mut state, store) = setup(Driver::Codex);
            let mut root = vec![];
            state.started(Some("root-turn"), &crate::now(), &mut root);
            store
                .ingest(state.batch(root, &crate::now()).events, None, &crate::now())
                .unwrap();
            let translated = codex(
                state,
                "item/started",
                &json!({"threadId":"root-native","item":{"type":"collabAgentToolCall","tool":"spawnAgent","receiverThreadIds":["child"],"prompt":"work"}}),
                None,
                &crate::now(),
            );
            let mut state = translated.state;
            let child = commit(&mut state, &store).remove(0);
            let run_id = state.run.id.clone();
            let command = Command {
                command_id: CommandId::new("restart-native").unwrap(),
                thread_id: state.run.thread_id.clone(),
                body: CommandBody::MessageDispatch(Box::new(MessageDispatch {
                    message_id: MessageId::new("restart-input").unwrap(),
                    text: "restart".into(),
                    dispatch_mode: if intent {
                        DispatchMode::QueueAfterActive
                    } else {
                        DispatchMode::RestartActive {
                            target_run_id: run_id.clone(),
                        }
                    },
                    delivery_intent: intent.then_some(DeliveryIntent::Restart),
                    created_by: CreatedBy::User,
                    creation_source: CreationSource::Desktop,
                    native_continuation: None,
                    delegated_completion: None,
                    source_plan_ref: None,
                    context: None,
                    attachments: vec![],
                    model_selection: None,
                })),
            };
            for replay in [false, true] {
                let commit = store
                    .dispatch(
                        &command,
                        &crate::now(),
                        &capabilities::capabilities(Driver::Codex).turns,
                        Driver::Codex,
                    )
                    .unwrap();
                assert_eq!(commit.replayed, replay);
                let p = store.projection(&command.thread_id).unwrap();
                assert_eq!(p.subagents[0].status, NodeStatus::Interrupted);
                assert!(
                    store
                        .projection(&child)
                        .unwrap()
                        .nodes
                        .iter()
                        .all(|n| !matches!(
                            n.status,
                            NodeStatus::Running | NodeStatus::Pending | NodeStatus::Waiting
                        ))
                );
            }
        }
    }
    #[test]
    fn continuation_overflow_always_preserves_a_bounded_terminal_failure() {
        for large in [false, true] {
            let mut buffer = WakeFrames::default();
            for _ in 0..600 {
                buffer.push(json!({"type":"assistant","message":{"content":if large { "x".repeat(100_000) } else { "delta".into() }}}));
            }
            buffer.push(json!({"type":"result","subtype":"success","num_turns":1}));
            assert!(buffer.frames.len() <= 512);
            assert!(buffer.bytes <= 2 * 1024 * 1024 + 1024);
            let last = buffer.frames.last().unwrap();
            assert_eq!(last["type"], "result");
            assert!(last["is_error"].as_bool().unwrap());
            let state = normalize::claude_plain(
                normalize::tests::state(Driver::Claude),
                last,
                &crate::now(),
            )
            .state;
            assert!(state.terminal);
            assert_eq!(state.run.status, RunStatus::Failed);
        }
    }
    #[test]
    fn claude_early_child_text_is_runless_and_owned_after_root_returns() {
        let (state, store) = setup(Driver::Claude);
        let state = frame(
            state,
            json!({"type":"assistant","parent_tool_use_id":"tool-1","message":{"id":"child-message","content":[{"type":"text","text":"Child answer"}]}}),
        );
        let mut state = launch(state);
        let children = commit(&mut state, &store);
        assert_eq!(children.len(), 1);
        let child = store.projection(&children[0]).unwrap();
        assert!(child.runs.is_empty());
        assert!(child.provider_sessions.is_empty());
        assert!(child.provider_threads[0].provider_session_id.is_none());
        assert!(child.provider_threads[0].native_thread_ref.is_none());
        assert!(
            child
                .messages
                .iter()
                .any(|m| m.text == "Child answer" && m.run_id.is_none())
        );
        let mut completed = state.run.clone();
        completed.status = RunStatus::Completed;
        store
            .ingest(
                events(
                    &state.run.thread_id,
                    "return",
                    vec![EventPayload::RunUpdated(completed.clone())],
                    &crate::now(),
                ),
                None,
                &crate::now(),
            )
            .unwrap();
        state.run = completed;
        state.terminal = true;
        state = frame(
            state,
            json!({"type":"system","subtype":"task_notification","task_id":"native-1","status":"completed","summary":"Final result"}),
        );
        commit(&mut state, &store);
        let parent = store.projection(&state.run.thread_id).unwrap();
        assert_eq!(parent.subagents[0].result.as_deref(), Some("Final result"));
        assert_eq!(parent.runs.len(), 1);
        assert_eq!(parent.runs[0].status, RunStatus::Completed);
        assert!(
            store
                .projection(&children[0])
                .unwrap()
                .nodes
                .iter()
                .all(|n| n.run_id.is_none())
        );
    }
    #[test]
    fn child_input_queues_until_native_work_finishes_and_nested_early_frames_survive() {
        let (state, store) = setup(Driver::Claude);
        let state = frame(
            state,
            json!({"type":"assistant","parent_tool_use_id":"tool-1","message":{"content":[{"type":"tool_use","id":"nested-tool","name":"Agent","input":{"prompt":"Nested task"}}]}}),
        );
        let state = frame(
            state,
            json!({"type":"system","subtype":"task_started","task_id":"nested-task","tool_use_id":"nested-tool"}),
        );
        let mut state = launch(state);
        let children = commit(&mut state, &store);
        assert_eq!(children.len(), 2);
        let child = state.native_agents.as_ref().unwrap().children["native-1"]
            .thread
            .id
            .clone();
        let mut dispatch = normalize::tests::projection(Driver::Claude).messages[0].clone();
        dispatch.id = MessageId::new("queued-native-input").unwrap();
        let command = Command {
            command_id: CommandId::new("queue-child-input").unwrap(),
            thread_id: child.clone(),
            body: CommandBody::MessageDispatch(Box::new(MessageDispatch {
                message_id: dispatch.id,
                created_by: CreatedBy::User,
                creation_source: CreationSource::Desktop,
                text: "Later input".into(),
                context: None,
                attachments: vec![],
                model_selection: None,
                source_plan_ref: None,
                delegated_completion: None,
                native_continuation: None,
                delivery_intent: None,
                dispatch_mode: DispatchMode::QueueAfterActive,
            })),
        };
        store
            .dispatch(
                &command,
                &crate::now(),
                &orchestration::capabilities::capabilities(Driver::Claude).turns,
                Driver::Claude,
            )
            .unwrap();
        assert_eq!(
            store.projection(&child).unwrap().runs[0].status,
            RunStatus::Queued
        );
        state = frame(
            state,
            json!({"type":"system","subtype":"task_notification","task_id":"native-1","status":"completed"}),
        );
        commit(&mut state, &store);
        assert_eq!(
            store.projection(&child).unwrap().runs[0].status,
            RunStatus::Starting
        );
    }
    #[test]
    fn native_ingestion_rejects_cross_thread_output_and_a_restarted_attempt() {
        let (state, store) = setup(Driver::Claude);
        let mut state = launch(state);
        commit(&mut state, &store);
        state = frame(
            state,
            json!({"type":"system","subtype":"task_progress","task_id":"native-1","description":"Working"}),
        );
        let batch = state.native_batches.remove(0);
        let mut evil = batch.events.clone();
        evil[0].thread_id = ThreadId::new("unrelated").unwrap();
        assert!(
            store
                .ingest_native(
                    evil,
                    &batch.thread_id,
                    &batch.run_id,
                    &batch.attempt_id,
                    &batch.owner,
                    &batch.now
                )
                .is_err()
        );
        let mut run = state.run.clone();
        run.active_attempt_id = Some(RunAttemptId::new("restarted").unwrap());
        store
            .ingest(
                events(
                    &state.run.thread_id,
                    "restart",
                    vec![EventPayload::RunUpdated(run)],
                    &crate::now(),
                ),
                None,
                &crate::now(),
            )
            .unwrap();
        assert!(
            store
                .ingest_native(
                    batch.events,
                    &batch.thread_id,
                    &batch.run_id,
                    &batch.attempt_id,
                    &batch.owner,
                    &batch.now
                )
                .unwrap()
                .events
                .is_empty()
        );
    }
    #[test]
    fn codex_child_turns_are_routed_to_the_child_and_stop_has_actual_native_ids() {
        let (state, store) = setup(Driver::Codex);
        let mut state=codex(state,"item/completed",&json!({"threadId":"root-native","item":{"type":"collabAgentToolCall","tool":"spawnAgent","receiverThreadIds":["codex-child"],"prompt":"Check task"}}),None,&crate::now()).state;
        let child = commit(&mut state, &store).remove(0);
        state = codex(
            state,
            "turn/started",
            &json!({"threadId":"codex-child","turn":{"id":"child-turn"}}),
            None,
            &crate::now(),
        )
        .state;
        commit(&mut state, &store);
        assert_eq!(
            state.native_agents.as_ref().unwrap().live_codex_turns(),
            vec![("codex-child".into(), "child-turn".into())]
        );
        state=codex(state,"item/agentMessage/delta",&json!({"threadId":"codex-child","turnId":"child-turn","itemId":"reply","delta":"answer"}),None,&crate::now()).state;
        commit(&mut state, &store);
        assert!(
            store
                .projection(&child)
                .unwrap()
                .messages
                .iter()
                .any(|m| m.text == "answer" && m.run_id.is_none())
        );
        let outputs = state.native_agents.as_mut().unwrap().stop(&crate::now());
        state.native_batches.extend(outputs);
        commit(&mut state, &store);
        assert_eq!(
            store.projection(&state.run.thread_id).unwrap().subagents[0].status,
            NodeStatus::Interrupted
        );
        assert!(
            store
                .projection(&child)
                .unwrap()
                .turn_items
                .iter()
                .all(|i| i.status != ItemStatus::Running)
        );
    }
    #[test]
    fn native_task_reopen_retains_one_child_and_terminal_progress_cannot_reopen_it() {
        let (state, store) = setup(Driver::Claude);
        let mut state = launch(state);
        let child = commit(&mut state, &store).remove(0);
        state = frame(
            state,
            json!({"type":"system","subtype":"task_notification","task_id":"native-1","status":"completed","summary":"first"}),
        );
        commit(&mut state, &store);
        state = frame(
            state,
            json!({"type":"system","subtype":"task_progress","task_id":"native-1","description":"late"}),
        );
        assert!(state.native_batches.is_empty());
        state = frame(
            state,
            json!({"type":"system","subtype":"task_started","task_id":"native-1","tool_use_id":"tool-1"}),
        );
        assert!(commit(&mut state, &store).is_empty());
        let parent = store.projection(&state.run.thread_id).unwrap();
        assert_eq!(parent.subagents.len(), 1);
        assert_eq!(parent.subagents[0].child_thread_id.as_ref(), Some(&child));
        assert_eq!(parent.subagents[0].status, NodeStatus::Running);
        assert!(parent.subagents[0].result.is_none());
        store.recover(&crate::now()).unwrap();
        assert_eq!(
            store.projection(&state.run.thread_id).unwrap().subagents[0].status,
            NodeStatus::Interrupted
        );
    }
    #[test]
    fn stop_preserves_native_ownership_until_confirmation_and_rejects_late_output() {
        let (state, store) = setup(Driver::Claude);
        let mut state = launch(state);
        let child = commit(&mut state, &store).remove(0);
        let command = Command {
            command_id: CommandId::new("stop-native").unwrap(),
            thread_id: state.run.thread_id.clone(),
            body: CommandBody::RunInterrupt {
                run_id: state.run.id.clone(),
                reason: None,
                hold_queue: true,
            },
        };
        store
            .dispatch(
                &command,
                &crate::now(),
                &orchestration::capabilities::capabilities(Driver::Claude).turns,
                Driver::Claude,
            )
            .unwrap();
        assert_eq!(
            store.projection(&state.run.thread_id).unwrap().subagents[0].status,
            NodeStatus::Running
        );
        state = frame(
            state,
            json!({"type":"assistant","parent_tool_use_id":"tool-1","message":{"id":"late","content":[{"type":"text","text":"Late output"}]}}),
        );
        commit(&mut state, &store);
        assert!(
            !store
                .projection(&child)
                .unwrap()
                .messages
                .iter()
                .any(|m| m.text == "Late output")
        );
        let stopped = state.native_agents.as_mut().unwrap().stop(&crate::now());
        for _ in 0..2 {
            state.native_batches.extend(stopped.clone());
            commit(&mut state, &store);
        }
        assert_eq!(
            store.projection(&state.run.thread_id).unwrap().subagents[0].status,
            NodeStatus::Interrupted
        );
        assert!(
            store
                .projection(&child)
                .unwrap()
                .nodes
                .iter()
                .all(|n| !matches!(
                    n.status,
                    NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
                ))
        );
    }
    #[test]
    fn native_wake_waits_in_the_queue_and_drains_into_the_promoted_run() {
        let (state, store) = setup(Driver::Claude);
        let mut state = launch(state);
        commit(&mut state, &store);
        let mut source = state.run.clone();
        source.status = RunStatus::Completed;
        store
            .ingest(
                events(
                    &source.thread_id,
                    "return",
                    vec![EventPayload::RunUpdated(source.clone())],
                    &crate::now(),
                ),
                None,
                &crate::now(),
            )
            .unwrap();
        state.run = source;
        state.terminal = true;
        state = frame(
            state,
            json!({"type":"system","subtype":"task_notification","task_id":"native-1","status":"completed","summary":"done"}),
        );
        commit(&mut state, &store);
        let user = Command {
            command_id: CommandId::new("next-user").unwrap(),
            thread_id: state.run.thread_id.clone(),
            body: CommandBody::MessageDispatch(
                MessageDispatch {
                    native_continuation: None,
                    delegated_completion: None,
                    source_plan_ref: None,
                    created_by: CreatedBy::User,
                    creation_source: CreationSource::Desktop,
                    message_id: MessageId::new("next-user").unwrap(),
                    text: "Next request".into(),
                    context: None,
                    attachments: vec![],
                    model_selection: None,
                    delivery_intent: None,
                    dispatch_mode: DispatchMode::QueueAfterActive,
                }
                .into(),
            ),
        };
        store
            .dispatch(
                &user,
                &crate::now(),
                &capabilities::capabilities(Driver::Claude).turns,
                Driver::Claude,
            )
            .unwrap();
        let translated = claude(
            state,
            &json!({"type":"assistant","message":{"id":"wake","content":[{"type":"text","text":"Background answer"}]}}),
            &crate::now(),
        );
        assert!(translated.payloads.is_empty());
        let mut state = translated.state;
        let batch = state.batch(translated.payloads, &crate::now());
        let offer = batch.native_continuation_offer.unwrap();
        store
            .offer_native_continuation(&state.run.thread_id, &offer, &crate::now())
            .unwrap();
        state = frame(
            state,
            json!({"type":"result","subtype":"success","result":"Background answer","origin":{"kind":"task-notification"}}),
        );
        let p = store.projection(&state.run.thread_id).unwrap();
        let queued = p
            .runs
            .iter()
            .find(|r| r.user_message_id == offer.message_id)
            .unwrap();
        assert_eq!(queued.status, RunStatus::Queued);
        assert!(
            !p.turn_items
                .iter()
                .any(|i| i.run_id.as_ref() == Some(&queued.id))
        );
        let mut user = p
            .runs
            .iter()
            .find(|r| r.user_message_id.as_str() == "next-user")
            .unwrap()
            .clone();
        user.status = RunStatus::Completed;
        store
            .ingest(
                events(
                    &p.thread.id,
                    "user-return",
                    vec![EventPayload::RunUpdated(user)],
                    &crate::now(),
                ),
                None,
                &crate::now(),
            )
            .unwrap();
        let p = store.projection(&p.thread.id).unwrap();
        let run = p
            .runs
            .iter()
            .find(|r| r.user_message_id == offer.message_id)
            .unwrap()
            .clone();
        assert_eq!(run.status, RunStatus::Starting);
        let frames = state
            .native_agents
            .as_mut()
            .unwrap()
            .take_wake(&offer.message_id)
            .unwrap();
        let attempt = p
            .attempts
            .iter()
            .find(|a| Some(&a.id) == run.active_attempt_id.as_ref())
            .unwrap()
            .clone();
        let mut drain = TurnState::prepare(
            run,
            attempt,
            state.provider_thread.clone(),
            state.session.clone(),
            2,
            &crate::now(),
        );
        drain.native_agents = state.native_agents.take();
        for frame in frames {
            let translated = claude(drain, &frame, &crate::now());
            drain = translated.state;
            let batch = drain.batch(translated.payloads, &crate::now());
            store
                .ingest(
                    batch.events,
                    Some((&batch.run_id, Some(&batch.attempt_id))),
                    &crate::now(),
                )
                .unwrap();
        }
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(p.runs.last().unwrap().status, RunStatus::Completed);
        assert!(p.messages.iter().any(|m| m.text == "Background answer"));
        assert!(
            p.turn_items
                .iter()
                .any(|i| matches!(i.body, TurnItemBody::Notification { .. }))
        );
    }
}
