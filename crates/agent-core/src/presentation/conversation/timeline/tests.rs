use super::*;
use crate::{localization::UiLanguage, models, state::Snapshot};
use serde_json::{Value, json};
use std::collections::HashMap;

fn item(id: &str, status: &str, body: Value) -> Value {
    json!({"id":id,"status":status,"clientInputId":null,"body":{"inline":{"body":body}}})
}
fn user(id: &str) -> Value {
    item(
        id,
        "completed",
        json!({"userMessage":{"text":id,"content":[]}}),
    )
}
fn text(id: &str, phase: &str) -> Value {
    item(
        id,
        "completed",
        json!({"assistantText":{"text":id,"phase":phase}}),
    )
}
fn reasoning(id: &str) -> Value {
    item(
        id,
        "completed",
        json!({"reasoning":{"content":[],"summary":[id]}}),
    )
}
fn command(command: &str, status: &str) -> Value {
    item(
        command,
        status,
        json!({"commandExecution":{"command":command,"cwd":null,"output":"","exitCode":null}}),
    )
}
fn edit(id: &str) -> Value {
    item(
        id,
        "completed",
        json!({"fileChange":{"changes":[{"path":"a.txt","kind":"add","diff":"","proposal":null}],"output":""}}),
    )
}
fn tool(id: &str) -> Value {
    item(
        id,
        "completed",
        json!({"toolCall":{"kind":"dynamic","tool":id,"arguments":{},"content":[]}}),
    )
}

fn turn(
    language: UiLanguage,
    status: &str,
    items: Vec<Value>,
    error: Option<Value>,
) -> Arc<RenderedTurn> {
    let mut turn = json!({"id":"turn","status":status,"items":items,"durationMs":2500});
    if let Some(error) = error {
        turn["error"] = error;
    }
    let thread: models::Thread = serde_json::from_value(json!({
        "id":{"provider":"codex","id":"thread"},
        "turns":[turn]
    }))
    .unwrap();
    let snapshot = Snapshot::default();
    super::super::project_conversation(
        language,
        &snapshot.panes.primary().navigation.draft_key,
        &snapshot.sessions.pending_submissions,
        Arc::new(thread),
        &None,
    )
    .turns[0]
        .clone()
}

/// The rows, named for comparison.
fn names(rows: &[TurnRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            TurnRow::Row(row) => match &row.content {
                ConversationRowContent::User { item } => {
                    format!("user {}", item.data.body.clone().unwrap_or_default())
                }
                ConversationRowContent::Response { item, .. } => {
                    format!("message {}", item.data.body.clone().unwrap_or_default())
                }
                ConversationRowContent::Error { error } => format!("error {}", error.message),
                _ => "row".into(),
            },
            TurnRow::Fold { activity, open } => {
                format!("fold {} {open}", activity.activity_summary)
            }
            TurnRow::Working => "working".into(),
            TurnRow::Commentary(item) => {
                format!("commentary {}", item.data.body.clone().unwrap_or_default())
            }
            TurnRow::Group { summary, open, .. } => format!("group {summary} {open}"),
            TurnRow::Work { label, .. } => format!("work {label}"),
            TurnRow::Live { label, .. } => format!("live {label}"),
            TurnRow::Thinking => "thinking".into(),
            TurnRow::Requested { label, approval } => format!("requested {label} {approval}"),
            TurnRow::Retrying { label } => format!("retrying {label}"),
            TurnRow::Plan { title, text, .. } => format!("plan {title}: {text}"),
            TurnRow::Subagent { agent, .. } => {
                format!("subagent {}: {}", agent.name, agent.message)
            }
            TurnRow::Divider { label, .. } => format!("divider {label}"),
            TurnRow::Actions(row) => match &row.content {
                ConversationRowContent::Response { item, .. } => {
                    format!("actions {}", item.data.body.clone().unwrap_or_default())
                }
                _ => "actions".into(),
            },
            TurnRow::Interrupted => "interrupted".into(),
        })
        .collect()
}

fn work() -> Vec<Value> {
    vec![
        user("u"),
        text("checking", "commentary"),
        reasoning("planning"),
        command("./gradlew test", "completed"),
        text("next", "commentary"),
        command("pwd", "completed"),
        text("done", "final_answer"),
    ]
}

#[test]
fn a_completed_turn_folds_its_work_and_opens_it_as_commentary_groups_and_tools() {
    let turn = turn(UiLanguage::English, "completed", work(), None);
    let folded = turn_rows(&turn, &HashMap::new(), &HashSet::new());
    assert_eq!(
        names(&folded),
        ["user u", "fold Worked for 2.5s false", "message done"]
    );
    let TurnRow::Fold { activity, .. } = &folded[1] else {
        panic!("a fold")
    };
    let opened = [(activity.id.clone(), activity.status.clone())].into();
    let rows = turn_rows(&turn, &opened, &HashSet::new());
    assert_eq!(
        names(&rows),
        [
            "user u",
            "fold Worked for 2.5s true",
            "commentary checking",
            "group Ran 1 command false",
            "commentary next",
            "work pwd",
            "message done"
        ]
    );
    let TurnRow::Group { id, .. } = &rows[3] else {
        panic!("a group")
    };
    let group = turn_rows(&turn, &opened, &[id.clone()].into());
    assert_eq!(
        names(&group)[3..6],
        [
            "group Ran 1 command true",
            "work planning",
            "work ./gradlew test"
        ]
    );
}

#[test]
fn a_running_turn_leads_with_its_header_and_ends_in_its_live_work() {
    let mut items = work();
    items.truncate(6);
    items[5] = command("pwd", "running");
    let turn = turn(UiLanguage::English, "running", items, None);
    assert_eq!(
        names(&turn_rows(&turn, &HashMap::new(), &HashSet::new())),
        [
            "user u",
            "working",
            "message checking",
            "group Ran 1 command false",
            "message next",
            "live Running pwd"
        ]
    );
    let started = self::turn(UiLanguage::English, "running", vec![user("u")], None);
    assert_eq!(
        names(&turn_rows(&started, &HashMap::new(), &HashSet::new())),
        ["user u", "working", "thinking"]
    );
}

#[test]
fn failed_work_stays_open_a_row_each_and_an_interrupted_turn_says_so() {
    let items = vec![
        user("u"),
        reasoning("planning"),
        command("./gradlew test", "completed"),
    ];
    let error = json!({"message":"context is full","category":"contextLimit"});
    let failed = turn(UiLanguage::English, "failed", items.clone(), Some(error));
    assert_eq!(
        names(&turn_rows(&failed, &HashMap::new(), &HashSet::new())),
        [
            "user u",
            "work planning",
            "work ./gradlew test",
            "error context is full"
        ]
    );
    let interrupted = turn(UiLanguage::English, "interrupted", items, None);
    assert_eq!(
        names(&turn_rows(&interrupted, &HashMap::new(), &HashSet::new())),
        ["user u", "group Ran 1 command false", "interrupted"]
    );
    // The answer's actions follow the work that ends the settled turn.
    let commented = vec![
        user("u"),
        text("checking", "commentary"),
        reasoning("planning"),
        command("./gradlew test", "running"),
    ];
    let interrupted = turn(UiLanguage::English, "interrupted", commented.clone(), None);
    let rows = turn_rows(&interrupted, &HashMap::new(), &HashSet::new());
    assert_eq!(
        names(&rows),
        [
            "user u",
            "message checking",
            "group Ran 1 command false",
            "actions checking",
            "interrupted"
        ]
    );
    let actions = |row: &TurnRow| match row {
        TurnRow::Row(row) | TurnRow::Actions(row) => matches!(
            row.content,
            ConversationRowContent::Response {
                show_actions: true,
                ..
            }
        ),
        _ => false,
    };
    assert_eq!(
        rows.iter().map(actions).collect::<Vec<_>>(),
        [false, false, false, true, false]
    );
    let error = json!({"message":"context is full","category":"contextLimit"});
    let failed = turn(UiLanguage::English, "failed", commented, Some(error));
    assert_eq!(
        names(&turn_rows(&failed, &HashMap::new(), &HashSet::new())),
        [
            "user u",
            "message checking",
            "work planning",
            "work ./gradlew test",
            "error context is full",
            "actions checking"
        ]
    );
}

#[test]
fn a_retry_is_the_live_work() {
    let error = json!({"message":"stream disconnected","category":"overloaded",
        "details":null,"providerCode":null,"retry":{"retrying":true}});
    let turn = turn(
        UiLanguage::English,
        "running",
        vec![user("u"), command("./gradlew test", "running")],
        Some(error),
    );
    assert_eq!(
        names(&turn_rows(&turn, &HashMap::new(), &HashSet::new())),
        ["user u", "working", "retrying Retrying provider"]
    );
}

#[test]
fn a_request_is_the_live_work_while_it_waits() {
    use agent_protocol::requests::{ElicitationInput, RequestBody, RequestTarget};
    let mut thread: models::Thread = serde_json::from_value(json!({
        "id":{"provider":"codex","id":"thread"},
        "turns":[{"id":"turn","status":"running","items":[
            user("u"), reasoning("planning"), command("./gradlew test", "running")
        ]}]
    }))
    .unwrap();
    let request = agent_protocol::requests::Request {
        id: "request".into(),
        target: RequestTarget::Turn {
            turn_id: "turn".into(),
            item_id: None,
        },
        delivery: agent_protocol::session::RequestDelivery::Awaiting,
        body: RequestBody::Elicitation {
            server: "mcp".into(),
            message: "confirm".into(),
            input: ElicitationInput::Url {
                url: "https://example.com/".into(),
            },
        },
    };
    thread.requests = [(request.id.clone(), Arc::new(request))].into();
    let snapshot = Snapshot::default();
    let turn = super::super::project_conversation(
        UiLanguage::English,
        &snapshot.panes.primary().navigation.draft_key,
        &snapshot.sessions.pending_submissions,
        Arc::new(thread),
        &None,
    )
    .turns[0]
        .clone();
    assert_eq!(
        names(&turn_rows(&turn, &HashMap::new(), &HashSet::new())),
        ["user u", "working", "requested Input requested false"]
    );
}

#[test]
fn groups_name_their_tools_as_t3_code_does() {
    let summary = |language, items: Vec<Value>| {
        let mut all = vec![user("u")];
        all.extend(items);
        let turn = turn(language, "interrupted", all, None);
        names(&turn_rows(&turn, &HashMap::new(), &HashSet::new()))[1].clone()
    };
    use UiLanguage::{English, Japanese};
    assert_eq!(
        summary(
            English,
            vec![command("a", "completed"), command("b", "completed")]
        ),
        "group Ran 2 commands false"
    );
    assert_eq!(
        summary(
            English,
            vec![tool("t"), command("a", "completed"), edit("e")]
        ),
        "group Ran 1 command, changed 1 file, and performed 1 other action false"
    );
    assert_eq!(
        summary(English, vec![command("a", "completed"), edit("e")]),
        "group Ran 1 command and changed 1 file false"
    );
    assert_eq!(
        summary(Japanese, vec![command("a", "completed"), edit("e")]),
        "group 1 件のコマンドを実行しました、1 件のファイルを変更しました false"
    );
    assert_eq!(
        summary(English, vec![reasoning("a"), reasoning("b")]),
        "group Thought (×2) false"
    );
}

#[test]
fn plans_subagents_and_compaction_stand_apart_and_neutral_work_hides() {
    let body = |id: &str, body: Value| item(id, "completed", body);
    let mcp = json!({"toolCall":{"kind":"mcp","tool":"lookup","server":"fixture","namespace":null,
        "arguments":{},"result":null,"error":null,"content":[],"success":true,"durationMs":null}});
    let subagent = |message: Value| {
        json!({"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":null,
            "receivers":[],"states":[{"session":{"provider":"codex","id":"child"},"status":"completed",
            "message":message}],"agentId":null,"result":null}})
    };
    let turn = turn(
        UiLanguage::English,
        "interrupted",
        vec![
            user("u"),
            reasoning("checking"),
            command("./gradlew test", "completed"),
            body("plan", json!({"plan":{"text":"# Ship it\nsteps"}})),
            body("mcp", mcp),
            tool("t"),
            body("spawn", subagent(json!("Reviewed."))),
            body("activity", subagent(Value::Null)),
            body("web", json!({"webSearch":{"query":"Codex","action":null}})),
            body("image", json!({"imageView":{"path":"/tmp/a.png"}})),
            body("sleep", json!({"sleep":{}})),
            body("review", json!({"review":{"entering":true,"text":""}})),
            body("compaction", json!({"compaction":{}})),
        ],
        None,
    );
    assert_eq!(
        names(&turn_rows(&turn, &HashMap::new(), &HashSet::new())),
        [
            "user u",
            "group Ran 1 command false",
            "plan Ship it: # Ship it\nsteps",
            "group Used fixture integration and used 1 tool false",
            "subagent Subagent: Reviewed.",
            "work Web search",
            "divider Context compacted",
            "interrupted"
        ]
    );
}

#[test]
fn plan_exports_keep_all_markdown_and_suggest_a_safe_filename() {
    assert_eq!(plan_export("# ship.it").0, "shipit.md");
    assert_eq!(plan_export("# A  /  B").0, "a-b.md");
    assert_eq!(
        plan_export("# Ship (it)!\n\n- one\n\n"),
        ("ship-it.md".into(), "# Ship (it)!\n\n- one\n".into())
    );
    assert_eq!(
        plan_export("# 計画\n本文"),
        ("plan.md".into(), "# 計画\n本文\n".into())
    );
    assert_eq!(
        plan_export("no heading"),
        ("plan.md".into(), "no heading\n".into())
    );
}

#[test]
fn each_child_remains_navigable_when_adjacent_activity_updates_another_child() {
    let subagent = |id: &str, children: Vec<(&str, &str)>| {
        item(
            id,
            "completed",
            json!({"subagent":{
                "tool":"spawnAgent", "prompt":null, "model":null, "effort":null, "sender":null,
                "receivers":[], "states":children.into_iter().map(|(child, message)| json!({
                    "session":{"provider":"codex","id":child}, "status":"completed", "message":message
                })).collect::<Vec<_>>(), "agentId":null, "result":null
            }}),
        )
    };
    let mut update = subagent("update", vec![("one", "Finished")]);
    update["body"]["inline"]["body"]["subagent"]["states"][0]["status"] = "unknown".into();
    let turn = turn(
        UiLanguage::English,
        "interrupted",
        vec![
            subagent("spawn", vec![("one", "First"), ("two", "Second")]),
            update,
        ],
        None,
    );
    let agents = turn_rows(&turn, &HashMap::new(), &HashSet::new())
        .into_iter()
        .filter_map(|row| {
            if let TurnRow::Subagent { agent, .. } = row {
                Some((agent.target.unwrap().id, agent.message, agent.status))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        agents,
        [
            ("one".into(), "Finished".into(), TurnStatus::Completed),
            ("two".into(), "Second".into(), TurnStatus::Completed)
        ]
    );
}

#[test]
fn a_command_is_named_by_its_program() {
    assert_eq!(program_name("./gradlew test").as_deref(), Some("gradlew"));
    assert_eq!(
        program_name("RUST_LOG=1 cargo test").as_deref(),
        Some("cargo")
    );
    assert_eq!(program_name("/usr/bin/env").as_deref(), Some("env"));
    assert_eq!(program_name("  ").as_deref(), None);
}
