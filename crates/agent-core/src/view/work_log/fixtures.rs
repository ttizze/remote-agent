//! Domain items of every kind, work-log entries and thread state for view tests.
use super::{ItemType, ToolLifecycleStatus, ToolSource, ToolSourceKind, WorkLogEntry, WorkTone};
use agent_domain::*;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) use crate::sync::fixtures::{at, plan, run, thread_id, thread_state};

pub(crate) fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

/// A completed item started at [`at`].
pub(crate) fn item(id: &str, kind: ItemKind) -> Item {
    Item {
        id: TurnItemId::new(id).unwrap(),
        run: None,
        attempt: None,
        native_key: id.into(),
        ordinal: 1,
        kind,
        status: ItemStatus::Completed,
        text: String::new(),
        started_at: at(),
        completed_at: Some(at()),
        output_omitted: false,
        output_indicates_failure: false,
    }
}

pub(crate) fn command(id: &str, command: &str) -> Item {
    item(
        id,
        ItemKind::CommandExecution {
            command: command.into(),
            cwd: None,
            exit_code: Some(0),
            title: None,
        },
    )
}

pub(crate) fn file_change(id: &str, changes: Value) -> Item {
    item(
        id,
        ItemKind::FileChange {
            changes: Json(changes),
        },
    )
}

pub(crate) fn dynamic_tool(id: &str, name: &str, input: Value, output: Option<Value>) -> Item {
    item(
        id,
        ItemKind::DynamicTool {
            presentation: ToolPresentation::default(),
            name: name.into(),
            input: Json(input),
            output: output.map(Json),
        },
    )
}

pub(crate) fn web_search(id: &str, query: &str) -> Item {
    item(
        id,
        ItemKind::WebSearch {
            query: query.into(),
            results: None,
        },
    )
}

pub(crate) fn reasoning(id: &str, text: &str) -> Item {
    item(id, ItemKind::Reasoning).text(text)
}

pub(crate) fn user_message(id: &str, message: &str) -> Item {
    item(
        id,
        ItemKind::UserMessage {
            message: MessageId::new(message).unwrap(),
        },
    )
}

pub(crate) fn assistant_message(id: &str, message: &str) -> Item {
    item(
        id,
        ItemKind::AssistantMessage {
            message: MessageId::new(message).unwrap(),
        },
    )
}

pub(crate) fn proposed_plan(id: &str, plan: &str) -> Item {
    item(
        id,
        ItemKind::ProposedPlan {
            plan: PlanId::new(plan).unwrap(),
        },
    )
}

pub(crate) fn todo_list(id: &str, plan: &str) -> Item {
    item(
        id,
        ItemKind::TodoList {
            plan: PlanId::new(plan).unwrap(),
        },
    )
}

pub(crate) fn approval_item(id: &str, request: &str) -> Item {
    item(
        id,
        ItemKind::ApprovalRequest {
            request: RuntimeRequestId::new(request).unwrap(),
        },
    )
}

pub(crate) fn user_input_item(id: &str, request: &str) -> Item {
    item(
        id,
        ItemKind::UserInputRequest {
            request: RuntimeRequestId::new(request).unwrap(),
        },
    )
}

pub(crate) fn subagent(id: &str, task: &str) -> Item {
    item(
        id,
        ItemKind::Subagent {
            task: NodeId::new(task).unwrap(),
        },
    )
}

pub(crate) fn compaction(id: &str, before: Option<u64>, after: Option<u64>) -> Item {
    item(id, ItemKind::Compaction { before, after })
}

pub(crate) fn error(id: &str, message: &str) -> Item {
    item(
        id,
        ItemKind::Error {
            message: message.into(),
            retry: None,
            code: None,
            class: None,
            retryable: None,
            reset_at: None,
        },
    )
}

pub(crate) fn system_notice(id: &str, message: &str) -> Item {
    item(
        id,
        ItemKind::SystemNotice {
            message: message.into(),
        },
    )
}

pub(crate) fn notification(id: &str, summary: &str) -> Item {
    item(
        id,
        ItemKind::Notification {
            notification: Notification {
                source: NotificationSource::Native(BackgroundKind::Command),
                child_thread: None,
                outcome: NotificationOutcome::Completed,
                summary: summary.into(),
                detail: None,
            },
        },
    )
}

pub(crate) fn thread_created(id: &str, thread: &str, title: &str) -> Item {
    item(
        id,
        ItemKind::ThreadCreated {
            thread: ThreadId::new(thread).unwrap(),
            run: None,
            title: title.into(),
            instance: "codex".into(),
            model: "gpt".into(),
        },
    )
}

pub(crate) fn fork(id: &str, parent: &str, boundary: u64) -> Item {
    item(
        id,
        ItemKind::Fork {
            parent: ThreadId::new(parent).unwrap(),
            boundary,
        },
    )
}

pub(crate) fn run_interrupt_request(id: &str) -> Item {
    item(id, ItemKind::RunInterruptRequest)
}

pub(crate) fn run_interrupt_result(id: &str, request: &str) -> Item {
    item(
        id,
        ItemKind::RunInterruptResult {
            request: TurnItemId::new(request).unwrap(),
        },
    )
}

/// Chainable changes to a fixture item.
pub(crate) trait ItemBuilder: Sized {
    fn status(self, status: ItemStatus) -> Self;
    fn run(self, run: &str) -> Self;
    fn ordinal(self, ordinal: u64) -> Self;
    fn text(self, text: &str) -> Self;
    fn started(self, at: &str) -> Self;
    fn completed(self, at: Option<&str>) -> Self;
    fn exit_code(self, exit_code: Option<i64>) -> Self;
    fn output_indicates_failure(self) -> Self;
    fn output_omitted(self) -> Self;
}

impl ItemBuilder for Item {
    fn status(mut self, status: ItemStatus) -> Self {
        self.status = status;
        if !status.terminal() {
            self.completed_at = None;
        }
        self
    }
    fn run(mut self, run: &str) -> Self {
        self.run = Some(RunId::new(run).unwrap());
        self
    }
    fn ordinal(mut self, ordinal: u64) -> Self {
        self.ordinal = ordinal;
        self
    }
    fn text(mut self, text: &str) -> Self {
        self.text = text.into();
        self
    }
    fn started(mut self, at: &str) -> Self {
        self.started_at = timestamp(at);
        self
    }
    fn completed(mut self, at: Option<&str>) -> Self {
        self.completed_at = at.map(timestamp);
        self
    }
    fn exit_code(mut self, code: Option<i64>) -> Self {
        if let ItemKind::CommandExecution { exit_code, .. } = &mut self.kind {
            *exit_code = code;
        }
        self
    }
    fn output_indicates_failure(mut self) -> Self {
        self.output_indicates_failure = true;
        self
    }
    fn output_omitted(mut self) -> Self {
        self.output_omitted = true;
        self
    }
}

/// A tool entry labelled "Tool call", created at [`at`].
pub(crate) fn entry(id: &str) -> WorkLogEntry {
    WorkLogEntry::new(id, at(), "Tool call", WorkTone::Tool)
}

/// The entry for an item: its id, type, run, start and lifecycle status.
pub(crate) fn item_entry(item: Item) -> WorkLogEntry {
    let mut entry = WorkLogEntry::new(
        item.id.as_str(),
        item.started_at.clone(),
        "Tool call",
        WorkTone::Tool,
    );
    entry.run = item.run.clone();
    entry.item_type = Some(ItemType::of(&item.kind));
    entry.tool_lifecycle_status = Some(item.status.into());
    entry.item = Some(Arc::new(item));
    entry
}

/// Chainable changes to a fixture entry.
pub(crate) trait EntryBuilder: Sized {
    fn label(self, label: &str) -> Self;
    fn tone(self, tone: WorkTone) -> Self;
    fn detail(self, detail: &str) -> Self;
    fn command(self, command: &str) -> Self;
    fn tool_title(self, title: &str) -> Self;
    fn request_kind(self, kind: &str) -> Self;
    fn item_type(self, item_type: ItemType) -> Self;
    fn status(self, status: ToolLifecycleStatus) -> Self;
    fn viewed_image(self, path: &str) -> Self;
    fn changed_files(self, files: &[&str]) -> Self;
    fn source(self, key: &str, name: &str, kind: ToolSourceKind) -> Self;
    fn created(self, at: &str) -> Self;
    /// The item without changing the entry's type or status.
    fn item(self, item: Item) -> Self;
}

impl EntryBuilder for WorkLogEntry {
    fn label(mut self, label: &str) -> Self {
        self.label = label.into();
        self
    }
    fn tone(mut self, tone: WorkTone) -> Self {
        self.tone = tone;
        self
    }
    fn detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.into());
        self
    }
    fn command(mut self, command: &str) -> Self {
        self.command = Some(command.into());
        self
    }
    fn tool_title(mut self, title: &str) -> Self {
        self.tool_title = Some(title.into());
        self
    }
    fn request_kind(mut self, kind: &str) -> Self {
        self.request_kind = Some(kind.into());
        self
    }
    fn item_type(mut self, item_type: ItemType) -> Self {
        self.item_type = Some(item_type);
        self
    }
    fn status(mut self, status: ToolLifecycleStatus) -> Self {
        self.tool_lifecycle_status = Some(status);
        self
    }
    fn viewed_image(mut self, path: &str) -> Self {
        self.viewed_image_path = Some(path.into());
        self
    }
    fn changed_files(mut self, files: &[&str]) -> Self {
        self.changed_files = Some(files.iter().map(|file| (*file).into()).collect());
        self
    }
    fn source(mut self, key: &str, name: &str, kind: ToolSourceKind) -> Self {
        self.tool_source = Some(ToolSource {
            key: key.into(),
            name: name.into(),
            kind,
            icon: None,
        });
        self
    }
    fn created(mut self, at: &str) -> Self {
        self.created_at = timestamp(at);
        self
    }
    fn item(mut self, item: Item) -> Self {
        self.item = Some(Arc::new(item));
        self
    }
}

pub(crate) fn message(id: &str, role: Role, text: &str) -> Message {
    Message {
        scheduled_task: None,
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: None,
        role,
        text: text.into(),
        attachments: vec![],
        intent: InputIntent::TurnStart,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}

pub(crate) fn task(id: &str, child_thread: &str, prompt: &str) -> Task {
    Task {
        original_message: None,
        native_task: None,
        background: false,
        id: NodeId::new(id).unwrap(),
        native_key: id.into(),
        run: None,
        attempt: RunAttemptId::new("attempt").unwrap(),
        child_thread: ThreadId::new(child_thread).unwrap(),
        parent_task: None,
        prompt: prompt.into(),
        title: None,
        started_at: at(),
        completed_at: None,
        model: None,
        status: ItemStatus::Running,
        result: None,
        progress: None,
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }
}

pub(crate) fn request(id: &str, body: RequestBody) -> Request {
    Request {
        owner_path: vec![],
        id: RuntimeRequestId::new(id).unwrap(),
        attempt: RunAttemptId::new("attempt").unwrap(),
        native_key: id.into(),
        body,
        capability: ResponseCapability::Live,
        status: RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: BTreeMap::new(),
        created_at: at(),
        resolved_at: None,
    }
}

pub(crate) fn approval(id: &str, kind: &str, title: &str) -> Request {
    request(
        id,
        RequestBody::Approval {
            kind: kind.into(),
            title: title.into(),
            detail: None,
            options: vec![],
            input: Json(Value::Null),
        },
    )
}

pub(crate) fn questions(id: &str, questions: Vec<Question>) -> Request {
    request(id, RequestBody::Questions { questions })
}

/// A created thread holding `items`.
pub(crate) fn state(items: Vec<Item>) -> State {
    State {
        items,
        ..thread_state("Thread")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_item_of_every_kind() {
        let items = [
            command("command", "pwd"),
            file_change("file", serde_json::json!([])),
            dynamic_tool("tool", "Read", serde_json::json!({}), None),
            web_search("search", "rust"),
            reasoning("thought", "Thinking"),
            user_message("user", "message-user"),
            assistant_message("assistant", "message-assistant"),
            proposed_plan("plan", "plan"),
            todo_list("todo", "plan"),
            approval_item("approval", "request"),
            user_input_item("input", "request"),
            subagent("subagent", "task"),
            compaction("compaction", Some(2), Some(1)),
            error("error", "Failed"),
            system_notice("notice", "Notice"),
            notification("notification", "Done"),
            thread_created("created", "child", "Child"),
            fork("fork", "parent", 3),
            run_interrupt_request("interrupt"),
            run_interrupt_result("interrupted", "interrupt"),
        ];
        let types: std::collections::HashSet<_> =
            items.iter().map(|item| ItemType::of(&item.kind)).collect();
        assert_eq!(types.len(), items.len());
        let state = State {
            runs: vec![run("run", 1, RunStatus::Running)],
            messages: vec![message("message-user", Role::User, "Hi")],
            plans: vec![plan("plan", "run")],
            tasks: vec![task("task", "child", "Explore")],
            requests: vec![
                approval("request", "command", "Run pwd"),
                questions("questions", vec![]),
            ],
            ..state(items.to_vec())
        };
        assert_eq!(state.items.len(), 20);
        assert_eq!(state.active_run().map(|run| run.id.as_str()), Some("run"));
        assert_eq!(
            state.thread.as_ref().map(|thread| &thread.id),
            Some(&thread_id())
        );
    }

    #[test]
    fn builds_entries_from_items() {
        let item = command("command", "pwd")
            .run("run")
            .ordinal(2)
            .status(ItemStatus::Running)
            .started("2026-06-20T00:00:01Z")
            .completed(None)
            .exit_code(None)
            .output_omitted();
        let entry = item_entry(item)
            .label("Ran command")
            .created("2026-06-20T00:00:02Z");
        assert_eq!(entry.item_type, Some(ItemType::CommandExecution));
        assert_eq!(
            entry.tool_lifecycle_status,
            Some(ToolLifecycleStatus::InProgress)
        );
        assert_eq!(entry.run.as_ref().map(RunId::as_str), Some("run"));
        assert_eq!(entry.created_at, timestamp("2026-06-20T00:00:02Z"));
    }
}
