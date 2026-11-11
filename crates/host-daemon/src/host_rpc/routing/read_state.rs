//! Completion receipts belong to the Host, including while clients are offline.
use super::*;
use crate::host_rpc::store::ConversationStore;
use agent_protocol::ids::TurnId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::host_rpc) struct Completion {
    session: SessionRef,
    turn_id: TurnId,
    unread: bool,
}

pub(super) struct ReadState {
    store: Option<Arc<ConversationStore>>,
    completions: Result<Vec<Completion>, String>,
}
impl Default for ReadState {
    fn default() -> Self {
        Self {
            store: None,
            completions: Ok(Vec::new()),
        }
    }
}
impl ReadState {
    pub(super) fn load(store: Arc<ConversationStore>) -> Self {
        let completions = store.load_state("reads");
        Self {
            store: Some(store),
            completions,
        }
    }
    pub(super) fn check(&self) -> Result<(), Failure> {
        self.completions
            .as_ref()
            .map(|_| ())
            .map_err(|error| Failure::new("read_state_storage_failed", error))
    }
    pub(super) fn unread(&self) -> Vec<(SessionRef, TurnId)> {
        self.completions
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|completion| completion.unread)
            .map(|completion| (completion.session.clone(), completion.turn_id.clone()))
            .collect()
    }
    pub(super) fn forget(&mut self, session: &SessionRef) {
        let Ok(completions) = &self.completions else {
            return;
        };
        let next: Vec<_> = completions
            .iter()
            .filter(|completion| &completion.session != session)
            .cloned()
            .collect();
        if next.len() != completions.len() {
            self.update(next);
        }
    }
    fn save(&self, completions: &[Completion]) -> Result<(), Failure> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .save_state("reads", &completions)
            .map_err(|error| Failure::new("read_state_storage_failed", error))
    }
    pub(super) fn record(
        &mut self,
        session: &SessionRef,
        active: bool,
        completed: Option<&TurnId>,
    ) -> bool {
        let Ok(completions) = &self.completions else {
            return false;
        };
        let previous = completions
            .iter()
            .find(|completion| &completion.session == session);
        if !completed.is_some_and(|turn| previous.is_none_or(|old| &old.turn_id != turn))
            && !previous.is_some_and(|old| active && old.unread)
        {
            return false;
        }
        let completion = Completion {
            session: session.clone(),
            turn_id: completed
                .or_else(|| previous.map(|old| &old.turn_id))
                .unwrap()
                .clone(),
            unread: !active,
        };
        let mut next = completions.clone();
        if let Some(previous) = next.iter_mut().find(|old| &old.session == session) {
            *previous = completion;
        } else {
            next.push(completion);
        }
        self.update(next);
        true
    }
    fn update(&mut self, next: Vec<Completion>) {
        // Live facts survive temporary disk failures; read receipts only clear
        // a badge after their write succeeds.
        if let Err(error) = self.save(&next) {
            tracing::warn!(target: "agent", operation = "host.read_state.record", message = %error);
        }
        self.completions = Ok(next);
    }
    pub(super) fn mark_read(
        &mut self,
        session: &SessionRef,
        turn_id: &TurnId,
    ) -> Result<bool, Failure> {
        self.check()?;
        let mut next = self.completions.as_ref().unwrap().clone();
        let Some(completion) = next.iter_mut().find(|completion| {
            &completion.session == session && &completion.turn_id == turn_id && completion.unread
        }) else {
            return Ok(false);
        };
        completion.unread = false;
        self.save(&next)?;
        self.completions = Ok(next);
        Ok(true)
    }
}

impl SessionRouter {
    pub(in crate::host_rpc) fn check_read_state(&self) -> Result<(), Failure> {
        lock_state(&self.activity).reads.check()
    }
    pub(in crate::host_rpc) fn mark_session_read(
        &self,
        params: &agent_protocol::live_activity::MarkSessionRead,
    ) -> Result<(), Failure> {
        let mut state = lock_state(&self.activity);
        if !state.reads.mark_read(&params.session, &params.turn_id)? {
            return Ok(());
        }
        state.task_revision += 1;
        let activity =
            activity::task_activity_state(state.task_revision, &state.tasks, state.reads.unread());
        let failed = activity::publish_task_activity(
            state.apns.as_deref(),
            &lock_state(&self.state).sessions,
            &activity,
        );
        drop(state);
        self.close_failed(failed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_io_does_not_block_connection_lifecycle() {
        let router = SessionRouter::new();
        // Receipt I/O holds this lock. Connection registration must remain
        // independent for its entire lifetime, including while SQLite waits.
        let activity = lock_state(&router.activity);
        let other = router.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let session = other.open_session();
            drop(session);
            sent.send(()).unwrap();
        });
        let result = received.recv_timeout(std::time::Duration::from_secs(2));
        drop(activity);
        thread.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn only_a_fresh_running_list_clears_a_saved_completion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let router = SessionRouter::new().with_store(Arc::new(
            crate::host_rpc::store::ConversationStore::open(&path),
        ));
        let id = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        router.task_activity(
            vec![Thread {
                id: Some(id.clone()),
                ..Default::default()
            }],
            0,
        );
        let complete = |turn: &str| {
            router.session_change(
                &id,
                SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: turn.into(),
                        status: agent_protocol::execution::TurnStatus::Completed,
                        ..Default::default()
                    },
                    completed: true,
                },
            )
        };
        complete("first");
        let revision = router.task_activity_revision();
        complete("second");
        let running = Thread {
            id: Some(id.clone()),
            status: agent_protocol::models::SessionStatus::Running,
            ..Default::default()
        };
        let late = router.task_activity(vec![running.clone()], revision);
        assert_eq!(late.unread, vec![(id.clone(), "second".into())]);
        let fresh = router.task_activity(vec![running], late.revision);
        assert!(fresh.unread.is_empty());
        assert!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread()
            .is_empty()
        );
    }

    #[test]
    fn receipts_survive_restart_and_do_not_hide_newer_or_other_provider_completions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let codex = SessionRef::new(ProviderKind::Codex, "same".into()).unwrap();
        let claude = SessionRef::new(ProviderKind::Claude, "same".into()).unwrap();
        let first = "first".into();
        let second = "second".into();
        let mut state = ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
            &path,
        )));
        assert!(state.record(&codex, false, Some(&first)));
        assert!(state.record(&claude, false, Some(&first)));
        assert!(state.mark_read(&codex, &first).unwrap());
        assert!(
            !state.record(&codex, false, Some(&first)),
            "duplicate completion must stay read"
        );
        assert_eq!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread(),
            vec![(claude.clone(), first.clone())]
        );
        assert!(state.record(&codex, false, Some(&second)));
        assert!(
            !state.mark_read(&codex, &first).unwrap(),
            "a delayed receipt cannot clear new work"
        );
        assert_eq!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread()
            .len(),
            2
        );
        assert!(state.mark_read(&claude, &first).unwrap());
        assert_eq!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread(),
            vec![(codex.clone(), second.clone())]
        );
        assert!(
            state.record(&codex, true, None),
            "starting new work clears the old badge"
        );
        assert!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread()
            .is_empty()
        );
    }

    #[test]
    fn subagent_completions_never_survive_as_standalone_unread_results() {
        use agent_protocol::{execution::TurnStatus, models::SessionStatus};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let router = SessionRouter::new().with_store(Arc::new(
            crate::host_rpc::store::ConversationStore::open(&path),
        ));
        let root = SessionRef::new(ProviderKind::Codex, "root".into()).unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let conversation = Thread {
            id: Some(root.clone()),
            status: SessionStatus::Idle,
            ..Default::default()
        };
        router.task_activity(vec![conversation.clone()], 0);
        for session in [&root, &child] {
            router.session_change(
                session,
                SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "completed".into(),
                        status: TurnStatus::Completed,
                        ..Default::default()
                    },
                    completed: true,
                },
            );
        }
        let expected = vec![(root, "completed".into())];
        assert_eq!(router.task_activity(Vec::new(), 0).unread, expected);
        assert_eq!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread(),
            expected
        );
        let restarted = SessionRouter::new().with_store(Arc::new(
            crate::host_rpc::store::ConversationStore::open(&path),
        ));
        let state = restarted.task_activity(vec![conversation], 0);
        assert_eq!(state.unread, expected);
        assert_eq!(state.display.current.total, 1);
        assert!(!state.display.ongoing);
    }

    #[test]
    fn a_failed_receipt_write_preserves_unread_and_can_be_retried() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("state");
        let path = parent.join("state.sqlite");
        let id = SessionRef::new(ProviderKind::Claude, "task".into()).unwrap();
        let turn = "completed".into();
        let mut state = ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
            &path,
        )));
        assert!(state.record(&id, false, Some(&turn)));
        state.store.as_ref().unwrap().reject_state_writes(true);
        assert!(state.mark_read(&id, &turn).is_err());
        assert_eq!(state.unread(), vec![(id.clone(), turn.clone())]);
        state.store.as_ref().unwrap().reject_state_writes(false);
        assert!(state.mark_read(&id, &turn).unwrap());
        assert!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread()
            .is_empty()
        );
    }

    #[test]
    fn rewinding_forgets_removed_completion_receipts_without_affecting_other_tasks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let mut state = ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
            &path,
        )));
        let a = SessionRef::new(ProviderKind::Codex, "a".into()).unwrap();
        let b = SessionRef::new(ProviderKind::Codex, "b".into()).unwrap();
        state.record(&a, false, Some(&"removed".into()));
        state.record(&b, false, Some(&"retained".into()));
        state.forget(&a);
        state.forget(&a);
        assert!(!state.mark_read(&a, &"removed".into()).unwrap());
        assert_eq!(
            ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
                &path
            )))
            .unread(),
            [(b, "retained".into())]
        );
    }

    #[test]
    fn corrupted_receipts_are_reported_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        std::fs::write(&path, b"damaged").unwrap();
        let mut state = ReadState::load(Arc::new(crate::host_rpc::store::ConversationStore::open(
            &path,
        )));
        let id = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        assert!(state.check().is_err());
        assert!(state.mark_read(&id, &"turn".into()).is_err());
        assert!(!state.record(&id, false, Some(&"turn".into())));
        assert_eq!(std::fs::read(path).unwrap(), b"damaged");
    }
}
