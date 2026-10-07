use super::*;
use crate::commands::outbox::Phase;
use crate::view::work_log::fixtures::*;
use crate::view::work_log::presentation::{
    work_entry_display_indicates_tool_failure, work_entry_indicates_tool_failure,
    work_entry_indicates_tool_success,
};
use crate::view::work_log::{ToolIcon, ToolSource, ToolSourceKind, ToolSurface};
use agent_domain::{
    Answer, AttachmentKind, Attempt, CheckpointId, Question, RequestStatus, ResponseCapability,
    RetryProgress, RunStatus, ToolPresentation,
};
use serde_json::json;

/// A thread holding `items` and a completed run for each run they name.
fn timeline(items: Vec<Item>) -> State {
    let mut state = state(items);
    for item in state.items.clone() {
        if let Some(id) = &item.run
            && !state.runs.iter().any(|run| &run.id == id)
        {
            let ordinal = state.runs.len() as u64 + 1;
            state
                .runs
                .push(run(id.as_str(), ordinal, RunStatus::Completed));
        }
    }
    state
}

fn derive(state: &State) -> Vec<TimelineEntry> {
    derive_timeline_entries(state, &EntriesInput::default())
}

fn kinds(entries: &[TimelineEntry]) -> Vec<(&'static str, String)> {
    entries
        .iter()
        .map(|entry| {
            let kind = match entry.kind {
                TimelineEntryKind::Message { .. } => "message",
                TimelineEntryKind::ProposedPlan(_) => "proposed-plan",
                TimelineEntryKind::Work(_) => "work",
                TimelineEntryKind::Event(_) => "event",
                TimelineEntryKind::Handoff(_) => "handoff",
            };
            (kind, entry.id.clone())
        })
        .collect()
}

fn only_work(state: &State) -> WorkLogEntry {
    let entries = derive(state);
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries[0].work().cloned().expect("a work-log entry")
}

fn with_message(mut state: State, id: &str, role: Role, text: &str, run: &str) -> State {
    let mut message = message(id, role, text);
    message.run = Some(RunId::new(run).unwrap());
    state.messages.push(message);
    state
}

fn retry_error(status: ItemStatus, class: &str, attempt: u64) -> Item {
    let mut item = error("item-provider-retry", "Claude API overloaded.").run("run-provider-retry");
    if let ItemKind::Error {
        retry, class: kind, ..
    } = &mut item.kind
    {
        *retry = Some(RetryProgress {
            attempt,
            max_attempts: Some(10),
            delay_ms: Some(1_500),
        });
        *kind = Some(class.into());
    }
    item.status(status)
}

fn pending(id: &str, created_at: &str, queued: bool) -> PendingMessage {
    PendingMessage {
        command: agent_domain::CommandId::new(id).unwrap(),
        thread: thread_id(),
        id: MessageId::new(id).unwrap(),
        text: id.into(),
        attachments: vec![],
        context: None,
        queued,
        created_at: timestamp(created_at),
        phase: Phase::Queued,
    }
}

#[test]
fn labels_provider_retry_progress_delay_recovery_and_exhaustion() {
    let label = |item: &Item| provider_error_presentation(item).unwrap().label;
    assert_eq!(
        label(&retry_error(ItemStatus::Failed, "usage_limit", 2)),
        "Usage limit reached after 2/10 retries"
    );
    let recovered = only_work(&timeline(vec![retry_error(
        ItemStatus::Completed,
        "usage_limit",
        2,
    )]));
    assert_eq!(recovered.label, "Provider recovered (2/10 retries)");
    assert_ne!(
        recovered.source_activity,
        Some(SourceActivity::RuntimeWarning)
    );
    assert!(!work_entry_display_indicates_tool_failure(&recovered));
    let running =
        provider_error_presentation(&retry_error(ItemStatus::Running, "provider_error", 2))
            .unwrap();
    assert_eq!(
        (running.label.as_str(), running.detail.as_str()),
        (
            "Retrying provider (2/10)",
            "Claude API overloaded. Retrying in 1.5s."
        )
    );
    assert_eq!(
        label(&retry_error(ItemStatus::Completed, "provider_error", 2)),
        "Provider recovered (2/10 retries)"
    );
    assert_eq!(
        label(&retry_error(ItemStatus::Failed, "provider_error", 10)),
        "Provider error after 10/10 retries"
    );
}

#[test]
fn assigns_run_rollback_to_the_turn_start_message_instead_of_a_later_steer() {
    let run = "run-steered";
    let mut state = timeline(vec![
        user_message("item-start", "message-turn-start")
            .run(run)
            .ordinal(1),
        user_message("item-steer", "message-steer")
            .run(run)
            .ordinal(2),
        assistant_message("item-assistant", "message-assistant")
            .run(run)
            .ordinal(3),
    ]);
    state = with_message(state, "message-turn-start", Role::User, "Start", run);
    state = with_message(state, "message-steer", Role::User, "Steer", run);
    state.messages.last_mut().unwrap().intent = InputIntent::Steer;
    state = with_message(state, "message-assistant", Role::Assistant, "Done", run);
    let checkpoint = Checkpoint {
        status: CheckpointStatus::Ready,
        scope: None,
        id: CheckpointId::new("checkpoint-run-1").unwrap(),
        run: Some(RunId::new(run).unwrap()),
        run_ordinal: 1,
        native_heads: Default::default(),
        file_ref: "checkpoint-run-1".into(),
        files: vec![],
    };
    let targets = revert_turn_counts(&derive(&state), &[checkpoint]);
    assert_eq!(
        targets.into_iter().collect::<Vec<_>>(),
        [(MessageId::new("message-turn-start").unwrap(), 0)]
    );
}

#[test]
fn uses_visible_turn_item_order_and_keeps_provider_errors_in_the_work_log() {
    let run = "run-visible";
    let mut failed = error("item-error", "Invalid reasoning effort.")
        .run(run)
        .ordinal(5)
        .status(ItemStatus::Failed);
    if let ItemKind::Error { code, class, .. } = &mut failed.kind {
        *code = Some("invalid_request".into());
        *class = Some("validation_error".into());
    }
    let workspace = command("item-workspace-preparation", "Preparing workspace")
        .run(run)
        .ordinal(7)
        .text("Workspace preparation completed.");
    let mut state = timeline(vec![
        user_message("item-user", "message-user")
            .run(run)
            .ordinal(0),
        run_interrupt_request("item-interrupt-request")
            .run(run)
            .ordinal(1),
        command("item-command", "sleep 1")
            .run(run)
            .ordinal(2)
            .text("done"),
        run_interrupt_result("item-interrupt-result", "item-interrupt-request")
            .run(run)
            .ordinal(3),
        todo_list("item-todo", "plan-visible").run(run).ordinal(4),
        failed,
        thread_created(
            "item-thread-created",
            "thread-follow-up",
            "Follow-up thread",
        )
        .run(run)
        .ordinal(6),
        workspace,
    ]);
    state = with_message(state, "message-user", Role::User, "Start", run);
    state.messages[0].creation_source = "web".into();
    let entries = derive(&state);
    assert_eq!(
        kinds(&entries),
        [
            ("message", "message-user".into()),
            ("event", "item-interrupt-request".into()),
            ("work", "item-command".into()),
            ("event", "item-interrupt-result".into()),
            ("work", "item-error".into()),
            ("work", "item-thread-created".into()),
        ]
    );
    let user = entries[0].message().unwrap();
    assert_eq!(user.input_intent, Some(InputIntent::TurnStart));
    assert_eq!(user.created_by, Some(MessageAuthor::User));
    assert_eq!(user.creation_source.as_deref(), Some("web"));
    assert_eq!(
        entries[0].item().map(|item| item.id.as_str()),
        Some("item-user")
    );
    let command = entries[2].work().unwrap();
    assert_eq!(command.item.as_ref().unwrap().id.as_str(), "item-command");
    assert_eq!(command.command.as_deref(), Some("sleep 1"));
    assert_eq!(command.detail, None);
    let error = entries[4].work().unwrap();
    assert_eq!(error.label, "Provider error");
    assert_eq!(error.detail.as_deref(), Some("Invalid reasoning effort."));
    assert_eq!(error.tone, WorkTone::Info);
    assert_eq!(
        error.tool_lifecycle_status,
        Some(ToolLifecycleStatus::Failed)
    );
    assert_eq!(
        entries[5].work().unwrap().item_type,
        Some(ItemType::ThreadCreated)
    );
}

#[test]
fn keeps_task_progress_available_to_the_composer_and_out_of_the_timeline() {
    for status in [
        ItemStatus::Pending,
        ItemStatus::Running,
        ItemStatus::Completed,
    ] {
        let state = timeline(vec![
            todo_list("item-tasks", "plan-tasks")
                .run("run-tasks")
                .status(status),
        ]);
        assert_eq!(derive(&state), []);
    }
}

#[test]
fn keeps_failed_tool_items_tool_toned_so_groups_still_summarize() {
    let entry = only_work(&timeline(vec![
        command("item-failed-command", "ssh host true")
            .run("run-1")
            .text("connection refused")
            .exit_code(Some(255))
            .status(ItemStatus::Failed),
    ]));
    assert_eq!(entry.tone, WorkTone::Tool);
    assert_eq!(
        entry.tool_lifecycle_status,
        Some(ToolLifecycleStatus::Failed)
    );
}

#[test]
fn waits_for_a_dispatched_turn_item_before_adding_queued_input_to_the_timeline() {
    let queued = [pending(
        "message-dispatched-queued",
        "2026-06-20T00:00:00.000Z",
        true,
    )];
    let input = EntriesInput { pending: &queued };
    assert_eq!(derive_timeline_entries(&thread_state("Thread"), &input), []);
    let mut state = timeline(vec![
        user_message("item-dispatched-queued", "message-dispatched-queued")
            .run("run-dispatched-queued")
            .ordinal(200),
    ]);
    state = with_message(
        state,
        "message-dispatched-queued",
        Role::User,
        "Queued input",
        "run-dispatched-queued",
    );
    state.messages[0].created_by = MessageAuthor::Agent;
    state.messages[0].creation_source = "mcp".into();
    let promoted = derive_timeline_entries(&state, &input);
    assert_eq!(
        kinds(&promoted),
        [("message", "message-dispatched-queued".into())]
    );
    let message = promoted[0].message().unwrap();
    assert_eq!(message.input_intent, Some(InputIntent::TurnStart));
    assert_eq!(message.created_by, Some(MessageAuthor::Agent));
}

#[test]
fn appends_unsent_messages_after_committed_history_without_reordering_it() {
    let message_item = |id: &str, user: bool, at: &str, ordinal: u64| {
        let item = if user {
            user_message(&format!("item-{id}"), id)
        } else {
            assistant_message(&format!("item-{id}"), id)
        };
        item.run(&format!("run-{ordinal}"))
            .ordinal(ordinal)
            .started(at)
    };
    let state = timeline(vec![
        message_item("old-user", true, "2026-08-29T00:00:01Z", 1),
        message_item("old-assistant", false, "2026-08-29T00:00:02Z", 2),
        message_item("later-user", true, "2026-08-29T00:00:05Z", 3),
        message_item("later-assistant", false, "2026-08-29T00:00:04Z", 4),
    ]);
    let unsent = [pending("optimistic-user", "2026-08-29T00:00:00Z", false)];
    let entries = derive_timeline_entries(&state, &EntriesInput { pending: &unsent });
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        [
            "old-user",
            "old-assistant",
            "later-user",
            "later-assistant",
            "optimistic-user"
        ]
    );
    assert_eq!(entries[4].item(), None);
}

#[test]
fn deduplicates_unsent_messages_once_their_items_arrive() {
    let unsent = [pending("live-user", "2026-08-29T00:00:05Z", false)];
    let input = EntriesInput { pending: &unsent };
    let history = timeline(vec![
        user_message("item-history-user", "history-user")
            .run("run-1")
            .ordinal(1),
        assistant_message("item-history-assistant", "history-assistant")
            .run("run-1")
            .ordinal(2),
    ]);
    assert_eq!(
        derive_timeline_entries(&history, &input)
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["history-user", "history-assistant", "live-user"]
    );
    let mut items = history.items.clone();
    items.extend([
        user_message("item-live-user", "live-user")
            .run("run-2")
            .ordinal(3),
        assistant_message("item-live-assistant", "live-assistant")
            .run("run-2")
            .ordinal(4),
    ]);
    let live = timeline(items);
    let entries = derive_timeline_entries(&live, &input);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        [
            "history-user",
            "history-assistant",
            "live-user",
            "live-assistant"
        ]
    );
    assert!(entries[2].item().is_some());
}

#[test]
fn uses_projected_plan_status_and_file_contents_in_timeline_entries() {
    let run = "run-timeline-artifacts";
    let mut state = timeline(vec![
        proposed_plan("item-proposed-plan", "plan-timeline-artifacts")
            .run(run)
            .ordinal(0)
            .text("Finished plan"),
        file_change(
            "item-file-change",
            json!({"file_path": "src/example.ts", "content": "export const answer = 42;\n"}),
        )
        .run(run)
        .ordinal(1),
    ]);
    let mut plan = plan("plan-timeline-artifacts", run);
    plan.markdown.clear();
    plan.implemented_by = Some(RunId::new("run-implementation").unwrap());
    state.plans.push(plan);
    let entries = derive(&state);
    let TimelineEntryKind::ProposedPlan(plan) = &entries[0].kind else {
        panic!("a proposed plan entry")
    };
    assert_eq!(plan.status, PlanStatus::Completed);
    assert_eq!(plan.markdown, "Finished plan");
    let file = entries[1].work().unwrap();
    assert_eq!(file.detail, None);
    assert_eq!(file.changed_files, Some(vec!["src/example.ts".into()]));
}

#[test]
fn resolves_the_attempt_identity_of_each_item() {
    let run = RunId::new("run-steered").unwrap();
    let attempt = |id: &str, ordinal: u64, status: AttemptStatus| Attempt {
        id: RunAttemptId::new(id).unwrap(),
        run: run.clone(),
        ordinal,
        status,
        native_thread: None,
        native_turn: None,
        native_head: None,
        accepted: true,
        usage: None,
        context_usage: None,
        turn_usage: None,
        usage_accumulator: None,
        usage_observed: false,
        rejected_limits: Default::default(),
        started_at: at(),
        completed_at: None,
    };
    let in_attempt = |item: Item, attempt: &str| Item {
        attempt: Some(RunAttemptId::new(attempt).unwrap()),
        ..item
    };
    let mut state = timeline(vec![
        in_attempt(
            assistant_message("item-superseded", "message-superseded")
                .run("run-steered")
                .ordinal(0)
                .status(ItemStatus::Running),
            "attempt-1",
        ),
        in_attempt(
            assistant_message("item-active", "message-active")
                .run("run-steered")
                .ordinal(1)
                .status(ItemStatus::Running),
            "attempt-2",
        ),
    ]);
    state.attempts = vec![
        attempt("attempt-1", 1, AttemptStatus::Superseded),
        attempt("attempt-2", 2, AttemptStatus::Running),
    ];
    let attempts = |state: &State| -> Vec<_> {
        derive(state)
            .iter()
            .map(|entry| {
                let attempt = entry.attempt.as_ref().unwrap();
                (attempt.id.to_string(), attempt.status)
            })
            .collect()
    };
    assert_eq!(
        attempts(&state),
        [
            ("attempt-1".into(), AttemptStatus::Superseded),
            ("attempt-2".into(), AttemptStatus::Running)
        ]
    );
    state.attempts[1].status = AttemptStatus::Superseded;
    assert_eq!(attempts(&state)[1].1, AttemptStatus::Superseded);
}

#[test]
fn keeps_async_answers_in_the_question_row() {
    let mut question = questions(
        "question",
        vec![Question {
            required: true,
            id: "color".into(),
            header: "Color".into(),
            question: "Which color?".into(),
            multiple: false,
            options: vec![],
        }],
    );
    question.capability = ResponseCapability::Message;
    question.status = RequestStatus::Resolved;
    question.answers = Some([("color".to_string(), Answer::Text("Blue".into()))].into());
    let reply = user_message("answer", "async-answer:question")
        .run("native-run")
        .ordinal(2);
    let mut both = timeline(vec![
        user_input_item("native-item", "question")
            .run("native-run")
            .ordinal(1),
        reply.clone(),
    ]);
    both.requests.push(question.clone());
    both = with_message(
        both,
        "async-answer:question",
        Role::User,
        "Which color?\nBlue",
        "native-run",
    );
    let entries = derive(&both);
    assert_eq!(kinds(&entries), [("work", "native-item".into())]);
    let answer = entries[0].work().unwrap().question_answer.clone().unwrap();
    assert_eq!(
        answer.answers,
        [("color".to_string(), Answer::Text("Blue".into()))]
    );
    // A separately paged reply stays visible until its question history is available.
    let mut reply_only = timeline(vec![reply]);
    reply_only.requests.push(question);
    reply_only.messages = both.messages.clone();
    assert_eq!(
        kinds(&derive(&reply_only)),
        [("message", "async-answer:question".into())]
    );
}

#[test]
fn keeps_completed_command_failures_visible_without_exposing_output() {
    for item in [
        command("native-item", "foo").output_indicates_failure(),
        command("native-item", "foo").exit_code(Some(2)),
        command("native-item", "foo").text("bash: foo: command not found"),
    ] {
        let entry = only_work(&timeline(vec![item]));
        assert_eq!(entry.detail, None);
        assert_eq!(entry.command.as_deref(), Some("foo"));
        assert_eq!(
            entry.tool_lifecycle_status,
            Some(ToolLifecycleStatus::Completed)
        );
        assert!(work_entry_display_indicates_tool_failure(&entry));
        assert!(!work_entry_indicates_tool_success(&entry));
    }
}

#[test]
fn retains_read_image_previews_without_tool_output() {
    let entry = only_work(&timeline(vec![dynamic_tool(
        "native-item",
        "Read",
        json!({"file_path": "/workspace/reference.png"}),
        None,
    )]));
    assert_eq!(
        entry.viewed_image_path.as_deref(),
        Some("/workspace/reference.png")
    );
}

#[test]
fn labels_a_read_of_a_bare_filename_from_its_structured_input() {
    let entry = only_work(&timeline(vec![dynamic_tool(
        "native-item",
        "Read",
        json!({"file_path": "README"}),
        Some(json!("project notes")),
    )]));
    assert_eq!(entry.label, "Read README");
}

#[test]
fn keeps_browser_identity_and_its_source_on_a_completed_tool_row() {
    let mut item = dynamic_tool(
        "native-item",
        "browser_snapshot",
        json!({}),
        Some(json!({})),
    );
    if let ItemKind::DynamicTool { presentation, .. } = &mut item.kind {
        *presentation = ToolPresentation {
            title: None,
            source: Some(agent_domain::Json(
                json!({"key": "browser-use:browser", "name": "Chrome", "kind": "browser"}),
            )),
            surface: Some("browser".into()),
            icon: Some(agent_domain::Json(
                json!({"_tag": "website", "pageUrl": "https://example.com/checkout"}),
            )),
        };
    }
    let entry = only_work(&timeline(vec![item]));
    assert_eq!(entry.tool_surface, Some(ToolSurface::Browser));
    assert_eq!(
        entry.tool_icon,
        Some(ToolIcon::Website {
            page_url: "https://example.com/checkout".into(),
            favicon_url: None,
            favicon_url_dark: None,
        })
    );
    assert_eq!(
        entry.tool_source,
        Some(ToolSource {
            key: "browser-use:browser".into(),
            name: "Chrome".into(),
            kind: ToolSourceKind::Browser,
            icon: None,
        })
    );
    assert_eq!(
        entry.tool_lifecycle_status,
        Some(ToolLifecycleStatus::Completed)
    );
}

#[test]
fn keeps_provider_returned_images_on_an_assistant_message() {
    let image = Attachment {
        kind: AttachmentKind::Image,
        source: None,
        id: "assistant-image".into(),
        name: "screenshot.png".into(),
        mime_type: "image/png".into(),
        path: "assistant-image.png".into(),
        size: 512,
    };
    let mut state = with_message(
        timeline(vec![
            assistant_message("native-item", "assistant-native-image").run("native-run"),
        ]),
        "assistant-native-image",
        Role::Assistant,
        "Here is the screenshot.",
        "native-run",
    );
    state.messages[0].attachments = vec![image.clone()];
    let entries = derive(&state);
    let message = entries[0].message().unwrap();
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(message.attachments, [image]);
}

#[test]
fn keeps_an_idle_provider_task_neutral_without_a_completion_mark() {
    let mut entry = entry("idle-tool").label("Waiting for the next task");
    entry.tool_lifecycle_status = Some(ToolLifecycleStatus::Idle);
    assert!(!work_entry_indicates_tool_success(&entry));
    assert!(!work_entry_display_indicates_tool_failure(&entry));
}

fn tool_entry(status: ToolLifecycleStatus) -> WorkLogEntry {
    let mut entry = entry("entry-1").label("Ran command");
    entry.tool_lifecycle_status = Some(status);
    entry
}

#[test]
fn flags_success_status_rows_whose_output_text_reports_a_failure() {
    for detail in ["bash: foo: command not found", "<exited with exit code 2>"] {
        let entry = tool_entry(ToolLifecycleStatus::Completed).detail(detail);
        assert!(work_entry_indicates_tool_failure(&entry));
    }
}

#[test]
fn keeps_the_rendered_row_calm_when_only_the_command_mentions_failure_text() {
    let entry = tool_entry(ToolLifecycleStatus::Completed).command("rg 'command not found' src/");
    assert!(work_entry_indicates_tool_failure(&entry));
    assert!(!work_entry_display_indicates_tool_failure(&entry));
}

#[test]
fn does_not_call_a_clean_completed_row_failed() {
    let entry = tool_entry(ToolLifecycleStatus::Completed).detail("3 files changed");
    assert!(!work_entry_indicates_tool_failure(&entry));
    assert!(work_entry_indicates_tool_success(&entry));
}

#[test]
fn recovered_failure_text_no_longer_counts_as_success() {
    let entry = tool_entry(ToolLifecycleStatus::Completed).detail("ENOENT: no such file");
    assert!(!work_entry_indicates_tool_success(&entry));
}

#[test]
fn shows_the_full_system_notice_without_a_detail() {
    let message = "2026-03-14T16:11:12.550224Z ERROR codex_core::codex: failed to load skill";
    let entry = only_work(&timeline(vec![
        system_notice("diagnostic", message).run("run-1"),
    ]));
    assert_eq!(entry.label, message);
    assert_eq!(entry.detail, None);
}

#[test]
fn does_not_turn_an_unrelated_tool_payload_message_into_a_diagnostic() {
    let mut item = dynamic_tool(
        "diagnostic",
        "read_file",
        json!({}),
        Some(json!({"message": "failed to load skill"})),
    );
    if let ItemKind::DynamicTool { presentation, .. } = &mut item.kind {
        presentation.title = Some("Read file".into());
    }
    let entry = only_work(&timeline(vec![item]));
    assert_eq!(entry.label, "Read file");
    assert_eq!(entry.detail, None);
}

#[test]
fn places_a_context_handoff_before_the_run_that_received_it() {
    let mut state = timeline(vec![
        user_message("item-first", "first").run("run-1").ordinal(1),
        user_message("item-second", "second")
            .run("run-2")
            .ordinal(2),
    ]);
    let mut first = run("run-1", 1, RunStatus::Completed);
    first.selection.instance = "claude".into();
    first.selection.model = "sonnet".into();
    let mut second = run("run-2", 2, RunStatus::Completed);
    second.selection.instance = "codex".into();
    second.selection.model = "gpt".into();
    state.runs = vec![first, second];
    state = with_message(state, "first", Role::User, "First", "run-1");
    state.transfers.push(agent_domain::Transfer {
        native_source: None,
        instance: Some("codex".into()),
        target_run: None,
        delivery: Some(agent_domain::ContextDelivery {
            attempt: RunAttemptId::new("attempt-2").unwrap(),
            run: RunId::new("run-2").unwrap(),
            native_thread: None,
            status: agent_domain::ContextDeliveryStatus::Injected,
            item_ids: vec![],
            omitted_item_ids: vec![],
        }),
        id: agent_domain::ContextTransferId::new("handoff").unwrap(),
        kind: agent_domain::TransferKind::ProviderHandoff,
        source: thread_id(),
        target: thread_id(),
        boundary: 1,
        history: agent_domain::HistoricalContext {
            messages: vec![agent_domain::HistoricalMessage {
                role: Role::User,
                text: "First".into(),
                thread: thread_id().to_string(),
                run: Some("run-1".into()),
                item: "item-first".into(),
                provider_thread: None,
                status: "completed".into(),
                kind: "user_message".into(),
                run_status: Some("completed".into()),
            }],
            context: String::new(),
            omitted_items: 0,
            omitted_item_ids: vec![],
        },
        superseded: false,
    });
    assert_eq!(
        kinds(&derive(&state)),
        [
            ("message", "first".into()),
            ("handoff", "handoff:run-2".into()),
            ("message", "second".into()),
        ]
    );
}
