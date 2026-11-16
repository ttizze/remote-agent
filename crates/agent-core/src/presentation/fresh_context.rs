use crate::{session::SessionRef, state::Snapshot};

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn fresh_context_turns(&self, session: SessionRef) -> Vec<agent_protocol::ids::TurnId> {
        self.sessions
            .conversations
            .get(&session)
            .into_iter()
            .flat_map(|thread| thread.turns.iter().flatten())
            .filter(|turn| turn.fresh_context)
            .map(|turn| turn.id.clone())
            .collect()
    }
    pub fn fresh_context_available(&self, session: SessionRef) -> bool {
        self.connection.connected
            && self
                .sessions
                .conversations
                .get(&session)
                .is_some_and(|thread| {
                    thread.preparations.is_empty()
                        && thread.requests.is_empty()
                        && thread.can_accept_direct_input != Some(false)
                        && (thread.parent_id.is_none()
                            || thread.can_accept_direct_input == Some(true))
                        && thread.turns.as_ref().is_some_and(|turns| !turns.is_empty())
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Thread, Turn},
        state::{Draft, DraftKey, Event, Intent, reduce, reduce_event},
    };
    use std::sync::Arc;

    fn ready() -> (Snapshot, SessionRef) {
        let session =
            SessionRef::new(crate::session::ProviderKind::Codex, "conversation".into()).unwrap();
        let mut state = Snapshot::default();
        state.connection.connected = true;
        state.connection.storage_scope = "host".into();
        Arc::make_mut(&mut state.sessions.conversations).insert(
            session.clone(),
            Arc::new(Thread {
                id: Some(session.clone()),
                cwd: Some("/workspace".into()),
                turns: Some(vec![Arc::new(Turn {
                    id: "old".into(),
                    status: crate::models::TurnStatus::Completed,
                    ..Default::default()
                })]),
                capabilities: Some(crate::session::Capabilities {
                    steer: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut state.input.drafts).insert(
            session.clone().into(),
            Arc::new(Draft {
                text: "next prompt".into(),
                ..Default::default()
            }),
        );
        (state, session)
    }
    fn arm(state: &Snapshot, session: &SessionRef, scope: &str, enabled: bool) -> Snapshot {
        reduce(
            state,
            "",
            Event::Intent(Intent::SetFreshContext {
                storage_scope: scope.into(),
                session: session.clone(),
                enabled,
            }),
        )
        .0
    }

    #[test]
    fn fresh_context_arming_and_cancellation_preserve_input_and_reject_foreign_hosts() {
        let (state, session) = ready();
        let key = DraftKey::from(&session);
        assert!(state.fresh_context_available(session.clone()));
        let unchanged = arm(&state, &session, "other-host", true);
        assert!(!unchanged.input.drafts[&key].fresh_context);
        let armed = arm(&state, &session, "host", true);
        assert!(armed.input.drafts[&key].fresh_context);
        assert_eq!(armed.input.drafts[&key].text, "next prompt");
        let cancelled = arm(&armed, &session, "host", false);
        assert_eq!(cancelled.input.drafts[&key], state.input.drafts[&key]);
        let saved = serde_json::to_vec(&armed).unwrap();
        let restored: Snapshot = serde_json::from_slice(&saved).unwrap();
        assert!(restored.input.drafts[&key].fresh_context);
        let changed = reduce_event(&armed, Event::StorageScope("other-host".into())).0;
        assert!(!changed.input.drafts.contains_key(&key));
        let returned = reduce_event(&changed, Event::StorageScope("host".into())).0;
        assert!(returned.input.drafts[&key].fresh_context);
    }

    #[test]
    fn fresh_context_waits_for_a_new_turn_and_is_restored_after_a_definite_send_failure() {
        let (mut state, session) = ready();
        let key = DraftKey::from(&session);
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        thread.status = crate::models::SessionStatus::Running;
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]).status =
            crate::models::TurnStatus::Running;
        let mut state = arm(&state, &session, "host", true);
        state.input.composer_preferences.follow_up = agent_protocol::operations::FollowUp::Steer;
        let (sent, effects) = reduce(
            &state,
            "",
            Event::Intent(Intent::Submit {
                thread_id: Some(session.clone()),
                client_user_message_id: "input".into(),
                alternate: false,
            }),
        );
        assert!(!effects.is_empty());
        assert!(
            sent.sessions.pending_submissions["input"]
                .draft
                .fresh_context
        );
        assert!(sent.sessions.pending_submissions["input"].queued);
        assert!(!sent.input.drafts[&key].fresh_context);
        assert!(sent.input.drafts[&key].text.is_empty());
        let failed = reduce_event(&sent, Event::SubmissionFailed("input".into())).0;
        assert!(failed.input.drafts[&key].fresh_context);
        assert_eq!(failed.input.drafts[&key].text, "next prompt");
    }

    #[test]
    fn fresh_context_is_unavailable_without_history_or_on_a_read_only_child() {
        let (ready, session) = ready();
        for (history, parent, direct, connected, expected) in [
            (true, false, None, true, true),
            (false, false, None, true, false),
            (true, true, None, true, false),
            (true, true, Some(true), true, true),
            (true, false, Some(false), true, false),
            (true, false, None, false, false),
        ] {
            let mut state = ready.clone();
            state.connection.connected = connected;
            let thread = Arc::make_mut(
                Arc::make_mut(&mut state.sessions.conversations)
                    .get_mut(&session)
                    .unwrap(),
            );
            if !history {
                thread.turns = Some(Vec::new());
            }
            thread.parent_id = parent.then(|| session.clone());
            thread.can_accept_direct_input = direct;
            assert_eq!(state.fresh_context_available(session.clone()), expected);
            let armed = arm(&state, &session, "host", true);
            assert_eq!(
                armed.input.drafts[&DraftKey::from(&session)].fresh_context,
                expected
            );
        }
    }
}
