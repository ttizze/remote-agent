//! Canonical context ownership and durable delivery, matching T3's handoff
//! service. Only turn items own text; handoff metadata names the source items.
use super::*;
use agent_protocol::orchestration::{self as model, handoffs as history};

impl Storage {
    pub(in crate::host_rpc) fn prepare_context(
        &self,
        effect: &model::Effect,
        worker: &str,
        can_fork: bool,
        recovery: &str,
        now: &str,
    ) -> Result<Option<model::State>, String> {
        self.with(|connection| {
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let state = projections::load(&tx, &effect.thread_id, &[])?;
            if !owns(&state, effect, worker) {
                return Ok(None);
            }
            let model::EffectRequest::ProviderTurnStart { run_id } = &effect.request else {
                return Err("context requires a start effect".into());
            };
            let run = state
                .runs
                .get(run_id)
                .ok_or("context target run is missing")?;
            let thread = run
                .provider_thread_id
                .as_ref()
                .and_then(|id| state.provider_threads.get(id))
                .ok_or("context target provider thread is missing")?;
            let fresh = thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_ref())
                .is_some_and(|native| {
                    state.context_handoffs.values().any(|handoff| {
                        handoff.to_provider_thread_id == thread.id
                            && handoff.delivery.as_ref().is_some_and(|delivery| {
                                delivery.native_thread_id == *native
                                    && delivery.status == model::HandoffDeliveryStatus::Pending
                            })
                    })
                });
            let mut changes = Vec::new();
            let pending = state
                .unconsumed_fork()
                .filter(|transfer| transfer.target_run_id.as_ref() == Some(&run.id));
            let origin = if can_fork
                && !fresh
                && pending.is_some_and(|transfer| {
                    transfer.source_provider_instance_id.as_deref()
                        == Some(run.provider_instance_id.as_str())
                }) {
                super::forks::native_origin(&tx, &state)?
            } else {
                None
            };
            if let Some(origin) = origin {
                changes.push(model::Change::ProviderThread(model::ProviderThread {
                    forked_from: Some(origin),
                    ..thread.clone()
                }));
            } else if fresh
                || !state.context_handoffs.values().any(|handoff| {
                    handoff.target_run_id == run.id && handoff.to_provider_thread_id == thread.id
                })
            {
                changes.extend(prepare(
                    &tx, &state, run, thread, pending, fresh, recovery, now,
                )?);
            }
            let decision = model::Decision {
                changes,
                effects: Vec::new(),
            };
            commands::append_decision(&tx, &state, decision.clone(), None, now)?;
            let result = state.apply(&decision);
            tx.commit().map_err(failure)?;
            Ok(Some(result))
        })
    }

    pub(in crate::host_rpc) fn pending_context(
        &self,
        effect: &model::Effect,
        worker: &str,
        native: &SessionRef,
        budget: usize,
        recovery: &str,
    ) -> Result<Option<(Vec<model::ContextHandoff>, history::SelectedHistory)>, String> {
        self.with(|connection| {
            let tx=connection.transaction().map_err(failure)?;
            let state=projections::metadata(&tx,&effect.thread_id)?;
            // Re-read the lease under this snapshot too; cancellation must not
            // allocate or inject history after another worker owns the write.
            let effects=event_store::effects(&tx,&effect.thread_id)?;
            if !effects.get(&effect.id).is_some_and(|saved|saved.status==model::EffectStatus::Running && saved.lease_owner.as_deref()==Some(worker)) {return Ok(None);}
            let model::EffectRequest::ProviderTurnStart {run_id}=&effect.request else {return Err("context requires a start effect".into());};
            let run=&state.runs[run_id];
            let thread=run.provider_thread_id.as_ref().and_then(|id|state.provider_threads.get(id)).ok_or("context provider thread is missing")?;
            let mut pending:Vec<_>=state.context_handoffs.values().filter(|handoff|handoff.to_provider_thread_id==thread.id && handoff.status==model::HandoffStatus::Ready && handoff.delivery.as_ref().is_none_or(|delivery|delivery.native_thread_id!=native.id || delivery.status==model::HandoffDeliveryStatus::Pending)).cloned().collect();
            pending.sort_by(|a,b|a.created_at.cmp(&b.created_at).then_with(||a.id.cmp(&b.id)));
            if pending.is_empty() {return Ok(None);}
            if pending.iter().any(|handoff|handoff.delivery.as_ref().is_some_and(|delivery|delivery.native_thread_id==native.id && delivery.status==model::HandoffDeliveryStatus::Pending)) {return Err("historical context delivery is uncertain; replace the native thread before retrying".into());}
            let mut seen:std::collections::BTreeSet<_>=state.context_handoffs.values().filter_map(|handoff|handoff.delivery.as_ref()).filter(|delivery|delivery.native_thread_id==native.id && delivery.status!=model::HandoffDeliveryStatus::Pending).flat_map(|delivery|delivery.item_ids.iter().cloned()).collect();
            let mut messages=Vec::new();
            let mut omitted_ids=Vec::new();
            let mut omitted=0;
            let mut coverage=Vec::new();
            for handoff in &pending {
                let stored=handoff.history.as_ref().ok_or("handoff history is missing")?;
                coverage.push(format!("Context handoff ({}):\n{}",serde_json::to_value(handoff.strategy).map_err(failure)?.as_str().ok_or("invalid strategy")?,stored.coverage));
                omitted+=stored.omitted_items as usize;
                omitted_ids.extend(stored.omitted_item_ids.iter().cloned());
                for reference in &stored.messages {
                    if !seen.insert(reference.item_id.clone()) {continue;}
                    let item=projections::read::<model::TurnItem>(&tx,&reference.item_id)?.filter(|item|item.thread_id==reference.thread_id).ok_or("handoff source body is missing")?;
                    let mut message=history::historical_message(&item,None).ok_or("handoff source kind changed")?;
                    message.message=reference.clone();
                    messages.push(message);
                }
            }
            let mut coverage=coverage.join("\n");
            if history::history_cost(&[],&coverage)>4_000.min(budget/2) {coverage=format!("Context handoff. {} records; detailed coverage references omitted. Recover an omitted item only when the request needs it.\n{recovery}",pending.len());}
            let selected=history::select_history(&messages,&coverage,omitted,&omitted_ids,budget)?;
            tx.commit().map_err(failure)?;
            Ok(Some((pending,selected)))
        })
    }

    pub(in crate::host_rpc) fn context_delivery(
        &self,
        effect: &model::Effect,
        worker: &str,
        native: &SessionRef,
        handoffs: &[model::ContextHandoff],
        selected: &history::SelectedHistory,
        status: Option<model::HandoffDeliveryStatus>,
        now: &str,
    ) -> Result<bool, String> {
        let events = self.observe_effect(effect, worker, now, |state| {
            let changes = handoffs
                .iter()
                .filter_map(|original| {
                    state.context_handoffs.get(&original.id).map(|current| {
                        let delivery = status
                            .map(|status| model::HandoffDelivery {
                                native_thread_id: native.id.clone(),
                                status,
                                item_ids: selected
                                    .messages
                                    .iter()
                                    .filter(|message| {
                                        current.history.as_ref().is_some_and(|history| {
                                            history.messages.iter().any(|reference| {
                                                reference.item_id == message.message.item_id
                                            })
                                        })
                                    })
                                    .map(|message| message.message.item_id.clone())
                                    .collect(),
                                omitted_item_ids: current.history.as_ref().map_or_else(
                                    Vec::new,
                                    |history| {
                                        history
                                            .omitted_item_ids
                                            .iter()
                                            .cloned()
                                            .chain(
                                                history
                                                    .messages
                                                    .iter()
                                                    .filter(|reference| {
                                                        selected
                                                            .omitted_item_ids
                                                            .contains(&reference.item_id)
                                                    })
                                                    .map(|reference| reference.item_id.clone()),
                                            )
                                            .collect()
                                    },
                                ),
                            })
                            .or_else(|| original.delivery.clone());
                        model::Change::ContextHandoff(model::ContextHandoff {
                            delivery,
                            ..current.clone()
                        })
                    })
                })
                .collect();
            Ok(model::Decision {
                changes,
                effects: Vec::new(),
            })
        })?;
        Ok(!events.is_empty())
    }
}

fn owns(state: &model::State, effect: &model::Effect, worker: &str) -> bool {
    state.effects.get(&effect.id).is_some_and(|saved| {
        saved.status == model::EffectStatus::Running && saved.lease_owner.as_deref() == Some(worker)
    })
}

fn prepare(
    connection: &Connection,
    state: &model::State,
    run: &model::Run,
    thread: &model::ProviderThread,
    fork: Option<&model::ContextTransfer>,
    fresh: bool,
    recovery: &str,
    now: &str,
) -> Result<Vec<model::Change>, String> {
    let source = if let Some(fork) = fork {
        projections::metadata(connection, &fork.source_thread_id)?
    } else {
        state.clone()
    };
    let latest = if let Some(fork) = fork {
        fork.source_point
            .run_id
            .as_ref()
            .and_then(|id| source.runs.get(id))
    } else {
        source
            .runs
            .values()
            .filter(|source| {
                source.ordinal < run.ordinal
                    && matches!(
                        source.status,
                        model::RunStatus::Completed
                            | model::RunStatus::Waiting
                            | model::RunStatus::Failed
                            | model::RunStatus::Interrupted
                            | model::RunStatus::Cancelled
                    )
            })
            .max_by_key(|source| source.ordinal)
    };
    let switched =
        latest.is_some_and(|source| source.provider_instance_id != run.provider_instance_id);
    let imported = thread.native_thread_ref.is_none()
        && state.thread.history_origin == Some(model::ThreadHistoryOrigin::V1Import);
    if fork.is_none() && !fresh && !switched && !imported {
        return Ok(Vec::new());
    }
    let through = latest.map_or(0, |source| source.ordinal);
    let after = if fork.is_some() || fresh {
        0
    } else {
        thread.last_run_ordinal.unwrap_or(0)
    };
    let items = timeline::handoff_items(connection, &source.thread.id, after, through, after == 0)?;
    let messages: Vec<_> = items
        .iter()
        .filter_map(|(item, status)| history::historical_message(item, *status))
        .collect();
    if messages.is_empty() && fork.is_none() && !fresh {
        return Ok(Vec::new());
    }
    let ordinals: Vec<_> = items
        .iter()
        .filter_map(|(item, _)| {
            item.run_id
                .as_ref()
                .and_then(|id| source.runs.get(id))
                .map(|run| run.ordinal)
        })
        .collect();
    let covered = model::CoveredRunOrdinals {
        from: ordinals.iter().min().copied().unwrap_or(1),
        to: ordinals.iter().max().copied().unwrap_or(1),
    };
    let coverage = format!(
        "Provider context handoff. Thread: {}. Covered app runs: {}-{}.\nSource item range: {} through {}.\nRecover omitted history only when the request needs it.\n{recovery}",
        source.thread.id,
        covered.from,
        covered.to,
        items.first().map_or("none", |(item, _)| item.id.as_str()),
        items.last().map_or("none", |(item, _)| item.id.as_str())
    );
    let selected =
        history::select_history(&messages, &coverage, 0, &[], history::DEFAULT_TOKEN_CAP)?;
    let id = model::derived_id(
        "context-handoff",
        &[&run.id, if fresh { "recovery" } else { "provider" }],
    );
    let transfer_id = fork.map_or_else(
        || model::derived_id("context-transfer", &[&run.id, "provider"]),
        |fork| fork.id.clone(),
    );
    let from: Vec<_> = items
        .iter()
        .filter_map(|(item, _)| item.provider_thread_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let strategy = if after == 0 {
        model::HandoffStrategy::FullThreadSummary
    } else {
        model::HandoffStrategy::DeltaSinceTargetLastSeen
    };
    let handoff = model::ContextHandoff {
        id: id.clone(),
        transfer_id: Some(transfer_id.clone()),
        thread_id: state.thread.id.clone(),
        target_run_id: run.id.clone(),
        from_provider_thread_ids: from.clone(),
        to_provider_thread_id: thread.id.clone(),
        covered_run_ordinals: covered,
        strategy,
        status: model::HandoffStatus::Ready,
        summary_message_id: None,
        history: Some(model::HandoffHistory {
            messages: selected
                .messages
                .iter()
                .map(|message| message.message.clone())
                .collect(),
            coverage,
            omitted_items: selected.omitted_items as u64,
            omitted_item_ids: selected.omitted_item_ids,
        }),
        delivery: None,
        detail_in_turn_item: true,
        created_by_provider_instance_id: None,
        created_at: now.into(),
        updated_at: now.into(),
    };
    let resolution = if after == 0 {
        model::ContextTransferResolution::PortableContext {
            context_handoff_id: id.clone(),
        }
    } else {
        model::ContextTransferResolution::DeltaContext {
            context_handoff_id: id.clone(),
        }
    };
    let transfer = if let Some(fork) = fork {
        model::ContextTransfer {
            target_run_id: Some(run.id.clone()),
            target_provider_instance_id: Some(run.provider_instance_id.clone()),
            status: model::TransferStatus::ResolvedPortable,
            resolution: Some(resolution),
            error: None,
            updated_at: now.into(),
            ..fork.clone()
        }
    } else {
        model::ContextTransfer {
            id: transfer_id,
            kind: if fresh {
                model::TransferType::ProviderResumeFallback
            } else {
                model::TransferType::ProviderHandoff
            },
            source_thread_id: source.thread.id.clone(),
            target_thread_id: state.thread.id.clone(),
            source_point: model::ContextSourcePoint {
                thread_id: source.thread.id.clone(),
                run_id: latest.map(|run| run.id.clone()),
                checkpoint_id: latest.and_then(|run| run.checkpoint_id.clone()),
                turn_item_id: None,
                provider_thread_ref: latest
                    .and_then(|run| run.provider_thread_id.as_ref())
                    .and_then(|id| source.provider_threads.get(id))
                    .and_then(|thread| thread.native_thread_ref.clone()),
                provider_turn_ref: None,
            },
            base_point: None,
            source_provider_instance_id: latest.map(|run| run.provider_instance_id.clone()),
            target_provider_instance_id: Some(run.provider_instance_id.clone()),
            target_run_id: Some(run.id.clone()),
            status: model::TransferStatus::Consumed,
            resolution: Some(resolution),
            created_by: model::Actor::User,
            error: None,
            created_at: now.into(),
            updated_at: now.into(),
            consumed_at: Some(now.into()),
        }
    };
    let mut handoff_ids = thread.handoff_ids.clone();
    if !handoff_ids.contains(&id) {
        handoff_ids.push(id.clone());
    }
    let item = model::TurnItem {
        id: model::derived_id("handoff", &[&id]),
        thread_id: state.thread.id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run
            .active_attempt_id
            .as_ref()
            .and_then(|id| state.run_attempts.get(id))
            .map(|attempt| attempt.root_node_id.clone()),
        provider_thread_id: Some(thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: model::TurnItemStatus::Completed,
        title: None,
        started_at: Some(now.into()),
        completed_at: Some(now.into()),
        updated_at: now.into(),
        body: model::TurnItemBody::Handoff {
            context_handoff_id: id,
            from_provider_thread_ids: from,
            to_provider_thread_id: thread.id.clone(),
            from_provider_instance_ids: latest
                .map(|run| vec![run.provider_instance_id.clone()])
                .unwrap_or_default(),
            to_provider_instance_id: run.provider_instance_id.clone(),
            strategy,
            summary: Some(history::render_history(
                &selected.messages,
                &selected.context,
            )),
        },
    };
    Ok(vec![
        model::Change::ProviderThread(model::ProviderThread {
            native_thread_ref: if fresh {
                None
            } else {
                thread.native_thread_ref.clone()
            },
            forked_from: None,
            handoff_ids,
            updated_at: now.into(),
            ..thread.clone()
        }),
        model::Change::ContextHandoff(handoff),
        if fork.is_some() {
            model::Change::ContextTransfer(transfer)
        } else {
            model::Change::ContextTransferCreated(transfer)
        },
        model::Change::TurnItem(item),
    ])
}
