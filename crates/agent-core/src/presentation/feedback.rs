use crate::{
    session::SessionRef,
    state::{
        Snapshot,
        operations::{OperationKey, OperationPhase},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FeedbackPhase {
    Uploading,
    Sent,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FeedbackNotice {
    pub id: String,
    pub storage_scope: String,
    pub session: SessionRef,
    pub phase: FeedbackPhase,
    pub feedback_id: Option<String>,
    pub error: Option<String>,
    pub can_dismiss: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn feedback_available(&self, session: SessionRef) -> bool {
        self.connection.connected
            && !self.operation_running(OperationKey::Feedback {
                session: session.clone(),
            })
            && self
                .sessions
                .conversations
                .get(&session)
                .is_some_and(|thread| {
                    thread
                        .capabilities
                        .is_some_and(|capabilities| capabilities.feedback)
                        && thread.preparation.is_none()
                        && thread.turns.as_ref().is_some_and(|turns| !turns.is_empty())
                })
    }
    pub fn feedback_notice(&self, session: SessionRef) -> Option<FeedbackNotice> {
        let submission = self.sessions.feedback.get(&session)?;
        let busy = self.operation_running(OperationKey::Feedback {
            session: session.clone(),
        });
        let (phase, error) = if submission.receipt.is_some() {
            (FeedbackPhase::Sent, None)
        } else {
            match self.operation_phase(OperationKey::Feedback {
                session: session.clone(),
            }) {
                Some(OperationPhase::Running) => (FeedbackPhase::Uploading, None),
                Some(OperationPhase::Failed { message }) => (FeedbackPhase::Failed, Some(message)),
                None => (FeedbackPhase::Unknown, None),
            }
        };
        Some(FeedbackNotice {
            id: submission.id.clone(),
            storage_scope: self.connection.storage_scope.clone(),
            session,
            phase,
            feedback_id: submission
                .receipt
                .as_ref()
                .map(|receipt| receipt.feedback_id.clone()),
            error,
            can_dismiss: !busy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn feedback_control_requires_a_loaded_supported_conversation_and_a_free_slot() {
        let session =
            SessionRef::new(crate::session::ProviderKind::Codex, "session".into()).unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        assert!(!snapshot.feedback_available(session.clone()));
        let mut thread = crate::models::Thread {
            id: Some(session.clone()),
            capabilities: Some(crate::session::Capabilities {
                feedback: true,
                ..Default::default()
            }),
            turns: Some(vec![Arc::new(crate::models::Turn {
                id: "turn".into(),
                ..Default::default()
            })]),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session.clone(), Arc::new(thread.clone()));
        assert!(snapshot.feedback_available(session.clone()));
        let ready = snapshot.clone();
        thread.turns = None;
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session.clone(), Arc::new(thread.clone()));
        assert!(!snapshot.feedback_available(session.clone()));
        thread.turns = Some(vec![Arc::new(crate::models::Turn {
            id: "turn".into(),
            ..Default::default()
        })]);
        thread.capabilities.as_mut().unwrap().feedback = false;
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session.clone(), Arc::new(thread));
        assert!(!snapshot.feedback_available(session.clone()));
        let mut offline = ready.clone();
        offline.connection.connected = false;
        assert!(!offline.feedback_available(session.clone()));
        let mut busy = ready.clone();
        Arc::make_mut(&mut busy.operations).insert(
            OperationKey::Feedback {
                session: session.clone(),
            },
            crate::state::operations::OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(!busy.feedback_available(session.clone()));
        let mut preparing = ready;
        Arc::make_mut(
            Arc::make_mut(&mut preparing.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        )
        .preparation = Some(agent_protocol::session::SessionPreparation {
            submission: agent_protocol::operations::Submission {
                fresh_context: false,
                thread_id: session.clone(),
                client_user_message_id: "input".into(),
                input: Vec::new(),
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            status: agent_protocol::session::PreparationStatus::Preparing,
            stage: agent_protocol::session::PreparationStage::Provider,
        });
        assert!(!preparing.feedback_available(session));
    }
}
