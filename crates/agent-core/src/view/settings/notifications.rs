//! Device-local notification preferences.
use super::registry::{Context, Section};
use super::{
    SettingControl, SettingId, SettingValue, SettingsRow, SettingsScope, choice, row, section,
};
use crate::{
    state::{Intent, Preferences},
    view::notifications::NotificationMode,
};

fn mode_choices() -> Vec<super::SettingChoice> {
    [
        (
            NotificationMode::Off,
            "Off",
            "Do not play sounds or create operating-system notices.",
        ),
        (
            NotificationMode::Notifications,
            "Notifications",
            "Show notices when the app is in the background.",
        ),
        (
            NotificationMode::Sound,
            "Sound",
            "Play a sound when attention or completion arrives.",
        ),
        (
            NotificationMode::NotificationsAndSound,
            "Notifications and sound",
            "Use both local notices and sound.",
        ),
    ]
    .into_iter()
    .map(|(mode, label, description)| choice(mode.id(), label, Some(description)))
    .collect()
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::NotificationMode,
        SettingId::InAppNotifications,
        SettingId::LiveActivities,
    ],
    host: |context: &Context| {
        let preferences = &context.snapshot.preferences;
        Some(section(
            "notifications",
            "Notifications",
            vec![
                SettingsRow {
                    resettable: preferences.notification_mode
                        != Preferences::default().notification_mode,
                    ..row(
                        SettingId::NotificationMode,
                        "Notification mode",
                        Some("Choose how this device presents thread attention and completion."),
                        SettingControl::Choice {
                            choices: mode_choices(),
                            selected: Some(preferences.notification_mode.id().into()),
                        },
                    )
                },
                SettingsRow {
                    resettable: preferences.in_app_notifications_enabled
                        != Preferences::default().in_app_notifications_enabled,
                    ..row(
                        SettingId::InAppNotifications,
                        "In-app notifications",
                        Some(
                            "Show a notice while the app is visible and the thread is not selected.",
                        ),
                        SettingControl::Switch {
                            on: preferences.in_app_notifications_enabled,
                        },
                    )
                },
                SettingsRow {
                    resettable: preferences.live_activities_enabled
                        != Preferences::default().live_activities_enabled,
                    ..row(
                        SettingId::LiveActivities,
                        "Live activity",
                        Some("Keep active agent work visible in the system activity surface."),
                        SettingControl::Switch {
                            on: preferences.live_activities_enabled,
                        },
                    )
                },
            ],
            Some("Operating-system delivery still follows the platform's notification permission."),
        ))
    },
    project: |_| None,
    intent: |_, scope, id, value| {
        if !matches!(scope, SettingsScope::Host) {
            return None;
        }
        match (id, value) {
            (SettingId::NotificationMode, SettingValue::Choice { id }) => mode_choices()
                .into_iter()
                .find(|choice| choice.id == *id)
                .and_then(|choice| {
                    [
                        NotificationMode::Off,
                        NotificationMode::Notifications,
                        NotificationMode::Sound,
                        NotificationMode::NotificationsAndSound,
                    ]
                    .into_iter()
                    .find(|mode| mode.id() == choice.id)
                })
                .map(|mode| Intent::SetNotificationMode { mode }),
            (SettingId::InAppNotifications, SettingValue::Switch { on }) => {
                Some(Intent::SetInAppNotificationsEnabled { enabled: *on })
            }
            (SettingId::LiveActivities, SettingValue::Switch { on }) => {
                Some(Intent::SetLiveActivitiesEnabled { enabled: *on })
            }
            _ => None,
        }
    },
    reset: |id| match id {
        SettingId::NotificationMode => Some(Intent::SetNotificationMode {
            mode: Preferences::default().notification_mode,
        }),
        SettingId::InAppNotifications => Some(Intent::SetInAppNotificationsEnabled {
            enabled: Preferences::default().in_app_notifications_enabled,
        }),
        SettingId::LiveActivities => Some(Intent::SetLiveActivitiesEnabled {
            enabled: Preferences::default().live_activities_enabled,
        }),
        _ => None,
    },
    inherit: |_| None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::Snapshot,
        view::{
            settings::{SettingValue, settings_view},
            time::TimestampFormat,
        },
    };

    #[test]
    fn notification_rows_follow_device_preferences() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.notification_mode = NotificationMode::Notifications;
        snapshot.preferences.in_app_notifications_enabled = false;
        snapshot.preferences.live_activities_enabled = false;
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let rows = &view
            .sections
            .iter()
            .find(|section| section.id == "notifications")
            .unwrap()
            .rows;
        assert_eq!(
            rows[0].control,
            SettingControl::Choice {
                choices: mode_choices(),
                selected: Some("notifications".into()),
            }
        );
        assert_eq!(rows[1].control, SettingControl::Switch { on: false });
        assert_eq!(rows[2].control, SettingControl::Switch { on: false });
        assert_eq!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::NotificationMode,
                &SettingValue::Choice { id: "sound".into() }
            ),
            Some(Intent::SetNotificationMode {
                mode: NotificationMode::Sound
            })
        );
        assert_eq!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::LiveActivities,
                &SettingValue::Switch { on: true }
            ),
            Some(Intent::SetLiveActivitiesEnabled { enabled: true })
        );
    }
}
