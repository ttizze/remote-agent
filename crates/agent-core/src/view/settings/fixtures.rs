//! Test helpers the section tests share.
use super::{SettingId, SettingsRow, SettingsScope, SettingsView};
use crate::{
    models::{HostSettings, ProjectSettingsOverrides},
    state::Intent,
};

pub(super) fn host_with(project: &str, overrides: ProjectSettingsOverrides) -> HostSettings {
    let mut host = HostSettings::default();
    host.project_overrides.insert(project.into(), overrides);
    host
}

pub(super) fn project(id: &str) -> SettingsScope {
    SettingsScope::Project {
        project_id: id.into(),
    }
}

pub(super) fn same(left: Option<Intent>, right: Option<Intent>) {
    assert_eq!(format!("{left:?}"), format!("{right:?}"));
}

/// Each section's id with the ids of its rows.
pub(super) fn ids(view: &SettingsView) -> Vec<(String, Vec<SettingId>)> {
    view.sections
        .iter()
        .map(|section| {
            (
                section.id.clone(),
                section.rows.iter().map(|row| row.id).collect(),
            )
        })
        .collect()
}

pub(super) fn find(view: &SettingsView, id: SettingId) -> SettingsRow {
    view.sections
        .iter()
        .flat_map(|section| &section.rows)
        .find(|row| row.id == id)
        .cloned()
        .unwrap()
}
