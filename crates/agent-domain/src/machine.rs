use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The error code of a run whose workspace preparation failed.
pub const WORKSPACE_PREPARATION_FAILURE_CODE: &str = "workspace_preparation_failed";
/// The command row that stands for a deferred run's workspace preparation.
pub const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";
/// What a rollback the provider could not carry out records.
pub const ROLLBACK_FAILED_MESSAGE: &str = "The provider could not roll back this conversation. Try again; if it keeps failing, check the provider and server logs.";
const INTERRUPT_REQUESTED: &str = "Interrupt requested";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputEnvelope {
    pub at: Timestamp,
    pub key: String,
    pub input: Input,
}
/// Negotiated values used by the domain, rather than an adapter or session object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnSupport {
    pub steer: bool,
    pub interrupt: bool,
    pub restart: bool,
    pub queue: bool,
}
impl TurnSupport {
    pub fn for_driver(driver: Driver) -> Self {
        Self {
            steer: true,
            interrupt: true,
            restart: driver == Driver::Codex,
            queue: true,
        }
    }
}
pub fn resolve_dispatch(
    active: Option<(&RunId, RunStatus)>,
    requested: &DispatchMode,
    intent: Option<DeliveryIntent>,
    support: TurnSupport,
) -> DispatchMode {
    let Some((run, status)) = active else {
        return if intent.is_some() {
            DispatchMode::StartImmediately
        } else {
            requested.clone()
        };
    };
    match intent {
        Some(DeliveryIntent::Steer) => DispatchMode::SteerActive { run: run.clone() },
        Some(DeliveryIntent::Restart) => DispatchMode::RestartActive { run: run.clone() },
        Some(DeliveryIntent::Auto)
            if matches!(status, RunStatus::Preparing | RunStatus::Starting) =>
        {
            DispatchMode::QueueAfterActive
        }
        Some(DeliveryIntent::Auto) if support.steer => {
            DispatchMode::SteerActive { run: run.clone() }
        }
        Some(DeliveryIntent::Auto) if support.queue => DispatchMode::QueueAfterActive,
        Some(DeliveryIntent::Auto) if support.interrupt && support.restart => {
            DispatchMode::RestartActive { run: run.clone() }
        }
        _ => requested.clone(),
    }
}
struct Decision {
    state: State,
    at: Timestamp,
    seed: String,
    facts: Vec<Fact>,
    effects: Vec<Effect>,
}
impl Decision {
    fn new(state: &State, input: &InputEnvelope) -> Self {
        Self {
            state: state.clone(),
            at: input.at.clone(),
            seed: input.key.clone(),
            facts: vec![],
            effects: vec![],
        }
    }
    fn fact(&mut self, body: FactBody) {
        self.fact_at(self.at.clone(), body);
    }
    fn fact_at(&mut self, at: Timestamp, body: FactBody) {
        let fact = Fact { at, body };
        apply(&mut self.state, &fact).expect("domain decisions produce valid facts");
        self.facts.push(fact);
    }
    fn key(&self, kind: &str, source: &str) -> String {
        format!("{kind}:{}:{}:{source}", self.seed.len(), self.seed)
    }
    fn effect(&mut self, attempt: Option<RunAttemptId>, body: EffectBody) {
        self.effects.push(Effect {
            id: self.key("effect", &self.effects.len().to_string()),
            attempt,
            body,
        });
    }
    fn item_ordinal(&self) -> u64 {
        self.state
            .items
            .iter()
            .chain(&self.state.inherited_items)
            .map(|i| i.ordinal)
            .max()
            .unwrap_or(0)
            + 1
    }
    fn item_start(
        &mut self,
        id: TurnItemId,
        run: Option<RunId>,
        attempt: Option<RunAttemptId>,
        key: String,
        kind: ItemKind,
    ) {
        self.fact(FactBody::ItemStarted {
            id,
            run,
            attempt,
            native_key: key,
            ordinal: self.item_ordinal(),
            kind,
        });
    }
    fn user_item(&mut self, message: &MessageId, run: &RunId) {
        if self
            .state
            .items
            .iter()
            .any(|i| matches!(&i.kind,ItemKind::UserMessage { message:m } if m==message))
        {
            return;
        }
        let id =
            TurnItemId::new(format!("item:user:{}:{}", message.as_str().len(), message)).unwrap();
        self.item_start(
            id.clone(),
            Some(run.clone()),
            None,
            message.to_string(),
            ItemKind::UserMessage {
                message: message.clone(),
            },
        );
        let text = self
            .state
            .messages
            .iter()
            .find(|m| &m.id == message)
            .unwrap()
            .text
            .clone();
        self.append_text(&id, &text);
        self.fact(FactBody::ItemCompleted {
            id,
            status: ItemStatus::Completed,
        });
    }
    fn append_text(&mut self, id: &TurnItemId, text: &str) {
        self.append_text_at(self.at.clone(), id, text);
    }
    fn append_text_at(&mut self, at: Timestamp, id: &TurnItemId, text: &str) {
        for chunk in text_chunks(text) {
            let offset = self
                .state
                .items
                .iter()
                .find(|i| &i.id == id)
                .unwrap()
                .text
                .len();
            self.fact_at(
                at.clone(),
                FactBody::ItemTextAppended {
                    id: id.clone(),
                    offset,
                    text: chunk.to_owned(),
                },
            );
        }
    }
    fn replace_text(&mut self, id: &TurnItemId, text: &str) {
        let chunks = text_chunks(text);
        self.fact(FactBody::ItemTextReplaced {
            id: id.clone(),
            text: chunks.first().copied().unwrap_or_default().to_owned(),
        });
        for chunk in chunks.iter().skip(1) {
            self.append_text(id, chunk);
        }
    }
    fn append_plan(&mut self, id: &PlanId, text: &str) {
        for chunk in text_chunks(text) {
            let offset = self
                .state
                .plans
                .iter()
                .find(|p| &p.id == id)
                .unwrap()
                .markdown
                .len();
            self.fact(FactBody::PlanMarkdownAppended {
                id: id.clone(),
                offset,
                text: chunk.to_owned(),
            });
        }
    }
    fn prepare_provider_handoff(&mut self, target_run: &Run, native_thread: Option<&str>) {
        let is_source = |run: &Run| {
            matches!(
                run.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Interrupted
            )
        };
        // Imported history reached only the native session it was imported from.
        let imported = self
            .state
            .items
            .iter()
            .filter(|item| {
                item.run.is_none()
                    && item.attempt.is_none()
                    && matches!(
                        item.kind,
                        ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. }
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let latest = self.state.runs.iter().rev().find(|run| is_source(run));
        let needs_backfill = native_thread.is_none()
            && (!imported.is_empty()
                || self.state.attempts.iter().any(|attempt| {
                    attempt.native_thread.is_some()
                        && self.state.runs.iter().any(|run| {
                            run.id == attempt.run
                                && run.selection.instance == target_run.selection.instance
                                && is_source(run)
                        })
                }));
        if latest.is_none_or(|latest| latest.selection.instance == target_run.selection.instance)
            && !needs_backfill
        {
            if let Some(native) = native_thread {
                self.prepare_missed_inputs(target_run, native);
            }
            return;
        }
        let boundary = latest.map_or(0, |latest| latest.ordinal);
        if self.state.transfers.iter().any(|transfer| {
            !transfer.superseded
                && transfer.instance.as_deref() == Some(target_run.selection.instance.as_str())
                && matches!(
                    transfer.kind,
                    TransferKind::ProviderHandoff | TransferKind::ProviderHandoffDelta
                )
                && transfer.boundary == boundary
                && (native_thread.is_some() || transfer.kind == TransferKind::ProviderHandoff)
        }) {
            return;
        }
        let last_seen = native_thread
            .and_then(|native| {
                self.state.runs.iter().rev().find(|run| {
                    is_source(run)
                        && run.selection.instance == target_run.selection.instance
                        && run
                            .attempt
                            .as_ref()
                            .and_then(|id| {
                                self.state.attempts.iter().find(|attempt| &attempt.id == id)
                            })
                            .is_some_and(|attempt| {
                                attempt.native_thread.as_deref() == Some(native) && attempt.accepted
                            })
                })
            })
            .map_or(0, |run| run.ordinal);
        let items = imported
            .into_iter()
            .filter(|_| native_thread.is_none())
            .chain(
                self.state
                    .visible_items()
                    .into_iter()
                    .filter(|item| {
                        item.run.as_ref().is_some_and(|id| {
                            self.state.runs.iter().any(|run| {
                                &run.id == id
                                    && is_source(run)
                                    && run.ordinal > last_seen
                                    && run.ordinal <= boundary
                            })
                        })
                    })
                    .cloned(),
            )
            .collect::<Vec<_>>();
        if items.is_empty() {
            return;
        }
        let thread = self.state.thread.as_ref().unwrap().id.clone();
        self.fact(FactBody::TransferOpened {
            native_source: None,
            target_run: None,
            id: ContextTransferId::new(self.key("provider-handoff", target_run.id.as_str()))
                .unwrap(),
            kind: if native_thread.is_some() {
                TransferKind::ProviderHandoffDelta
            } else {
                TransferKind::ProviderHandoff
            },
            source: thread.clone(),
            target: thread,
            boundary,
            instance: Some(target_run.selection.instance.clone()),
            history: prepare_history(&self.state, &items, boundary),
        });
    }
    /// Failed or interrupted inputs that the provider never accepted are
    /// missing from the native history of a session that is otherwise current.
    fn prepare_missed_inputs(&mut self, target_run: &Run, native: &str) {
        let instance = &target_run.selection.instance;
        let missed = self
            .state
            .runs
            .iter()
            .filter(|run| {
                run.ordinal < target_run.ordinal
                    && &run.selection.instance == instance
                    && matches!(run.status, RunStatus::Failed | RunStatus::Interrupted)
                    && run.attempt.as_ref().is_some_and(|id| {
                        self.state
                            .attempts
                            .iter()
                            .any(|attempt| &attempt.id == id && !attempt.accepted)
                    })
            })
            .map(|run| (run.id.clone(), run.ordinal))
            .collect::<BTreeMap<_, _>>();
        let Some(boundary) = missed.values().copied().max() else {
            return;
        };
        let covered = self
            .state
            .transfers
            .iter()
            .filter(|transfer| transfer.instance.as_ref() == Some(instance))
            .filter_map(|transfer| transfer.delivery.as_ref())
            .filter(|delivery| {
                delivery.native_thread.as_deref() == Some(native)
                    && delivery.status != ContextDeliveryStatus::Pending
            })
            .flat_map(|delivery| delivery.item_ids.iter().chain(&delivery.omitted_item_ids))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let items = self
            .state
            .visible_items()
            .into_iter()
            .filter(|item| {
                item.run
                    .as_ref()
                    .is_some_and(|run| missed.contains_key(run))
                    && !covered.contains(item.id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if items.is_empty()
            || self.state.transfers.iter().any(|transfer| {
                !transfer.superseded
                    && transfer.instance.as_ref() == Some(instance)
                    && transfer.kind == TransferKind::ProviderHandoffDelta
                    && transfer.boundary == boundary
                    && transfer.delivery.is_none()
            })
        {
            return;
        }
        let thread = self.state.thread.as_ref().unwrap().id.clone();
        self.fact(FactBody::TransferOpened {
            native_source: None,
            target_run: None,
            id: ContextTransferId::new(self.key("missed-inputs", target_run.id.as_str())).unwrap(),
            kind: TransferKind::ProviderHandoffDelta,
            source: thread.clone(),
            target: thread,
            boundary,
            instance: Some(instance.clone()),
            history: prepare_history(&self.state, &items, boundary),
        });
    }
    fn start_run(&mut self, id: &RunId) {
        // Fork and merge-back transfers are consumed only on a direct dispatch,
        // never when a queued run starts.
        let direct = self
            .state
            .runs
            .iter()
            .any(|r| &r.id == id && r.status != RunStatus::Queued);
        let checkpoint_scope = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == id)
            .unwrap()
            .checkpoint_scope
            .clone()
            .or(self.state.checkpoint_scope.clone());
        self.fact(FactBody::RunStarted {
            id: id.clone(),
            checkpoint_scope,
            native_baseline_heads: self.state.native_heads.clone(),
        });
        let run = self
            .state
            .runs
            .iter()
            .find(|run| &run.id == id)
            .unwrap()
            .clone();
        self.user_item(&run.message, id);
        let ordinal = self
            .state
            .attempts
            .iter()
            .filter(|a| a.run == run.id)
            .map(|a| a.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let attempt = RunAttemptId::new(self.key("attempt", &format!("{}:{ordinal}", id))).unwrap();
        self.fact(FactBody::AttemptStarted {
            id: attempt.clone(),
            run: id.clone(),
            ordinal,
        });
        if run.continuation {
            let events = self
                .state
                .native_continuations
                .get(id)
                .cloned()
                .unwrap_or_default();
            self.fact(FactBody::NativeContinuationReleased { run: id.clone() });
            self.provider(&attempt, &ProviderEvent::TurnStarted { native_turn: None });
            for event in events {
                self.provider(&attempt, &event);
            }
            return;
        }
        self.dispatch_start(id, &attempt, direct);
    }
    /// Sends a started attempt to its provider, after a native fork when the
    /// run resolves a pending fork on the source's provider.
    fn dispatch_start(&mut self, id: &RunId, attempt: &RunAttemptId, direct: bool) {
        let attempt = attempt.clone();
        let run = self
            .state
            .runs
            .iter()
            .find(|run| &run.id == id)
            .unwrap()
            .clone();
        let previous = self.state.thread.as_ref().unwrap().selection.clone();
        if previous != run.selection {
            self.fact(FactBody::ModelSelected {
                selection: run.selection.clone(),
            });
            // The provider-switch release plan applies when a queued run
            // takes another instance.
            if previous.instance != run.selection.instance {
                self.release_other_instances(&run.selection.instance);
            }
        }
        let message = self
            .state
            .messages
            .iter()
            .find(|m| m.id == run.message)
            .unwrap()
            .clone();
        if direct
            && let Some(source) = self.pending_fork().and_then(|transfer| {
                transfer
                    .native_source
                    .clone()
                    .filter(|source| source.instance == run.selection.instance)
            })
        {
            self.effect(
                Some(attempt),
                EffectBody::ForkNative {
                    instance: run.selection.instance.clone(),
                    provider: ProviderCommand::Fork {
                        native_thread: source.thread,
                        through_turn: source.head,
                    },
                },
            );
            return;
        }
        let native_thread = self.start_native_thread(&run);
        self.prepare_provider_handoff(&run, native_thread.as_deref());
        self.send_start(&run, attempt, message, native_thread, direct);
    }
    /// The native session a start continues, unless history may already have
    /// reached it through an unconfirmed delivery.
    fn start_native_thread(&mut self, run: &Run) -> Option<String> {
        let thread = self.state.thread.as_ref().unwrap().id.clone();
        let native_thread = self
            .state
            .native_sessions
            .get(&run.selection.instance)
            .cloned();
        if native_thread.is_some()
            && self.state.transfers.iter().any(|transfer| {
                !transfer.superseded
                    && transfer.target == thread
                    && transfer.instance.as_deref() == Some(run.selection.instance.as_str())
                    && transfer.delivery.as_ref().is_some_and(|delivery| {
                        delivery.native_thread == native_thread
                            && delivery.status == ContextDeliveryStatus::Pending
                    })
            })
        {
            // History may already be in that native thread; continue in a fresh one.
            self.fact(FactBody::NativeSessionCleared {
                instance: run.selection.instance.clone(),
            });
            return None;
        }
        native_thread
    }
    /// Sends `message` as the attempt's turn with the transfers it consumes.
    fn send_start(
        &mut self,
        run: &Run,
        attempt: RunAttemptId,
        message: Message,
        native_thread: Option<String>,
        direct: bool,
    ) {
        let id = &run.id;
        let thread = self.state.thread.as_ref().unwrap().clone();
        let restart_work = pending_restart_work(
            run,
            &self.state.runs,
            &self.state.attempts,
            &self.state.messages,
        );
        let note = (!restart_work.is_empty()).then(|| restart_background_note(&restart_work));
        // Native compaction defers portable context until the next ordinary input.
        if message.text.trim() == "/compact" {
            self.effect(
                Some(attempt),
                EffectBody::Provider(ProviderCommand::Compact { native_thread }),
            );
            return;
        }
        let transfers = self
            .state
            .transfers
            .iter()
            .filter(|transfer| {
                !transfer.superseded
                    && transfer.target == thread.id
                    && consumable(&self.state, transfer, run, direct)
                    && transfer.delivery.as_ref().is_none_or(|delivery| {
                        delivery.native_thread != native_thread
                            || delivery.status == ContextDeliveryStatus::Pending
                    })
            })
            .collect::<Vec<_>>();
        let context = if transfers.is_empty() {
            None
        } else {
            let previous = self
                .state
                .attempts
                .iter()
                .rev()
                .filter(|previous| {
                    previous.id != attempt && previous.native_thread == native_thread
                })
                .find_map(|previous| {
                    let previous_run = self.state.runs.iter().find(|r| {
                        r.id == previous.run
                            && r.selection.instance == run.selection.instance
                            && r.status != RunStatus::RolledBack
                    })?;
                    previous
                        .context_usage
                        .as_ref()
                        .map(|usage| (usage, previous_run.selection == run.selection))
                });
            let model_window = self
                .state
                .context_windows
                .get(&run.selection.instance)
                .copied();
            let usage = context_usage_for_handoff(
                native_thread.is_some(),
                previous.is_some_and(|(_, same)| same),
                false,
                previous.map(|(usage, _)| usage),
                model_window,
            );
            let delivered = self
                .state
                .transfers
                .iter()
                .filter(|transfer| {
                    transfer.instance.as_deref() == Some(run.selection.instance.as_str())
                })
                .filter_map(|transfer| transfer.delivery.as_ref())
                .filter(|delivery| {
                    delivery.native_thread == native_thread
                        && delivery.status != ContextDeliveryStatus::Pending
                })
                .flat_map(|delivery| delivery.item_ids.clone())
                .collect();
            let estimate = native_thread.as_deref().map_or(0, |native| {
                self.native_history_estimate(run, native, &delivered)
            });
            let budget = handoff_budget(
                self.state
                    .handoff_token_cap
                    .unwrap_or(DEFAULT_HANDOFF_TOKEN_CAP),
                &note.as_ref().map_or_else(
                    || message.text.clone(),
                    |note| format!("{note}\n\n{}", message.text),
                ),
                &message.attachments,
                usage.as_ref(),
                estimate,
                model_window,
            );
            match combine_handoffs(&transfers, &thread.id, &delivered, budget) {
                Ok(context) => Some(context),
                Err(message) => {
                    self.fail_start(id, &attempt, message);
                    return;
                }
            }
        };
        if let Some(context) = &context {
            let deliveries = transfers
                .iter()
                .map(|transfer| {
                    (
                        transfer.id.clone(),
                        ContextDelivery {
                            attempt: attempt.clone(),
                            run: run.id.clone(),
                            native_thread: native_thread.clone(),
                            status: ContextDeliveryStatus::Pending,
                            item_ids: context
                                .messages
                                .iter()
                                .filter(|message| {
                                    transfer
                                        .history
                                        .messages
                                        .iter()
                                        .any(|candidate| candidate.item == message.item)
                                })
                                .map(|message| message.item.clone())
                                .collect(),
                            omitted_item_ids: transfer
                                .history
                                .omitted_item_ids
                                .iter()
                                .cloned()
                                .chain(
                                    context
                                        .omitted_item_ids
                                        .iter()
                                        .filter(|id| {
                                            transfer
                                                .history
                                                .messages
                                                .iter()
                                                .any(|message| &message.item == *id)
                                        })
                                        .cloned(),
                                )
                                .collect(),
                        },
                    )
                })
                .collect::<Vec<_>>();
            for (id, delivery) in deliveries {
                self.fact(FactBody::TransferDeliveryChanged { id, delivery });
            }
        }
        self.effect(
            Some(attempt),
            EffectBody::Provider(ProviderCommand::Start {
                resume_interrupted_turn: native_thread.is_some()
                    && context.is_none()
                    && run
                        .restart_of
                        .as_ref()
                        .and_then(|source| self.state.runs.iter().find(|run| &run.id == source))
                        .is_some_and(|source| {
                            restart_continuation_work(
                                source,
                                &self.state.runs,
                                &self.state.attempts,
                            )
                            .is_empty()
                        }),
                selection: run.selection.clone(),
                runtime_mode: thread.runtime_mode,
                interaction_mode: thread.interaction_mode,
                text: project_context_for_provider(&message.text, message.context.as_ref()),
                note,
                attachments: message.attachments,
                native_thread,
                resume_at: self
                    .state
                    .native_heads
                    .get(&run.selection.instance)
                    .cloned()
                    .flatten(),
                context,
            }),
        );
    }
    /// Occupancy of a native session without telemetry: its own accepted
    /// history and the foreign history already delivered to it, plus the
    /// attachment allowance of inputs that reached it.
    fn native_history_estimate(
        &self,
        target: &Run,
        native: &str,
        delivered: &std::collections::BTreeSet<String>,
    ) -> u64 {
        let thread = &self.state.thread.as_ref().unwrap().id;
        let attempt_of = |run: &Run| {
            run.attempt
                .as_ref()
                .and_then(|id| self.state.attempts.iter().find(|attempt| &attempt.id == id))
        };
        let own = |id: &RunId| {
            self.state.runs.iter().find(|run| {
                &run.id == id
                    && run.selection.instance == target.selection.instance
                    && !(matches!(run.status, RunStatus::Failed | RunStatus::Interrupted)
                        && attempt_of(run).is_none_or(|attempt| !attempt.accepted))
            })
        };
        self.state
            .visible_items()
            .into_iter()
            .filter(|item| item.run.as_ref() != Some(&target.id))
            .filter_map(|item| {
                let run = item.run.as_ref().and_then(own);
                if run.is_none() && !delivered.contains(item.id.as_str()) {
                    return None;
                }
                let text = historical_message(item, thread, None, None)?.text.len() as u64;
                let reached = run.and_then(attempt_of).is_some_and(|attempt| {
                    attempt.accepted && attempt.native_thread.as_deref() == Some(native)
                });
                let allowance = match &item.kind {
                    ItemKind::UserMessage { message } if reached => self
                        .state
                        .messages
                        .iter()
                        .find(|candidate| &candidate.id == message)
                        .map_or(0, |message| attachment_allowance(&message.attachments)),
                    _ => 0,
                };
                Some(text + allowance)
            })
            .sum()
    }
    fn fail_start(&mut self, run: &RunId, attempt: &RunAttemptId, message: &str) {
        let key = self.key("handoff-failure", attempt.as_str());
        self.provider(
            attempt,
            &ProviderEvent::ItemFinished {
                key,
                kind: ProviderItem::Error {
                    message: message.into(),
                    retry: None,
                    code: None,
                    class: Some("context_handoff".into()),
                    retryable: Some(true),
                },
                text: None,
                status: ItemStatus::Failed,
            },
        );
        self.finish(run, RunStatus::Failed, false);
    }
    fn complete_context_delivery(&mut self, attempt: &RunAttemptId, status: ContextDeliveryStatus) {
        let deliveries = self
            .state
            .transfers
            .iter()
            .filter_map(|transfer| {
                transfer
                    .delivery
                    .as_ref()
                    .filter(|delivery| {
                        &delivery.attempt == attempt
                            && delivery.status == ContextDeliveryStatus::Pending
                    })
                    .map(|delivery| {
                        let mut delivery = delivery.clone();
                        delivery.status = status;
                        let instance = self
                            .state
                            .runs
                            .iter()
                            .find(|run| run.id == delivery.run)
                            .map(|run| &run.selection.instance);
                        delivery.native_thread = instance
                            .and_then(|instance| self.state.native_sessions.get(instance).cloned());
                        (transfer.id.clone(), delivery)
                    })
            })
            .collect::<Vec<_>>();
        for (id, delivery) in deliveries {
            self.fact(FactBody::TransferDeliveryChanged { id, delivery });
        }
    }
    fn promote(&mut self) {
        if self.state.active_run().is_some()
            || !self.state.captures.is_empty()
            || self.state.rollback.is_some()
            || self
                .state
                .thread
                .as_ref()
                .is_none_or(|t| t.archived_at.is_some() || t.deleted_at.is_some())
            || self.state.queued_runs().iter().any(|r| r.queue_held)
            || usage_limited(&self.state)
        {
            return;
        }
        if let Some(run) = self.state.queued_runs().first().map(|r| r.id.clone()) {
            self.start_run(&run);
        }
    }
    /// A provider failure of the latest executed run holds queued input for
    /// the same provider until the user resumes it.
    fn hold_after_failure(&mut self, run: &RunId) {
        let Some(failed) = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == run && r.status == RunStatus::Failed)
        else {
            return;
        };
        if latest_executed_run(&self.state).map(|r| &r.id) != Some(run) {
            return;
        }
        let Some(class) = failure_class(&self.state, run) else {
            return;
        };
        if class == "validation_error" || class == "usage_limit" {
            return;
        }
        if self
            .state
            .queued_runs()
            .first()
            .is_some_and(|next| next.selection.instance == failed.selection.instance)
        {
            self.hold_queue();
        }
    }
    fn dispose_cohorts(&mut self) {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                task.app_owned()
                    && matches!(
                        task.delivery,
                        DeliveryState::Pending | DeliveryState::Claimed
                    )
            })
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        for id in tasks {
            self.fact(FactBody::TaskDeliveryChanged {
                id,
                state: DeliveryState::Disposed,
            });
        }
    }
    /// Cancelling a delegated completion delivery disposes its whole parent-run
    /// cohort, so siblings still running never wake the parent.
    fn cancel_queued_run(&mut self, run: &RunId) {
        let cohort = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == run)
            .and_then(|r| self.delivery_cohort(&r.message));
        match cohort {
            Some(cohort) => self.dispose_cohort(&cohort),
            None => self.fact(FactBody::RunFinished {
                id: run.clone(),
                status: RunStatus::Cancelled,
            }),
        }
    }
    /// The parent run whose delegated completions a message delivers.
    fn delivery_cohort(&self, message: &MessageId) -> Option<RunId> {
        let NotificationSource::Delegated { task_ids } =
            &self.state.message(message)?.notification.as_ref()?.source
        else {
            return None;
        };
        task_ids.iter().find_map(|id| {
            self.state
                .tasks
                .iter()
                .find(|task| &task.id == id)
                .and_then(|task| task.run.clone())
        })
    }
    /// Disposes the undelivered completions of a parent run and cancels its
    /// queued delivery.
    fn dispose_cohort(&mut self, cohort: &RunId) {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                task.app_owned()
                    && task.run.as_ref() == Some(cohort)
                    && matches!(
                        task.delivery,
                        DeliveryState::Pending | DeliveryState::Claimed
                    )
            })
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        for id in tasks {
            self.fact(FactBody::TaskDeliveryChanged {
                id,
                state: DeliveryState::Disposed,
            });
        }
        let deliveries = self
            .state
            .runs
            .iter()
            .filter(|run| {
                run.status == RunStatus::Queued
                    && self.delivery_cohort(&run.message).as_ref() == Some(cohort)
            })
            .map(|run| run.id.clone())
            .collect::<Vec<_>>();
        for id in deliveries {
            self.fact(FactBody::RunFinished {
                id,
                status: RunStatus::Cancelled,
            });
        }
    }
    fn hold_queue(&mut self) {
        let runs = self
            .state
            .queued_runs()
            .iter()
            .filter(|r| !r.queue_held)
            .map(|r| r.id.clone())
            .collect::<Vec<_>>();
        for id in runs {
            self.fact(FactBody::QueueHeld { id, held: true });
        }
    }
    /// Message-capable questions outlive their turn; the user answers them
    /// later. Retained background work keeps its row until it reports, as does
    /// a persistent Codex tool after a completed turn.
    fn close_attempt_items(&mut self, attempt: &RunAttemptId, status: ItemStatus) {
        let keeps_tools =
            status == ItemStatus::Completed && self.attempt_driver(attempt) == Some(Driver::Codex);
        let items=self.state.items.iter().filter(|i| i.attempt.as_ref()==Some(attempt) && !i.status.terminal() && !(keeps_tools && i.persistent_tool()) &&!self.state.background_work.values().any(|w| &w.attempt==attempt && (w.key==i.native_key || w.tool==i.native_key)) && !matches!(&i.kind,ItemKind::Subagent {task} if self.state.tasks.iter().any(|candidate|&candidate.id==task && !candidate.status.terminal())) && !matches!(&i.kind,ItemKind::UserInputRequest { request } if self.state.requests.iter().any(|r| &r.id==request && r.capability==ResponseCapability::Message))).map(|i| i.id.clone()).collect::<Vec<_>>();
        for id in items {
            self.fact(FactBody::ItemCompleted { id, status });
        }
        let requests = self
            .state
            .requests
            .iter()
            .filter(|r| {
                &r.attempt == attempt
                    && r.status == RequestStatus::Pending
                    && r.capability != ResponseCapability::Message
            })
            .map(|r| r.id.clone())
            .collect::<Vec<_>>();
        for id in requests {
            self.fact(FactBody::RequestResolved {
                id,
                status: RequestStatus::Cancelled,
                decision: None,
                answers: None,
                attachments: BTreeMap::new(),
            });
        }
    }
    /// Ends the provider's native subagents of an attempt. Delegated children
    /// run in their own threads and outlive the parent turn.
    fn stop_tasks(&mut self, attempt: &RunAttemptId, status: ItemStatus, confirmed: bool) {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| &task.attempt == attempt && !task.app_owned() && !task.status.terminal())
            .cloned()
            .collect::<Vec<_>>();
        for task in tasks {
            if confirmed {
                self.fact(FactBody::TaskFinished {
                    id: task.id.clone(),
                    status,
                    result: String::new(),
                });
            }
            self.effect(
                Some(attempt.clone()),
                EffectBody::SendToThread {
                    thread: task.child_thread,
                    command: Box::new(Command::Stop),
                },
            );
        }
    }
    fn finish_task(&mut self, id: &NodeId, status: ItemStatus, result: &str) {
        let Some(task) = self.state.tasks.iter().find(|t| &t.id == id).cloned() else {
            return;
        };
        let changed =
            task.status != status || !result.is_empty() && task.result.as_deref() != Some(result);
        if changed {
            self.fact(FactBody::TaskFinished {
                id: id.clone(),
                status,
                result: result.into(),
            });
        }
        if let Some(item) = self
            .state
            .items
            .iter()
            .find(|i| {
                matches!(&i.kind,ItemKind::Subagent { task } if task==id) && !i.status.terminal()
            })
            .map(|i| i.id.clone())
        {
            self.fact(FactBody::ItemCompleted { id: item, status });
        }
        if changed && task.background && !task.app_owned() && !task.status.terminal() {
            self.fact(FactBody::NativeWorkReported {
                key: id.to_string(),
                report: WorkReport {
                    kind: BackgroundKind::Subagent,
                    label: Some(task.title.unwrap_or(task.prompt)),
                    outcome: status.into(),
                    child_thread: Some(task.child_thread),
                    exit_code: None,
                },
                text: result.into(),
            });
        }
    }
    /// The provider handoff for a restart onto another instance: the thread's
    /// history through the running run, taken before the steer is recorded.
    fn restart_handoff(&self, target: &Run) -> HistoricalContext {
        let mut items = self
            .state
            .items
            .iter()
            .filter(|item| {
                item.run.as_ref().is_none_or(|id| {
                    self.state
                        .runs
                        .iter()
                        .any(|run| &run.id == id && run.status != RunStatus::RolledBack)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|a, b| (a.ordinal, &a.id).cmp(&(b.ordinal, &b.id)));
        prepare_history_covering(&self.state, &items, 1, target.ordinal)
    }
    /// Supersedes the running attempt and starts the run again with `message`.
    /// With a handoff the run moves to `selection`'s instance, and the session
    /// it leaves is detached.
    fn restart_run(
        &mut self,
        target: &Run,
        attempt: &RunAttemptId,
        selection: &ModelSelection,
        message: &MessageId,
        handoff: Option<HistoricalContext>,
    ) {
        let run = &target.id;
        // Native children keep running until their own terminal events.
        self.close_attempt_items(attempt, ItemStatus::Interrupted);
        self.fact(FactBody::AttemptFinished {
            id: attempt.clone(),
            status: AttemptStatus::Superseded,
        });
        self.interrupt_provider(attempt);
        if handoff.is_some() {
            self.effect(
                None,
                EffectBody::DetachSessions {
                    reason: "Provider thread handoff replaced this session binding.".into(),
                    revoke_credentials: false,
                    instance: Some(target.selection.instance.clone()),
                },
            );
        }
        self.fact(FactBody::RunRestarting {
            id: run.clone(),
            selection: selection.clone(),
        });
        let ordinal = self.state.attempts.iter().filter(|a| &a.run == run).count() as u64 + 1;
        let next = RunAttemptId::new(self.key("attempt", &format!("{}:{ordinal}", run))).unwrap();
        self.fact(FactBody::AttemptStarted {
            id: next.clone(),
            run: run.clone(),
            ordinal,
        });
        let message = self.state.message(message).unwrap().clone();
        let Some(history) = handoff else {
            let t = self.state.thread.as_ref().unwrap();
            let command = ProviderCommand::Start {
                resume_interrupted_turn: false,
                selection: selection.clone(),
                runtime_mode: t.runtime_mode,
                interaction_mode: t.interaction_mode,
                text: project_context_for_provider(&message.text, message.context.as_ref()),
                note: None,
                attachments: message.attachments,
                native_thread: self.state.native_sessions.get(&selection.instance).cloned(),
                resume_at: None,
                context: None,
            };
            self.effect(Some(next), EffectBody::Provider(command));
            return;
        };
        let thread = self.state.thread.as_ref().unwrap().id.clone();
        self.fact(FactBody::TransferOpened {
            native_source: None,
            target_run: None,
            id: ContextTransferId::new(self.key("restart-handoff", run.as_str())).unwrap(),
            kind: TransferKind::ProviderHandoff,
            source: thread.clone(),
            target: thread,
            boundary: target.ordinal,
            instance: Some(selection.instance.clone()),
            history,
        });
        let restarted = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == run)
            .unwrap()
            .clone();
        let native_thread = self.start_native_thread(&restarted);
        self.send_start(&restarted, next, message, native_thread, false);
    }
    fn interrupt_provider(&mut self, attempt: &RunAttemptId) {
        let owner = self.state.attempts.iter().find(|a| &a.id == attempt);
        let (native_thread, native_turn) = if self.state.native_owner.as_ref() == Some(attempt) {
            (
                self.state.native_child_thread.clone(),
                self.state.native_child_turn.clone(),
            )
        } else {
            (
                owner.and_then(|a| a.native_thread.clone()),
                owner.and_then(|a| a.native_turn.clone()),
            )
        };
        self.effect(
            Some(attempt.clone()),
            EffectBody::Provider(ProviderCommand::Interrupt {
                native_thread,
                native_turn,
            }),
        );
    }
    /// The run's interrupt request and, once known, its result.
    fn interrupt_item(
        &mut self,
        run: &RunId,
        attempt: Option<&RunAttemptId>,
        request: &str,
        result: Option<(ItemStatus, &str)>,
    ) {
        let key = format!("interrupt-request:{}:{run}", run.as_str().len());
        let request_id = TurnItemId::new(key.clone()).unwrap();
        if !self.state.items.iter().any(|item| item.id == request_id) {
            self.item_start(
                request_id.clone(),
                Some(run.clone()),
                attempt.cloned(),
                key,
                ItemKind::RunInterruptRequest,
            );
            self.append_text(&request_id, request);
            self.fact(FactBody::ItemCompleted {
                id: request_id.clone(),
                status: ItemStatus::Completed,
            });
        }
        if let Some((status, text)) = result {
            let key = format!("interrupt-result:{}:{run}", run.as_str().len());
            let result = TurnItemId::new(key.clone()).unwrap();
            if !self.state.items.iter().any(|item| item.id == result) {
                self.item_start(
                    result.clone(),
                    Some(run.clone()),
                    attempt.cloned(),
                    key,
                    ItemKind::RunInterruptResult {
                        request: request_id,
                    },
                );
                self.append_text(&result, text);
                self.fact(FactBody::ItemCompleted { id: result, status });
            }
        }
    }
    /// `None` when the limit recovery may resume the run now.
    fn usage_limit_continuation(
        &mut self,
        command: &CommandId,
        run: &RunId,
        request: Option<&CommandId>,
    ) -> Option<Reply> {
        let thread = self.state.thread.as_ref().unwrap().clone();
        let failure = usage_limit_failure(&self.state);
        let recovery = thread.limit_recovery.clone();
        let snoozed = thread
            .snoozed_until
            .as_ref()
            .is_some_and(|until| until > &self.at);
        let resumable = failure.as_ref().zip(recovery.as_ref()).is_some_and(
            |((limited, reset_at), recovery)| {
                &limited.id == run
                    && recovery.auto_resume
                    && recovery.request.as_ref() == request
                    && &recovery.run == run
                    && Some(&recovery.reset_at) == reset_at.as_ref()
                    && recovery.reset_at <= self.at
                    && thread.selection.instance == limited.selection.instance
            },
        ) && thread.archived_at.is_none()
            && thread.settled != Some(true)
            && !self
                .state
                .requests
                .iter()
                .any(|r| r.status == RequestStatus::Pending)
            && !snoozed;
        if resumable {
            return None;
        }
        // A resume that came while still snoozed is re-armed under its own id.
        if let Some(mut recovery) = recovery.filter(|recovery| {
            recovery.auto_resume
                && recovery.request.as_ref() == request
                && &recovery.run == run
                && snoozed
        }) {
            recovery.request = Some(command.clone());
            self.fact(FactBody::LimitRecoveryChanged {
                recovery: Some(recovery),
            });
        }
        Some(Reply::Ignored)
    }
    /// The usage-limit reset for a failure of `attempt`: Claude's latest
    /// rejected window when every one reports a reset, Codex's account snapshot.
    fn usage_limit_reset(&self, attempt: &RunAttemptId) -> Option<Timestamp> {
        let record = self.state.attempts.iter().find(|a| &a.id == attempt)?;
        let run = self.state.runs.iter().find(|run| run.id == record.run)?;
        let seconds = match run.selection.driver {
            Driver::Claude => {
                let resets = record
                    .rejected_limits
                    .values()
                    .copied()
                    .collect::<Option<Vec<_>>>()?;
                resets.into_iter().max()?
            }
            Driver::Codex => self
                .state
                .rate_limit_resets
                .get(&run.selection.instance)
                .copied()
                .flatten()?,
        };
        Timestamp::from_millis(seconds.saturating_mul(1000)).ok()
    }
    fn source_run(&self, source: &SourcePoint) -> Option<&Run> {
        match source {
            SourcePoint::LatestStable => latest_stable_run(&self.state),
            SourcePoint::Run(id) => self.state.runs.iter().find(|run| &run.id == id),
            SourcePoint::Checkpoint(id) => self
                .state
                .checkpoints
                .iter()
                .find(|checkpoint| &checkpoint.id == id)
                .and_then(|checkpoint| checkpoint.run.as_ref())
                .and_then(|run| {
                    self.state
                        .runs
                        .iter()
                        .find(|candidate| &candidate.id == run)
                }),
        }
    }
    /// The fork transfer no direct turn has consumed yet.
    fn pending_fork(&self) -> Option<&Transfer> {
        self.state.transfers.iter().find(|transfer| {
            transfer.kind == TransferKind::Fork
                && !transfer.superseded
                && transfer.delivery.is_none()
        })
    }
    /// Releases the sessions of every instance but `kept` when the thread moves
    /// to another instance.
    fn release_other_instances(&mut self, kept: &str) {
        for instance in self
            .used_instances(None)
            .into_iter()
            .filter(|instance| instance != kept)
        {
            self.effect(
                None,
                EffectBody::DetachSessions {
                    reason: "Provider or model selection changed.".into(),
                    revoke_credentials: false,
                    instance: Some(instance),
                },
            );
        }
    }
    /// The driver of the attempt's run, or of the thread for a native child turn.
    fn attempt_driver(&self, attempt: &RunAttemptId) -> Option<Driver> {
        self.state
            .runs
            .iter()
            .find(|run| run.attempt.as_ref() == Some(attempt))
            .map(|run| run.selection.driver)
            .or_else(|| {
                self.state
                    .thread
                    .as_ref()
                    .map(|thread| thread.selection.driver)
            })
    }
    /// The starting run waiting on a native fork from `attempt`.
    fn awaiting_fork(&self, attempt: &RunAttemptId) -> Option<(Run, Transfer)> {
        let run = self.state.runs.iter().find(|run| {
            run.attempt.as_ref() == Some(attempt) && run.status == RunStatus::Starting
        })?;
        let transfer = self.pending_fork()?;
        transfer
            .native_source
            .as_ref()
            .filter(|source| source.instance == run.selection.instance)?;
        Some((run.clone(), transfer.clone()))
    }
    /// Provider instances this thread has run or holds a native session for,
    /// optionally only those of one driver.
    fn used_instances(&self, driver: Option<Driver>) -> BTreeSet<String> {
        let driver_of = |instance: &str| {
            self.state
                .runs
                .iter()
                .rev()
                .map(|run| &run.selection)
                .chain(self.state.thread.as_ref().map(|thread| &thread.selection))
                .find(|selection| selection.instance == instance)
                .map(|selection| selection.driver)
        };
        self.state
            .runs
            .iter()
            .map(|run| run.selection.instance.clone())
            .chain(self.state.native_sessions.keys().cloned())
            .filter(|instance| driver.is_none_or(|driver| driver_of(instance) == Some(driver)))
            .collect()
    }
    fn preparation_item(&self, run: &RunId) -> Option<Item> {
        self.state
            .items
            .iter()
            .find(|item| {
                item.run.as_ref() == Some(run)
                    && matches!(&item.kind, ItemKind::CommandExecution { command, .. }
                        if command == WORKSPACE_PREPARATION_INPUT)
            })
            .cloned()
    }
    /// Settles a running preparation row with its title, output and exit code.
    fn end_preparation(
        &mut self,
        run: &RunId,
        status: ItemStatus,
        title: &str,
        output: &str,
        exit_code: Option<i64>,
    ) {
        let Some(item) = self
            .preparation_item(run)
            .filter(|item| !item.status.terminal())
        else {
            return;
        };
        self.fact(FactBody::ItemDetailChanged {
            id: item.id.clone(),
            kind: preparation_kind(title, exit_code),
        });
        self.replace_text(&item.id, output);
        self.fact(FactBody::ItemCompleted {
            id: item.id,
            status,
        });
    }
    fn complete_delegation(&mut self, run: &RunId, status: RunStatus) {
        let Some(origin) = self.state.delegation.clone() else {
            return;
        };
        let record = self
            .state
            .runs
            .iter()
            .find(|candidate| &candidate.id == run)
            .unwrap();
        if original_run(record, &self.state.runs).message != origin.message {
            return;
        }
        let items = self
            .state
            .items
            .iter()
            .filter(|item| item.run.as_ref() == Some(run))
            .cloned()
            .collect::<Vec<_>>();
        let result = delegated_result(record, &self.state.items, &self.state.messages);
        let history = prepare_history(&self.state, &items, record.ordinal);
        self.effect(
            record.attempt.clone(),
            EffectBody::SendToThread {
                thread: origin.parent,
                command: Box::new(Command::TaskResult {
                    source_message: Some(origin.message),
                    generation: None,
                    context: Some(TaskResultContext {
                        boundary: record.ordinal,
                        history,
                    }),
                    task: origin.task,
                    status: match status {
                        RunStatus::Completed => ItemStatus::Completed,
                        RunStatus::Interrupted => ItemStatus::Interrupted,
                        RunStatus::Cancelled => ItemStatus::Cancelled,
                        _ => ItemStatus::Failed,
                    },
                    result,
                }),
            },
        );
    }
    fn finish(&mut self, run: &RunId, status: RunStatus, capture: bool) {
        let r = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == run)
            .unwrap()
            .clone();
        if let Some(attempt) = &r.attempt {
            let (a, i) = match status {
                RunStatus::Completed => (AttemptStatus::Completed, ItemStatus::Completed),
                RunStatus::Interrupted => (AttemptStatus::Interrupted, ItemStatus::Interrupted),
                RunStatus::Failed => (AttemptStatus::Failed, ItemStatus::Failed),
                _ => (AttemptStatus::Cancelled, ItemStatus::Cancelled),
            };
            if r.selection.driver == Driver::Codex {
                let a = self
                    .state
                    .attempts
                    .iter()
                    .find(|a| &a.id == attempt)
                    .unwrap();
                let usage = complete_codex_usage(
                    a.usage_accumulator.as_ref(),
                    a.usage_observed,
                    status == RunStatus::Completed,
                    self.state.tasks.iter().any(|task| &task.attempt == attempt),
                );
                self.fact(FactBody::TurnUsageRecorded {
                    attempt: attempt.clone(),
                    usage,
                });
            }
            if r.selection.driver == Driver::Claude
                && self
                    .state
                    .attempts
                    .iter()
                    .find(|a| &a.id == attempt)
                    .unwrap()
                    .turn_usage
                    .is_none()
            {
                self.fact(FactBody::TurnUsageRecorded {
                    attempt: attempt.clone(),
                    usage: TurnTokenUsage::unavailable(
                        self.state.tasks.iter().any(|task| &task.attempt == attempt),
                    ),
                });
            }
            self.fact(FactBody::AttemptFinished {
                id: attempt.clone(),
                status: a,
            });
            self.close_attempt_items(attempt, i);
            if self.state.stopping.contains(attempt) {
                self.interrupt_item(
                    run,
                    Some(attempt),
                    INTERRUPT_REQUESTED,
                    Some((i, "Run interrupted by user")),
                );
            }
        }
        if let Some(scope) = r.checkpoint_scope.filter(|_| capture) {
            self.fact(FactBody::RunWaitingForCapture {
                id: run.clone(),
                terminal: status,
            });
            self.effect(
                r.attempt,
                EffectBody::CaptureCheckpoint {
                    run: run.clone(),
                    scope,
                    native_baseline_heads: r.native_baseline_heads,
                },
            );
        } else {
            self.fact(FactBody::RunFinished {
                id: run.clone(),
                status,
            });
            self.complete_delegation(run, status);
            self.settle_deliveries(run);
            self.hold_after_failure(run);
            self.promote();
            self.wake_tasks();
        }
    }
    fn create_run(&mut self, message: &SendMessage) -> Reply {
        if self.state.native_parent.is_some() {
            return reject(
                "This subagent is run by its provider and cannot take messages. Message the parent thread instead.",
            );
        }
        let Some(thread) = self.state.thread.as_ref() else {
            return reject("thread-not-found");
        };
        if self.state.messages.iter().any(|m| m.id == message.id) {
            return reject("message-id-conflict");
        }
        if let Err(reason) = validate_attachments(&message.attachments) {
            return reject(reason);
        }
        let context = match message.context.as_ref().map(MessageContext::normalized) {
            Some(None) => return reject("invalid-message-context"),
            context => context.flatten(),
        };
        let provider_text = project_context_for_provider(&message.text, context.as_ref());
        if let Some(source) = &message.source_plan {
            // Another thread's plan is read by the Host before dispatch.
            let found = if source.thread == thread.id {
                self.state
                    .plans
                    .iter()
                    .find(|p| p.id == source.plan)
                    .map(|plan| ResolvedPlan {
                        project: thread.project.clone(),
                        kind: plan.kind,
                        implemented: plan.implemented_by.is_some(),
                    })
            } else {
                message.resolved_plan.clone()
            };
            let Some(found) = found.filter(|plan| plan.kind == PlanKind::Proposed) else {
                return reject("plan-not-found");
            };
            if found.project != thread.project {
                return reject("plan-in-another-project");
            }
            if found.implemented {
                return reject("plan-not-active");
            }
        }
        if let Some(Continuation::Manual { run }) = &message.continuation {
            let source = self.state.runs.iter().find(|r| &r.id == run);
            let resumable = source.is_some_and(|source| {
                source.status == RunStatus::Interrupted
                    || source.status == RunStatus::Failed
                        && failure_class(&self.state, &source.id).as_deref() == Some("usage_limit")
            });
            if !matches!(message.mode, DispatchMode::StartImmediately)
                || !resumable
                || latest_executed_run(&self.state).map(|r| &r.id) != Some(run)
                || thread.archived_at.is_some()
                || self
                    .state
                    .requests
                    .iter()
                    .any(|r| r.status == RequestStatus::Pending)
            {
                return reject("continuation-unavailable");
            }
        }
        let selection = message
            .selection
            .clone()
            .unwrap_or_else(|| thread.selection.clone());
        // A merge-back is not consumed from the queue, and one direct turn
        // takes merge-backs from a single fork.
        let merge_backs = self
            .state
            .transfers
            .iter()
            .filter(|t| t.kind == TransferKind::MergeBack && !t.superseded && t.delivery.is_none())
            .map(|t| &t.source)
            .collect::<BTreeSet<_>>();
        if !merge_backs.is_empty() {
            let active = self.state.active_run();
            let steers = matches!(
                resolve_dispatch(
                    active.map(|r| (&r.id, r.status)),
                    &message.mode,
                    message.intent,
                    TurnSupport::for_driver(selection.driver),
                ),
                DispatchMode::SteerActive { run } | DispatchMode::RestartActive { run }
                    if self.state.runs.iter().any(|r| r.id == run && r.status == RunStatus::Running)
            );
            if !steers && (active.is_some() || !self.state.captures.is_empty()) {
                return reject("merge-back-pending");
            }
            if !steers && merge_backs.len() > 1 {
                return reject("merge-backs-from-multiple-forks");
            }
        }
        let thread_selection = thread.selection.clone();
        let settled = thread.settled.is_some();
        let earlier_users = self
            .state
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .collect::<Vec<_>>();
        let title_armed = maintenance(&message.text, &message.attachments).is_none()
            && (message.title_seed.is_some() && self.state.messages.is_empty()
                || message.created_by == MessageAuthor::User
                    && !earlier_users.is_empty()
                    && earlier_users
                        .iter()
                        .all(|m| maintenance(&m.text, &m.attachments).is_some()));
        if settled {
            self.fact(FactBody::ThreadUnsettled);
        }
        if title_armed {
            if let Some(seed) = &message.title_seed {
                self.fact(FactBody::ThreadRenamed {
                    title: seed.clone(),
                });
            }
            let request = CommandId::new(format!("title:{}", message.id)).unwrap();
            self.fact(FactBody::TitleRequested {
                request: request.clone(),
            });
            self.effect(
                None,
                EffectBody::GenerateTitle {
                    request,
                    message: Some(message.id.clone()),
                },
            );
        }
        if self
            .state
            .thread
            .as_ref()
            .is_some_and(|thread| thread.snoozed_until.is_some())
        {
            self.fact(FactBody::ThreadSnoozed { until: None });
        }
        let active = self.state.active_run();
        let support = TurnSupport::for_driver(selection.driver);
        let mut mode = resolve_dispatch(
            active.map(|r| (&r.id, r.status)),
            &message.mode,
            message.intent,
            support,
        );
        // A steer that missed its turn is kept as a new turn.
        if let DispatchMode::SteerActive { run } = &mode
            && self.state.runs.iter().any(|r| {
                &r.id == run && matches!(r.status, RunStatus::Completed | RunStatus::Waiting)
            })
        {
            mode = DispatchMode::StartImmediately;
        }
        if let DispatchMode::SteerActive { run } | DispatchMode::RestartActive { run } = &mode {
            let Some(target) = self
                .state
                .runs
                .iter()
                .find(|r| &r.id == run && r.status == RunStatus::Running)
                .cloned()
            else {
                return reject("no-running-provider-turn");
            };
            let Some(attempt) = target.attempt.clone() else {
                return reject("no-running-provider-turn");
            };
            if maintenance(&message.text, &message.attachments).is_some() {
                return reject("maintenance-must-run-separately");
            }
            if self
                .state
                .message(&target.message)
                .is_some_and(|m| maintenance(&m.text, &m.attachments).is_some())
            {
                return reject("maintenance-in-progress");
            }
            // The running session decides how it can be steered;
            // another instance can only take over by restarting the run.
            let support = TurnSupport::for_driver(target.selection.driver);
            let moves = selection.instance != target.selection.instance;
            let restart = moves
                || match mode {
                    DispatchMode::RestartActive { .. } => true,
                    _ => !support.steer,
                };
            if restart && !(support.interrupt && support.restart) {
                return reject("restart-unsupported");
            }
            let handoff = moves.then(|| self.restart_handoff(&target));
            if selection != thread_selection {
                self.fact(FactBody::ModelSelected {
                    selection: selection.clone(),
                });
            }
            self.fact(FactBody::MessageCreated {
                id: message.id.clone(),
                run: Some(run.clone()),
                role: Role::User,
                text: message.text.clone(),
                attachments: message.attachments.clone(),
                intent: InputIntent::Steer,
                created_by: message.created_by,
                creation_source: message.creation_source.clone(),
                context: context.clone(),
            });
            self.user_item(&message.id, run);
            if restart {
                self.restart_run(&target, &attempt, &selection, &message.id, handoff);
            } else {
                self.effect(
                    Some(attempt),
                    EffectBody::Provider(ProviderCommand::Steer {
                        message: message.id.clone(),
                        text: provider_text,
                        attachments: message.attachments.clone(),
                    }),
                );
            }
            return Reply::Run(run.clone());
        }
        let held = self.state.queued_runs().iter().any(|r| r.queue_held);
        let queued =
            active.is_some() || !self.state.captures.is_empty() || self.state.rollback.is_some();
        // A deferred start is queued behind an active run without preparation.
        let deferred = matches!(mode, DispatchMode::DeferStart) && !queued;
        let id = RunId::new(format!("run:{}:{}", message.id.as_str().len(), message.id)).unwrap();
        let ordinal = self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0) + 1;
        let intent = if queued {
            InputIntent::QueuedTurn
        } else {
            InputIntent::TurnStart
        };
        if !queued && selection != thread_selection {
            self.fact(FactBody::ModelSelected {
                selection: selection.clone(),
            });
        }
        self.fact(FactBody::MessageCreated {
            id: message.id.clone(),
            run: Some(id.clone()),
            role: Role::User,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            intent,
            created_by: message.created_by,
            creation_source: message.creation_source.clone(),
            context,
        });
        self.fact(FactBody::RunRequested {
            id: id.clone(),
            message: message.id.clone(),
            ordinal,
            selection,
            status: if deferred {
                RunStatus::Preparing
            } else if queued {
                RunStatus::Queued
            } else {
                RunStatus::Starting
            },
            queue_position: queued.then(|| self.state.queued_runs().len() as u64 + 1),
            held,
            source_plan: message.source_plan.clone(),
        });
        if let Some(source) = &message.source_plan {
            if source.thread == self.state.thread.as_ref().unwrap().id {
                self.fact(FactBody::PlanImplemented {
                    id: source.plan.clone(),
                    run: id.clone(),
                });
            } else {
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: source.thread.clone(),
                        command: Box::new(Command::ImplementPlan {
                            plan: source.plan.clone(),
                            run: id.clone(),
                        }),
                    },
                );
            }
        }
        if deferred {
            self.user_item(&message.id, &id);
            let key = format!("workspace-preparation:{}:{id}", id.as_str().len());
            self.item_start(
                TurnItemId::new(key.clone()).unwrap(),
                Some(id.clone()),
                None,
                key,
                preparation_kind(WORKSPACE_PREPARATION_INPUT, None),
            );
            self.effect(None, EffectBody::PrepareWorkspace { run: id.clone() });
        } else if !queued {
            self.start_run(&id);
        }
        Reply::Run(id)
    }
    fn command(&mut self, id: &CommandId, command: &Command) -> Reply {
        use Command::*;
        if !matches!(
            command,
            Create { .. } | Import { .. } | AcceptFork { .. } | AcceptDelegation { .. }
        ) {
            let Some(thread) = &self.state.thread else {
                return reject("thread-not-found");
            };
            if thread.deleted_at.is_some() && !matches!(command, Delete) {
                return reject("thread-deleted");
            }
        }
        // Commands are accepted while a rollback runs: new messages wait behind it
        // and the rollback checks the active provider when it executes. Its
        // results carry only the rollback identity and never overwrite metadata.
        if self.state.rollback.is_some()
            && matches!(
                command,
                ContinueRestart { .. }
                    | ReleasePrepared { .. }
                    | RetryPrepared { .. }
                    | Rollback { .. }
            )
        {
            return reject("rollback-pending");
        }
        match command {
            ContinueRestart { source, enabled } => {
                let Some(source) = self
                    .state
                    .runs
                    .iter()
                    .find(|run| &run.id == source)
                    .cloned()
                else {
                    return reject("run-not-found");
                };
                if self
                    .state
                    .runs
                    .iter()
                    .any(|run| run.restart_of.as_ref() == Some(&source.id))
                {
                    return Reply::Ignored;
                }
                let thread = self.state.thread.as_ref().unwrap();
                let declined = !enabled
                    || source.status != RunStatus::Cancelled
                    || !source.restart_cancelled_work.is_empty()
                        && source.attempt.as_ref().is_some_and(|id| {
                            self.state.attempts.iter().any(|attempt| {
                                &attempt.id == id && attempt.status == AttemptStatus::Completed
                            })
                        })
                    || self.state.active_run().is_some()
                    || thread.archived_at.is_some()
                    || thread.selection.instance != source.selection.instance
                    || source
                        .attempt
                        .as_ref()
                        .is_some_and(|attempt| self.state.stopping.contains(attempt))
                    || self.state.messages.iter().any(|message| {
                        message.id == source.message
                            && matches!(
                                message.text.trim().to_ascii_lowercase().as_str(),
                                "/compact" | "/logout"
                            )
                    })
                    || self.state.runs.iter().any(|run| {
                        run.id != source.id
                            && run.status != RunStatus::Queued
                            && run_ran_after(run, &source)
                    });
                if declined {
                    self.complete_delegation(&source.id, source.status);
                    return Reply::Ignored;
                }
                let work =
                    restart_continuation_work(&source, &self.state.runs, &self.state.attempts);
                let text = if work.is_empty() {
                    "Continue where you left off.".into()
                } else {
                    format!(
                        "{}\n\nContinue where you left off.",
                        restart_background_note(&work)
                    )
                };
                let message =
                    MessageId::new(self.key("restart-message", source.id.as_str())).unwrap();
                let run = RunId::new(self.key("restart-run", source.id.as_str())).unwrap();
                self.fact(FactBody::MessageCreated {
                    id: message.clone(),
                    run: Some(run.clone()),
                    role: Role::User,
                    text,
                    attachments: vec![],
                    intent: InputIntent::TurnStart,
                    created_by: MessageAuthor::Agent,
                    creation_source: "server".into(),
                    context: None,
                });
                self.fact(FactBody::RunRequested {
                    id: run.clone(),
                    message,
                    ordinal: self
                        .state
                        .runs
                        .iter()
                        .map(|run| run.ordinal)
                        .max()
                        .unwrap_or(0)
                        + 1,
                    selection: source.selection,
                    status: RunStatus::Starting,
                    queue_position: None,
                    held: false,
                    source_plan: None,
                });
                self.fact(FactBody::RestartContinuationLinked {
                    run: run.clone(),
                    source: source.id,
                });
                self.start_run(&run);
                Reply::Run(run)
            }
            RecordCreatedThread {
                run,
                thread,
                project,
                target_run,
                title,
                selection,
            } => {
                let Some(parent) = self.state.runs.iter().find(|r| &r.id == run).cloned() else {
                    return reject("run-not-found");
                };
                if &self.state.thread.as_ref().unwrap().project != project {
                    return reject("thread-in-another-project");
                }
                let item = TurnItemId::new(format!("turn-item:created-thread:{id}")).unwrap();
                if self.state.items.iter().any(|i| i.id == item) {
                    return Reply::Ignored;
                }
                self.item_start(
                    item.clone(),
                    Some(parent.id.clone()),
                    parent.attempt.clone(),
                    item.to_string(),
                    ItemKind::ThreadCreated {
                        thread: thread.clone(),
                        run: target_run.clone(),
                        title: title.clone(),
                        instance: selection.instance.clone(),
                        model: selection.model.clone(),
                    },
                );
                self.fact(FactBody::ItemCompleted {
                    id: item,
                    status: ItemStatus::Completed,
                });
                Reply::Accepted
            }
            Stop => {
                if let Some(run) = self.state.active_run().map(|r| r.id.clone()) {
                    return self.command(
                        id,
                        &Interrupt {
                            run,
                            hold_queue: true,
                            reason: None,
                        },
                    );
                }
                if let Some(run) = self
                    .state
                    .runs
                    .iter()
                    .rev()
                    .find(|run| {
                        self.state.tasks.iter().any(|task| {
                            task.run.as_ref() == Some(&run.id) && !task.status.terminal()
                        }) || self
                            .state
                            .background_work
                            .values()
                            .any(|task| run.attempt.as_ref() == Some(&task.attempt))
                    })
                    .map(|run| run.id.clone())
                {
                    return self.command(
                        id,
                        &Interrupt {
                            run,
                            hold_queue: true,
                            reason: None,
                        },
                    );
                }
                if let Some(owner) = self.state.native_owner.clone() {
                    self.stop_tasks(&owner, ItemStatus::Interrupted, false);
                    self.fact(FactBody::StopRequested {
                        attempt: owner.clone(),
                    });
                    self.interrupt_provider(&owner);
                }
                Reply::Accepted
            }
            BindNativeChild {
                native_thread,
                owner,
                parent,
                task,
                generation,
            } => {
                self.fact(FactBody::NativeChildBound {
                    native_thread: native_thread.clone(),
                    owner: owner.clone(),
                    parent: parent.clone(),
                    task: task.clone(),
                    generation: *generation,
                });
                Reply::Accepted
            }
            NativeInput { attempt, event } => self.provider(attempt, event),
            Create {
                thread,
                project,
                title,
                selection,
                runtime_mode,
                interaction_mode,
                workspace,
                created_by,
                creation_source,
            } => {
                if self.state.thread.is_some() {
                    return reject("thread-already-exists");
                }
                self.fact(FactBody::ThreadCreated {
                    id: thread.clone(),
                    project: project.clone(),
                    title: title.clone(),
                    selection: selection.clone(),
                    runtime_mode: *runtime_mode,
                    interaction_mode: *interaction_mode,
                    created_by: *created_by,
                    creation_source: creation_source.clone(),
                });
                if workspace.is_some() {
                    self.fact(FactBody::WorkspaceBound {
                        workspace: workspace.clone(),
                    });
                }
                Reply::Thread(thread.clone())
            }
            Rename { title } => {
                if title.trim().is_empty() {
                    return reject("title-required");
                }
                self.fact(FactBody::ThreadRenamed {
                    title: title.trim().to_owned(),
                });
                Reply::Accepted
            }
            RegenerateTitle => {
                self.fact(FactBody::TitleRequested {
                    request: id.clone(),
                });
                self.effect(
                    None,
                    EffectBody::GenerateTitle {
                        request: id.clone(),
                        message: None,
                    },
                );
                Reply::Accepted
            }
            Import {
                thread,
                project,
                title,
                selection,
                workspace,
                created_at,
                updated_at,
                messages,
                native,
            } => {
                if let Some(existing) = &self.state.thread {
                    return if existing.imported {
                        Reply::Ignored
                    } else {
                        reject("thread-has-activity")
                    };
                }
                let title = match title.trim() {
                    "" => "Untitled thread".to_owned(),
                    title => title.to_owned(),
                };
                self.fact_at(
                    created_at.clone(),
                    FactBody::ThreadCreated {
                        id: thread.clone(),
                        project: project.clone(),
                        title,
                        selection: selection.clone(),
                        runtime_mode: crate::RuntimeMode::FullAccess,
                        interaction_mode: crate::InteractionMode::Default,
                        created_by: MessageAuthor::User,
                        creation_source: "provider".into(),
                    },
                );
                self.fact_at(created_at.clone(), FactBody::ThreadImported);
                if workspace.is_some() {
                    self.fact_at(
                        created_at.clone(),
                        FactBody::WorkspaceBound {
                            workspace: workspace.clone(),
                        },
                    );
                }
                for (index, imported) in messages.iter().enumerate() {
                    let suffix = format!("{index:06}");
                    let message = MessageId::new(format!("{thread}:{suffix}")).unwrap();
                    let item = TurnItemId::new(format!("import:{thread}:{suffix}")).unwrap();
                    let at = imported.at.clone();
                    self.fact_at(
                        at.clone(),
                        FactBody::MessageCreated {
                            id: message.clone(),
                            run: None,
                            role: imported.role,
                            text: if imported.role == Role::Assistant {
                                String::new()
                            } else {
                                imported.text.clone()
                            },
                            attachments: vec![],
                            intent: InputIntent::TurnStart,
                            created_by: if imported.role == Role::User {
                                MessageAuthor::User
                            } else {
                                MessageAuthor::Agent
                            },
                            creation_source: "server".into(),
                            context: None,
                        },
                    );
                    self.fact_at(
                        at.clone(),
                        FactBody::ItemStarted {
                            id: item.clone(),
                            run: None,
                            attempt: None,
                            native_key: message.to_string(),
                            ordinal: index as u64 + 1,
                            kind: if imported.role == Role::User {
                                ItemKind::UserMessage {
                                    message: message.clone(),
                                }
                            } else {
                                ItemKind::AssistantMessage {
                                    message: message.clone(),
                                }
                            },
                        },
                    );
                    self.append_text_at(at.clone(), &item, &imported.text);
                    self.fact_at(
                        at.clone(),
                        FactBody::ItemCompleted {
                            id: item,
                            status: ItemStatus::Completed,
                        },
                    );
                    self.fact_at(at, FactBody::MessageFinished { id: message });
                }
                self.fact_at(
                    updated_at.clone(),
                    FactBody::ThreadSettled {
                        settled: true,
                        at: updated_at.clone(),
                    },
                );
                self.fact_at(
                    updated_at.clone(),
                    FactBody::NativeSessionBound {
                        instance: native.instance.clone(),
                        native_thread: native.thread.clone(),
                        head: native.head.clone(),
                    },
                );
                Reply::Thread(thread.clone())
            }
            Archive { archived } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() == *archived {
                    return reject(if *archived {
                        "thread-already-archived"
                    } else {
                        "thread-not-archived"
                    });
                }
                self.fact(FactBody::ThreadArchived {
                    archived: *archived,
                });
                if *archived && thread_title_pending(&self.state) {
                    self.fact(FactBody::TitleRequestCleared);
                }
                if *archived {
                    let queued = self
                        .state
                        .runs
                        .iter()
                        .filter(|r| r.status == RunStatus::Queued)
                        .map(|r| r.id.clone())
                        .collect::<Vec<_>>();
                    for run in queued {
                        self.fact(FactBody::RunFinished {
                            id: run,
                            status: RunStatus::Cancelled,
                        });
                    }
                    self.dispose_cohorts();
                    self.effect(
                        None,
                        EffectBody::DetachSessions {
                            reason: "Thread archived.".into(),
                            revoke_credentials: true,
                            instance: None,
                        },
                    );
                    self.effect(None, EffectBody::CleanupTerminals);
                }
                Reply::Accepted
            }
            Delete => {
                let paths = self
                    .state
                    .messages
                    .iter()
                    .flat_map(|m| &m.attachments)
                    .map(|a| a.path.clone())
                    .collect::<Vec<_>>();
                // Deleting again repeats only the cleanup.
                if self
                    .state
                    .thread
                    .as_ref()
                    .is_some_and(|thread| thread.deleted_at.is_some())
                {
                    self.effect(None, EffectBody::CleanupTerminals);
                    self.effect(None, EffectBody::DeleteAttachments { paths });
                    return Reply::Accepted;
                }
                let runs = self
                    .state
                    .runs
                    .iter()
                    .filter(|r| r.status.blocking() || r.status == RunStatus::Queued)
                    .cloned()
                    .collect::<Vec<_>>();
                self.hold_queue();
                // Detaching the sessions below ends provider work; delegated
                // children keep running in their own threads.
                for run in runs {
                    if let Some(a) = &run.attempt {
                        self.stop_tasks(a, ItemStatus::Cancelled, true);
                    }
                    self.finish(&run.id, RunStatus::Cancelled, false);
                }
                if let Some(owner) = self.state.native_owner.clone() {
                    self.provider(
                        &owner,
                        &ProviderEvent::TurnFinished {
                            status: RunStatus::Cancelled,
                            native_head: None,
                        },
                    );
                }
                let requests = self
                    .state
                    .requests
                    .iter()
                    .filter(|r| r.status == RequestStatus::Pending)
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                for id in requests {
                    self.resolve_request(
                        &id,
                        RequestStatus::Cancelled,
                        None,
                        ItemStatus::Cancelled,
                    );
                }
                self.dispose_cohorts();
                self.fact(FactBody::ThreadDeleted);
                self.effect(
                    None,
                    EffectBody::DetachSessions {
                        reason: "Thread deleted.".into(),
                        revoke_credentials: true,
                        instance: None,
                    },
                );
                self.effect(None, EffectBody::CleanupTerminals);
                self.effect(None, EffectBody::DeleteAttachments { paths });
                Reply::Accepted
            }
            Settle { settled, at } => {
                let thread = self.state.thread.as_ref().unwrap().clone();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                if !settled {
                    if thread.settled != Some(false) {
                        self.fact(FactBody::ThreadSettled {
                            settled: false,
                            at: self.at.clone(),
                        });
                    }
                    return Reply::Accepted;
                }
                let automatic = |run: &Run| {
                    run.status == RunStatus::Queued
                        && self
                            .state
                            .message(&run.message)
                            .is_some_and(|m| m.notification.is_some())
                };
                let message_question = |request: &Request| {
                    request.capability == ResponseCapability::Message
                        && matches!(request.body, RequestBody::Questions { .. })
                };
                if self.state.runs.iter().any(|run| {
                    (run.status.blocking() || run.status == RunStatus::Queued) && !automatic(run)
                }) || self
                    .state
                    .requests
                    .iter()
                    .any(|r| r.status == RequestStatus::Pending && !message_question(r))
                {
                    return reject("thread-has-active-work");
                }
                let wakes = self
                    .state
                    .runs
                    .iter()
                    .filter(|run| automatic(run))
                    .map(|run| run.id.clone())
                    .collect::<Vec<_>>();
                let questions = self
                    .state
                    .requests
                    .iter()
                    .filter(|r| r.status == RequestStatus::Pending)
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                for id in questions {
                    self.resolve_request(
                        &id,
                        RequestStatus::Resolved,
                        Some(ApprovalDecision::Cancel),
                        ItemStatus::Cancelled,
                    );
                }
                for run in wakes {
                    self.cancel_queued_run(&run);
                }
                // Settling a settled, unpinned thread again changes nothing.
                if !(thread.settled == Some(true) && thread.pinned_at.is_none()) {
                    self.fact(FactBody::ThreadSettled {
                        settled: true,
                        at: at.clone().unwrap_or_else(|| self.at.clone()),
                    });
                }
                self.effect(
                    None,
                    EffectBody::DetachSessions {
                        reason: "Thread settled.".into(),
                        revoke_credentials: false,
                        instance: None,
                    },
                );
                Reply::Accepted
            }
            Snooze { until } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                if until.as_ref().is_some_and(|t| t <= &self.at) {
                    return reject("snooze-must-be-future");
                }
                if until.is_some()
                    && (!self.state.queued_runs().is_empty()
                        || self
                            .state
                            .requests
                            .iter()
                            .any(|r| r.status == RequestStatus::Pending))
                {
                    return reject("pending-work-cannot-snooze");
                }
                if self.state.thread.as_ref().unwrap().snoozed_until != *until {
                    self.fact(FactBody::ThreadSnoozed {
                        until: until.clone(),
                    });
                }
                // A manual snooze takes over from a recovery's snooze.
                if until.is_some()
                    && let Some(mut recovery) = self
                        .state
                        .thread
                        .as_ref()
                        .unwrap()
                        .limit_recovery
                        .clone()
                        .filter(|recovery| recovery.snooze)
                {
                    recovery.snooze = false;
                    self.fact(FactBody::LimitRecoveryChanged {
                        recovery: Some(recovery),
                    });
                }
                Reply::Accepted
            }
            Pin { pinned, order } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                // A re-pin keeps its time and slot; it still clears a settle or snooze.
                let changes = if *pinned {
                    thread.pinned_at.is_none()
                        || thread.settled == Some(true)
                        || thread.snoozed_until.is_some()
                } else {
                    thread.pinned_at.is_some() || thread.pin_order.is_some()
                };
                if changes {
                    self.fact(FactBody::ThreadPinned {
                        pinned: *pinned,
                        order: order.clone(),
                    });
                }
                Reply::Accepted
            }
            ReorderPinned { order } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                if thread.pinned_at.is_none() {
                    return reject("thread-not-pinned");
                }
                if thread.pin_order.as_ref() != Some(order) {
                    self.fact(FactBody::ThreadPinReordered {
                        order: order.clone(),
                    });
                }
                Reply::Accepted
            }
            ReorderActive { order } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                if thread.pinned_at.is_some() || thread.settled == Some(true) {
                    return reject("thread-not-active");
                }
                self.fact(FactBody::ThreadActiveReordered {
                    order: order.clone(),
                });
                Reply::Accepted
            }
            Visit { at } => {
                if self
                    .state
                    .thread
                    .as_ref()
                    .unwrap()
                    .last_visited_at
                    .as_ref()
                    .is_none_or(|previous| at > previous)
                {
                    self.fact(FactBody::ThreadVisited { at: at.clone() });
                }
                Reply::Accepted
            }
            MarkUnread => {
                let Some(at) = self
                    .state
                    .runs
                    .iter()
                    .max_by_key(|r| r.ordinal)
                    .and_then(|r| r.completed_at.as_ref())
                else {
                    return reject("no-completed-run");
                };
                let Ok(at) = Timestamp::from_millis(at.millis() - 1) else {
                    return reject("invalid-visit-time");
                };
                self.fact(FactBody::ThreadVisited { at });
                Reply::Accepted
            }
            AutoSettle { enabled } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-archived");
                }
                if thread.auto_settle != *enabled {
                    self.fact(FactBody::AutoSettleChanged { enabled: *enabled });
                }
                Reply::Accepted
            }
            RuntimeMode { mode } => {
                self.fact(FactBody::RuntimeModeChanged { mode: *mode });
                // Codex takes the mode on its next turn; a Claude session cannot
                // switch in place, so it is detached.
                for instance in self.used_instances(Some(Driver::Claude)) {
                    self.effect(
                        None,
                        EffectBody::DetachSessions {
                            reason: "Runtime mode changed.".into(),
                            revoke_credentials: false,
                            instance: Some(instance),
                        },
                    );
                }
                Reply::Accepted
            }
            InteractionMode { mode } => {
                self.fact(FactBody::InteractionModeChanged { mode: *mode });
                Reply::Accepted
            }
            SelectModel { selection } | SwitchProvider { selection } => {
                let current = self.state.thread.as_ref().unwrap().selection.clone();
                self.fact(FactBody::ModelSelected {
                    selection: selection.clone(),
                });
                // A model applies from the next turn; another instance takes over
                // with a handoff.
                if current.instance != selection.instance {
                    self.release_other_instances(&selection.instance);
                }
                Reply::Accepted
            }
            DetachProviderSession { instance, reason } => {
                if !self.used_instances(None).contains(instance) {
                    return reject("provider-session-not-found");
                }
                self.effect(
                    None,
                    EffectBody::DetachSessions {
                        reason: reason.clone().unwrap_or_default(),
                        revoke_credentials: false,
                        instance: Some(instance.clone()),
                    },
                );
                Reply::Accepted
            }
            Send(message) => {
                if let Some(Continuation::UsageLimit { run, recovery }) = &message.continuation
                    && let Some(reply) = self.usage_limit_continuation(id, run, recovery.as_ref())
                {
                    return reply;
                }
                self.create_run(message)
            }
            Compact => {
                let message = SendMessage {
                    context: None,
                    created_by: MessageAuthor::User,
                    creation_source: "client".into(),
                    id: MessageId::new(self.key("message", id.as_str())).unwrap(),
                    text: "/compact".into(),
                    attachments: vec![],
                    selection: None,
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                    resolved_plan: None,
                    continuation: None,
                    title_seed: None,
                };
                self.create_run(&message)
            }
            UpdateMetadata {
                title,
                regenerate_title,
                branch,
                worktree_path,
                expected_worktree_path,
                expected_empty,
                limit_recovery,
                linked_pull_request,
                project_root,
            } => {
                let thread = self.state.thread.as_ref().unwrap().clone();
                let current_worktree = thread
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.worktree_path.clone());
                if expected_worktree_path
                    .as_ref()
                    .is_some_and(|expected| *expected != current_worktree)
                {
                    return reject("worktree-changed");
                }
                if *expected_empty
                    && (!self.state.messages.is_empty() || !self.state.runs.is_empty())
                {
                    return reject("thread-not-empty");
                }
                let title = title.as_deref().map(str::trim);
                if title.is_some_and(str::is_empty) {
                    return reject("title-required");
                }
                let blank = |value: &Option<Option<String>>| matches!(value, Some(Some(value)) if value.trim().is_empty());
                if blank(branch) || blank(worktree_path) || blank(expected_worktree_path) {
                    return reject("invalid-workspace");
                }
                if let Some(Some(update)) = limit_recovery {
                    if update.auto_resume.is_none() && update.snooze.is_none() {
                        return reject("invalid-limit-recovery");
                    }
                    if update.snooze == Some(true) && update.reset_at <= self.at {
                        return reject("limit-reset-passed");
                    }
                    let failure = usage_limit_failure(&self.state);
                    if thread.archived_at.is_some()
                        || thread.settled == Some(true)
                        || failure.as_ref().is_none_or(|(run, reset_at)| {
                            run.id != update.run
                                || reset_at.as_ref() != Some(&update.reset_at)
                                || update.reset_at
                                    <= run.completed_at.clone().unwrap_or(run.requested_at.clone())
                        })
                        || self
                            .state
                            .requests
                            .iter()
                            .any(|r| r.status == RequestStatus::Pending)
                    {
                        return reject("limit-changed");
                    }
                }
                if let Some(title) = title {
                    self.fact(FactBody::ThreadRenamed {
                        title: title.to_owned(),
                    });
                }
                if let Some(update) = limit_recovery {
                    let previous = thread.limit_recovery.as_ref().filter(|previous| {
                        update.as_ref().is_some_and(|update| {
                            previous.run == update.run && previous.reset_at == update.reset_at
                        })
                    });
                    let recovery = update.as_ref().map(|update| LimitRecovery {
                        request: Some(id.clone()),
                        run: update.run.clone(),
                        reset_at: update.reset_at.clone(),
                        auto_resume: update
                            .auto_resume
                            .or(previous.map(|p| p.auto_resume))
                            .unwrap_or(false),
                        snooze: update
                            .snooze
                            .or(previous.map(|p| p.snooze))
                            .unwrap_or(false),
                    });
                    if let Some(recovery) = recovery
                        .as_ref()
                        .filter(|r| r.snooze && r.reset_at > self.at)
                    {
                        self.fact(FactBody::ThreadSnoozed {
                            until: Some(recovery.reset_at.clone()),
                        });
                    } else if thread.limit_recovery.as_ref().is_some_and(|old| {
                        old.snooze && thread.snoozed_until.as_ref() == Some(&old.reset_at)
                    }) {
                        self.fact(FactBody::ThreadSnoozed { until: None });
                    }
                    self.fact(FactBody::LimitRecoveryChanged { recovery });
                }
                if branch.is_some() || worktree_path.is_some() {
                    let mut workspace = thread.workspace.clone().unwrap_or(Workspace {
                        cwd: project_root.clone().unwrap_or_default(),
                        worktree_path: None,
                        branch: None,
                    });
                    if let Some(branch) = branch {
                        workspace.branch = branch.as_deref().map(str::trim).map(str::to_owned);
                    }
                    if let Some(path) = worktree_path {
                        workspace.worktree_path = path.clone();
                        workspace.cwd = match path {
                            Some(path) => path.clone(),
                            None if current_worktree.as_ref() == Some(&workspace.cwd) => {
                                project_root.clone().unwrap_or(workspace.cwd.clone())
                            }
                            None => workspace.cwd.clone(),
                        };
                    }
                    self.fact(FactBody::WorkspaceBound {
                        workspace: Some(workspace),
                    });
                    // The provider sessions are detached when the worktree moves.
                    if worktree_path
                        .as_ref()
                        .is_some_and(|path| *path != current_worktree)
                    {
                        self.effect(
                            None,
                            EffectBody::DetachSessions {
                                reason: "Workspace changed.".into(),
                                revoke_credentials: false,
                                instance: None,
                            },
                        );
                    }
                }
                if let Some(pull_request) = linked_pull_request {
                    self.fact(FactBody::PullRequestLinked {
                        pull_request: pull_request.clone(),
                    });
                }
                match regenerate_title {
                    Some(true) => {
                        self.fact(FactBody::TitleRequested {
                            request: id.clone(),
                        });
                        self.effect(
                            None,
                            EffectBody::GenerateTitle {
                                request: id.clone(),
                                message: None,
                            },
                        );
                    }
                    Some(false) if title.is_none() && thread_title_pending(&self.state) => {
                        self.fact(FactBody::TitleRequestCleared);
                    }
                    _ => {}
                }
                Reply::Accepted
            }
            // Any change after the sweep's snapshot, or an explicit settle or
            // un-settle, wins over the sweep.
            SettleAutomatically {
                snapshot_at,
                settled_at,
            } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.settled.is_some() || &thread.updated_at > snapshot_at {
                    return reject("thread-changed");
                }
                self.command(
                    id,
                    &Settle {
                        settled: true,
                        at: settled_at.clone(),
                    },
                )
            }
            ImplementPlan { plan, run } => {
                let Some(found) = self
                    .state
                    .plans
                    .iter()
                    .find(|p| &p.id == plan && p.kind == PlanKind::Proposed)
                else {
                    return reject("plan-not-found");
                };
                if found.implemented_by.is_none() {
                    self.fact(FactBody::PlanImplemented {
                        id: plan.clone(),
                        run: run.clone(),
                    });
                }
                Reply::Accepted
            }
            ReleasePrepared { run } => {
                if self.state.active_run().is_some_and(|r| &r.id != run)
                    || !self
                        .state
                        .runs
                        .iter()
                        .any(|r| &r.id == run && r.status == RunStatus::Preparing)
                {
                    return reject("run-not-preparing");
                }
                self.end_preparation(
                    run,
                    ItemStatus::Completed,
                    "Workspace ready",
                    "Workspace preparation completed.",
                    Some(0),
                );
                self.start_run(run);
                Reply::Run(run.clone())
            }
            PreparedRunProgress { run, phase } => {
                let Some(item) = self.preparation_item(run).filter(|_| {
                    self.state
                        .runs
                        .iter()
                        .any(|r| &r.id == run && r.status == RunStatus::Preparing)
                }) else {
                    return reject("run-not-preparing");
                };
                let mut kind = item.kind.clone();
                if let ItemKind::CommandExecution { title, .. } = &mut kind {
                    *title = Some(
                        match phase {
                            PreparationPhase::Worktree => "Preparing worktree",
                            PreparationPhase::Setup => "Starting setup script",
                        }
                        .into(),
                    );
                }
                if kind != item.kind {
                    self.fact(FactBody::ItemDetailChanged { id: item.id, kind });
                }
                Reply::Accepted
            }
            FailPrepared { run, message } => {
                if !self
                    .state
                    .runs
                    .iter()
                    .any(|r| &r.id == run && r.status == RunStatus::Preparing)
                {
                    return reject("run-not-preparing");
                }
                self.end_preparation(
                    run,
                    ItemStatus::Failed,
                    "Workspace preparation failed",
                    message,
                    Some(1),
                );
                self.error_item_coded(
                    run,
                    message,
                    Some("validation_error"),
                    Some(WORKSPACE_PREPARATION_FAILURE_CODE),
                );
                self.finish(run, RunStatus::Failed, false);
                Reply::Accepted
            }
            RetryPrepared { run } => {
                // Only a run whose workspace preparation failed goes back to preparing.
                let failure = self
                    .state
                    .items
                    .iter()
                    .find(|item| {
                        item.run.as_ref() == Some(run)
                            && item.status == ItemStatus::Failed
                            && matches!(&item.kind, ItemKind::Error { code: Some(code), .. }
                                if code == WORKSPACE_PREPARATION_FAILURE_CODE)
                    })
                    .map(|item| item.id.clone());
                let thread = self.state.thread.as_ref().unwrap();
                let Some(failure) = failure.filter(|_| {
                    thread.archived_at.is_none()
                        && self.state.runs.iter().any(|r| {
                            &r.id == run && r.status == RunStatus::Failed && r.attempt.is_none()
                        })
                }) else {
                    return reject("run-not-retryable");
                };
                if self.state.active_run().is_some() {
                    return reject("run-not-retryable");
                }
                self.fact(FactBody::ItemCompleted {
                    id: failure,
                    status: ItemStatus::Cancelled,
                });
                if let Some(item) = self.preparation_item(run) {
                    self.fact(FactBody::ItemReopened {
                        id: item.id.clone(),
                    });
                    self.fact(FactBody::ItemDetailChanged {
                        id: item.id.clone(),
                        kind: preparation_kind(WORKSPACE_PREPARATION_INPUT, None),
                    });
                    self.replace_text(&item.id, "");
                }
                self.fact(FactBody::RunPrepared { id: run.clone() });
                self.effect(None, EffectBody::PrepareWorkspace { run: run.clone() });
                Reply::Accepted
            }
            Interrupt {
                run,
                hold_queue,
                reason,
            } => {
                let Some(target) = self.state.runs.iter().find(|r| &r.id == run).cloned() else {
                    return reject("run-not-active");
                };
                let background = self
                    .state
                    .tasks
                    .iter()
                    .any(|t| t.run.as_ref() == Some(run) && !t.status.terminal())
                    || self
                        .state
                        .background_work
                        .values()
                        .any(|t| target.attempt.as_ref() == Some(&t.attempt));
                if !target.status.blocking()
                    && !(target.status == RunStatus::Completed && background)
                {
                    return reject("run-not-active");
                }
                if target
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| self.state.stopping.contains(attempt))
                {
                    return Reply::Ignored;
                }
                if *hold_queue {
                    self.hold_queue();
                }
                let request = reason.as_deref().unwrap_or(INTERRUPT_REQUESTED);
                let cohort = self.delivery_cohort(&target.message).unwrap_or(run.clone());
                let started = target.attempt.as_ref().is_some_and(|attempt| {
                    self.state
                        .attempts
                        .iter()
                        .any(|a| &a.id == attempt && a.status == AttemptStatus::Running)
                });
                if let Some(attempt) = &target.attempt {
                    self.fact(FactBody::StopRequested {
                        attempt: attempt.clone(),
                    });
                    self.interrupt_provider(attempt);
                    self.stop_tasks(attempt, ItemStatus::Interrupted, false);
                }
                if started || background && !target.status.blocking() {
                    self.interrupt_item(run, target.attempt.as_ref(), request, None);
                    self.dispose_cohort(&cohort);
                    return Reply::Accepted;
                }
                self.interrupt_item(
                    run,
                    target.attempt.as_ref(),
                    request,
                    Some((
                        ItemStatus::Interrupted,
                        "Run interrupted before provider start",
                    )),
                );
                self.end_preparation(
                    run,
                    ItemStatus::Interrupted,
                    "Workspace preparation interrupted",
                    reason
                        .as_deref()
                        .unwrap_or("Interrupted before provider start"),
                    None,
                );
                self.dispose_cohort(&cohort);
                self.finish(run, RunStatus::Interrupted, true);
                Reply::Accepted
            }
            ResumeQueue => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.archived_at.is_some() {
                    return reject("thread-not-active");
                }
                if usage_limited(&self.state) {
                    return reject("usage-limited");
                }
                let ids = self
                    .state
                    .queued_runs()
                    .iter()
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                for id in ids {
                    self.fact(FactBody::QueueHeld { id, held: false });
                }
                self.promote();
                Reply::Accepted
            }
            ReorderQueued { run, before } => {
                let queued = self
                    .state
                    .queued_runs()
                    .iter()
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                if !queued.contains(run) {
                    return reject("queued-run-not-found");
                }
                if self.automatic_run(run) {
                    return reject("automatic-delivery-not-reorderable");
                }
                let delegated = |id: &RunId| {
                    self.state
                        .runs
                        .iter()
                        .find(|r| &r.id == id)
                        .is_some_and(|r| self.state.delegated_delivery(&r.message))
                };
                if before.as_ref().is_some_and(&delegated) {
                    return reject("cannot-reorder-ahead-of-automatic-delivery");
                }
                let (automatic, rest): (Vec<_>, Vec<_>) =
                    queued.into_iter().partition(|id| delegated(id));
                let mut order = rest.into_iter().filter(|id| id != run).collect::<Vec<_>>();
                let position = match before {
                    None => order.len(),
                    Some(id) => match order.iter().position(|r| r == id) {
                        Some(position) => position,
                        None => return reject("queued-run-not-found"),
                    },
                };
                order.insert(position, run.clone());
                self.fact(FactBody::QueueReordered {
                    order: automatic.into_iter().chain(order).collect(),
                });
                Reply::Accepted
            }
            CancelQueued { run } => {
                if !self
                    .state
                    .runs
                    .iter()
                    .any(|r| &r.id == run && r.status == RunStatus::Queued)
                {
                    return reject("queued-run-not-found");
                }
                self.cancel_queued_run(run);
                self.promote();
                Reply::Accepted
            }
            EditQueued {
                run,
                text,
                attachments,
                context,
            } => {
                if text.trim().is_empty() {
                    return reject("empty-message");
                }
                let context = match context.as_ref().map(MessageContext::normalized) {
                    Some(None) => return reject("invalid-message-context"),
                    context => context.flatten(),
                };
                let Some(r) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == run && r.status == RunStatus::Queued)
                else {
                    return reject("queued-run-not-found");
                };
                if self.automatic_run(run) {
                    return reject("automatic-delivery-not-editable");
                }
                if let Some(attachments) = attachments
                    && let Err(reason) = validate_attachments(attachments)
                {
                    return reject(reason);
                }
                self.fact(FactBody::MessageEdited {
                    context,
                    id: r.message.clone(),
                    text: text.clone(),
                    attachments: attachments.clone(),
                });
                Reply::Accepted
            }
            PromoteToSteer { queued, active } => {
                if self
                    .state
                    .thread
                    .as_ref()
                    .is_some_and(|thread| thread.archived_at.is_some())
                {
                    return reject("thread-not-active");
                }
                let Some(run) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == queued && r.status == RunStatus::Queued)
                    .cloned()
                else {
                    return reject("queued-run-not-found");
                };
                if self.automatic_run(queued) {
                    return reject("automatic-delivery-not-promotable");
                }
                let m = self
                    .state
                    .messages
                    .iter()
                    .find(|m| m.id == run.message)
                    .unwrap()
                    .clone();
                if maintenance(&m.text, &m.attachments).is_some() {
                    return reject("maintenance-must-run-separately");
                }
                let Some(target) = self.state.runs.iter().find(|r| &r.id == active).cloned() else {
                    return reject("run-not-active");
                };
                if self
                    .state
                    .message(&target.message)
                    .is_some_and(|m| maintenance(&m.text, &m.attachments).is_some())
                {
                    return reject("maintenance-in-progress");
                }
                if target.status != RunStatus::Running {
                    return reject("run-not-active");
                }
                // Promotion uses the thread's selection, so another instance restarts the run.
                let selection = self.state.thread.as_ref().unwrap().selection.clone();
                let handoff = if selection.instance != target.selection.instance {
                    let support = TurnSupport::for_driver(target.selection.driver);
                    if !(support.interrupt && support.restart) {
                        return reject("restart-unsupported");
                    }
                    Some(self.restart_handoff(&target))
                } else {
                    None
                };
                let Some(attempt) = target.attempt.clone() else {
                    return reject("run-not-active");
                };
                self.fact(FactBody::RunFinished {
                    id: queued.clone(),
                    status: RunStatus::Cancelled,
                });
                self.fact(FactBody::MessageAdopted {
                    id: m.id.clone(),
                    run: active.clone(),
                    intent: InputIntent::PromotedQueuedToSteer,
                });
                self.user_item(&m.id, active);
                if handoff.is_some() {
                    self.restart_run(&target, &attempt, &selection, &m.id, handoff);
                    return Reply::Run(active.clone());
                }
                self.effect(
                    Some(attempt),
                    EffectBody::Provider(ProviderCommand::Steer {
                        message: m.id.clone(),
                        text: project_context_for_provider(&m.text, m.context.as_ref()),
                        attachments: m.attachments,
                    }),
                );
                Reply::Run(active.clone())
            }
            Respond {
                request,
                decision,
                answers,
                attachments,
            } => {
                let Some(r) = self
                    .state
                    .requests
                    .iter()
                    .find(|r| &r.id == request && r.status == RequestStatus::Pending)
                    .cloned()
                else {
                    return reject("request-not-ready");
                };
                let declined = matches!(
                    decision,
                    Some(ApprovalDecision::Decline | ApprovalDecision::Cancel)
                );
                match &r.body {
                    RequestBody::Approval { .. } if decision.is_none() => {
                        return reject("invalid-approval-decision");
                    }
                    RequestBody::Questions { questions }
                        if !declined
                            && (answers.is_none()
                                || questions.iter().any(|q| {
                                    q.required
                                        && answers.as_ref().is_none_or(|a| !a.contains_key(&q.id))
                                })) =>
                    {
                        return reject("missing-question-answer");
                    }
                    _ => {}
                }
                if r.capability == ResponseCapability::NotResumable {
                    return reject("request-not-resumable");
                }
                let card = if declined {
                    ItemStatus::Cancelled
                } else {
                    ItemStatus::Completed
                };
                if declined && r.capability == ResponseCapability::Message {
                    self.resolve_request(request, RequestStatus::Resolved, *decision, card);
                    return Reply::Request(request.clone());
                }
                let provider_answers = match answers.as_ref().filter(|_| !declined) {
                    Some(answers) => match append_answer_attachments(answers, attachments) {
                        Ok(answers) => Some(answers),
                        Err(reason) => return reject(reason),
                    },
                    None => None,
                };
                let async_text = if r.capability == ResponseCapability::Message && !declined {
                    let RequestBody::Questions { questions } = &r.body else {
                        return reject("question-not-found");
                    };
                    let mut replies = vec![];
                    for question in questions {
                        let answer = provider_answers.as_ref().and_then(|a| a.get(&question.id));
                        match answer {
                            Some(Answer::Text(text)) if !text.trim().is_empty() => {
                                replies.push(format!("{}\n{}", question.question, text.trim()));
                            }
                            _ if !question.required => {}
                            _ => return reject("missing-question-answer"),
                        }
                    }
                    if replies.is_empty() {
                        return reject("missing-question-answer");
                    }
                    Some(replies.join("\n\n"))
                } else {
                    None
                };
                self.fact(FactBody::RequestResolved {
                    id: request.clone(),
                    status: RequestStatus::Resolved,
                    decision: *decision,
                    answers: answers.clone(),
                    attachments: attachments.clone(),
                });
                self.complete_request_cards(request, card);
                if r.capability == ResponseCapability::Message {
                    let message = SendMessage {
                        context: None,
                        created_by: MessageAuthor::User,
                        creation_source: "server".into(),
                        id: MessageId::new(format!("async-answer:{request}")).unwrap(),
                        text: async_text.unwrap(),
                        attachments: vec![],
                        selection: None,
                        mode: DispatchMode::QueueAfterActive,
                        intent: Some(DeliveryIntent::Auto),
                        source_plan: None,
                        resolved_plan: None,
                        continuation: None,
                        title_seed: None,
                    };
                    return self.create_run(&message);
                }
                let input = match r.body {
                    RequestBody::Approval { input, .. } => Some(input),
                    _ => None,
                };
                self.effect(
                    Some(r.attempt),
                    EffectBody::Provider(ProviderCommand::Respond {
                        native_key: r.native_key,
                        decision: *decision,
                        answers: provider_answers,
                        input,
                    }),
                );
                Reply::Request(request.clone())
            }
            DismissQuestion { request } => {
                let Some(r) = self
                    .state
                    .requests
                    .iter()
                    .find(|r| &r.id == request && r.status == RequestStatus::Pending)
                else {
                    return reject("question-already-answered");
                };
                if r.capability != ResponseCapability::Message
                    || !matches!(r.body, RequestBody::Questions { .. })
                {
                    return reject("question-needs-answer");
                }
                self.resolve_request(
                    request,
                    RequestStatus::Resolved,
                    Some(ApprovalDecision::Cancel),
                    ItemStatus::Cancelled,
                );
                Reply::Accepted
            }
            Rollback {
                checkpoint,
                restore_files,
                restore_refusal,
            } => {
                if self.state.active_run().is_some()
                    || self.state.tasks.iter().any(|t| !t.status.terminal())
                {
                    return reject("provider-work-active");
                }
                // Rolling back targets the active provider thread: the instance
                // of the latest run that left the queue.
                let Some(active) =
                    latest_executed_run(&self.state).map(|run| run.selection.instance.clone())
                else {
                    return reject("no-active-provider-thread");
                };
                let Some(cp) = self
                    .state
                    .checkpoints
                    .iter()
                    .find(|c| &c.id == checkpoint)
                    .cloned()
                else {
                    return reject("checkpoint-not-found");
                };
                if cp.status != CheckpointStatus::Ready {
                    return reject("checkpoint-not-ready");
                }
                // Restore isolation is checked at admission too.
                if *restore_files && let Some(refusal) = restore_refusal {
                    return reject(refusal);
                }
                if cp.run_ordinal > 0 {
                    let target = self
                        .state
                        .runs
                        .iter()
                        .find(|run| run.ordinal == cp.run_ordinal);
                    let turn = target.and_then(|run| {
                        self.state.attempts.iter().find(|attempt| {
                            Some(&attempt.id) == run.attempt.as_ref() && attempt.accepted
                        })
                    });
                    if turn.is_none() {
                        return reject("rollback-provider-turn-unavailable");
                    }
                    if target.is_some_and(|run| run.selection.instance != active) {
                        return reject("rollback-provider-thread-mismatch");
                    }
                }
                self.fact(FactBody::RollbackRequested {
                    command: id.clone(),
                    checkpoint: checkpoint.clone(),
                    restore_files: *restore_files,
                });
                // The rollback request fails when the selection moved to
                // another instance than the active one.
                let thread = self.state.thread.as_ref().unwrap();
                if thread.selection.instance != active {
                    let message = rollback_provider_changed(checkpoint, &thread.id);
                    self.fact(FactBody::RollbackFailed {
                        command: id.clone(),
                        message,
                    });
                    return Reply::Accepted;
                }
                let rewinds = self.state.runs.iter().any(|run| {
                    run.ordinal > cp.run_ordinal
                        && run.status.terminal()
                        && run.status != RunStatus::RolledBack
                });
                // The provider is asked to rewind whenever later runs exist,
                // which fails without a native thread to rewind.
                if rewinds && !self.state.native_sessions.contains_key(&active) {
                    self.fact(FactBody::RollbackFailed {
                        command: id.clone(),
                        message: ROLLBACK_FAILED_MESSAGE.into(),
                    });
                    return Reply::Accepted;
                }
                let providers = self
                    .state
                    .native_sessions
                    .get(&active)
                    .filter(|_| rewinds)
                    .map(|native_thread| ProviderRollback {
                        instance: active.clone(),
                        command: ProviderCommand::Rollback {
                            native_thread: native_thread.clone(),
                            absolute_head: cp.native_heads.get(&active).cloned().flatten(),
                        },
                    })
                    .into_iter()
                    .collect();
                let stale_file_refs = self
                    .state
                    .checkpoints
                    .iter()
                    .filter(|candidate| {
                        candidate.scope == cp.scope
                            && candidate.run_ordinal > cp.run_ordinal
                            && candidate.status == CheckpointStatus::Ready
                    })
                    .map(|candidate| candidate.file_ref.clone())
                    .collect();
                self.effect(
                    None,
                    EffectBody::Rollback {
                        command: id.clone(),
                        providers,
                        restore: restore_files.then(|| RestoreFiles {
                            scope: cp.scope.clone(),
                            checkpoint: checkpoint.clone(),
                            file_ref: cp.file_ref.clone(),
                        }),
                        stale_file_refs,
                    },
                );
                Reply::Accepted
            }
            Fork {
                target,
                source,
                title,
                created_by,
                creation_source,
            } => {
                let title = title.as_deref().map(str::trim);
                if title.is_some_and(str::is_empty) {
                    return reject("title-required");
                }
                let Some(run) = self.source_run(source).cloned() else {
                    return reject("no-stable-source-run");
                };
                // Provider-finished and unsuccessful runs fork; in-progress and
                // rolled-back runs do not.
                if !matches!(
                    run.status,
                    RunStatus::Completed
                        | RunStatus::Waiting
                        | RunStatus::Interrupted
                        | RunStatus::Failed
                        | RunStatus::Cancelled
                ) {
                    return reject("fork-source-not-ready");
                }
                let thread = self.state.thread.as_ref().unwrap().clone();
                // History inherits the source's own inheritance and its items
                // through the boundary run, whatever those runs' status.
                let mut history = self
                    .state
                    .inherited_items
                    .iter()
                    .chain(self.state.items.iter().filter(|item| {
                        item.run.as_ref().is_none_or(|id| {
                            self.state
                                .runs
                                .iter()
                                .any(|r| &r.id == id && r.ordinal <= run.ordinal)
                        })
                    }))
                    .map(|item| self.state.notification_card(item).into_owned())
                    .collect::<Vec<_>>();
                history.sort_by(|a, b| (a.ordinal, &a.id).cmp(&(b.ordinal, &b.id)));
                let messages = history
                    .iter()
                    .filter_map(|item| match &item.kind {
                        ItemKind::UserMessage { message }
                        | ItemKind::AssistantMessage { message } => {
                            self.state.message(message).cloned()
                        }
                        _ => None,
                    })
                    .collect();
                let context = prepare_history(&self.state, &history, run.ordinal);
                // Only a provider-finished run has a stable native boundary.
                let native = run
                    .attempt
                    .as_ref()
                    .and_then(|id| self.state.attempts.iter().find(|a| &a.id == id))
                    .filter(|_| matches!(run.status, RunStatus::Completed | RunStatus::Waiting))
                    .and_then(|a| {
                        let latest = !self.state.runs.iter().any(|later| {
                            later.ordinal > run.ordinal
                                && later.selection.instance == run.selection.instance
                                && later.attempt.as_ref().is_some_and(|id| {
                                    self.state.attempts.iter().any(|attempt| {
                                        &attempt.id == id
                                            && attempt.accepted
                                            && attempt.native_thread == a.native_thread
                                    })
                                })
                        });
                        a.native_thread
                            .clone()
                            .filter(|_| latest || a.native_head.is_some())
                            .map(|native| NativeBinding {
                                instance: run.selection.instance.clone(),
                                thread: native,
                                head: a.native_head.clone(),
                            })
                    });
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: target.clone(),
                        command: Box::new(AcceptFork {
                            thread: target.clone(),
                            parent: thread.id.clone(),
                            project: thread.project.clone(),
                            title: title
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("{} fork", thread.title)),
                            selection: thread.selection.clone(),
                            runtime_mode: thread.runtime_mode,
                            interaction_mode: thread.interaction_mode,
                            boundary: run.ordinal,
                            history,
                            messages,
                            workspace: thread.workspace.clone(),
                            arrangement: Box::new(ThreadArrangement::of(&thread)),
                            context,
                            native,
                            created_by: *created_by,
                            creation_source: creation_source.clone(),
                        }),
                    },
                );
                Reply::Thread(target.clone())
            }
            AcceptFork {
                thread,
                parent,
                project,
                title,
                selection,
                runtime_mode,
                interaction_mode,
                boundary,
                history,
                messages,
                workspace,
                arrangement,
                context,
                native,
                created_by,
                creation_source,
            } => {
                if self.state.thread.is_some() {
                    return reject("thread-already-exists");
                }
                self.fact(FactBody::ThreadCreated {
                    id: thread.clone(),
                    project: project.clone(),
                    title: title.clone(),
                    selection: selection.clone(),
                    runtime_mode: *runtime_mode,
                    interaction_mode: *interaction_mode,
                    created_by: *created_by,
                    creation_source: creation_source.clone(),
                });
                self.fact(FactBody::ThreadArranged(arrangement.as_ref().clone()));
                if workspace.is_some() {
                    self.fact(FactBody::WorkspaceBound {
                        workspace: workspace.clone(),
                    });
                }
                // The child's runs get a checkpoint scope of their own when they start.
                self.fact(FactBody::ForkAccepted {
                    parent: parent.clone(),
                    boundary: *boundary,
                    history: history.clone(),
                    messages: messages.clone(),
                });
                let marker = TurnItemId::new(self.key("fork", thread.as_str())).unwrap();
                self.item_start(
                    marker.clone(),
                    None,
                    None,
                    String::new(),
                    ItemKind::Fork {
                        parent: parent.clone(),
                        boundary: *boundary,
                    },
                );
                self.fact(FactBody::ItemCompleted {
                    id: marker,
                    status: ItemStatus::Completed,
                });
                // The first direct message resolves the transfer against its own
                // provider: a native fork on the source's provider, otherwise
                // the portable history.
                self.fact(FactBody::TransferOpened {
                    native_source: native.clone(),
                    id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                    kind: TransferKind::Fork,
                    source: parent.clone(),
                    target: thread.clone(),
                    boundary: *boundary,
                    instance: None,
                    target_run: None,
                    history: context.clone(),
                });
                Reply::Thread(thread.clone())
            }
            MergeBack { target, source } => {
                let thread = self.state.thread.as_ref().unwrap();
                if thread.parent.as_ref() != Some(target) || thread.fork_boundary.is_none() {
                    return reject("not-a-fork-of-target");
                }
                let Some(source) = self.source_run(source) else {
                    return reject("no-stable-source-run");
                };
                if !matches!(source.status, RunStatus::Completed | RunStatus::Waiting) {
                    return reject("merge-back-source-not-finished");
                }
                if !self.state.transfers.iter().any(|transfer| {
                    transfer.kind == TransferKind::Fork
                        && &transfer.source == target
                        && transfer.target == thread.id
                }) {
                    return reject("no-fork-transfer");
                }
                let boundary = source.ordinal;
                let history = prepare_history(
                    &self.state,
                    &self
                        .state
                        .visible_items()
                        .into_iter()
                        .filter(|i| {
                            i.run.as_ref().is_some_and(|id| {
                                self.state
                                    .runs
                                    .iter()
                                    .any(|r| &r.id == id && r.ordinal <= boundary)
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                    boundary,
                );
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: target.clone(),
                        command: Box::new(AcceptTransfer {
                            id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                            kind: TransferKind::MergeBack,
                            source: thread.id.clone(),
                            boundary,
                            history,
                        }),
                    },
                );
                Reply::Accepted
            }
            AcceptTransfer {
                id,
                kind,
                source,
                boundary,
                history,
            } => {
                if self.state.transfers.iter().any(|t| {
                    t.id == *id
                        || (t.kind == *kind
                            && t.source == *source
                            && t.boundary == *boundary
                            && t.history == *history
                            && !t.superseded)
                }) {
                    return Reply::Accepted;
                }
                // The next direct message consumes it with its own provider.
                self.fact(FactBody::TransferOpened {
                    native_source: None,
                    id: id.clone(),
                    kind: *kind,
                    source: source.clone(),
                    target: self.state.thread.as_ref().unwrap().id.clone(),
                    boundary: *boundary,
                    instance: None,
                    target_run: None,
                    history: history.clone(),
                });
                Reply::Accepted
            }
            Delegate {
                task,
                child,
                prompt,
                title,
                selection,
                runtime_mode,
                interaction_mode,
                wake,
            } => {
                let Some(run) = self.state.active_run().cloned() else {
                    return reject("no-active-run");
                };
                let Some(attempt) = run.attempt else {
                    return reject("no-active-attempt");
                };
                if self
                    .state
                    .tasks
                    .iter()
                    .any(|candidate| &candidate.id == task)
                {
                    return reject("task-already-exists");
                }
                // The task and title are decoded as trimmed non-empty strings.
                let prompt = prompt.trim().to_owned();
                if prompt.is_empty() {
                    return reject("task-required");
                }
                let title = title.as_deref().map(str::trim);
                if title.is_some_and(str::is_empty) {
                    return reject("title-required");
                }
                let title = title.map(str::to_owned);
                let child_title = subagent_thread_title(
                    &self.state.thread.as_ref().unwrap().title,
                    title.as_deref(),
                    &prompt,
                    self.state.tasks.len() + 1,
                );
                let message = MessageId::new(self.key("delegate-message", task.as_str())).unwrap();
                self.fact(FactBody::TaskStarted {
                    original_message: Some(message.clone()),
                    background: false,
                    id: task.clone(),
                    native_key: task.to_string(),
                    run: Some(run.id.clone()),
                    attempt: attempt.clone(),
                    child: child.clone(),
                    parent: None,
                    prompt: prompt.clone(),
                    model: Some(selection.model.clone()),
                    wake: *wake,
                });
                if let Some(title) = &title {
                    self.fact(FactBody::TaskNamed {
                        id: task.clone(),
                        title: title.clone(),
                    });
                }
                self.item_start(
                    TurnItemId::new(self.key("delegate-item", task.as_str())).unwrap(),
                    Some(run.id),
                    Some(attempt),
                    task.to_string(),
                    ItemKind::Subagent { task: task.clone() },
                );
                let thread = self.state.thread.as_ref().unwrap();
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: child.clone(),
                        command: Box::new(AcceptDelegation {
                            thread: child.clone(),
                            project: thread.project.clone(),
                            title: child_title,
                            selection: selection.clone(),
                            runtime_mode: *runtime_mode,
                            interaction_mode: *interaction_mode,
                            workspace: thread.workspace.clone(),
                            arrangement: Box::new(ThreadArrangement::of(thread)),
                            origin: Delegation {
                                parent: thread.id.clone(),
                                task: task.clone(),
                                message: message.clone(),
                            },
                            created_by: MessageAuthor::Agent,
                            creation_source: "mcp".into(),
                            message: Box::new(SendMessage {
                                context: None,
                                created_by: MessageAuthor::Agent,
                                creation_source: "mcp".into(),
                                id: message,
                                text: prompt.clone(),
                                attachments: vec![],
                                selection: None,
                                mode: DispatchMode::StartImmediately,
                                intent: None,
                                source_plan: None,
                                resolved_plan: None,
                                continuation: None,
                                title_seed: None,
                            }),
                        }),
                    },
                );
                Reply::Thread(child.clone())
            }
            AcceptDelegation {
                thread,
                project,
                title,
                selection,
                runtime_mode,
                interaction_mode,
                workspace,
                arrangement,
                origin,
                message,
                created_by,
                creation_source,
            } => {
                if self.state.thread.is_some() {
                    return reject("thread-already-exists");
                }
                self.fact(FactBody::ThreadCreated {
                    id: thread.clone(),
                    project: project.clone(),
                    title: title.clone(),
                    selection: selection.clone(),
                    runtime_mode: *runtime_mode,
                    interaction_mode: *interaction_mode,
                    created_by: *created_by,
                    creation_source: creation_source.clone(),
                });
                self.fact(FactBody::ThreadArranged(arrangement.as_ref().clone()));
                if workspace.is_some() {
                    self.fact(FactBody::WorkspaceBound {
                        workspace: workspace.clone(),
                    });
                }
                self.fact(FactBody::DelegationAccepted {
                    origin: origin.clone(),
                });
                self.create_run(message)
            }
            TaskProgress {
                task,
                progress,
                model,
            } => {
                if !self.state.tasks.iter().any(|t| &t.id == task) {
                    return reject("task-not-found");
                }
                self.fact(FactBody::TaskProgressed {
                    id: task.clone(),
                    progress: progress.clone(),
                    model: model.clone(),
                });
                Reply::Accepted
            }
            TaskResult {
                task,
                source_message,
                generation,
                context,
                status,
                result,
            } => {
                let Some(existing) = self
                    .state
                    .tasks
                    .iter()
                    .find(|candidate| &candidate.id == task)
                    .cloned()
                else {
                    return reject("task-not-found");
                };
                if existing.original_message != *source_message
                    || existing.app_owned() && existing.status.terminal()
                    || generation.is_some_and(|generation| generation != existing.generation)
                {
                    return Reply::Ignored;
                }
                if let Some(context) = context {
                    // The result is handed to the spawning run's provider thread; a
                    // later turn receives it only after that run failed or was
                    // interrupted.
                    let spawning = existing
                        .run
                        .as_ref()
                        .and_then(|id| self.state.runs.iter().find(|run| &run.id == id));
                    self.fact(FactBody::TransferOpened {
                        native_source: None,
                        id: ContextTransferId::new(self.key("task-result", task.as_str())).unwrap(),
                        kind: TransferKind::SubagentResult,
                        source: existing.child_thread,
                        target: self.state.thread.as_ref().unwrap().id.clone(),
                        instance: Some(spawning.map_or_else(
                            || {
                                self.state
                                    .thread
                                    .as_ref()
                                    .unwrap()
                                    .selection
                                    .instance
                                    .clone()
                            },
                            |run| run.selection.instance.clone(),
                        )),
                        target_run: existing.run.clone(),
                        boundary: context.boundary,
                        history: context.history.clone(),
                    });
                }
                self.finish_task(task, *status, result);
                self.wake_tasks();
                Reply::Accepted
            }
            SetTaskWake { task, wake } => {
                if !self.state.tasks.iter().any(|t| &t.id == task) {
                    return reject("task-not-found");
                }
                self.fact(FactBody::TaskWakeChanged {
                    id: task.clone(),
                    wake: *wake,
                });
                self.wake_tasks();
                Reply::Accepted
            }
            AcknowledgeTask { task } | DisposeTask { task } => {
                if !self.state.tasks.iter().any(|t| &t.id == task) {
                    return reject("task-not-found");
                }
                self.fact(FactBody::TaskDeliveryChanged {
                    id: task.clone(),
                    state: if matches!(command, DisposeTask { .. }) {
                        DeliveryState::Disposed
                    } else {
                        DeliveryState::Acknowledged
                    },
                });
                Reply::Accepted
            }
            AcceptTaskWake { task_ids } => {
                let Some(cohort) = task_ids.iter().find_map(|id| {
                    self.state
                        .tasks
                        .iter()
                        .find(|task| &task.id == id && task.app_owned())
                        .and_then(|task| task.run.clone())
                }) else {
                    return reject("invalid-completion-cohort");
                };
                let eligible = self.wake_eligible(&cohort);
                if eligible.is_empty() {
                    return Reply::Ignored;
                }
                match self.cohort_wake(&cohort) {
                    Some((message, RunStatus::Queued)) => {
                        let mut ids = self
                            .state
                            .message(&message)
                            .and_then(|m| m.notification.as_ref())
                            .map(|n| match &n.source {
                                NotificationSource::Delegated { task_ids } => task_ids.clone(),
                                _ => vec![],
                            })
                            .unwrap_or_default();
                        ids.extend(eligible.iter().cloned());
                        self.fact(FactBody::MessageEdited {
                            context: None,
                            id: message.clone(),
                            text: wake_text(&ids),
                            attachments: None,
                        });
                        self.fact(FactBody::MessageNotificationAssigned {
                            id: message,
                            notification: delegated_notification(&ids, &cohort, &self.state.tasks),
                        });
                        for id in eligible {
                            self.fact(FactBody::TaskDeliveryChanged {
                                id,
                                state: DeliveryState::Claimed,
                            });
                        }
                        return Reply::Accepted;
                    }
                    Some((_, status)) if !status.terminal() => return Reply::Ignored,
                    _ => {}
                }
                let task_ids = &eligible;
                let notification = delegated_notification(task_ids, &cohort, &self.state.tasks);
                let text = wake_text(task_ids);
                let message_id =
                    MessageId::new(self.key("completion-message", id.as_str())).unwrap();
                let always = self.state.tasks.iter().all(|task| {
                    !task_ids.contains(&task.id) || task.wake == CompletionWake::Always
                });
                if let Some(target) = self
                    .state
                    .active_run()
                    .filter(|run| {
                        always
                            && run.status == RunStatus::Running
                            && run.attempt.is_some()
                            && TurnSupport::for_driver(run.selection.driver).steer
                            && self
                                .state
                                .message(&run.message)
                                .is_none_or(|m| maintenance(&m.text, &m.attachments).is_none())
                    })
                    .cloned()
                {
                    self.fact(FactBody::MessageCreated {
                        id: message_id.clone(),
                        run: Some(target.id.clone()),
                        role: Role::User,
                        text: text.clone(),
                        attachments: vec![],
                        intent: InputIntent::Steer,
                        created_by: MessageAuthor::Agent,
                        creation_source: "server".into(),
                        context: None,
                    });
                    self.fact(FactBody::MessageNotificationAssigned {
                        id: message_id.clone(),
                        notification,
                    });
                    self.user_item(&message_id, &target.id);
                    for task in task_ids {
                        self.fact(FactBody::TaskDeliveryChanged {
                            id: task.clone(),
                            state: DeliveryState::Claimed,
                        });
                    }
                    self.effect(
                        target.attempt.clone(),
                        EffectBody::Provider(ProviderCommand::Steer {
                            message: message_id,
                            text,
                            attachments: vec![],
                        }),
                    );
                    return Reply::Run(target.id);
                }
                let tasks = task_ids.clone();
                let message = SendMessage {
                    context: None,
                    created_by: MessageAuthor::Agent,
                    creation_source: "server".into(),
                    id: message_id,
                    text,
                    attachments: vec![],
                    selection: None,
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                    resolved_plan: None,
                    continuation: None,
                    title_seed: None,
                };
                let reply = self.create_run(&message);
                if !matches!(reply, Reply::Rejected { .. }) {
                    self.fact(FactBody::MessageNotificationAssigned {
                        id: message.id.clone(),
                        notification,
                    });
                    for id in tasks {
                        self.fact(FactBody::TaskDeliveryChanged {
                            id,
                            state: DeliveryState::Claimed,
                        });
                    }
                }
                reply
            }
        }
    }
    fn automatic_run(&self, run: &RunId) -> bool {
        self.state
            .runs
            .iter()
            .find(|r| &r.id == run)
            .and_then(|r| self.state.message(&r.message))
            .is_some_and(|message| message.notification.is_some())
    }
    /// Terminal tasks of a parent run awaiting delivery. A settled-only task
    /// waits for its spawning run, not for unrelated active runs.
    fn wake_eligible(&self, cohort: &RunId) -> Vec<NodeId> {
        let spawning_live = self.state.runs.iter().any(|run| {
            &run.id == cohort
                && matches!(
                    run.status,
                    RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
                )
        });
        self.state
            .tasks
            .iter()
            .filter(|task| {
                task.app_owned()
                    && task.status.terminal()
                    && task.delivery == DeliveryState::Pending
                    && task.run.as_ref() == Some(cohort)
                    && (task.wake == CompletionWake::Always || !spawning_live)
            })
            .map(|task| task.id.clone())
            .collect()
    }
    /// The latest completion message of a parent run's cohort and its run status.
    fn cohort_wake(&self, cohort: &RunId) -> Option<(MessageId, RunStatus)> {
        self.state.messages.iter().rev().find_map(|message| {
            let NotificationSource::Delegated { task_ids } = &message.notification.as_ref()?.source
            else {
                return None;
            };
            task_ids
                .iter()
                .any(|id| {
                    self.state
                        .tasks
                        .iter()
                        .any(|task| &task.id == id && task.run.as_ref() == Some(cohort))
                })
                .then(|| {
                    let status = message
                        .run
                        .as_ref()
                        .and_then(|run| self.state.runs.iter().find(|r| &r.id == run))
                        .map_or(RunStatus::Completed, |run| run.status);
                    (message.id.clone(), status)
                })
        })
    }
    fn wake_tasks(&mut self) {
        let Some(thread) = &self.state.thread else {
            return;
        };
        if thread.archived_at.is_some() || thread.deleted_at.is_some() {
            return;
        }
        let thread = thread.id.clone();
        let cohorts = self
            .state
            .tasks
            .iter()
            .filter_map(|task| task.run.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for cohort in cohorts {
            let task_ids = self.wake_eligible(&cohort);
            if task_ids.is_empty()
                || self
                    .cohort_wake(&cohort)
                    .is_some_and(|(_, status)| status != RunStatus::Queued && !status.terminal())
            {
                continue;
            }
            self.effect(
                None,
                EffectBody::SendToThread {
                    thread: thread.clone(),
                    command: Box::new(Command::AcceptTaskWake { task_ids }),
                },
            );
        }
    }
    /// Claimed completions are delivered once the run that carried them ends.
    fn settle_deliveries(&mut self, run: &RunId) {
        let delivered = self
            .state
            .messages
            .iter()
            .filter(|message| message.run.as_ref() == Some(run))
            .filter_map(|message| match &message.notification.as_ref()?.source {
                NotificationSource::Delegated { task_ids } => Some(task_ids.clone()),
                _ => None,
            })
            .flatten()
            .filter(|id| {
                self.state
                    .tasks
                    .iter()
                    .any(|task| &task.id == id && task.delivery == DeliveryState::Claimed)
            })
            .collect::<Vec<_>>();
        for id in delivered {
            self.fact(FactBody::TaskDeliveryChanged {
                id,
                state: DeliveryState::Delivered,
            });
        }
    }

    fn resolve_request(
        &mut self,
        id: &RuntimeRequestId,
        status: RequestStatus,
        decision: Option<ApprovalDecision>,
        card: ItemStatus,
    ) {
        self.fact(FactBody::RequestResolved {
            id: id.clone(),
            status,
            decision,
            answers: None,
            attachments: BTreeMap::new(),
        });
        self.complete_request_cards(id, card);
    }
    fn complete_request_cards(&mut self, request: &RuntimeRequestId, status: ItemStatus) {
        let items = self
            .state
            .items
            .iter()
            .filter(|i| {
                !i.status.terminal()
                    && matches!(&i.kind, ItemKind::ApprovalRequest { request: r } | ItemKind::UserInputRequest { request: r } if r == request)
            })
            .map(|i| i.id.clone())
            .collect::<Vec<_>>();
        for id in items {
            self.fact(FactBody::ItemCompleted { id, status });
        }
    }
    fn error_item_with_class(&mut self, run: &RunId, message: &str, class: Option<&str>) {
        self.error_item_coded(run, message, class, None);
    }
    fn error_item_coded(
        &mut self,
        run: &RunId,
        message: &str,
        class: Option<&str>,
        code: Option<&str>,
    ) {
        let id = TurnItemId::new(self.key("error", run.as_str())).unwrap();
        let attempt = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == run)
            .and_then(|r| r.attempt.clone());
        self.item_start(
            id.clone(),
            Some(run.clone()),
            attempt,
            String::new(),
            ItemKind::Error {
                message: provider_failure_message(message),
                retry: None,
                code: code.and_then(provider_failure_code),
                class: class.map(str::to_owned),
                retryable: code.map(|_| false),
                reset_at: None,
            },
        );
        self.fact(FactBody::ItemCompleted {
            id,
            status: ItemStatus::Failed,
        });
    }
    fn native_key(&self, kind: &str, attempt: &RunAttemptId, key: &str) -> String {
        let thread = &self.state.thread.as_ref().unwrap().id;
        format!(
            "{kind}:{}:{thread}:{}:{attempt}:{key}",
            thread.as_str().len(),
            attempt.as_str().len()
        )
    }
    fn provider_item(
        &mut self,
        attempt: &RunAttemptId,
        run: Option<&RunId>,
        key: &str,
        kind: &ProviderItem,
    ) -> TurnItemId {
        let id = TurnItemId::new(self.native_key("item", attempt, key)).unwrap();
        if let ProviderItem::UsageLimit { limit, resets_at } = kind
            && self.state.attempts.iter().any(|a| &a.id == attempt)
        {
            self.fact(FactBody::RateLimitRejected {
                attempt: attempt.clone(),
                limit: limit.clone().unwrap_or_default(),
                resets_at: *resets_at,
            });
        }
        let item_kind = match kind {
            ProviderItem::Text => {
                let message = MessageId::new(self.native_key("message", attempt, key)).unwrap();
                if !self.state.messages.iter().any(|m| m.id == message) {
                    self.fact(FactBody::MessageCreated {
                        id: message.clone(),
                        run: run.cloned(),
                        role: Role::Assistant,
                        text: String::new(),
                        attachments: vec![],
                        intent: InputIntent::TurnStart,
                        created_by: MessageAuthor::Agent,
                        creation_source: "provider".into(),
                        context: None,
                    });
                }
                ItemKind::AssistantMessage { message }
            }
            ProviderItem::Reasoning => ItemKind::Reasoning,
            ProviderItem::Command {
                command,
                cwd,
                exit_code,
            } => ItemKind::CommandExecution {
                command: command.clone(),
                cwd: cwd.clone(),
                exit_code: *exit_code,
                title: None,
            },
            ProviderItem::FileChange { changes } => ItemKind::FileChange {
                changes: changes.clone(),
            },
            ProviderItem::Tool {
                presentation,
                name,
                input,
                output,
            } => ItemKind::DynamicTool {
                presentation: presentation.clone(),
                name: name.clone(),
                input: input.clone(),
                output: output.clone(),
            },
            ProviderItem::WebSearch { query, results } => ItemKind::WebSearch {
                query: query.clone(),
                results: results.clone(),
            },
            ProviderItem::Compaction { before, after } => ItemKind::Compaction {
                before: *before,
                after: *after,
            },
            ProviderItem::Notice { message } => ItemKind::SystemNotice {
                message: message.clone(),
            },
            ProviderItem::UsageLimit { limit, resets_at } => ItemKind::SystemNotice {
                message: usage_limit_notice(limit.as_deref(), *resets_at, self.at.millis()),
            },
            ProviderItem::Error {
                message,
                retry,
                code,
                class,
                retryable,
            } => ItemKind::Error {
                message: provider_failure_message(message),
                retry: retry.clone(),
                code: code.as_deref().and_then(provider_failure_code),
                class: class.clone(),
                retryable: *retryable,
                reset_at: (class.as_deref() == Some("usage_limit"))
                    .then(|| self.usage_limit_reset(attempt))
                    .flatten(),
            },
        };
        if let Some(item) = self.state.items.iter().find(|i| i.id == id) {
            if item.kind != item_kind {
                self.fact(FactBody::ItemDetailChanged {
                    id: id.clone(),
                    kind: item_kind,
                });
            }
        } else {
            self.item_start(
                id.clone(),
                run.cloned(),
                Some(attempt.clone()),
                key.to_owned(),
                item_kind,
            );
        }
        id
    }
    fn ensure_plan(
        &mut self,
        run: &RunId,
        attempt: &RunAttemptId,
        key: &str,
        kind: PlanKind,
    ) -> (PlanId, TurnItemId) {
        let id = PlanId::new(self.native_key("plan", attempt, key)).unwrap();
        let item = TurnItemId::new(self.native_key("plan-item", attempt, key)).unwrap();
        if !self.state.plans.iter().any(|p| p.id == id) {
            self.fact(FactBody::PlanStarted {
                id: id.clone(),
                run: run.clone(),
                native_key: key.into(),
                kind,
            });
            self.item_start(
                item.clone(),
                Some(run.clone()),
                Some(attempt.clone()),
                key.into(),
                match kind {
                    PlanKind::Proposed => ItemKind::ProposedPlan { plan: id.clone() },
                    PlanKind::Todo => ItemKind::TodoList { plan: id.clone() },
                },
            );
        }
        (id, item)
    }
    fn provider(&mut self, attempt: &RunAttemptId, event: &ProviderEvent) -> Reply {
        if self
            .state
            .thread
            .as_ref()
            .is_some_and(|thread| thread.deleted_at.is_some())
        {
            return Reply::Ignored;
        }
        if let ProviderEvent::NativeOutput {
            echoed_prompts,
            acknowledged_prompt,
            root,
            result,
            events,
        } = event
        {
            return self.native_output(
                attempt,
                echoed_prompts,
                acknowledged_prompt.as_deref(),
                *root,
                result.as_ref(),
                events,
            );
        }
        let child = self.state.native_owner.as_ref() == Some(attempt);
        let run = self
            .state
            .runs
            .iter()
            .find(|r| r.attempt.as_ref() == Some(attempt))
            .cloned();
        if !child && run.is_none() {
            return Reply::Ignored;
        }
        let retained = |key: &String| {
            self.state
                .background_work
                .values()
                .any(|work| &work.attempt == attempt && (&work.key == key || &work.tool == key))
                || self.state.items.iter().any(|item| {
                    item.attempt.as_ref() == Some(attempt)
                        && &item.native_key == key
                        && item.persistent_tool()
                })
        };
        let background = matches!(event, ProviderEvent::RequestOpened { owner_path, .. } if !owner_path.is_empty())
            || matches!(event, ProviderEvent::ItemFinished { key, .. } | ProviderEvent::TextDelta { key, .. } if retained(key))
            || matches!(
                event,
                ProviderEvent::SubagentStarted { .. }
                    | ProviderEvent::UsageTotals { .. }
                    | ProviderEvent::SubagentNativeBound { .. }
                    | ProviderEvent::SubagentNamed { .. }
                    | ProviderEvent::SubagentProgress { .. }
                    | ProviderEvent::SubagentFinished { .. }
                    | ProviderEvent::Child { .. }
                    | ProviderEvent::BackgroundTask { .. }
                    | ProviderEvent::BackgroundRoster { .. }
                    | ProviderEvent::Wake { .. }
                    | ProviderEvent::SessionClosed { .. }
                    | ProviderEvent::RateLimits { .. }
            );
        if !child
            && run
                .as_ref()
                .is_some_and(|r| !matches!(r.status, RunStatus::Starting | RunStatus::Running))
            && !(background
                && run.as_ref().is_some_and(|r| {
                    matches!(
                        r.status,
                        RunStatus::Completed | RunStatus::Waiting | RunStatus::Interrupted
                    )
                }))
            && !matches!(event, ProviderEvent::RateLimits { .. })
        {
            return Reply::Ignored;
        }
        let run_id = run.as_ref().map(|r| &r.id);
        use ProviderEvent::*;
        match event {
            AssistantCursor { key } => {
                if run.is_some() {
                    self.fact(FactBody::AttemptHeadRecorded {
                        attempt: attempt.clone(),
                        head: Some(key.clone()),
                    });
                }
            }
            ResultText { key, text, status } => {
                let previous = self
                    .state
                    .items
                    .iter()
                    .rev()
                    .find(|i| matches!(i.kind, ItemKind::AssistantMessage { .. }))
                    .map(|i| i.text.as_str())
                    .unwrap_or("");
                if text.trim().is_empty()
                    || *status == ItemStatus::Completed
                        && previous.split_whitespace().eq(text.split_whitespace())
                {
                    return Reply::Ignored;
                }
                return self.provider(
                    attempt,
                    &ProviderEvent::ItemFinished {
                        key: key.clone(),
                        kind: ProviderItem::Text,
                        text: Some(text.clone()),
                        status: *status,
                    },
                );
            }
            SubagentNamed { key, title } => {
                if let Some(task) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .cloned()
                    && task.title.as_ref() != Some(title)
                {
                    self.fact(FactBody::TaskNamed {
                        id: task.id,
                        title: title.clone(),
                    });
                    self.effect(
                        None,
                        EffectBody::SendToThread {
                            thread: task.child_thread,
                            command: Box::new(Command::Rename {
                                title: title.clone(),
                            }),
                        },
                    );
                }
            }
            RateLimits { resets_at } => {
                let instance = run.as_ref().map_or_else(
                    || {
                        self.state
                            .thread
                            .as_ref()
                            .unwrap()
                            .selection
                            .instance
                            .clone()
                    },
                    |run| run.selection.instance.clone(),
                );
                if self.state.rate_limit_resets.get(&instance) != Some(resets_at) {
                    self.fact(FactBody::RateLimitsReported {
                        instance: instance.clone(),
                        resets_at: *resets_at,
                    });
                }
                // A stopped turn's missing reset is filled once.
                let reset = resets_at.and_then(|at| Timestamp::from_millis(at * 1000).ok());
                let unfilled = self
                    .state
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(&item.kind, ItemKind::Error { class: Some(class), reset_at: None, .. }
                            if class == "usage_limit")
                            && item.run.as_ref().is_some_and(|id| {
                                self.state.runs.iter().any(|run| {
                                    &run.id == id && run.selection.instance == instance
                                })
                            })
                    })
                    .map(|item| (item.id.clone(), item.kind.clone()))
                    .collect::<Vec<_>>();
                if let Some(reset) = reset {
                    for (id, mut kind) in unfilled {
                        if let ItemKind::Error { reset_at, .. } = &mut kind {
                            *reset_at = Some(reset.clone());
                        }
                        self.fact(FactBody::ItemDetailChanged { id, kind });
                    }
                }
            }
            PromptOffered { key } => self.fact(FactBody::PromptOffered {
                attempt: attempt.clone(),
                key: key.clone(),
            }),
            NativeOutput { .. } => {
                unreachable!("native output is routed before applying its events")
            }
            SessionClosed { error } => {
                // The process that could answer them is gone.
                let expired = self
                    .state
                    .requests
                    .iter()
                    .filter(|r| {
                        &r.attempt == attempt
                            && r.status == RequestStatus::Pending
                            && r.capability != ResponseCapability::Message
                    })
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                for id in expired {
                    self.resolve_request(&id, RequestStatus::Expired, None, ItemStatus::Failed);
                }
                let background = self
                    .state
                    .background_work
                    .iter()
                    .filter(|(_, work)| &work.attempt == attempt)
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                for key in background {
                    self.fact(FactBody::BackgroundTaskFinished { key });
                }
                if run.as_ref().is_some_and(|run| {
                    !matches!(run.status, RunStatus::Starting | RunStatus::Running)
                }) {
                    let tools = self
                        .state
                        .items
                        .iter()
                        .filter(|item| {
                            item.attempt.as_ref() == Some(attempt) && item.persistent_tool()
                        })
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>();
                    for id in tools {
                        self.fact(FactBody::ItemCompleted {
                            id,
                            status: ItemStatus::Interrupted,
                        });
                    }
                    self.stop_tasks(
                        attempt,
                        if self.state.stopping.contains(attempt) {
                            ItemStatus::Interrupted
                        } else {
                            ItemStatus::Cancelled
                        },
                        true,
                    );
                    return Reply::Accepted;
                }

                if let Some(run) = &run {
                    let status = if self.state.stopping.contains(attempt) {
                        RunStatus::Interrupted
                    } else {
                        RunStatus::Failed
                    };
                    if let Some(error) = error {
                        self.provider(
                            attempt,
                            &ProviderEvent::ItemFinished {
                                key: self.native_key("session-error", attempt, "exit"),
                                kind: ProviderItem::Error {
                                    message: error.clone(),
                                    retry: None,
                                    code: None,
                                    class: Some("provider_error".into()),
                                    retryable: None,
                                },
                                text: None,
                                status: ItemStatus::Failed,
                            },
                        );
                    }
                    self.stop_tasks(
                        attempt,
                        if status == RunStatus::Interrupted {
                            ItemStatus::Interrupted
                        } else {
                            ItemStatus::Failed
                        },
                        true,
                    );
                    self.finish(&run.id, status, true);
                } else if child {
                    let status = if self.state.stopping.contains(attempt) {
                        RunStatus::Interrupted
                    } else {
                        RunStatus::Failed
                    };
                    self.stop_tasks(
                        attempt,
                        if status == RunStatus::Interrupted {
                            ItemStatus::Interrupted
                        } else {
                            ItemStatus::Failed
                        },
                        true,
                    );
                    self.provider(
                        attempt,
                        &ProviderEvent::TurnFinished {
                            status,
                            native_head: None,
                        },
                    );
                }
            }
            TurnAborted { .. } => {
                if let Some(run) = &run {
                    if !self.state.stopping.contains(attempt)
                        && self.state.messages.iter().any(|m| {
                            m.run.as_ref() == Some(&run.id)
                                && matches!(
                                    m.intent,
                                    InputIntent::Steer | InputIntent::PromotedQueuedToSteer
                                )
                        })
                    {
                        return Reply::Ignored;
                    }
                    self.finish(&run.id, RunStatus::Interrupted, true);
                }
            }
            UsageTotals {
                native_thread,
                native_turn,
                total,
                last,
            } => {
                if !child
                    && run.as_ref().is_some_and(|run| {
                        self.state.native_sessions.get(&run.selection.instance)
                            != Some(native_thread)
                    })
                {
                    return Reply::Ignored;
                }
                let delta =
                    codex_usage_delta(self.state.usage_baselines.get(native_thread), total, last);
                self.fact(FactBody::UsageBaselineChanged {
                    native_thread: native_thread.clone(),
                    counters: total.clone(),
                });
                if child && self.state.native_child_turn.as_deref() == Some(native_turn.as_str()) {
                    self.fact(FactBody::NativeUsageAdded {
                        counters: delta.clone(),
                    });
                }
                if let Some(a) = self.state.attempts.iter().find(|a| {
                    &a.id == attempt
                        && a.native_turn.as_deref() == Some(native_turn.as_str())
                        && a.status == AttemptStatus::Running
                }) {
                    self.fact(FactBody::UsageAdded {
                        attempt: a.id.clone(),
                        counters: delta,
                    });
                }
            }
            TurnUsage(usage) => {
                let mut usage = usage.clone();
                if self.state.stopping.contains(attempt) && usage.status == UsageStatus::Complete {
                    usage.status = UsageStatus::Partial;
                }
                usage.has_subagents = self.state.tasks.iter().any(|task| &task.attempt == attempt);
                if child {
                    self.fact(FactBody::NativeTurnUsageRecorded { usage });
                } else {
                    self.fact(FactBody::TurnUsageRecorded {
                        attempt: attempt.clone(),
                        usage,
                    });
                }
            }
            ContextUsage(usage) => {
                if child {
                    self.fact(FactBody::NativeContextUsageRecorded {
                        usage: usage.clone(),
                    });
                } else {
                    self.fact(FactBody::ContextUsageRecorded {
                        attempt: attempt.clone(),
                        usage: usage.clone(),
                    });
                }
            }
            ContextInjected => {
                if !child {
                    self.complete_context_delivery(attempt, ContextDeliveryStatus::Injected);
                }
            }
            SessionReady { native_thread } => {
                if !child {
                    self.fact(FactBody::SessionBound {
                        attempt: attempt.clone(),
                        native_thread: native_thread.clone(),
                    });
                }
            }
            TurnStarted { native_turn } => {
                if !child {
                    self.complete_context_delivery(attempt, ContextDeliveryStatus::Inline);
                }
                if child {
                    self.fact(FactBody::NativeChildTurnBound {
                        native_turn: native_turn.clone(),
                    });
                } else {
                    self.fact(FactBody::TurnBound {
                        attempt: attempt.clone(),
                        native_turn: native_turn.clone(),
                    });
                }
            }
            TurnFinished {
                status,
                native_head,
            } => {
                if !status.terminal() {
                    return reject("invalid-terminal-status");
                }
                if let Some(run) = run {
                    if native_head.is_some() {
                        self.fact(FactBody::AttemptHeadRecorded {
                            attempt: attempt.clone(),
                            head: native_head.clone(),
                        });
                    }
                    let status = if self.state.stopping.contains(attempt) {
                        RunStatus::Interrupted
                    } else {
                        *status
                    };
                    let head = if run.selection.driver == Driver::Claude
                        && status == RunStatus::Completed
                    {
                        None
                    } else {
                        native_head.clone()
                    };
                    self.fact(FactBody::NativeHeadChanged {
                        instance: run.selection.instance,
                        head,
                    });
                    self.finish(&run.id, status, true);
                } else {
                    if self.state.thread.as_ref().unwrap().selection.driver == Driver::Codex {
                        let usage = complete_codex_usage(
                            self.state.native_usage_accumulator.as_ref(),
                            self.state.native_usage_observed,
                            *status == RunStatus::Completed,
                            !self.state.tasks.is_empty(),
                        );
                        self.fact(FactBody::NativeTurnUsageRecorded { usage });
                    }
                    self.close_attempt_items(
                        attempt,
                        match status {
                            RunStatus::Failed => ItemStatus::Failed,
                            RunStatus::Interrupted => ItemStatus::Interrupted,
                            RunStatus::Cancelled => ItemStatus::Cancelled,
                            _ => ItemStatus::Completed,
                        },
                    );
                    if let Some((parent, task)) = self.state.native_parent.clone() {
                        let boundary = self
                            .state
                            .items
                            .iter()
                            .rev()
                            .find(|item| matches!(item.kind, ItemKind::UserMessage { .. }))
                            .map_or(0, |item| item.ordinal);
                        let result = self
                            .state
                            .items
                            .iter()
                            .filter(|item| {
                                item.ordinal > boundary
                                    && matches!(item.kind, ItemKind::AssistantMessage { .. })
                                    && !item.text.trim().is_empty()
                            })
                            .max_by_key(|item| item.ordinal)
                            .map(|item| item.text.clone())
                            .unwrap_or_default();
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: parent,
                                command: Box::new(Command::TaskResult {
                                    source_message: None,
                                    generation: Some(self.state.native_generation),
                                    context: None,
                                    task,
                                    status: match status {
                                        RunStatus::Failed => ItemStatus::Failed,
                                        RunStatus::Interrupted => ItemStatus::Interrupted,
                                        RunStatus::Cancelled => ItemStatus::Cancelled,
                                        _ => ItemStatus::Completed,
                                    },
                                    result,
                                }),
                            },
                        );
                    }
                }
            }
            ItemStarted { key, kind } => {
                self.provider_item(attempt, run_id, key, kind);
            }
            TextDelta { key, kind, text } => {
                let id = self.provider_item(attempt, run_id, key, kind);
                let item = self.state.items.iter().find(|i| i.id == id).unwrap();
                if !item.status.terminal() {
                    self.append_text(&id, text);
                }
            }
            ItemFinished {
                key,
                kind,
                text,
                status,
            } => {
                let key = if key.is_empty() {
                    self.key("provider-item", &self.facts.len().to_string())
                } else {
                    key.clone()
                };
                let id = self.provider_item(attempt, run_id, &key, kind);
                if let Some(text) = text {
                    let current = self
                        .state
                        .items
                        .iter()
                        .find(|i| i.id == id)
                        .unwrap()
                        .text
                        .clone();
                    if *text != current {
                        if let Some(tail) = text.strip_prefix(current.as_str()) {
                            let tail = tail.to_owned();
                            self.append_text(&id, &tail);
                        } else {
                            self.replace_text(&id, text);
                        }
                    }
                }
                if !self
                    .state
                    .items
                    .iter()
                    .find(|i| i.id == id)
                    .unwrap()
                    .status
                    .terminal()
                {
                    self.fact(FactBody::ItemCompleted {
                        id,
                        status: *status,
                    });
                }
            }
            RequestOpened {
                owner_path,
                key,
                body,
                capability,
            } => {
                let id = RuntimeRequestId::new(self.native_key("request", attempt, key)).unwrap();
                if self.state.requests.iter().any(|r| r.id == id) {
                    return Reply::Ignored;
                }
                self.fact(FactBody::RequestOpened {
                    owner_path: owner_path.clone(),
                    id: id.clone(),
                    attempt: attempt.clone(),
                    native_key: key.clone(),
                    body: body.clone(),
                    capability: *capability,
                });
                let kind = if matches!(body, RequestBody::Questions { .. }) {
                    ItemKind::UserInputRequest { request: id }
                } else {
                    ItemKind::ApprovalRequest { request: id }
                };
                self.item_start(
                    TurnItemId::new(self.native_key("request-item", attempt, key)).unwrap(),
                    run_id.cloned(),
                    Some(attempt.clone()),
                    key.clone(),
                    kind,
                );
            }
            RequestClosed { key } => {
                if let Some(id) = self
                    .state
                    .requests
                    .iter()
                    .find(|r| {
                        &r.native_key == key
                            && &r.attempt == attempt
                            && r.status == RequestStatus::Pending
                    })
                    .map(|r| r.id.clone())
                {
                    self.resolve_request(
                        &id,
                        RequestStatus::Cancelled,
                        None,
                        ItemStatus::Cancelled,
                    );
                }
            }
            UserMessage { key, text } => {
                if child {
                    let message = MessageId::new(self.native_key("message", attempt, key)).unwrap();
                    if !self.state.messages.iter().any(|m| m.id == message) {
                        self.fact(FactBody::MessageCreated {
                            id: message.clone(),
                            run: None,
                            role: Role::User,
                            text: text.clone(),
                            attachments: vec![],
                            intent: InputIntent::TurnStart,
                            created_by: MessageAuthor::Agent,
                            creation_source: "provider".into(),
                            context: None,
                        });
                        let item = TurnItemId::new(self.native_key("item", attempt, key)).unwrap();
                        self.item_start(
                            item.clone(),
                            None,
                            Some(attempt.clone()),
                            key.clone(),
                            ItemKind::UserMessage { message },
                        );
                        self.append_text(&item, text);
                        self.fact(FactBody::ItemCompleted {
                            id: item,
                            status: ItemStatus::Completed,
                        });
                    }
                }
            }
            PlanDelta { key, text } => {
                let Some(run) = run_id else {
                    return Reply::Ignored;
                };
                let (id, item) = self.ensure_plan(run, attempt, key, PlanKind::Proposed);
                self.append_plan(&id, text);
                self.append_text(&item, text);
            }
            Plan {
                kind,
                key,
                markdown,
                steps,
            } => {
                let Some(run) = run_id else {
                    return Reply::Ignored;
                };
                let (id, item) = self.ensure_plan(run, attempt, key, *kind);
                if self
                    .state
                    .plans
                    .iter()
                    .find(|p| p.id == id)
                    .unwrap()
                    .markdown
                    != *markdown
                {
                    let chunks = text_chunks(markdown);
                    self.fact(FactBody::PlanMarkdownReplaced {
                        id: id.clone(),
                        text: chunks.first().copied().unwrap_or_default().to_owned(),
                    });
                    for chunk in chunks.iter().skip(1) {
                        self.append_plan(&id, chunk);
                    }
                    self.replace_text(&item, markdown);
                }
                if self.state.plans.iter().find(|p| p.id == id).unwrap().steps != *steps {
                    self.fact(FactBody::PlanStepsReplaced {
                        id,
                        steps: steps.clone(),
                    });
                }
            }
            Usage(usage) => {
                if !child {
                    self.fact(FactBody::UsageRecorded {
                        attempt: attempt.clone(),
                        usage: usage.clone(),
                    });
                }
            }
            SubagentNativeBound { key, native_task } => {
                if let Some(id) = self
                    .state
                    .tasks
                    .iter()
                    .find(|task| &task.native_key == key)
                    .map(|t| t.id.clone())
                {
                    self.fact(FactBody::TaskNativeBound {
                        id,
                        native_task: native_task.clone(),
                    });
                }
            }
            ModelObserved { model } => {
                if child {
                    let mut selection = self.state.thread.as_ref().unwrap().selection.clone();
                    if selection.model != *model {
                        selection.model = model.clone();
                        self.fact(FactBody::ModelSelected { selection });
                    }
                    if let Some((parent, task)) = self.state.native_parent.clone() {
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: parent,
                                command: Box::new(Command::TaskProgress {
                                    task,
                                    progress: None,
                                    model: Some(model.clone()),
                                }),
                            },
                        );
                    }
                }
            }
            SubagentStarted {
                background,
                native_thread,
                key,
                parent,
                prompt,
                model,
            } => {
                if let Some(task) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .cloned()
                {
                    if task.status.terminal() {
                        self.fact(FactBody::TaskReopened {
                            id: task.id.clone(),
                            run: run_id.cloned(),
                            attempt: attempt.clone(),
                            prompt: prompt.clone(),
                        });
                    }
                    let generation = self
                        .state
                        .tasks
                        .iter()
                        .find(|candidate| candidate.id == task.id)
                        .map_or(0, |task| task.generation);
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: task.child_thread.clone(),
                            command: Box::new(Command::BindNativeChild {
                                native_thread: native_thread.clone(),
                                owner: attempt.clone(),
                                parent: self.state.thread.as_ref().unwrap().id.clone(),
                                task: task.id,
                                generation,
                            }),
                        },
                    );
                    if task.status.terminal() && !prompt.is_empty() {
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: task.child_thread,
                                command: Box::new(Command::NativeInput {
                                    attempt: attempt.clone(),
                                    event: Box::new(ProviderEvent::UserMessage {
                                        key: self.key("resume-prompt", key),
                                        text: prompt.clone(),
                                    }),
                                }),
                            },
                        );
                    }
                } else {
                    let id = NodeId::new(self.native_key("task", attempt, key)).unwrap();
                    let child_thread =
                        ThreadId::new(self.native_key("child", attempt, key)).unwrap();
                    let parent_task = parent
                        .as_ref()
                        .and_then(|p| self.state.tasks.iter().find(|t| &t.native_key == p))
                        .map(|t| t.id.clone())
                        .or_else(|| {
                            parent.as_ref().and(
                                self.state
                                    .native_parent
                                    .as_ref()
                                    .map(|(_, task)| task.clone()),
                            )
                        });
                    self.fact(FactBody::TaskStarted {
                        original_message: None,
                        background: *background,
                        id: id.clone(),
                        native_key: key.clone(),
                        run: run_id.cloned(),
                        attempt: attempt.clone(),
                        child: child_thread.clone(),
                        parent: parent_task,
                        prompt: prompt.clone(),
                        model: model.clone(),
                        wake: CompletionWake::Always,
                    });
                    self.item_start(
                        TurnItemId::new(self.native_key("task-item", attempt, key)).unwrap(),
                        run_id.cloned(),
                        Some(attempt.clone()),
                        key.clone(),
                        ItemKind::Subagent { task: id.clone() },
                    );
                    let t = self.state.thread.as_ref().unwrap().clone();
                    let mut selection = t.selection;
                    if let Some(model) = model {
                        selection.model = model.clone();
                    }
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: child_thread.clone(),
                            command: Box::new(Command::Create {
                                thread: child_thread.clone(),
                                project: t.project,
                                title: prompt.clone(),
                                selection,
                                runtime_mode: t.runtime_mode,
                                interaction_mode: t.interaction_mode,
                                workspace: t.workspace.clone(),
                                created_by: MessageAuthor::Agent,
                                creation_source: "provider".into(),
                            }),
                        },
                    );
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: child_thread.clone(),
                            command: Box::new(Command::BindNativeChild {
                                native_thread: native_thread.clone(),
                                owner: attempt.clone(),
                                parent: t.id,
                                task: id,
                                generation: 0,
                            }),
                        },
                    );
                    if !prompt.is_empty() {
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: child_thread.clone(),
                                command: Box::new(Command::NativeInput {
                                    attempt: attempt.clone(),
                                    event: Box::new(ProviderEvent::UserMessage {
                                        key: "spawn-prompt".into(),
                                        text: prompt.clone(),
                                    }),
                                }),
                            },
                        );
                    }
                    if let Some(events) = self
                        .state
                        .pending_children
                        .get(&self.native_key("pending", attempt, key))
                        .cloned()
                    {
                        for event in events {
                            self.effect(
                                Some(attempt.clone()),
                                EffectBody::SendToThread {
                                    thread: child_thread.clone(),
                                    command: Box::new(Command::NativeInput {
                                        attempt: attempt.clone(),
                                        event: Box::new(event),
                                    }),
                                },
                            );
                        }
                        self.fact(FactBody::ChildEventsReleased {
                            key: self.native_key("pending", attempt, key),
                        });
                    }
                }
            }
            SubagentProgress {
                key,
                progress,
                model,
            } => {
                if let Some(id) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .map(|t| t.id.clone())
                {
                    self.fact(FactBody::TaskProgressed {
                        id,
                        progress: Some(progress.clone()),
                        model: model.clone(),
                    });
                }
            }
            SubagentFinished {
                key,
                status,
                result,
            } => {
                if let Some(task) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .cloned()
                {
                    if !result.is_empty() {
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: task.child_thread.clone(),
                                command: Box::new(Command::NativeInput {
                                    attempt: attempt.clone(),
                                    event: Box::new(ProviderEvent::ResultText {
                                        key: format!("{key}:result"),
                                        text: result.clone(),
                                        status: *status,
                                    }),
                                }),
                            },
                        );
                    }
                    self.finish_task(&task.id, *status, result);
                    self.wake_tasks();
                }
            }
            Child { key, event } => {
                if let Some(task) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .cloned()
                {
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: task.child_thread,
                            command: Box::new(Command::NativeInput {
                                attempt: task.attempt,
                                event: event.clone(),
                            }),
                        },
                    );
                } else {
                    self.fact(FactBody::ChildEventDeferred {
                        key: self.native_key("pending", attempt, key),
                        event: event.clone(),
                    });
                }
            }
            BackgroundTask {
                key,
                tool,
                kind,
                description,
                status,
                summary,
                exit_code,
            } => {
                if let Some(status) = status {
                    // Work the user stopped does not report back. A roster
                    // snapshot may have dropped the work before its report.
                    let work = self.state.background_work.get(key).cloned();
                    let owner = work.as_ref().map_or(attempt, |work| &work.attempt);
                    if !self.state.stopping.contains(owner) {
                        self.fact(FactBody::NativeWorkReported {
                            key: key.clone(),
                            report: WorkReport {
                                kind: work.as_ref().map_or(*kind, |work| work.kind),
                                label: Some(
                                    work.map_or_else(
                                        || description.clone(),
                                        |work| work.description,
                                    ),
                                ),
                                outcome: (*status).into(),
                                child_thread: None,
                                exit_code: *exit_code,
                            },
                            text: summary.clone().unwrap_or_default(),
                        });
                    }
                    if self.state.background_work.contains_key(key) {
                        self.fact(FactBody::BackgroundTaskFinished { key: key.clone() });
                    }
                } else {
                    self.fact(FactBody::BackgroundTaskStarted {
                        key: key.clone(),
                        tool: tool.clone(),
                        description: description.clone(),
                        kind: *kind,
                        attempt: attempt.clone(),
                    });
                }
            }
            BackgroundRoster { tasks } => {
                let removed = self
                    .state
                    .background_work
                    .keys()
                    .filter(|key| !tasks.iter().any(|task| &task.key == *key))
                    .cloned()
                    .collect::<Vec<_>>();
                for key in removed {
                    self.fact(FactBody::BackgroundTaskFinished { key });
                }
                for task in tasks {
                    if !self.state.background_work.contains_key(&task.key) {
                        self.fact(FactBody::BackgroundTaskStarted {
                            key: task.key.clone(),
                            tool: task.tool.clone(),
                            description: task.description.clone(),
                            kind: task.kind,
                            attempt: attempt.clone(),
                        });
                    }
                }
            }
            Wake { text, detail } => {
                let message = SendMessage {
                    context: None,
                    created_by: MessageAuthor::Agent,
                    creation_source: "provider".into(),
                    id: MessageId::new(self.key("wake", &self.facts.len().to_string())).unwrap(),
                    text: text.clone(),
                    attachments: vec![],
                    selection: run.as_ref().map(|r| r.selection.clone()),
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                    resolved_plan: None,
                    continuation: None,
                    title_seed: None,
                };
                let notification = background_notification(
                    &self
                        .state
                        .wake_reports
                        .iter()
                        .map(|record| record.report.clone())
                        .collect::<Vec<_>>(),
                )
                .map(|mut notification| {
                    notification.detail = detail.clone();
                    notification
                });
                if !matches!(self.create_run(&message), Reply::Rejected { .. }) {
                    if let Some(notification) = notification {
                        self.fact(FactBody::MessageNotificationAssigned {
                            id: message.id.clone(),
                            notification,
                        });
                    }
                    self.fact(FactBody::WakeReportsConsumed);
                }
            }
        }
        Reply::Accepted
    }
    fn queue_native_turn(
        &mut self,
        owner: &RunAttemptId,
        events: &[ProviderEvent],
    ) -> Option<RunAttemptId> {
        let selection = self
            .state
            .runs
            .iter()
            .find(|r| r.attempt.as_ref() == Some(owner))?
            .selection
            .clone();
        let message = MessageId::new(self.key("continuation", owner.as_str())).unwrap();
        let run = RunId::new(format!("run:{}:{}", message.as_str().len(), message)).unwrap();
        let ordinal = self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0) + 1;
        self.fact(FactBody::MessageCreated {
            id: message.clone(),
            run: Some(run.clone()),
            role: Role::User,
            text: if self.state.wake_reports.is_empty() {
                "Background task completed.".into()
            } else {
                self.state
                    .wake_reports
                    .iter()
                    .map(|report| report.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            attachments: vec![],
            intent: InputIntent::QueuedTurn,
            created_by: MessageAuthor::Agent,
            creation_source: "provider".into(),
            context: None,
        });
        if let Some(notification) = background_notification(
            &self
                .state
                .wake_reports
                .iter()
                .map(|record| record.report.clone())
                .collect::<Vec<_>>(),
        ) {
            self.fact(FactBody::MessageNotificationAssigned {
                id: message.clone(),
                notification,
            });
        }
        self.fact(FactBody::WakeReportsConsumed);
        self.fact(FactBody::RunRequested {
            id: run.clone(),
            message,
            ordinal,
            selection,
            status: RunStatus::Queued,
            queue_position: Some(0),
            held: false,
            source_plan: None,
        });
        self.fact(FactBody::NativeContinuationQueued {
            run: run.clone(),
            events: events.to_vec(),
        });
        self.promote();
        self.state
            .runs
            .iter()
            .find(|r| r.id == run)
            .and_then(|r| r.attempt.clone())
    }
    fn native_output(
        &mut self,
        owner: &RunAttemptId,
        echoes: &[String],
        ack: Option<&str>,
        root: bool,
        result: Option<&NativeResult>,
        events: &[ProviderEvent],
    ) -> Reply {
        if root && self.state.stopping.contains(owner) {
            let mut reply = Reply::Ignored;
            for event in events {
                if self.provider(owner, event) != Reply::Ignored {
                    reply = Reply::Accepted;
                }
            }
            return reply;
        }

        if !root
            && self
                .state
                .tasks
                .iter()
                .any(|task| &task.attempt == owner && !task.status.terminal())
        {
            for event in events {
                self.provider(owner, event);
            }
            return Reply::Accepted;
        }
        if !self.state.attempts.iter().any(|a| {
            &a.id == owner
                && !matches!(
                    a.status,
                    AttemptStatus::Superseded
                        | AttemptStatus::Cancelled
                        | AttemptStatus::Interrupted
                        | AttemptStatus::Failed
                )
        }) && self.state.native_owner.as_ref() != Some(owner)
        {
            return Reply::Ignored;
        }
        if root
            && let (Some(active), Some(source)) = (
                self.state.active_run(),
                self.state
                    .runs
                    .iter()
                    .find(|r| r.attempt.as_ref() == Some(owner)),
            )
            && active.selection.instance != source.selection.instance
        {
            return Reply::Ignored;
        }
        if let Some(prompt) = self.state.pending_prompt.clone().filter(|p| !p.confirmed) {
            if echoes.contains(&prompt.key) {
                if matches!(
                    self.state.prompt_echo_mode,
                    PromptEchoMode::Unknown | PromptEchoMode::Acknowledged
                ) {
                    self.fact(FactBody::PromptEchoModeLearned {
                        mode: if result.is_none() && prompt.frames_before_echo == 0 {
                            PromptEchoMode::Early
                        } else {
                            PromptEchoMode::ResultOnly
                        },
                    });
                }
                self.fact(FactBody::PromptConfirmed);
                self.fact(FactBody::PromptOutputReleased);
                for event in &prompt.held {
                    self.provider(&prompt.attempt, event);
                }
            } else {
                if self.state.prompt_echo_mode == PromptEchoMode::Unknown
                    && ack == Some(prompt.key.as_str())
                {
                    self.fact(FactBody::PromptEchoModeLearned {
                        mode: PromptEchoMode::Acknowledged,
                    });
                }
                if root {
                    self.fact(FactBody::PromptFrameObserved);
                    if let Some(result) = result {
                        let foreign = if echoes.is_empty() {
                            self.state.prompt_echo_mode != PromptEchoMode::Unknown
                                && result.origin.as_deref().is_some_and(|o| o != "human")
                        } else {
                            !echoes.contains(&prompt.key)
                        };
                        if foreign {
                            if result.turn_count == 0 {
                                return Reply::Ignored;
                            }
                            if self.state.prompt_echo_mode == PromptEchoMode::Early {
                                let mut held = prompt.held;
                                held.extend_from_slice(events);
                                self.fact(FactBody::PromptOutputReleased);
                                self.queue_native_turn(owner, &held);
                            }
                            return Reply::Accepted;
                        }
                        self.fact(FactBody::PromptConfirmed);
                        self.fact(FactBody::PromptOutputReleased);
                        for event in &prompt.held {
                            self.provider(&prompt.attempt, event);
                        }
                    } else if self.state.prompt_echo_mode == PromptEchoMode::Early {
                        self.fact(FactBody::PromptOutputHeld {
                            events: events.to_vec(),
                        });
                        return Reply::Accepted;
                    }
                }
            }
        }
        let target = self.state.active_run().and_then(|r| r.attempt.clone());
        if root && target.is_none() && result.is_none() {
            self.queue_native_turn(owner, events);
        } else if root && target.is_none() {
            return Reply::Ignored;
        } else {
            let target = if root {
                target.as_ref().unwrap_or(owner)
            } else {
                owner
            };
            for event in events {
                self.provider(target, event);
            }
        }
        Reply::Accepted
    }
    fn effect_result(&mut self, result: &EffectResult) -> Reply {
        match result {
            EffectResult::NativeForked {
                attempt,
                native_thread,
            } => {
                let Some((run, transfer)) = self.awaiting_fork(attempt) else {
                    return Reply::Ignored;
                };
                let source = transfer.native_source.clone().unwrap();
                // A Claude fork's transcript ends at the head, so its first
                // query resumes without one.
                self.fact(FactBody::NativeSessionBound {
                    instance: run.selection.instance.clone(),
                    native_thread: native_thread.clone(),
                    head: source
                        .head
                        .filter(|_| run.selection.driver != Driver::Claude),
                });
                self.fact(FactBody::TransferDeliveryChanged {
                    id: transfer.id.clone(),
                    delivery: ContextDelivery {
                        attempt: attempt.clone(),
                        run: run.id.clone(),
                        native_thread: Some(native_thread.clone()),
                        status: ContextDeliveryStatus::NativeFork,
                        item_ids: transfer
                            .history
                            .messages
                            .iter()
                            .map(|message| message.item.clone())
                            .collect(),
                        omitted_item_ids: transfer.history.omitted_item_ids.clone(),
                    },
                });
                self.dispatch_start(&run.id, attempt, true);
                return Reply::Accepted;
            }
            // A source run that is no longer completed or waiting is handed
            // over as portable context instead.
            EffectResult::ForkSourceChanged { attempt } => {
                let Some((run, transfer)) = self.awaiting_fork(attempt) else {
                    return Reply::Ignored;
                };
                self.fact(FactBody::TransferOpened {
                    native_source: None,
                    id: ContextTransferId::new(self.key("portable-fork", transfer.id.as_str()))
                        .unwrap(),
                    kind: TransferKind::Fork,
                    source: transfer.source.clone(),
                    target: transfer.target.clone(),
                    boundary: transfer.boundary,
                    instance: None,
                    target_run: None,
                    history: transfer.history.clone(),
                });
                self.dispatch_start(&run.id, attempt, true);
                return Reply::Accepted;
            }
            // A native fork that fails on the last attempt fails the run; the
            // transfer stays pending.
            EffectResult::ForkFailed { attempt, message } => {
                let Some((run, _)) = self.awaiting_fork(attempt) else {
                    return Reply::Ignored;
                };
                self.error_item_with_class(&run.id, message, Some("provider_error"));
                self.finish(&run.id, RunStatus::Failed, false);
            }
            EffectResult::ProviderFailed {
                attempt,
                operation,
                message,
                message_id,
                turn_completed,
                session_lost,
            } => {
                let Some(run) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| r.attempt.as_ref() == Some(attempt))
                    .cloned()
                else {
                    return Reply::Ignored;
                };
                if *operation == ProviderOperation::Steer && *turn_completed {
                    if let Some(id) = message_id
                        && let Some(m) = self.state.messages.iter().find(|m| &m.id == id).cloned()
                    {
                        // Reuse the accepted message and its timeline row as a new turn on
                        // the thread's saved selection.
                        let next =
                            RunId::new(format!("followup:{}:{}", id.as_str().len(), id)).unwrap();
                        if self.state.runs.iter().any(|r| r.id == next) {
                            return Reply::Ignored;
                        }
                        let ordinal =
                            self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0) + 1;
                        let queued =
                            self.state.active_run().is_some() || !self.state.captures.is_empty();
                        let held = queued && self.state.queued_runs().iter().any(|r| r.queue_held);
                        self.fact(FactBody::MessageAdopted {
                            id: m.id.clone(),
                            run: next.clone(),
                            intent: if queued {
                                InputIntent::QueuedTurn
                            } else {
                                InputIntent::TurnStart
                            },
                        });
                        self.fact(FactBody::RunRequested {
                            id: next.clone(),
                            message: m.id.clone(),
                            ordinal,
                            selection: self.state.thread.as_ref().unwrap().selection.clone(),
                            status: if queued {
                                RunStatus::Queued
                            } else {
                                RunStatus::Starting
                            },
                            queue_position: queued
                                .then(|| self.state.queued_runs().len() as u64 + 1),
                            held,
                            source_plan: None,
                        });
                        if let Some(item) = self
                            .state
                            .items
                            .iter()
                            .find(|i| matches!(&i.kind, ItemKind::UserMessage { message } if message == &m.id))
                            .map(|i| i.id.clone())
                        {
                            self.fact(FactBody::ItemMoved {
                                id: item,
                                run: next.clone(),
                                ordinal: (!queued).then(|| self.item_ordinal()),
                            });
                        }
                        if !queued {
                            self.start_run(&next);
                        }
                        return Reply::Run(next);
                    }
                    return Reply::Ignored;
                }
                if !run.status.blocking() {
                    return Reply::Ignored;
                }
                if *operation == ProviderOperation::Start
                    && *session_lost
                    && !self.state.stopping.contains(attempt)
                    && self
                        .state
                        .native_sessions
                        .contains_key(&run.selection.instance)
                {
                    self.fact(FactBody::AttemptFinished {
                        id: attempt.clone(),
                        status: AttemptStatus::Failed,
                    });
                    self.fact(FactBody::NativeSessionCleared {
                        instance: run.selection.instance.clone(),
                    });
                    self.start_run(&run.id);
                    return Reply::Accepted;
                }
                match operation {
                    ProviderOperation::Start | ProviderOperation::Compact => {
                        self.error_item_with_class(&run.id, message, Some("provider_error"));
                        self.stop_tasks(attempt, ItemStatus::Failed, true);
                        self.finish(&run.id, RunStatus::Failed, false);
                    }
                    // Failed control requests keep the native turn authoritative.
                    _ => {
                        self.error_item_with_class(&run.id, message, Some("provider_error"));
                    }
                }
            }
            EffectResult::CheckpointCaptured {
                status: capture_status,
                baselines,
                run,
                attempt,
                checkpoint,
                file_ref,
                files,
            } => {
                let Some(r) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == run && &r.attempt == attempt)
                    .cloned()
                else {
                    return Reply::Ignored;
                };
                if r.status == RunStatus::RolledBack
                    || r.checkpoint.is_some()
                    || self.state.checkpoints.iter().any(|c| &c.id == checkpoint)
                {
                    return Reply::Ignored;
                }
                if r.checkpoint_scope.is_some()
                    && [0, r.ordinal - 1].iter().any(|ordinal| {
                        !self.state.checkpoints.iter().any(|checkpoint| {
                            checkpoint.scope == r.checkpoint_scope
                                && checkpoint.run_ordinal == *ordinal
                        }) && !baselines
                            .iter()
                            .any(|baseline| baseline.ordinal == *ordinal)
                    })
                {
                    return reject("incomplete-checkpoint-baseline");
                }
                if baselines
                    .iter()
                    .any(|baseline| baseline.ordinal >= r.ordinal)
                {
                    return reject("invalid-checkpoint-baseline");
                }
                for baseline in baselines {
                    if !self.state.checkpoints.iter().any(|checkpoint| {
                        checkpoint.status == CheckpointStatus::Ready
                            && (checkpoint.id == baseline.checkpoint
                                || checkpoint.scope == r.checkpoint_scope
                                    && checkpoint.run_ordinal == baseline.ordinal)
                    }) {
                        self.fact(FactBody::CheckpointCaptured {
                            status: baseline.status,
                            id: baseline.checkpoint.clone(),
                            scope: r.checkpoint_scope.clone(),
                            run: None,
                            run_ordinal: baseline.ordinal,
                            native_heads: baseline.native_heads.clone(),
                            file_ref: baseline.file_ref.clone(),
                            files: vec![],
                        });
                    }
                }
                self.fact(FactBody::CheckpointCaptured {
                    status: *capture_status,
                    scope: r.checkpoint_scope.clone(),
                    id: checkpoint.clone(),
                    run: Some(run.clone()),
                    run_ordinal: r.ordinal,
                    native_heads: r
                        .native_baseline_heads
                        .clone()
                        .into_iter()
                        .chain(
                            self.state
                                .runs
                                .iter()
                                .filter(|candidate| {
                                    candidate.ordinal <= r.ordinal
                                        && candidate.status != RunStatus::RolledBack
                                })
                                .filter_map(|candidate| {
                                    candidate
                                        .attempt
                                        .as_ref()
                                        .and_then(|id| {
                                            self.state
                                                .attempts
                                                .iter()
                                                .find(|attempt| &attempt.id == id)
                                        })
                                        .map(|attempt| {
                                            (
                                                candidate.selection.instance.clone(),
                                                attempt.native_head.clone(),
                                            )
                                        })
                                }),
                        )
                        .collect(),
                    file_ref: file_ref.clone(),
                    files: files.clone(),
                });
                if let Some(status) = self.state.captures.get(run).copied() {
                    self.fact(FactBody::RunFinished {
                        id: run.clone(),
                        status,
                    });
                    self.complete_delegation(run, status);
                    self.settle_deliveries(run);
                    self.hold_after_failure(run);
                    self.promote();
                    self.wake_tasks();
                }
            }
            EffectResult::RollbackFinished { command, bindings } => {
                let Some(pending) = self
                    .state
                    .rollback
                    .as_ref()
                    .filter(|p| &p.command == command)
                    .cloned()
                else {
                    return Reply::Ignored;
                };
                self.fact(FactBody::RolledBack {
                    command: command.clone(),
                    checkpoint: pending.checkpoint,
                });
                for binding in bindings {
                    self.fact(FactBody::NativeSessionBound {
                        instance: binding.instance.clone(),
                        native_thread: binding.thread.clone(),
                        head: binding.head.clone(),
                    });
                }
                self.promote();
            }
            EffectResult::RollbackFailed { command, message } => {
                let Some(pending) = self
                    .state
                    .rollback
                    .as_ref()
                    .filter(|p| &p.command == command)
                    .cloned()
                else {
                    return Reply::Ignored;
                };
                // A rewound native history no longer matches the kept conversation.
                for instance in &pending.rewinding {
                    if self.state.native_sessions.contains_key(instance) {
                        self.fact(FactBody::NativeSessionCleared {
                            instance: instance.clone(),
                        });
                    }
                }
                self.fact(FactBody::RollbackFailed {
                    command: command.clone(),
                    message: message.clone(),
                });
                self.promote();
            }
            EffectResult::ThreadCommandFailed {
                command, reason, ..
            } => match command.as_ref() {
                Command::AcceptDelegation { origin, .. } => {
                    if !self.state.tasks.iter().any(|task| {
                        task.id == origin.task
                            && task.original_message.as_ref() == Some(&origin.message)
                            && !task.status.terminal()
                    }) {
                        return Reply::Ignored;
                    }
                    self.finish_task(
                        &origin.task,
                        ItemStatus::Failed,
                        &format!("The delegated thread could not be started: {reason}"),
                    );
                    self.wake_tasks();
                }
                // A continuation that can never run settles its delegation as declined.
                Command::ContinueRestart { source, .. } => {
                    let Some(run) = self
                        .state
                        .runs
                        .iter()
                        .find(|run| &run.id == source)
                        .cloned()
                    else {
                        return Reply::Ignored;
                    };
                    if self
                        .state
                        .runs
                        .iter()
                        .any(|candidate| candidate.restart_of.as_ref() == Some(source))
                    {
                        return Reply::Ignored;
                    }
                    self.complete_delegation(source, run.status);
                }
                _ => return Reply::Ignored,
            },
            EffectResult::TitleGenerated { request, title } => {
                let Some(thread) =
                    self.state.thread.as_ref().filter(|thread| {
                        thread.title_request.as_ref().map(|r| &r.id) == Some(request)
                    })
                else {
                    return Reply::Ignored;
                };
                match title.as_deref().map(str::trim) {
                    Some(title)
                        if !title.is_empty() && title != "New thread" && title != thread.title =>
                    {
                        self.fact(FactBody::ThreadRenamed {
                            title: title.to_owned(),
                        })
                    }
                    _ => self.fact(FactBody::TitleRequestCleared),
                }
            }
        }
        Reply::Accepted
    }
    fn record_cancelled_background_work(&mut self) {
        let mut work: BTreeMap<String, Vec<CancelledBackgroundWork>> = BTreeMap::new();
        let mut native = std::collections::BTreeSet::new();
        for item in self
            .state
            .items
            .iter()
            .filter(|item| !item.status.terminal())
        {
            let (kind, label) = match &item.kind {
                ItemKind::CommandExecution { command, .. } => {
                    ("shell", compact_restart_label(command))
                }
                ItemKind::DynamicTool {
                    name,
                    input,
                    presentation,
                    ..
                } => (
                    if input.0["persistent"] == true {
                        "monitor"
                    } else {
                        "task"
                    },
                    presentation
                        .title
                        .as_deref()
                        .map(compact_restart_label)
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| compact_restart_label(name)),
                ),
                ItemKind::Subagent { task } => {
                    let Some(task) = self
                        .state
                        .tasks
                        .iter()
                        .find(|candidate| &candidate.id == task && !candidate.app_owned())
                    else {
                        continue;
                    };
                    (
                        "subagent",
                        task.title
                            .as_deref()
                            .map(compact_restart_label)
                            .filter(|title| !title.is_empty())
                            .unwrap_or_else(|| compact_restart_label(&task.prompt)),
                    )
                }
                _ => continue,
            };
            let instance = item
                .attempt
                .as_ref()
                .and_then(|id| self.state.attempts.iter().find(|attempt| &attempt.id == id))
                .and_then(|attempt| self.state.runs.iter().find(|run| run.id == attempt.run))
                .map(|run| &run.selection.instance);
            let Some(instance) = instance else {
                continue;
            };
            native.insert((instance.clone(), item.native_key.clone()));
            work.entry(instance.clone())
                .or_default()
                .push(CancelledBackgroundWork {
                    id: item.id.to_string(),
                    kind: kind.into(),
                    label: if label.is_empty() {
                        match kind {
                            "shell" => "background command",
                            "subagent" => "subagent",
                            _ => "background tool",
                        }
                        .into()
                    } else {
                        label
                    },
                });
        }
        for (key, task) in &self.state.background_work {
            let Some(run) = self
                .state
                .attempts
                .iter()
                .find(|attempt| attempt.id == task.attempt)
                .and_then(|attempt| self.state.runs.iter().find(|run| run.id == attempt.run))
            else {
                continue;
            };
            if native.contains(&(run.selection.instance.clone(), key.clone()))
                || native.contains(&(run.selection.instance.clone(), task.tool.clone()))
            {
                continue;
            }
            let description = compact_restart_label(&task.description);
            work.entry(run.selection.instance.clone())
                .or_default()
                .push(CancelledBackgroundWork {
                    id: key.clone(),
                    kind: match task.kind {
                        BackgroundKind::Command => "shell",
                        BackgroundKind::Monitor => "monitor",
                        BackgroundKind::Subagent => "subagent",
                        BackgroundKind::BackgroundTask => "task",
                    }
                    .into(),
                    label: compact_restart_label(&if description.is_empty() {
                        key.clone()
                    } else {
                        format!("{description} (id {key})")
                    }),
                });
        }
        for (instance, work) in work {
            let target = self
                .state
                .runs
                .iter()
                .filter(|run| {
                    run.selection.instance == instance
                        && !matches!(run.status, RunStatus::Queued | RunStatus::RolledBack)
                })
                .reduce(|latest, run| {
                    if run_ran_after(run, latest) {
                        run
                    } else {
                        latest
                    }
                })
                .map(|run| run.id.clone());
            if let Some(run) = target {
                self.fact(FactBody::RunBackgroundWorkCancelled { run, work });
            }
        }
    }
    fn recover(
        &mut self,
        trigger: RecoveryTrigger,
        continue_after_restart: bool,
        capturing: &BTreeSet<RunId>,
    ) {
        let continuation = continue_after_restart
            .then(|| self.state.active_run())
            .flatten()
            .filter(|run| {
                (run.status == RunStatus::Running
                    || run.status == RunStatus::Starting && run.restart_of.is_some())
                    && self.state.thread.as_ref().is_some_and(|thread| {
                        thread.archived_at.is_none()
                            && thread.deleted_at.is_none()
                            && thread.selection.instance == run.selection.instance
                    })
                    && self
                        .state
                        .native_sessions
                        .contains_key(&run.selection.instance)
                    && run
                        .attempt
                        .as_ref()
                        .is_none_or(|attempt| !self.state.stopping.contains(attempt))
            })
            .map(|run| run.id.clone());
        self.hold_queue();
        self.record_cancelled_background_work();
        self.fact(FactBody::BackgroundWorkStopped);
        let requests = self
            .state
            .requests
            .iter()
            .filter(|r| {
                r.status == RequestStatus::Pending && r.capability != ResponseCapability::Message
            })
            .map(|r| r.id.clone())
            .collect::<Vec<_>>();
        for id in requests {
            self.fact(FactBody::RequestResolved {
                id,
                status: if trigger == RecoveryTrigger::Startup {
                    RequestStatus::Expired
                } else {
                    RequestStatus::Cancelled
                },
                decision: None,
                answers: None,
                attachments: BTreeMap::new(),
            });
        }
        let runs = self
            .state
            .runs
            .iter()
            .filter(|r| {
                r.status.blocking()
                    && !(r.status == RunStatus::Waiting
                        && self.state.captures.contains_key(&r.id)
                        && capturing.contains(&r.id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for run in runs {
            if let Some(attempt) = &run.attempt
                && run.status != RunStatus::Waiting
            {
                self.fact(FactBody::AttemptFinished {
                    id: attempt.clone(),
                    status: AttemptStatus::Cancelled,
                });
                self.close_attempt_items(attempt, ItemStatus::Cancelled);
            }
            self.fact(FactBody::RunFinished {
                id: run.id.clone(),
                status: RunStatus::Cancelled,
            });
            if continuation.as_ref() != Some(&run.id) {
                self.complete_delegation(&run.id, RunStatus::Cancelled);
            }
        }
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|t| !t.app_owned() && !t.status.terminal())
            .map(|t| t.id.clone())
            .collect::<Vec<_>>();
        for id in tasks {
            self.fact(FactBody::TaskFinished {
                id: id.clone(),
                status: ItemStatus::Cancelled,
                result: format!(
                    "Cancelled because the server {} before the provider work completed.",
                    if trigger == RecoveryTrigger::Startup {
                        "restarted"
                    } else {
                        "shut down"
                    }
                ),
            });
            self.fact(FactBody::TaskDeliveryChanged {
                id,
                state: DeliveryState::Disposed,
            });
        }
        let items=self.state.items.iter().filter(|i| !i.status.terminal() && !matches!(&i.kind,ItemKind::Subagent { task } if self.state.tasks.iter().any(|t| &t.id==task && t.app_owned())) && !matches!(&i.kind,ItemKind::UserInputRequest { request } if self.state.requests.iter().any(|r| &r.id==request && r.capability==ResponseCapability::Message))).map(|i| i.id.clone()).collect::<Vec<_>>();
        for id in items {
            self.fact(FactBody::ItemCompleted {
                id,
                status: ItemStatus::Cancelled,
            });
        }
        let messages = self
            .state
            .messages
            .iter()
            .filter(|m| m.streaming)
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        for id in messages {
            self.fact(FactBody::MessageFinished { id });
        }
        if let Some(owner) = self.state.native_owner.clone() {
            self.provider(
                &owner,
                &ProviderEvent::TurnFinished {
                    status: RunStatus::Cancelled,
                    native_head: None,
                },
            );
            self.fact(FactBody::NativeChildClosed);
        }
        if let Some(source) = continuation {
            self.effect(
                None,
                EffectBody::SendToThread {
                    thread: self.state.thread.as_ref().unwrap().id.clone(),
                    command: Box::new(Command::ContinueRestart {
                        source,
                        enabled: true,
                    }),
                },
            );
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maintenance {
    Compact,
    Logout,
}
/// Native maintenance commands run as their own turn and never take restart
/// notes, title generation or steering.
pub fn maintenance(text: &str, attachments: &[Attachment]) -> Option<Maintenance> {
    if !attachments.is_empty() {
        return None;
    }
    match text.trim().to_lowercase().as_str() {
        "/compact" => Some(Maintenance::Compact),
        "/logout" => Some(Maintenance::Logout),
        _ => None,
    }
}
/// The latest executed run when it failed on a usage limit, with the limit's
/// reset.
pub fn usage_limit_failure(state: &State) -> Option<(&Run, Option<Timestamp>)> {
    let run = latest_executed_run(state).filter(|run| run.status == RunStatus::Failed)?;
    let failure = state
        .items
        .iter()
        .filter(|item| item.run.as_ref() == Some(&run.id) && item.status == ItemStatus::Failed)
        .filter_map(|item| match &item.kind {
            ItemKind::Error {
                class, reset_at, ..
            } => Some((class.as_deref(), reset_at.clone())),
            _ => None,
        })
        .next_back()?;
    (failure.0 == Some("usage_limit")).then_some((run, failure.1))
}
/// The highest completed run with a checkpoint.
pub fn latest_stable_run(state: &State) -> Option<&Run> {
    state
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::Completed && run.checkpoint.is_some())
        .max_by_key(|run| run.ordinal)
}
/// The failure message when the selection left the active provider before
/// the rollback executed.
pub fn rollback_provider_changed(checkpoint: &CheckpointId, thread: &ThreadId) -> String {
    format!(
        "Active provider changed before rollback target {checkpoint} could execute on thread {thread}."
    )
}
/// Whether a run's start takes a transfer. Fork and merge-back transfers take
/// the provider of the direct turn that consumes them; handoffs keep their
/// instance; a delegated result reaches only a turn after its spawning run
/// failed or was interrupted.
fn consumable(state: &State, transfer: &Transfer, run: &Run, direct: bool) -> bool {
    let instance = transfer.instance.as_deref();
    match transfer.kind {
        TransferKind::Fork | TransferKind::MergeBack if transfer.delivery.is_none() => {
            direct && instance.is_none_or(|instance| instance == run.selection.instance)
        }
        TransferKind::SubagentResult => {
            instance == Some(run.selection.instance.as_str())
                && transfer.target_run.as_ref().is_some_and(|spawning| {
                    state.runs.iter().any(|candidate| {
                        &candidate.id == spawning
                            && matches!(
                                candidate.status,
                                RunStatus::Failed | RunStatus::Interrupted
                            )
                    })
                })
        }
        _ => instance == Some(run.selection.instance.as_str()),
    }
}
/// The latest run that executed, by completion order.
pub fn latest_executed_run(state: &State) -> Option<&Run> {
    state
        .runs
        .iter()
        .filter(|run| {
            run.status != RunStatus::Queued
                && !(run.status == RunStatus::Cancelled && run.started_at.is_none())
        })
        .reduce(|latest, run| {
            if run_ran_after(run, latest) {
                run
            } else {
                latest
            }
        })
}
/// Class of the latest failure recorded on a failed run.
pub fn failure_class(state: &State, run: &RunId) -> Option<String> {
    state
        .items
        .iter()
        .filter(|item| item.run.as_ref() == Some(run) && item.status == ItemStatus::Failed)
        .filter_map(|item| match &item.kind {
            ItemKind::Error { class, .. } => class.clone(),
            _ => None,
        })
        .next_back()
}
/// A usage-limit failure of the latest executed run keeps the queue waiting.
pub fn usage_limited(state: &State) -> bool {
    latest_executed_run(state).is_some_and(|run| {
        run.status == RunStatus::Failed
            && failure_class(state, &run.id).as_deref() == Some("usage_limit")
    })
}
/// The trimmed title or prompt, clipped past 72 UTF-16 units.
fn subagent_thread_title(
    parent_title: &str,
    title: Option<&str>,
    prompt: &str,
    ordinal: usize,
) -> String {
    let detail = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(prompt.trim());
    let units = detail.encode_utf16().collect::<Vec<_>>();
    if units.is_empty() {
        format!("{parent_title} subagent {ordinal}")
    } else if units.len() > 72 {
        format!("{}...", String::from_utf16_lossy(&units[..69]))
    } else {
        detail.into()
    }
}
fn thread_title_pending(state: &State) -> bool {
    state
        .thread
        .as_ref()
        .is_some_and(|thread| thread.title_request.is_some())
}
fn wake_text(task_ids: &[NodeId]) -> String {
    let list = task_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if task_ids.len() == 1 {
        format!(
            "Delegated task {list} reached a terminal state. Use task_status with taskId {list} to read the result."
        )
    } else {
        format!(
            "Delegated tasks {list} reached terminal states. Use task_status with each taskId to read the results."
        )
    }
}
fn preparation_kind(title: &str, exit_code: Option<i64>) -> ItemKind {
    ItemKind::CommandExecution {
        command: WORKSPACE_PREPARATION_INPUT.into(),
        cwd: None,
        exit_code,
        title: Some(title.into()),
    }
}
/// What the Host fills in before dispatch is not part of a command's identity, so
/// a resent command returns its first result.
fn command_fingerprint(command: &Command) -> String {
    let mut identity = command.clone();
    match &mut identity {
        Command::Rollback {
            restore_refusal, ..
        } => *restore_refusal = None,
        Command::Send(message) => message.resolved_plan = None,
        Command::UpdateMetadata { project_root, .. } => *project_root = None,
        _ => {}
    }
    serde_json::to_string(&identity).expect("domain commands serialize")
}
fn reject(reason: &str) -> Reply {
    Reply::Rejected {
        reason: reason.into(),
    }
}
pub struct ThreadMachine;
impl ThreadMachine {
    pub fn step(state: &State, envelope: &InputEnvelope) -> Step {
        // The common streaming path only inspects its owner and item. It does
        // not copy history or accumulated text into a scratch projection.
        if let Input::Provider {
            attempt: owner,
            event,
        } = &envelope.input
        {
            let event = event.as_ref();
            let mut attempt = owner;
            let mut observed = false;
            let event = if let ProviderEvent::NativeOutput {
                echoed_prompts,
                acknowledged_prompt,
                root: true,
                result: None,
                events,
            } = event
            {
                if echoed_prompts.is_empty()
                    && acknowledged_prompt.is_none()
                    && events.len() == 1
                    && state.pending_prompt.as_ref().is_none_or(|p| {
                        p.confirmed || state.prompt_echo_mode != PromptEchoMode::Early
                    })
                {
                    if let Some(active) = state
                        .active_run()
                        .filter(|r| matches!(r.status, RunStatus::Starting | RunStatus::Running))
                    {
                        let owner_run = state
                            .runs
                            .iter()
                            .find(|r| r.attempt.as_ref() == Some(owner));
                        let owner_current = !state.stopping.contains(owner)
                            && state.attempts.iter().any(|a| {
                                &a.id == owner
                                    && !matches!(
                                        a.status,
                                        AttemptStatus::Superseded
                                            | AttemptStatus::Cancelled
                                            | AttemptStatus::Interrupted
                                            | AttemptStatus::Failed
                                    )
                            });
                        if owner_current
                            && owner_run
                                .is_some_and(|r| r.selection.instance == active.selection.instance)
                        {
                            attempt = active.attempt.as_ref().unwrap_or(owner);
                            observed = state.pending_prompt.as_ref().is_some_and(|p| !p.confirmed);
                            &events[0]
                        } else {
                            event
                        }
                    } else {
                        event
                    }
                } else {
                    event
                }
            } else {
                event
            };
            if let ProviderEvent::TextDelta { key, text, .. }
            | ProviderEvent::PlanDelta { key, text } = event
                && text.len() <= MAX_FACT_TEXT
            {
                let current = state.native_owner.as_ref() == Some(attempt)
                    || state.runs.iter().any(|r| {
                        r.attempt.as_ref() == Some(attempt)
                            && matches!(r.status, RunStatus::Starting | RunStatus::Running)
                    });
                if !current {
                    return Step {
                        facts: vec![],
                        effects: vec![],
                        reply: Reply::Ignored,
                        receipt: None,
                    };
                }
                if let Some(item) = state.items.iter().find(|i| {
                    i.attempt.as_ref() == Some(attempt)
                        && &i.native_key == key
                        && !i.status.terminal()
                }) {
                    let mut bodies = vec![];
                    if observed {
                        bodies.push(FactBody::PromptFrameObserved);
                    }
                    if let ProviderEvent::PlanDelta { .. } = event
                        && let ItemKind::ProposedPlan { plan } = &item.kind
                        && let Some(plan) = state.plans.iter().find(|p| &p.id == plan)
                    {
                        bodies.push(FactBody::PlanMarkdownAppended {
                            id: plan.id.clone(),
                            offset: plan.markdown.len(),
                            text: text.clone(),
                        });
                    }
                    bodies.push(FactBody::ItemTextAppended {
                        id: item.id.clone(),
                        offset: item.text.len(),
                        text: text.clone(),
                    });
                    return Step {
                        facts: bodies
                            .into_iter()
                            .map(|body| Fact {
                                at: envelope.at.clone(),
                                body,
                            })
                            .collect(),
                        effects: vec![],
                        reply: Reply::Accepted,
                        receipt: None,
                    };
                }
            }
        }
        let mut decision = Decision::new(state, envelope);
        let mut receipt = None;
        let reply = match &envelope.input {
            Input::Command {
                id,
                command,
                receipt: existing,
            } => {
                let fingerprint = command_fingerprint(command);
                if let Some(existing) = existing {
                    if existing.command == *id && existing.fingerprint == fingerprint {
                        return Step {
                            facts: vec![],
                            effects: vec![],
                            reply: existing.reply.clone(),
                            receipt: Some(existing.clone()),
                        };
                    }
                    return Step {
                        facts: vec![],
                        effects: vec![],
                        reply: reject("command-id-conflict"),
                        receipt: None,
                    };
                }
                let reply = decision.command(id, command);
                if matches!(reply, Reply::Rejected { .. }) {
                    decision.facts.clear();
                    decision.effects.clear();
                }
                receipt = Some(Receipt {
                    command: id.clone(),
                    fingerprint,
                    reply: reply.clone(),
                });
                reply
            }
            Input::RuntimeOpened { instance, attempt } => {
                let current = decision.state.active_run();
                if decision
                    .state
                    .thread
                    .as_ref()
                    .is_none_or(|thread| thread.selection.instance != *instance)
                    || current.and_then(|run| run.attempt.as_ref()) != attempt.as_ref()
                {
                    Reply::Ignored
                } else {
                    decision.fact(FactBody::PromptEchoModeLearned {
                        mode: PromptEchoMode::Unknown,
                    });
                    Reply::Accepted
                }
            }
            Input::CheckpointScope {
                run,
                attempt,
                scope,
            } => {
                if run.as_ref().is_some_and(|id| {
                    !decision.state.runs.iter().any(|run| {
                        &run.id == id
                            && &run.attempt == attempt
                            && run.status != RunStatus::RolledBack
                            && (run.checkpoint_scope.is_none() || run.checkpoint_scope == *scope)
                    })
                }) {
                    Reply::Ignored
                } else if scope.as_ref().is_some_and(|scope| {
                    scope.cwd.trim().is_empty() || scope.cwd.trim() != scope.cwd
                }) {
                    reject("invalid-checkpoint-scope")
                } else {
                    decision.fact(FactBody::CheckpointScopeBound {
                        run: run.clone(),
                        scope: scope.clone(),
                    });
                    Reply::Accepted
                }
            }
            Input::HandoffPolicy {
                instance,
                model_window,
                token_cap,
            } => {
                decision.fact(FactBody::HandoffPolicyChanged {
                    instance: instance.clone(),
                    model_window: *model_window,
                    token_cap: *token_cap,
                });
                Reply::Accepted
            }
            Input::Workspace { workspace } => {
                if decision.state.thread.is_none() {
                    Reply::Ignored
                } else {
                    decision.fact(FactBody::WorkspaceBound {
                        workspace: workspace.clone(),
                    });
                    Reply::Accepted
                }
            }
            Input::NativeSessionReset { instance } => {
                decision.fact(FactBody::NativeSessionCleared {
                    instance: instance.clone(),
                });
                Reply::Accepted
            }
            Input::RollbackRewindStarted { command, instances } => {
                let rewinding = decision
                    .state
                    .rollback
                    .as_ref()
                    .filter(|pending| &pending.command == command)
                    .map(|pending| &pending.rewinding);
                match rewinding {
                    None => Reply::Ignored,
                    Some(known) if instances.iter().all(|i| known.contains(i)) => Reply::Accepted,
                    Some(_) => {
                        decision.fact(FactBody::RollbackRewindStarted {
                            command: command.clone(),
                            instances: instances.clone(),
                        });
                        Reply::Accepted
                    }
                }
            }
            Input::NativeForkReserved {
                attempt,
                native_thread,
            } => {
                if decision
                    .state
                    .runs
                    .iter()
                    .any(|run| run.attempt.as_ref() == Some(attempt) && !run.status.terminal())
                {
                    decision.fact(FactBody::ForkSessionReserved {
                        attempt: attempt.clone(),
                        native_thread: native_thread.clone(),
                    });
                    Reply::Accepted
                } else {
                    Reply::Ignored
                }
            }
            Input::Provider { attempt, event } => decision.provider(attempt, event),
            Input::Effect(result) => decision.effect_result(result),
            Input::Recover {
                trigger,
                continue_after_restart,
                capturing,
            } => {
                decision.recover(*trigger, *continue_after_restart, capturing);
                Reply::Accepted
            }
            Input::Timer => {
                if let Some(t) = &decision.state.thread
                    && t.snoozed_until
                        .as_ref()
                        .is_some_and(|until| until <= &envelope.at)
                {
                    decision.fact(FactBody::ThreadSnoozed { until: None });
                }
                decision.wake_tasks();
                Reply::Accepted
            }
        };
        Step {
            facts: decision.facts,
            effects: decision.effects,
            reply,
            receipt,
        }
    }
}
/// Schemas and budgets, shared by dispatch and question uploads. The Host
/// names attachments `chat:…`, so the id character set is not checked.
pub fn validate_attachments(files: &[Attachment]) -> Result<(), &'static str> {
    if files.len() > 100 {
        return Err("too-many-attachments");
    }
    let bounded = |value: &str, max: usize| {
        let value = value.trim();
        !value.is_empty() && value.encode_utf16().count() <= max
    };
    let mut image_bytes = 0u64;
    let mut ids = std::collections::BTreeSet::new();
    for file in files {
        if !ids.insert(&file.id) {
            return Err("duplicate-attachment-id");
        }
        if !bounded(&file.id, 128) || !bounded(&file.name, 255) || !bounded(&file.mime_type, 100) {
            return Err("invalid-attachment");
        }
        let mime = file.mime_type.trim().to_ascii_lowercase();
        match file.kind {
            AttachmentKind::Image if !mime.starts_with("image/") => {
                return Err("invalid-attachment");
            }
            AttachmentKind::Image if file.size > 10 * 1024 * 1024 => {
                return Err("image-too-large");
            }
            AttachmentKind::File if file.size == 0 => return Err("invalid-attachment"),
            AttachmentKind::File if file.size > 50 * 1024 * 1024 => {
                return Err("file-too-large");
            }
            _ => {}
        }
        if file.kind == AttachmentKind::Image
            || ["image/gif", "image/jpeg", "image/png", "image/webp"].contains(&mime.as_str())
        {
            image_bytes = image_bytes.saturating_add(file.size);
        }
    }
    if image_bytes > 80 * 1024 * 1024 {
        return Err("total-images-too-large");
    }
    Ok(())
}
