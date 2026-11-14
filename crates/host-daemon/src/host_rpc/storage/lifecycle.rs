//! Process lifecycle facts update canonical execution and its durable work in
//! one writer transaction, independently of native resource cleanup.
use super::*;
use agent_protocol::orchestration as model;

impl Storage {
    pub(in crate::host_rpc) fn provider_stopped(
        &self,
        provider_instance_id: &str,
        error: &model::ProviderFailure,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let targets = {
                let mut query = tx.prepare("SELECT b.thread_id FROM orchestration_v2_projection_provider_session_bindings b JOIN orchestration_v2_projection_provider_sessions s ON s.provider_session_id=b.provider_session_id WHERE s.provider_instance_id=?1 UNION SELECT thread_id FROM orchestration_v2_projection_runs WHERE provider_instance_id=?1 AND status IN ('starting','running','waiting')").map_err(failure)?;
                query.query_map([provider_instance_id], |row| row.get::<_,String>(0)).map_err(failure)?.collect::<Result<Vec<_>,_>>().map_err(failure)?
            };
            for target in targets {
                let state = projections::load(&tx, &target, &[])?;
                let command = Uuid::new_v4().to_string();
                let decision = model::provider_stopped(&state, provider_instance_id, error, model::Context {
                    command_id: &command,
                    now,
                    next_run_ordinal: state.runs.values().map(|run| run.ordinal).max().unwrap_or(0).saturating_add(1),
                    can_steer: false,
                })?;
                commands::append_decision(&tx, &state, decision, None, now)?;
            }
            tx.commit().map_err(failure)
        })
    }
}
