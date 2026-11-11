//! Binding and transcript changes commit together; failed writes alter neither.
use super::*;

impl Conversations {
    pub fn replace_history(
        &self,
        row: Conversation,
        after: ThreadResponse,
    ) -> Result<u64, Failure> {
        let mut state = self.state.lock().unwrap();
        let rows = state
            .as_ref()
            .cloned()
            .map_err(|error| Failure::new("conversation_storage_failed", error))?;
        let id = row.id.clone();
        let next = replacing(rows, row);
        validate(&next).map_err(|error| Failure::new("invalid_conversation", error))?;
        let version = self
            .store
            .replace_with_bindings(&id, after, &next)
            .map_err(|error| Failure::new("conversation_storage_failed", error))?;
        *state = Ok(next);
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> (Conversation, Conversation, ThreadResponse, ThreadResponse) {
        let thread = |id: &str| Thread {
            id: Some(SessionRef::new(ProviderKind::Codex, id.into()).unwrap()),
            cwd: Some("/workspace".into()),
            ..Default::default()
        };
        let original = Conversation::new(&thread("original")).unwrap();
        let mut replacement = Conversation::new(&thread("replacement")).unwrap();
        replacement.id = original.id.clone();
        let mut before = ThreadResponse {
            thread: thread("original"),
            ..Default::default()
        };
        before.thread.turns = Some(
            ["first", "removed"]
                .into_iter()
                .map(|id| {
                    Arc::new(Turn {
                        id: id.into(),
                        status: TurnStatus::Completed,
                        ..Default::default()
                    })
                })
                .collect(),
        );
        let mut after = before.clone();
        after.thread.turns.as_mut().unwrap().truncate(1);
        (original, replacement, before, after)
    }

    #[test]
    fn failed_rewind_rolls_back_history_version_and_bindings_then_can_retry() {
        use crate::host_rpc::store::ConversationStore;
        for managed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("history.sqlite");
            let transcripts = Arc::new(ConversationStore::open(&path));
            let store = Conversations::new(transcripts.clone());
            let (original, replacement, before, after) = values();
            if managed {
                store.save(original.clone()).unwrap();
            }
            transcripts
                .import(&original.id, None, before.clone())
                .unwrap();
            let version = transcripts.version(&original.id).unwrap();
            transcripts.reject_state_writes(true);
            assert!(
                store
                    .replace_history(replacement.clone(), after.clone())
                    .is_err()
            );
            assert_eq!(transcripts.version(&original.id).unwrap(), version);
            assert_eq!(
                transcripts
                    .history(&original.id)
                    .unwrap()
                    .unwrap()
                    .thread
                    .turns
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                store.get(&original.id).unwrap().map(|row| row.active),
                managed.then(|| original.active.clone())
            );
            let reopened = Conversations::new(Arc::new(ConversationStore::open(&path)));
            assert_eq!(
                reopened.get(&original.id).unwrap().map(|row| row.active),
                managed.then(|| original.active.clone())
            );
            transcripts.reject_state_writes(false);
            let committed = store.replace_history(replacement.clone(), after).unwrap();
            // An import captured before rewind cannot resurrect removed turns.
            transcripts.import(&original.id, version, before).unwrap();
            assert_eq!(transcripts.version(&original.id).unwrap(), Some(committed));
            drop(store);
            drop(transcripts);
            let transcripts = Arc::new(ConversationStore::open(&path));
            let reopened = Conversations::new(transcripts.clone());
            assert_eq!(
                reopened.get(&original.id).unwrap().unwrap().active,
                replacement.active
            );
            assert_eq!(
                transcripts
                    .history(&original.id)
                    .unwrap()
                    .unwrap()
                    .thread
                    .turns
                    .unwrap()
                    .len(),
                1
            );
        }
    }
    #[test]
    fn failed_fork_leaves_neither_an_imported_history_nor_a_binding() {
        use crate::host_rpc::store::ConversationStore;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite");
        let database = Arc::new(ConversationStore::open(&path));
        let store = Conversations::new(database.clone());
        let (original, _, before, _) = values();
        database.reject_state_writes(true);
        assert!(
            store
                .replace_history(original.clone(), before.clone())
                .is_err()
        );
        assert!(database.imported().unwrap().is_empty());
        assert!(store.get(&original.id).unwrap().is_none());
        database.reject_state_writes(false);
        store.replace_history(original.clone(), before).unwrap();
        assert_eq!(database.imported().unwrap().len(), 1);
        assert!(store.get(&original.id).unwrap().is_some());
    }
}
