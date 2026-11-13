//! Correlation belongs beside the native connection. A resumed transcript is
//! observed before a new write gets a scope; its frames cannot acquire that
//! write's execution identity from the Host's current conversation state.
use super::AgentChange;
use agent_protocol::{ids::TurnId, session::SessionChange};
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Scopes(Mutex<HashMap<String, Source>>);
#[derive(Default)]
struct Source {
    pending: Option<Uuid>,
    current: Option<Uuid>,
    turns: HashMap<TurnId, Option<Uuid>>,
    suspended: bool,
    owned: bool,
}
impl Scopes {
    pub(super) fn admit(&self, source: &str) {
        self.0
            .lock()
            .unwrap()
            .entry(source.into())
            .or_default()
            .owned = true;
    }
    pub(super) fn suspend(&self, source: &str) {
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.entry(source.into()).or_default();
        scope.suspended = true;
        scope.pending = None;
    }
    pub(super) fn history(&self, response: &agent_protocol::models::ThreadResponse) {
        let Some(source) = &response.thread.id else {
            return;
        };
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.entry(source.id.clone()).or_default();
        for turn in response.thread.turns.iter().flatten() {
            scope.turns.entry(turn.id.clone()).or_insert(None);
        }
    }
    pub(super) fn reserve(&self, source: &str, execution: Uuid) {
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.entry(source.into()).or_default();
        scope.suspended = false;
        scope.owned = true;
        scope.pending = Some(execution);
        scope.current = Some(execution);
    }
    pub(super) fn cancel(&self, source: &str, execution: Uuid) {
        let mut scopes = self.0.lock().unwrap();
        let Some(scope) = scopes.get_mut(source) else {
            return;
        };
        if scope.pending == Some(execution) {
            scope.pending = None;
        }
        let bound = scope.turns.values().any(|id| *id == Some(execution));
        if !bound && scope.current == Some(execution) {
            scope.current = None;
        }
    }
    pub(super) fn bind(&self, source: &str, turn: TurnId, execution: Uuid) {
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.entry(source.into()).or_default();
        scope.turns.insert(turn, Some(execution));
        if scope.pending == Some(execution) {
            scope.pending = None;
        }
    }
    /// Only call after the native subscription is closed and older frames have
    /// drained; a resumed source then installs its own history before reserve.
    pub(super) fn forget(&self, source: &str) {
        self.0.lock().unwrap().remove(source);
    }
    pub(super) fn observe(&self, change: &AgentChange) -> Option<Uuid> {
        let (source, turn, started) = match change {
            AgentChange::Session { session, change } => (
                &session.id,
                change.turn_id(),
                matches!(
                    change,
                    SessionChange::Turn {
                        completed: false,
                        ..
                    }
                ),
            ),
            AgentChange::Request {
                session, request, ..
            } => (
                &session.id,
                match &request.target {
                    agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => Some(turn_id),
                    _ => None,
                },
                false,
            ),
            _ => return None,
        };
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.get_mut(source)?;
        let Some(turn) = turn else {
            return scope.current;
        };
        if let Some(execution) = scope.turns.get(turn) {
            return *execution;
        }
        if !started || scope.suspended {
            return None;
        }
        let execution = scope
            .pending
            .take()
            .or_else(|| scope.owned.then(Uuid::new_v4))?;
        scope.current = Some(execution);
        scope.turns.insert(turn.clone(), Some(execution));
        Some(execution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        models::{Thread, ThreadResponse, Turn},
        session::{ProviderKind, SessionRef},
    };
    use std::sync::Arc;
    fn started(turn: &str) -> AgentChange {
        AgentChange::Session {
            session: SessionRef {
                provider: Some(ProviderKind::Codex),
                id: "native".into(),
            },
            change: SessionChange::Turn {
                turn: Turn {
                    id: turn.into(),
                    ..Default::default()
                },
                completed: false,
            },
        }
    }
    #[test]
    fn resume_history_never_inherits_the_next_write_scope() {
        let scopes = Scopes::default();
        scopes.suspend("native");
        assert_eq!(scopes.observe(&started("previous")), None);
        scopes.history(&ThreadResponse {
            thread: Thread {
                id: Some(SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: "native".into(),
                }),
                turns: Some(vec![Arc::new(Turn {
                    id: "previous".into(),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        });
        let execution = Uuid::new_v4();
        scopes.reserve("native", execution);
        assert_eq!(scopes.observe(&started("previous")), None);
        assert_eq!(scopes.observe(&started("next")), Some(execution));
    }
    #[test]
    fn late_callbacks_keep_the_execution_that_produced_them() {
        let scopes = Scopes::default();
        let first = Uuid::new_v4();
        scopes.reserve("native", first);
        assert_eq!(scopes.observe(&started("first")), Some(first));
        let second = Uuid::new_v4();
        scopes.suspend("native");
        scopes.reserve("native", second);
        assert_eq!(scopes.observe(&started("first")), Some(first));
        assert_eq!(scopes.observe(&started("second")), Some(second));
        assert_eq!(scopes.observe(&started("first")), Some(first));
    }
    #[test]
    fn a_rejected_write_cannot_capture_the_next_background_wake() {
        let scopes = Scopes::default();
        let failed = Uuid::new_v4();
        scopes.reserve("native", failed);
        scopes.cancel("native", failed);
        let wake = scopes.observe(&started("wake")).unwrap();
        assert_ne!(wake, failed);
        scopes.reserve("native", wake);
        scopes.cancel("native", wake);
        assert_eq!(
            scopes.observe(&started("wake")),
            Some(wake),
            "a rejected steer does not erase a bound turn"
        );
    }
    #[test]
    fn closed_sources_stop_accepting_frames_and_release_only_their_own_identity() {
        let scopes = Scopes::default();
        let execution = Uuid::new_v4();
        scopes.reserve("native", execution);
        assert_eq!(scopes.observe(&started("first")), Some(execution));
        scopes.admit("another-source");
        scopes.forget("native");
        assert_eq!(scopes.observe(&started("first")), None);
        let mut other = started("other-turn");
        if let AgentChange::Session { session, .. } = &mut other {
            session.id = "another-source".into();
        }
        assert!(scopes.observe(&other).is_some());
    }
    #[test]
    fn an_owned_background_wake_gets_a_fresh_scope_without_a_new_client_write() {
        let scopes = Scopes::default();
        let first = Uuid::new_v4();
        scopes.reserve("native", first);
        scopes.observe(&started("first"));
        let wake = scopes.observe(&started("wake")).unwrap();
        assert_ne!(wake, first);
        assert_eq!(scopes.observe(&started("wake")), Some(wake));
    }
}
