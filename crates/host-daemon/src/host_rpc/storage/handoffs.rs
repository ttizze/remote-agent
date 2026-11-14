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
            if !state.effects.get(&effect.id).is_some_and(|saved|saved.status==model::EffectStatus::Running && saved.lease_owner.as_deref()==Some(worker)) {
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
            let mut decision = model::Decision {changes,effects:Vec::new()};
            if !fresh && let Some(native)=thread.native_thread_ref.as_ref().and_then(|reference|reference.native_id.as_deref()) {
                let candidate=state.apply(&decision);
                if pending_handoffs(&candidate.context_handoffs,&thread.id,native).next().is_some() {
                let budget=context_budget(&tx,&candidate,run,thread,Some(native))?;
                if matches!(select_context(&tx,&candidate.context_handoffs,&thread.id,native,budget,recovery)?,ContextPlan::InsufficientBudget) {
                    let replacement=prepare(&tx,&candidate,run,&candidate.provider_threads[&thread.id],None,true,recovery,now)?;
                    let restored=candidate.apply(&model::Decision {changes:replacement.clone(),effects:Vec::new()});
                    let budget=context_budget(&tx,&restored,run,thread,None)?;
                    if matches!(select_context(&tx,&restored.context_handoffs,&thread.id,"",budget,recovery)?,ContextPlan::Deliver(..)) {
                        if !thread.pending_background_tasks.is_empty() || state.nodes.values().any(|node|node.provider_thread_id.as_ref()==Some(&thread.id) && node.run_id.as_ref()!=Some(&run.id) && matches!(node.status,model::NodeStatus::Pending|model::NodeStatus::Running|model::NodeStatus::Waiting)) {return Err("native context has pending background work; cannot replace its thread".into());}
                        decision.changes.extend(replacement);
                    }
                }
                }
            }
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
        recovery: &str,
    ) -> Result<Option<(Vec<model::ContextHandoff>, history::SelectedHistory)>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let state = projections::load(&tx, &effect.thread_id, &[])?;
            if !state.effects.get(&effect.id).is_some_and(|saved| {
                saved.status == model::EffectStatus::Running
                    && saved.lease_owner.as_deref() == Some(worker)
            }) {
                return Ok(None);
            }
            let model::EffectRequest::ProviderTurnStart { run_id } = &effect.request else {
                return Err("context requires a start effect".into());
            };
            let run = state.runs.get(run_id).ok_or("context run is missing")?;
            let thread = run
                .provider_thread_id
                .as_ref()
                .and_then(|id| state.provider_threads.get(id))
                .ok_or("context provider thread is missing")?;
            if pending_handoffs(&state.context_handoffs, &thread.id, &native.id)
                .next()
                .is_none()
            {
                return Ok(None);
            }
            let budget = context_budget(&tx, &state, run, thread, Some(&native.id))?;
            let context = match select_context(
                &tx,
                &state.context_handoffs,
                &thread.id,
                &native.id,
                budget,
                recovery,
            )? {
                ContextPlan::Empty => None,
                ContextPlan::Deliver(handoffs, selected) => Some((handoffs, selected)),
                ContextPlan::InsufficientBudget => {
                    return Err("historical context exceeds the available native budget".into());
                }
            };
            tx.commit().map_err(failure)?;
            Ok(context)
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
    let fork_source = fork
        .map(|fork| projections::metadata(connection, &fork.source_thread_id))
        .transpose()?;
    let source = fork_source.as_ref().unwrap_or(state);
    let latest = if let Some(fork) = fork {
        fork.source_point
            .run_id
            .as_ref()
            .and_then(|id| source.runs.get(id))
    } else {
        source
            .runs
            .values()
            .filter(|source| source.ordinal < run.ordinal && source.is_handoff_source())
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
        source
            .last_delivered_run(&thread.id)
            .map_or(0, |run| run.ordinal)
    };
    let items = timeline::handoff_items(connection, &source.thread.id, after, through, after == 0)?;
    let messages: Vec<_> = items
        .iter()
        .filter_map(|(item, status)| history::historical_message(item, *status))
        .collect();
    if messages.is_empty() && fork.is_none() && !fresh {
        return Ok(Vec::new());
    }
    let covered_runs: Vec<_> = source
        .runs
        .values()
        .filter(|candidate| {
            candidate.ordinal > after
                && candidate.ordinal <= through
                && candidate.is_handoff_source()
        })
        .collect();
    let ordinals: Vec<_> = if fork.is_some() {
        items
            .iter()
            .filter_map(|(item, _)| {
                item.run_id
                    .as_ref()
                    .and_then(|id| source.runs.get(id))
                    .map(|run| run.ordinal)
            })
            .collect()
    } else {
        covered_runs.iter().map(|run| run.ordinal).collect()
    };
    let covered = if fresh {
        model::CoveredRunOrdinals {
            from: 1,
            to: run.ordinal.saturating_sub(1).max(1),
        }
    } else {
        model::CoveredRunOrdinals {
            from: ordinals.iter().min().copied().unwrap_or(1),
            to: ordinals.iter().max().copied().unwrap_or(1),
        }
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
    let scope = if fresh {
        format!(
            "recovery:{}",
            thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                .ok_or("recovery source native identity is missing")?
        )
    } else {
        "provider".into()
    };
    let id = model::derived_id("context-handoff", &[&run.id, &scope]);
    let legacy_import = latest.is_none() && imported && fork.is_none() && !fresh;
    let transfer_id = (!legacy_import).then(|| {
        fork.map_or_else(
            || model::derived_id("context-transfer", &[&run.id, &scope]),
            |fork| fork.id.clone(),
        )
    });
    let mut source_runs: Vec<_> = if fork.is_some() {
        latest.into_iter().collect()
    } else if fresh {
        source
            .runs
            .values()
            .filter(|candidate| {
                candidate.ordinal < run.ordinal
                    && candidate.provider_thread_id.as_ref() == Some(&thread.id)
            })
            .max_by_key(|run| run.ordinal)
            .into_iter()
            .collect()
    } else {
        covered_runs
    };
    source_runs.sort_by_key(|run| run.ordinal);
    let from: Vec<_> = if fresh {
        vec![thread.id.clone()]
    } else {
        source_runs
            .iter()
            .filter_map(|run| run.provider_thread_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let from_models =
        history::distinct_model_selections(source_runs.iter().map(|run| &run.model_selection));
    let from_providers = source_runs
        .iter()
        .map(|run| run.provider_instance_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let strategy = if legacy_import {
        model::HandoffStrategy::ManualContext
    } else if after == 0 {
        model::HandoffStrategy::FullThreadSummary
    } else {
        model::HandoffStrategy::DeltaSinceTargetLastSeen
    };
    let handoff = model::ContextHandoff {
        id: id.clone(),
        transfer_id: transfer_id.clone(),
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
    let mut handoff_ids = thread.handoff_ids.clone();
    if !handoff_ids.contains(&id) {
        handoff_ids.push(id.clone());
    }
    let planned_id = id.clone();
    let item = model::TurnItem {
        id: model::derived_id(
            "turn-item",
            &["run", &run.id, "signal", &format!("context-handoff:{id}")],
        ),
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
        title: Some(
            if fresh {
                "Recovered context"
            } else if fork.is_some() {
                "Fork context"
            } else if legacy_import {
                "Imported context"
            } else {
                "Provider handoff"
            }
            .into(),
        ),
        started_at: Some(now.into()),
        completed_at: Some(now.into()),
        updated_at: now.into(),
        body: model::TurnItemBody::Handoff {
            context_handoff_id: id,
            from_provider_thread_ids: from,
            to_provider_thread_id: thread.id.clone(),
            from_provider_instance_ids: from_providers,
            to_provider_instance_id: run.provider_instance_id.clone(),
            from_model_selections: Some(from_models),
            to_model: Some(run.model_selection.model.clone()),
            strategy,
            summary: Some(history::render_history(
                &selected.messages,
                &selected.context,
            )),
        },
    };
    let mut changes = vec![
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
        model::Change::TurnItem(item),
    ];
    if !fresh && !(fork.is_some() && thread.forked_from.is_some()) {
        changes.push(model::Change::Run(model::Run {
            context_handoff_id: Some(planned_id),
            ..run.clone()
        }));
    }
    let Some(transfer_id) = transfer_id else {
        return Ok(changes);
    };
    let mut transfer = fork.cloned().unwrap_or(model::ContextTransfer {
        id: transfer_id,
        kind: model::TransferType::ProviderHandoff,
        source_thread_id: source.thread.id.clone(),
        target_thread_id: state.thread.id.clone(),
        source_point: model::ContextSourcePoint {
            thread_id: source.thread.id.clone(),
            run_id: None,
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        },
        base_point: None,
        source_provider_instance_id: Some(thread.provider_instance_id.clone()),
        target_provider_instance_id: None,
        target_run_id: None,
        status: model::TransferStatus::ResolvedPortable,
        resolution: None,
        created_by: model::Actor::System,
        error: None,
        created_at: now.into(),
        updated_at: now.into(),
        consumed_at: None,
    });
    transfer.target_provider_instance_id = Some(run.provider_instance_id.clone());
    transfer.target_run_id = Some(run.id.clone());
    transfer.status = model::TransferStatus::ResolvedPortable;
    transfer.resolution = Some(resolution);
    transfer.error = None;
    transfer.updated_at = now.into();
    if fork.is_none() && !fresh {
        transfer.source_point =
            source.context_source_point(latest.ok_or("provider handoff source run is missing")?);
        transfer.base_point = source
            .runs
            .values()
            .find(|run| run.ordinal == after)
            .map(|run| source.context_source_point(run));
        transfer.source_provider_instance_id = latest.map(|run| run.provider_instance_id.clone());
        transfer.status = model::TransferStatus::Consumed;
        transfer.created_by = model::Actor::User;
        transfer.consumed_at = Some(now.into());
    }
    changes.push(if fork.is_some() || fresh {
        model::Change::ContextTransfer(transfer)
    } else {
        model::Change::ContextTransferCreated(transfer)
    });
    Ok(changes)
}

// Absence of telemetry is not an empty native conversation. T3 estimates the
// text and native attachments already accepted by this exact native identity.
fn context_budget(
    connection: &Connection,
    state: &model::State,
    run: &model::Run,
    thread: &model::ProviderThread,
    native: Option<&str>,
) -> Result<usize, String> {
    let item=state.turn_items.values().find(|item|matches!(&item.body,model::TurnItemBody::UserMessage {message_id,..} if message_id==&run.user_message_id)).ok_or("context target input is missing")?;
    let model::TurnItemBody::UserMessage {
        text, attachments, ..
    } = &item.body
    else {
        unreachable!()
    };
    let usage = state
        .provider_turns
        .values()
        .filter(|turn| {
            turn.provider_thread_id == thread.id
                && turn
                    .run_attempt_id
                    .as_ref()
                    .and_then(|id| state.run_attempts.get(id))
                    .is_some_and(|attempt| {
                        native.is_some()
                            && attempt.native_thread_id.as_deref() == native
                            && attempt.provider_thread_id == turn.provider_thread_id
                            && attempt.root_node_id == turn.node_id
                    })
        })
        .filter_map(|turn| turn.token_usage.as_ref())
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at));
    let window = usage.map(|usage| {
        usage
            .max_tokens
            .filter(|max| *max > 0)
            .unwrap_or(128_000)
            .min(
                usage
                    .auto_compact_threshold
                    .filter(|max| *max > 0)
                    .unwrap_or(u64::MAX),
            )
    });
    let used = if let Some(usage) = usage {
        usage.used_tokens
    } else if let Some(native) = native {
        let accepted: std::collections::BTreeSet<_> = state
            .provider_turns
            .values()
            .filter_map(|turn| {
                turn.run_attempt_id
                    .as_ref()
                    .and_then(|id| state.run_attempts.get(id))
            })
            .filter(|attempt| {
                attempt.provider_thread_id == thread.id
                    && attempt.native_thread_id.as_deref() == Some(native)
            })
            .map(|attempt| attempt.run_id.as_str())
            .collect();
        let delivered: std::collections::BTreeSet<_> = state
            .context_handoffs
            .values()
            .filter(|handoff| handoff.to_provider_thread_id == thread.id)
            .filter_map(|handoff| handoff.delivery.as_ref())
            .filter(|delivery| {
                delivery.native_thread_id == native
                    && delivery.status != model::HandoffDeliveryStatus::Pending
            })
            .flat_map(|delivery| delivery.item_ids.iter().map(String::as_str))
            .collect();
        timeline::handoff_items(
            connection,
            &state.thread.id,
            0,
            run.ordinal.saturating_sub(1),
            true,
        )?
        .iter()
        .map(|(item, status)| {
            let native_input = item.provider_thread_id.as_ref() == Some(&thread.id)
                && item
                    .run_id
                    .as_deref()
                    .is_some_and(|id| accepted.contains(id));
            if !native_input && !delivered.contains(item.id.as_str()) {
                return 0;
            }
            let text = history::historical_message(item, *status)
                .map_or(0, |message| message.text.len().div_ceil(4) as u64);
            text + if native_input
                && let model::TurnItemBody::UserMessage { attachments, .. } = &item.body
            {
                history::attachment_token_allowance(attachments)
            } else {
                0
            }
        })
        .sum()
    } else {
        0
    };
    Ok(history::handoff_budget(text, attachments, used, window))
}

enum ContextPlan {
    Empty,
    Deliver(Vec<model::ContextHandoff>, history::SelectedHistory),
    InsufficientBudget,
}

fn pending_handoffs<'a>(
    handoffs: &'a std::collections::BTreeMap<String, model::ContextHandoff>,
    thread_id: &'a str,
    native_id: &'a str,
) -> impl Iterator<Item = &'a model::ContextHandoff> {
    handoffs.values().filter(move |handoff| {
        handoff.to_provider_thread_id == thread_id
            && handoff.status == model::HandoffStatus::Ready
            && handoff.delivery.as_ref().is_none_or(|delivery| {
                delivery.native_thread_id != native_id
                    || delivery.status == model::HandoffDeliveryStatus::Pending
            })
    })
}

fn select_context(
    connection: &Connection,
    handoffs: &std::collections::BTreeMap<String, model::ContextHandoff>,
    thread_id: &str,
    native_id: &str,
    budget: usize,
    recovery: &str,
) -> Result<ContextPlan, String> {
    let mut pending: Vec<_> = pending_handoffs(handoffs, thread_id, native_id)
        .cloned()
        .collect();
    pending.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if pending.is_empty() {
        return Ok(ContextPlan::Empty);
    }
    if pending.iter().any(|handoff| {
        handoff.delivery.as_ref().is_some_and(|delivery| {
            delivery.native_thread_id == native_id
                && delivery.status == model::HandoffDeliveryStatus::Pending
        })
    }) {
        return Err(
            "historical context delivery is uncertain; replace the native thread before retrying"
                .into(),
        );
    }
    let mut seen: std::collections::BTreeSet<_> = handoffs
        .values()
        .filter_map(|handoff| handoff.delivery.as_ref())
        .filter(|delivery| {
            delivery.native_thread_id == native_id
                && delivery.status != model::HandoffDeliveryStatus::Pending
        })
        .flat_map(|delivery| delivery.item_ids.iter().cloned())
        .collect();
    let mut messages = Vec::new();
    let mut omitted_ids = Vec::new();
    let mut omitted = 0;
    let mut coverage = Vec::new();
    for handoff in &pending {
        let stored = handoff
            .history
            .as_ref()
            .ok_or("handoff history is missing")?;
        coverage.push(format!(
            "Context handoff ({}):\n{}",
            serde_json::to_value(handoff.strategy)
                .map_err(failure)?
                .as_str()
                .ok_or("invalid strategy")?,
            stored.coverage
        ));
        omitted += stored.omitted_items as usize;
        omitted_ids.extend(stored.omitted_item_ids.iter().cloned());
        for reference in &stored.messages {
            if !seen.insert(reference.item_id.clone()) {
                continue;
            }
            let item = projections::read::<model::TurnItem>(connection, &reference.item_id)?
                .filter(|item| item.thread_id == reference.thread_id)
                .ok_or("handoff source body is missing")?;
            let mut message =
                history::historical_message(&item, None).ok_or("handoff source kind changed")?;
            message.message = reference.clone();
            messages.push(message);
        }
    }
    let mut coverage = coverage.join("\n");
    if history::history_cost(&[], &coverage) > 4_000.min(budget / 2) {
        coverage = format!(
            "Context handoff. {} records; detailed coverage references omitted. Recover an omitted item only when the request needs it.\n{recovery}",
            pending.len()
        );
    }

    Ok(
        match history::select_history(&messages, &coverage, omitted, &omitted_ids, budget) {
            Ok(selected) => ContextPlan::Deliver(pending, selected),
            Err(_) => ContextPlan::InsufficientBudget,
        },
    )
}
