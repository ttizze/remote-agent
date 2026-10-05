use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
        let fact = Fact {
            at: self.at.clone(),
            body,
        };
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
        self.fact(FactBody::ItemTextAppended {
            id: id.clone(),
            offset: 0,
            text,
        });
        self.fact(FactBody::ItemCompleted {
            id,
            status: ItemStatus::Completed,
        });
    }
    fn start_run(&mut self, id: &RunId) {
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
        if let Some(plan) = &run.source_plan {
            self.fact(FactBody::PlanImplemented {
                id: plan.clone(),
                run: id.clone(),
            });
        }
        let thread = self.state.thread.as_ref().unwrap().clone();
        let message = self
            .state
            .messages
            .iter()
            .find(|m| m.id == run.message)
            .unwrap()
            .clone();
        let native_thread = self
            .state
            .native_sessions
            .get(&run.selection.instance)
            .cloned();
        let native_forks = self
            .state
            .transfers
            .iter()
            .filter(|transfer| {
                !transfer.superseded
                    && transfer.instance == run.selection.instance
                    && transfer.native_fork.is_some()
                    && transfer.native_fork == native_thread
                    && transfer.delivery.is_none()
            })
            .map(|transfer| (transfer.id.clone(), transfer.history.clone()))
            .collect::<Vec<_>>();
        for (id, history) in native_forks {
            self.fact(FactBody::TransferDeliveryChanged {
                id,
                delivery: ContextDelivery {
                    attempt: attempt.clone(),
                    run: run.id.clone(),
                    native_thread: native_thread.clone(),
                    status: ContextDeliveryStatus::NativeFork,
                    item_ids: history
                        .messages
                        .iter()
                        .map(|message| message.item.clone())
                        .collect(),
                    omitted_item_ids: history.omitted_item_ids,
                },
            });
        }
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
                    && transfer.instance == run.selection.instance
                    && transfer.delivery.as_ref().is_none_or(|delivery| {
                        delivery.native_thread != native_thread
                            || delivery.status == ContextDeliveryStatus::Pending
                    })
            })
            .collect::<Vec<_>>();
        if transfers.iter().any(|transfer| {
            transfer.delivery.as_ref().is_some_and(|delivery| {
                delivery.native_thread == native_thread
                    && delivery.status == ContextDeliveryStatus::Pending
            })
        }) {
            self.fail_start(id, &attempt, HANDOFF_UNCERTAIN_ERROR);
            return;
        }
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
            let estimate = if native_thread.is_some() {
                self.state
                    .visible_items()
                    .into_iter()
                    .filter(|item| {
                        item.run.as_ref().is_some_and(|id| {
                            self.state.runs.iter().any(|r| {
                                &r.id == id && r.selection.instance == run.selection.instance
                            })
                        })
                    })
                    .map(|item| item.text.len() as u64)
                    .sum()
            } else {
                0
            };
            let budget = handoff_budget(
                self.state
                    .handoff_token_cap
                    .unwrap_or(DEFAULT_HANDOFF_TOKEN_CAP),
                &message.text,
                &message.attachments,
                usage.as_ref(),
                estimate,
                model_window,
            );
            let delivered = self
                .state
                .transfers
                .iter()
                .filter_map(|transfer| transfer.delivery.as_ref())
                .filter(|delivery| {
                    delivery.native_thread == native_thread
                        && delivery.status != ContextDeliveryStatus::Pending
                })
                .flat_map(|delivery| delivery.item_ids.clone())
                .collect();
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
                selection: run.selection.clone(),
                runtime_mode: thread.runtime_mode,
                interaction_mode: thread.interaction_mode,
                text: message.text,
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
    fn fail_start(&mut self, run: &RunId, attempt: &RunAttemptId, message: &str) {
        let key = self.key("handoff-failure", attempt.as_str());
        self.provider(
            attempt,
            &ProviderEvent::ItemFinished {
                key,
                kind: ProviderItem::Error {
                    message: message.into(),
                    retrying: false,
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
        {
            return;
        }
        if let Some(run) = self
            .state
            .queued_runs()
            .first()
            .filter(|r| !r.queue_held)
            .map(|r| r.id.clone())
        {
            self.start_run(&run);
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
    fn close_attempt_items(
        &mut self,
        attempt: &RunAttemptId,
        status: ItemStatus,
        keep_message_questions: bool,
    ) {
        let items=self.state.items.iter().filter(|i| i.attempt.as_ref()==Some(attempt) && !i.status.terminal() && !matches!(&i.kind,ItemKind::Subagent {task} if self.state.tasks.iter().any(|candidate|&candidate.id==task && !candidate.status.terminal())) && !(keep_message_questions && matches!(&i.kind,ItemKind::UserInputRequest { request } if self.state.requests.iter().any(|r| &r.id==request && r.capability==ResponseCapability::Message)))).map(|i| i.id.clone()).collect::<Vec<_>>();
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
                    && !(keep_message_questions && r.capability == ResponseCapability::Message)
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
    fn stop_tasks(&mut self, attempt: &RunAttemptId, status: ItemStatus, confirmed: bool) {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                &task.attempt == attempt
                    && (!task.status.terminal()
                        || task.app_owned()
                            && matches!(
                                task.delivery,
                                DeliveryState::Pending | DeliveryState::Claimed
                            ))
            })
            .cloned()
            .collect::<Vec<_>>();
        let task_ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        for task in tasks {
            if !task.status.terminal() {
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
            self.fact(FactBody::TaskDeliveryChanged {
                id: task.id,
                state: DeliveryState::Disposed,
            });
        }
        let queued=self.state.runs.iter().filter(|run|run.status==RunStatus::Queued && self.state.messages.iter().find(|message|message.id==run.message).and_then(|message|message.notification.as_ref()).is_some_and(|notification| matches!(&notification.source,NotificationSource::Delegated {task_ids:cohort} if cohort.iter().any(|id| task_ids.contains(id))))).map(|run|run.id.clone()).collect::<Vec<_>>();
        for id in queued {
            self.fact(FactBody::RunFinished {
                id,
                status: RunStatus::Cancelled,
            });
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
    fn interrupt_item(&mut self, attempt: &RunAttemptId, run: &RunId, status: Option<ItemStatus>) {
        let key = self.native_key("interrupt-request", attempt, "stop");
        let request = TurnItemId::new(key.clone()).unwrap();
        if !self.state.items.iter().any(|item| item.id == request) {
            self.item_start(
                request.clone(),
                Some(run.clone()),
                Some(attempt.clone()),
                key,
                ItemKind::RunInterruptRequest,
            );
            self.fact(FactBody::ItemCompleted {
                id: request.clone(),
                status: ItemStatus::Completed,
            });
        }
        if let Some(status) = status {
            let key = self.native_key("interrupt-result", attempt, "stop");
            let result = TurnItemId::new(key.clone()).unwrap();
            if !self.state.items.iter().any(|item| item.id == result) {
                self.item_start(
                    result.clone(),
                    Some(run.clone()),
                    Some(attempt.clone()),
                    key,
                    ItemKind::RunInterruptResult { request },
                );
                self.fact(FactBody::ItemCompleted { id: result, status });
            }
        }
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
        if record.message != origin.message {
            return;
        }
        let items = self
            .state
            .items
            .iter()
            .filter(|item| item.run.as_ref() == Some(run))
            .cloned()
            .collect::<Vec<_>>();
        let result = items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::AssistantMessage { .. }))
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let history = prepare_history(&self.state, &items, record.ordinal);
        self.effect(
            record.attempt.clone(),
            EffectBody::SendToThread {
                thread: origin.parent,
                command: Box::new(Command::TaskResult {
                    source_message: Some(origin.message),
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
            self.close_attempt_items(attempt, i, false);
            if self.state.stopping.contains(attempt) {
                self.interrupt_item(attempt, run, Some(i));
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
            self.promote();
        }
    }
    fn create_run(&mut self, message: &SendMessage) -> Reply {
        if self.state.native_owner.is_some() {
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
        if thread.archived_at.is_some() {
            return reject("thread-archived");
        }
        if message.text.encode_utf16().count() > 120_000 {
            return reject("message-too-long");
        }
        if let Err(reason) = validate_attachments(&message.attachments) {
            return reject(reason);
        }
        if message.text.trim().is_empty() && message.attachments.is_empty() {
            return reject("empty-message");
        }
        if message
            .source_plan
            .as_ref()
            .is_some_and(|id| !self.state.plans.iter().any(|p| &p.id == id))
        {
            return reject("plan-not-found");
        }
        let selection = message
            .selection
            .clone()
            .unwrap_or_else(|| thread.selection.clone());
        let active = self.state.active_run();
        let mode = resolve_dispatch(
            active.map(|r| (&r.id, r.status)),
            &message.mode,
            message.intent,
            TurnSupport::for_driver(selection.driver),
        );
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
            if selection.instance != target.selection.instance {
                return reject("steering-provider-mismatch");
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
            });
            self.user_item(&message.id, run);
            if matches!(mode, DispatchMode::RestartActive { .. }) {
                self.stop_tasks(&attempt, ItemStatus::Interrupted, true);
                self.close_attempt_items(&attempt, ItemStatus::Interrupted, false);
                self.fact(FactBody::AttemptFinished {
                    id: attempt.clone(),
                    status: AttemptStatus::Superseded,
                });
                self.interrupt_provider(&attempt);
                let ordinal =
                    self.state.attempts.iter().filter(|a| a.run == *run).count() as u64 + 1;
                let next =
                    RunAttemptId::new(self.key("attempt", &format!("{}:{ordinal}", run))).unwrap();
                self.fact(FactBody::AttemptStarted {
                    id: next.clone(),
                    run: run.clone(),
                    ordinal,
                });
                let t = self.state.thread.as_ref().unwrap();
                let command = ProviderCommand::Start {
                    selection: target.selection.clone(),
                    runtime_mode: t.runtime_mode,
                    interaction_mode: t.interaction_mode,
                    text: message.text.clone(),
                    attachments: message.attachments.clone(),
                    native_thread: self
                        .state
                        .native_sessions
                        .get(&target.selection.instance)
                        .cloned(),
                    resume_at: None,
                    context: None,
                };
                self.effect(Some(next), EffectBody::Provider(command));
            } else {
                self.effect(
                    Some(attempt),
                    EffectBody::Provider(ProviderCommand::Steer {
                        text: message.text.clone(),
                        attachments: message.attachments.clone(),
                    }),
                );
            }
            return Reply::Run(run.clone());
        }
        let held = self.state.queued_runs().iter().any(|r| r.queue_held);
        let queued = active.is_some()
            || !self.state.captures.is_empty()
            || held
            || !self.state.queued_runs().is_empty();
        let deferred = matches!(mode, DispatchMode::DeferStart);
        if deferred && active.is_some() {
            return reject("run-already-active");
        }
        let id = RunId::new(format!("run:{}:{}", message.id.as_str().len(), message.id)).unwrap();
        let ordinal = self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0) + 1;
        let intent = if queued {
            InputIntent::QueuedTurn
        } else {
            InputIntent::TurnStart
        };
        self.fact(FactBody::MessageCreated {
            id: message.id.clone(),
            run: Some(id.clone()),
            role: Role::User,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            intent,
            created_by: message.created_by,
            creation_source: message.creation_source.clone(),
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
        if deferred {
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
            Create { .. } | AcceptFork { .. } | AcceptDelegation { .. }
        ) {
            let Some(thread) = &self.state.thread else {
                return reject("thread-not-found");
            };
            if thread.deleted_at.is_some() {
                return reject("thread-deleted");
            }
        }
        // Metadata can change during rollback; operation results carry only the
        // rollback identity and never overwrite thread metadata.
        if self.state.rollback.is_some()
            && matches!(
                command,
                Send(_)
                    | ReleasePrepared { .. }
                    | RetryPrepared { .. }
                    | ResumeQueue
                    | Rollback { .. }
                    | Fork { .. }
                    | MergeBack { .. }
                    | Delegate { .. }
                    | SelectModel { .. }
                    | SwitchProvider { .. }
                    | RuntimeMode { .. }
                    | InteractionMode { .. }
                    | Compact
                    | PromoteToSteer { .. }
            )
        {
            return reject("rollback-pending");
        }
        match command {
            Stop => {
                if let Some(run) = self.state.active_run().map(|r| r.id.clone()) {
                    return self.command(
                        id,
                        &Interrupt {
                            run,
                            hold_queue: true,
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
            } => {
                self.fact(FactBody::NativeChildBound {
                    native_thread: native_thread.clone(),
                    owner: owner.clone(),
                    parent: parent.clone(),
                    task: task.clone(),
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
                });
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
            Archive { archived } => {
                self.fact(FactBody::ThreadArchived {
                    archived: *archived,
                });
                if !archived {
                    self.promote();
                    self.wake_tasks();
                }
                Reply::Accepted
            }
            Delete => {
                let runs = self
                    .state
                    .runs
                    .iter()
                    .filter(|r| r.status.blocking() || r.status == RunStatus::Queued)
                    .cloned()
                    .collect::<Vec<_>>();
                self.hold_queue();
                for run in runs {
                    if let Some(a) = &run.attempt {
                        self.interrupt_provider(a);
                        self.stop_tasks(a, ItemStatus::Cancelled, true);
                    }
                    self.finish(&run.id, RunStatus::Cancelled, false);
                }
                if let Some(owner) = self.state.native_owner.clone() {
                    self.interrupt_provider(&owner);
                    self.provider(
                        &owner,
                        &ProviderEvent::TurnFinished {
                            status: RunStatus::Cancelled,
                            native_head: None,
                        },
                    );
                }
                self.fact(FactBody::ThreadDeleted);
                let paths = self
                    .state
                    .messages
                    .iter()
                    .flat_map(|m| &m.attachments)
                    .map(|a| a.path.clone())
                    .collect();
                self.effect(None, EffectBody::DeleteAttachments { paths });
                Reply::Accepted
            }
            Settle { settled, at } => {
                self.fact(FactBody::ThreadSettled {
                    settled: *settled,
                    at: at.clone().unwrap_or_else(|| self.at.clone()),
                });
                Reply::Accepted
            }
            Snooze { until } => {
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
                self.fact(FactBody::ThreadSnoozed {
                    until: until.clone(),
                });
                Reply::Accepted
            }
            Pin { pinned, order } => {
                self.fact(FactBody::ThreadPinned {
                    pinned: *pinned,
                    order: order.clone(),
                });
                Reply::Accepted
            }
            ReorderActive { order } => {
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
                    .filter(|r| r.status.terminal() && r.status != RunStatus::RolledBack)
                    .filter_map(|r| r.completed_at.as_ref())
                    .max()
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
                self.fact(FactBody::AutoSettleChanged { enabled: *enabled });
                Reply::Accepted
            }
            RuntimeMode { mode } => {
                self.fact(FactBody::RuntimeModeChanged { mode: *mode });
                if let Some(a) = self.state.active_run().and_then(|r| r.attempt.clone()) {
                    self.effect(
                        Some(a),
                        EffectBody::Provider(ProviderCommand::SetRuntimeMode {
                            runtime_mode: *mode,
                            interaction_mode: self.state.thread.as_ref().unwrap().interaction_mode,
                        }),
                    );
                }
                Reply::Accepted
            }
            InteractionMode { mode } => {
                self.fact(FactBody::InteractionModeChanged { mode: *mode });
                Reply::Accepted
            }
            SelectModel { selection } | SwitchProvider { selection } => {
                let t = self.state.thread.as_ref().unwrap().clone();
                if matches!(command, SwitchProvider { .. }) && self.state.active_run().is_some() {
                    return reject("provider-switch-while-active");
                }
                if t.selection.instance != selection.instance
                    && !self.state.visible_items().is_empty()
                {
                    self.fact(FactBody::TransferOpened {
                        native_fork: None,
                        id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                        kind: TransferKind::ProviderHandoff,
                        source: t.id.clone(),
                        target: t.id,
                        boundary: self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0),
                        instance: selection.instance.clone(),
                        history: prepare_history(
                            &self.state,
                            &self
                                .state
                                .visible_items()
                                .into_iter()
                                .cloned()
                                .collect::<Vec<_>>(),
                            self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0),
                        ),
                    });
                }
                self.fact(FactBody::ModelSelected {
                    selection: selection.clone(),
                });
                if let Some(a) = self.state.active_run().and_then(|r| r.attempt.clone()) {
                    self.effect(
                        Some(a),
                        EffectBody::Provider(ProviderCommand::SetModel {
                            selection: selection.clone(),
                        }),
                    );
                }
                Reply::Accepted
            }
            Send(message) => self.create_run(message),
            Compact => {
                let message = SendMessage {
                    created_by: MessageAuthor::User,
                    creation_source: "client".into(),
                    id: MessageId::new(self.key("message", id.as_str())).unwrap(),
                    text: "/compact".into(),
                    attachments: vec![],
                    selection: None,
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                };
                self.create_run(&message)
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
                self.start_run(run);
                Reply::Run(run.clone())
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
                self.error_item(run, message);
                self.finish(run, RunStatus::Failed, false);
                Reply::Accepted
            }
            RetryPrepared { run } => {
                if self.state.active_run().is_some()
                    || !self
                        .state
                        .runs
                        .iter()
                        .any(|r| &r.id == run && r.status == RunStatus::Failed)
                {
                    return reject("run-not-retryable");
                }
                self.fact(FactBody::RunPrepared { id: run.clone() });
                self.effect(None, EffectBody::PrepareWorkspace { run: run.clone() });
                Reply::Accepted
            }
            Interrupt { run, hold_queue } => {
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
                if *hold_queue {
                    self.hold_queue();
                }
                if let Some(attempt) = &target.attempt {
                    if self.state.stopping.contains(attempt) {
                        return Reply::Ignored;
                    }
                    self.interrupt_item(attempt, run, None);
                    self.fact(FactBody::StopRequested {
                        attempt: attempt.clone(),
                    });
                    self.interrupt_provider(attempt);
                    self.stop_tasks(attempt, ItemStatus::Interrupted, false);
                    if background && !target.status.blocking() {
                        return Reply::Accepted;
                    }
                    let started = self
                        .state
                        .attempts
                        .iter()
                        .any(|a| &a.id == attempt && a.status == AttemptStatus::Running);
                    if started {
                        return Reply::Accepted;
                    }
                }
                self.finish(run, RunStatus::Interrupted, true);
                Reply::Accepted
            }
            ResumeQueue => {
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
                let mut order = self
                    .state
                    .queued_runs()
                    .iter()
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>();
                if !order.contains(run) || before.as_ref().is_some_and(|id| !order.contains(id)) {
                    return reject("queued-run-not-found");
                }
                if before.as_ref() == Some(run) {
                    return Reply::Accepted;
                }
                order.retain(|id| id != run);
                let position = before
                    .as_ref()
                    .and_then(|id| order.iter().position(|r| r == id))
                    .unwrap_or(order.len());
                order.insert(position, run.clone());
                self.fact(FactBody::QueueReordered { order });
                Reply::Accepted
            }
            CancelQueued { run } => {
                let Some(r) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == run && r.status == RunStatus::Queued)
                    .cloned()
                else {
                    return reject("queued-run-not-found");
                };
                let ids = self
                    .state
                    .messages
                    .iter()
                    .find(|message| message.id == r.message)
                    .and_then(|message| message.notification.as_ref())
                    .and_then(|notification| match &notification.source {
                        NotificationSource::Delegated { task_ids } => Some(task_ids.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                for id in ids {
                    self.fact(FactBody::TaskDeliveryChanged {
                        id,
                        state: DeliveryState::Disposed,
                    });
                }
                self.fact(FactBody::RunFinished {
                    id: run.clone(),
                    status: RunStatus::Cancelled,
                });
                self.promote();
                Reply::Accepted
            }
            EditQueued {
                run,
                text,
                attachments,
            } => {
                let Some(r) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == run && r.status == RunStatus::Queued)
                else {
                    return reject("queued-run-not-found");
                };
                self.fact(FactBody::MessageEdited {
                    id: r.message.clone(),
                    text: text.clone(),
                    attachments: attachments.clone(),
                });
                Reply::Accepted
            }
            PromoteToSteer { queued, active } => {
                let Some(target) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == active && r.status == RunStatus::Running)
                    .cloned()
                else {
                    return reject("run-not-active");
                };
                let Some(run) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| &r.id == queued && r.status == RunStatus::Queued)
                    .cloned()
                else {
                    return reject("queued-run-not-found");
                };
                let m = self
                    .state
                    .messages
                    .iter()
                    .find(|m| m.id == run.message)
                    .unwrap()
                    .clone();
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
                self.effect(
                    target.attempt,
                    EffectBody::Provider(ProviderCommand::Steer {
                        text: m.text,
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
                match &r.body {
                    RequestBody::Approval { .. } if decision.is_none() => {
                        return reject("invalid-approval-decision");
                    }
                    RequestBody::Questions { questions }
                        if answers.is_none()
                            || questions.iter().any(|q| {
                                q.required
                                    && answers.as_ref().is_none_or(|a| !a.contains_key(&q.id))
                            }) =>
                    {
                        return reject("missing-question-answer");
                    }
                    _ => {}
                }
                if r.capability == ResponseCapability::NotResumable {
                    return reject("request-not-resumable");
                }
                let provider_answers = match answers {
                    Some(answers) => match append_answer_attachments(answers, attachments) {
                        Ok(answers) => Some(answers),
                        Err(reason) => return reject(reason),
                    },
                    None => None,
                };
                let async_text = if r.capability == ResponseCapability::Message {
                    let RequestBody::Questions { questions } = &r.body else {
                        return reject("question-not-found");
                    };
                    let mut replies = vec![];
                    for question in questions {
                        let answer = answers.as_ref().and_then(|a| a.get(&question.id));
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
                let items=self.state.items.iter().filter(|i| matches!(&i.kind,ItemKind::ApprovalRequest { request:r }|ItemKind::UserInputRequest { request:r } if r==request)).map(|i| i.id.clone()).collect::<Vec<_>>();
                for id in items {
                    self.fact(FactBody::ItemCompleted {
                        id,
                        status: ItemStatus::Completed,
                    });
                }
                if r.capability == ResponseCapability::Message {
                    let message = SendMessage {
                        created_by: MessageAuthor::User,
                        creation_source: "server".into(),
                        id: MessageId::new(format!("async-answer:{request}")).unwrap(),
                        text: async_text.unwrap(),
                        attachments: vec![],
                        selection: None,
                        mode: DispatchMode::QueueAfterActive,
                        intent: Some(DeliveryIntent::Auto),
                        source_plan: None,
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
                if !self.state.requests.iter().any(|r| {
                    &r.id == request
                        && r.status == RequestStatus::Pending
                        && r.capability == ResponseCapability::Message
                }) {
                    return reject("question-not-dismissible");
                }
                self.fact(FactBody::RequestResolved {
                    id: request.clone(),
                    status: RequestStatus::Cancelled,
                    decision: None,
                    answers: None,
                    attachments: BTreeMap::new(),
                });
                Reply::Accepted
            }
            Rollback {
                checkpoint,
                restore_files,
            } => {
                if self.state.active_run().is_some()
                    || self.state.tasks.iter().any(|t| !t.status.terminal())
                {
                    return reject("provider-work-active");
                }
                let Some(cp) = self
                    .state
                    .checkpoints
                    .iter()
                    .find(|c| &c.id == checkpoint)
                    .cloned()
                else {
                    return reject("checkpoint-not-found");
                };
                self.fact(FactBody::RollbackRequested {
                    command: id.clone(),
                    checkpoint: checkpoint.clone(),
                    restore_files: *restore_files,
                });
                for (instance, native_thread) in self.state.native_sessions.clone() {
                    self.effect(
                        None,
                        EffectBody::Provider(ProviderCommand::Rollback {
                            native_thread,
                            absolute_head: cp.native_heads.get(&instance).cloned().flatten(),
                        }),
                    );
                }
                if *restore_files {
                    self.effect(
                        None,
                        EffectBody::RestoreCheckpoint {
                            checkpoint: checkpoint.clone(),
                            file_ref: cp.file_ref,
                            scope: cp.scope,
                        },
                    );
                }
                Reply::Accepted
            }
            Fork {
                target,
                through_run,
                title,
            } => {
                if self.state.active_run().is_some()
                    || self.state.tasks.iter().any(|t| !t.status.terminal())
                {
                    return reject("provider-work-active");
                }
                let Some(run) = self
                    .state
                    .runs
                    .iter()
                    .find(|r| {
                        &r.id == through_run
                            && matches!(
                                r.status,
                                RunStatus::Completed | RunStatus::Interrupted | RunStatus::Failed
                            )
                    })
                    .cloned()
                else {
                    return reject("fork-source-not-ready");
                };
                let thread = self.state.thread.as_ref().unwrap().clone();
                let history = self
                    .state
                    .activity_items()
                    .into_iter()
                    .filter(|i| {
                        i.run.as_ref().is_none_or(|id| {
                            self.state
                                .runs
                                .iter()
                                .any(|r| &r.id == id && r.ordinal <= run.ordinal)
                        })
                    })
                    .map(|item| item.into_owned())
                    .collect::<Vec<_>>();
                let context = prepare_history(&self.state, &history, run.ordinal);
                let child_command = Box::new(AcceptFork {
                    thread: target.clone(),
                    parent: thread.id,
                    project: thread.project,
                    title: title
                        .clone()
                        .unwrap_or_else(|| format!("{} fork", thread.title)),
                    selection: thread.selection,
                    runtime_mode: thread.runtime_mode,
                    interaction_mode: thread.interaction_mode,
                    boundary: run.ordinal,
                    history,
                    checkpoint_scope: self.state.checkpoint_scope.clone(),
                    context,
                    native: None,
                });
                let native = run
                    .attempt
                    .as_ref()
                    .and_then(|id| self.state.attempts.iter().find(|a| &a.id == id))
                    .and_then(|a| {
                        a.native_thread
                            .clone()
                            .map(|thread| (thread, a.native_head.clone()))
                    });
                if let Some((native_thread, head)) = native {
                    self.fact(FactBody::ForkPrepared {
                        command: id.clone(),
                        target: target.clone(),
                        child_command,
                        instance: run.selection.instance,
                        head: head.clone(),
                    });
                    self.effect(
                        run.attempt,
                        EffectBody::ForkNative {
                            command: id.clone(),
                            provider: ProviderCommand::Fork {
                                native_thread,
                                through_turn: head,
                            },
                        },
                    );
                } else {
                    self.effect(
                        None,
                        EffectBody::SendToThread {
                            thread: target.clone(),
                            command: child_command,
                        },
                    );
                }
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
                checkpoint_scope,
                context,
                native,
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
                });
                self.fact(FactBody::CheckpointScopeBound {
                    run: None,
                    scope: checkpoint_scope.clone(),
                });
                self.fact(FactBody::ForkAccepted {
                    parent: parent.clone(),
                    boundary: *boundary,
                    history: history.clone(),
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
                self.fact(FactBody::TransferOpened {
                    native_fork: native.as_ref().map(|binding| binding.thread.clone()),
                    id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                    kind: TransferKind::Fork,
                    source: parent.clone(),
                    target: thread.clone(),
                    boundary: *boundary,
                    instance: selection.instance.clone(),
                    history: context.clone(),
                });
                if let Some(native) = native {
                    self.fact(FactBody::NativeSessionBound {
                        instance: native.instance.clone(),
                        native_thread: native.thread.clone(),
                        head: native.head.clone(),
                    });
                }
                Reply::Thread(thread.clone())
            }
            MergeBack { target } => {
                let thread = self.state.thread.as_ref().unwrap();
                let boundary = self
                    .state
                    .runs
                    .iter()
                    .filter(|r| r.status != RunStatus::RolledBack)
                    .map(|r| r.ordinal)
                    .max()
                    .unwrap_or(0);
                let history = prepare_history(
                    &self.state,
                    &self
                        .state
                        .items
                        .iter()
                        .filter(|i| {
                            i.run.as_ref().is_none_or(|id| {
                                self.state
                                    .runs
                                    .iter()
                                    .any(|r| &r.id == id && r.status != RunStatus::RolledBack)
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
                self.fact(FactBody::TransferOpened {
                    native_fork: None,
                    id: id.clone(),
                    kind: *kind,
                    source: source.clone(),
                    target: self.state.thread.as_ref().unwrap().id.clone(),
                    boundary: *boundary,
                    instance: self
                        .state
                        .thread
                        .as_ref()
                        .unwrap()
                        .selection
                        .instance
                        .clone(),
                    history: history.clone(),
                });
                Reply::Accepted
            }
            Delegate {
                task,
                child,
                prompt,
                selection,
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
                            title: prompt.lines().next().unwrap_or("Task").into(),
                            selection: selection.clone(),
                            runtime_mode: thread.runtime_mode,
                            interaction_mode: thread.interaction_mode,
                            origin: Delegation {
                                parent: thread.id.clone(),
                                task: task.clone(),
                                message: message.clone(),
                            },
                            message: SendMessage {
                                created_by: MessageAuthor::Agent,
                                creation_source: "server".into(),
                                id: message,
                                text: prompt.clone(),
                                attachments: vec![],
                                selection: None,
                                mode: DispatchMode::StartImmediately,
                                intent: None,
                                source_plan: None,
                            },
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
                origin,
                message,
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
                });
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
                {
                    return Reply::Ignored;
                }
                if let Some(context) = context {
                    self.fact(FactBody::TransferOpened {
                        native_fork: None,
                        id: ContextTransferId::new(self.key("task-result", task.as_str())).unwrap(),
                        kind: TransferKind::SubagentResult,
                        source: existing.child_thread,
                        target: self.state.thread.as_ref().unwrap().id.clone(),
                        instance: self
                            .state
                            .thread
                            .as_ref()
                            .unwrap()
                            .selection
                            .instance
                            .clone(),
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
                let tasks = self
                    .state
                    .tasks
                    .iter()
                    .filter(|t| {
                        task_ids.contains(&t.id)
                            && t.status.terminal()
                            && t.delivery == DeliveryState::Pending
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if tasks.is_empty()
                    || tasks.len() != task_ids.len()
                    || tasks
                        .iter()
                        .any(|task| !task.app_owned() || task.run != tasks[0].run)
                {
                    return reject("invalid-completion-cohort");
                }
                let notification = delegated_notification(
                    task_ids,
                    tasks[0].run.as_ref().unwrap(),
                    &self.state.tasks,
                );
                let list = task_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let text = if task_ids.len() == 1 {
                    format!(
                        "Delegated task {list} reached a terminal state. Use task_status with taskId {list} to read the result."
                    )
                } else {
                    format!(
                        "Delegated tasks {list} reached terminal states. Use task_status with each taskId to read the results."
                    )
                };
                let message = SendMessage {
                    created_by: MessageAuthor::Agent,
                    creation_source: "server".into(),
                    id: MessageId::new(self.key("completion-message", id.as_str())).unwrap(),
                    text,
                    attachments: vec![],
                    selection: None,
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                };
                let reply = self.create_run(&message);
                if !matches!(reply, Reply::Rejected { .. }) {
                    self.fact(FactBody::MessageNotificationAssigned {
                        id: message.id.clone(),
                        notification,
                    });
                    for t in tasks {
                        self.fact(FactBody::TaskDeliveryChanged {
                            id: t.id,
                            state: DeliveryState::Claimed,
                        });
                    }
                }
                reply
            }
        }
    }
    fn wake_tasks(&mut self) {
        let Some(thread) = &self.state.thread else {
            return;
        };
        if thread.archived_at.is_some() || thread.deleted_at.is_some() {
            return;
        }
        let thread = thread.id.clone();
        let mut cohorts = BTreeMap::<RunId, Vec<NodeId>>::new();
        for task in &self.state.tasks {
            if task.app_owned()
                && task.status.terminal()
                && task.delivery == DeliveryState::Pending
                && (task.wake == CompletionWake::Always || self.state.active_run().is_none())
                && let Some(run) = &task.run
            {
                cohorts
                    .entry(run.clone())
                    .or_default()
                    .push(task.id.clone());
            }
        }
        for task_ids in cohorts.into_values() {
            self.effect(
                None,
                EffectBody::SendToThread {
                    thread: thread.clone(),
                    command: Box::new(Command::AcceptTaskWake { task_ids }),
                },
            );
        }
    }

    fn error_item(&mut self, run: &RunId, message: &str) {
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
                message: message.into(),
                retrying: false,
                code: None,
                class: None,
                retryable: None,
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
            ProviderItem::Error {
                message,
                retrying,
                code,
                class,
                retryable,
            } => ItemKind::Error {
                message: message.clone(),
                retrying: *retrying,
                code: code.clone(),
                class: class.clone(),
                retryable: *retryable,
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
        let background = matches!(event, ProviderEvent::RequestOpened { owner_path, .. } if !owner_path.is_empty())
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
                    | ProviderEvent::Wake { .. }
                    | ProviderEvent::SessionClosed { .. }
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
            PromptOffered { key } => self.fact(FactBody::PromptOffered {
                attempt: attempt.clone(),
                key: key.clone(),
            }),
            NativeOutput { .. } => {
                unreachable!("native output is routed before applying its events")
            }
            SessionClosed { error } => {
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
                                    retrying: false,
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
                    self.wake_tasks();
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
                        false,
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
                            })
                            .map(|item| item.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: parent,
                                command: Box::new(Command::TaskResult {
                                    source_message: None,
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
                    self.fact(FactBody::ItemTextAppended {
                        id,
                        offset: item.text.len(),
                        text: text.clone(),
                    });
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
                    let current = &self.state.items.iter().find(|i| i.id == id).unwrap().text;
                    if text != current {
                        if let Some(tail) = text.strip_prefix(current) {
                            self.fact(FactBody::ItemTextAppended {
                                id: id.clone(),
                                offset: current.len(),
                                text: tail.to_owned(),
                            });
                        } else {
                            self.fact(FactBody::ItemTextReplaced {
                                id: id.clone(),
                                text: text.clone(),
                            });
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
                    self.fact(FactBody::RequestResolved {
                        id,
                        status: RequestStatus::Resolved,
                        decision: None,
                        answers: None,
                        attachments: BTreeMap::new(),
                    });
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
                        });
                        let item = TurnItemId::new(self.native_key("item", attempt, key)).unwrap();
                        self.item_start(
                            item.clone(),
                            None,
                            Some(attempt.clone()),
                            key.clone(),
                            ItemKind::UserMessage { message },
                        );
                        self.fact(FactBody::ItemTextAppended {
                            id: item.clone(),
                            offset: 0,
                            text: text.clone(),
                        });
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
                let offset = self
                    .state
                    .plans
                    .iter()
                    .find(|p| p.id == id)
                    .unwrap()
                    .markdown
                    .len();
                self.fact(FactBody::PlanMarkdownAppended {
                    id,
                    offset,
                    text: text.clone(),
                });
                let offset = self
                    .state
                    .items
                    .iter()
                    .find(|i| i.id == item)
                    .unwrap()
                    .text
                    .len();
                self.fact(FactBody::ItemTextAppended {
                    id: item,
                    offset,
                    text: text.clone(),
                });
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
                    self.fact(FactBody::PlanMarkdownReplaced {
                        id: id.clone(),
                        text: markdown.clone(),
                    });
                    self.fact(FactBody::ItemTextReplaced {
                        id: item,
                        text: markdown.clone(),
                    });
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
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: task.child_thread.clone(),
                            command: Box::new(Command::BindNativeChild {
                                native_thread: native_thread.clone(),
                                owner: attempt.clone(),
                                parent: self.state.thread.as_ref().unwrap().id.clone(),
                                task: task.id,
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
                                attempt: attempt.clone(),
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
            } => {
                if let Some(status) = status {
                    if let Some(work) = self.state.background_work.get(key).cloned() {
                        self.fact(FactBody::NativeWorkReported {
                            key: key.clone(),
                            report: WorkReport {
                                kind: work.kind,
                                label: Some(work.description),
                                outcome: (*status).into(),
                                child_thread: None,
                                exit_code: None,
                            },
                            text: summary.clone().unwrap_or_default(),
                        });
                    }
                    self.fact(FactBody::BackgroundTaskFinished { key: key.clone() });
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
            Wake { text } => {
                let message = SendMessage {
                    created_by: MessageAuthor::Agent,
                    creation_source: "provider".into(),
                    id: MessageId::new(self.key("wake", text)).unwrap(),
                    text: text.clone(),
                    attachments: vec![],
                    selection: run.as_ref().map(|r| r.selection.clone()),
                    mode: DispatchMode::QueueAfterActive,
                    intent: None,
                    source_plan: None,
                };
                self.create_run(&message);
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
            for event in events {
                if matches!(
                    event,
                    ProviderEvent::Child { .. }
                        | ProviderEvent::SubagentFinished { .. }
                        | ProviderEvent::BackgroundTask {
                            status: Some(_),
                            ..
                        }
                ) {
                    self.provider(owner, event);
                }
            }
            return Reply::Accepted;
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
                command,
                native_thread,
            } => {
                let Some(pending) = self.state.pending_forks.get(command).cloned() else {
                    return Reply::Ignored;
                };
                let mut child_command = pending.child_command;
                if let Command::AcceptFork { native, .. } = child_command.as_mut() {
                    *native = Some(NativeBinding {
                        instance: pending.instance,
                        thread: native_thread.clone(),
                        head: pending.head,
                    });
                }
                self.fact(FactBody::ForkResolved {
                    command: command.clone(),
                });
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: pending.target,
                        command: child_command,
                    },
                );
                return Reply::Accepted;
            }
            EffectResult::ForkFailed { command, message } => {
                if !self.state.pending_forks.contains_key(command) {
                    return Reply::Ignored;
                }
                self.fact(FactBody::ForkResolved {
                    command: command.clone(),
                });
                return reject(message);
            }
            EffectResult::ProviderFailed {
                attempt,
                operation,
                message,
                message_id,
                turn_completed,
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
                        // Reuse the accepted message instead of creating a second one.
                        let next =
                            RunId::new(format!("followup:{}:{}", id.as_str().len(), id)).unwrap();
                        if self.state.runs.iter().any(|r| r.id == next) {
                            return Reply::Ignored;
                        }
                        let ordinal =
                            self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0) + 1;
                        self.fact(FactBody::MessageAdopted {
                            id: m.id.clone(),
                            run: next.clone(),
                            intent: InputIntent::QueuedTurn,
                        });
                        self.fact(FactBody::RunRequested {
                            id: next.clone(),
                            message: m.id,
                            ordinal,
                            selection: run.selection,
                            status: RunStatus::Queued,
                            queue_position: Some(self.state.queued_runs().len() as u64 + 1),
                            held: false,
                            source_plan: None,
                        });
                        self.promote();
                        return Reply::Run(next);
                    }
                    return Reply::Ignored;
                }
                if !run.status.blocking() {
                    return Reply::Ignored;
                }
                match operation {
                    ProviderOperation::Start | ProviderOperation::Compact => {
                        self.error_item(&run.id, message);
                        self.stop_tasks(attempt, ItemStatus::Failed, true);
                        self.finish(&run.id, RunStatus::Failed, false);
                    }
                    // Failed control requests keep the native turn authoritative.
                    _ => {
                        self.error_item(&run.id, message);
                    }
                }
            }
            EffectResult::CheckpointCaptured {
                baselines,
                run,
                attempt,
                checkpoint,
                file_ref,
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
                        checkpoint.id == baseline.checkpoint
                            || checkpoint.scope == r.checkpoint_scope
                                && checkpoint.run_ordinal == baseline.ordinal
                    }) {
                        self.fact(FactBody::CheckpointCaptured {
                            id: baseline.checkpoint.clone(),
                            scope: r.checkpoint_scope.clone(),
                            run: None,
                            run_ordinal: baseline.ordinal,
                            native_heads: baseline.native_heads.clone(),
                            file_ref: baseline.file_ref.clone(),
                        });
                    }
                }
                self.fact(FactBody::CheckpointCaptured {
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
                });
                if let Some(status) = self.state.captures.get(run).copied() {
                    self.fact(FactBody::RunFinished {
                        id: run.clone(),
                        status,
                    });
                    self.complete_delegation(run, status);
                    self.promote();
                }
            }
            EffectResult::CheckpointFailed { run, attempt, .. } => {
                if !self
                    .state
                    .runs
                    .iter()
                    .any(|r| &r.id == run && &r.attempt == attempt)
                {
                    return Reply::Ignored;
                }
                // The outbox retries a failed capture. Keep its durable target
                // and block promotion until a successful result arrives.
                if !self.state.captures.contains_key(run) {
                    return Reply::Ignored;
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
            }
            EffectResult::RollbackFailed { command, message } => {
                if self
                    .state
                    .rollback
                    .as_ref()
                    .is_none_or(|p| &p.command != command)
                {
                    return Reply::Ignored;
                }
                self.fact(FactBody::RollbackFailed {
                    command: command.clone(),
                    message: message.clone(),
                });
            }
            EffectResult::TitleGenerated { title } => self.fact(FactBody::ThreadRenamed {
                title: title.clone(),
            }),
        }
        Reply::Accepted
    }
    fn recover(&mut self, trigger: RecoveryTrigger) {
        self.hold_queue();
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
                    && !(r.status == RunStatus::Waiting && self.state.captures.contains_key(&r.id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for run in runs {
            if let Some(attempt) = &run.attempt {
                self.fact(FactBody::AttemptFinished {
                    id: attempt.clone(),
                    status: AttemptStatus::Cancelled,
                });
                self.close_attempt_items(attempt, ItemStatus::Cancelled, true);
            }
            self.fact(FactBody::RunFinished {
                id: run.id,
                status: RunStatus::Cancelled,
            });
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
                result:
                    "Cancelled because the server restarted before the provider work completed."
                        .into(),
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
    }
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
                        if owner_run
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
                let fingerprint =
                    serde_json::to_string(command).expect("domain commands serialize");
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
            Input::NativeSessionReset { instance } => {
                decision.fact(FactBody::NativeSessionCleared {
                    instance: instance.clone(),
                });
                Reply::Accepted
            }
            Input::Provider { attempt, event } => decision.provider(attempt, event),
            Input::Effect(result) => decision.effect_result(result),
            Input::Recover { trigger } => {
                decision.recover(*trigger);
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
/// Fixed T3 chatAttachment.ts budgets, shared by dispatch and question uploads.
pub fn validate_attachments(files: &[Attachment]) -> Result<(), &'static str> {
    if files.len() > 100 {
        return Err("too-many-attachments");
    }
    let mut image_bytes = 0u64;
    let mut ids = std::collections::BTreeSet::new();
    for file in files {
        if !ids.insert(&file.id) {
            return Err("duplicate-attachment-id");
        }
        if file.kind == AttachmentKind::Image {
            if file.size > 10 * 1024 * 1024 {
                return Err("image-too-large");
            }
            image_bytes = image_bytes.saturating_add(file.size);
        } else if file.size > 50 * 1024 * 1024 {
            return Err("file-too-large");
        }
    }
    if image_bytes > 80 * 1024 * 1024 {
        return Err("total-images-too-large");
    }
    Ok(())
}
