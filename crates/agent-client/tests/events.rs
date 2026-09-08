use agent_client::conversation::{Conversation, reduce};
use serde_json::Value;

#[test]
fn event_corpus_matches_native_projections() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/events.json")).unwrap();
    for mut case in cases {
        let mut state = Conversation {
            thread: case["previous"].take(),
            ..Default::default()
        };
        for event in case["events"].as_array_mut().unwrap() {
            (state, _) = reduce(state, event.take());
        }
        assert_eq!(state.thread, case["expected"], "{}", case["name"]);
    }
}

#[test]
fn submission_corpus_matches_native_routing() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/submission.json")).unwrap();
    for case in cases {
        let plan = conversation_presentation::state::plan_send(&case["snapshot"], &case["listed"]);
        assert_eq!(
            serde_json::to_value(plan).unwrap(),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}
