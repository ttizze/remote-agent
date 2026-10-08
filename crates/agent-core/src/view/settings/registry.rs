//! The settings page assembled from its sections, in display order.
use super::{
    ProjectOverridesHeader, ProjectSettingKey, ResolvedSettings, SettingId, SettingSource,
    SettingValue, SettingsRow, SettingsScope, SettingsSection, SettingsView, agent, auto_settle,
    behavior, beta, browser, capture, follow_ups, maintenance, new_threads, notifications, patch,
    source_control, storage, update, usage_limits,
};
use crate::{
    models::HostSettings,
    state::{Intent, Snapshot},
    view::time::TimestampFormat,
};
use agent_protocol::models::{ProjectSettingsOverrides, WorktreeSettings};

/// What the Host page's rows are built from.
pub(super) struct Context<'a> {
    pub snapshot: &'a Snapshot,
    /// The Host's conversation settings once read.
    pub host: Option<&'a HostSettings>,
    pub timestamp_format: TimestampFormat,
}

/// One section of the page and the rows it owns.
pub(super) struct Section {
    /// The rows this section edits and resets; no other section lists them.
    pub ids: &'static [SettingId],
    /// The section on the Host page; `None` leaves it out, such as while
    /// the Host's settings are unread.
    pub host: fn(&Context) -> Option<SettingsSection>,
    /// The section on a project page, showing the project's effective
    /// values; `None` for sections without project overrides.
    pub project: fn(&ResolvedSettings) -> Option<SettingsSection>,
    /// What giving one of the section's rows a new value does; `None` when
    /// the value does not fit.
    pub intent: fn(&Snapshot, &SettingsScope, SettingId, &SettingValue) -> Option<Intent>,
    /// What returns a Host row to its default; `None` for rows without one.
    pub reset: fn(SettingId) -> Option<Intent>,
    /// The Host setting a project row overrides.
    pub inherit: fn(SettingId) -> Option<ProjectSettingKey>,
}

static SECTIONS: [Section; 13] = [
    usage_limits::SECTION,
    auto_settle::SECTION,
    follow_ups::SECTION,
    behavior::SECTION,
    agent::SECTION,
    maintenance::SECTION,
    source_control::SECTION,
    storage::SECTION,
    beta::SECTION,
    browser::SECTION,
    capture::SECTION,
    new_threads::SECTION,
    notifications::SECTION,
];

fn owner(id: SettingId) -> &'static Section {
    SECTIONS
        .iter()
        .find(|section| section.ids.contains(&id))
        .expect("every setting belongs to a section")
}

/// The settings page. `host` is the Host's conversation settings once read;
/// rows backed by them are left out until then. A project page shows that
/// project's effective auto-settle and restart continuation.
pub fn settings_view(
    snapshot: &Snapshot,
    host: Option<&HostSettings>,
    scope: &SettingsScope,
    timestamp_format: TimestampFormat,
) -> SettingsView {
    match scope {
        SettingsScope::Host => {
            let context = Context {
                snapshot,
                host,
                timestamp_format,
            };
            SettingsView {
                project: None,
                sections: SECTIONS
                    .iter()
                    .filter_map(|section| (section.host)(&context))
                    .collect(),
            }
        }
        SettingsScope::Project { project_id } => {
            let label = snapshot
                .shell_projects()
                .iter()
                .find(|project| &project.id == project_id)
                .map_or_else(
                    || "Unavailable project".into(),
                    |project| project.name.clone(),
                );
            let Some(host) = host else {
                return SettingsView {
                    project: None,
                    sections: vec![],
                };
            };
            let resolved = patch::resolve_project_settings(host, Some(project_id));
            SettingsView {
                project: Some(ProjectOverridesHeader {
                    project_id: project_id.clone(),
                    label,
                    has_overrides: host
                        .project_overrides
                        .get(project_id)
                        .is_some_and(|overrides| overrides != &ProjectSettingsOverrides::default()),
                }),
                sections: SECTIONS
                    .iter()
                    .filter_map(|section| (section.project)(&resolved))
                    .collect(),
            }
        }
    }
}

/// What giving setting `id` the new `value` on the page of `scope` does;
/// `None` when the value does not fit the setting.
pub fn setting_intent(
    snapshot: &Snapshot,
    scope: &SettingsScope,
    id: SettingId,
    value: &SettingValue,
) -> Option<Intent> {
    (owner(id).intent)(snapshot, scope, id, value)
}

/// What a row's reset arrow does: a project row follows the Host again, a
/// Host row returns to its default. `None` when the row offers no reset.
pub fn setting_reset_intent(scope: &SettingsScope, row: &SettingsRow) -> Option<Intent> {
    let section = owner(row.id);
    match scope {
        SettingsScope::Project { .. } => {
            if row.source != Some(SettingSource::Project) {
                return None;
            }
            let key = (section.inherit)(row.id)?;
            Some(update(scope, super::SettingChange::Inherit { key }))
        }
        SettingsScope::Host => row.resettable.then(|| (section.reset)(row.id)).flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::build::FollowUpBehavior,
        models::AutoSettle,
        view::settings::{
            SettingChange, SettingControl,
            fixtures::{find, host_with, ids, project, same},
        },
    };

    #[test]
    fn each_row_belongs_to_exactly_one_section() {
        let mut seen = vec![];
        for section in &SECTIONS {
            for id in section.ids {
                assert!(!seen.contains(id), "{id:?} listed twice");
                seen.push(*id);
            }
        }
        for section in &SECTIONS {
            for id in section.ids {
                assert!(std::ptr::eq(owner(*id), section));
            }
        }
    }

    #[test]
    fn host_rows_wait_for_the_host_settings() {
        let snapshot = Snapshot::default();
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        assert_eq!(
            ids(&view),
            [
                ("follow-ups".into(), vec![SettingId::FollowUpBehavior]),
                ("behavior".into(), vec![SettingId::TimeFormat]),
                ("beta".into(), vec![SettingId::WorkingSection]),
                (
                    "browser".into(),
                    vec![
                        SettingId::BrowserDefaultViewport,
                        SettingId::BrowserDefaultZoom,
                        SettingId::BrowserDefaultAppearance,
                        SettingId::BrowserRecordingFrameRate,
                        SettingId::BrowserRecordingShowKeyPresses,
                        SettingId::BrowserRecordingShowMousePresses,
                        SettingId::BrowserLinkTarget,
                        SettingId::BrowserAutoShowFloatingPreview,
                        SettingId::BrowserDefaultProfile,
                    ]
                ),
                (
                    "capture".into(),
                    vec![
                        SettingId::LoadBalancing,
                        SettingId::SnapshotCapture,
                        SettingId::SnapshotIncludeAccessibility,
                        SettingId::SnapshotShortcut,
                        SettingId::SnapshotPlaySound,
                        SettingId::SnapshotSound,
                        SettingId::SnapshotFlash,
                        SettingId::SnapshotAnimations,
                    ]
                ),
                (
                    "new-threads".into(),
                    vec![SettingId::DefaultModel, SettingId::DefaultPermissions]
                ),
                (
                    "notifications".into(),
                    vec![
                        SettingId::NotificationMode,
                        SettingId::InAppNotifications,
                        SettingId::LiveActivities,
                    ]
                ),
            ]
        );
    }

    #[test]
    fn the_host_page_shows_current_values_and_offers_resets() {
        let mut snapshot = Snapshot {
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        snapshot.workspace.worktree_settings = Some(WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        });
        let host = HostSettings {
            snooze_limited_threads: true,
            ..Default::default()
        };
        let view = settings_view(
            &snapshot,
            Some(&host),
            &SettingsScope::Host,
            TimestampFormat::TwentyFourHour,
        );
        assert_eq!(
            ids(&view),
            [
                (
                    "usage-limits".into(),
                    vec![
                        SettingId::AutoResumeLimitedThreads,
                        SettingId::SnoozeLimitedThreads
                    ]
                ),
                (
                    "auto-settle".into(),
                    vec![
                        SettingId::AutoSettleInactiveThreads,
                        SettingId::AutoSettleDays
                    ]
                ),
                ("follow-ups".into(), vec![SettingId::FollowUpBehavior]),
                (
                    "behavior".into(),
                    vec![SettingId::TimeFormat, SettingId::ContinueAfterRestart]
                ),
                (
                    "agent".into(),
                    vec![
                        SettingId::ProviderUpdateChecks,
                        SettingId::AgentBrowserAccess
                    ]
                ),
                (
                    "maintenance".into(),
                    vec![
                        SettingId::ResponseStreaming,
                        SettingId::AutoSettleOnMerge,
                        SettingId::BackgroundActivity
                    ]
                ),
                (
                    "source-control".into(),
                    vec![
                        SettingId::DefaultAutoPull,
                        SettingId::BranchNaming,
                        SettingId::SourceControlWritingStyle,
                        SettingId::FollowChangeRequestTemplates,
                        SettingId::PullRequestMergeMethod
                    ]
                ),
                (
                    "storage".into(),
                    vec![
                        SettingId::StorageWorktreeAfterDays,
                        SettingId::StorageWorktreeOnMerge,
                        SettingId::StorageBrowserArtifactsAfterDays,
                        SettingId::StorageLogsAfterDays,
                        SettingId::AddProjectBaseDirectory,
                        SettingId::StorageWorktreeOnDelete,
                        SettingId::StorageWorktreeUnchanged
                    ]
                ),
                ("beta".into(), vec![SettingId::WorkingSection]),
                (
                    "browser".into(),
                    vec![
                        SettingId::BrowserDefaultViewport,
                        SettingId::BrowserDefaultZoom,
                        SettingId::BrowserDefaultAppearance,
                        SettingId::BrowserRecordingFrameRate,
                        SettingId::BrowserRecordingShowKeyPresses,
                        SettingId::BrowserRecordingShowMousePresses,
                        SettingId::BrowserLinkTarget,
                        SettingId::BrowserAutoShowFloatingPreview,
                        SettingId::BrowserDefaultProfile,
                    ]
                ),
                (
                    "capture".into(),
                    vec![
                        SettingId::LoadBalancing,
                        SettingId::SnapshotCapture,
                        SettingId::SnapshotIncludeAccessibility,
                        SettingId::SnapshotShortcut,
                        SettingId::SnapshotPlaySound,
                        SettingId::SnapshotSound,
                        SettingId::SnapshotFlash,
                        SettingId::SnapshotAnimations,
                    ]
                ),
                (
                    "new-threads".into(),
                    vec![
                        SettingId::DefaultModel,
                        SettingId::DefaultPermissions,
                        SettingId::DefaultWorkspace,
                        SettingId::WorktreeSubmodules,
                        SettingId::StartFromOrigin
                    ]
                ),
                (
                    "notifications".into(),
                    vec![
                        SettingId::NotificationMode,
                        SettingId::InAppNotifications,
                        SettingId::LiveActivities,
                    ]
                ),
            ]
        );
        assert!(find(&view, SettingId::SnoozeLimitedThreads).resettable);
        assert!(!find(&view, SettingId::AutoResumeLimitedThreads).resettable);
        assert_eq!(
            find(&view, SettingId::AutoSettleDays).control,
            SettingControl::Number {
                value: 3,
                min: 1,
                max: 90
            }
        );
        assert!(matches!(&find(&view, SettingId::FollowUpBehavior).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "steer"));
        assert!(matches!(&find(&view, SettingId::TimeFormat).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "24-hour"));
        assert!(matches!(&find(&view, SettingId::DefaultWorkspace).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "worktree"));
        assert_eq!(
            find(&view, SettingId::DefaultModel).control,
            SettingControl::Model {
                model_label: "Choose model".into(),
                traits_label: None
            }
        );
    }

    #[test]
    fn each_control_value_becomes_its_intent() {
        let snapshot = Snapshot::default();
        let host = SettingsScope::Host;
        let on = SettingValue::Switch { on: true };
        same(
            setting_intent(&snapshot, &host, SettingId::AutoSettleInactiveThreads, &on),
            Some(Intent::UpdateSettings {
                scope: SettingsScope::Host,
                change: SettingChange::AutoSettle { days: Some(3) },
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &project("p"),
                SettingId::AutoSettleDays,
                &SettingValue::Number { value: 7 },
            ),
            Some(Intent::UpdateSettings {
                scope: project("p"),
                change: SettingChange::AutoSettle { days: Some(7) },
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::AutoSettleDays,
                &SettingValue::Number { value: 91 },
            ),
            None,
        );
        same(
            setting_intent(&snapshot, &host, SettingId::WorkingSection, &on),
            Some(Intent::SetWorkingSection { enabled: true }),
        );
        let choice = |id: &str| SettingValue::Choice { id: id.into() };
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::FollowUpBehavior,
                &choice("steer"),
            ),
            Some(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Steer,
            }),
        );
        same(
            setting_intent(&snapshot, &host, SettingId::TimeFormat, &choice("24-hour")),
            Some(Intent::SetTimestampFormat {
                format: TimestampFormat::TwentyFourHour,
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::DefaultPermissions,
                &choice("auto"),
            ),
            Some(Intent::UpdateSettings {
                scope: SettingsScope::Host,
                change: SettingChange::DefaultRuntimeMode {
                    mode: agent_domain::RuntimeMode::Auto,
                },
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::DefaultWorkspace,
                &choice("worktree"),
            ),
            Some(Intent::UpdateSettings {
                scope: SettingsScope::Host,
                change: SettingChange::DefaultThreadEnvMode {
                    mode: Some(agent_protocol::models::ThreadEnvMode::Worktree),
                },
            }),
        );
        same(
            setting_intent(&snapshot, &host, SettingId::TimeFormat, &on),
            None,
        );
    }

    #[test]
    fn a_reset_returns_a_host_row_to_its_default_and_a_project_row_to_the_host() {
        let snapshot = Snapshot {
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        let host = HostSettings {
            auto_settle: AutoSettle::Never,
            ..Default::default()
        };
        let view = settings_view(
            &snapshot,
            Some(&host),
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        same(
            setting_reset_intent(
                &SettingsScope::Host,
                &find(&view, SettingId::AutoSettleInactiveThreads),
            ),
            Some(Intent::UpdateSettings {
                scope: SettingsScope::Host,
                change: SettingChange::AutoSettle { days: Some(3) },
            }),
        );
        same(
            setting_reset_intent(
                &SettingsScope::Host,
                &find(&view, SettingId::FollowUpBehavior),
            ),
            Some(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Queue,
            }),
        );
        same(
            setting_reset_intent(&SettingsScope::Host, &find(&view, SettingId::TimeFormat)),
            None,
        );
        let overridden = host_with(
            "p",
            ProjectSettingsOverrides {
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let page = settings_view(
            &snapshot,
            Some(&overridden),
            &project("p"),
            TimestampFormat::Locale,
        );
        same(
            setting_reset_intent(&project("p"), &find(&page, SettingId::ContinueAfterRestart)),
            Some(Intent::UpdateSettings {
                scope: project("p"),
                change: SettingChange::Inherit {
                    key: ProjectSettingKey::ContinueAfterRestart,
                },
            }),
        );
        same(
            setting_reset_intent(
                &project("p"),
                &find(&page, SettingId::AutoSettleInactiveThreads),
            ),
            None,
        );
    }

    #[test]
    fn a_project_page_shows_effective_values_with_their_source() {
        let host = host_with(
            "first-project",
            ProjectSettingsOverrides {
                auto_settle: Some(AutoSettle::Never),
                ..Default::default()
            },
        );
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("first-project"),
            TimestampFormat::Locale,
        );
        assert_eq!(
            view.project,
            Some(ProjectOverridesHeader {
                project_id: "first-project".into(),
                label: "Unavailable project".into(),
                has_overrides: true,
            })
        );
        assert_eq!(
            ids(&view),
            [
                (
                    "auto-settle".into(),
                    vec![SettingId::AutoSettleInactiveThreads]
                ),
                ("behavior".into(), vec![SettingId::ContinueAfterRestart]),
                ("agent".into(), vec![SettingId::AgentBrowserAccess]),
                (
                    "maintenance".into(),
                    vec![SettingId::ResponseStreaming, SettingId::AutoSettleOnMerge]
                ),
                (
                    "source-control".into(),
                    vec![
                        SettingId::DefaultAutoPull,
                        SettingId::BranchNaming,
                        SettingId::PullRequestMergeMethod
                    ]
                ),
                (
                    "new-threads".into(),
                    vec![
                        SettingId::DefaultWorkspace,
                        SettingId::WorktreeSubmodules,
                        SettingId::DefaultPermissions,
                        SettingId::StartFromOrigin
                    ]
                ),
            ]
        );
        assert_eq!(
            view.sections[0].rows[0].source,
            Some(SettingSource::Project)
        );
        assert_eq!(
            view.sections[0].rows[0].control,
            SettingControl::Switch { on: false }
        );
        assert_eq!(view.sections[1].rows[0].source, Some(SettingSource::Host));
        let other = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("other"),
            TimestampFormat::Locale,
        );
        assert!(!other.project.unwrap().has_overrides);
    }
}
