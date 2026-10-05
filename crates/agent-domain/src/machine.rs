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
        let run = self
            .state
            .runs
            .iter()
            .find(|r| &r.id == id)
            .unwrap()
            .clone();
        self.fact(FactBody::RunStarted { id: id.clone() });
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
            .attempts
            .iter()
            .rev()
            .filter(|a| a.id != attempt)
            .find_map(|a| {
                self.state
                    .runs
                    .iter()
                    .find(|r| r.id == a.run && r.selection.instance == run.selection.instance)
                    .and(a.native_thread.clone())
            });
        let context = self
            .state
            .transfers
            .iter()
            .filter(|t| !t.superseded && t.consumed_by.is_none() && t.target == thread.id)
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        // /compact remains a native command, even when context is waiting.
        if message.text.trim() == "/compact" {
            self.effect(
                Some(attempt),
                EffectBody::Provider(ProviderCommand::Compact { native_thread }),
            );
        } else {
            let transfers = self
                .state
                .transfers
                .iter()
                .filter(|t| !t.superseded && t.consumed_by.is_none() && t.target == thread.id)
                .map(|t| t.id.clone())
                .collect::<Vec<_>>();
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
            for id in transfers {
                self.fact(FactBody::TransferConsumed {
                    id,
                    run: run.id.clone(),
                });
            }
        }
    }
    fn promote(&mut self) {
        if self.state.active_run().is_some()
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
        let items=self.state.items.iter().filter(|i| i.attempt.as_ref()==Some(attempt) && !i.status.terminal() && !(keep_message_questions && matches!(&i.kind,ItemKind::UserInputRequest { request } if self.state.requests.iter().any(|r| &r.id==request && r.capability==ResponseCapability::Message)))).map(|i| i.id.clone()).collect::<Vec<_>>();
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
    fn stop_tasks(&mut self, attempt: &RunAttemptId, status: ItemStatus) {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|t| &t.attempt == attempt && !t.status.terminal())
            .cloned()
            .collect::<Vec<_>>();
        for task in tasks {
            self.fact(FactBody::TaskFinished {
                id: task.id.clone(),
                status,
                result: String::new(),
            });
            self.fact(FactBody::TaskDeliveryChanged {
                id: task.id,
                state: DeliveryState::Disposed,
            });
            self.effect(
                Some(attempt.clone()),
                EffectBody::SendToThread {
                    thread: task.child_thread,
                    command: Box::new(Command::Stop),
                },
            );
        }
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
            self.fact(FactBody::AttemptFinished {
                id: attempt.clone(),
                status: a,
            });
            self.close_attempt_items(attempt, i, false);
        }
        if capture
            && self
                .state
                .checkpoints
                .iter()
                .any(|c| c.run_ordinal < r.ordinal)
        {
            self.fact(FactBody::RunWaitingForCapture {
                id: run.clone(),
                terminal: status,
            });
            self.effect(
                r.attempt,
                EffectBody::CaptureCheckpoint { run: run.clone() },
            );
        } else {
            self.fact(FactBody::RunFinished {
                id: run.clone(),
                status,
            });
            self.promote();
        }
    }
    fn create_run(&mut self, message: &SendMessage) -> Reply {
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
                self.stop_tasks(&attempt, ItemStatus::Interrupted);
                self.close_attempt_items(&attempt, ItemStatus::Interrupted, false);
                self.fact(FactBody::AttemptFinished {
                    id: attempt.clone(),
                    status: AttemptStatus::Superseded,
                });
                self.effect(
                    Some(attempt),
                    EffectBody::Provider(ProviderCommand::Interrupt),
                );
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
                    selection: target.selection,
                    runtime_mode: t.runtime_mode,
                    interaction_mode: t.interaction_mode,
                    text: message.text.clone(),
                    attachments: message.attachments.clone(),
                    native_thread: self
                        .state
                        .attempts
                        .iter()
                        .find(|a| a.run == *run)
                        .and_then(|a| a.native_thread.clone()),
                    resume_at: None,
                    context: String::new(),
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
        let queued = active.is_some() || held || !self.state.queued_runs().is_empty();
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
        if !matches!(command, Create { .. } | AcceptFork { .. }) {
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
                    self.stop_tasks(&owner, ItemStatus::Interrupted);
                    self.close_attempt_items(&owner, ItemStatus::Interrupted, false);
                    self.effect(
                        Some(owner),
                        EffectBody::Provider(ProviderCommand::Interrupt),
                    );
                }
                Reply::Accepted
            }
            BindNativeChild {
                owner,
                parent,
                task,
            } => {
                self.fact(FactBody::NativeChildBound {
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
                        self.effect(
                            Some(a.clone()),
                            EffectBody::Provider(ProviderCommand::Interrupt),
                        );
                        self.stop_tasks(a, ItemStatus::Cancelled);
                    }
                    self.finish(&run.id, RunStatus::Cancelled, false);
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
                        id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                        kind: TransferKind::ProviderHandoff,
                        source: t.id.clone(),
                        target: t.id,
                        boundary: self.state.runs.iter().map(|r| r.ordinal).max().unwrap_or(0),
                        text: context_text(self.state.visible_items().into_iter()),
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
                self.fact(FactBody::BackgroundWorkStopped);
                if let Some(attempt) = &target.attempt {
                    self.fact(FactBody::StopRequested {
                        attempt: attempt.clone(),
                    });
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::Provider(ProviderCommand::Interrupt),
                    );
                    self.stop_tasks(attempt, ItemStatus::Interrupted);
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
                if self
                    .state
                    .messages
                    .iter()
                    .any(|m| m.id == r.message && m.created_by == MessageAuthor::Agent)
                {
                    let ids = self
                        .state
                        .tasks
                        .iter()
                        .filter(|t| t.delivery == DeliveryState::Claimed)
                        .map(|t| t.id.clone())
                        .collect::<Vec<_>>();
                    for id in ids {
                        self.fact(FactBody::TaskDeliveryChanged {
                            id,
                            state: DeliveryState::Disposed,
                        });
                    }
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
                    RequestBody::Approval { options, .. }
                        if decision.is_none()
                            || decision
                                .is_some_and(|d| !options.iter().any(|o| o.decision == d)) =>
                    {
                        return reject("invalid-approval-decision");
                    }
                    RequestBody::Questions { questions }
                        if answers.is_none()
                            || questions.iter().any(|q| {
                                answers.as_ref().is_none_or(|a| !a.contains_key(&q.id))
                            }) =>
                    {
                        return reject("missing-question-answer");
                    }
                    _ => {}
                }
                if r.capability == ResponseCapability::NotResumable {
                    return reject("request-not-resumable");
                }
                let mut provider_answers = answers.clone();
                if let Some(a) = &mut provider_answers {
                    for (key, files) in attachments {
                        a.entry(key.clone())
                            .or_default()
                            .extend(files.iter().map(|f| f.path.clone()));
                    }
                }
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
                    let RequestBody::Questions { questions } = &r.body else {
                        return reject("question-not-found");
                    };
                    let text = questions
                        .iter()
                        .filter_map(|q| {
                            provider_answers
                                .as_ref()
                                .and_then(|a| a.get(&q.id))
                                .map(|a| format!("{}\n{}", q.question, a.join(", ")))
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let message = SendMessage {
                        created_by: MessageAuthor::User,
                        creation_source: "client".into(),
                        id: MessageId::new(format!("async-answer:{request}")).unwrap(),
                        text,
                        attachments: attachments.values().flatten().cloned().collect(),
                        selection: None,
                        mode: DispatchMode::QueueAfterActive,
                        intent: None,
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
                for (instance, head) in &cp.native_heads {
                    let native_thread = self.state.attempts.iter().rev().find_map(|a| {
                        self.state
                            .runs
                            .iter()
                            .find(|r| r.id == a.run && &r.selection.instance == instance)
                            .and(a.native_thread.clone())
                    });
                    if let Some(native_thread) = native_thread {
                        self.effect(
                            None,
                            EffectBody::Provider(ProviderCommand::Rollback {
                                native_thread,
                                absolute_head: head.clone(),
                            }),
                        );
                    }
                }
                if *restore_files {
                    self.effect(
                        None,
                        EffectBody::RestoreCheckpoint {
                            checkpoint: checkpoint.clone(),
                            file_ref: cp.file_ref,
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
                    .visible_items()
                    .into_iter()
                    .filter(|i| {
                        i.run.as_ref().is_none_or(|id| {
                            self.state
                                .runs
                                .iter()
                                .any(|r| &r.id == id && r.ordinal <= run.ordinal)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let context = context_text(history.iter());
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: target.clone(),
                        command: Box::new(AcceptFork {
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
                            context,
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
                context,
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
                self.fact(FactBody::ForkAccepted {
                    parent: parent.clone(),
                    boundary: *boundary,
                    history: history.clone(),
                });
                self.fact(FactBody::TransferOpened {
                    id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                    kind: TransferKind::Fork,
                    source: parent.clone(),
                    target: thread.clone(),
                    boundary: *boundary,
                    text: context.clone(),
                });
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
                let text = context_text(self.state.items.iter().filter(|i| {
                    i.run.as_ref().is_none_or(|id| {
                        self.state
                            .runs
                            .iter()
                            .any(|r| &r.id == id && r.status != RunStatus::RolledBack)
                    })
                }));
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: target.clone(),
                        command: Box::new(AcceptTransfer {
                            id: ContextTransferId::new(self.key("transfer", id.as_str())).unwrap(),
                            kind: TransferKind::MergeBack,
                            source: thread.id.clone(),
                            boundary,
                            text,
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
                text,
            } => {
                if self.state.transfers.iter().any(|t| {
                    t.id == *id
                        || (t.kind == *kind
                            && t.source == *source
                            && t.boundary == *boundary
                            && t.text == *text
                            && !t.superseded)
                }) {
                    return Reply::Accepted;
                }
                self.fact(FactBody::TransferOpened {
                    id: id.clone(),
                    kind: *kind,
                    source: source.clone(),
                    target: self.state.thread.as_ref().unwrap().id.clone(),
                    boundary: *boundary,
                    text: text.clone(),
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
                if self.state.tasks.iter().any(|t| &t.id == task) {
                    return reject("task-already-exists");
                }
                self.fact(FactBody::TaskStarted {
                    id: task.clone(),
                    native_key: task.to_string(),
                    run: Some(run.id),
                    attempt,
                    child: child.clone(),
                    parent: None,
                    app_owned: true,
                    prompt: prompt.clone(),
                    model: Some(selection.model.clone()),
                    wake: *wake,
                });
                let t = self.state.thread.as_ref().unwrap();
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: child.clone(),
                        command: Box::new(Create {
                            thread: child.clone(),
                            project: t.project.clone(),
                            title: prompt.lines().next().unwrap_or("Task").to_string(),
                            selection: selection.clone(),
                            runtime_mode: t.runtime_mode,
                            interaction_mode: t.interaction_mode,
                        }),
                    },
                );
                self.effect(
                    None,
                    EffectBody::SendToThread {
                        thread: child.clone(),
                        command: Box::new(Send(SendMessage {
                            created_by: MessageAuthor::Agent,
                            creation_source: "server".into(),
                            id: MessageId::new(self.key("delegate-message", task.as_str()))
                                .unwrap(),
                            text: prompt.clone(),
                            attachments: vec![],
                            selection: None,
                            mode: DispatchMode::StartImmediately,
                            intent: None,
                            source_plan: None,
                        })),
                    },
                );
                Reply::Thread(child.clone())
            }
            TaskResult {
                task,
                status,
                result,
            } => {
                if !self.state.tasks.iter().any(|t| &t.id == task) {
                    return reject("task-not-found");
                }
                self.fact(FactBody::TaskFinished {
                    id: task.clone(),
                    status: *status,
                    result: result.clone(),
                });
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
                if tasks.len() != task_ids.len() {
                    return reject("invalid-completion-cohort");
                }
                let text = tasks
                    .iter()
                    .map(|t| format!("Task {}: {}", t.id, t.result.as_deref().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join("\n");
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
        let task_ids = self
            .state
            .tasks
            .iter()
            .filter(|t| {
                t.app_owned
                    && t.status.terminal()
                    && t.delivery == DeliveryState::Pending
                    && (t.wake == CompletionWake::Always || self.state.active_run().is_none())
            })
            .map(|t| t.id.clone())
            .collect::<Vec<_>>();
        if !task_ids.is_empty() {
            self.effect(
                None,
                EffectBody::SendToThread {
                    thread: thread.id.clone(),
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
                name,
                input,
                output,
            } => ItemKind::DynamicTool {
                name: name.clone(),
                input: input.clone(),
                output: output.clone(),
            },
            ProviderItem::WebSearch { query } => ItemKind::WebSearch {
                query: query.clone(),
            },
            ProviderItem::Compaction { before, after } => ItemKind::Compaction {
                before: *before,
                after: *after,
            },
            ProviderItem::Notice { message } => ItemKind::SystemNotice {
                message: message.clone(),
            },
            ProviderItem::Error { message, retrying } => ItemKind::Error {
                message: message.clone(),
                retrying: *retrying,
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
    fn provider(&mut self, attempt: &RunAttemptId, event: &ProviderEvent) -> Reply {
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
        let background = matches!(
            event,
            ProviderEvent::SubagentStarted { .. }
                | ProviderEvent::SubagentProgress { .. }
                | ProviderEvent::SubagentFinished { .. }
                | ProviderEvent::Child { .. }
                | ProviderEvent::BackgroundTask { .. }
                | ProviderEvent::Wake { .. }
        );
        if !child
            && run
                .as_ref()
                .is_some_and(|r| !matches!(r.status, RunStatus::Starting | RunStatus::Running))
            && !(background
                && run
                    .as_ref()
                    .is_some_and(|r| matches!(r.status, RunStatus::Completed | RunStatus::Waiting)))
        {
            return Reply::Ignored;
        }
        let run_id = run.as_ref().map(|r| &r.id);
        use ProviderEvent::*;
        match event {
            PromptOffered { key } => self.fact(FactBody::PromptOffered {
                attempt: attempt.clone(),
                key: key.clone(),
            }),
            NativeOutput { .. } => {
                unreachable!("native output is routed before applying its events")
            }
            TurnAborted { .. } => {
                if let Some(run) = &run {
                    if self.state.messages.iter().any(|m| {
                        m.run.as_ref() == Some(&run.id)
                            && matches!(
                                m.intent,
                                InputIntent::Steer | InputIntent::PromotedQueuedToSteer
                            )
                    }) {
                        return Reply::Ignored;
                    }
                    self.finish(&run.id, RunStatus::Completed, true);
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
                    self.close_attempt_items(
                        attempt,
                        match status {
                            RunStatus::Failed => ItemStatus::Failed,
                            RunStatus::Interrupted => ItemStatus::Interrupted,
                            _ => ItemStatus::Completed,
                        },
                        false,
                    );
                    if let Some((parent, task)) = self.state.native_parent.clone() {
                        let result = context_text(
                            self.state
                                .items
                                .iter()
                                .filter(|i| matches!(i.kind, ItemKind::AssistantMessage { .. })),
                        );
                        self.effect(
                            Some(attempt.clone()),
                            EffectBody::SendToThread {
                                thread: parent,
                                command: Box::new(Command::TaskResult {
                                    task,
                                    status: match status {
                                        RunStatus::Failed => ItemStatus::Failed,
                                        RunStatus::Interrupted => ItemStatus::Interrupted,
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
                let id = self.provider_item(attempt, run_id, key, kind);
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
                key,
                body,
                capability,
            } => {
                let id = RuntimeRequestId::new(self.native_key("request", attempt, key)).unwrap();
                if self.state.requests.iter().any(|r| r.id == id) {
                    return Reply::Ignored;
                }
                self.fact(FactBody::RequestOpened {
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
                let existing = self
                    .state
                    .plans
                    .iter()
                    .find(|p| &p.native_key == key)
                    .map(|p| p.markdown.as_str())
                    .unwrap_or("");
                let markdown = format!("{existing}{text}");
                return self.provider(
                    attempt,
                    &ProviderEvent::Plan {
                        key: key.clone(),
                        markdown,
                        steps: vec![],
                    },
                );
            }
            Plan {
                key,
                markdown,
                steps,
            } => {
                let Some(run) = run_id else {
                    return Reply::Ignored;
                };
                let id = PlanId::new(self.native_key("plan", attempt, key)).unwrap();
                self.fact(FactBody::PlanRecorded {
                    id: id.clone(),
                    run: run.clone(),
                    native_key: key.clone(),
                    markdown: markdown.clone(),
                    steps: steps.clone(),
                });
                let item = TurnItemId::new(self.native_key("plan-item", attempt, key)).unwrap();
                if !self.state.items.iter().any(|i| i.id == item) {
                    self.item_start(
                        item.clone(),
                        Some(run.clone()),
                        Some(attempt.clone()),
                        key.clone(),
                        if steps.is_empty() {
                            ItemKind::ProposedPlan { plan: id }
                        } else {
                            ItemKind::TodoList { plan: id }
                        },
                    );
                }
                self.fact(FactBody::ItemTextReplaced {
                    id: item,
                    text: markdown.clone(),
                });
            }
            Usage(usage) => {
                if !child {
                    self.fact(FactBody::UsageRecorded {
                        attempt: attempt.clone(),
                        usage: usage.clone(),
                    });
                }
            }
            SubagentStarted {
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
                        self.fact(FactBody::TaskReopened { id: task.id });
                    }
                } else {
                    let id = NodeId::new(self.native_key("task", attempt, key)).unwrap();
                    let child_thread =
                        ThreadId::new(self.native_key("child", attempt, key)).unwrap();
                    let parent_task = parent
                        .as_ref()
                        .and_then(|p| self.state.tasks.iter().find(|t| &t.native_key == p))
                        .map(|t| t.id.clone());
                    self.fact(FactBody::TaskStarted {
                        id: id.clone(),
                        native_key: key.clone(),
                        run: run_id.cloned(),
                        attempt: attempt.clone(),
                        child: child_thread.clone(),
                        parent: parent_task,
                        app_owned: false,
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
                    self.effect(
                        Some(attempt.clone()),
                        EffectBody::SendToThread {
                            thread: child_thread.clone(),
                            command: Box::new(Command::Create {
                                thread: child_thread.clone(),
                                project: t.project,
                                title: prompt.clone(),
                                selection: t.selection,
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
                                owner: attempt.clone(),
                                parent: t.id,
                                task: id,
                            }),
                        },
                    );
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
                        progress: progress.clone(),
                        model: model.clone(),
                    });
                }
            }
            SubagentFinished {
                key,
                status,
                result,
            } => {
                if let Some(id) = self
                    .state
                    .tasks
                    .iter()
                    .find(|t| &t.native_key == key)
                    .map(|t| t.id.clone())
                {
                    self.fact(FactBody::TaskFinished {
                        id: id.clone(),
                        status: *status,
                        result: result.clone(),
                    });
                    if let Some(item) = self
                        .state
                        .items
                        .iter()
                        .find(|i| matches!(&i.kind,ItemKind::Subagent { task } if task==&id))
                        .map(|i| i.id.clone())
                    {
                        self.fact(FactBody::ItemCompleted {
                            id: item,
                            status: *status,
                        });
                    }
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
                        let label = match work.kind {
                            BackgroundKind::Command => "Command",
                            BackgroundKind::Monitor => "Monitor",
                            BackgroundKind::Subagent => "Subagent",
                            BackgroundKind::BackgroundTask => "Background task",
                        };
                        let outcome = match status {
                            ItemStatus::Completed => "finished",
                            ItemStatus::Cancelled | ItemStatus::Interrupted => "was stopped",
                            _ => "failed",
                        };
                        let id =
                            TurnItemId::new(self.native_key("notification", attempt, key)).unwrap();
                        self.item_start(
                            id.clone(),
                            run_id.cloned(),
                            Some(attempt.clone()),
                            key.clone(),
                            ItemKind::BackgroundNotification {
                                summary: format!("{label} \"{}\" {outcome}", work.description),
                                outcome: *status,
                                source: work.kind,
                            },
                        );
                        self.fact(FactBody::ItemCompleted {
                            id,
                            status: ItemStatus::Completed,
                        });
                    }
                    self.fact(FactBody::BackgroundTaskFinished {
                        key: key.clone(),
                        summary: summary.clone(),
                    });
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
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            attachments: vec![],
            intent: InputIntent::QueuedTurn,
            created_by: MessageAuthor::Agent,
            creation_source: "provider".into(),
        });
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
        if self.state.stopping.contains(owner) && self.state.active_run().is_none() {
            return Reply::Ignored;
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
                        self.stop_tasks(attempt, ItemStatus::Failed);
                        self.finish(&run.id, RunStatus::Failed, false);
                    }
                    // A failed control operation must never claim the native turn died.
                    ProviderOperation::Interrupt => {
                        self.stop_tasks(attempt, ItemStatus::Interrupted);
                        self.finish(&run.id, RunStatus::Interrupted, true);
                    }
                    _ => {
                        self.error_item(&run.id, message);
                    }
                }
            }
            EffectResult::CheckpointCaptured {
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
                if self.state.checkpoints.iter().any(|c| &c.id == checkpoint) {
                    return Reply::Ignored;
                }
                self.fact(FactBody::CheckpointCaptured {
                    id: checkpoint.clone(),
                    run: Some(run.clone()),
                    run_ordinal: r.ordinal,
                    native_heads: self.state.native_heads.clone(),
                    file_ref: file_ref.clone(),
                });
                if let Some(status) = self.state.captures.get(run).copied() {
                    self.fact(FactBody::RunFinished {
                        id: run.clone(),
                        status,
                    });
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
                if let Some(status) = self.state.captures.get(run).copied() {
                    self.fact(FactBody::CaptureFailed { run: run.clone() });
                    self.fact(FactBody::RunFinished {
                        id: run.clone(),
                        status,
                    });
                    self.promote();
                }
            }
            EffectResult::RollbackFinished { command } => {
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
            .filter(|t| !t.app_owned && !t.status.terminal())
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
        let items=self.state.items.iter().filter(|i| !i.status.terminal() && !matches!(&i.kind,ItemKind::Subagent { task } if self.state.tasks.iter().any(|t| &t.id==task && t.app_owned)) && !matches!(&i.kind,ItemKind::UserInputRequest { request } if self.state.requests.iter().any(|r| &r.id==request && r.capability==ResponseCapability::Message))).map(|i| i.id.clone()).collect::<Vec<_>>();
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
fn context_text<'a>(items: impl Iterator<Item = &'a Item>) -> String {
    items
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::UserMessage { .. }
                    | ItemKind::AssistantMessage { .. }
                    | ItemKind::ProposedPlan { .. }
            )
        })
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub struct ThreadMachine;
impl ThreadMachine {
    pub fn step(state: &State, envelope: &InputEnvelope) -> Step {
        // The common streaming path only inspects its owner and item. It does
        // not copy history or accumulated text into a scratch projection.
        if let Input::Provider {
            attempt,
            event: ProviderEvent::TextDelta { key, text, .. },
        } = &envelope.input
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
                i.attempt.as_ref() == Some(attempt) && &i.native_key == key && !i.status.terminal()
            }) {
                return Step {
                    facts: vec![Fact {
                        at: envelope.at.clone(),
                        body: FactBody::ItemTextAppended {
                            id: item.id.clone(),
                            offset: item.text.len(),
                            text: text.clone(),
                        },
                    }],
                    effects: vec![],
                    reply: Reply::Accepted,
                    receipt: None,
                };
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
        if file.mime_type.starts_with("image/") {
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
