use serde::{Deserialize, de::DeserializeOwned};
use std::path::PathBuf;

#[derive(Deserialize)]
struct Row {
    kind: String,
    case: serde_json::Value,
}

pub(crate) fn cases<T: DeserializeOwned>(kind: &str, expected_count: usize) -> Vec<T> {
    let directory = std::env::var_os("AGENT_CORE_CASES").expect("set AGENT_CORE_CASES");
    let path = PathBuf::from(directory).join("presentation.jsonl");
    let input = std::fs::read_to_string(path).expect("read Lean presentation fixtures");
    let cases: Vec<_> = input
        .lines()
        .map(|line| serde_json::from_str::<Row>(line).expect("invalid Lean presentation row"))
        .filter(|row| row.kind == kind)
        .map(|row| serde_json::from_value(row.case).expect("invalid Lean presentation case"))
        .collect();
    assert_eq!(cases.len(), expected_count, "incomplete {kind} corpus");
    cases
}

#[derive(Debug, Deserialize)]
struct HistoryCase {
    more: bool,
    busy: bool,
    oldest: bool,
    latest: bool,
    following: bool,
    load: bool,
}

#[test]
#[ignore = "requires Lean-generated presentation.jsonl"]
fn lean_core_history_conformance() {
    for row in cases::<HistoryCase>("history", 32) {
        let load = super::conversation::should_load_history(
            row.more,
            row.busy,
            row.oldest,
            row.latest,
            row.following,
        );
        assert_eq!(load, row.load, "{row:?}");
    }
}
