//! Bounded recovery of terminal Codex capacity failures. The Host owns the wait
//! and admission; the adapter owns native continuation without another prompt.
use super::*;
use crate::providers::AgentChange;
use agent_protocol::{
    execution::{RetryEvidence, SessionStatus, TurnStatus},
    ids::TurnId,
    models::Turn,
    session::{SessionChange, SessionRef},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const DELAYS: [u64; 5] = [2, 4, 8, 16, 30];

fn capacity_failure(status: TurnStatus, code: Option<&str>, provider_retrying: bool) -> bool {
    status == TurnStatus::Failed && code == Some("serverOverloaded") && !provider_retrying
}

enum Finish {
    Continued,
    Interrupted,
    Failed,
}

pub(super) struct Run {
    turns: Vec<TurnId>,
    attempts: usize,
    waiting: Option<Arc<Pending>>,
    starting: bool,
    cancelled: CancellationToken,
}

struct Pending {
    terminal: Turn,
    delay: Duration,
    cancelled: CancellationToken,
}

impl Run {
    fn new(turn: TurnId) -> Self {
        Self {
            turns: vec![turn],
            attempts: 0,
            waiting: None,
            starting: false,
            cancelled: CancellationToken::new(),
        }
    }

    fn current(&self) -> &TurnId {
        self.turns.last().expect("a run has a native turn")
    }

    fn failure(&mut self, turn: Turn) -> Option<Arc<Pending>> {
        if self.cancelled.is_cancelled() {
            return None;
        }
        // A generic HTTP 503, message text, or Codex's own willRetry notification
        // is insufficient evidence to start another native turn.
        if !capacity_failure(
            turn.status,
            turn.error
                .as_ref()
                .and_then(|error| error.provider_code.as_deref()),
            turn.error
                .as_ref()
                .and_then(|error| error.retry.as_ref())
                .is_some_and(|retry| retry.retrying),
        ) {
            return None;
        }
        if let Some(pending) = &self.waiting {
            return Some(pending.clone());
        }
        let delay = Duration::from_secs(*DELAYS.get(self.attempts)?);
        self.attempts += 1;
        let pending = Arc::new(Pending {
            terminal: turn,
            delay,
            cancelled: self.cancelled.clone(),
        });
        self.waiting = Some(pending.clone());
        Some(pending)
    }

    fn owns(&self, pending: &Arc<Pending>) -> bool {
        self.waiting
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, pending))
    }

    fn ready(&self, pending: &Arc<Pending>) -> bool {
        !self.cancelled.is_cancelled() && self.owns(pending)
    }

    fn cancel(&mut self) -> Option<Arc<Pending>> {
        self.cancelled.cancel();
        if self.starting {
            None
        } else {
            self.waiting.take()
        }
    }
}

impl HostRpcService {
    pub(super) fn capacity_event(&self, event: &mut AgentChange) {
        match event {
            AgentChange::Stopped {
                provider: ProviderKind::Codex,
                ..
            } => {
                let mut runs = self.inner.capacity.lock().unwrap();
                for run in runs.values() {
                    run.cancelled.cancel();
                }
                runs.clear();
            }
            AgentChange::Request { session, .. } => {
                self.cancel_capacity_wait(session, Finish::Failed);
            }
            AgentChange::Session { session, change } => {
                self.capacity_change(session, change);
            }
            _ => {}
        }
    }

    fn capacity_change(&self, session: &SessionRef, change: &mut SessionChange) {
        let mut runs = self.inner.capacity.lock().unwrap();
        match change {
            SessionChange::Status { status } => {
                if runs.get(session).is_some_and(|run| run.waiting.is_some()) {
                    *status = SessionStatus::Running;
                }
            }
            SessionChange::Turn {
                turn,
                completed: false,
            } if turn.provider.unwrap_or(session.provider) == ProviderKind::Codex => {
                match runs.get_mut(session) {
                    Some(run) if run.current() == &turn.id => {}
                    Some(run) if run.starting => {
                        if let Some(pending) = run.waiting.take() {
                            self.finish_capacity_turn(session, &pending, Finish::Continued);
                        }
                        run.turns.push(turn.id.clone());
                        run.starting = false;
                    }
                    _ => {
                        if let Some(pending) =
                            runs.get_mut(session).and_then(|run| run.waiting.take())
                        {
                            self.finish_capacity_turn(session, &pending, Finish::Continued);
                        }
                        runs.insert(session.clone(), Run::new(turn.id.clone()));
                    }
                }
            }
            SessionChange::Turn { turn, completed } if *completed => {
                let Some(run) = runs.get_mut(session) else {
                    return;
                };
                if run.current() != &turn.id {
                    return;
                }
                let mut terminal = turn.clone();
                if terminal.status == TurnStatus::Failed && terminal.error.is_none() {
                    // Codex may send the typed failure only in an earlier error
                    // notification. Read its existing live evidence rather than
                    // guessing from message text or keeping another error cache.
                    terminal.error = run
                        .waiting
                        .as_ref()
                        .and_then(|pending| pending.terminal.error.clone())
                        .or_else(|| {
                            self.inner
                                .router
                                .current_turn(session, turn.id.as_str())
                                .and_then(|current| current.error.clone())
                        });
                }
                let scheduled = run.waiting.is_some();
                let Some(pending) = run.failure(terminal) else {
                    runs.remove(session);
                    return;
                };
                turn.status = TurnStatus::Running;
                turn.error = pending.terminal.error.clone();
                turn.completed_at_ms = None;
                turn.duration_ms = None;
                if let Some(error) = &mut turn.error {
                    error.retry = Some(RetryEvidence {
                        retrying: true,
                        overloaded: true,
                    });
                }
                *completed = false;
                if !scheduled {
                    self.schedule_capacity_retry(session.clone(), pending);
                }
            }
            _ => {}
        }
    }

    fn finish_capacity_turn(&self, session: &SessionRef, pending: &Pending, finish: Finish) {
        let mut turn = pending.terminal.clone();
        if matches!(finish, Finish::Interrupted) {
            turn.status = TurnStatus::Interrupted;
        }
        // Once continuation starts, this failure no longer describes a stopped
        // task. Terminalize its native turn and clear the temporary retry notice.
        if !matches!(finish, Finish::Failed) {
            turn.error = None;
        }
        self.inner.router.session_change(
            session,
            SessionChange::Turn {
                turn,
                completed: true,
            },
        );
    }

    fn cancel_capacity_wait(&self, session: &SessionRef, finish: Finish) {
        let mut runs = self.inner.capacity.lock().unwrap();
        let Some(run) = runs.get_mut(session) else {
            return;
        };
        if run.waiting.is_none() {
            return;
        }
        if let Some(pending) = run.cancel() {
            self.finish_capacity_turn(session, &pending, finish);
            runs.remove(session);
        }
    }

    pub(super) fn capacity_input(&self, session: &SessionRef) {
        self.cancel_capacity_wait(session, Finish::Continued);
        if let Some(run) = self.inner.capacity.lock().unwrap().get_mut(session) {
            run.attempts = 0;
            run.turns = vec![run.current().clone()];
            run.cancelled = CancellationToken::new();
        }
    }

    pub(super) fn capacity_compaction(&self, session: &SessionRef) {
        // A failed /compact is not a request to resume the preceding task.
        self.cancel_capacity_wait(session, Finish::Failed);
        if let Some(run) = self.inner.capacity.lock().unwrap().get(session) {
            run.cancelled.cancel();
        }
    }

    pub(super) fn capacity_interrupt(&self, session: &SessionRef, turn: &TurnId) -> (bool, TurnId) {
        let mut runs = self.inner.capacity.lock().unwrap();
        let Some(run) = runs.get_mut(session).filter(|run| run.turns.contains(turn)) else {
            return (false, turn.clone());
        };
        let current = run.current().clone();
        let waiting = run.waiting.is_some();
        if let Some(pending) = run.cancel() {
            self.finish_capacity_turn(session, &pending, Finish::Interrupted);
            runs.remove(session);
        }
        (waiting, current)
    }

    fn schedule_capacity_retry(&self, session: SessionRef, pending: Arc<Pending>) {
        let owner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::select! {
                _ = pending.cancelled.cancelled() => return,
                _ = tokio::time::sleep(pending.delay) => {},
            }
            let Some(inner) = owner.upgrade() else { return };
            let service = Self { inner };
            if let Err(error) = service.retry_capacity(&session, &pending).await {
                tracing::warn!(target: "agent", operation = "host.capacity.retry_failed", code = error.code);
                let mut runs = service.inner.capacity.lock().unwrap();
                if let Some(run) = runs.get(&session).filter(|run| run.owns(&pending)) {
                    let finish = if run.cancelled.is_cancelled() {
                        Finish::Interrupted
                    } else {
                        Finish::Failed
                    };
                    service.finish_capacity_turn(&session, &pending, finish);
                    runs.remove(&session);
                }
            }
        });
    }

    async fn retry_capacity(
        &self,
        session: &SessionRef,
        pending: &Arc<Pending>,
    ) -> Result<(), Failure> {
        let _lease = self
            .inner
            .router
            .retain_execution(session.clone())
            .map_err(|error| Failure::new("invalid_session", error))?;
        let _serial = self
            .inner
            .router
            .submission_lock(session)
            .lock_owned()
            .await;
        let _workspace = self.inner.worktree_access.read().await;
        if !self
            .inner
            .capacity
            .lock()
            .unwrap()
            .get(session)
            .is_some_and(|run| run.ready(pending))
        {
            return Ok(());
        }
        if self
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Failure::new("host_updating", "Host is updating"));
        }
        let row = self.inner.database.get(session)?;
        let native = row.as_ref().map_or(session, |row| &row.active);
        let source = row
            .as_ref()
            .map(|row| row.source(&pending.terminal.id))
            .transpose()?;
        if source.as_ref().is_some_and(|(source, _)| source != native) {
            return Err(Failure::new(
                "provider_changed",
                "The active provider changed",
            ));
        }
        let agent = self.inner.providers.get(native.provider)?;
        let state = agent.state(&native.id).await?;
        let options = self.inner.model_selections.resolve(
            native,
            state.response.model.as_ref(),
            state.response.effort.as_deref(),
            state.response.service_tier.as_deref(),
        )?;
        let thread = self
            .inner
            .router
            .overlay_execution(session, state.response.thread.clone());
        if native.provider != ProviderKind::Codex
            || state.response.thread.id.as_ref() != Some(native)
            || state.needs_reload
            || state.response.thread.parent_id.is_some()
            || !matches!(
                state.response.thread.status,
                SessionStatus::Idle | SessionStatus::Unavailable
            )
            || !thread.requests.is_empty()
        {
            return Err(Failure::new(
                "continuation_unavailable",
                "Native conversation cannot be continued",
            ));
        }
        self.require_provider_enabled(native.provider)?;
        {
            let mut runs = self.inner.capacity.lock().unwrap();
            let Some(run) = runs.get_mut(session).filter(|run| run.ready(pending)) else {
                return Ok(());
            };
            run.starting = true;
        }
        let receipt = agent
            .continue_turn(
                &native.id,
                options.model.as_ref().map(|model| model.id.as_str()),
                options.effort.as_deref(),
                options.service_tier.as_deref(),
            )
            .await?;
        if pending.cancelled.is_cancelled()
            && let Some(turn) = receipt.turn_id
        {
            agent.interrupt(&native.id, &turn).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
