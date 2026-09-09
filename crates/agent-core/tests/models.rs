use agent_core::models::{Thread, ThreadList, ThreadResponse};
use serde_json::Value;

#[test]
fn conversation_corpus_preserves_unknown_fields_absence_and_null() {
    for input in [
        include_str!("fixtures/history.json"),
        include_str!("fixtures/events.json"),
        include_str!("fixtures/submission.json"),
    ] {
        let cases: Vec<Value> = serde_json::from_str(input).unwrap();
        for case in cases {
            for field in ["previous", "incoming", "expected", "snapshot", "listed"] {
                let Some(value) = case.get(field).filter(|value| value.is_object()) else {
                    continue;
                };
                // Submission decisions are not wire Thread records.
                if value.get("action").is_some() {
                    continue;
                }
                let thread: Thread = serde_json::from_value(value.clone()).unwrap();
                assert_eq!(
                    serde_json::to_value(thread).unwrap(),
                    *value,
                    "{}: {field}",
                    case["name"]
                );
            }
        }
    }
}

#[test]
fn operation_replies_preserve_opaque_thread_metadata() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/operations.json")).unwrap();
    for case in cases {
        let Some(value) = case.get("result") else {
            continue;
        };
        if value.get("thread").is_some() {
            let reply: ThreadResponse = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(reply).unwrap(),
                *value,
                "{}",
                case["name"]
            );
        } else if value.get("moreProjectIds").is_some() {
            let reply: ThreadList = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(reply).unwrap(),
                *value,
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn daemon_wire_preserves_nonempty_roots_and_structured_tool_results() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/daemon-wire.json")).unwrap();
    let list: ThreadList = serde_json::from_value(fixture["list"].clone()).unwrap();
    assert_eq!(list.projects[0].roots[0].path, "/fixture/workspace");
    assert_eq!(serde_json::to_value(list).unwrap(), fixture["list"]);
    let history: ThreadResponse = serde_json::from_value(fixture["history"].clone()).unwrap();
    let item = history.thread.turns.as_ref().unwrap()[0]
        .items
        .as_ref()
        .unwrap()
        .iter()
        .find(|item| item.kind.as_deref() == Some("mcpToolCall"))
        .unwrap();
    assert_eq!(
        item.result.as_ref().unwrap()["content"][0]["text"],
        "Fixture lookup result"
    );
    assert_eq!(serde_json::to_value(history).unwrap(), fixture["history"]);
}
