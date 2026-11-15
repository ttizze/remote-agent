use super::*;
#[path = "stream_tests.rs"]
mod stream;

#[test]
fn scopes_preserve_provider_qualified_and_application_conversation_addresses() {
    use crate::session::{ProviderKind, SessionRef};
    for provider in [None, Some(ProviderKind::Codex), Some(ProviderKind::Claude)] {
        let scope = Scope::Conversation(SessionRef::new(provider, "thread".into()).unwrap());
        let json = serde_json::to_value(&scope).unwrap();
        assert_eq!(json["Conversation"]["id"], "thread");
        assert_eq!(
            serde_json::from_value::<Scope<SessionRef>>(json).unwrap(),
            scope
        );
    }
    assert_eq!(
        serde_json::to_value(Scope::<SessionRef>::List).unwrap(),
        "List"
    );
}
