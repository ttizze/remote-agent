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
    ) -> Result<Option<(model::ProviderRef, Option<model::ProviderRef>)>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let source_state = source_point
                .map(|point| projections::metadata(&tx, &point.thread_id))
                .transpose()?;
            if let (Some(point), Some(state)) = (source_point, source_state.as_ref()) {
                if let Some(run_id) = &point.run_id {
                    let run = state
                        .runs
                        .get(run_id)
                        .ok_or("native fork source run is missing")?;
                    if run.provider_thread_id.as_ref() != Some(&origin.provider_thread_id) {
                        return Err("native fork source provider execution is missing".into());
                    }
                    if state
                        .active_run()
                        .is_some_and(|candidate| candidate.ordinal > run.ordinal)
                    {
                        return Ok(None);
                    }
                } else if state.active_run().is_some()
                    || state.has_uncertain_input()
                    || state.provider_turns.values().any(|turn| {
                        turn.provider_thread_id == origin.provider_thread_id
                            && turn.status == model::ProviderTurnStatus::Running
                    })
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
            if source_point.is_some_and(|point| {
                point.provider_thread_ref.as_ref() != turn.native_thread_ref.as_ref()
                    || point.provider_turn_ref.as_ref() != turn.native_turn_ref.as_ref()
            }) {
                return Ok(None);
            }
            let source = turn
                .native_thread_ref
                .clone()
                .ok_or("native fork source reference is missing")?;
            let source_state = match source_state {
                None if thread.driver == "claudeAgent" => Some(projections::metadata(
                    &tx,
                    thread
                        .app_thread_id
                        .as_deref()
                        .ok_or("native fork source conversation is missing")?,
                )?),
                state => state,
            };
            if source_state
                .as_ref()
                .is_some_and(|state| state.is_rolled_back_attempt(turn.run_attempt_id.as_deref()))
            {
                return Err(
                    "native fork boundary no longer belongs to the current provider conversation"
                        .into(),
                );
            }
            let mut through = turn.native_turn_ref.clone();
            if thread.driver == "claudeAgent" {
                if !through
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_deref())
                    .is_some_and(model::is_claude_assistant_cursor)
                {
                    let state = source_state
                        .as_ref()
                        .ok_or("native fork source conversation is missing")?;
                    if state.provider_turns.values().any(|candidate| {
                        candidate.provider_thread_id == thread.id
                            && candidate.ordinal > turn.ordinal
                            && candidate.status.is_terminal()
                            && !state.is_rolled_back_attempt(candidate.run_attempt_id.as_deref())
                    }) {
                        return Err(
                            "Claude assistant cursor is unavailable for this earlier fork boundary"
                                .into(),
                        );
                    }
                    // A rewind leaves the discarded raw transcript intact.
                    // Latest missing-cursor forks copy its pin, or the full head.
                    through = thread.native_conversation_head_ref.clone();
                }
            } else if through.is_none() {
                return Err("native fork turn reference is missing".into());
            }
            tx.commit().map_err(failure)?;
            Ok(Some((source, through)))
        })
    }

    pub(in crate::host_rpc) fn fork_thread(
        &self,
        source: &SessionRef,
        target: &SessionRef,
        through: &agent_protocol::conversation::records::ForkPoint,
        now: &str,
    ) -> Result<(), String> {
        if target.provider.is_some() || Uuid::parse_str(&target.id).is_err() {
            return Err("fork target must be a new UUID".into());
        }
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let source_id=source.application_id();
            let target_id=target.application_id();
            if let Some(existing)=projections::read::<model::Thread>(&transaction,&target_id)? {
                let target=projections::metadata(&transaction,&target_id)?;
                let same=existing.deleted_at.is_none() && target.context_transfers.values().any(|transfer|
                    transfer.kind==model::TransferType::Fork && transfer.source_thread_id==source_id && transfer.target_thread_id==target_id
                        && match through {
                            agent_protocol::conversation::records::ForkPoint::Run {run_id}=>transfer.source_point.run_id.as_ref()==Some(run_id),
                            agent_protocol::conversation::records::ForkPoint::ProviderTurn {provider_turn_id}=>matches!(&existing.forked_from,Some(model::ThreadForkSource::ProviderThread {provider_turn_id:Some(id),..}) if id==provider_turn_id),
                            agent_protocol::conversation::records::ForkPoint::TurnItem {turn_item_id}=>transfer.source_point.run_id.is_none() && transfer.source_point.turn_item_id.as_ref()==Some(turn_item_id),
                        });
                return if same {Ok(())} else {Err("fork target already exists".into())};
            }
            let state=projections::metadata(&transaction,&source_id)?;
            let transfer_id=model::derived_id("context-transfer",&[&target_id,"fork"]);
            let (thread,transfer)=plan(&transaction,&state,through,&target_id,&transfer_id,now)?;
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

fn plan(
    connection: &Connection,
    state: &model::State,
    through: &agent_protocol::conversation::records::ForkPoint,
    target_id: &str,
    transfer_id: &str,
    now: &str,
) -> Result<(model::Thread, model::ContextTransfer), String> {
    use agent_protocol::conversation::records::ForkPoint;
    if let ForkPoint::Run { run_id } = through {
        let run = state.runs.get(run_id).ok_or("fork run does not exist")?;
        return model::fork_thread(
            &state.thread,
            run,
            state.context_source_point(run),
            target_id,
            transfer_id,
            now,
        );
    }
    let turn = if let ForkPoint::ProviderTurn { provider_turn_id } = through {
        Some(
            state
                .provider_turns
                .get(provider_turn_id)
                .ok_or("native fork turn does not exist")?,
        )
    } else {
        None
    };
    let provider = if let Some(turn) = turn {
        state.provider_threads.get(&turn.provider_thread_id)
    } else {
        state.selected_native_thread()
    }
    .ok_or("native fork provider thread is missing")?;
    let cutoff=match through {
        ForkPoint::ProviderTurn {provider_turn_id}=>connection.query_row(
            "SELECT turn_item_id FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND provider_turn_id=?2 AND run_id IS NULL ORDER BY ordinal DESC LIMIT 1",
            params![state.thread.id,provider_turn_id],|row|row.get::<_,String>(0)).optional().map_err(failure)?.ok_or("native fork turn has no history")?,
        ForkPoint::TurnItem {turn_item_id}=>connection.query_row(
            "SELECT turn_item_id FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND turn_item_id=?2 AND run_id IS NULL AND provider_turn_id IS NULL AND type IN ('user_message','assistant_message','reasoning') AND status='completed'",
            params![state.thread.id,turn_item_id],|row|row.get::<_,String>(0)).optional().map_err(failure)?.ok_or("imported fork boundary does not exist")?,
        ForkPoint::Run {..}=>unreachable!(),
    };
    let reference = turn.and_then(|turn| turn.native_thread_ref.clone());
    let point = model::ContextSourcePoint {
        thread_id: state.thread.id.clone(),
        run_id: None,
        checkpoint_id: None,
        turn_item_id: Some(cutoff),
        provider_thread_ref: reference,
        provider_turn_ref: turn.and_then(|turn| turn.native_turn_ref.clone()),
    };
    model::fork_native_thread(
        &state.thread,
        provider,
        turn,
        point,
        target_id,
        transfer_id,
        now,
    )
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
        .map(|id| source.runs.get(id).ok_or("fork source run is missing"))
        .transpose()?;
    // CommandPolicy.decideForkExecution: terminal unsuccessful work and a
    // later active execution use a bounded portable handoff.
    if let Some(run) = run {
        if !matches!(
            run.status,
            model::RunStatus::Completed | model::RunStatus::Waiting
        ) || source
            .active_run()
            .is_some_and(|active| active.ordinal > run.ordinal)
        {
            return Ok(None);
        }
    } else if source.active_run().is_some()
        || source.has_uncertain_input()
        || source
            .provider_turns
            .values()
            .any(|turn| turn.status == model::ProviderTurnStatus::Running)
    {
        return Ok(None);
    }
    let provider_id = run
        .and_then(|run| run.provider_thread_id.as_ref())
        .or(match &target.thread.forked_from {
            Some(model::ThreadForkSource::ProviderThread {
                provider_thread_id, ..
            }) => Some(provider_thread_id),
            _ => None,
        })
        .ok_or("fork source provider thread is missing")?;
    let thread = source
        .provider_threads
        .get(provider_id)
        .ok_or("fork source provider thread is missing")?;
    let reference = fork
        .source_point
        .provider_thread_ref
        .as_ref()
        .filter(|reference| {
            reference.strength == model::NativeRefStrength::Strong && reference.native_id.is_some()
        });
    if reference.is_none() {
        return Ok(None);
    }
    let Some(turn) = source.provider_turns.values().find(|turn| {
        turn.provider_thread_id == thread.id
            && turn.native_thread_ref.as_ref() == reference
            && turn.native_turn_ref.is_some()
            && turn.native_turn_ref == fork.source_point.provider_turn_ref
            && (run.is_some() || turn.status == model::ProviderTurnStatus::Completed)
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
