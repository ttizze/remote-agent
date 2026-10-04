//! Live execution and unconfirmed input delivery. Native history is never retained here.
use agent_protocol::models::Thread;

#[derive(Default)]
pub(super) struct SessionActor {
    pub(super) timeline: agent_protocol::session::Timeline,
    pub(super) request_origins:
        std::collections::BTreeMap<agent_protocol::ids::RequestId, super::requests::RequestOrigin>,
    pub(super) leases: usize,
    pub(super) submission_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub(super) import_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub(super) subscriptions: std::collections::HashMap<
        uuid::Uuid,
        (super::routing::SessionId, super::routing::Outbound),
    >,
}
impl SessionActor {
    /// Reads and submissions may race completion. Keep completed turns until leases finish,
    /// then retain only live work and delivery evidence. Never adopt native history.
    pub(super) fn release(&mut self) -> bool {
        if self.leases > 0 {
            return true;
        }
        // Retire evidence only once the provider has echoed the input in a
        // finished turn. Completion alone says nothing about a queued input or
        // an RPC still in flight on another client.
        self.timeline.submissions.retain(|id, delivery| {
            !agent_protocol::session::submission_confirmed(
                id,
                delivery,
                self.timeline.turns.as_deref().unwrap_or_default(),
            )
        });
        if let Some(turns) = &mut self.timeline.turns {
            turns.retain(|turn| turn.status == agent_protocol::execution::TurnStatus::Running || self.timeline.submissions.values().any(|delivery| matches!(delivery, agent_protocol::session::SubmissionDelivery::Accepted {turn_id:Some(id)} if id == &turn.id)));
        }
        if self.timeline.turns.as_ref().is_some_and(Vec::is_empty) {
            self.timeline.turns = None;
        }
        if self.timeline.turns.is_none()
            && self.timeline.status != agent_protocol::models::SessionStatus::Running
        {
            self.timeline.status = agent_protocol::models::SessionStatus::Unknown;
        }
        !self.timeline.submissions.is_empty()
            || !self.timeline.requests.is_empty()
            || self.timeline.turns.is_some()
    }

    pub(super) fn overlay(&self, mut thread: Thread) -> Thread {
        for turn in self.timeline.turns.iter().flatten() {
            // Native transcripts do not identify a still-running local process.
            // Its owned turn is authoritative, even if persisted messages look complete.
            let turns = thread.turns.get_or_insert_default();
            if let Some(index) = turns.iter().rposition(|old| old.id == turn.id) {
                turns[index] = turn.clone();
            } else {
                turns.push(turn.clone());
            }
        }
        if self.timeline.status != agent_protocol::models::SessionStatus::Unknown {
            thread.status = self.timeline.status;
        }
        thread.requests = self.timeline.requests.clone();
        thread.submissions.extend(self.timeline.submissions.clone());
        thread
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::session::SubmissionDelivery;

    #[test]
    fn finished_echo_retires_only_confirmed_input_and_stale_execution_status() {
        let mut actor = SessionActor {
            timeline: agent_protocol::session::Timeline {
                status: agent_protocol::models::SessionStatus::Idle,
                turns: Some(serde_json::from_value(serde_json::json!([{"id":"done","status":"completed","items":[{"id":"echo","status":"unknown","clientInputId":"accepted","body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}}]}])).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        actor.timeline.submissions.insert(
            "accepted".into(),
            SubmissionDelivery::Accepted {
                turn_id: Some("done".into()),
            },
        );
        actor
            .timeline
            .submissions
            .insert("waiting".into(), SubmissionDelivery::Sending);
        actor
            .timeline
            .submissions
            .insert("uncertain".into(), SubmissionDelivery::Unknown);
        assert!(actor.release());
        assert!(!actor.timeline.submissions.contains_key("accepted"));
        assert_eq!(actor.timeline.submissions.len(), 2);
        assert!(actor.timeline.turns.is_none());
        assert!(
            actor.timeline.status == agent_protocol::models::SessionStatus::Unknown,
            "retained delivery evidence must not override newer provider execution state"
        );
        let mut durable = Thread::default();
        durable
            .submissions
            .insert("after-restart".into(), SubmissionDelivery::Unknown);
        let merged = actor.overlay(durable);
        assert_eq!(
            merged.submissions["after-restart"],
            SubmissionDelivery::Unknown
        );
        assert_eq!(merged.submissions["waiting"], SubmissionDelivery::Sending);
    }
}
