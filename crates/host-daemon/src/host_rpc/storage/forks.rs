use super::*;
use agent_protocol::orchestration as model;

impl Storage {
    pub(in crate::host_rpc) fn fork_thread(
        &self,
        source: &SessionRef,
        target: &SessionRef,
        through: &TurnId,
        now: &str,
    ) -> Result<(), String> {
        if target.provider.is_some() || Uuid::parse_str(&target.id).is_err() {
            return Err("fork target must be a new UUID".into());
        }
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let source_id=source.application_id();
            let target_id=target.application_id();
            let point=model::ThreadForkSource::Run {thread_id:source_id.clone(),run_id:through.to_string()};
            if let Some(existing)=projections::read::<model::Thread>(&transaction,&target_id)? {
                return if existing.forked_from.as_ref()==Some(&point) {Ok(())} else {Err("fork target already exists".into())};
            }
            let state=projections::metadata(&transaction,&source_id)?;
            let run=state.runs.get(through.as_str()).ok_or("fork run does not exist")?;
            let provider_thread=run.provider_thread_id.as_ref().and_then(|id|state.provider_threads.get(id));
            let provider_turn=state.provider_turns.values().filter(|turn|run.active_attempt_id.as_ref().is_some_and(|attempt|turn.run_attempt_id.as_ref()==Some(attempt))).max_by_key(|turn|turn.ordinal);
            let transfer_id=model::derived_id("context-transfer",&[&target_id,"fork"]);
            let (thread,transfer)=model::fork_thread(&state.thread,run,model::ContextSourcePoint {
                thread_id:source_id,run_id:Some(run.id.clone()),
                checkpoint_id:run.checkpoint_id.clone(),
                turn_item_id:None,provider_thread_ref:provider_thread.and_then(|thread|thread.native_thread_ref.clone()),
                provider_turn_ref:provider_turn.and_then(|turn|turn.native_turn_ref.clone()),
            },&target_id,&transfer_id,now)?;
            let target_state=model::State::new(thread.clone());
            commands::append_decision(&transaction,&target_state,model::Decision {
                changes:vec![model::Change::ThreadCreated(thread),model::Change::ContextTransfer(transfer)],effects:vec![],
            },Some(&target.id),now)?;
            let sequence=event_store::head(&transaction,Some(&target_id))?;
            transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,error,command_type) VALUES (?1,'thread',?2,?3,?4,'accepted',NULL,'thread.fork')",params![target.id,target_id,now,integer(sequence)?]).map_err(failure)?;
            transaction.commit().map_err(failure)
        })
    }
}
