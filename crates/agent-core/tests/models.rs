use agent_core::models::{Thread, ThreadList, ThreadResponse};
use serde_json::{Value, json};

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

#[test]
fn thread_wire_preserves_unknown_fields_and_nullable_updates() {
    let source = json!({"id":"thread", "projectId":null, "future":{"nested":[true,7]},
        "turns":[{"id":"turn", "itemsNextCursor":null, "futureTurn":[1],
            "items":[{"id":"item", "type":"futureItem", "futureItem":{"text":"opaque"}}]}]});
    let thread: Thread = serde_json::from_value(source.clone()).unwrap();
    assert_eq!(thread.project_id, Some(None));
    assert_eq!(thread.cwd, None);
    assert_eq!(serde_json::to_value(thread).unwrap(), source);
}
