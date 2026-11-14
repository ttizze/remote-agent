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
                .provider_thread_id
                .as_ref()
                .and_then(|id| self.provider_threads.get(id))
                .and_then(|thread| thread.native_thread_ref.clone()),
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

    pub fn native_thread_matches(&self, attempt_id: &str, driver: &str, native_id: &str) -> bool {
        self.run_attempts
            .get(attempt_id)
            .and_then(|attempt| self.provider_threads.get(&attempt.provider_thread_id))
            .and_then(|thread| thread.native_thread_ref.as_ref())
            .is_some_and(|reference| {
                reference.driver == driver && reference.native_id.as_deref() == Some(native_id)
            })
    }

    pub(super) fn owned_attempt(&self, attempt_id: &str, session_id: &str) -> Option<&RunAttempt> {
        let Some(attempt) = self.run_attempts.get(attempt_id) else {
            return None;
        };
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

impl Run {
    pub fn is_handoff_source(&self) -> bool {
        matches!(
            self.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Interrupted
        ) || (self.status == RunStatus::Cancelled && self.started_at.is_some())
    }
}
