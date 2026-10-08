//! Device-local screenshot capture and provider routing preferences.
use super::registry::{Context, Section};
use super::{
    SettingControl, SettingId, SettingValue, SettingsRow, SettingsScope, choice, row, section,
};
use crate::{
    state::{Intent, Preferences},
    view::snapshot_capture::{SnapshotShortcut, SnapshotSound},
};

fn shortcut_choices() -> Vec<super::SettingChoice> {
    [
        (
            SnapshotShortcut::BothShiftKeys,
            "Both Shift keys",
            "Press both Shift keys together.",
        ),
        (
            SnapshotShortcut::CommandShiftFour,
            "Command Shift 4",
            "Use the macOS capture chord.",
        ),
        (
            SnapshotShortcut::ControlShiftS,
            "Control Shift S",
            "Use the portable capture chord.",
        ),
    ]
    .into_iter()
    .map(|(shortcut, label, description)| choice(shortcut_id(shortcut), label, Some(description)))
    .collect()
}

fn shortcut_id(shortcut: SnapshotShortcut) -> &'static str {
    match shortcut {
        SnapshotShortcut::BothShiftKeys => "both-shift-keys",
        SnapshotShortcut::CommandShiftFour => "command-shift-4",
        SnapshotShortcut::ControlShiftS => "control-shift-s",
    }
}

fn sound_choices() -> Vec<super::SettingChoice> {
    [
        (SnapshotSound::SoftPop, "Soft pop"),
        (SnapshotSound::CameraShutter, "Camera shutter"),
    ]
    .into_iter()
    .map(|(sound, label)| choice(sound_id(sound), label, None))
    .collect()
}

fn sound_id(sound: SnapshotSound) -> &'static str {
    match sound {
        SnapshotSound::SoftPop => "soft-pop",
        SnapshotSound::CameraShutter => "camera-shutter",
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::LoadBalancing,
        SettingId::SnapshotCapture,
        SettingId::SnapshotIncludeAccessibility,
        SettingId::SnapshotShortcut,
        SettingId::SnapshotPlaySound,
        SettingId::SnapshotSound,
        SettingId::SnapshotFlash,
        SettingId::SnapshotAnimations,
    ],
    host: |context: &Context| {
        let preferences = &context.snapshot.preferences;
        let snapshot = &preferences.snapshot_capture;
        Some(section(
            "capture",
            "Capture and routing",
            vec![
                SettingsRow {
                    resettable: preferences.load_balancing_enabled,
                    ..row(
                        SettingId::LoadBalancing,
                        "Load balancing",
                        Some(
                            "Route new threads across ready provider instances using their saved weights.",
                        ),
                        SettingControl::Switch {
                            on: preferences.load_balancing_enabled,
                        },
                    )
                },
                SettingsRow {
                    resettable: snapshot.enabled,
                    ..row(
                        SettingId::SnapshotCapture,
                        "Screenshot capture",
                        Some(
                            "Capture the desktop and attach it to the current draft from the configured shortcut.",
                        ),
                        SettingControl::Switch {
                            on: snapshot.enabled,
                        },
                    )
                },
                SettingsRow {
                    resettable: !snapshot.include_accessibility,
                    ..row(
                        SettingId::SnapshotIncludeAccessibility,
                        "Include accessibility details",
                        Some(
                            "Ask the platform bridge for accessibility text when it is available.",
                        ),
                        SettingControl::Switch {
                            on: snapshot.include_accessibility,
                        },
                    )
                },
                SettingsRow {
                    resettable: snapshot.shortcut
                        != Preferences::default().snapshot_capture.shortcut,
                    ..row(
                        SettingId::SnapshotShortcut,
                        "Capture shortcut",
                        None,
                        SettingControl::Choice {
                            choices: shortcut_choices(),
                            selected: Some(shortcut_id(snapshot.shortcut).into()),
                        },
                    )
                },
                SettingsRow {
                    resettable: !snapshot.play_sound,
                    ..row(
                        SettingId::SnapshotPlaySound,
                        "Capture sound",
                        Some("Play the selected sound after a capture is ready."),
                        SettingControl::Switch {
                            on: snapshot.play_sound,
                        },
                    )
                },
                SettingsRow {
                    resettable: snapshot.sound != Preferences::default().snapshot_capture.sound,
                    ..row(
                        SettingId::SnapshotSound,
                        "Capture sound type",
                        None,
                        SettingControl::Choice {
                            choices: sound_choices(),
                            selected: Some(sound_id(snapshot.sound).into()),
                        },
                    )
                },
                SettingsRow {
                    resettable: !snapshot.flash,
                    ..row(
                        SettingId::SnapshotFlash,
                        "Capture flash",
                        Some(
                            "Flash the capture overlay when the native bridge confirms the image.",
                        ),
                        SettingControl::Switch { on: snapshot.flash },
                    )
                },
                SettingsRow {
                    resettable: !snapshot.animations,
                    ..row(
                        SettingId::SnapshotAnimations,
                        "Capture animations",
                        Some("Animate the capture overlay and completion state."),
                        SettingControl::Switch {
                            on: snapshot.animations,
                        },
                    )
                },
            ],
            Some(
                "Provider weights are kept per instance on this device; a weight of zero disables routing to that instance.",
            ),
        ))
    },
    project: |_| None,
    intent: |_, scope, id, value| {
        if !matches!(scope, SettingsScope::Host) {
            return None;
        }
        match (id, value) {
            (SettingId::LoadBalancing, SettingValue::Switch { on }) => {
                Some(Intent::SetLoadBalancingEnabled { enabled: *on })
            }
            (SettingId::SnapshotCapture, SettingValue::Switch { on }) => {
                Some(Intent::SetSnapshotCaptureEnabled { enabled: *on })
            }
            (SettingId::SnapshotIncludeAccessibility, SettingValue::Switch { on }) => {
                Some(Intent::SetSnapshotIncludeAccessibility { enabled: *on })
            }
            (SettingId::SnapshotPlaySound, SettingValue::Switch { on }) => {
                Some(Intent::SetSnapshotPlaySound { enabled: *on })
            }
            (SettingId::SnapshotFlash, SettingValue::Switch { on }) => {
                Some(Intent::SetSnapshotFlash { enabled: *on })
            }
            (SettingId::SnapshotAnimations, SettingValue::Switch { on }) => {
                Some(Intent::SetSnapshotAnimations { enabled: *on })
            }
            (SettingId::SnapshotShortcut, SettingValue::Choice { id }) => shortcut_choices()
                .into_iter()
                .find(|choice| choice.id == *id)
                .and_then(|choice| match choice.id.as_str() {
                    "both-shift-keys" => Some(SnapshotShortcut::BothShiftKeys),
                    "command-shift-4" => Some(SnapshotShortcut::CommandShiftFour),
                    "control-shift-s" => Some(SnapshotShortcut::ControlShiftS),
                    _ => None,
                })
                .map(|shortcut| Intent::SetSnapshotShortcut { shortcut }),
            (SettingId::SnapshotSound, SettingValue::Choice { id }) => sound_choices()
                .into_iter()
                .find(|choice| choice.id == *id)
                .and_then(|choice| match choice.id.as_str() {
                    "soft-pop" => Some(SnapshotSound::SoftPop),
                    "camera-shutter" => Some(SnapshotSound::CameraShutter),
                    _ => None,
                })
                .map(|sound| Intent::SetSnapshotSound { sound }),
            _ => None,
        }
    },
    reset: |id| match id {
        SettingId::LoadBalancing => Some(Intent::SetLoadBalancingEnabled {
            enabled: Preferences::default().load_balancing_enabled,
        }),
        SettingId::SnapshotCapture => Some(Intent::SetSnapshotCaptureEnabled {
            enabled: Preferences::default().snapshot_capture.enabled,
        }),
        SettingId::SnapshotIncludeAccessibility => Some(Intent::SetSnapshotIncludeAccessibility {
            enabled: Preferences::default()
                .snapshot_capture
                .include_accessibility,
        }),
        SettingId::SnapshotShortcut => Some(Intent::SetSnapshotShortcut {
            shortcut: Preferences::default().snapshot_capture.shortcut,
        }),
        SettingId::SnapshotPlaySound => Some(Intent::SetSnapshotPlaySound {
            enabled: Preferences::default().snapshot_capture.play_sound,
        }),
        SettingId::SnapshotSound => Some(Intent::SetSnapshotSound {
            sound: Preferences::default().snapshot_capture.sound,
        }),
        SettingId::SnapshotFlash => Some(Intent::SetSnapshotFlash {
            enabled: Preferences::default().snapshot_capture.flash,
        }),
        SettingId::SnapshotAnimations => Some(Intent::SetSnapshotAnimations {
            enabled: Preferences::default().snapshot_capture.animations,
        }),
        _ => None,
    },
    inherit: |_| None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Snapshot;
    use crate::view::settings::{SettingValue, SettingsScope, settings_view};
    use crate::view::time::TimestampFormat;

    #[test]
    fn capture_rows_follow_saved_device_preferences() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.snapshot_capture.enabled = true;
        snapshot.preferences.load_balancing_enabled = true;
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let section = view
            .sections
            .iter()
            .find(|section| section.id == "capture")
            .unwrap();
        assert_eq!(section.rows[0].control, SettingControl::Switch { on: true });
        assert_eq!(section.rows[1].control, SettingControl::Switch { on: true });
        assert_eq!(
            (SECTION.intent)(
                &snapshot,
                &SettingsScope::Host,
                SettingId::SnapshotSound,
                &SettingValue::Choice {
                    id: "camera-shutter".into()
                },
            ),
            Some(Intent::SetSnapshotSound {
                sound: SnapshotSound::CameraShutter
            })
        );
    }
}
