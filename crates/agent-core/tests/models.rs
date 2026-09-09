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
