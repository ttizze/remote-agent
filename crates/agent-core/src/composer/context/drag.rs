//! Dragged conversations are data references. One drop updates a draft atomically.
use super::{ContextAction, change};
use agent_protocol::{
    context::{ContextContent, MessageContext},
    session::SessionRef,
};

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_context(
    host: String,
    storage_scope: String,
    session: SessionRef,
    title: String,
) -> Option<MessageContext> {
    let address = serde_json::to_vec(&(&host, &storage_scope, &session)).ok()?;
    let context = MessageContext {
        id: format!(
            "thread:{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, &address)
        ),
        label: title.clone(),
        content: ContextContent::Thread {
            host,
            storage_scope,
            session,
            title,
        },
    };
    context.validate().ok()?;
    Some(context)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_drag_payload(contexts: Vec<MessageContext>) -> Option<String> {
    threads_only(&contexts).ok()?;
    agent_protocol::context::validate_contexts(&contexts).ok()?;
    serde_json::to_string(&contexts).ok()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_drag_contexts(
    payloads: Vec<String>,
) -> Result<Vec<MessageContext>, crate::client::AgentError> {
    let decode = || -> Result<_, &'static str> {
        if payloads.is_empty() || payloads.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
            return Err("invalid conversation drop size");
        }
        let mut contexts = Vec::new();
        for payload in payloads {
            let batch: Vec<MessageContext> =
                serde_json::from_str(&payload).map_err(|_| "invalid conversation drop")?;
            threads_only(&batch)?;
            contexts.extend(batch);
        }
        change(&[], ContextAction::Add { contexts })
    };
    decode().map_err(crate::client::error)
}

fn threads_only(contexts: &[MessageContext]) -> Result<(), &'static str> {
    if contexts.is_empty()
        || contexts
            .iter()
            .any(|context| !matches!(context.content, ContextContent::Thread { .. }))
    {
        return Err("drop does not contain conversations");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(host: &str, storage: &str, title: &str) -> MessageContext {
        thread_context(
            host.into(),
            storage.into(),
            SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "conversation".into(),
            )
            .unwrap(),
            title.into(),
        )
        .unwrap()
    }

    #[test]
    fn repeated_drags_replace_the_same_source_without_aliasing_hosts_or_storage() {
        let first = source("host", "storage", "First");
        let updated = source("host", "storage", "Renamed");
        assert_eq!(first.id, updated.id);
        assert_ne!(first.id, source("other", "storage", "First").id);
        assert_ne!(first.id, source("host", "replacement", "First").id);
        let contexts = thread_drag_contexts(vec![
            thread_drag_payload(vec![first.clone()]).unwrap(),
            thread_drag_payload(vec![updated.clone()]).unwrap(),
        ])
        .unwrap();
        assert_eq!(contexts, vec![updated]);
        assert_eq!(first.label, "First");
    }

    #[test]
    fn a_multi_drop_is_atomic_preserves_order_and_cannot_smuggle_other_context_kinds() {
        let first = source("first", "storage", "First");
        let second = source("second", "storage", "Second");
        let payload = thread_drag_payload(vec![first.clone(), second.clone()]).unwrap();
        assert_eq!(
            thread_drag_contexts(vec![payload.clone()]).unwrap(),
            vec![first.clone(), second.clone()]
        );
        let file = MessageContext {
            id: "file".into(),
            label: "file".into(),
            content: ContextContent::File {
                path: "/file".into(),
            },
        };
        assert!(thread_drag_payload(vec![file.clone()]).is_none());
        assert!(
            thread_drag_contexts(vec![payload, serde_json::to_string(&vec![file]).unwrap()])
                .is_err()
        );
        let current = vec![first];
        let invalid = MessageContext {
            label: "x".repeat(2049),
            ..second.clone()
        };
        assert!(
            change(
                &current,
                ContextAction::Add {
                    contexts: vec![invalid.clone(), second.clone()]
                }
            )
            .is_err()
        );
        assert!(
            change(
                &current,
                ContextAction::Add {
                    contexts: vec![second, invalid]
                }
            )
            .is_err()
        );
        assert_eq!(current.len(), 1);
        assert!(thread_drag_contexts(vec!["[]".into()]).is_err());
        assert!(thread_drag_contexts(vec!["[".into()]).is_err());
        assert!(thread_drag_contexts(vec!["x".repeat(1024 * 1024 + 1)]).is_err());
        assert!(thread_drag_contexts(Vec::new()).is_err());
        let mut boundary =
            thread_drag_payload(vec![source("host", "storage", "Boundary")]).unwrap();
        boundary.extend(std::iter::repeat_n(' ', 1024 * 1024 - boundary.len()));
        assert_eq!(
            thread_drag_contexts(vec![boundary.clone()]).unwrap().len(),
            1
        );
        boundary.push(' ');
        assert!(thread_drag_contexts(vec![boundary]).is_err());
    }
}
