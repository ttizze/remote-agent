//! A native transcript is run-less history, as in T3's AgentSessionImporter.
//! The marker and items commit together; a concurrent first input wins over a
//! late native read, which must never import that input a second time.
use super::*;
use agent_protocol::orchestration as model;

impl Storage {
    pub(in crate::host_rpc) fn import_native_history(
        &self,
        thread: &model::Thread,
        provider_thread: &model::ProviderThread,
        items: Vec<model::TurnItem>,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let source = native_bindings::header_source(thread, provider_thread)?;
            // EventSink.unlessNativeThreadOwned: a native alias cannot import
            // private history into a second application conversation.
            if native_bindings::owner(&tx, &source)?
                .is_some_and(|owner| owner.application_id() != thread.id)
            {
                return Ok(());
            }
            let existing = projections::read::<model::Thread>(&tx, &thread.id)?;
            let created = existing.is_none();
            let state = match existing {
                Some(_) => projections::load(&tx, &thread.id, &[])?,
                None => model::State::new(thread.clone()),
            };
            if state.thread.history_origin == Some(model::ThreadHistoryOrigin::V1Import)
                || !state.runs.is_empty()
            {
                return Ok(());
            }
            let mut changes = Vec::new();
            if created {
                changes.push(model::Change::ThreadCreated(thread.clone()));
            }
            if !created
                && state.thread.active_provider_thread_id.as_deref() != Some(&provider_thread.id)
            {
                return Err("native import source changed".into());
            }
            if !state.provider_threads.contains_key(&provider_thread.id) {
                changes.push(model::Change::ProviderThread(provider_thread.clone()));
            }
            for item in items {
                if item.thread_id != thread.id || item.run_id.is_some() || item.node_id.is_some() {
                    return Err(
                        "native history must contain only this thread's run-less items".into(),
                    );
                }
                changes.push(model::Change::TurnItem(item));
            }
            changes.push(model::Change::Thread(model::Thread {
                history_origin: Some(model::ThreadHistoryOrigin::V1Import),
                workspace_launch: None,
                model_selection: thread.model_selection.clone(),
                created_at: thread.created_at.clone(),
                deleted_at: None,
                ..state.thread.clone()
            }));
            commands::append_decision(
                &tx,
                &state,
                model::Decision {
                    changes,
                    effects: vec![],
                },
                None,
                now,
            )?;
            tx.commit().map_err(failure)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{NOW, thread};
    use super::*;

    #[test]
    fn history_marker_and_bodies_commit_once_without_creating_runs_or_native_executions() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let mut thread = thread("imported");
        thread.active_provider_thread_id = Some("native".into());
        let native: model::ProviderThread = serde_json::from_value(serde_json::json!({
            "id":"native","driver":"codex","providerInstanceId":"codex",
            "providerSessionId":null,"appThreadId":"imported","ownerNodeId":null,
            "nativeThreadRef":{"driver":"codex","nativeId":"native","strength":"strong"},
            "nativeConversationHeadRef":null,"status":"idle","firstRunOrdinal":null,
            "lastRunOrdinal":null,"handoffIds":[],"forkedFrom":null,"createdAt":NOW,"updatedAt":NOW
        }))
        .unwrap();
        let item: model::TurnItem = serde_json::from_value(serde_json::json!({
            "id":"history","threadId":"imported","runId":null,"nodeId":null,
            "providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,
            "parentItemId":null,"ordinal":1,"status":"completed","title":null,
            "startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,"type":"assistant_message",
            "messageId":"message","text":"full native body","attachments":[],"streaming":false
        }))
        .unwrap();
        storage
            .import_native_history(&thread, &native, vec![item.clone()], NOW)
            .unwrap();
        let target = SessionRef::new(None, thread.id.clone()).unwrap();
        let first = storage.thread_stream(&target, None).unwrap();
        storage
            .import_native_history(&thread, &native, vec![item], NOW)
            .unwrap();
        assert_eq!(first, storage.thread_stream(&target, None).unwrap());
        let model::projection::ThreadStreamItem::Snapshot(snapshot) = &first[0] else {
            panic!("snapshot")
        };
        assert_eq!(
            snapshot.projection.thread.history_origin,
            Some(model::ThreadHistoryOrigin::V1Import)
        );
        assert!(snapshot.projection.runs.is_empty());
        assert!(snapshot.projection.attempts.is_empty());
        assert!(snapshot.projection.provider_turns.is_empty());
        assert_eq!(snapshot.projection.turn_items.len(), 1);
        assert!(
            matches!(&snapshot.projection.turn_items[0].body, model::TurnItemBody::AssistantMessage {text,..} if text=="full native body")
        );
    }
}
