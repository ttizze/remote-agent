//! Only live execution state. Native history is read for each open and is never retained here.
use agent_core::{
    models::{Thread, ThreadResponse},
    session::SessionChange,
};
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct SessionActor {
    pub(super) live: Thread,
    pub(super) readers: usize,
    pub(super) inputs: HashSet<String>,
}
impl SessionActor {
    pub(super) fn update(&mut self, change: &SessionChange) -> Result<(), &'static str> {
        self.live = change.apply(&self.live)?;
        if matches!(
            change,
            SessionChange::Turn {
                completed: true,
                ..
            }
        ) && self
            .live
            .status
            .as_ref()
            .is_none_or(|status| status.kind != agent_core::models::ThreadStatusKind::Active)
        {
            self.inputs.clear();
        }
        Ok(())
    }

    /// An open may race completion. Keep that execution until its readers finish,
    /// then release it even if clients remain subscribed. Never adopt native turns.
    pub(super) fn release(&mut self) -> bool {
        if self.readers > 0 {
            return true;
        }
        if let Some(turns) = &mut self.live.turns {
            turns.retain(|turn| turn.status.as_deref() == Some("inProgress"));
        }
        !self.inputs.is_empty()
            || !self.live.requests.is_empty()
            || self
                .live
                .turns
                .as_ref()
                .is_some_and(|turns| !turns.is_empty())
    }

    pub(super) fn overlay(&self, response: &mut ThreadResponse) {
        for turn in self.live.turns.iter().flatten() {
            // Native transcripts do not identify a still-running local process.
            // Its owned turn is authoritative, even if persisted messages look complete.
            let turns = response.thread.turns.get_or_insert_default();
            if let Some(index) = turns.iter().rposition(|old| old.id == turn.id) {
                turns[index] = turn.clone();
            } else {
                turns.push(turn.clone());
            }
        }
        response.thread.status = self.live.status.clone().or(response.thread.status.take());
        response.thread.requests = self.live.requests.clone();
    }
}
