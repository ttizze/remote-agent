//! Process lifecycle facts update canonical execution and its durable work in
//! one writer transaction, independently of native resource cleanup.
use super::*;
use agent_protocol::orchestration as model;
use std::collections::HashMap;

impl Storage {
    pub(in crate::host_rpc) fn restart_continuation_state(
        &self,
        thread_id: &str,
        source_run_id: &str,
    ) -> Result<model::State, String> {
        self.with(|connection| {
            let tx=connection.transaction().map_err(failure)?;
            let items={
                let mut query=tx.prepare("SELECT i.turn_item_id FROM orchestration_v2_projection_turn_items i JOIN orchestration_v2_projection_runs r ON r.thread_id=i.thread_id AND r.run_id=i.run_id WHERE i.thread_id=?1 AND json_extract(r.payload_json,'$.restartContinuationOfRunId')=?2 AND i.type='error'").map_err(failure)?;
                query.query_map(params![thread_id,source_run_id],|row|row.get::<_,String>(0)).map_err(failure)?.collect::<Result<Vec<_>,_>>().map_err(failure)?
            };
            let mut state=projections::load(&tx,thread_id,&items)?;
            {
                let mut query=tx.prepare(&format!("SELECT {} FROM orchestration_v2_effect_outbox WHERE thread_id=?1 AND effect_type='provider-runtime.continue' AND json_extract(payload_json,'$.sourceRunId')=?2",event_store::EFFECT_COLUMNS)).map_err(failure)?;
                let mut rows=query.query(params![thread_id,source_run_id]).map_err(failure)?;
                while let Some(row)=rows.next().map_err(failure)? {
                    let effect=event_store::effect(row)?;state.effects.insert(effect.id.clone(),effect);
                }
            }
            tx.commit().map_err(failure)?;Ok(state)
        })
    }

    pub(in crate::host_rpc) fn runtime_restarted(
        &self,
        continue_after_restart: bool,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let states = {
                let mut query = tx.prepare("SELECT thread_id FROM orchestration_v2_projection_threads WHERE deleted_at IS NULL ORDER BY thread_id").map_err(failure)?;
                let ids = query.query_map([], |row| row.get::<_,String>(0)).map_err(failure)?.collect::<Result<Vec<_>,_>>().map_err(failure)?;
                ids.into_iter().map(|id| projections::load(&tx, &id, &[])).collect::<Result<Vec<_>,_>>()?
            };
            // Provider sessions are shared. Read every source's liveness before
            // publishing any stopped session, as T3's sessionsLiveAtStart does.
            for state in states {
                let command = Uuid::new_v4().to_string();
                let decision = model::runtime_restarted(&state, continue_after_restart, model::Context {
                    command_id: &command, now, next_run_ordinal: state.next_run_ordinal().ok_or("run ordinal exhausted")?, can_steer: false,
                })?;
                commands::append_decision(&tx, &state, decision, None, now)?;
            }
            tx.commit().map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn continue_runtime(
        &self,
        effect_id: &str,
        worker: &str,
        thread_id: &str,
        source_run_id: &str,
        text: &str,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let source_message: String = connection.query_row("SELECT json_extract(payload_json,'$.userMessageId') FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND run_id=?2", params![thread_id, source_run_id], |row| row.get(0)).map_err(failure)?;
            let items = [model::user_item_id(thread_id, &source_message)];
            let command = format!("command:restart-continuation:{source_run_id}");
            let receipt = commands::execute(connection, commands::Command {
                id: &command, thread_id, kind: "message.dispatch", now, item_ids: &items,
            }, None, |state| {
                if !state.effects.get(effect_id).is_some_and(|effect| effect.status == model::EffectStatus::Running && effect.lease_owner.as_deref() == Some(worker)) {return Ok(model::Decision::default());}
                model::continue_restarted(state, source_run_id, text, model::Context {
                command_id: &command, now, next_run_ordinal: state.next_run_ordinal().ok_or("run ordinal exhausted")?, can_steer: false,
            })})?;
            receipt.receipt.error.map_or(Ok(()), Err)
        })
    }

    pub(in crate::host_rpc) fn provider_stopped(
        &self,
        provider_instance_id: &str,
        error: &model::ProviderFailure,
        tails: &HashMap<String, HashMap<String, agent_protocol::sync::Output>>,
        now: &str,
    ) -> Result<Vec<model::StoredEvent>, String> {
        self.with(|connection| {
            let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let targets = {
                let mut query = tx.prepare("SELECT b.thread_id FROM orchestration_v2_projection_provider_session_bindings b JOIN orchestration_v2_projection_provider_sessions s ON s.provider_session_id=b.provider_session_id WHERE s.provider_instance_id=?1 UNION SELECT thread_id FROM orchestration_v2_projection_runs WHERE provider_instance_id=?1 AND status IN ('starting','running','waiting') UNION SELECT p.thread_id FROM orchestration_v2_projection_provider_threads p WHERE p.provider_instance_id=?1 AND p.status IN ('idle','active','unknown') AND EXISTS(SELECT 1 FROM orchestration_v2_projection_runs r WHERE r.provider_thread_id=p.provider_thread_id)").map_err(failure)?;
                query.query_map([provider_instance_id], |row| row.get::<_,String>(0)).map_err(failure)?.collect::<Result<Vec<_>,_>>().map_err(failure)?
            };
            let mut events=Vec::new();
            for target in targets {
                let state = projections::load(&tx, &target, &[])?;
                let command = Uuid::new_v4().to_string();
                let decision = model::provider_stopped(&state, provider_instance_id, error, model::Context {
                    command_id: &command,
                    now,
                    next_run_ordinal: state.next_run_ordinal().ok_or("run ordinal exhausted")?,
                    can_steer: false,
                })?.with_command_outputs(tails.get(&target));
                events.extend(commands::append_decision(&tx, &state, decision, None, now)?.events);
            }
            tx.commit().map_err(failure)?;
            Ok(events)
        })
    }
}
