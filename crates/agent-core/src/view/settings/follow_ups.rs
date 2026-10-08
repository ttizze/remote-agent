//! While the agent is running: what a follow-up message does.
use super::{
    SettingControl, SettingId, SettingValue, SettingsRow, choice,
    registry::{Context, Section},
    row, section,
};
use crate::{commands::build::FollowUpBehavior, state::Intent};

pub(super) const SECTION: Section = Section {
    ids: &[SettingId::FollowUpBehavior],
    host: |context: &Context| {
        let follow_up = match context.snapshot.follow_up {
            FollowUpBehavior::Queue => Some("queue"),
            FollowUpBehavior::Steer => Some("steer"),
            FollowUpBehavior::Restart => None,
        };
        Some(section(
            "follow-ups",
            "While the agent is running",
            vec![SettingsRow {
                resettable: context.snapshot.follow_up != FollowUpBehavior::default(),
                ..row(
                    SettingId::FollowUpBehavior,
                    "Follow-up behavior",
                    Some("Queue follow-ups while the agent runs or steer the current run."),
                    SettingControl::Choice {
                        choices: vec![
                            choice(
                                "queue",
                                "Queue",
                                Some(
                                    "Your message waits and runs after the current turn finishes.",
                                ),
                            ),
                            choice(
                                "steer",
                                "Steer",
                                Some(
                                    "Your message reaches the agent right away, changing what it is working on.",
                                ),
                            ),
                        ],
                        selected: follow_up.map(Into::into),
                    },
                )
            }],
            Some(
                "Long-press the send button to use the other option for a single message. With a hardware keyboard, hold Command while sending.",
            ),
        ))
    },
    project: |_| None,
    intent: |_, _, id, value| match (id, value) {
        (SettingId::FollowUpBehavior, SettingValue::Choice { id }) => {
            let behavior = match id.as_str() {
                "queue" => FollowUpBehavior::Queue,
                "steer" => FollowUpBehavior::Steer,
                _ => return None,
            };
            Some(Intent::SetFollowUpBehavior { behavior })
        }
        _ => None,
    },
    reset: |id| match id {
        SettingId::FollowUpBehavior => Some(Intent::SetFollowUpBehavior {
            behavior: FollowUpBehavior::default(),
        }),
        _ => None,
    },
    inherit: |_| None,
};
