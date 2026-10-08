//! Behavior: the clock format and restart continuation.
use super::{
    ProjectSettingKey, SettingChange, SettingControl, SettingId, SettingSource, SettingValue,
    SettingsRow, SettingsScope, choice,
    registry::{Context, Section},
    resettable_for, row, section, update,
};
use crate::{models::HostSettings, state::Intent, view::time::TimestampFormat};

pub fn timestamp_format_id(format: TimestampFormat) -> &'static str {
    match format {
        TimestampFormat::Locale => "locale",
        TimestampFormat::TwelveHour => "12-hour",
        TimestampFormat::TwentyFourHour => "24-hour",
    }
}

fn continue_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: resettable_for(
            source,
            on != HostSettings::default().continue_after_restart,
        ),
        source,
        ..row(
            SettingId::ContinueAfterRestart,
            "Continue threads after restarts",
            Some(
                "Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.",
            ),
            SettingControl::Switch { on },
        )
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[SettingId::TimeFormat, SettingId::ContinueAfterRestart],
    host: |context: &Context| {
        let mut rows = vec![SettingsRow {
            resettable: context.timestamp_format != TimestampFormat::default(),
            ..row(
                SettingId::TimeFormat,
                "Time format",
                Some("System default follows your browser or OS clock preference."),
                SettingControl::Choice {
                    choices: vec![
                        choice("locale", "System default", None),
                        choice("12-hour", "12-hour", None),
                        choice("24-hour", "24-hour", None),
                    ],
                    selected: Some(timestamp_format_id(context.timestamp_format).into()),
                },
            )
        }];
        if let Some(host) = context.host {
            rows.push(continue_row(host.continue_after_restart, None));
        }
        Some(section("behavior", "Behavior", rows, None))
    },
    project: |resolved| {
        Some(section(
            "behavior",
            "Behavior",
            vec![continue_row(
                resolved.continue_after_restart.0,
                Some(resolved.continue_after_restart.1),
            )],
            None,
        ))
    },
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::ContinueAfterRestart, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::ContinueAfterRestart { on: *on },
        )),
        (SettingId::TimeFormat, SettingValue::Choice { id }) => [
            TimestampFormat::Locale,
            TimestampFormat::TwelveHour,
            TimestampFormat::TwentyFourHour,
        ]
        .into_iter()
        .find(|format| timestamp_format_id(*format) == id)
        .map(|format| Intent::SetTimestampFormat { format }),
        _ => None,
    },
    reset: |id| match id {
        SettingId::ContinueAfterRestart => Some(update(
            &SettingsScope::Host,
            SettingChange::ContinueAfterRestart {
                on: HostSettings::default().continue_after_restart,
            },
        )),
        SettingId::TimeFormat => Some(Intent::SetTimestampFormat {
            format: TimestampFormat::default(),
        }),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::ContinueAfterRestart => Some(ProjectSettingKey::ContinueAfterRestart),
        _ => None,
    },
};
