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
            Ok(events.events)
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
            transaction
                .execute_batch("SAVEPOINT orchestration_decision")
                .map_err(failure)?;
            let appended = append_decision(
                &transaction,
                &state,
                decision,
                Some(command.id),
                command.now,
            )?;
            if let Some(error) = appended.preparation_error {
                transaction
                    .execute_batch(
                        "ROLLBACK TO orchestration_decision; RELEASE orchestration_decision",
                    )
                    .map_err(failure)?;
                Some(error)
            } else {
                transaction
                    .execute_batch("RELEASE orchestration_decision")
                    .map_err(failure)?;
                None
            }
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

pub(super) struct Appended {
    pub events: Vec<model::StoredEvent>,
    pub preparation_error: Option<String>,
}

/// Settled outbox rows are still existing identities. Working State only
/// retains live rows, so use the persisted outbox for admission deduplication.
fn new_start_runs(
    connection: &Connection,
    effects: &[model::Effect],
) -> Result<Vec<String>, String> {
    let mut runs = std::collections::BTreeSet::new();
    for effect in effects
        .iter()
        .filter(|effect| effect.status == model::EffectStatus::Pending)
    {
        let model::EffectRequest::ProviderTurnStart { run_id } = &effect.request else {
            continue;
        };
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM orchestration_v2_effect_outbox WHERE effect_id=?1)",
                [&effect.id],
                |row| row.get(0),
            )
            .map_err(failure)?;
        if !exists {
            runs.insert(run_id.clone());
        }
    }
    Ok(runs.into_iter().collect())
}

pub(super) fn append_decision(
    transaction: &rusqlite::Transaction<'_>,
    state: &model::State,
    decision: model::Decision,
    command_id: Option<&str>,
    now: &str,
) -> Result<Appended, String> {
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
    let starts = new_start_runs(transaction, &decision.effects)?;
    let mut effects = decision.effects;
    let mut preparation_error = None;
    let (users,mut changes):(Vec<_>,Vec<_>)=decision.changes.into_iter().partition(|change|matches!(change,model::Change::TurnItem(item) if matches!(item.body,model::TurnItemBody::UserMessage {..}) && item.run_id.as_ref().is_some_and(|id|starts.contains(id))));
    let mut events = Vec::new();
    // Write source observations first. Plan against those same uncommitted
    // rows, then emit the handoff before the newly starting user's item.
    for phase in 0..2 {
        for change in changes.drain(..) {
            let index = events.len();
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
        if phase == 0 && !starts.is_empty() {
            let mut current = projections::load(transaction, &state.thread.id, &[])?;
            for change in &users {
                if let model::Change::TurnItem(item) = change {
                    current.turn_items.insert(item.id.clone(), item.clone());
                }
            }
            let mut failed_changes = Vec::new();
            for run_id in &starts {
                match super::handoffs::prepare_admission(
                    transaction,
                    &current,
                    std::slice::from_ref(run_id),
                    now,
                ) {
                    Ok(plan) => changes.extend(plan),
                    Err(projections::ReadError::Storage(error)) => return Err(failure(error)),
                    Err(projections::ReadError::Data(error)) => {
                        preparation_error.get_or_insert_with(|| error.clone());
                        let run = current
                            .runs
                            .get(run_id)
                            .ok_or("context target run is missing")?;
                        let attempt = run
                            .active_attempt_id
                            .as_deref()
                            .ok_or("context target attempt is missing")?;
                        let failure_id = model::derived_id("context-preparation", &[&seed, run_id]);
                        let context = model::Context {
                            command_id: &failure_id,
                            now,
                            next_run_ordinal: current
                                .runs
                                .values()
                                .map(|run| run.ordinal)
                                .max()
                                .unwrap_or(0)
                                .checked_add(1)
                                .ok_or("run ordinal exhausted")?,
                            can_steer: false,
                        };
                        let failed = model::fail_start(
                            &current,
                            attempt,
                            model::ProviderFailure {
                                class: "provider_error".into(),
                                message: error.clone(),
                                code: Some("context_prepare_failed".into()),
                                retryable: Some(false),
                                reset_at: None,
                            },
                            context,
                        )?;
                        if failed.changes.is_empty() {
                            return Err("context preparation target is no longer starting".into());
                        }
                        current = current.apply(&failed);
                        failed_changes.extend(failed.changes);
                        effects.extend(failed.effects);
                        for effect in effects.iter_mut().filter(|effect| {
                            effect.status == model::EffectStatus::Pending
                                && matches!(&effect.request, model::EffectRequest::ProviderTurnStart {run_id:id} if id==run_id)
                        }) {
                            // The new outbox identity and its rejection commit
                            // together; settlement must refer to an owned row.
                            event_store::save_effect(transaction, effect)?;
                            effect.status = model::EffectStatus::Failed;
                            effect.last_error = Some(error.clone());
                            effect.updated_at = now.into();
                            effect.completed_at = Some(now.into());
                            effect.lease_owner = None;
                            effect.lease_expires_at = None;
                        }
                    }
                }
            }
            changes.extend(users.iter().cloned());
            changes.extend(failed_changes);
        }
    }
    for effect in effects {
        if effect.thread_id != state.thread.id {
            return Err("effect and command have different owners".into());
        }
        event_store::save_effect(transaction, &effect)?;
    }
    Ok(Appended {
        events,
        preparation_error,
    })
}

#[cfg(test)]
mod tests;
