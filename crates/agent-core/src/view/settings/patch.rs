//! Which Host settings a project page edits, the project's effective
//! values, and the patch that saves one change.
use super::{SettingSource, SettingsScope};
use crate::models::{AutoSettle, HostSettings, HostSettingsPatch};
use agent_protocol::models::{OverrideChange, ProjectSettingsOverridesPatch};

/// A project's effective values and where each comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSettings {
    pub auto_settle: (AutoSettle, SettingSource),
    pub continue_after_restart: (bool, SettingSource),
    pub new_worktrees_start_from_origin: (bool, SettingSource),
}

pub fn resolve_project_settings(host: &HostSettings, project_id: Option<&str>) -> ResolvedSettings {
    fn pick<T>(value: Option<T>, fallback: T) -> (T, SettingSource) {
        value.map_or((fallback, SettingSource::Host), |value| {
            (value, SettingSource::Project)
        })
    }
    let overrides = project_id.and_then(|id| host.project_overrides.get(id));
    ResolvedSettings {
        auto_settle: pick(
            overrides.and_then(|project| project.auto_settle),
            host.auto_settle,
        ),
        continue_after_restart: pick(
            overrides.and_then(|project| project.continue_after_restart),
            host.continue_after_restart,
        ),
        new_worktrees_start_from_origin: pick(
            overrides.and_then(|project| project.new_worktrees_start_from_origin),
            host.new_worktrees_start_from_origin,
        ),
    }
}

/// Whether a project's new worktrees start from origin; on until the Host's
/// settings are read.
pub fn new_worktrees_start_from_origin(
    host: Option<&HostSettings>,
    project_id: Option<&str>,
) -> bool {
    host.map_or(
        HostSettings::default().new_worktrees_start_from_origin,
        |host| {
            resolve_project_settings(host, project_id)
                .new_worktrees_start_from_origin
                .0
        },
    )
}

/// A Host setting a project can override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProjectSettingKey {
    AutoSettle,
    ContinueAfterRestart,
    NewWorktreesStartFromOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingChange {
    AutoResumeLimitedThreads {
        on: bool,
    },
    SnoozeLimitedThreads {
        on: bool,
    },
    /// `None` turns auto-settle off.
    AutoSettle {
        days: Option<u32>,
    },
    ContinueAfterRestart {
        on: bool,
    },
    NewWorktreesStartFromOrigin {
        on: bool,
    },
    /// A project follows the Host's value again.
    Inherit {
        key: ProjectSettingKey,
    },
}

fn auto_settle(days: Option<u32>) -> AutoSettle {
    days.map_or(AutoSettle::Never, AutoSettle::AfterDays)
}

fn clear(patch: &mut ProjectSettingsOverridesPatch, key: ProjectSettingKey) {
    match key {
        ProjectSettingKey::AutoSettle => patch.auto_settle = Some(OverrideChange::Inherit),
        ProjectSettingKey::ContinueAfterRestart => {
            patch.continue_after_restart = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::NewWorktreesStartFromOrigin => {
            patch.new_worktrees_start_from_origin = Some(OverrideChange::Inherit)
        }
    }
}

fn project_patch(project_id: &str, patch: ProjectSettingsOverridesPatch) -> HostSettingsPatch {
    HostSettingsPatch {
        project_overrides: [(project_id.into(), Some(patch))].into(),
        ..Default::default()
    }
}

/// The patch saving one change. A project page writes only that project's
/// overrides; usage-limit handling stays Host-wide, so a project page cannot
/// change it (`None`).
pub fn plan_settings_update(
    scope: &SettingsScope,
    change: &SettingChange,
) -> Option<HostSettingsPatch> {
    let mut patch = HostSettingsPatch::default();
    match scope {
        SettingsScope::Host => match change {
            SettingChange::AutoResumeLimitedThreads { on } => {
                patch.auto_resume_limited_threads = Some(*on)
            }
            SettingChange::SnoozeLimitedThreads { on } => patch.snooze_limited_threads = Some(*on),
            SettingChange::AutoSettle { days } => patch.auto_settle = Some(auto_settle(*days)),
            SettingChange::ContinueAfterRestart { on } => patch.continue_after_restart = Some(*on),
            SettingChange::NewWorktreesStartFromOrigin { on } => {
                patch.new_worktrees_start_from_origin = Some(*on)
            }
            SettingChange::Inherit { .. } => return None,
        },
        SettingsScope::Project { project_id } => {
            let mut overrides = ProjectSettingsOverridesPatch::default();
            match change {
                SettingChange::AutoSettle { days } => {
                    overrides.auto_settle = Some(OverrideChange::Value(auto_settle(*days)))
                }
                SettingChange::ContinueAfterRestart { on } => {
                    overrides.continue_after_restart = Some(OverrideChange::Value(*on))
                }
                SettingChange::NewWorktreesStartFromOrigin { on } => {
                    overrides.new_worktrees_start_from_origin = Some(OverrideChange::Value(*on))
                }
                SettingChange::Inherit { key } => clear(&mut overrides, *key),
                SettingChange::AutoResumeLimitedThreads { .. }
                | SettingChange::SnoozeLimitedThreads { .. } => return None,
            }
            patch = project_patch(project_id, overrides);
        }
    }
    Some(patch)
}

/// Clears the project's overrides of `keys`, keeping its others.
pub fn clear_project_overrides(project_id: &str, keys: &[ProjectSettingKey]) -> HostSettingsPatch {
    let mut overrides = ProjectSettingsOverridesPatch::default();
    for key in keys {
        clear(&mut overrides, *key);
    }
    project_patch(project_id, overrides)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::ProjectSettingsOverrides,
        view::settings::fixtures::{host_with, project},
    };

    #[test]
    fn edits_a_project_override_without_changing_the_host_default() {
        let host = host_with(
            "first-project",
            ProjectSettingsOverrides {
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let next = host.patched(
            &plan_settings_update(
                &project("first-project"),
                &SettingChange::AutoSettle { days: None },
            )
            .unwrap(),
        );
        assert_eq!(
            next.project_overrides["first-project"],
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::Never),
                continue_after_restart: Some(true),
                ..Default::default()
            }
        );
        assert_eq!(next.auto_settle, host.auto_settle);
        let second = host.patched(
            &plan_settings_update(
                &project("second-project"),
                &SettingChange::ContinueAfterRestart { on: false },
            )
            .unwrap(),
        );
        assert_eq!(
            second.project_overrides["second-project"].continue_after_restart,
            Some(false)
        );
        assert!(!second.continue_after_restart);
    }

    #[test]
    fn inheriting_removes_only_that_project_override() {
        let host = host_with(
            "first-project",
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::AfterDays(7)),
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let next = host.patched(
            &plan_settings_update(
                &project("first-project"),
                &SettingChange::Inherit {
                    key: ProjectSettingKey::ContinueAfterRestart,
                },
            )
            .unwrap(),
        );
        assert_eq!(
            next.project_overrides["first-project"],
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::AfterDays(7)),
                continue_after_restart: None,
                ..Default::default()
            }
        );
        assert_eq!(
            plan_settings_update(
                &SettingsScope::Host,
                &SettingChange::Inherit {
                    key: ProjectSettingKey::AutoSettle
                },
            ),
            None
        );
    }

    #[test]
    fn resets_only_the_selected_override_and_rejects_host_wide_writes_from_a_project() {
        let host = host_with(
            "first-project",
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::Never),
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let cleared = host.patched(&clear_project_overrides(
            "first-project",
            &[ProjectSettingKey::AutoSettle],
        ));
        assert_eq!(
            cleared.project_overrides["first-project"],
            ProjectSettingsOverrides {
                auto_settle: None,
                continue_after_restart: Some(true),
                ..Default::default()
            }
        );
        let all = host.patched(&clear_project_overrides(
            "first-project",
            &[
                ProjectSettingKey::AutoSettle,
                ProjectSettingKey::ContinueAfterRestart,
            ],
        ));
        assert!(all.project_overrides.is_empty());
        assert_eq!(
            plan_settings_update(
                &project("first-project"),
                &SettingChange::SnoozeLimitedThreads { on: true },
            ),
            None
        );
    }

    #[test]
    fn host_wide_changes_patch_only_the_host_value() {
        let host = HostSettings::default();
        let resume = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::AutoResumeLimitedThreads { on: true },
        )
        .unwrap();
        assert_eq!(
            resume,
            HostSettingsPatch {
                auto_resume_limited_threads: Some(true),
                ..Default::default()
            }
        );
        // A second change planned from the same settings keeps the first.
        let settle = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::AutoSettle { days: Some(7) },
        )
        .unwrap();
        let next = host.patched(&resume).patched(&settle);
        assert!(next.auto_resume_limited_threads);
        assert_eq!(next.auto_settle, AutoSettle::AfterDays(7));
    }

    #[test]
    fn concurrent_project_edits_and_inheritance_keep_unrelated_overrides() {
        let original = host_with(
            "p",
            ProjectSettingsOverrides {
                branch_name_prefix: Some("team".into()),
                ..Default::default()
            },
        );
        let settle =
            plan_settings_update(&project("p"), &SettingChange::AutoSettle { days: Some(7) })
                .unwrap();
        let restart = plan_settings_update(
            &project("p"),
            &SettingChange::ContinueAfterRestart { on: true },
        )
        .unwrap();
        let result = original
            .patched(&settle)
            .patched(&restart)
            .patched(&clear_project_overrides(
                "p",
                &[ProjectSettingKey::AutoSettle],
            ));
        assert_eq!(
            result.project_overrides["p"],
            ProjectSettingsOverrides {
                continue_after_restart: Some(true),
                branch_name_prefix: Some("team".into()),
                ..Default::default()
            }
        );
    }
}
