use agent_client::conversation::{Conversation, HistoryPage, merge_older, refresh_history};
use serde_json::{Value, json};

#[test]
fn history_corpus_matches_native_projections() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/history.json")).unwrap();
    for mut case in cases {
        let state = Conversation {
            thread: case["previous"].take(),
            ..Default::default()
        };
        let (state, result) = if case["operation"] == "older" {
            let page = HistoryPage {
                turn: case["turnId"].as_str().map(str::to_owned),
                cursor: case["cursor"].take(),
            };
            let (state, result) =
                merge_older(state, json!({"thread":case["incoming"].take()}), &page);
            (state, result.map(|_| ()))
        } else {
            refresh_history(state, case["incoming"].take())
        };
        if let Some(error) = case["errorContains"].as_str() {
            assert!(result.unwrap_err().contains(error), "{}", case["name"]);
        } else {
            result.unwrap();
            assert_eq!(state.thread, case["expected"], "{}", case["name"]);
        }
    }
}
