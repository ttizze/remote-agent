use agent_protocol::session::{ProviderKind, SessionRef};
use serde_json::json;

#[test]
fn malformed_session_refs_are_rejected_at_the_wire_boundary() {
    for id in [
        "".into(),
        " leading".into(),
        "trailing ".into(),
        "x".repeat(4097),
    ] {
        let session = json!({"provider":"claude", "id":id});
        assert!(serde_json::from_value::<SessionRef>(session).is_err());
    }
    assert!(serde_json::from_value::<SessionRef>(json!("claude:native")).is_err());
    assert!(SessionRef::new(ProviderKind::Claude, "x".repeat(4096)).is_ok());
    assert!(SessionRef::new(ProviderKind::Codex, "claude:".into()).is_ok());
}
