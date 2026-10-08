//! Host-owned cleanup switches. Cleanup remains opt-in until one of these
//! values is changed, and the Host executes the resulting policy.
use super::{
    SettingChange, SettingControl, SettingId, SettingValue, SettingsScope,
    registry::{Context, Section},
    row, section, update,
};
use crate::models::HostSettings;

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::StorageWorktreeAfterDays,
        SettingId::StorageWorktreeOnMerge,
        SettingId::StorageBrowserArtifactsAfterDays,
        SettingId::StorageLogsAfterDays,
        SettingId::AddProjectBaseDirectory,
        SettingId::StorageWorktreeOnDelete,
        SettingId::StorageWorktreeUnchanged,
    ],
    host: |context: &Context| {
        let host = context.host?;
        Some(section(
            "storage",
            "Storage",
            vec![
                super::SettingsRow {
                    resettable: host.storage_cleanup.worktree_after_days.is_some(),
                    ..row(
                        SettingId::StorageWorktreeAfterDays,
                        "Remove inactive worktrees after days",
                        Some("Leave empty to keep inactive worktrees."),
                        SettingControl::Number {
                            value: host.storage_cleanup.worktree_after_days.unwrap_or(30),
                            min: agent_protocol::models::MIN_RETENTION_DAYS,
                            max: agent_protocol::models::MAX_RETENTION_DAYS,
                        },
                    )
                },
                row(
                    SettingId::StorageWorktreeOnMerge,
                    "Remove merged worktrees",
                    Some("Remove clean linked worktrees after their branch is merged."),
                    SettingControl::Switch {
                        on: host.storage_cleanup.worktree_on_merge,
                    },
                ),
                super::SettingsRow {
                    resettable: host.storage_cleanup.browser_artifacts_after_days.is_some(),
                    ..row(
                        SettingId::StorageBrowserArtifactsAfterDays,
                        "Remove browser artifacts after days",
                        Some("Leave empty to keep saved browser artifacts."),
                        SettingControl::Number {
                            value: host
                                .storage_cleanup
                                .browser_artifacts_after_days
                                .unwrap_or(30),
                            min: agent_protocol::models::MIN_RETENTION_DAYS,
                            max: agent_protocol::models::MAX_RETENTION_DAYS,
                        },
                    )
                },
                super::SettingsRow {
                    resettable: host.storage_cleanup.logs_after_days.is_some(),
                    ..row(
                        SettingId::StorageLogsAfterDays,
                        "Remove logs after days",
                        Some("Leave empty to keep diagnostic logs."),
                        SettingControl::Number {
                            value: host.storage_cleanup.logs_after_days.unwrap_or(30),
                            min: agent_protocol::models::MIN_RETENTION_DAYS,
                            max: agent_protocol::models::MAX_RETENTION_DAYS,
                        },
                    )
                },
                super::SettingsRow {
                    resettable: !host.add_project_base_directory.is_empty(),
                    ..row(
                        SettingId::AddProjectBaseDirectory,
                        "Add-project folder",
                        Some(
                            "Start the add-project folder picker here; leave empty for your home directory.",
                        ),
                        SettingControl::Text {
                            value: host.add_project_base_directory.clone(),
                            placeholder: Some("Home directory".into()),
                        },
                    )
                },
                row(
                    SettingId::StorageWorktreeOnDelete,
                    "Remove deleted-thread worktrees",
                    Some("Remove clean worktrees after their last thread is deleted."),
                    SettingControl::Switch {
                        on: host.storage_cleanup.worktree_on_delete,
                    },
                ),
                row(
                    SettingId::StorageWorktreeUnchanged,
                    "Remove unchanged worktrees",
                    Some("Allow cleanup of clean worktrees with no changes."),
                    SettingControl::Switch {
                        on: host.storage_cleanup.worktree_unchanged,
                    },
                ),
            ],
            Some("Cleanup never removes a checkout with uncommitted or unexpected ignored files."),
        ))
    },
    project: |_| None,
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::StorageWorktreeAfterDays, SettingValue::Number { value }) => {
            (agent_protocol::models::MIN_RETENTION_DAYS
                ..=agent_protocol::models::MAX_RETENTION_DAYS)
                .contains(value)
                .then(|| {
                    update(
                        scope,
                        SettingChange::StorageWorktreeAfterDays { days: Some(*value) },
                    )
                })
        }
        (SettingId::StorageWorktreeOnMerge, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::StorageWorktreeOnMerge { on: *on },
        )),
        (SettingId::StorageWorktreeOnDelete, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::StorageWorktreeOnDelete { on: *on },
        )),
        (SettingId::StorageWorktreeUnchanged, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::StorageWorktreeUnchanged { on: *on },
        )),
        (SettingId::StorageBrowserArtifactsAfterDays, SettingValue::Number { value }) => {
            (agent_protocol::models::MIN_RETENTION_DAYS
                ..=agent_protocol::models::MAX_RETENTION_DAYS)
                .contains(value)
                .then(|| {
                    update(
                        scope,
                        SettingChange::StorageBrowserArtifactsAfterDays { days: Some(*value) },
                    )
                })
        }
        (SettingId::StorageLogsAfterDays, SettingValue::Number { value }) => {
            (agent_protocol::models::MIN_RETENTION_DAYS
                ..=agent_protocol::models::MAX_RETENTION_DAYS)
                .contains(value)
                .then(|| {
                    update(
                        scope,
                        SettingChange::StorageLogsAfterDays { days: Some(*value) },
                    )
                })
        }
        (SettingId::AddProjectBaseDirectory, SettingValue::Text { value }) => Some(update(
            scope,
            SettingChange::AddProjectBaseDirectory {
                value: value.trim().to_owned(),
            },
        )),
        _ => None,
    },
    reset: |id| match id {
        SettingId::StorageWorktreeAfterDays => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageWorktreeAfterDays { days: None },
        )),
        SettingId::StorageWorktreeOnMerge => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageWorktreeOnMerge {
                on: HostSettings::default().storage_cleanup.worktree_on_merge,
            },
        )),
        SettingId::StorageWorktreeOnDelete => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageWorktreeOnDelete {
                on: HostSettings::default().storage_cleanup.worktree_on_delete,
            },
        )),
        SettingId::StorageWorktreeUnchanged => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageWorktreeUnchanged {
                on: HostSettings::default().storage_cleanup.worktree_unchanged,
            },
        )),
        SettingId::StorageBrowserArtifactsAfterDays => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageBrowserArtifactsAfterDays { days: None },
        )),
        SettingId::StorageLogsAfterDays => Some(update(
            &SettingsScope::Host,
            SettingChange::StorageLogsAfterDays { days: None },
        )),
        SettingId::AddProjectBaseDirectory => Some(update(
            &SettingsScope::Host,
            SettingChange::AddProjectBaseDirectory {
                value: String::new(),
            },
        )),
        _ => None,
    },
    inherit: |_| None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::HostSettings,
        state::Snapshot,
        view::{settings::settings_view, time::TimestampFormat},
    };

    #[test]
    fn add_project_base_directory_is_a_persisted_host_setting() {
        let host = HostSettings {
            add_project_base_directory: "~/projects".into(),
            ..Default::default()
        };
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let row = view
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.id == SettingId::AddProjectBaseDirectory)
            .expect("add-project setting row");
        assert!(row.resettable);
        assert!(matches!(
            row.control,
            SettingControl::Text { ref value, .. } if value == "~/projects"
        ));
        assert!(matches!(
            (SECTION.intent)(
                &Snapshot::default(),
                &SettingsScope::Host,
                SettingId::AddProjectBaseDirectory,
                &SettingValue::Text {
                    value: " /tmp/projects ".into()
                },
            ),
            Some(crate::state::Intent::UpdateSettings {
                scope: SettingsScope::Host,
                change: SettingChange::AddProjectBaseDirectory {
                    value,
                },
            }) if value == "/tmp/projects"
        ));
    }
}
