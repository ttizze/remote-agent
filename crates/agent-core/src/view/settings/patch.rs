//! Which Host settings a project page edits, the project's effective
//! values, and the patch that saves one change.
use super::{SettingSource, SettingsScope};
use crate::models::{AutoSettle, HostSettings, HostSettingsPatch};
use agent_domain::RuntimeMode;
use agent_protocol::models::{
    BackgroundActivityProfileSelection, OverrideChange, ProjectSettingsOverridesPatch,
    PullRequestMergeMethod, ResponseStreamingMode, SourceControlWritingStyleMode, ThreadEnvMode,
    WorktreeSubmodules,
};

/// A project's effective values and where each comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSettings {
    pub auto_settle: (AutoSettle, SettingSource),
    pub continue_after_restart: (bool, SettingSource),
    pub default_runtime_mode: (RuntimeMode, SettingSource),
    pub default_thread_env_mode: (Option<ThreadEnvMode>, SettingSource),
    pub worktree_submodules: (Option<WorktreeSubmodules>, SettingSource),
    pub new_worktrees_start_from_origin: (bool, SettingSource),
    pub agent_browser_access: (bool, SettingSource),
    pub default_auto_pull: (bool, SettingSource),
    pub auto_settle_on_merge: (bool, SettingSource),
    pub response_streaming_mode: (ResponseStreamingMode, SettingSource),
    pub branch_naming_mode: (agent_domain::BranchNamingMode, SettingSource),
    pub pull_request_merge_method: (Option<PullRequestMergeMethod>, SettingSource),
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
        default_runtime_mode: pick(
            overrides.and_then(|project| project.default_runtime_mode),
            host.default_runtime_mode,
        ),
        default_thread_env_mode: pick(
            overrides.and_then(|project| project.default_thread_env_mode.map(Some)),
            host.default_thread_env_mode,
        ),
        worktree_submodules: pick(
            overrides.and_then(|project| project.worktree_submodules.map(Some)),
            host.worktree_submodules,
        ),
        new_worktrees_start_from_origin: pick(
            overrides.and_then(|project| project.new_worktrees_start_from_origin),
            host.new_worktrees_start_from_origin,
        ),
        agent_browser_access: pick(
            overrides.and_then(|project| project.enable_agent_browser_access),
            host.enable_agent_browser_access,
        ),
        default_auto_pull: pick(
            overrides.and_then(|project| project.default_auto_pull),
            host.default_auto_pull,
        ),
        auto_settle_on_merge: pick(
            overrides.and_then(|project| project.auto_settle_on_merge),
            host.auto_settle_on_merge,
        ),
        response_streaming_mode: pick(
            overrides.and_then(|project| project.response_streaming_mode),
            host.response_streaming_mode,
        ),
        branch_naming_mode: pick(
            overrides.and_then(|project| project.branch_naming_mode),
            host.branch_naming_mode,
        ),
        pull_request_merge_method: pick(
            overrides
                .and_then(|project| project.pull_request_merge_method)
                .map(Into::into),
            host.pull_request_merge_method,
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
    DefaultRuntimeMode,
    DefaultThreadEnvMode,
    WorktreeSubmodules,
    NewWorktreesStartFromOrigin,
    AgentBrowserAccess,
    DefaultAutoPull,
    AutoSettleOnMerge,
    ResponseStreamingMode,
    BranchNamingMode,
    PullRequestMergeMethod,
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
    DefaultRuntimeMode {
        mode: RuntimeMode,
    },
    DefaultThreadEnvMode {
        mode: Option<ThreadEnvMode>,
    },
    WorktreeSubmodules {
        mode: Option<WorktreeSubmodules>,
    },
    NewWorktreesStartFromOrigin {
        on: bool,
    },
    ProviderUpdateChecks {
        on: bool,
    },
    AgentBrowserAccess {
        on: bool,
    },
    ResponseStreamingMode {
        mode: ResponseStreamingMode,
    },
    AutoSettleOnMerge {
        on: bool,
    },
    BackgroundActivityProfile {
        profile: BackgroundActivityProfileSelection,
    },
    DefaultAutoPull {
        on: bool,
    },
    BranchNamingMode {
        mode: agent_domain::BranchNamingMode,
    },
    SourceControlWritingStyleMode {
        mode: SourceControlWritingStyleMode,
    },
    FollowChangeRequestTemplates {
        on: bool,
    },
    PullRequestMergeMethod {
        method: Option<PullRequestMergeMethod>,
    },
    StorageWorktreeAfterDays {
        days: Option<u32>,
    },
    StorageWorktreeOnMerge {
        on: bool,
    },
    StorageWorktreeOnDelete {
        on: bool,
    },
    StorageWorktreeUnchanged {
        on: bool,
    },
    StorageBrowserArtifactsAfterDays {
        days: Option<u32>,
    },
    StorageLogsAfterDays {
        days: Option<u32>,
    },
    AddProjectBaseDirectory {
        value: String,
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
        ProjectSettingKey::DefaultRuntimeMode => {
            patch.default_runtime_mode = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::DefaultThreadEnvMode => {
            patch.default_thread_env_mode = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::WorktreeSubmodules => {
            patch.worktree_submodules = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::NewWorktreesStartFromOrigin => {
            patch.new_worktrees_start_from_origin = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::AgentBrowserAccess => {
            patch.enable_agent_browser_access = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::DefaultAutoPull => {
            patch.default_auto_pull = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::AutoSettleOnMerge => {
            patch.auto_settle_on_merge = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::ResponseStreamingMode => {
            patch.response_streaming_mode = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::BranchNamingMode => {
            patch.branch_naming_mode = Some(OverrideChange::Inherit)
        }
        ProjectSettingKey::PullRequestMergeMethod => {
            patch.pull_request_merge_method = Some(OverrideChange::Inherit)
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
            SettingChange::DefaultRuntimeMode { mode } => patch.default_runtime_mode = Some(*mode),
            SettingChange::DefaultThreadEnvMode { mode } => {
                patch.default_thread_env_mode = Some(match mode {
                    Some(mode) => agent_protocol::models::Nullable::Value(*mode),
                    None => agent_protocol::models::Nullable::Null,
                })
            }
            SettingChange::WorktreeSubmodules { mode } => {
                patch.worktree_submodules = Some(match mode {
                    Some(mode) => agent_protocol::models::Nullable::Value(*mode),
                    None => agent_protocol::models::Nullable::Null,
                })
            }
            SettingChange::NewWorktreesStartFromOrigin { on } => {
                patch.new_worktrees_start_from_origin = Some(*on)
            }
            SettingChange::ProviderUpdateChecks { on } => {
                patch.enable_provider_update_checks = Some(*on)
            }
            SettingChange::AgentBrowserAccess { on } => {
                patch.enable_agent_browser_access = Some(*on)
            }
            SettingChange::ResponseStreamingMode { mode } => {
                patch.response_streaming_mode = Some(*mode)
            }
            SettingChange::AutoSettleOnMerge { on } => patch.auto_settle_on_merge = Some(*on),
            SettingChange::BackgroundActivityProfile { profile } => {
                patch.background_activity = Some(agent_protocol::models::BackgroundActivityPatch {
                    profile: Some(*profile),
                    ..Default::default()
                })
            }
            SettingChange::DefaultAutoPull { on } => patch.default_auto_pull = Some(*on),
            SettingChange::BranchNamingMode { mode } => patch.branch_naming_mode = Some(*mode),
            SettingChange::SourceControlWritingStyleMode { mode } => {
                patch.source_control_writing_style =
                    Some(agent_protocol::models::SourceControlWritingStylePatch {
                        mode: Some(*mode),
                        ..Default::default()
                    })
            }
            SettingChange::FollowChangeRequestTemplates { on } => {
                patch.source_control_writing_style =
                    Some(agent_protocol::models::SourceControlWritingStylePatch {
                        follow_change_request_templates: Some(*on),
                        ..Default::default()
                    })
            }
            SettingChange::PullRequestMergeMethod { method } => {
                patch.pull_request_merge_method = Some(match method {
                    Some(method) => agent_protocol::models::Nullable::Value(*method),
                    None => agent_protocol::models::Nullable::Null,
                })
            }
            SettingChange::StorageWorktreeAfterDays { days } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    worktree_after_days: Some(match days {
                        Some(days) => agent_protocol::models::Nullable::Value(*days),
                        None => agent_protocol::models::Nullable::Null,
                    }),
                    ..Default::default()
                })
            }
            SettingChange::StorageWorktreeOnMerge { on } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    worktree_on_merge: Some(*on),
                    ..Default::default()
                })
            }
            SettingChange::StorageWorktreeOnDelete { on } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    worktree_on_delete: Some(*on),
                    ..Default::default()
                })
            }
            SettingChange::StorageWorktreeUnchanged { on } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    worktree_unchanged: Some(*on),
                    ..Default::default()
                })
            }
            SettingChange::StorageBrowserArtifactsAfterDays { days } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    browser_artifacts_after_days: Some(match days {
                        Some(days) => agent_protocol::models::Nullable::Value(*days),
                        None => agent_protocol::models::Nullable::Null,
                    }),
                    ..Default::default()
                })
            }
            SettingChange::StorageLogsAfterDays { days } => {
                patch.storage_cleanup = Some(agent_protocol::models::StorageCleanupPatch {
                    logs_after_days: Some(match days {
                        Some(days) => agent_protocol::models::Nullable::Value(*days),
                        None => agent_protocol::models::Nullable::Null,
                    }),
                    ..Default::default()
                })
            }
            SettingChange::AddProjectBaseDirectory { value } => {
                patch.add_project_base_directory = Some(value.trim().to_owned())
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
                SettingChange::DefaultRuntimeMode { mode } => {
                    overrides.default_runtime_mode = Some(OverrideChange::Value(*mode))
                }
                SettingChange::DefaultThreadEnvMode { mode } => {
                    overrides.default_thread_env_mode = Some(match mode {
                        Some(mode) => OverrideChange::Value(*mode),
                        None => OverrideChange::Inherit,
                    })
                }
                SettingChange::WorktreeSubmodules { mode } => {
                    overrides.worktree_submodules = Some(match mode {
                        Some(mode) => OverrideChange::Value(*mode),
                        None => OverrideChange::Inherit,
                    })
                }
                SettingChange::NewWorktreesStartFromOrigin { on } => {
                    overrides.new_worktrees_start_from_origin = Some(OverrideChange::Value(*on))
                }
                SettingChange::AgentBrowserAccess { on } => {
                    overrides.enable_agent_browser_access = Some(OverrideChange::Value(*on))
                }
                SettingChange::DefaultAutoPull { on } => {
                    overrides.default_auto_pull = Some(OverrideChange::Value(*on))
                }
                SettingChange::AutoSettleOnMerge { on } => {
                    overrides.auto_settle_on_merge = Some(OverrideChange::Value(*on))
                }
                SettingChange::ResponseStreamingMode { mode } => {
                    overrides.response_streaming_mode = Some(OverrideChange::Value(*mode))
                }
                SettingChange::BranchNamingMode { mode } => {
                    overrides.branch_naming_mode = Some(OverrideChange::Value(*mode))
                }
                SettingChange::PullRequestMergeMethod { method } => {
                    overrides.pull_request_merge_method = Some(match method {
                        Some(method) => {
                            OverrideChange::Value(agent_protocol::models::Nullable::Value(*method))
                        }
                        None => OverrideChange::Value(agent_protocol::models::Nullable::Null),
                    })
                }
                SettingChange::Inherit { key } => clear(&mut overrides, *key),
                SettingChange::AutoResumeLimitedThreads { .. }
                | SettingChange::SnoozeLimitedThreads { .. }
                | SettingChange::ProviderUpdateChecks { .. }
                | SettingChange::BackgroundActivityProfile { .. }
                | SettingChange::SourceControlWritingStyleMode { .. }
                | SettingChange::FollowChangeRequestTemplates { .. }
                | SettingChange::StorageWorktreeAfterDays { .. }
                | SettingChange::StorageWorktreeOnMerge { .. }
                | SettingChange::StorageWorktreeOnDelete { .. }
                | SettingChange::StorageWorktreeUnchanged { .. } => return None,
                SettingChange::StorageBrowserArtifactsAfterDays { .. }
                | SettingChange::StorageLogsAfterDays { .. }
                | SettingChange::AddProjectBaseDirectory { .. } => return None,
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
    fn new_sections_use_sparse_host_and_project_patches() {
        let mut host = HostSettings::default();
        host.source_control_writing_style
            .follow_change_request_templates = false;
        host.storage_cleanup.worktree_on_merge = true;
        let style = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::SourceControlWritingStyleMode {
                mode: SourceControlWritingStyleMode::ConventionalCommits,
            },
        )
        .unwrap();
        assert_eq!(
            style.source_control_writing_style,
            Some(agent_protocol::models::SourceControlWritingStylePatch {
                mode: Some(SourceControlWritingStyleMode::ConventionalCommits),
                ..Default::default()
            })
        );
        let updated = host.patched(&style);
        assert!(
            !updated
                .source_control_writing_style
                .follow_change_request_templates
        );
        assert_eq!(
            updated.source_control_writing_style.mode,
            SourceControlWritingStyleMode::ConventionalCommits
        );
        let storage = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::StorageWorktreeAfterDays { days: Some(14) },
        )
        .unwrap();
        assert_eq!(
            storage.storage_cleanup,
            Some(agent_protocol::models::StorageCleanupPatch {
                worktree_after_days: Some(agent_protocol::models::Nullable::Value(14)),
                ..Default::default()
            })
        );
        assert!(host.patched(&storage).storage_cleanup.worktree_on_merge);
        let browser = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::StorageBrowserArtifactsAfterDays { days: Some(21) },
        )
        .unwrap();
        assert_eq!(
            host.patched(&browser)
                .storage_cleanup
                .browser_artifacts_after_days,
            Some(21)
        );
        let logs = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::StorageLogsAfterDays { days: Some(45) },
        )
        .unwrap();
        assert_eq!(
            host.patched(&logs).storage_cleanup.logs_after_days,
            Some(45)
        );
        let project_patch = plan_settings_update(
            &project("p"),
            &SettingChange::ResponseStreamingMode {
                mode: ResponseStreamingMode::Turn,
            },
        )
        .unwrap();
        assert_eq!(
            project_patch.project_overrides["p"]
                .as_ref()
                .unwrap()
                .response_streaming_mode,
            Some(OverrideChange::Value(ResponseStreamingMode::Turn))
        );
        let resolved = resolve_project_settings(&host.patched(&project_patch), Some("p"));
        assert_eq!(
            resolved.response_streaming_mode.0,
            ResponseStreamingMode::Turn
        );
        assert_eq!(resolved.response_streaming_mode.1, SettingSource::Project);

        let merge = plan_settings_update(
            &project("p"),
            &SettingChange::PullRequestMergeMethod {
                method: Some(PullRequestMergeMethod::Squash),
            },
        )
        .unwrap();
        let merged = host.patched(&merge);
        assert_eq!(
            resolve_project_settings(&merged, Some("p")).pull_request_merge_method,
            (Some(PullRequestMergeMethod::Squash), SettingSource::Project)
        );
        let inherited = plan_settings_update(
            &project("p"),
            &SettingChange::Inherit {
                key: ProjectSettingKey::PullRequestMergeMethod,
            },
        )
        .unwrap();
        assert_eq!(
            resolve_project_settings(&merged.patched(&inherited), Some("p"))
                .pull_request_merge_method,
            (None, SettingSource::Host)
        );

        let submodules = plan_settings_update(
            &project("p"),
            &SettingChange::WorktreeSubmodules {
                mode: Some(WorktreeSubmodules::TopLevel),
            },
        )
        .unwrap();
        let submodule_host = host.patched(&submodules);
        assert_eq!(
            resolve_project_settings(&submodule_host, Some("p")).worktree_submodules,
            (Some(WorktreeSubmodules::TopLevel), SettingSource::Project)
        );
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

    #[test]
    fn absent_nullable_project_overrides_inherit_the_host_value() {
        let mut host = HostSettings {
            default_thread_env_mode: Some(ThreadEnvMode::Worktree),
            ..Default::default()
        };
        let inherited = resolve_project_settings(&host, Some("p"));
        assert_eq!(
            inherited.default_thread_env_mode,
            (Some(ThreadEnvMode::Worktree), SettingSource::Host)
        );

        host.project_overrides
            .insert("p".into(), ProjectSettingsOverrides::default());
        assert_eq!(
            resolve_project_settings(&host, Some("p")).default_thread_env_mode,
            (Some(ThreadEnvMode::Worktree), SettingSource::Host)
        );
    }
}
