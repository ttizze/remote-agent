//! Shared screenshot preferences and shortcut decisions.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotShortcut {
    #[default]
    BothShiftKeys,
    CommandShiftFour,
    ControlShiftS,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotSound {
    #[default]
    SoftPop,
    CameraShutter,
}

/// Device-local screenshot behavior. The Host never needs these preferences:
/// capture happens on the client and the resulting file enters the ordinary
/// attachment pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SnapshotPreferences {
    pub enabled: bool,
    pub include_accessibility: bool,
    pub shortcut: SnapshotShortcut,
    pub play_sound: bool,
    pub sound: SnapshotSound,
    pub flash: bool,
    pub animations: bool,
}
impl Default for SnapshotPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            include_accessibility: true,
            shortcut: SnapshotShortcut::default(),
            play_sound: true,
            sound: SnapshotSound::default(),
            flash: true,
            animations: true,
        }
    }
}

/// The platform bridge calls this before opening a capture overlay.
pub fn capture_is_allowed(settings: &SnapshotPreferences, permission_granted: bool) -> bool {
    settings.enabled && permission_granted
}

/// Matches the portable shortcut forms supported by native clients. The
/// bridge supplies the physical modifier state instead of re-deriving it.
pub fn shortcut_matches(
    shortcut: SnapshotShortcut,
    key: &str,
    shift: bool,
    command: bool,
    control: bool,
    both_shift_keys: bool,
) -> bool {
    match shortcut {
        SnapshotShortcut::BothShiftKeys => both_shift_keys && shift,
        SnapshotShortcut::CommandShiftFour => key.eq_ignore_ascii_case("4") && shift && command,
        SnapshotShortcut::ControlShiftS => key.eq_ignore_ascii_case("s") && shift && control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_or_unapproved_capture_never_reaches_the_native_bridge() {
        let defaults = SnapshotPreferences::default();
        assert!(!capture_is_allowed(&defaults, true));
        let mut enabled = defaults;
        enabled.enabled = true;
        assert!(!capture_is_allowed(&enabled, false));
        assert!(capture_is_allowed(&enabled, true));
    }

    #[test]
    fn shortcuts_require_their_modifier_chord() {
        assert!(shortcut_matches(
            SnapshotShortcut::CommandShiftFour,
            "4",
            true,
            true,
            false,
            false
        ));
        assert!(!shortcut_matches(
            SnapshotShortcut::CommandShiftFour,
            "4",
            true,
            false,
            false,
            false
        ));
        assert!(shortcut_matches(
            SnapshotShortcut::BothShiftKeys,
            "Shift",
            true,
            false,
            false,
            true
        ));
    }
}
