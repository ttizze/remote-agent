use super::*;
use serde::Deserialize;
use std::{env, fs, path::PathBuf};

#[derive(Deserialize)]
struct HistoryRow {
    parent: bool,
    id: u64,
    version: u64,
    request: bool,
    affected: bool,
}

impl HistoryRow {
    fn turn_item(&self) -> TurnItem {
        let mut value = item(&format!("item-{}", self.id), self.id);
        if self.parent {
            value.thread_id = "parent".into();
        }
        if !self.affected {
            value.run_id = Some("other-run".into());
        }
        let text = format!("version-{}", self.version);
        value.body = if self.request {
            TurnItemBody::ApprovalRequest {
                request_id: "request".into(),
                request_kind: "command".into(),
                prompt: Some(text),
                reason: None,
                options: vec![],
            }
        } else {
            TurnItemBody::SystemNotice { message: text }
        };
        value
    }

    fn projected(&self, position: usize) -> ProjectedTurnItem {
        let visibility = if self.parent {
            Visibility::Inherited
        } else {
            Visibility::Local
        };
        row(self.turn_item(), visibility, position as u64)
    }
}

fn rows(values: &[HistoryRow]) -> Vec<ProjectedTurnItem> {
    values
        .iter()
        .enumerate()
        .map(|(position, value)| value.projected(position))
        .collect()
}

fn items(values: &[HistoryRow]) -> Vec<Arc<TurnItem>> {
    values.iter().map(|row| Arc::new(row.turn_item())).collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryCase {
    rollback: bool,
    initial: Vec<HistoryRow>,
    cache: Vec<HistoryRow>,
    older: Vec<HistoryRow>,
    expected_rows: Vec<HistoryRow>,
    expected_items: Vec<HistoryRow>,
}

impl HistoryCase {
    fn check(&self, line: usize) {
        let mut current = projection();
        current.visible_turn_items = rows(&self.initial);
        current.turn_items = items(&self.cache);
        if self.rollback {
            current = apply(&current, Change::Run(run("rolled_back")));
        }
        let page = rows(&self.older);
        let merged = merge_older_history(&current, &page);
        let expected = ThreadProjection {
            visible_turn_items: rows(&self.expected_rows),
            turn_items: items(&self.expected_items),
            ..current
        };
        assert_eq!(merged, expected, "history.jsonl:{line}");
        assert_eq!(
            merge_older_history(&merged, &page),
            expected,
            "replay:{line}"
        );
    }
}

#[test]
#[ignore = "requires cases emitted by History.lean"]
fn lean_core_history_conformance() {
    let directory = env::var_os("AGENT_CORE_CASES").expect("missing AGENT_CORE_CASES");
    let cases = fs::read_to_string(PathBuf::from(directory).join("history.jsonl")).unwrap();
    assert_eq!(cases.lines().count(), 144, "incomplete history case matrix");
    for (index, line) in cases.lines().enumerate() {
        let case: HistoryCase = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("history.jsonl:{}: {error}", index + 1));
        case.check(index + 1);
    }
}
