//! Whether a started thread may move to another provider instance. Every
//! served driver hands a conversation off to another, so only a thread whose
//! state is unknown or whose turn has no native session yet is held.
use crate::view::thread_summary::ThreadSummary;
use agent_domain::State;

/// What decides whether a loaded thread can hand off its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HandoffFacts {
    /// A native session exists for the thread's current instance.
    pub native_session: bool,
    /// A run is preparing, starting, running or waiting.
    pub active_run: bool,
    pub imported: bool,
    pub has_runs: bool,
}
impl HandoffFacts {
    pub fn of(state: &State) -> Option<Self> {
        let thread = state.thread.as_ref()?;
        Some(Self {
            native_session: state
                .native_sessions
                .contains_key(&thread.selection.instance),
            active_run: state.active_run().is_some(),
            imported: thread.imported,
            has_runs: !state.runs.is_empty(),
        })
    }
}

pub fn thread_supports_provider_handoff(facts: Option<HandoffFacts>) -> bool {
    let Some(facts) = facts else {
        return false;
    };
    if facts.native_session {
        return true;
    }
    !facts.active_run && (facts.imported || !facts.has_runs)
}

fn thread_has_started(thread: &ThreadSummary) -> bool {
    thread.latest_run.is_some()
        || thread.latest_user_message_at.is_some()
        || thread.runtime.is_some()
}

/// The picker may offer other providers: a thread that never ran a turn is
/// bound to nothing, and a loaded thread follows its handoff support.
pub fn thread_allows_provider_switch(
    facts: Option<HandoffFacts>,
    shell: Option<&ThreadSummary>,
) -> bool {
    match facts {
        Some(_) => thread_supports_provider_handoff(facts),
        None => !shell.is_some_and(thread_has_started),
    }
}

/// Why a started thread cannot take a model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelChangeBlock {
    pub title: String,
    pub description: String,
}

/// A started session keeps its instance unless the thread can hand off;
/// every served instance changes models within a session.
pub fn started_thread_model_change_block(
    has_started_session: bool,
    supports_handoff: bool,
    current_instance: &str,
    current_model: &str,
    next_instance: &str,
    next_model: &str,
) -> Option<ModelChangeBlock> {
    if !has_started_session || (current_instance == next_instance && current_model == next_model) {
        return None;
    }
    (current_instance != next_instance && !supports_handoff).then(|| ModelChangeBlock {
        title: "Start a new chat to switch providers".into(),
        description: "This thread does not support switching providers after it has started."
            .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::{
        RuntimeStatus,
        fixtures::{run, runtime, summary},
    };

    fn started() -> ThreadSummary {
        ThreadSummary {
            latest_run: Some(run("run-1", RuntimeStatus::Completed)),
            latest_user_message_at: Some(0),
            runtime: Some(runtime(RuntimeStatus::Idle)),
            ..summary("thread")
        }
    }

    #[test]
    fn offers_every_provider_when_the_session_can_hand_the_conversation_off() {
        let facts = HandoffFacts {
            native_session: true,
            active_run: true,
            has_runs: true,
            ..Default::default()
        };
        assert!(thread_allows_provider_switch(Some(facts), Some(&started())));
    }

    #[test]
    fn leaves_a_thread_that_never_ran_a_turn_unbound() {
        assert!(thread_allows_provider_switch(
            None,
            Some(&summary("thread"))
        ));
        assert!(thread_allows_provider_switch(None, None));
    }

    #[test]
    fn allows_an_imported_thread_to_hand_off_before_opening_a_provider_session() {
        let facts = HandoffFacts {
            imported: true,
            ..Default::default()
        };
        assert!(thread_allows_provider_switch(Some(facts), Some(&started())));
    }

    #[test]
    fn keeps_a_preparing_turn_bound_before_its_provider_session_appears() {
        let facts = HandoffFacts {
            imported: true,
            active_run: true,
            has_runs: true,
            ..Default::default()
        };
        assert!(!thread_allows_provider_switch(
            Some(facts),
            Some(&started())
        ));
    }

    #[test]
    fn keeps_a_started_thread_bound_until_its_projection_resolves_a_session() {
        assert!(!thread_allows_provider_switch(None, Some(&started())));
        let facts = HandoffFacts {
            has_runs: true,
            ..Default::default()
        };
        assert!(!thread_allows_provider_switch(
            Some(facts),
            Some(&started())
        ));
    }

    #[test]
    fn an_empty_loaded_state_reads_as_unknown() {
        assert_eq!(HandoffFacts::of(&State::default()), None);
    }

    #[test]
    fn allows_model_changes_before_a_provider_session_has_started() {
        assert_eq!(
            started_thread_model_change_block(false, false, "codex", "a", "claude", "b"),
            None
        );
    }

    #[test]
    fn allows_unchanged_model_selections() {
        assert_eq!(
            started_thread_model_change_block(true, false, "codex", "a", "codex", "a"),
            None
        );
    }

    #[test]
    fn blocks_a_provider_switch_only_without_handoff() {
        assert_eq!(
            started_thread_model_change_block(true, false, "codex", "a", "codex", "b"),
            None
        );
        assert_eq!(
            started_thread_model_change_block(true, true, "codex", "a", "claude", "b"),
            None
        );
        assert_eq!(
            started_thread_model_change_block(true, false, "codex", "a", "claude", "b")
                .map(|block| block.title),
            Some("Start a new chat to switch providers".into())
        );
    }
}
