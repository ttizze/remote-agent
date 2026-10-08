//! Usage limits: what happens to threads a provider's usage limit stops.
use super::{
    ConversationSettingChange, SettingControl, SettingId, SettingValue, SettingsRow, SettingsScope,
    registry::{Context, Section},
    row, section, update,
};
use crate::models::ConversationSettings;

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::AutoResumeLimitedThreads,
        SettingId::SnoozeLimitedThreads,
    ],
    host: |context: &Context| {
        let host = context.host?;
        Some(section(
            "usage-limits",
            "Usage limits",
            vec![
                SettingsRow {
                    resettable: host.auto_resume_limited_threads,
                    ..row(
                        SettingId::AutoResumeLimitedThreads,
                        "Auto-resume limited threads",
                        Some(
                            "Resume usage-limit stops at the reported reset time. Each thread can cancel its scheduled continuation.",
                        ),
                        SettingControl::Switch {
                            on: host.auto_resume_limited_threads,
                        },
                    )
                },
                SettingsRow {
                    resettable: host.snooze_limited_threads,
                    ..row(
                        SettingId::SnoozeLimitedThreads,
                        "Snooze limited threads",
                        Some(
                            "Snooze usage-limit stops until the reported reset time. Combine with auto-resume to continue when they wake.",
                        ),
                        SettingControl::Switch {
                            on: host.snooze_limited_threads,
                        },
                    )
                },
            ],
            None,
        ))
    },
    project: |_| None,
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::AutoResumeLimitedThreads, SettingValue::Switch { on }) => Some(update(
            scope,
            ConversationSettingChange::AutoResumeLimitedThreads { on: *on },
        )),
        (SettingId::SnoozeLimitedThreads, SettingValue::Switch { on }) => Some(update(
            scope,
            ConversationSettingChange::SnoozeLimitedThreads { on: *on },
        )),
        _ => None,
    },
    reset: |id| {
        let defaults = ConversationSettings::default();
        let change = match id {
            SettingId::AutoResumeLimitedThreads => {
                ConversationSettingChange::AutoResumeLimitedThreads {
                    on: defaults.auto_resume_limited_threads,
                }
            }
            SettingId::SnoozeLimitedThreads => ConversationSettingChange::SnoozeLimitedThreads {
                on: defaults.snooze_limited_threads,
            },
            _ => return None,
        };
        Some(update(&SettingsScope::Host, change))
    },
    inherit: |_| None,
};
