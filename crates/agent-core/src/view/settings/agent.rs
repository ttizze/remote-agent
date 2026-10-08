//! Agent access and provider update checks.
use super::{
    ProjectSettingKey, SettingChange, SettingControl, SettingId, SettingSource, SettingValue,
    SettingsRow, SettingsScope,
    registry::{Context, Section},
    row, section, update,
};
use crate::models::HostSettings;

fn browser_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: on != HostSettings::default().enable_agent_browser_access,
        source,
        ..row(
            SettingId::AgentBrowserAccess,
            "Browser access",
            Some("Allow agents to use the Host browser when a task needs it."),
            SettingControl::Switch { on },
        )
    }
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::ProviderUpdateChecks,
        SettingId::AgentBrowserAccess,
    ],
    host: |context: &Context| {
        let host = context.host?;
        Some(section(
            "agent",
            "Agent",
            vec![
                SettingsRow {
                    resettable: !host.enable_provider_update_checks,
                    ..row(
                        SettingId::ProviderUpdateChecks,
                        "Provider update checks",
                        Some("Check configured provider CLIs and show an update notice."),
                        SettingControl::Switch {
                            on: host.enable_provider_update_checks,
                        },
                    )
                },
                browser_row(host.enable_agent_browser_access, None),
            ],
            Some(
                "Updates are reported as a notice; installation remains a deliberate user action.",
            ),
        ))
    },
    project: |resolved| {
        Some(section(
            "agent",
            "Agent",
            vec![browser_row(
                resolved.agent_browser_access.0,
                Some(resolved.agent_browser_access.1),
            )],
            None,
        ))
    },
    intent: |_, scope, id, value| match (id, value) {
        (SettingId::ProviderUpdateChecks, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::ProviderUpdateChecks { on: *on },
        )),
        (SettingId::AgentBrowserAccess, SettingValue::Switch { on }) => {
            Some(update(scope, SettingChange::AgentBrowserAccess { on: *on }))
        }
        _ => None,
    },
    reset: |id| match id {
        SettingId::ProviderUpdateChecks => Some(update(
            &SettingsScope::Host,
            SettingChange::ProviderUpdateChecks {
                on: HostSettings::default().enable_provider_update_checks,
            },
        )),
        SettingId::AgentBrowserAccess => Some(update(
            &SettingsScope::Host,
            SettingChange::AgentBrowserAccess {
                on: HostSettings::default().enable_agent_browser_access,
            },
        )),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::AgentBrowserAccess => Some(ProjectSettingKey::AgentBrowserAccess),
        _ => None,
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::Snapshot,
        view::{
            settings::{fixtures::find, settings_view},
            time::TimestampFormat,
        },
    };

    #[test]
    fn browser_access_is_project_scoped_but_update_checks_are_host_only() {
        let mut host = HostSettings::default();
        host.enable_agent_browser_access = false;
        host.project_overrides.insert(
            "p".into(),
            crate::models::ProjectSettingsOverrides {
                enable_agent_browser_access: Some(true),
                ..Default::default()
            },
        );
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &SettingsScope::Project {
                project_id: "p".into(),
            },
            TimestampFormat::Locale,
        );
        assert_eq!(
            find(&view, SettingId::AgentBrowserAccess).source,
            Some(SettingSource::Project)
        );
        assert!(
            view.sections
                .iter()
                .flat_map(|section| &section.rows)
                .all(|row| row.id != SettingId::ProviderUpdateChecks)
        );
    }
}
