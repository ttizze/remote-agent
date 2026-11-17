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
                    if item.data.image_sources.is_empty() {
                        format!("message {}", item.data.body.clone().unwrap_or_default())
                    } else {
                        format!("images {}", item.data.image_sources.join(", "))
                    }
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
            TurnRow::Requested { label, .. } => format!("requested {label}"),
            TurnRow::Retrying { label } => format!("retrying {label}"),
            TurnRow::Subagent { agent, .. } => {
                format!("subagent {}: {}", agent.name, agent.message)
            }
            TurnRow::Subagents { label, open, .. } => format!("subagents {label} {open}"),
            TurnRow::Divider { label, .. } => format!("divider {label}"),
            TurnRow::Plan { title, .. } => format!("plan {title}"),
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

/// Detail tests explicitly open the turn disclosure; its visibility contract
/// is checked separately by terminal_work_retains_its_disclosure.
fn opened_work(turn: &RenderedTurn) -> Vec<TurnRow> {
    opened_work_style(turn, TimelineStyle::T3)
}
fn opened_work_style(turn: &RenderedTurn, style: TimelineStyle) -> Vec<TurnRow> {
    let expansion = turn
        .rows
        .iter()
        .filter_map(|row| match &row.content {
            ConversationRowContent::ActivityHeader { activity } => {
                Some((activity.id.clone(), activity.status.clone()))
            }
            _ => None,
        })
        .collect();
    turn_rows(style, turn, &expansion, &HashSet::new())
        .into_iter()
        .filter(|row| !matches!(row, TurnRow::Fold { .. }))
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
    let folded = turn_rows(
        crate::presentation::conversation::TimelineStyle::T3,
        &turn,
        &HashMap::new(),
        &HashSet::new(),
    );
    assert_eq!(
        names(&folded),
        ["user u", "fold Worked for 2.5s false", "message done"]
    );
    let TurnRow::Fold { activity, .. } = &folded[1] else {
        panic!("a fold")
    };
    let opened = [(activity.id.clone(), activity.status.clone())].into();
    let rows = turn_rows(
        crate::presentation::conversation::TimelineStyle::T3,
        &turn,
        &opened,
        &HashSet::new(),
    );
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
    let group = turn_rows(
        crate::presentation::conversation::TimelineStyle::T3,
        &turn,
        &opened,
        &[id.clone()].into(),
    );
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
        names(&turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::new()
        )),
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
        names(&turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &started,
            &HashMap::new(),
            &HashSet::new()
        )),
        ["user u", "working", "thinking"]
    );
}

#[rstest::rstest]
#[case::failed("failed", Some(json!({"message":"provider failed","category":"other"})))]
#[case::interrupted("interrupted", None)]
fn terminal_work_retains_its_disclosure_and_keeps_the_outcome_visible(
    #[case] status: &str,
    #[case] error: Option<Value>,
) {
    let turn = turn(
        UiLanguage::English,
        status,
        vec![
            user("u"),
            reasoning("planning"),
            command("./gradlew test", "completed"),
        ],
        error,
    );
    let closed = turn_rows(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        &turn,
        &HashMap::new(),
        &HashSet::new(),
    );
    let (id, status) = closed
        .iter()
        .find_map(|row| match row {
            TurnRow::Fold {
                activity,
                open: false,
            } => Some((activity.id.clone(), activity.status.clone())),
            _ => None,
        })
        .expect("Bex keeps a disclosure for failed and interrupted work");
    assert!(
        !closed
            .iter()
            .any(|row| matches!(row, TurnRow::Work { .. } | TurnRow::Group { .. }))
    );
    let has_outcome = |rows: &[TurnRow]| {
        rows.iter().any(|row| {
            matches!(
                row,
                TurnRow::Interrupted
                    | TurnRow::Row(ConversationRow {
                        content: ConversationRowContent::Error { .. },
                        ..
                    })
            )
        })
    };
    assert!(has_outcome(&closed));
    let expanded = [(id, status)].into();
    let opened = turn_rows(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        &turn,
        &expanded,
        &HashSet::new(),
    );
    let groups = opened
        .iter()
        .filter_map(|row| match row {
            TurnRow::Group { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let revealed = turn_rows(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        &turn,
        &expanded,
        &groups,
    );
    assert!(
        names(&revealed)
            .iter()
            .any(|row| row == "work ./gradlew test")
    );
    assert!(has_outcome(&revealed));
}

#[test]
fn opened_terminal_work_keeps_details_actions_and_the_outcome() {
    let items = vec![
        user("u"),
        reasoning("planning"),
        command("./gradlew test", "completed"),
    ];
    let error = json!({"message":"context is full","category":"contextLimit"});
    let failed = turn(UiLanguage::English, "failed", items.clone(), Some(error));
    assert_eq!(
        names(&opened_work(&failed)),
        [
            "user u",
            "work planning",
            "work ./gradlew test",
            "error context is full"
        ]
    );
    let interrupted = turn(UiLanguage::English, "interrupted", items, None);
    assert_eq!(
        names(&opened_work(&interrupted)),
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
    let rows = opened_work(&interrupted);
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
        names(&opened_work(&failed)),
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

#[rstest::rstest]
#[case::running(vec![user("u"), command("./gradlew test", "running")])]
#[case::no_work(vec![user("u")])]
#[case::failed_work(vec![user("u"), command("./gradlew test", "failed")])]
fn a_retry_is_the_live_work(#[case] items: Vec<Value>) {
    let error = json!({"message":"stream disconnected","category":"overloaded",
        "details":null,"providerCode":null,"retry":{"retrying":true}});
    let turn = turn(UiLanguage::English, "running", items, Some(error));
    assert_eq!(
        names(&turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::new()
        )),
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
    thread.requests = [(request.id.clone(), Arc::new(request.clone()))].into();
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
        names(&turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::new()
        )),
        [
            "user u",
            "working",
            "group Ran 1 command false",
            "requested Input requested"
        ]
    );
    let rows = turn_rows(
        crate::presentation::conversation::TimelineStyle::T3,
        &turn,
        &HashMap::new(),
        &HashSet::new(),
    );
    let shown = rows
        .iter()
        .find_map(|row| match row {
            TurnRow::Requested { request, .. } => Some(request),
            _ => None,
        })
        .expect("a typed request row");
    assert_eq!(shown.id, request.id);
    assert_eq!(shown.request_body, request.body);
    assert!(shown.can_respond);
}

#[test]
fn groups_name_their_tools_as_t3_code_does() {
    let summary = |language, items: Vec<Value>| {
        let mut all = vec![user("u")];
        all.extend(items);
        let turn = turn(language, "interrupted", all, None);
        names(&opened_work(&turn))[1].clone()
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
fn assistant_documents_keep_normal_actions_while_subagents_and_compaction_stand_apart() {
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
        names(&opened_work_style(&turn, TimelineStyle::NATIVE)),
        [
            "user u",
            "group Ran 1 command false",
            "message # Ship it\nsteps",
            "group Used fixture integration and used 1 tool false",
            "actions # Ship it\nsteps",
            "subagent Subagent: Reviewed.",
            "work Web search",
            "divider Context compacted",
            "images /tmp/a.png",
            "interrupted"
        ]
    );
}

#[test]
fn completed_plans_stay_visible_when_the_work_is_collapsed() {
    let turn = turn(
        UiLanguage::English,
        "completed",
        vec![
            user("u"),
            command("check", "completed"),
            text("answer", "final"),
            item(
                "plan",
                "completed",
                json!({"plan":{"text":"# Ship it\nsteps"}}),
            ),
        ],
        None,
    );
    let rows = turn_rows(
        crate::presentation::conversation::TimelineStyle::T3,
        &turn,
        &HashMap::new(),
        &HashSet::new(),
    );
    assert!(rows.iter().any(|row| matches!(row, TurnRow::Plan { title, text, .. } if title == "Ship it" && text == "# Ship it\nsteps")));
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
    update["body"]["inline"]["body"]["subagent"]["result"] = "Result ready".into();
    let mut spawn = subagent("spawn", vec![("one", "First"), ("two", "Second")]);
    spawn["body"]["inline"]["body"]["subagent"]["model"] = "review-model".into();
    spawn["body"]["inline"]["body"]["subagent"]["effort"] = "high".into();
    let turn = turn(
        UiLanguage::English,
        "interrupted",
        vec![spawn, update],
        None,
    );
    let agents = opened_work(&turn)
        .into_iter()
        .find_map(|row| match row {
            TurnRow::Subagents { members, .. } => Some(members),
            _ => None,
        })
        .unwrap();
    let agents = agents
        .into_iter()
        .map(|member| {
            let agent = member.agent;
            assert_eq!(agent.model.as_deref(), Some("review-model"));
            assert_eq!(agent.effort.as_deref(), Some("high"));
            assert_eq!(
                agent.result.as_deref(),
                (agent.target.as_ref().unwrap().id == "one").then_some("Result ready")
            );
            (agent.target.unwrap().id, agent.message, agent.status)
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
fn a_new_child_model_does_not_inherit_an_old_models_effort() {
    let subagent = |id: &str, model: Option<&str>, effort: Option<&str>| {
        item(
            id,
            "completed",
            json!({"subagent":{
                "tool":"task","prompt":"Review","model":model,"effort":effort,"sender":null,
                "receivers":[{"provider":"codex","id":"child"}],"states":[],"agentId":null,"result":null
            }}),
        )
    };
    for (next_model, next_effort, expected_model, expected_effort) in [
        (None, None, "first-model", Some("high")),
        (Some("next-model"), None, "next-model", None),
        (None, Some("low"), "first-model", Some("low")),
        (
            Some("next-model"),
            Some("medium"),
            "next-model",
            Some("medium"),
        ),
    ] {
        let turn = turn(
            UiLanguage::English,
            "interrupted",
            vec![
                subagent("first", Some("first-model"), Some("high")),
                subagent("update", next_model, next_effort),
            ],
            None,
        );
        let rows = turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::new(),
        );
        let agent = rows
            .iter()
            .find_map(|row| match row {
                TurnRow::Subagent { agent, .. } => Some(agent),
                _ => None,
            })
            .unwrap();
        assert_eq!(agent.model.as_deref(), Some(expected_model));
        assert_eq!(agent.effort.as_deref(), expected_effort);
    }
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

#[test]
fn child_groups_follow_adjacency_and_provider_turn_boundaries_and_keep_the_first_identity() {
    let child = |id: &str, attempt: Option<uuid::Uuid>| {
        let mut item = item(
            id,
            "completed",
            json!({"subagent":{
                "tool":"task", "prompt":id,"model":null,"effort":null,"sender":null,
                "receivers":[{"provider":"codex","id":id}],"states":[],"agentId":null,"result":null
            }}),
        );
        item["attempt"] = json!(attempt);
        item
    };
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    for attempt in [None, Some(a)] {
        let turn = turn(
            UiLanguage::English,
            "interrupted",
            vec![
                child("one", attempt),
                child("two", attempt),
                text("barrier", "commentary"),
                child("three", attempt),
                child("four", Some(b)),
            ],
            None,
        );
        let rows = turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::new(),
        );
        let (id, members) = rows
            .iter()
            .find_map(|row| match row {
                TurnRow::Subagents {
                    id,
                    label,
                    members,
                    open,
                } => {
                    assert_eq!(label, "2 subagents");
                    assert!(!open);
                    Some((id.clone(), members.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            members
                .iter()
                .map(|member| member.agent.target.as_ref().unwrap().id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, TurnRow::Subagents { .. }))
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, TurnRow::Subagent { .. }))
                .count(),
            2
        );
        let opened = turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::from([id.clone()]),
        );
        assert!(opened.iter().any(|row| matches!(row, TurnRow::Subagents {id: key, members: same, open: true, ..} if *key == id && same == &members)));
        let another = turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &turn,
            &HashMap::new(),
            &HashSet::from(["unrelated".into()]),
        );
        assert_eq!(names(&rows), names(&another));
    }
}
