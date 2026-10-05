//! Device-owned follow-up policy; the Host still validates the live native turn.
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FollowUpBehavior {
    #[default]
    Queue,
    Steer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ComposerAction {
    Send,
    Queue,
    Steer,
    Save,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn composer_action_label(action: ComposerAction) -> String {
    match action {
        ComposerAction::Send => "送信",
        ComposerAction::Queue => "キューに追加",
        ComposerAction::Steer => "Steer",
        ComposerAction::Save => "変更を保存",
    }
    .into()
}

pub(super) fn action(
    running: bool,
    steer_supported: bool,
    preference: FollowUpBehavior,
    alternate: bool,
    editing: bool,
) -> ComposerAction {
    if editing {
        ComposerAction::Save
    } else if !running {
        ComposerAction::Send
    } else if steer_supported && ((preference == FollowUpBehavior::Steer) != alternate) {
        ComposerAction::Steer
    } else {
        ComposerAction::Queue
    }
}

impl Snapshot {
    pub(super) fn submission_action(
        &self,
        session: Option<&crate::session::SessionRef>,
        alternate: bool,
        editing: bool,
    ) -> ComposerAction {
        let thread = session.and_then(|session| self.conversations.get(session));
        action(
            thread.is_some_and(|thread| thread.status == crate::models::SessionStatus::Running),
            thread.is_some_and(|thread| {
                agent_protocol::queue::steering_turn(
                    thread.status,
                    thread.active_turn_id().as_ref().map(|turn| turn.as_str()),
                    thread
                        .capabilities
                        .as_ref()
                        .is_some_and(|caps| caps.active_steering),
                )
                .is_some()
            }),
            self.follow_up_behavior,
            alternate,
            editing,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_offers_the_alternate_only_with_a_valid_active_steer_target() {
        let session = crate::session::SessionRef { id: "owned".into() };
        for (status, active, supported, expected) in [
            (
                crate::models::SessionStatus::Idle,
                Some("turn"),
                true,
                ComposerAction::Send,
            ),
            (
                crate::models::SessionStatus::Running,
                None,
                true,
                ComposerAction::Queue,
            ),
            (
                crate::models::SessionStatus::Running,
                Some(" "),
                true,
                ComposerAction::Queue,
            ),
            (
                crate::models::SessionStatus::Running,
                Some("turn"),
                false,
                ComposerAction::Queue,
            ),
            (
                crate::models::SessionStatus::Running,
                Some("turn"),
                true,
                ComposerAction::Steer,
            ),
        ] {
            let thread = Thread {
                id: Some(session.clone()),
                status,
                turns: active.map(|id| {
                    vec![Arc::new(crate::models::Turn {
                        id: id.into(),
                        status: crate::models::TurnStatus::Running,
                        ..Default::default()
                    })]
                }),
                capabilities: Some(crate::session::Capabilities {
                    active_steering: supported,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let snapshot = Snapshot {
                navigation: Arc::new(Navigation {
                    thread_id: Some(session.clone()),
                    draft_key: session.clone().into(),
                    ..Default::default()
                }),
                conversations: Arc::new([(session.clone(), Arc::new(thread))].into()),
                ..Default::default()
            };
            let controls = snapshot.composer_controls(false);
            assert_eq!(
                snapshot.submission_action(Some(&session), true, false),
                expected
            );
            assert_eq!(
                controls.alternate_action,
                (expected == ComposerAction::Steer).then_some(expected)
            );
        }
    }

    #[test]
    fn follow_ups_switch_action_only_for_a_supported_active_turn() {
        assert_eq!(FollowUpBehavior::default(), FollowUpBehavior::Queue);
        for preference in [FollowUpBehavior::Queue, FollowUpBehavior::Steer] {
            for alternate in [false, true] {
                assert_eq!(
                    action(false, true, preference, alternate, false),
                    ComposerAction::Send
                );
                assert_eq!(
                    action(true, false, preference, alternate, false),
                    ComposerAction::Queue
                );
                assert_eq!(
                    action(true, true, preference, alternate, true),
                    ComposerAction::Save
                );
            }
        }
        for (preference, primary, opposite) in [
            (
                FollowUpBehavior::Queue,
                ComposerAction::Queue,
                ComposerAction::Steer,
            ),
            (
                FollowUpBehavior::Steer,
                ComposerAction::Steer,
                ComposerAction::Queue,
            ),
        ] {
            assert_eq!(action(true, true, preference, false, false), primary);
            assert_eq!(action(true, true, preference, true, false), opposite);
        }
    }
}
