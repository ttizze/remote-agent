//! Completion receipts share the conversation transaction and header.
use super::*;
use agent_protocol::ids::TurnId;

impl Storage {
    #[cfg(test)]
    pub(in crate::host_rpc) fn activity(
        &self,
    ) -> Result<agent_protocol::live_activity::TaskActivityState, String> {
        let shell = self.shell_snapshot()?;
        agent_protocol::orchestration::shell::activity(shell.threads.iter().map(AsRef::as_ref))
    }
    pub(in crate::host_rpc) fn mark_read(
        &self,
        id: &SessionRef,
        turn: &TurnId,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let thread_id = id.application_id();
            let mut thread = projections::read::<agent_protocol::orchestration::Thread>(
                &transaction,
                &thread_id,
            )?
            .ok_or("conversation does not exist")?;
            let snapshot = super::shell::thread(&transaction, thread.clone())?;
            if let Some((receipt, completed)) = snapshot
                .completion_receipt()
                .filter(|(receipt, _)| *receipt == turn.as_str())
            {
                let run = (snapshot.latest_run_id.as_deref() == Some(receipt))
                    .then(|| receipt.to_owned());
                let completed_at =
                    chrono::DateTime::parse_from_rfc3339(&completed).map_err(failure)?;
                let visited = thread
                    .last_visited_at
                    .as_deref()
                    .map(chrono::DateTime::parse_from_rfc3339)
                    .transpose()
                    .map_err(failure)?;
                if visited.is_none_or(|visited| visited < completed_at) {
                    thread.last_visited_at = Some(completed.to_owned());
                    thread.updated_at = projects::timestamp();
                    event_store::append(
                        &transaction,
                        &agent_protocol::orchestration::Event {
                            id: Uuid::new_v4().to_string(),
                            thread_id,
                            run_id: run,
                            node_id: None,
                            driver: None,
                            provider_instance_id: None,
                            raw_event_id: None,
                            occurred_at: thread.updated_at.clone(),
                            change: agent_protocol::orchestration::Change::Thread(thread),
                        },
                        None,
                        None,
                    )?;
                }
            }
            transaction.commit().map_err(failure)
        })
    }
}
