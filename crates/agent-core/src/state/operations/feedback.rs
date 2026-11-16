use super::*;
pub use agent_protocol::feedback::UploadFeedback;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackSubmission {
    pub id: String,
    pub receipt: Option<agent_protocol::feedback::FeedbackReceipt>,
}

impl Operation for UploadFeedback {
    rpc_operation!();
    const REPORT_ERROR: bool = false;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Feedback {
            session: self.thread_id.clone(),
        })
    }
    fn navigation_bound(&self) -> bool {
        false
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        self.validate().map_err(str::to_owned)?;
        Arc::make_mut(&mut snapshot.sessions.feedback).insert(
            self.thread_id.clone(),
            FeedbackSubmission {
                id: self.id.clone(),
                receipt: None,
            },
        );
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if self.storage_scope == snapshot.connection.storage_scope
            && snapshot
                .sessions
                .feedback
                .get(&self.thread_id)
                .is_some_and(|submission| submission.id == self.id)
        {
            Arc::make_mut(&mut snapshot.sessions.feedback)
                .get_mut(&self.thread_id)
                .unwrap()
                .receipt = Some(output);
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::feedback::FeedbackPhase;
    fn request() -> UploadFeedback {
        UploadFeedback {
            id: uuid::Uuid::nil().to_string(),
            storage_scope: "host".into(),
            thread_id: crate::session::SessionRef::new(
                crate::session::ProviderKind::Codex,
                "session".into(),
            )
            .unwrap(),
            reason: None,
            include_logs: false,
        }
    }
    fn initial() -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.connection.storage_scope = "host".into();
        snapshot
    }
    #[test]
    fn feedback_notices_survive_navigation_and_old_dismissals_cannot_remove_new_reports() {
        let request = request();
        let session = request.thread_id.clone();
        let (mut pending, effects) =
            crate::state::reduce_intent(&initial(), "", Intent::UploadFeedback(request.clone()));
        assert_eq!(effects.len(), 1);
        Arc::make_mut(&mut pending.operations).insert(
            OperationKey::Feedback {
                session: session.clone(),
            },
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        let notice = pending.feedback_notice(session.clone()).unwrap();
        assert_eq!(notice.phase, FeedbackPhase::Uploading);
        assert!(!notice.can_dismiss);
        let (same, effects) =
            crate::state::reduce_intent(&pending, "", Intent::UploadFeedback(request.clone()));
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(
            &same.sessions.feedback,
            &pending.sessions.feedback
        ));
        let (same, _) = crate::state::reduce_intent(
            &pending,
            "",
            Intent::DismissFeedback {
                storage_scope: "host".into(),
                session: session.clone(),
                id: request.id.clone(),
            },
        );
        assert!(same.feedback_notice(session.clone()).is_some());
        let mut sent = pending.clone();
        request.clone().stale(
            &mut sent,
            "",
            agent_protocol::feedback::FeedbackReceipt {
                feedback_id: "report".into(),
            },
        );
        let notice = sent.feedback_notice(session.clone()).unwrap();
        assert_eq!(notice.phase, FeedbackPhase::Sent);
        assert_eq!(notice.feedback_id.as_deref(), Some("report"));
        Arc::make_mut(&mut sent.operations).remove(&OperationKey::Feedback {
            session: session.clone(),
        });
        let mut next = request.clone();
        next.id = "00000000-0000-0000-0000-000000000001".into();
        let (newer, _) =
            crate::state::reduce_intent(&sent, "", Intent::UploadFeedback(next.clone()));
        let mut newer = newer;
        Arc::make_mut(&mut newer.operations).insert(
            OperationKey::Feedback {
                session: session.clone(),
            },
            OperationState {
                generation: 2,
                phase: OperationPhase::Running,
            },
        );
        let (newer, _) = crate::state::reduce_intent(
            &newer,
            "",
            Intent::DismissFeedback {
                storage_scope: "host".into(),
                session: session.clone(),
                id: request.id.clone(),
            },
        );
        assert_eq!(newer.feedback_notice(session.clone()).unwrap().id, next.id);
        let mut newer = newer;
        request.stale(
            &mut newer,
            "",
            agent_protocol::feedback::FeedbackReceipt {
                feedback_id: "old report".into(),
            },
        );
        assert_eq!(
            newer.feedback_notice(session.clone()).unwrap().phase,
            FeedbackPhase::Uploading
        );
        let (disconnected, effects) =
            crate::state::reduce_event(&newer, crate::state::Event::Disconnected);
        assert!(effects.is_empty());
        assert_eq!(
            disconnected.feedback_notice(session.clone()).unwrap().phase,
            FeedbackPhase::Unknown
        );
        let (dismissed, _) = crate::state::reduce_intent(
            &disconnected,
            "",
            Intent::DismissFeedback {
                storage_scope: "host".into(),
                session: session.clone(),
                id: next.id,
            },
        );
        assert!(dismissed.feedback_notice(session).is_none());
    }
    #[test]
    fn failed_and_foreign_scope_feedback_has_no_copyable_id() {
        let request = request();
        let session = request.thread_id.clone();
        let (mut state, _) =
            crate::state::reduce_intent(&initial(), "", Intent::UploadFeedback(request));
        state.fail_operation(
            OperationKey::Feedback {
                session: session.clone(),
            },
            "Rejected".into(),
        );
        let notice = state.feedback_notice(session.clone()).unwrap();
        assert_eq!(notice.phase, FeedbackPhase::Failed);
        assert_eq!(notice.error.as_deref(), Some("Rejected"));
        assert!(notice.feedback_id.is_none());
        let (foreign, _) = crate::state::reduce_event(
            &state,
            crate::state::Event::StorageScope("other-host".into()),
        );
        assert!(foreign.feedback_notice(session).is_none());
    }
    #[test]
    fn feedback_from_a_retired_storage_scope_neither_sends_nor_applies() {
        let request = request();
        let mut other = initial();
        other.connection.storage_scope = "other-host".into();
        let (unchanged, effects) =
            crate::state::reduce_intent(&other, "", Intent::UploadFeedback(request.clone()));
        assert!(effects.is_empty());
        assert!(unchanged.sessions.feedback.is_empty());
        let mut current = request.clone();
        current.storage_scope = "other-host".into();
        current.prepare(&mut other, "").unwrap();
        request.stale(
            &mut other,
            "",
            agent_protocol::feedback::FeedbackReceipt {
                feedback_id: "old-host-report".into(),
            },
        );
        assert!(
            other
                .feedback_notice(current.thread_id)
                .unwrap()
                .feedback_id
                .is_none()
        );
    }
}
