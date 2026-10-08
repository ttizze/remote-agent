//! Auto-settle: sidebar threads with no activity settle on their own.
use super::{
    ConversationSettingChange, ProjectSettingKey, SettingControl, SettingId, SettingSource,
    SettingValue, SettingsRow, SettingsScope, SettingsSection,
    registry::{Context, Section},
    row, section, update,
};
use crate::models::{AutoSettle, ConversationSettings};

pub const MIN_AUTO_SETTLE_DAYS: u32 = 1;
pub const MAX_AUTO_SETTLE_DAYS: u32 = 90;
/// The days an inactive thread waits when auto-settle is turned on.
pub const AUTO_SETTLE_DEFAULT_DAYS: u32 = 3;

/// The days the auto-settle field commits: an integer from 1 to 90.
pub fn parse_auto_settle_days(text: &str) -> Option<u32> {
    let value: f64 = text.trim().parse().ok()?;
    (value.fract() == 0.0
        && (f64::from(MIN_AUTO_SETTLE_DAYS)..=f64::from(MAX_AUTO_SETTLE_DAYS)).contains(&value))
    .then_some(value as u32)
}

fn auto_settle_days(value: AutoSettle) -> Option<u32> {
    match value {
        AutoSettle::AfterDays(days) => Some(days),
        AutoSettle::Never => None,
    }
}

fn auto_settle_section(value: AutoSettle, source: Option<SettingSource>) -> SettingsSection {
    let days = auto_settle_days(value);
    let mut rows = vec![SettingsRow {
        resettable: value != ConversationSettings::default().auto_settle,
        source,
        ..row(
            SettingId::AutoSettleInactiveThreads,
            "Auto-settle inactive threads",
            Some("Sidebar threads with no activity for this long settle automatically."),
            SettingControl::Switch { on: days.is_some() },
        )
    }];
    if let Some(days) = days {
        rows.push(SettingsRow {
            source,
            ..row(
                SettingId::AutoSettleDays,
                "Days of inactivity before auto-settle",
                Some("Any new activity un-settles a thread automatically."),
                SettingControl::Number {
                    value: days,
                    min: MIN_AUTO_SETTLE_DAYS,
                    max: MAX_AUTO_SETTLE_DAYS,
                },
            )
        });
    }
    section("auto-settle", "Auto-settle", rows, None)
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::AutoSettleInactiveThreads,
        SettingId::AutoSettleDays,
    ],
    host: |context: &Context| {
        context
            .host
            .map(|host| auto_settle_section(host.auto_settle, None))
    },
    project: |resolved| {
        Some(auto_settle_section(
            resolved.auto_settle.0,
            Some(resolved.auto_settle.1),
        ))
    },
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::AutoSettleInactiveThreads, SettingValue::Switch { on }) => Some(update(
            scope,
            ConversationSettingChange::AutoSettle {
                days: on.then_some(AUTO_SETTLE_DEFAULT_DAYS),
            },
        )),
        (SettingId::AutoSettleDays, SettingValue::Number { value })
            if (MIN_AUTO_SETTLE_DAYS..=MAX_AUTO_SETTLE_DAYS).contains(value) =>
        {
            Some(update(
                scope,
                ConversationSettingChange::AutoSettle { days: Some(*value) },
            ))
        }
        _ => None,
    },
    reset: |id| match id {
        SettingId::AutoSettleInactiveThreads | SettingId::AutoSettleDays => Some(update(
            &SettingsScope::Host,
            ConversationSettingChange::AutoSettle {
                days: auto_settle_days(ConversationSettings::default().auto_settle),
            },
        )),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::AutoSettleInactiveThreads | SettingId::AutoSettleDays => {
            Some(ProjectSettingKey::AutoSettle)
        }
        _ => None,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_days_field_commits_only_whole_days_in_range() {
        assert_eq!(parse_auto_settle_days("7"), Some(7));
        assert_eq!(parse_auto_settle_days(" 90 "), Some(90));
        assert_eq!(parse_auto_settle_days("3.5"), None);
        assert_eq!(parse_auto_settle_days("0"), None);
        assert_eq!(parse_auto_settle_days("91"), None);
        assert_eq!(parse_auto_settle_days(""), None);
    }
}
