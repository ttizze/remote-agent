use super::*;
use agent_protocol::orchestration as model;

impl Storage {
    pub(in crate::host_rpc) fn fork_origin(
        &self,
        target: &SessionRef,
    ) -> Result<Option<model::ProviderThreadFork>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let target = projections::metadata(&tx, &target.application_id())?;
            let origin = native_origin(&tx, &target)?;
            tx.commit().map_err(failure)?;
            Ok(origin)
        })
    }

    /// Resolve the canonical provenance recorded at admission, rather than
    /// interpreting the transfer again in the native I/O worker.
    pub(in crate::host_rpc) fn native_fork_source(
        &self,
        origin: &model::ProviderThreadFork,
        source_point: Option<&model::ContextSourcePoint>,
    ) -> Result<Option<(model::ProviderRef, model::ProviderRef)>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            if let Some(point) = source_point {
                let state = projections::metadata(&tx, &point.thread_id)?;
                let run = point
                    .run_id
                    .as_ref()
                    .and_then(|id| state.runs.get(id))
                    .ok_or("native fork source run is missing")?;
                if run.provider_thread_id.as_ref() != Some(&origin.provider_thread_id)
                    || !state
                        .provider_threads
                        .contains_key(&origin.provider_thread_id)
                {
                    return Err("native fork source provider execution is missing".into());
                }
                if state
                    .active_run()
                    .is_some_and(|candidate| candidate.ordinal > run.ordinal)
                {
                    return Ok(None);
                }
            }
            let thread =
                projections::read::<model::ProviderThread>(&tx, &origin.provider_thread_id)?
                    .ok_or("native fork source is missing")?;
            let turn = origin
                .provider_turn_id
                .as_deref()
                .ok_or("native fork boundary is missing")?;
            let turn = projections::read::<model::ProviderTurn>(&tx, turn)?
                .ok_or("native fork turn is missing")?;
            if turn.provider_thread_id != thread.id {
                return Err("fork turn belongs to another provider thread".into());
            }
            let source = thread
                .native_thread_ref
                .ok_or("native fork source reference is missing")?;
            let through = turn
                .native_turn_ref
                .ok_or("native fork turn reference is missing")?;
            tx.commit().map_err(failure)?;
            Ok(Some((source, through)))
        })
    }

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
            let transfer_id=model::derived_id("context-transfer",&[&target_id,"fork"]);
            let (thread,transfer)=model::fork_thread(&state.thread,run,state.context_source_point(run),&target_id,&transfer_id,now)?;
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

pub(super) fn native_origin(
    connection: &Connection,
    target: &model::State,
) -> Result<Option<model::ProviderThreadFork>, projections::ReadError> {
    let Some(fork) = target.unconsumed_fork() else {
        return Ok(None);
    };
    let source = projections::metadata(connection, &fork.source_thread_id)?;
    let run = fork
        .source_point
        .run_id
        .as_ref()
        .and_then(|id| source.runs.get(id))
        .ok_or("fork source run is missing")?;
    // CommandPolicy.decideForkExecution: these sources require a
    // bounded portable handoff, never an unbounded fresh native turn.
    if !matches!(
        run.status,
        model::RunStatus::Completed | model::RunStatus::Waiting
    ) || source
        .active_run()
        .is_some_and(|active| active.ordinal > run.ordinal)
    {
        return Ok(None);
    }
    let thread = run
        .provider_thread_id
        .as_ref()
        .and_then(|id| source.provider_threads.get(id))
        .ok_or("fork source provider thread is missing")?;
    let reference = thread.native_thread_ref.as_ref().filter(|reference| {
        reference.strength == model::NativeRefStrength::Strong && reference.native_id.is_some()
    });
    if reference.is_none() || reference != fork.source_point.provider_thread_ref.as_ref() {
        return Ok(None);
    }
    let Some(turn) = source.provider_turns.values().find(|turn| {
        turn.provider_thread_id == thread.id
            && turn.native_turn_ref.is_some()
            && turn.native_turn_ref == fork.source_point.provider_turn_ref
    }) else {
        return Ok(None);
    };
    let origin = model::ProviderThreadFork {
        provider_thread_id: thread.id.clone(),
        provider_turn_id: Some(turn.id.clone()),
        checkpoint_id: None,
    };
    Ok(Some(origin))
}
