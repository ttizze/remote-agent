//! Native screen-reader announcements use committed conversation transitions.
use super::RenderedConversation;
use crate::localization::UiLanguage;
use agent_protocol::{
    ids::{RequestId, TurnId},
    session::SessionRef,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationAnnouncementPhase {
    Idle,
    Working,
    Approval,
    Input,
    Completed,
    Failed,
    Stopped,
}

impl ConversationAnnouncementPhase {
    fn active(self) -> bool {
        matches!(self, Self::Working | Self::Approval | Self::Input)
    }
    fn label(self, language: UiLanguage) -> String {
        language
            .text(match self {
                Self::Idle => "",
                Self::Working => "作業中",
                Self::Approval => "承認が必要です",
                Self::Input => "入力が必要です",
                Self::Completed => "完了",
                Self::Failed => "失敗",
                Self::Stopped => "停止しました",
            })
            .into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationAnnouncementState {
    pub session: SessionRef,
    pub turn: TurnId,
    pub phase: ConversationAnnouncementPhase,
    pub request: Option<RequestId>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedConversation {
    pub fn announcement_state(&self) -> Option<ConversationAnnouncementState> {
        use crate::models::TurnStatus;
        use agent_protocol::requests::RequestBody;
        let session = self.source.id.clone()?;
        let turn = self.turns.last()?;
        let request = turn.requests.iter().min_by_key(|request| &request.id);
        let phase = match (turn.source.status, request) {
            (TurnStatus::Running, Some(request)) => match request.body {
                RequestBody::Question { .. } | RequestBody::Elicitation { .. } => {
                    ConversationAnnouncementPhase::Input
                }
                _ => ConversationAnnouncementPhase::Approval,
            },
            (TurnStatus::Running, _) => ConversationAnnouncementPhase::Working,
            (TurnStatus::Completed, _) => ConversationAnnouncementPhase::Completed,
            (TurnStatus::Failed, _) => ConversationAnnouncementPhase::Failed,
            (TurnStatus::Interrupted, _) => ConversationAnnouncementPhase::Stopped,
            _ => ConversationAnnouncementPhase::Idle,
        };
        Some(ConversationAnnouncementState {
            session,
            turn: turn.source.id.clone(),
            phase,
            request: request.map(|request| request.id.clone()),
        })
    }
}

/// Opening history and switching sessions establish a silent baseline. Streamed text is never announced.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn conversation_announcement(
    previous: Option<ConversationAnnouncementState>,
    current: Option<ConversationAnnouncementState>,
    language: UiLanguage,
) -> Option<String> {
    use ConversationAnnouncementPhase::*;
    let (previous, current) = (previous?, current?);
    if previous.session != current.session {
        return None;
    }
    let changed = match current.phase {
        Approval | Input => {
            previous.phase != current.phase
                || previous.request != current.request
                || previous.turn != current.turn
        }
        Working => !previous.phase.active() || previous.turn != current.turn,
        Completed | Failed | Stopped => previous.phase.active() && previous.turn == current.turn,
        Idle => false,
    };
    changed.then(|| current.phase.label(language))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConversationAnnouncementPhase::*;
    fn state(turn: &str, phase: ConversationAnnouncementPhase) -> ConversationAnnouncementState {
        ConversationAnnouncementState {
            session: SessionRef::new(crate::session::ProviderKind::Codex, "thread".into()).unwrap(),
            turn: turn.into(),
            phase,
            request: None,
        }
    }
    #[test]
    fn committed_projection_uses_the_latest_turn_and_its_own_pending_request() {
        use crate::{models, state::Snapshot};
        use agent_protocol::{
            requests::{ElicitationInput, Request, RequestBody, RequestTarget},
            session::RequestDelivery,
        };
        use std::sync::Arc;
        let mut thread: models::Thread = serde_json::from_value(serde_json::json!({
            "id":{"provider":"codex","id":"thread"},"turns":[
                {"id":"old","status":"completed","items":[]},
                {"id":"run","status":"running","items":[]}
            ]
        }))
        .unwrap();
        let request = |id: &str, turn: &str, body| {
            Arc::new(Request {
                id: id.into(),
                target: RequestTarget::Turn {
                    turn_id: turn.into(),
                    item_id: None,
                },
                delivery: RequestDelivery::Awaiting,
                body,
            })
        };
        let old = request(
            "old",
            "old",
            RequestBody::Question {
                questions: Vec::new(),
            },
        );
        thread.requests.insert(old.id.clone(), old);
        let project = |thread| {
            let snapshot = Snapshot::default();
            super::super::project_conversation(
                UiLanguage::English,
                &snapshot.panes.primary().navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                Arc::new(thread),
                &None,
            )
            .announcement_state()
            .unwrap()
        };
        assert_eq!(project(thread.clone()).phase, Working);
        let approval = request(
            "z",
            "run",
            RequestBody::Permission {
                description: "allow".into(),
                details: String::new(),
                choices: Vec::new(),
            },
        );
        thread.requests.insert(approval.id.clone(), approval);
        assert_eq!(project(thread.clone()).phase, Approval);
        let input = request(
            "a",
            "run",
            RequestBody::Elicitation {
                server: "fixture".into(),
                message: "input".into(),
                input: ElicitationInput::Url {
                    url: "https://example.com".into(),
                },
            },
        );
        thread.requests.insert(input.id.clone(), input);
        let current = project(thread);
        assert_eq!(current.phase, Input);
        assert_eq!(current.request, Some("a".into()));
        assert_eq!(current.turn, "run".into());
    }

    #[test]
    fn opening_switching_and_repeated_publications_are_silent() {
        let current = state("old", Completed);
        assert_eq!(
            conversation_announcement(None, Some(current.clone()), UiLanguage::English),
            None
        );
        assert_eq!(
            conversation_announcement(Some(current.clone()), Some(current), UiLanguage::English),
            None
        );
        let before = state("run", Working);
        let mut after = state("run", Failed);
        after.session =
            SessionRef::new(crate::session::ProviderKind::Codex, "other".into()).unwrap();
        assert_eq!(
            conversation_announcement(Some(before), Some(after), UiLanguage::English),
            None
        );
    }
    #[test]
    fn requests_and_turn_endings_are_announced_once_without_replaying_history() {
        let say = |previous, current| {
            conversation_announcement(Some(previous), Some(current), UiLanguage::English)
        };
        let working = state("run", Working);
        let mut approval = state("run", Approval);
        approval.request = Some("one".into());
        assert_eq!(
            say(working.clone(), approval.clone()),
            Some("Needs approval".into())
        );
        assert_eq!(say(approval.clone(), approval.clone()), None);
        let mut second = approval.clone();
        second.request = Some("two".into());
        assert_eq!(say(approval.clone(), second), Some("Needs approval".into()));
        assert_eq!(say(approval, working.clone()), None);
        for (phase, label) in [
            (Completed, "Done"),
            (Failed, "Failed"),
            (Stopped, "Stopped"),
        ] {
            let ending = state("run", phase);
            assert_eq!(say(working.clone(), ending.clone()), Some(label.into()));
            assert_eq!(say(ending.clone(), ending), None);
            assert_eq!(say(working.clone(), state("unseen", phase)), None);
        }
        assert_eq!(
            say(working.clone(), state("queued-next", Working)),
            Some("Working".into())
        );
        assert_eq!(say(state("run", Input), working), None);
    }
}
