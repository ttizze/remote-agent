//! Address routing must retain the provider qualifier through serialization.
use super::*;
use crate::session::{ProviderKind, SessionRef};
use proptest::prelude::*;

proptest! {
    #[test]
    fn serialized_scopes_keep_arbitrary_conversation_addresses(id in "[a-zA-Z0-9/_ .あ界é-]{1,64}") {
        let id=id.trim();
        prop_assume!(!id.is_empty());
        for provider in [None,Some(ProviderKind::Codex),Some(ProviderKind::Claude),Some(ProviderKind::DeepSeek)] {
            let scope=Scope::Conversation(SessionRef::new(provider,id.to_string()).unwrap());
            let decoded: Scope<SessionRef>=serde_json::from_slice(&serde_json::to_vec(&scope).unwrap()).unwrap();
            prop_assert_eq!(decoded,scope);
        }
    }
}
