//! The mobile "Choose project" screen: "No project", the projects by their
//! latest thread activity, and what shows when there are none or none match.
use super::selection::{ProjectScope, filter_project_scopes};
use crate::models::Project;
use crate::state::{CHATS_PROJECT, Snapshot};
use crate::view::thread_sort::{ThreadSortOrder, thread_sort_timestamp};
use crate::view::thread_summary::ThreadSummary;
use std::cmp::Reverse;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectPickerRow {
    pub project_id: String,
    pub title: String,
    /// The workspace path, or how many workspaces the project has.
    pub subtitle: String,
}

/// The screen without projects to list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectPickerEmpty {
    pub title: String,
    pub detail: String,
    pub loading: bool,
    /// "Add new project"; otherwise "Add environment".
    pub add_project: bool,
    /// "Start without a project".
    pub start_without_project: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectPickerView {
    /// The "No project" row above the projects.
    pub no_project: bool,
    /// The header's "Add project" button.
    pub can_add_project: bool,
    pub rows: Vec<ProjectPickerRow>,
    pub empty: Option<ProjectPickerEmpty>,
    /// The search matched nothing: "No matching projects".
    pub no_matches: bool,
}

fn project_freshness(project: &Project) -> i64 {
    project
        .updated_at
        .as_ref()
        .or(project.created_at.as_ref())
        .map_or(i64::MIN, agent_domain::Timestamp::millis)
}

/// The listed projects, one scope each, newest thread activity first, then
/// the freshest project, then title and key. Threads without a project are
/// reached through "No project" instead.
pub fn project_scopes(projects: &[Project], threads: &[ThreadSummary]) -> Vec<ProjectScope> {
    let mut scopes: Vec<(i64, ProjectScope)> = projects
        .iter()
        .filter(|project| project.id != CHATS_PROJECT)
        .map(|project| {
            let activity = threads
                .iter()
                .filter(|thread| thread.project == project.id && thread.archived_at.is_none())
                .map(|thread| thread_sort_timestamp(thread, ThreadSortOrder::UpdatedAt))
                .max()
                .unwrap_or_else(|| project_freshness(project));
            let scope = ProjectScope {
                key: project.id.clone(),
                title: project.name.clone(),
                projects: vec![project.clone()],
            };
            (activity, scope)
        })
        .collect();
    scopes.sort_by(|(left_at, left), (right_at, right)| {
        (Reverse(left_at), &left.title, &left.key).cmp(&(Reverse(right_at), &right.title, &right.key))
    });
    scopes.into_iter().map(|(_, scope)| scope).collect()
}

fn empty_state(snapshot: &Snapshot, can_start_without_project: bool) -> ProjectPickerEmpty {
    let loaded = snapshot.shell.snapshot.is_some();
    let (title, detail, loading) = match (&snapshot.error, loaded) {
        (Some(error), false) if !snapshot.connected => {
            ("Environment unavailable", error.clone(), false)
        }
        (_, false) => (
            "Connecting to environment",
            "Loading projects from the saved environment.".into(),
            true,
        ),
        (_, true) => (
            "No projects found",
            "The connected environment did not report any projects.".into(),
            false,
        ),
    };
    ProjectPickerEmpty {
        title: title.into(),
        detail,
        loading,
        add_project: snapshot.connected,
        start_without_project: can_start_without_project,
    }
}

/// The screen for the search text `query`.
pub fn project_picker(snapshot: &Snapshot, query: &str) -> ProjectPickerView {
    let threads: Vec<ThreadSummary> = snapshot
        .shell
        .snapshot
        .as_ref()
        .map(|shell| shell.threads.iter().map(ThreadSummary::from_shell).collect())
        .unwrap_or_default();
    let projects = snapshot.shell_projects();
    let scopes = project_scopes(projects, &threads);
    let can_start_without_project =
        snapshot.connected && projects.iter().any(|project| project.id == CHATS_PROJECT);
    let visible = filter_project_scopes(&scopes, query);
    ProjectPickerView {
        no_project: can_start_without_project && !scopes.is_empty(),
        can_add_project: snapshot.connected,
        no_matches: !scopes.is_empty() && visible.is_empty(),
        empty: scopes
            .is_empty()
            .then(|| empty_state(snapshot, can_start_without_project)),
        rows: visible
            .iter()
            .filter_map(|scope| {
                let project = scope.representative()?;
                Some(ProjectPickerRow {
                    project_id: project.id.clone(),
                    title: scope.title.clone(),
                    subtitle: if scope.projects.len() > 1 {
                        format!("{} workspaces", scope.projects.len())
                    } else {
                        project
                            .roots
                            .first()
                            .map(|root| root.path.clone())
                            .unwrap_or_default()
                    },
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::paths::fixtures::project;
    use super::*;
    use crate::view::thread_summary::fixtures::{ms, summary};

    fn thread(id: &str, project: &str, updated: &str) -> ThreadSummary {
        ThreadSummary {
            project: project.into(),
            updated_at: ms(updated),
            ..summary(id)
        }
    }

    fn ids(scopes: &[ProjectScope]) -> Vec<&str> {
        scopes.iter().map(|scope| scope.key.as_str()).collect()
    }

    #[test]
    fn sorts_project_scopes_by_their_thread_activity() {
        let projects = [
            project("project-newer", "/workspaces/newer"),
            project("project-older", "/workspaces/older"),
        ];
        let threads = [
            thread(
                "thread-older-project",
                "project-older",
                "2026-06-03T00:00:00.000Z",
            ),
            thread(
                "thread-newer-project",
                "project-newer",
                "2026-06-02T00:00:00.000Z",
            ),
        ];
        assert_eq!(
            ids(&project_scopes(&projects, &threads)),
            ["project-older", "project-newer"]
        );
    }

    #[test]
    fn sorts_projects_without_a_timestamp_after_dated_ones() {
        let undated = Project {
            name: "A undated".into(),
            ..project("project-invalid", "/a")
        };
        let dated = Project {
            name: "Z dated".into(),
            created_at: Some(agent_domain::Timestamp::parse("2026-06-02T00:00:00.000Z").unwrap()),
            ..project("project-valid", "/z")
        };
        assert_eq!(
            ids(&project_scopes(&[undated, dated], &[])),
            ["project-valid", "project-invalid"]
        );
    }

    #[test]
    fn leaves_threads_without_a_project_to_the_no_project_row() {
        let projects = [project(CHATS_PROJECT, "/chats"), project("app", "/work/app")];
        assert_eq!(ids(&project_scopes(&projects, &[])), ["app"]);
    }
}
