use super::*;
use agent_protocol::orchestration as model;

impl Storage {
    pub(in crate::host_rpc) fn thread_receipt(
        &self,
        id: &str,
        thread_id: &str,
        kind: &str,
    ) -> Result<Option<Receipt>, String> {
        self.with(|connection| receipt(connection, id, thread_id, kind))
    }
    pub(in crate::host_rpc) fn observe_thread(
        &self,
        thread_id: &str,
        item_ids: &[String],
        now: &str,
        decide: impl FnOnce(&model::State) -> Result<model::Decision, String>,
    ) -> Result<Vec<model::StoredEvent>, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let state = projections::load(&transaction, thread_id, item_ids)?;
            let decision = decide(&state)?;
            let events = append_decision(&transaction, &state, decision, None, now)?;
            transaction.commit().map_err(failure)?;
            Ok(events)
        })
    }

    pub(in crate::host_rpc) fn thread_command(
        &self,
        command: Command<'_>,
        create: Option<&model::Thread>,
        decide: impl FnOnce(&model::State) -> Result<model::Decision, String>,
    ) -> Result<agent_protocol::conversation::records::Acknowledged, String> {
        self.with(|connection| execute(connection, command, create, decide))
    }
}

pub(in crate::host_rpc) struct Command<'a> {
    pub id: &'a str,
    pub thread_id: &'a str,
    pub kind: &'a str,
    pub now: &'a str,
    pub item_ids: &'a [String],
}

pub(super) type Receipt = agent_protocol::conversation::records::Acknowledged;

pub(super) fn receipt(
    connection: &Connection,
    id: &str,
    thread_id: &str,
    command_type: &str,
) -> Result<Option<Receipt>, String> {
    let value = connection
        .query_row(
            "SELECT aggregate_kind,aggregate_id,command_type,result_sequence,accepted_at,error
        FROM orchestration_command_receipts WHERE command_id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(failure)?;
    value
        .map(|(aggregate, thread, kind, sequence, accepted_at, error)| {
            if aggregate != "thread" || thread != thread_id || kind != command_type {
                return Err("command identity is already used for another operation".into());
            }
            Ok(Receipt {
                sequence: sequence.try_into().map_err(failure)?,
                accepted_at,
                error,
            })
        })
        .transpose()
}

/// The writer takes the current projection under the same lock and transaction
/// as its receipts and effects. Repeated commands return their original result
/// before invoking the decision; provider I/O starts only after commit.
pub(super) fn execute(
    connection: &mut Connection,
    command: Command<'_>,
    create: Option<&model::Thread>,
    decide: impl FnOnce(&model::State) -> Result<model::Decision, String>,
) -> Result<Receipt, String> {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(failure)?;
    if let Some(receipt) = receipt(&transaction, command.id, command.thread_id, command.kind)? {
        return Ok(receipt);
    }
    let existing = projections::read::<model::Thread>(&transaction, command.thread_id)?;
    let created = existing.is_none();
    let state = match existing {
        Some(_) => projections::load(&transaction, command.thread_id, command.item_ids)?,
        None => model::State::new(
            create
                .filter(|thread| thread.id == command.thread_id)
                .ok_or("conversation does not exist")?
                .clone(),
        ),
    };
    let outcome = decide(&state);
    let error = match outcome {
        Ok(mut decision) => {
            if created {
                decision
                    .changes
                    .insert(0, model::Change::ThreadCreated(state.thread.clone()));
            }
            append_decision(
                &transaction,
                &state,
                decision,
                Some(command.id),
                command.now,
            )?;
            None
        }
        Err(error) => Some(error),
    };
    let sequence:i64=transaction.query_row("SELECT coalesce((SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'),0)",[],|row|row.get(0)).map_err(failure)?;
    transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,error,command_type)
        VALUES (?1,'thread',?2,?3,?4,?5,?6,?7)",params![command.id,command.thread_id,command.now,sequence,
            if error.is_some() {"rejected"} else {"accepted"},error,command.kind]).map_err(failure)?;
    transaction.commit().map_err(failure)?;
    Ok(Receipt {
        sequence: sequence.try_into().map_err(failure)?,
        accepted_at: command.now.into(),
        error,
    })
}

pub(super) fn append_decision(
    transaction: &rusqlite::Transaction<'_>,
    state: &model::State,
    decision: model::Decision,
    command_id: Option<&str>,
    now: &str,
) -> Result<Vec<model::StoredEvent>, String> {
    let mut ordinals: std::collections::BTreeMap<_, _> = state
        .runs
        .values()
        .map(|run| (run.id.clone(), run.ordinal))
        .collect();
    for change in &decision.changes {
        if let model::Change::RunCreated(run) | model::Change::Run(run) = change {
            ordinals.insert(run.id.clone(), run.ordinal);
        }
    }
    let seed = command_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut events = Vec::new();
    for (index, change) in decision.changes.into_iter().enumerate() {
        let event = model::Event {
            id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("event:{seed}:{index}").as_bytes(),
            )
            .to_string(),
            thread_id: state.thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: now.into(),
            change,
        };
        let ordinal = if let model::Change::TurnItem(item) = &event.change {
            item.run_id
                .as_ref()
                .map(|id| ordinals.get(id).copied().ok_or("item run is missing"))
                .transpose()?
        } else {
            None
        };
        events.push(event_store::append(
            transaction,
            &event,
            command_id,
            ordinal,
        )?);
    }
    for effect in decision.effects {
        if effect.thread_id != state.thread.id {
            return Err("effect and command have different owners".into());
        }
        event_store::save_effect(transaction, &effect)?;
    }
    Ok(events)
}

#[cfg(test)]
mod tests;
