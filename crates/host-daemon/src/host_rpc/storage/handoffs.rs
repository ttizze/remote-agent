//! Canonical context ownership and durable delivery, matching T3's handoff
//! service. Only turn items own text; handoff metadata names the source items.
use super::*;
use agent_protocol::orchestration::{self as model, handoffs as history};

impl Storage {
    /// Allocation may return after its start was settled. Keep the physical
    /// identity private, but only a user-cancelled start can retain a ready
    /// resource. A process-loss result must not attach or revive its session.
    pub(in crate::host_rpc) fn bind_abandoned_native_start(
        &self,
        effect: &model::Effect,
        provider_thread_id: &str,
        expected: Option<&model::ProviderRef>,
        mut session: model::ProviderSession,
        native: model::ProviderRef,
        native_fork: bool,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let status: Option<String> = tx.query_row(
                "SELECT status FROM orchestration_v2_effect_outbox WHERE effect_id=?1 AND thread_id=?2 AND effect_type='provider-turn.start'",
                params![effect.id,effect.thread_id], |row|row.get(0),
            ).optional().map_err(failure)?;
            let Some(status) = status.filter(|status|matches!(status.as_str(),"failed"|"cancelled")) else {return Ok(())};
            let state = projections::load(&tx,&effect.thread_id,&[])?;
            if status == "failed" || state.provider_threads.get(provider_thread_id).is_some_and(|thread|thread.status==model::ProviderThreadStatus::Error) {
                session.status=model::ProviderSessionStatus::Error;
            }
            let decision = model::native_resource_loaded(&state,provider_thread_id,expected,session,native,native_fork,now)?;
            commands::append_decision(&tx,&state,decision,None,now)?;
            tx.commit().map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn prepare_context(
        &self,
        effect: &model::Effect,
        worker: &str,
        recovery: &str,
        now: &str,
    ) -> Result<Option<model::State>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
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
            if run.status != model::RunStatus::Starting {
                return Ok(None);
            }
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
            // A planned native fork is resolved only after native allocation.
            // Preflight must not publish a replacement that may fail to exist.
            let native_fork = pending.is_some() && thread.forked_from.is_some();
            if !native_fork && (fresh
                || !state.context_handoffs.values().any(|handoff| {
                    handoff.target_run_id == run.id && handoff.to_provider_thread_id == thread.id
                }))
            {
                changes.extend(prepare(
                    &tx, &state, run, thread, pending, fresh, now,
                )?);
            }
            if !fresh && let Some(retry) = prepare_retry(&tx, &state, run, thread, thread.native_thread_ref.as_ref().and_then(|reference| reference.native_id.as_deref()), now)? {
                changes.push(model::Change::ContextHandoff(retry));
            }
            let mut decision = model::Decision {changes,effects:Vec::new()};
            if !fresh && let Some(native)=thread.native_thread_ref.as_ref().and_then(|reference|reference.native_id.as_deref()) {
                let candidate=state.apply(&decision);
                if pending_handoffs(&candidate.context_handoffs,&thread.id,native).next().is_some() {
                let budget=context_budget(&tx,&candidate,run,thread,Some(native))?;
                if matches!(select_context(&tx,&candidate.context_handoffs,&thread.id,native,budget,recovery)?,ContextPlan::InsufficientBudget) {
                    let replacement=prepare(&tx,&candidate,run,&candidate.provider_threads[&thread.id],None,true,now)?;
                    let restored=candidate.apply(&model::Decision {changes:replacement.clone(),effects:Vec::new()});
                    let budget=context_budget(&tx,&restored,run,thread,None)?;
                    if matches!(select_context(&tx,&restored.context_handoffs,&thread.id,"",budget,recovery)?,ContextPlan::Deliver(..)) {
                        if !thread.pending_background_tasks.is_empty() || state.nodes.values().any(|node|node.provider_thread_id.as_ref()==Some(&thread.id) && node.run_id.as_ref()!=Some(&run.id) && matches!(node.status,model::NodeStatus::Pending|model::NodeStatus::Running|model::NodeStatus::Waiting)) {return Err("native context has pending background work; cannot replace its thread".into());}
                        decision.changes.extend(replacement);
                    }
                }
                }
            }
            let result = state.apply(&decision);
            tx.commit().map_err(failure)?;
            Ok(Some(result))
        })
    }

    /// Publish the actual native resource and its recovery together. Planning
    /// can read source bodies here; no native I/O runs under the writer lock.
    pub(in crate::host_rpc) fn bind_native_start(
        &self,
        effect: &model::Effect,
        worker: &str,
        session: model::ProviderSession,
        native: model::ProviderRef,
        native_fork: bool,
        fork_failure: Option<&str>,
        now: &str,
    ) -> Result<bool, String> {
        self.with(|connection| {
            let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let state = projections::load(&tx, &effect.thread_id, &[])?;
            if !state.effects.get(&effect.id).is_some_and(|saved| saved.status == model::EffectStatus::Running && saved.lease_owner.as_deref() == Some(worker)) {
                return Ok(false);
            }
            let model::EffectRequest::ProviderTurnStart {run_id} = &effect.request else {return Err("native binding requires a start effect".into())};
            let run = state.runs.get(run_id).ok_or("native binding run is missing")?;
            let Some(attempt) = state.active_attempt(run.active_attempt_id.as_deref()).filter(|attempt| run.status == model::RunStatus::Starting && attempt.status == model::RunAttemptStatus::Pending && attempt.provider_turn_id.is_none()) else {return Ok(false)};
            let thread = state.provider_threads.get(&attempt.provider_thread_id).ok_or("native binding provider thread is missing")?;
            if session.provider_instance_id != run.provider_instance_id {
                return Err("native binding uses another provider".into());
            }
            let fork = state.unconsumed_fork().filter(|fork| fork.target_run_id.as_ref() == Some(&run.id) && thread.forked_from.is_some());
            let replaced = thread.native_thread_ref.as_ref().and_then(|old| old.native_id.as_ref()).is_some_and(|old| native.native_id.as_ref() != Some(old));
            let mut changes = if !native_fork && fork.is_some() {
                prepare(&tx, &state, run, thread, fork, false, now)?
            } else if !native_fork && replaced {
                let changes = prepare(&tx, &state, run, thread, None, true, now)?;
                for change in &changes {
                    if let model::Change::TurnItem(item) = change {
                        event_store::item_position(&tx, &item.thread_id, &item.id, item.run_id.as_deref(), Some(run.ordinal), event_store::ItemPlacement::RunStart)?;
                    }
                }
                changes
            } else {Vec::new()};
            if !replaced && let Some(retry) = prepare_retry(&tx, &state, run, thread, native.native_id.as_deref(), now)? {
                changes.push(model::Change::ContextHandoff(retry));
            }
            let prepared_thread = changes.iter().rev().find_map(|change|match change {model::Change::ProviderThread(value) if value.id == thread.id => Some(value), _=>None}).unwrap_or(thread);
            let cwd=session.cwd.clone();
            let prepared=state.apply(&model::Decision{changes:changes.clone(),effects:Vec::new()});
            let loaded=model::native_resource_loaded(&prepared,&thread.id,prepared_thread.native_thread_ref.as_ref(),session,native.clone(),native_fork,now)?;
            if loaded.changes.is_empty(){return Ok(false);}
            changes.extend(loaded.changes);
            changes.push(model::Change::RunAttempt(model::RunAttempt{native_thread_ref:Some(native),..attempt.clone()}));
            let mut decision=model::Decision{changes,effects:Vec::new()};
            if let Some(error) = fork_failure {
                decision.changes.push(model::Change::TurnItem(model::TurnItem {
                    id:model::derived_id("turn-item", &["run",&run.id,"signal","native-fork-fallback"]),
                    thread_id:state.thread.id.clone(),run_id:Some(run.id.clone()),node_id:Some(attempt.root_node_id.clone()),provider_thread_id:Some(thread.id.clone()),provider_turn_id:None,native_item_ref:None,parent_item_id:None,ordinal:0,status:model::TurnItemStatus::Completed,title:Some("Native fork failed".into()),started_at:Some(now.into()),completed_at:Some(now.into()),updated_at:now.into(),
                    body:model::TurnItemBody::SystemNotice {message:format!("Could not fork the source conversation natively ({error}). Started a fresh session with its history.")},
                }));
            }
            let scope = model::prepare_checkpoint_scope(&state.apply(&decision), &run.id, &cwd, now)?;
            decision.changes.extend(scope.changes);
            if state.thread.worktree_path.as_deref() != Some(&cwd) {
                decision.changes.push(model::Change::Thread(model::Thread {worktree_path:Some(cwd),updated_at:now.into(),..state.thread.clone()}));
            }
            commands::append_decision(&tx, &state, decision, None, now)?;
            tx.commit().map_err(failure)?;
            Ok(true)
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

fn selected_context(
    thread_id: &str,
    covered: &model::CoveredRunOrdinals,
    items: &[(model::TurnItem, Option<model::RunStatus>)],
) -> Result<(String, history::SelectedHistory), projections::ReadError> {
    let messages: Vec<_> = items
        .iter()
        .filter_map(|(item, status)| history::historical_message(item, *status))
        .collect();
    let coverage = format!(
        "Provider context handoff. Thread: {}. Covered app runs: {}-{}.\nSource item range: {} through {}.\nRecover omitted history only when the request needs it. Items included below need no read.",
        thread_id,
        covered.from,
        covered.to,
        items.first().map_or("none", |(item, _)| item.id.as_str()),
        items.last().map_or("none", |(item, _)| item.id.as_str())
    );
    let selected =
        history::select_history(&messages, &coverage, 0, &[], history::DEFAULT_TOKEN_CAP)?;
    Ok((coverage, selected))
}

/// Failed requests without a native turn are history, never inputs to resend.
/// Settled delivery covers both selected and intentionally omitted items.
fn prepare_retry(
    connection: &Connection,
    state: &model::State,
    run: &model::Run,
    thread: &model::ProviderThread,
    native_id: Option<&str>,
    now: &str,
) -> Result<Option<model::ContextHandoff>, projections::ReadError> {
    let accepted: std::collections::BTreeSet<_> = state
        .provider_turns
        .values()
        .filter_map(|turn| turn.run_attempt_id.as_deref())
        .collect();
    let missed: Vec<_> = state
        .runs
        .values()
        .filter(|source| {
            source.ordinal < run.ordinal
                && source.provider_thread_id.as_ref() == Some(&thread.id)
                && matches!(
                    source.status,
                    model::RunStatus::Failed | model::RunStatus::Interrupted
                )
                && !source
                    .active_attempt_id
                    .as_deref()
                    .is_some_and(|id| accepted.contains(id))
        })
        .collect();
    let (Some(from), Some(to)) = (
        missed.iter().map(|run| run.ordinal).min(),
        missed.iter().map(|run| run.ordinal).max(),
    ) else {
        return Ok(None);
    };
    let covered_ids: std::collections::BTreeSet<_> = state
        .context_handoffs
        .values()
        .filter(|handoff| handoff.to_provider_thread_id == thread.id)
        .filter_map(|handoff| handoff.delivery.as_ref())
        .filter(|delivery| {
            Some(delivery.native_thread_id.as_str()) == native_id
                && delivery.status != model::HandoffDeliveryStatus::Pending
        })
        .flat_map(|delivery| delivery.item_ids.iter().chain(&delivery.omitted_item_ids))
        .collect();
    let missed_ids: std::collections::BTreeSet<_> =
        missed.iter().map(|run| run.id.as_str()).collect();
    let items: Vec<_> = timeline::handoff_items(
        connection,
        &state.thread.id,
        from.saturating_sub(1),
        to,
        false,
    )?
    .into_iter()
    .filter(|(item, _)| {
        item.run_id
            .as_deref()
            .is_some_and(|id| missed_ids.contains(id))
            && !covered_ids.contains(&item.id)
    })
    .collect();
    let covered = model::CoveredRunOrdinals { from, to };
    let (coverage, selected) = selected_context(&state.thread.id, &covered, &items)?;
    if selected.messages.is_empty() && selected.omitted_items == 0 {
        return Ok(None);
    }
    Ok(Some(model::ContextHandoff {
        id: model::derived_id("context-handoff", &[&run.id, "retry"]),
        transfer_id: None,
        thread_id: state.thread.id.clone(),
        target_run_id: run.id.clone(),
        from_provider_thread_ids: vec![thread.id.clone()],
        to_provider_thread_id: thread.id.clone(),
        covered_run_ordinals: covered,
        strategy: model::HandoffStrategy::DeltaSinceTargetLastSeen,
        status: model::HandoffStatus::Ready,
        summary_message_id: None,
        history: Some(model::HandoffHistory {
            messages: selected
                .messages
                .into_iter()
                .map(|message| message.message)
                .collect(),
            coverage,
            omitted_items: selected.omitted_items as u64,
            omitted_item_ids: selected.omitted_item_ids,
        }),
        delivery: None,
        detail_in_turn_item: false,
        created_by_provider_instance_id: None,
        created_at: now.into(),
        updated_at: now.into(),
    }))
}

fn prepare(
    connection: &Connection,
    state: &model::State,
    run: &model::Run,
    thread: &model::ProviderThread,
    fork: Option<&model::ContextTransfer>,
    fresh: bool,
    now: &str,
) -> Result<Vec<model::Change>, projections::ReadError> {
    let fork_source = fork
        .map(|fork| projections::metadata(connection, &fork.source_thread_id))
        .transpose()?;
    let source = fork_source.as_ref().unwrap_or(state);
    let latest = if let Some(fork) = fork {
        fork.source_point
            .run_id
            .as_ref()
            .map(|id| {
                source
                    .runs
                    .get(id)
                    .ok_or("fork context source run is missing")
            })
            .transpose()?
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
    let after = if fork.is_some() || fresh || thread.native_thread_ref.is_none() {
        0
    } else {
        source
            .last_delivered_run(&thread.id)
            .map_or(0, |run| run.ordinal)
    };
    let items = if fork.is_some() {
        // The target's inherited timeline already owns the exact source cutoff.
        // Its first new input is a local run and must not echo in that context.
        timeline::handoff_items(connection, &state.thread.id, 0, 0, true)?
    } else {
        timeline::handoff_items(connection, &source.thread.id, after, through, after == 0)?
    };
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
    let (coverage, selected) = selected_context(&source.thread.id, &covered, &items)?;
    if selected.messages.is_empty() && selected.omitted_items == 0 && fork.is_none() && !fresh {
        return Ok(Vec::new());
    }
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
                    && candidate.started_at.is_some()
            })
            .max_by_key(|run| run.ordinal)
            .into_iter()
            .collect()
    } else {
        covered_runs
    };
    source_runs.sort_by_key(|run| run.ordinal);
    let native_source =
        fork.filter(|_| latest.is_none())
            .and_then(|_| match &state.thread.forked_from {
                Some(model::ThreadForkSource::ProviderThread {
                    provider_thread_id, ..
                }) => source.provider_threads.get(provider_thread_id),
                _ => None,
            });
    let from: Vec<_> = if fresh {
        vec![thread.id.clone()]
    } else {
        source_runs
            .iter()
            .filter_map(|run| run.provider_thread_id.clone())
            .chain(native_source.map(|thread| thread.id.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let from_models =
        history::distinct_model_selections(source_runs.iter().map(|run| &run.model_selection));
    let from_providers = if fresh {
        vec![thread.provider_instance_id.clone()]
    } else {
        source_runs
            .iter()
            .map(|run| run.provider_instance_id.clone())
            .chain(native_source.map(|thread| thread.provider_instance_id.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let strategy = if legacy_import {
        model::HandoffStrategy::ManualContext
    } else if after == 0 {
        model::HandoffStrategy::FullThreadSummary
    } else {
        model::HandoffStrategy::DeltaSinceTargetLastSeen
    };
    let runtime_fork = fork.is_some() && thread.forked_from.is_some();
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
        detail_in_turn_item: !runtime_fork,
        created_by_provider_instance_id: None,
        created_at: now.into(),
        updated_at: now.into(),
    };
    let actor = state
        .turn_items
        .values()
        .find_map(|item| match &item.body {
            model::TurnItemBody::UserMessage {
                message_id,
                created_by,
                ..
            } if message_id == &run.user_message_id => Some(*created_by),
            _ => None,
        })
        .ok_or("context target input is missing")?;
    let point = latest
        .filter(|_| !fresh)
        .map(|run| source.context_source_point(run))
        .unwrap_or(model::ContextSourcePoint {
            thread_id: source.thread.id.clone(),
            run_id: None,
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        });
    let base = source
        .runs
        .values()
        .find(|run| run.ordinal == after)
        .map(|run| source.context_source_point(run));
    let transfer = history::provider_transfer(
        &handoff,
        point,
        base,
        if fresh {
            Some(thread.provider_instance_id.clone())
        } else {
            latest.map(|run| run.provider_instance_id.clone())
        },
        &run.provider_instance_id,
        actor,
        fork,
        fresh,
        now,
    );
    let mut handoff_ids = thread.handoff_ids.clone();
    if !handoff_ids.contains(&id) {
        handoff_ids.push(id.clone());
    }
    let planned_id = id.clone();
    let item = (!runtime_fork).then(|| model::TurnItem {
        id: if fresh {
            model::derived_id(
                "turn-item",
                &["run", &run.id, "signal", "resume-fallback-handoff"],
            )
        } else {
            model::derived_id(
                "turn-item",
                &["run", &run.id, "signal", &format!("context-handoff:{id}")],
            )
        },
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
    });
    let mut changes = vec![
        model::Change::ProviderThread(model::ProviderThread {
            native_thread_ref: if fresh {
                None
            } else {
                thread.native_thread_ref.clone()
            },
            forked_from: if fork.is_some() {
                None
            } else {
                thread.forked_from.clone()
            },
            handoff_ids,
            updated_at: now.into(),
            ..thread.clone()
        }),
        model::Change::ContextHandoff(handoff),
    ];
    changes.extend(item.map(model::Change::TurnItem));
    if !fresh && !runtime_fork {
        changes.push(model::Change::Run(model::Run {
            context_handoff_id: Some(planned_id),
            ..run.clone()
        }));
    }
    changes.extend(transfer);
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
                            && attempt
                                .native_thread_ref
                                .as_ref()
                                .and_then(|reference| reference.native_id.as_deref())
                                == native
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
                    && attempt
                        .native_thread_ref
                        .as_ref()
                        .and_then(|reference| reference.native_id.as_deref())
                        == Some(native)
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
    let mut coverage = format!(
        "{}\nRead omitted history on this Host with the BEX history command below.\n{recovery}",
        coverage.join("\n")
    );
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

/// Plan only newly admitted starts. Canonical native-fork provenance records
/// the capability decision made by admission; queued activation reuses it.
/// Source availability is rechecked under the same writer transaction.
pub(super) fn prepare_admission(
    connection: &Connection,
    state: &model::State,
    run_ids: &[String],
    now: &str,
) -> Result<Vec<model::Change>, projections::ReadError> {
    let mut changes = Vec::new();
    for id in run_ids {
        let run = state.runs.get(id).ok_or("context start run is missing")?;
        let mut thread = run
            .provider_thread_id
            .as_ref()
            .and_then(|id| state.provider_threads.get(id))
            .ok_or("context provider thread is missing")?
            .clone();
        if state.context_handoffs.values().any(|handoff| {
            handoff.target_run_id == run.id && handoff.to_provider_thread_id == thread.id
        }) {
            continue;
        }
        let fork = state
            .unconsumed_fork()
            .filter(|transfer| transfer.target_run_id.as_ref() == Some(&run.id));
        if fork.is_some() {
            let native = thread.forked_from.is_some()
                && fork.is_some_and(|transfer| {
                    transfer.source_provider_instance_id.as_deref()
                        == Some(run.provider_instance_id.as_str())
                })
                && super::forks::native_origin(connection, state)?.is_some();
            if native {
                continue;
            }
            thread.forked_from = None;
        }
        changes.extend(prepare(connection, state, run, &thread, fork, false, now)?);
    }
    Ok(changes)
}
