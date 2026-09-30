use agent_protocol::models::{ThreadList, ThreadResponse};
use serde_json::Value;

#[test]
fn daemon_wire_preserves_nonempty_roots_and_structured_tool_results() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/daemon-wire.json")).unwrap();
    let list: ThreadList = serde_json::from_value(fixture["list"].clone()).unwrap();
    assert_eq!(list.projects[0].roots[0].path, "/fixture/workspace");
    let history: ThreadResponse = serde_json::from_value(fixture["history"].clone()).unwrap();
    let item = history.thread.turns.as_ref().unwrap()[0]
        .items
        .as_ref()
        .unwrap()
        .iter()
        .find(|item| {
            matches!(
                item.body(),
                agent_protocol::items::ItemBody::ToolCall { .. }
            )
        })
        .unwrap();
    let agent_protocol::items::ItemBody::ToolCall {
        result: Some(result),
        ..
    } = item.body()
    else {
        panic!("tool result")
    };
    assert_eq!(result["content"][0]["text"], "Fixture lookup result");
}
