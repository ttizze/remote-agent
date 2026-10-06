//! Command, file and tool output is the item text.
use super::*;
use crate::sync::history::tests::{at, command_rows, item};
use agent_domain::{
    CompletionWake, DeliveryState, Fact, NodeId, RunAttemptId, ThreadId, ToolPresentation,
    TurnItemId, fold,
};
use serde_json::json;

fn tool(input: Value, output: Option<Value>) -> Item {
    item(
        "tool-1",
        1,
        ItemKind::DynamicTool {
            presentation: ToolPresentation {
                title: Some("MCP tool".into()),
                ..Default::default()
            },
            name: "mcp__github__fetch_pr".into(),
            input: Json(input),
            output: output.map(Json),
        },
        String::new(),
    )
}

fn command(input: &str, output: &str, exit_code: Option<i64>) -> Item {
    item(
        "command-1",
        1,
        ItemKind::CommandExecution {
            command: input.into(),
            cwd: None,
            exit_code,
            title: None,
        },
        output.into(),
    )
}

fn dynamic_input(item: &Item) -> &Value {
    match &item.kind {
        ItemKind::DynamicTool { input, .. } => &input.0,
        kind => panic!("{kind:?}"),
    }
}

fn dynamic_output(item: &Item) -> Option<&Value> {
    match &item.kind {
        ItemKind::DynamicTool { output, .. } => output.as_ref().map(|output| &output.0),
        kind => panic!("{kind:?}"),
    }
}

fn json_string<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

fn stored(sequence: u64, body: FactBody) -> Arc<[StoredFact]> {
    Arc::from(vec![StoredFact {
        global_seq: sequence,
        thread_seq: sequence,
        fact: Fact { at: at(), body },
    }])
}

#[test]
fn redacts_tool_output_while_keeping_its_input() {
    let item = tool(
        json!({ "file_path": "/workspace/reference.png" }),
        Some(json!({ "data": "private-image-data" })),
    );
    let projected = wire_item(&item);
    assert_eq!(
        dynamic_input(&projected),
        &json!({ "file_path": "/workspace/reference.png" })
    );
    assert_eq!(dynamic_output(&projected), None);
    assert!(!json_string(&*projected).contains("private-image-data"));
    assert_eq!(
        dynamic_output(&item),
        Some(&json!({ "data": "private-image-data" }))
    );
}

#[test]
fn preserves_provider_notices_in_bounded_items_and_live_events() {
    let notice = item(
        "notice",
        1,
        ItemKind::SystemNotice {
            message: "Safeguards flagged this message. Switched to Opus 4.8.".into(),
        },
        String::new(),
    );
    assert!(matches!(wire_item(&notice), Cow::Borrowed(_)));
    let mut state = command_rows(0);
    state.items.push(notice.clone());
    let live = stored(
        1,
        FactBody::ItemCompleted {
            id: notice.id.clone(),
            status: ItemStatus::Completed,
        },
    );
    assert!(Arc::ptr_eq(&client_facts(&state, &live), &live));
}

#[test]
fn omits_oversized_dynamic_tool_results_without_mutating_persistence_data() {
    let output = json!({ "content": [{ "type": "text", "text": format!("first line\n{}", "x".repeat(100_000)) }] });
    let item = tool(json!({ "pr": 42 }), Some(output.clone()));
    let projected = wire_item(&item);
    assert_eq!(dynamic_output(&item), Some(&output));
    assert!(json_string(&*projected).len() < 2_000);
    assert_eq!(dynamic_output(&projected), None);
}

#[test]
fn omits_even_small_dynamic_tool_results_while_retaining_input() {
    let item = tool(json!({ "pr": 42 }), Some(json!({ "ok": true })));
    let mut expected = tool(json!({ "pr": 42 }), None);
    expected.output_omitted = true;
    assert_eq!(*wire_item(&item), expected);
    assert_eq!(dynamic_output(&item), Some(&json!({ "ok": true })));
}

#[test]
fn keeps_undefined_dynamic_input_intact() {
    let item = tool(Value::Null, None);
    assert_eq!(*wire_item(&item), item);
}

#[test]
fn preserves_bounded_input_summary_normalization() {
    let cases = [
        (
            format!(" \t\r\n\n  first\t line  \r\n{}", "x\n".repeat(10_000)),
            "first line".to_owned(),
        ),
        (" \n".repeat(10_000), "Large tool output".to_owned()),
        (
            format!("{}\n{}", "a".repeat(160), "x".repeat(20_000)),
            "a".repeat(160),
        ),
        (
            format!("{}\n{}", "a".repeat(161), "x".repeat(20_000)),
            format!("{}…", "a".repeat(159)),
        ),
        (
            format!("{}\t b{}", "a".repeat(159), "x".repeat(20_000)),
            format!("{}…", "a".repeat(159)),
        ),
        (
            format!("  café\u{a0}\u{2003}😀 \t\r\n{}", "x".repeat(20_000)),
            "café 😀".to_owned(),
        ),
    ];
    for (index, (input, summary)) in cases.into_iter().enumerate() {
        let projected = wire_item(&tool(Value::String(input), None)).into_owned();
        assert_eq!(
            dynamic_input(&projected),
            &json!({ "summary": summary, "truncated": true }),
            "case {index}"
        );
    }
}

#[test]
fn uses_encoded_json_bytes_for_strings_near_the_dynamic_value_limit() {
    let small = tool(Value::String("\"".repeat(8_191)), None);
    assert_eq!(*wire_item(&small), small);
    let projected = wire_item(&tool(Value::String("\"".repeat(8_192)), None)).into_owned();
    assert_eq!(
        dynamic_input(&projected),
        &json!({ "summary": format!("{}…", "\"".repeat(159)), "truncated": true })
    );
}

#[test]
fn summarizes_an_oversized_structured_input() {
    let projected = wire_item(&tool(json!({ "text": "x".repeat(100_000) }), None)).into_owned();
    assert_eq!(dynamic_input(&projected)["truncated"], json!(true));
}

fn task(progress: String) -> Task {
    Task {
        original_message: None,
        native_task: None,
        background: false,
        id: NodeId::new("child-agent").unwrap(),
        native_key: "child-agent".into(),
        run: None,
        attempt: RunAttemptId::new("attempt").unwrap(),
        child_thread: ThreadId::new("child").unwrap(),
        parent_task: None,
        prompt: "Inspect code".into(),
        title: None,
        started_at: at(),
        completed_at: None,
        model: None,
        status: ItemStatus::Running,
        result: None,
        progress: Some(progress),
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }
}

#[test]
fn truncates_detail_at_a_utf8_boundary_without_changing_the_source() {
    let progress = format!("{}😀{}", "a".repeat(32_767), "x".repeat(100_000));
    let mut state = command_rows(0);
    state.tasks.push(task(progress.clone()));
    let state = Arc::new(state);
    let projected = client_state(&state);
    assert_eq!(
        projected.tasks[0].progress.as_deref(),
        Some(format!("{}\n… output truncated for transport", "a".repeat(32_767)).as_str())
    );
    assert_eq!(state.tasks[0].progress.as_deref(), Some(progress.as_str()));

    let live = stored(
        1,
        FactBody::TaskProgressed {
            id: NodeId::new("child-agent").unwrap(),
            progress: Some(progress.clone()),
            model: None,
        },
    );
    let FactBody::TaskProgressed { progress: sent, .. } = &client_facts(&state, &live)[0].fact.body
    else {
        panic!()
    };
    assert_eq!(sent, &projected.tasks[0].progress);
}

#[test]
fn omits_command_output_of_every_size() {
    for output in [String::new(), "small output".into(), "x".repeat(1_048_576)] {
        let item = command("test", &output, None);
        let projected = wire_item(&item);
        assert_eq!(projected.text, "");
        assert!(
            matches!(&projected.kind, ItemKind::CommandExecution { command, .. } if command == "test")
        );
        assert_eq!(projected.status, ItemStatus::Completed);
        // Clients fetch withheld output on demand, so they need to know it exists.
        assert_eq!(projected.output_omitted, !output.is_empty());
        assert_eq!(item.text, output);
    }
}

#[test]
fn bounds_fetched_command_input_without_changing_persistence() {
    for (input, expected) in [
        ("echo ok".to_owned(), "echo ok".to_owned()),
        (
            format!("{}😀", "a".repeat(262_143)),
            format!("{}\n… output truncated for transport", "a".repeat(262_143)),
        ),
    ] {
        let item = command(&input, "ok", None);
        let projected = detail_item(&item);
        assert!(
            matches!(&projected.kind, ItemKind::CommandExecution { command, .. } if *command == expected)
        );
        assert_eq!(projected.text, "ok");
        assert!(
            matches!(&item.kind, ItemKind::CommandExecution { command, .. } if *command == input)
        );
    }
}

#[test]
fn keeps_failure_evidence_without_retaining_command_output() {
    let item = command(
        "cat missing-file",
        "cat: missing-file: No such file or directory",
        None,
    );
    let projected = wire_item(&item).into_owned();
    assert_eq!(projected.text, "");
    assert!(projected.output_indicates_failure);
    assert_eq!(*wire_item(&projected), projected);
    let exited = wire_item(&command("cat missing-file", "", Some(2))).into_owned();
    assert!(exited.output_indicates_failure);
    assert!(matches!(
        exited.kind,
        ItemKind::CommandExecution {
            exit_code: Some(2),
            ..
        }
    ));
}

#[test]
fn omits_inline_file_bodies_but_preserves_file_identity() {
    let codex =
        json!([{ "path": "src/main.ts", "kind": { "type": "update" }, "diff": "+new code" }]);
    let claude =
        json!({ "file_path": "src/main.ts", "old_string": "old code", "new_string": "new code" });
    for (changes, identity) in [
        (
            codex,
            json!([{ "path": "src/main.ts", "kind": { "type": "update" } }]),
        ),
        (claude, json!({ "file_path": "src/main.ts" })),
    ] {
        let item = item(
            "file-1",
            1,
            ItemKind::FileChange {
                changes: Json(changes.clone()),
            },
            "+new code".into(),
        );
        let projected = wire_item(&item);
        assert_eq!(
            projected.kind,
            ItemKind::FileChange {
                changes: Json(identity)
            }
        );
        assert_eq!(projected.text, "");
        assert_eq!(
            item.kind,
            ItemKind::FileChange {
                changes: Json(changes)
            }
        );
        // A failed edit keeps the provider's error so expanding the row can show it.
        let mut failed = item.clone();
        failed.status = ItemStatus::Failed;
        failed.text = "String to replace not found".into();
        assert_eq!(wire_item(&failed).text, "String to replace not found");
        assert!(!json_string(&*wire_item(&failed)).contains("new code"));
    }
}

#[test]
fn retains_only_result_identities_and_failure_metadata_in_live_tool_events() {
    let output = json!({
        "isError": true,
        "structuredContent": { "threadId": "child-thread", "messageId": "message", "text": "PRIVATE_BODY" },
        "content": [{ "type": "text", "text": "PRIVATE_BODY" }],
    });
    let item = tool(json!({ "pr": 42 }), Some(output.clone()));
    let mut state = command_rows(0);
    state.items.push(item.clone());
    let live = stored(
        1,
        FactBody::ItemDetailChanged {
            id: item.id.clone(),
            kind: item.kind.clone(),
        },
    );
    let projected = client_facts(&state, &live);
    let FactBody::ItemProjected { item: sent } = &projected[0].fact.body else {
        panic!("{projected:?}")
    };
    assert_eq!(
        dynamic_output(sent),
        Some(&json!({ "isError": true, "threadId": "child-thread", "messageId": "message" }))
    );
    assert!(!json_string(&*projected).contains("PRIVATE_BODY"));
    assert_eq!(dynamic_output(&live_item(&live)), Some(&output));
    assert_eq!(*wire_item(sent), *sent);
}

fn live_item(facts: &[StoredFact]) -> Item {
    let FactBody::ItemDetailChanged { id, kind } = &facts[0].fact.body else {
        panic!()
    };
    let mut item = tool(Value::Null, None);
    item.id = id.clone();
    item.kind = kind.clone();
    item
}

#[test]
fn compacts_mcp_envelopes() {
    assert_eq!(compact_dynamic_tool_output(&json!({ "ok": true })), None);
    assert_eq!(
        compact_dynamic_tool_output(
            &json!([{ "type": "text", "text": "{\"taskId\":\"task-1\",\"status\":\"rolled_back\"}" }])
        ),
        Some(json!({ "taskId": "task-1", "status": "rolled_back" }))
    );
    assert_eq!(
        compact_dynamic_tool_output(
            &json!({ "content": [{ "text": { "text": "{\"threads\":[{\"threadId\":\"a\"},{\"status\":\"rolled_back\"}]}" } }] })
        ),
        Some(json!({ "threads": [{ "threadId": "a" }, { "status": "rolled_back" }] }))
    );
    // A partial batch must not leave a confident top-level id.
    assert_eq!(
        compact_dynamic_tool_output(&json!({ "threadId": "a", "threads": [{ "other": 1 }] })),
        None
    );
    assert_eq!(
        compact_dynamic_tool_output(&json!({ "error": "boom" })),
        Some(json!({ "isError": true }))
    );
    assert_eq!(
        compact_dynamic_tool_output(&json!({ "threadId": "x".repeat(257) })),
        None
    );
    let deep = json!({ "content": { "content": { "content": { "content": { "content": { "threadId": "a" } } } } } });
    assert_eq!(compact_dynamic_tool_output(&deep), None);
}

#[test]
fn recognizes_failure_text() {
    for text in [
        "ENOENT: open",
        "bash: foo: command not found",
        "Cannot find path 'C:\\x' because it does not exist.",
        "The term 'foo' is not recognized",
        "<exited with exit code 2>",
        "Process exited with exit code 1",
        "exit code: 3",
    ] {
        assert!(tool_output_indicates_failure(text), "{text}");
    }
    for text in [
        "exit code 0",
        "all good",
        "exit code: 0x",
        "Cannot find path x",
    ] {
        assert!(!tool_output_indicates_failure(text), "{text}");
    }
}

/// Output appended to a command reaches clients as the projected item, and the
/// client fold stays consistent with the projected state.
#[test]
fn command_output_facts_fold_into_the_projected_item() {
    let mut state = command_rows(0);
    let started = FactBody::ItemStarted {
        id: TurnItemId::new("command-1").unwrap(),
        run: None,
        attempt: None,
        native_key: "command-1".into(),
        ordinal: 1,
        kind: ItemKind::CommandExecution {
            command: "cat missing".into(),
            cwd: None,
            exit_code: None,
            title: None,
        },
    };
    let appended = FactBody::ItemTextAppended {
        id: TurnItemId::new("command-1").unwrap(),
        offset: 0,
        text: "No such file or directory".into(),
    };
    let facts: Arc<[StoredFact]> = [started, appended]
        .into_iter()
        .enumerate()
        .map(|(index, body)| StoredFact {
            global_seq: index as u64 + 1,
            thread_seq: index as u64 + 1,
            fact: Fact { at: at(), body },
        })
        .collect();
    let client = State::clone(&client_state(&Arc::new(state.clone())));
    let raw: Vec<Fact> = facts.iter().map(|stored| stored.fact.clone()).collect();
    state = fold(&state, &raw).unwrap();
    let delivered: Vec<Fact> = client_facts(&state, &facts)
        .iter()
        .map(|stored| stored.fact.clone())
        .collect();
    assert!(matches!(delivered[1].body, FactBody::ItemProjected { .. }));
    let folded = fold(&client, &delivered).unwrap();
    assert_eq!(folded, *client_state(&Arc::new(state)));
    let item = &folded.items[0];
    assert!(item.text.is_empty() && item.output_omitted && item.output_indicates_failure);
}
