use super::*;
use std::collections::BTreeSet;

/// The decision model is the saved model. Queued and executing work occupy
/// the same run map; observations never create a second mutable transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub thread: Thread,
    pub runs: BTreeMap<String, Run>,
    pub run_attempts: BTreeMap<String, RunAttempt>,
    pub nodes: BTreeMap<String, ExecutionNode>,
    pub provider_sessions: BTreeMap<String, ProviderSession>,
    pub attached_provider_sessions: BTreeSet<String>,
    pub provider_threads: BTreeMap<String, ProviderThread>,
    pub provider_turns: BTreeMap<String, ProviderTurn>,
    pub turn_items: BTreeMap<String, TurnItem>,
    pub runtime_requests: BTreeMap<String, RuntimeRequest>,
    pub plans: BTreeMap<String, Plan>,
    pub subagents: BTreeMap<String, Subagent>,
    pub checkpoint_scopes: BTreeMap<String, CheckpointScope>,
    pub checkpoints: BTreeMap<String, Checkpoint>,
    pub context_handoffs: BTreeMap<String, ContextHandoff>,
    pub context_transfers: BTreeMap<String, ContextTransfer>,
    pub effects: BTreeMap<String, Effect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Change {
    #[serde(rename = "thread.created")]
    ThreadCreated(Thread),
    #[serde(rename = "thread.metadata-updated")]
    Thread(Thread),
    #[serde(rename = "run.created")]
    RunCreated(Run),
    #[serde(rename = "run.updated")]
    Run(Run),
    #[serde(rename = "run-attempt.created")]
    RunAttemptCreated(RunAttempt),
    #[serde(rename = "run-attempt.updated")]
    RunAttempt(RunAttempt),
    #[serde(rename = "node.updated")]
    Node(ExecutionNode),
    #[serde(rename = "provider-session.updated")]
    ProviderSession(ProviderSession),
    #[serde(rename = "provider-session.attached")]
    ProviderSessionAttached(ProviderSession),
    #[serde(rename = "provider-session.detached")]
    ProviderSessionDetached(ProviderSessionDetached),
    #[serde(rename = "provider-thread.updated")]
    ProviderThread(ProviderThread),
    #[serde(rename = "provider-turn.updated")]
    ProviderTurn(ProviderTurn),
    #[serde(rename = "turn-item.updated")]
    TurnItem(TurnItem),
    #[serde(rename = "runtime-request.updated")]
    RuntimeRequest(RuntimeRequest),
    #[serde(rename = "plan.updated")]
    Plan(Plan),
    #[serde(rename = "subagent.updated")]
    Subagent(Subagent),
    #[serde(rename = "checkpoint-scope.created")]
    CheckpointScope(CheckpointScope),
    #[serde(rename = "checkpoint.captured")]
    Checkpoint(Checkpoint),
    #[serde(rename = "context-handoff.updated")]
    ContextHandoff(ContextHandoff),
    #[serde(rename = "context-transfer.created")]
    ContextTransferCreated(ContextTransfer),
    #[serde(rename = "context-transfer.updated")]
    ContextTransfer(ContextTransfer),
}

impl State {
    pub fn new(thread: Thread) -> Self {
        Self {
            thread,
            runs: BTreeMap::new(),
            run_attempts: BTreeMap::new(),
            nodes: BTreeMap::new(),
            provider_sessions: BTreeMap::new(),
            attached_provider_sessions: BTreeSet::new(),
            provider_threads: BTreeMap::new(),
            provider_turns: BTreeMap::new(),
            turn_items: BTreeMap::new(),
            runtime_requests: BTreeMap::new(),
            plans: BTreeMap::new(),
            subagents: BTreeMap::new(),
            checkpoint_scopes: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            context_handoffs: BTreeMap::new(),
            context_transfers: BTreeMap::new(),
            effects: BTreeMap::new(),
        }
    }

    pub fn apply(&self, decision: &Decision) -> Self {
        let mut next = self.clone();
        for change in &decision.changes {
            match change {
                Change::ThreadCreated(value) | Change::Thread(value) => next.thread = value.clone(),
                Change::RunCreated(value) | Change::Run(value) => {
                    next.runs.insert(value.id.clone(), value.clone());
                }
                Change::RunAttemptCreated(value) | Change::RunAttempt(value) => {
                    next.run_attempts.insert(value.id.clone(), value.clone());
                }
                Change::Node(value) => {
                    next.nodes.insert(value.id.clone(), value.clone());
                }
                Change::ProviderSession(value) => {
                    next.provider_sessions
                        .insert(value.id.clone(), value.clone());
                }
                Change::ProviderSessionAttached(value) => {
                    next.attached_provider_sessions.insert(value.id.clone());
                    next.provider_sessions
                        .insert(value.id.clone(), value.clone());
                }
                Change::ProviderSessionDetached(value) => {
                    next.attached_provider_sessions
                        .remove(&value.provider_session_id);
                }
                Change::ProviderThread(value) => {
                    next.provider_threads
                        .insert(value.id.clone(), value.clone());
                }
                Change::ProviderTurn(value) => {
                    next.provider_turns.insert(value.id.clone(), value.clone());
                }
                Change::TurnItem(value) => {
                    next.turn_items.insert(value.id.clone(), value.clone());
                }
                Change::RuntimeRequest(value) => {
                    next.runtime_requests
                        .insert(value.id.clone(), value.clone());
                }
                Change::Plan(value) => {
                    next.plans.insert(value.id.clone(), value.clone());
                }
                Change::Subagent(value) => {
                    next.subagents.insert(value.id.clone(), value.clone());
                }
                Change::CheckpointScope(value) => {
                    next.checkpoint_scopes
                        .insert(value.id.clone(), value.clone());
                }
                Change::Checkpoint(value) => {
                    next.checkpoints.insert(value.id.clone(), value.clone());
                }
                Change::ContextHandoff(value) => {
                    next.context_handoffs
                        .insert(value.id.clone(), value.clone());
                }
                Change::ContextTransferCreated(value) | Change::ContextTransfer(value) => {
                    next.context_transfers
                        .insert(value.id.clone(), value.clone());
                }
            }
        }
        for effect in &decision.effects {
            next.effects.insert(effect.id.clone(), effect.clone());
        }
        next
    }

    pub fn request_item(&self, request_id: &str) -> Option<&TurnItem> {
        let request = self.runtime_requests.get(request_id)?;
        self.turn_items.values().find(|item|item.node_id.as_ref()==Some(&request.node_id)
            && (matches!(&item.body,TurnItemBody::UserInputRequest {request_id:id,..}|TurnItemBody::ApprovalRequest {request_id:id,..} if id==request_id)
                    || matches!(item.body,TurnItemBody::DynamicTool {..}) && matches!(request.kind.as_str(),"dynamic_tool_call"|"mcp-elicitation")))
    }

    pub fn selected_native_thread(&self) -> Option<&ProviderThread> {
        let thread = self
            .thread
            .active_provider_thread_id
            .as_ref()
            .and_then(|id| self.provider_threads.get(id))?;
        let reference = thread.native_thread_ref.as_ref()?;
        (thread.app_thread_id.as_deref() == Some(&self.thread.id)
            && thread.provider_instance_id == self.thread.provider_instance_id
            && reference.driver == thread.driver
            && reference
                .native_id
                .as_ref()
                .is_some_and(|id| !id.is_empty()))
        .then_some(thread)
    }

    pub fn native_work_blocks_delivery(&self) -> bool {
        self.selected_native_thread()
            .is_some_and(|thread| thread.status == ProviderThreadStatus::Active)
    }

    /// A completed native attempt can report background work while the app
    /// still captures its checkpoint. It cannot adopt an active or uncertain
    /// application write, even though both may have a Waiting run.
    pub fn accepts_external_observation(&self) -> bool {
        !self.has_uncertain_input()
            && self.active_run().is_none_or(|run| {
                run.status == RunStatus::Waiting
                    && self
                        .active_attempt(run.active_attempt_id.as_deref())
                        .is_some_and(|attempt| attempt.status == RunAttemptStatus::Completed)
            })
    }

    pub fn has_pending_rollback(&self) -> bool {
        self.effects.values().any(|effect| {
            matches!(effect.request, EffectRequest::ProviderThreadRollback { .. })
                && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
        })
    }

    /// T3's rolledBackAttemptIds: audit turns stay saved, while an owned
    /// rolled-back attempt no longer bounds the current native conversation.
    pub fn is_rolled_back_attempt(&self, attempt_id: Option<&str>) -> bool {
        attempt_id
            .and_then(|id| self.run_attempts.get(id))
            .and_then(|attempt| self.runs.get(&attempt.run_id))
            .is_some_and(|run| run.status == RunStatus::RolledBack)
    }

    pub fn active_run(&self) -> Option<&Run> {
        self.runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Preparing
                        | RunStatus::Starting
                        | RunStatus::Running
                        | RunStatus::Waiting
                )
            })
            .max_by_key(|run| run.ordinal)
    }

    pub fn next_run_ordinal(&self) -> Option<u64> {
        self.runs
            .values()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            .checked_add(1)
    }

    pub fn next_queued_run(&self) -> Option<&Run> {
        self.runs
            .values()
            .filter(|run| run.status == RunStatus::Queued && !run.queue_held)
            .min_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal))
    }

    pub fn last_delivered_run(&self, provider_thread_id: &str) -> Option<&Run> {
        self.runs
            .values()
            .filter(|run| {
                run.provider_thread_id.as_deref() == Some(provider_thread_id)
                    && run.is_handoff_source()
                    && (run.status == RunStatus::Completed
                        || self.provider_turns.values().any(|turn| {
                            run.active_attempt_id
                                .as_ref()
                                .is_some_and(|id| turn.run_attempt_id.as_ref() == Some(id))
                        }))
            })
            .max_by_key(|run| run.ordinal)
    }

    pub fn context_source_point(&self, run: &Run) -> ContextSourcePoint {
        ContextSourcePoint {
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            checkpoint_id: run.checkpoint_id.clone(),
            turn_item_id: None,
            provider_thread_ref: run
                .active_attempt_id
                .as_ref()
                .and_then(|id| self.run_attempts.get(id))
                .and_then(|attempt| attempt.native_thread_ref.clone()),
            provider_turn_ref: self
                .provider_turns
                .values()
                .filter(|turn| {
                    run.active_attempt_id
                        .as_ref()
                        .is_some_and(|id| turn.run_attempt_id.as_ref() == Some(id))
                })
                .max_by_key(|turn| turn.ordinal)
                .and_then(|turn| turn.native_turn_ref.clone()),
        }
    }

    /// A failed/cancelled start has not delivered its fork context. The next
    /// input claims that same transfer instead of starting without its history.
    pub fn pending_fork(&self) -> Option<&ContextTransfer> {
        self.unconsumed_fork().filter(|transfer| {
            transfer.target_run_id.as_ref().is_none_or(|id| {
                self.runs.get(id).is_some_and(|run| {
                    matches!(
                        run.status,
                        RunStatus::Completed
                            | RunStatus::Failed
                            | RunStatus::Interrupted
                            | RunStatus::Cancelled
                            | RunStatus::RolledBack
                    )
                })
            })
        })
    }

    pub fn unconsumed_fork(&self) -> Option<&ContextTransfer> {
        self.context_transfers.values().find(|transfer| {
            transfer.kind == TransferType::Fork
                && transfer.target_thread_id == self.thread.id
                && transfer.status == TransferStatus::Pending
                && transfer.resolution.is_none()
        })
    }

    pub fn has_uncertain_input(&self) -> bool {
        self.effects.values().any(|effect| {
            effect.status == EffectStatus::Running
                && effect.last_error.is_some()
                && matches!(
                    effect.request,
                    EffectRequest::ProviderTurnStart { .. }
                        | EffectRequest::ProviderTurnSteer { .. }
                        | EffectRequest::ProviderTurnRestart { .. }
                )
        })
    }

    pub fn active_attempt(&self, id: Option<&str>) -> Option<&RunAttempt> {
        id.and_then(|id| self.run_attempts.get(id))
    }

    /// Actual native admission is witnessed by a provider turn, not merely a
    /// queued/allocated attempt. A rebound physical resource starts separately.
    pub fn accepted_native_run_ids(
        &self,
        provider_thread_id: &str,
        native_id: &str,
    ) -> BTreeSet<&str> {
        self.provider_turns
            .values()
            .filter(|turn| turn.provider_thread_id == provider_thread_id)
            .filter_map(|turn| {
                turn.run_attempt_id
                    .as_ref()
                    .and_then(|id| self.run_attempts.get(id))
            })
            .filter(|attempt| {
                attempt.provider_thread_id == provider_thread_id
                    && attempt
                        .native_thread_ref
                        .as_ref()
                        .and_then(|reference| reference.native_id.as_deref())
                        == Some(native_id)
            })
            .map(|attempt| attempt.run_id.as_str())
            .collect()
    }

    pub fn provider_session(&self, provider_thread_id: &str) -> Option<&ProviderSession> {
        self.provider_threads
            .get(provider_thread_id)
            .and_then(|thread| thread.provider_session_id.as_ref())
            .filter(|id| self.attached_provider_sessions.contains(*id))
            .and_then(|id| self.provider_sessions.get(id))
    }

    pub fn accepts_observation(&self, attempt_id: &str, session_id: &str) -> bool {
        self.owned_attempt(attempt_id, session_id)
            .is_some_and(|attempt| {
                matches!(
                    attempt.status,
                    RunAttemptStatus::Pending | RunAttemptStatus::Running
                ) && self.runs.get(&attempt.run_id).is_some_and(|run| {
                    matches!(
                        run.status,
                        RunStatus::Starting | RunStatus::Running | RunStatus::Waiting
                    )
                })
            })
    }

    /// Completed attempts may still own detached work while their process lives.
    pub fn accepts_background_observation(&self, attempt_id: &str, session_id: &str) -> bool {
        self.owned_attempt(attempt_id, session_id)
            .is_some_and(|attempt| {
                attempt.status == RunAttemptStatus::Completed
                    && (self
                        .provider_threads
                        .get(&attempt.provider_thread_id)
                        .is_some_and(|thread| !thread.pending_background_tasks.is_empty())
                        || self.turn_items.values().any(|item| {
                            item.run_id.as_ref() == Some(&attempt.run_id)
                                && super::execution::is_pending_background_item(item)
                        }))
            })
    }

    pub fn native_thread_matches(&self, attempt_id: &str, driver: &str, native_id: &str) -> bool {
        self.run_attempts
            .get(attempt_id)
            .and_then(|attempt| attempt.native_thread_ref.as_ref())
            .is_some_and(|reference| {
                reference.driver == driver && reference.native_id.as_deref() == Some(native_id)
            })
    }

    pub fn owns_native_source(
        &self,
        attempt_id: &str,
        session_id: &str,
        driver: &str,
        native_id: &str,
    ) -> bool {
        self.owned_attempt(attempt_id, session_id).is_some()
            && self.native_thread_matches(attempt_id, driver, native_id)
    }

    /// Canonical input lookup, shared by queued editing, start admission and
    /// context delivery. Native/transcript IDs do not identify an app input.
    pub fn user_message_item(&self, message_id: &str) -> Option<&TurnItem> {
        self.turn_items.values().find(|item| {
            matches!(&item.body,
            TurnItemBody::UserMessage {message_id:id,..} if id == message_id)
        })
    }

    pub(super) fn owned_attempt(&self, attempt_id: &str, session_id: &str) -> Option<&RunAttempt> {
        let attempt = self.run_attempts.get(attempt_id)?;
        self.runs
            .get(&attempt.run_id)
            .filter(|run| {
                run.active_attempt_id.as_deref() == Some(attempt_id)
                    && run.status != RunStatus::RolledBack
                    && self
                        .provider_session(&attempt.provider_thread_id)
                        .is_some_and(|session| {
                            session.id == session_id
                                && matches!(
                                    session.status,
                                    ProviderSessionStatus::Ready
                                        | ProviderSessionStatus::Running
                                        | ProviderSessionStatus::Waiting
                                )
                        })
            })
            .map(|_| attempt)
    }
}

impl State {
    /// T3 contextBoundaryOrdinal: runs before the latest started fresh-context
    /// run never reach a provider again.
    pub fn context_boundary_ordinal(&self) -> u64 {
        self.runs
            .values()
            .filter(|run| run.starts_context())
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
    }

    /// T3 rootProviderThreadsForProvider: this thread's latest root native
    /// conversation for the instance, excluding any predating the boundary.
    pub fn root_provider_thread(&self, instance: &str, boundary: u64) -> Option<&ProviderThread> {
        self.provider_threads
            .values()
            .filter(|thread| {
                thread.app_thread_id.as_deref() == Some(self.thread.id.as_str())
                    && thread.provider_instance_id == instance
                    && thread.owner_node_id.is_none()
                    && !thread.predates(boundary)
            })
            .max_by(|a, b| {
                a.last_run_ordinal
                    .unwrap_or(0)
                    .cmp(&b.last_run_ordinal.unwrap_or(0))
                    .then_with(|| a.updated_at.cmp(&b.updated_at))
                    .then_with(|| a.id.cmp(&b.id))
            })
    }
}

impl Thread {
    /// A provider-native subagent thread: read-only, and its prompts come from
    /// the parent agent rather than from the user.
    pub fn is_provider_subagent(&self) -> bool {
        self.creation_source == CreationSource::Provider
            && self.lineage.relationship_to_parent.as_deref() == Some("subagent")
    }

    /// T3 createdBy of a native user message in this thread.
    pub fn native_prompt_actor(&self) -> Actor {
        if self.is_provider_subagent() {
            Actor::Agent
        } else {
            Actor::User
        }
    }
}

impl ProviderThread {
    /// T3 providerThreadPredatesBoundary.
    pub fn predates(&self, boundary: u64) -> bool {
        boundary > 0 && self.first_run_ordinal.is_none_or(|first| first < boundary)
    }
}

impl Run {
    /// T3 cancelledBeforeStart: a stopped run that never reached a provider.
    pub fn cancelled_before_start(&self) -> bool {
        self.status == RunStatus::Cancelled
            && self.started_at.is_none()
            && self.restart_continuation_of_run_id.is_none()
    }

    /// A fresh start that took effect. A rewound fresh start never happened.
    pub fn starts_context(&self) -> bool {
        self.fresh_context
            && !matches!(self.status, RunStatus::Queued | RunStatus::RolledBack)
            && !self.cancelled_before_start()
    }

    pub fn is_handoff_source(&self) -> bool {
        matches!(
            self.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Interrupted
        ) || (self.status == RunStatus::Cancelled && self.started_at.is_some())
    }
}
