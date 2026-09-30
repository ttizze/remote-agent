//! Live execution and unconfirmed input delivery. Native history is never retained here.
use agent_protocol::models::{Thread, ThreadResponse};

#[derive(Default)]
pub(super) struct SessionActor {
    pub(super) live: Thread,
    pub(super) pending_requests:
        std::collections::BTreeMap<agent_protocol::ids::RequestId, super::requests::PendingRequest>,
    pub(super) leases: usize,
    pub(super) submission_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
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
        self.live.submissions.retain(|id, delivery| {
            matches!(
                delivery,
                agent_protocol::session::SubmissionDelivery::Sending
            ) || !self
                .live
                .turns
                .iter()
                .flatten()
                .filter(|turn| turn.status != agent_protocol::execution::TurnStatus::Running)
                .flat_map(|turn| turn.items.iter().flatten())
                .any(|item| {
                    matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::UserMessage { .. }
                    ) && item.client_input_id.as_ref() == Some(id)
                })
        });
        if let Some(turns) = &mut self.live.turns {
            turns.retain(|turn| turn.status == agent_protocol::execution::TurnStatus::Running || self.live.submissions.values().any(|delivery| matches!(delivery, agent_protocol::session::SubmissionDelivery::Accepted {turn_id:Some(id)} if id == &turn.id)));
        }
        if self
            .live
            .turns
            .as_ref()
            .is_none_or(|turns| turns.is_empty())
            && self.live.status != agent_protocol::models::SessionStatus::Running
        {
            self.live.status = agent_protocol::models::SessionStatus::Unknown;
        }
        !self.live.submissions.is_empty()
            || !self.pending_requests.is_empty()
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
        if self.live.status != agent_protocol::models::SessionStatus::Unknown {
            response.thread.status = self.live.status;
        }
        response.thread.requests = self
            .pending_requests
            .iter()
            .map(|(id, pending)| (id.clone(), std::sync::Arc::new(pending.request.clone())))
            .collect();
        response.thread.submissions = self.live.submissions.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::session::SubmissionDelivery;

    #[test]
    fn finished_echo_retires_only_confirmed_input_and_stale_execution_status() {
        let mut actor = SessionActor {
            live: serde_json::from_value(serde_json::json!({"status":"idle","turns":[{"id":"done","status":"completed","items":[{"id":"echo","status":"unknown","clientInputId":"accepted","body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}}]}]})).unwrap(),
            ..Default::default()
        };
        actor.live.submissions.insert(
            "accepted".into(),
            SubmissionDelivery::Accepted {
                turn_id: Some("done".into()),
            },
        );
        actor
            .live
            .submissions
            .insert("waiting".into(), SubmissionDelivery::Sending);
        actor
            .live
            .submissions
            .insert("uncertain".into(), SubmissionDelivery::Unknown);
        assert!(actor.release());
        assert!(!actor.live.submissions.contains_key("accepted"));
        assert_eq!(actor.live.submissions.len(), 2);
        assert!(actor.live.turns.as_ref().unwrap().is_empty());
        assert!(
            actor.live.status == agent_protocol::models::SessionStatus::Unknown,
            "retained delivery evidence must not override newer provider execution state"
        );
    }
}
