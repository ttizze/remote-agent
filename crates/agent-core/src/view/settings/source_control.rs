//! Source-control defaults shared by the Host and project settings pages.
use super::{
    ProjectSettingKey, SettingChange, SettingChoice, SettingControl, SettingId, SettingSource,
    SettingValue, SettingsRow, SettingsScope, choice,
    registry::{Context, Section},
    resettable_for, row, section, update,
};
use crate::models::HostSettings;
use agent_domain::BranchNamingMode;
use agent_protocol::models::{PullRequestMergeMethod, SourceControlWritingStyleMode};

fn branch_id(mode: BranchNamingMode) -> &'static str {
    match mode {
        BranchNamingMode::Static => "static",
        BranchNamingMode::Semantic => "semantic",
        BranchNamingMode::Custom => "custom",
    }
}
fn branch_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "static",
            "Prefix",
            Some("Use the configured branch prefix."),
        ),
        choice(
            "semantic",
            "Semantic",
            Some("Ask the text generator for a descriptive branch name."),
        ),
        choice(
            "custom",
            "Custom",
            Some("Use the configured naming instructions."),
        ),
    ]
}
fn style_id(mode: SourceControlWritingStyleMode) -> &'static str {
    match mode {
        SourceControlWritingStyleMode::RepoConventions => "repo-conventions",
        SourceControlWritingStyleMode::ConventionalCommits => "conventional-commits",
        SourceControlWritingStyleMode::Custom => "custom",
    }
}
fn style_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "repo-conventions",
            "Repository conventions",
            Some("Follow the repository's recent history."),
        ),
        choice("conventional-commits", "Conventional commits", None),
        choice("custom", "Custom instructions", None),
    ]
}
fn merge_id(method: Option<PullRequestMergeMethod>) -> &'static str {
    match method {
        None => "last-used",
        Some(PullRequestMergeMethod::Merge) => "merge",
        Some(PullRequestMergeMethod::Squash) => "squash",
        Some(PullRequestMergeMethod::Rebase) => "rebase",
    }
}
fn merge_choices() -> Vec<SettingChoice> {
    vec![
        choice(
            "last-used",
            "Last used",
            Some("Keep the device's last selected merge method."),
        ),
        choice("merge", "Merge", None),
        choice("squash", "Squash", None),
        choice("rebase", "Rebase", None),
    ]
}
fn merge_row(method: Option<PullRequestMergeMethod>, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: resettable_for(
            source,
            method != HostSettings::default().pull_request_merge_method,
        ),
        source,
        ..row(
            SettingId::PullRequestMergeMethod,
            "Pull request merge method",
            Some("Choose the method new pull requests start with."),
            SettingControl::Choice {
                choices: merge_choices(),
                selected: Some(merge_id(method).into()),
            },
        )
    }
}
fn auto_pull_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: resettable_for(source, on != HostSettings::default().default_auto_pull),
        source,
        ..row(
            SettingId::DefaultAutoPull,
            "Pull before new threads",
            Some("Update the checkout before starting a new thread."),
            SettingControl::Switch { on },
        )
    }
}
fn branch_row(mode: BranchNamingMode, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: resettable_for(source, mode != HostSettings::default().branch_naming_mode),
        source,
        ..row(
            SettingId::BranchNaming,
            "Branch naming",
            Some("Choose how generated worktree branches are named."),
            SettingControl::Choice {
                choices: branch_choices(),
                selected: Some(branch_id(mode).into()),
            },
        )
    }
}
fn style_row(mode: SourceControlWritingStyleMode, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: resettable_for(
            source,
            mode != HostSettings::default().source_control_writing_style.mode,
        ),
        source,
        ..row(
            SettingId::SourceControlWritingStyle,
            "Writing style",
            Some("Choose the style for generated commits and pull requests."),
            SettingControl::Choice {
                choices: style_choices(),
                selected: Some(style_id(mode).into()),
            },
        )
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::DefaultAutoPull,
        SettingId::BranchNaming,
        SettingId::SourceControlWritingStyle,
        SettingId::FollowChangeRequestTemplates,
        SettingId::PullRequestMergeMethod,
    ],
    host: |context: &Context| {
        let host = context.host?;
        Some(section(
            "source-control",
            "Source control",
            vec![
                auto_pull_row(host.default_auto_pull, None),
                branch_row(host.branch_naming_mode, None),
                style_row(host.source_control_writing_style.mode, None),
                SettingsRow {
                    resettable: !host
                        .source_control_writing_style
                        .follow_change_request_templates,
                    ..row(
                        SettingId::FollowChangeRequestTemplates,
                        "Use change request templates",
                        Some("Fill the repository's pull request template when generating text."),
                        SettingControl::Switch {
                            on: host
                                .source_control_writing_style
                                .follow_change_request_templates,
                        },
                    )
                },
                merge_row(host.pull_request_merge_method, None),
            ],
            Some(
                "GitHub actions and provider authentication remain available through the existing provider surfaces.",
            ),
        ))
    },
    project: |resolved| {
        Some(section(
            "source-control",
            "Source control",
            vec![
                auto_pull_row(
                    resolved.default_auto_pull.0,
                    Some(resolved.default_auto_pull.1),
                ),
                branch_row(
                    resolved.branch_naming_mode.0,
                    Some(resolved.branch_naming_mode.1),
                ),
                merge_row(
                    resolved.pull_request_merge_method.0,
                    Some(resolved.pull_request_merge_method.1),
                ),
            ],
            None,
        ))
    },
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::DefaultAutoPull, SettingValue::Switch { on }) => {
            Some(update(scope, SettingChange::DefaultAutoPull { on: *on }))
        }
        (SettingId::BranchNaming, SettingValue::Choice { id }) => [
            BranchNamingMode::Static,
            BranchNamingMode::Semantic,
            BranchNamingMode::Custom,
        ]
        .into_iter()
        .find(|mode| branch_id(*mode) == id)
        .map(|mode| update(scope, SettingChange::BranchNamingMode { mode })),
        (SettingId::SourceControlWritingStyle, SettingValue::Choice { id }) => [
            SourceControlWritingStyleMode::RepoConventions,
            SourceControlWritingStyleMode::ConventionalCommits,
            SourceControlWritingStyleMode::Custom,
        ]
        .into_iter()
        .find(|mode| style_id(*mode) == id)
        .map(|mode| update(scope, SettingChange::SourceControlWritingStyleMode { mode })),
        (SettingId::FollowChangeRequestTemplates, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::FollowChangeRequestTemplates { on: *on },
        )),
        (SettingId::PullRequestMergeMethod, SettingValue::Choice { id }) => [
            None,
            Some(PullRequestMergeMethod::Merge),
            Some(PullRequestMergeMethod::Squash),
            Some(PullRequestMergeMethod::Rebase),
        ]
        .into_iter()
        .find(|method| merge_id(*method) == id)
        .map(|method| update(scope, SettingChange::PullRequestMergeMethod { method })),
        _ => None,
    },
    reset: |id| match id {
        SettingId::DefaultAutoPull => Some(update(
            &SettingsScope::Host,
            SettingChange::DefaultAutoPull {
                on: HostSettings::default().default_auto_pull,
            },
        )),
        SettingId::BranchNaming => Some(update(
            &SettingsScope::Host,
            SettingChange::BranchNamingMode {
                mode: HostSettings::default().branch_naming_mode,
            },
        )),
        SettingId::SourceControlWritingStyle => Some(update(
            &SettingsScope::Host,
            SettingChange::SourceControlWritingStyleMode {
                mode: HostSettings::default().source_control_writing_style.mode,
            },
        )),
        SettingId::FollowChangeRequestTemplates => Some(update(
            &SettingsScope::Host,
            SettingChange::FollowChangeRequestTemplates {
                on: HostSettings::default()
                    .source_control_writing_style
                    .follow_change_request_templates,
            },
        )),
        SettingId::PullRequestMergeMethod => Some(update(
            &SettingsScope::Host,
            SettingChange::PullRequestMergeMethod {
                method: HostSettings::default().pull_request_merge_method,
            },
        )),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::DefaultAutoPull => Some(ProjectSettingKey::DefaultAutoPull),
        SettingId::BranchNaming => Some(ProjectSettingKey::BranchNamingMode),
        SettingId::PullRequestMergeMethod => Some(ProjectSettingKey::PullRequestMergeMethod),
        _ => None,
    },
};
