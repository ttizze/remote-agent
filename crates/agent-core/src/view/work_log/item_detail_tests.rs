use super::*;
use agent_domain::{
    ApprovalDecision, ApprovalOption, BackgroundKind, Json, NodeId, Notification,
    NotificationOutcome, NotificationSource, Plan, PlanId, PlanKind, PlanStep, Question, Request,
    RequestStatus, ResponseCapability, RunAttemptId, RunId, RuntimeRequestId, Timestamp,
    ToolPresentation, TurnItemId,
};
use serde_json::json;

fn at(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn item(kind: ItemKind) -> Item {
    Item {
        id: TurnItemId::new("item-1").unwrap(),
        run: Some(RunId::new("run-1").unwrap()),
        attempt: None,
        native_key: String::new(),
        ordinal: 0,
        kind,
        status: ItemStatus::Completed,
        text: String::new(),
        started_at: at("2026-06-20T00:00:00.000Z"),
        completed_at: Some(at("2026-06-20T00:00:05.000Z")),
        output_omitted: false,
        output_indicates_failure: false,
    }
}

fn command(command: &str, exit_code: Option<i64>, output: &str) -> Item {
    Item {
        text: output.into(),
        ..item(ItemKind::CommandExecution {
            command: command.into(),
            cwd: None,
            exit_code,
            title: None,
        })
    }
}

fn tool(input: Value, output: Option<Value>) -> Item {
    item(ItemKind::DynamicTool {
        presentation: ToolPresentation::default(),
        name: "mcp__docs__search".into(),
        input: Json(input),
        output: output.map(Json),
    })
}

fn web_search(query: &str, results: Option<Value>) -> Item {
    item(ItemKind::WebSearch {
        query: query.into(),
        results: results.map(Json),
    })
}

fn arg(key: &str, value: &str) -> ToolCallArg {
    ToolCallArg {
        key: key.into(),
        value: value.into(),
    }
}

fn request(id: &str, body: RequestBody) -> Request {
    Request {
        owner_path: vec![],
        id: RuntimeRequestId::new(id).unwrap(),
        attempt: RunAttemptId::new("attempt-1").unwrap(),
        native_key: id.into(),
        body,
        capability: ResponseCapability::Live,
        status: RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: Default::default(),
        created_at: at("2026-06-20T00:00:00.000Z"),
        resolved_at: None,
    }
}

fn plan(id: &str, kind: PlanKind, markdown: &str, steps: usize) -> Plan {
    Plan {
        kind,
        id: PlanId::new(id).unwrap(),
        run: RunId::new("run-1").unwrap(),
        native_key: id.into(),
        markdown: markdown.into(),
        steps: (0..steps)
            .map(|index| PlanStep {
                text: format!("Step {index}"),
                status: "pending".into(),
            })
            .collect(),
        implemented_by: None,
    }
}

#[test]
fn shows_the_full_trimmed_command_or_nothing() {
    assert_eq!(
        tool_call_lines(
            Some("  git status --short \n"),
            Some(&json!({ "ignored": 1 }))
        ),
        ToolCallLines {
            command: Some("git status --short".into()),
            ..Default::default()
        }
    );
    assert!(tool_call_lines(Some(" \n "), None).is_empty());
}

#[test]
fn shows_flat_arguments_as_pairs_and_keeps_empty_values() {
    assert_eq!(
        tool_call_lines(
            None,
            // Keys in sorted order: object key order depends on serde_json features.
            Some(&json!({ "filter": null, "limit": 5, "query": "rust", "reset": "", "tags": ["a", "b"] }))
        )
        .args,
        Some(vec![
            arg("filter", "null"),
            arg("limit", "5"),
            arg("query", "rust"),
            arg("reset", "\"\""),
            arg("tags", "[\"a\",\"b\"]"),
        ])
    );
    assert!(tool_call_lines(None, Some(&json!({}))).is_empty());
}

#[test]
fn formats_arguments_that_are_not_flat() {
    assert_eq!(
        tool_call_lines(
            None,
            Some(&json!([{ "type": "text", "text": "find docs" }]))
        )
        .args_text,
        Some("find docs".into())
    );
    // A summarized input is not the call's arguments, so it shows as data.
    assert_eq!(
        tool_call_lines(
            None,
            Some(&json!({ "summary": "Large input", "truncated": true }))
        )
        .args_text,
        Some("{\n  \"summary\": \"Large input\",\n  \"truncated\": true\n}".into())
    );
    assert!(tool_call_lines(None, Some(&json!([]))).is_empty());
    assert!(tool_call_lines(None, None).is_empty());
}

#[test]
fn keys_a_live_item_once_and_a_settled_item_by_its_last_update() {
    let running = Item {
        status: ItemStatus::Running,
        completed_at: None,
        ..command("vp test", None, "")
    };
    assert_eq!(turn_item_detail_revision(&running), "live");
    assert_eq!(
        turn_item_detail_revision(&command("vp test", Some(0), "")),
        "2026-06-20T00:00:05.000Z"
    );
}

#[test]
fn fetches_only_withheld_tool_content() {
    let omitted = Item {
        output_omitted: true,
        ..command("vp test", Some(0), "")
    };
    assert!(turn_item_needs_detail_fetch(&omitted));
    assert!(!turn_item_needs_detail_fetch(&command(
        "vp test",
        Some(0),
        "ok"
    )));
    assert!(turn_item_needs_detail_fetch(&tool(
        json!({ "summary": "Large input", "truncated": true }),
        None
    )));
    assert!(!turn_item_needs_detail_fetch(&tool(
        json!({ "query": "rust" }),
        None
    )));
    assert!(!turn_item_needs_detail_fetch(&Item {
        output_omitted: true,
        ..item(ItemKind::Reasoning)
    }));
}

#[test]
fn shows_command_output_text() {
    assert_eq!(
        turn_item_output_text(&command("vp test", Some(1), "  failed\n")),
        Some("  failed\n".into())
    );
    assert_eq!(
        turn_item_output_text(&command("vp test", Some(0), " \n")),
        None
    );
}

#[test]
fn formats_tool_output_blocks_and_json_text() {
    let output = |value: Value| turn_item_output_text(&tool(json!({}), Some(value)));
    assert_eq!(
        output(json!({ "content": [{ "type": "text", "text": "{\"a\":1}" }], "isError": false })),
        Some("{\n  \"a\": 1\n}".into())
    );
    assert_eq!(
        output(json!("{\"a\":1}\n\n[2]")),
        Some("{\n  \"a\": 1\n}\n\n[\n  2\n]".into())
    );
    assert_eq!(
        output(json!("{\"a\":1}\nnot json")),
        Some("{\"a\":1}\nnot json".into())
    );
    assert_eq!(
        output(json!([
            { "type": "text", "text": "Found" },
            { "type": "image", "data": "AA==" },
            { "type": "resource_link", "uri": "file:///a.md" },
            { "type": "resource", "resource": { "uri": "file:///b.md" } },
        ])),
        Some("Found\n[image]\nfile:///a.md\nfile:///b.md".into())
    );
    // Structured content shows only when the text is empty.
    let structured =
        json!({ "content": [{ "type": "text", "text": " " }], "structuredContent": { "n": 1 } });
    assert_eq!(
        output(structured.clone()),
        Some(serde_json::to_string_pretty(&structured).unwrap())
    );
    assert_eq!(
        output(
            json!({ "content": [{ "type": "text", "text": "Done" }], "structuredContent": { "n": 1 } })
        ),
        Some("Done".into())
    );
    assert_eq!(output(json!({ "content": "" })), None);
    assert_eq!(output(json!({})), None);
    assert_eq!(output(Value::Null), None);
    assert_eq!(output(json!(42)), Some("42".into()));
}

#[test]
fn hides_withheld_tool_output_until_it_is_fetched() {
    let withheld = Item {
        output_omitted: true,
        ..tool(json!({}), Some(json!("partial")))
    };
    assert_eq!(turn_item_output_text(&withheld), None);
}

#[test]
fn lists_web_search_results_as_titled_links() {
    let claude = json!([
        { "tool_use_id": "t1", "content": [
            { "title": " Tickets ", "url": "https://fifa.com/tickets" },
            { "title": "", "url": "https://fifa.com" },
        ] },
        "Summary text",
    ]);
    assert_eq!(
        turn_item_output_text(&web_search("tickets", Some(claude))),
        Some("Tickets\nhttps://fifa.com/tickets\n\nhttps://fifa.com".into())
    );
    assert_eq!(
        turn_item_output_text(&web_search(
            "rust",
            Some(
                json!([{ "title": "Rust", "url": "https://rust-lang.org", "snippet": " A language " }])
            )
        )),
        Some("Rust\nhttps://rust-lang.org\nA language".into())
    );
    assert_eq!(
        turn_item_output_text(&web_search("rust", Some(json!([])))),
        None
    );
    assert_eq!(turn_item_output_text(&web_search("rust", None)), None);
}

#[test]
fn offers_detail_only_for_rows_with_content() {
    let state = State {
        requests: vec![
            request(
                "approve-1",
                RequestBody::Approval {
                    kind: "command".into(),
                    title: "Run command".into(),
                    detail: Some("rm -rf build".into()),
                    options: vec![ApprovalOption {
                        label: "Allow".into(),
                        decision: ApprovalDecision::Accept,
                    }],
                    input: Json(json!({})),
                },
            ),
            request(
                "approve-2",
                RequestBody::Approval {
                    kind: "command".into(),
                    title: "Run command".into(),
                    detail: Some("  ".into()),
                    options: vec![],
                    input: Json(json!({})),
                },
            ),
            request(
                "ask-1",
                RequestBody::Questions {
                    questions: vec![Question {
                        required: true,
                        id: "scope".into(),
                        header: "Scope".into(),
                        question: "Which repository?".into(),
                        multiple: false,
                        options: vec![],
                    }],
                },
            ),
            request("ask-2", RequestBody::Questions { questions: vec![] }),
        ],
        plans: vec![
            plan("plan-1", PlanKind::Proposed, "# Plan", 0),
            plan("plan-2", PlanKind::Proposed, " \n", 0),
            plan("todo-1", PlanKind::Todo, "", 2),
            plan("todo-2", PlanKind::Todo, "", 0),
        ],
        ..State::default()
    };
    let has = |kind: ItemKind| turn_item_has_detail(&item(kind), &state);
    let request_id = |id: &str| RuntimeRequestId::new(id).unwrap();
    let plan_id = |id: &str| PlanId::new(id).unwrap();
    let notification = |detail: Option<&str>| ItemKind::Notification {
        notification: Notification {
            source: NotificationSource::Native(BackgroundKind::Command),
            child_thread: None,
            outcome: NotificationOutcome::Completed,
            summary: "Command finished".into(),
            detail: detail.map(Into::into),
        },
    };

    assert!(turn_item_has_detail(
        &Item {
            text: "Thinking".into(),
            ..item(ItemKind::Reasoning)
        },
        &state
    ));
    assert!(!has(ItemKind::Reasoning));

    assert!(turn_item_has_detail(
        &command("vp test", Some(0), ""),
        &state
    ));
    assert!(turn_item_has_detail(&command(" ", Some(2), ""), &state));
    assert!(turn_item_has_detail(&command(" ", None, "out"), &state));
    assert!(turn_item_has_detail(
        &Item {
            output_omitted: true,
            ..command(" ", Some(0), "")
        },
        &state
    ));
    assert!(!turn_item_has_detail(&command(" ", Some(0), " "), &state));

    assert!(has(ItemKind::FileChange {
        changes: Json(json!([]))
    }));
    assert!(has(ItemKind::Fork {
        parent: agent_domain::ThreadId::new("parent").unwrap(),
        boundary: 1
    }));

    assert!(turn_item_has_detail(&web_search("rust", None), &state));
    assert!(turn_item_has_detail(
        &web_search(" ", Some(json!([{ "url": "https://rust-lang.org" }]))),
        &state
    ));
    assert!(!turn_item_has_detail(
        &web_search(" ", Some(json!([]))),
        &state
    ));

    assert!(turn_item_has_detail(
        &tool(json!({ "q": "x" }), None),
        &state
    ));
    assert!(!turn_item_has_detail(&tool(json!({}), None), &state));
    assert!(turn_item_has_detail(
        &Item {
            output_omitted: true,
            ..tool(json!({}), None)
        },
        &state
    ));

    assert!(has(ItemKind::ApprovalRequest {
        request: request_id("approve-1")
    }));
    assert!(!has(ItemKind::ApprovalRequest {
        request: request_id("approve-2")
    }));
    assert!(!has(ItemKind::ApprovalRequest {
        request: request_id("missing")
    }));
    assert!(has(ItemKind::UserInputRequest {
        request: request_id("ask-1")
    }));
    assert!(!has(ItemKind::UserInputRequest {
        request: request_id("ask-2")
    }));

    assert!(has(notification(Some("Exit code 0"))));
    assert!(!has(notification(Some(" "))));
    assert!(!has(notification(None)));

    assert!(has(ItemKind::SystemNotice {
        message: "Compacted".into()
    }));
    assert!(!has(ItemKind::SystemNotice {
        message: " ".into()
    }));
    assert!(has(ItemKind::Error {
        message: "Failed".into(),
        retry: None,
        code: None,
        class: None,
        retryable: None,
        reset_at: None,
    }));

    assert!(has(ItemKind::ProposedPlan {
        plan: plan_id("plan-1")
    }));
    assert!(!has(ItemKind::ProposedPlan {
        plan: plan_id("plan-2")
    }));
    assert!(has(ItemKind::TodoList {
        plan: plan_id("todo-1")
    }));
    assert!(!has(ItemKind::TodoList {
        plan: plan_id("todo-2")
    }));

    assert!(!has(ItemKind::Subagent {
        task: NodeId::new("task-1").unwrap()
    }));
    assert!(!has(ItemKind::AssistantMessage {
        message: agent_domain::MessageId::new("message-1").unwrap()
    }));
}
