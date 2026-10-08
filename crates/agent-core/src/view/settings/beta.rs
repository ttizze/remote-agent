//! Beta: features a device opts into.
use super::{
    SettingControl, SettingId, SettingValue, SettingsRow,
    registry::{Context, Section},
    row, section,
};
use crate::state::{Intent, Preferences};

pub(super) const SECTION: Section = Section {
    ids: &[SettingId::WorkingSection],
    host: |context: &Context| {
        let on = context.snapshot.preferences.working_section;
        Some(section(
            "beta",
            "Beta",
            vec![SettingsRow {
                resettable: on,
                ..row(
                    SettingId::WorkingSection,
                    "Working section",
                    Some(
                        "Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you.",
                    ),
                    SettingControl::Switch { on },
                )
            }],
            None,
        ))
    },
    project: |_| None,
    intent: |_, _, id, value| match (id, value) {
        (SettingId::WorkingSection, SettingValue::Switch { on }) => {
            Some(Intent::SetWorkingSection { enabled: *on })
        }
        _ => None,
    },
    reset: |id| match id {
        SettingId::WorkingSection => Some(Intent::SetWorkingSection {
            enabled: Preferences::default().working_section,
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
            settings::{SettingsScope, fixtures::find, settings_view},
            time::TimestampFormat,
        },
    };

    #[test]
    fn the_working_section_row_follows_the_device_preference() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.working_section = true;
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let row = find(&view, SettingId::WorkingSection);
        assert_eq!(row.control, SettingControl::Switch { on: true });
        assert!(row.resettable);
    }
}
