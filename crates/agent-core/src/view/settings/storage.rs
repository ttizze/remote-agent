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
        _ => None,
    },
    inherit: |_| None,
};
