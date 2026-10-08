//! Background work and response delivery settings.
use super::{
    ProjectSettingKey, SettingChange, SettingChoice, SettingControl, SettingId, SettingSource,
    SettingValue, SettingsRow, SettingsScope, choice,
    registry::{Context, Section},
    row, section, update,
};
use crate::models::HostSettings;
use agent_protocol::models::{BackgroundActivityProfileSelection, ResponseStreamingMode};

fn streaming_id(mode: ResponseStreamingMode) -> &'static str {
    match mode {
        ResponseStreamingMode::Paragraph => "paragraph",
        ResponseStreamingMode::Turn => "turn",
    }
}

fn streaming_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "paragraph",
            "Paragraphs",
            Some("Show each finished paragraph or closed code block."),
        ),
        choice(
            "turn",
            "Whole turn",
            Some("Show the complete assistant message when the turn finishes."),
        ),
    ]
}

fn profile_id(profile: BackgroundActivityProfileSelection) -> &'static str {
    match profile {
        BackgroundActivityProfileSelection::Balanced => "balanced",
        BackgroundActivityProfileSelection::Performance => "performance",
        BackgroundActivityProfileSelection::BatterySaver => "battery-saver",
        BackgroundActivityProfileSelection::Custom => "custom",
    }
}

fn profile_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "balanced",
            "Balanced",
            Some("Keep information fresh with moderate background work."),
        ),
        choice(
            "performance",
            "Performance",
            Some("Fetch and refresh more often."),
        ),
        choice(
            "battery-saver",
            "Battery saver",
            Some("Reduce background work when the app is idle."),
        ),
        choice(
            "custom",
            "Custom",
            Some("Use the Host's configured intervals."),
        ),
    ]
}

fn streaming_row(mode: ResponseStreamingMode, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: mode != HostSettings::default().response_streaming_mode,
        source,
        ..row(
            SettingId::ResponseStreaming,
            "Response streaming",
            Some("Choose when completed assistant text is delivered."),
            SettingControl::Choice {
                choices: streaming_choices(),
                selected: Some(streaming_id(mode).into()),
            },
        )
    }
}

fn merge_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: on != HostSettings::default().auto_settle_on_merge,
        source,
        ..row(
            SettingId::AutoSettleOnMerge,
            "Settle after merge",
            Some("Automatically settle a thread when its pull request is merged."),
            SettingControl::Switch { on },
        )
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::ResponseStreaming,
        SettingId::AutoSettleOnMerge,
        SettingId::BackgroundActivity,
    ],
    host: |context: &Context| {
        let host = context.host?;
        Some(section(
            "maintenance",
            "Maintenance",
            vec![
                streaming_row(host.response_streaming_mode, None),
                merge_row(host.auto_settle_on_merge, None),
                SettingsRow {
                    resettable: host.background_activity
                        != HostSettings::default().background_activity,
                    ..row(
                        SettingId::BackgroundActivity,
                        "Background activity",
                        Some("Control how often Git remotes and provider health are refreshed."),
                        SettingControl::Choice {
                            choices: profile_choices(),
                            selected: Some(profile_id(host.background_activity.profile).into()),
                        },
                    )
                },
            ],
            Some("Intervals use fixed presets and do not inspect device power state."),
        ))
    },
    project: |resolved| {
        Some(section(
            "maintenance",
            "Maintenance",
            vec![
                streaming_row(
                    resolved.response_streaming_mode.0,
                    Some(resolved.response_streaming_mode.1),
                ),
                merge_row(
                    resolved.auto_settle_on_merge.0,
                    Some(resolved.auto_settle_on_merge.1),
                ),
            ],
            None,
        ))
    },
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::ResponseStreaming, SettingValue::Choice { id }) => [
            ResponseStreamingMode::Paragraph,
            ResponseStreamingMode::Turn,
        ]
        .into_iter()
        .find(|mode| streaming_id(*mode) == id)
        .map(|mode| update(scope, SettingChange::ResponseStreamingMode { mode })),
        (SettingId::AutoSettleOnMerge, SettingValue::Switch { on }) => {
            Some(update(scope, SettingChange::AutoSettleOnMerge { on: *on }))
        }
        (SettingId::BackgroundActivity, SettingValue::Choice { id }) => [
            BackgroundActivityProfileSelection::Balanced,
            BackgroundActivityProfileSelection::Performance,
            BackgroundActivityProfileSelection::BatterySaver,
            BackgroundActivityProfileSelection::Custom,
        ]
        .into_iter()
        .find(|profile| profile_id(*profile) == id)
        .map(|profile| update(scope, SettingChange::BackgroundActivityProfile { profile })),
        _ => None,
    },
    reset: |id| match id {
        SettingId::ResponseStreaming => Some(update(
            &SettingsScope::Host,
            SettingChange::ResponseStreamingMode {
                mode: HostSettings::default().response_streaming_mode,
            },
        )),
        SettingId::AutoSettleOnMerge => Some(update(
            &SettingsScope::Host,
            SettingChange::AutoSettleOnMerge {
                on: HostSettings::default().auto_settle_on_merge,
            },
        )),
        SettingId::BackgroundActivity => Some(update(
            &SettingsScope::Host,
            SettingChange::BackgroundActivityProfile {
                profile: HostSettings::default().background_activity.profile,
            },
        )),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::ResponseStreaming => Some(ProjectSettingKey::ResponseStreamingMode),
        SettingId::AutoSettleOnMerge => Some(ProjectSettingKey::AutoSettleOnMerge),
        _ => None,
    },
};
