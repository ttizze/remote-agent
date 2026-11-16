//! Compact public metadata must never carry conversation bodies.
use crate::{models, session};
pub fn preview(text: &str) -> Option<String> {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(120).collect())
}

/// Native catalogs and saved headers expose the same compact list contract.
pub fn compact(thread: models::Thread) -> models::Thread {
    let name = thread
        .name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| thread.preview.as_deref().and_then(preview));
    models::Thread {
        id: thread.id,
        active_provider: thread.active_provider,
        parent_id: thread.parent_id,
        name,
        cwd: thread.cwd,
        project_id: thread.project_id,
        updated_at: thread.updated_at,
        last_input_at: thread.last_input_at,
        status: thread.status,
        capabilities: thread.capabilities,
        can_accept_direct_input: thread.can_accept_direct_input,
        has_changes: thread.has_changes,
        git_branch: thread.git_branch,
        list_stale: thread.list_stale,
        agent_id: thread.agent_id,
        history_read_state: thread.history_read_state,
        ..Default::default()
    }
}

pub fn unavailable(mut thread: models::Thread, issue: &str) -> models::Thread {
    thread.status = models::SessionStatus::Unavailable;
    thread.capabilities = Some(session::Capabilities {
        rename: true,
        fork: true,
        ..Default::default()
    });
    thread.history_read_state = Some(session::HistoryReadState::new(
        session::HistoryReadKind::Unavailable,
        vec![issue.to_owned()],
    ));
    thread
}

/// List metadata and conversation views come from the same committed header.
#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use crate::session::ProviderKind;
    use std::sync::Arc;

    #[test]
    fn opening_validates_the_actual_rpc_identity_and_required_header() {
        for (wrong_session, expected) in [
            (true, "conversation identity changed"),
            (false, "conversation header is missing"),
        ] {
            use crate::operations::RpcMethod;
            let session =
                SessionRef::new(None, uuid::Uuid::from_u128(u128::MAX - 1).to_string()).unwrap();
            let response = Opened {
                session: if wrong_session {
                    SessionRef::new(None, uuid::Uuid::from_u128(u128::MAX - 2).to_string()).unwrap()
                } else {
                    session.clone()
                },
                subscription_id: uuid::Uuid::from_u128(u128::MAX - 3),
                initial: vec![crate::orchestration::projection::ThreadStreamItem::Synchronized],
                provider: ProviderKind::Codex,
                capabilities: Default::default(),
                project_root: None,
            };
            let request = Subscribe {
                session: session.clone(),
                subscription: uuid::Uuid::from_u128(u128::MAX - 5),
                after_sequence: None,
            };
            assert_eq!(request.validate(&response), Err(expected));
            let create = Create {
                workspace: None,
                session,
                provider: ProviderKind::Codex,
                subscription: uuid::Uuid::from_u128(u128::MAX - 6),
                cwd: None,
                selection: Default::default(),
                initial: None,
            };
            assert_eq!(create.validate(&response), Err(expected));
        }
    }

    #[test]
    fn list_metadata_keeps_only_a_short_fallback_title_and_no_conversation_payload() {
        let native = models::Thread {
            last_input_at: Some(1234),
            name: Some("  ".into()),
            preview: Some(format!("\n {}\nprivate body", "界".repeat(140))),
            turns: Some(vec![Arc::new(models::Turn {
                id: "secret".into(),
                ..Default::default()
            })]),
            history_cursor: Some("private cursor".into()),
            history_has_more: Some(true),
            ..Default::default()
        };
        let metadata = compact(native);
        assert_eq!(metadata.last_input_at, Some(1234));
        assert_eq!(metadata.name.as_deref(), Some("界".repeat(120).as_str()));
        assert!(metadata.preview.is_none());
        assert!(metadata.turns.is_none());
        assert!(metadata.history_cursor.is_none());
        assert!(metadata.history_has_more.is_none());
        assert!(metadata.requests.is_empty());
        let named = compact(models::Thread {
            name: Some("explicit title".into()),
            preview: Some("ignored".into()),
            ..Default::default()
        });
        assert_eq!(named.name.as_deref(), Some("explicit title"));
    }
}
