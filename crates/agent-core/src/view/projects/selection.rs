//! Choosing the project of a new task, and the checks before it starts.
use crate::models::Project;

/// One logical project in the picker; clones of a repository share a scope.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectScope {
    pub key: String,
    pub title: String,
    /// The first project represents the scope.
    pub projects: Vec<Project>,
}

impl ProjectScope {
    pub fn representative(&self) -> Option<&Project> {
        self.projects.first()
    }
}

/// Scopes whose name, or any of whose projects' name or root, contains the query.
pub fn filter_project_scopes(scopes: &[ProjectScope], search_text: &str) -> Vec<ProjectScope> {
    let query = search_text.trim().to_lowercase();
    if query.is_empty() {
        return scopes.to_vec();
    }
    let matches = |text: &str| text.to_lowercase().contains(&query);
    scopes
        .iter()
        .filter(|scope| {
            matches(&scope.title)
                || scope.projects.iter().any(|project| {
                    matches(&project.name) || project.roots.iter().any(|root| matches(&root.path))
                })
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DraftProjectSelection {
    /// The draft keeps the project it has.
    Preserve,
    /// The only project is chosen without asking.
    Select { project_id: String },
    /// The user picks a project.
    Pick,
}

pub fn resolve_draft_project_selection(
    selected_project_id: Option<&str>,
    projects: &[Project],
    scopes: &[ProjectScope],
) -> DraftProjectSelection {
    if selected_project_id.is_some_and(|id| projects.iter().any(|project| project.id == id)) {
        return DraftProjectSelection::Preserve;
    }
    match scopes {
        [only] => only
            .representative()
            .map_or(DraftProjectSelection::Pick, |project| {
                DraftProjectSelection::Select {
                    project_id: project.id.clone(),
                }
            }),
        _ => DraftProjectSelection::Pick,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadWorkspaceMode {
    /// The project's checkout.
    Local,
    /// A new worktree from a base branch.
    Worktree,
}

/// The branch a new thread records: the picked branch, else the checked-out
/// one for a local thread. A worktree never borrows the current checkout.
pub fn resolve_project_thread_creation_branch(
    workspace_mode: ThreadWorkspaceMode,
    selected_branch: Option<&str>,
    current_checkout_branch: Option<&str>,
) -> Option<String> {
    selected_branch
        .or(match workspace_mode {
            ThreadWorkspaceMode::Local => current_checkout_branch,
            ThreadWorkspaceMode::Worktree => None,
        })
        .map(str::to_owned)
}

/// Why a new task cannot start yet.
pub fn validate_project_thread_creation(
    workspace_mode: ThreadWorkspaceMode,
    branch: Option<&str>,
    initial_message_text: &str,
) -> Option<String> {
    if initial_message_text.trim().is_empty() {
        return Some("Enter a task before starting the thread.".into());
    }
    if workspace_mode == ThreadWorkspaceMode::Worktree && branch.is_none_or(str::is_empty) {
        return Some("Select a base branch before creating a worktree.".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::paths::fixtures::project;
    use super::*;

    fn named(id: &str, name: &str, root: &str) -> Project {
        Project {
            name: name.into(),
            ..project(id, root)
        }
    }

    fn scope(key: &str, title: &str, projects: Vec<Project>) -> ProjectScope {
        ProjectScope {
            key: key.into(),
            title: title.into(),
            projects,
        }
    }

    fn ids(scopes: &[ProjectScope]) -> Vec<&str> {
        scopes.iter().map(|scope| scope.key.as_str()).collect()
    }

    #[test]
    fn preserves_an_explicit_project_selection() {
        let app = project("app", "/work/app");
        assert_eq!(
            resolve_draft_project_selection(
                Some("app"),
                std::slice::from_ref(&app),
                &[scope("app", "app", vec![app.clone()])]
            ),
            DraftProjectSelection::Preserve
        );
    }

    #[test]
    fn selects_the_only_physical_project_when_no_project_was_explicitly_selected() {
        let app = project("app", "/work/app");
        assert_eq!(
            resolve_draft_project_selection(
                None,
                std::slice::from_ref(&app),
                &[scope("app", "app", vec![app.clone()])]
            ),
            DraftProjectSelection::Select {
                project_id: "app".into()
            }
        );
    }

    #[test]
    fn selects_one_logical_project_even_when_it_has_multiple_physical_workspaces() {
        let projects = vec![
            project("app", "/work/app"),
            project("app-2", "/work/app-2"),
            project("app-3", "/work/app-3"),
        ];
        assert_eq!(
            resolve_draft_project_selection(
                None,
                &projects,
                &[scope("app", "app", projects.clone())]
            ),
            DraftProjectSelection::Select {
                project_id: "app".into()
            }
        );
    }

    #[test]
    fn does_not_preserve_a_project_key_that_is_missing_from_the_catalog() {
        let app = project("app", "/work/app");
        assert_eq!(
            resolve_draft_project_selection(
                Some("removed"),
                std::slice::from_ref(&app),
                &[scope("app", "app", vec![app.clone()])]
            ),
            DraftProjectSelection::Select {
                project_id: "app".into()
            }
        );
    }

    #[test]
    fn asks_when_there_are_several_logical_projects() {
        let projects = vec![project("app", "/work/app"), project("docs", "/work/docs")];
        let scopes = [
            scope("app", "app", vec![projects[0].clone()]),
            scope("docs", "docs", vec![projects[1].clone()]),
        ];
        assert_eq!(
            resolve_draft_project_selection(None, &projects, &scopes),
            DraftProjectSelection::Pick
        );
    }

    fn catalog() -> Vec<ProjectScope> {
        let desktop = named("code", "Desktop checkout", "/work/code");
        let server = named("remote-code", "remote-code", "/srv/remote-workspace");
        vec![
            scope("github.com/acme/code", "Acme Code", vec![desktop, server]),
            scope("docs", "Documentation", vec![project("docs", "/work/docs")]),
        ]
    }

    #[test]
    fn keeps_all_projects_for_an_empty_or_whitespace_only_query() {
        let scopes = catalog();
        assert_eq!(filter_project_scopes(&scopes, ""), scopes);
        assert_eq!(filter_project_scopes(&scopes, "  "), scopes);
    }

    #[test]
    fn matches_logical_names_and_workspace_names_or_paths_without_case_sensitivity() {
        let scopes = catalog();
        assert_eq!(
            ids(&filter_project_scopes(&scopes, "  ACME CODE ")),
            ["github.com/acme/code"]
        );
        assert_eq!(
            ids(&filter_project_scopes(&scopes, "DESKTOP")),
            ["github.com/acme/code"]
        );
        assert_eq!(
            ids(&filter_project_scopes(&scopes, "REMOTE-WORKSPACE")),
            ["github.com/acme/code"]
        );
        assert_eq!(
            ids(&filter_project_scopes(&scopes, "documentation")),
            ["docs"]
        );
        assert!(filter_project_scopes(&scopes, "missing-project").is_empty());
    }

    #[test]
    fn preserves_the_whole_logical_project_when_a_workspace_matches() {
        let scopes = catalog();
        let matches = filter_project_scopes(&scopes, "REMOTE-WORKSPACE");
        assert_eq!(matches[0], scopes[0]);
        assert_eq!(
            matches[0].representative().map(|p| p.id.as_str()),
            Some("code")
        );
    }

    #[test]
    fn uses_the_live_checkout_for_an_untouched_local_draft_label_and_recorded_branch() {
        assert_eq!(
            resolve_project_thread_creation_branch(
                ThreadWorkspaceMode::Local,
                None,
                Some("feature/x")
            )
            .as_deref(),
            Some("feature/x")
        );
    }

    #[test]
    fn prefers_an_explicit_picker_choice_over_the_current_checkout() {
        assert_eq!(
            resolve_project_thread_creation_branch(
                ThreadWorkspaceMode::Local,
                Some("main"),
                Some("feature/x")
            )
            .as_deref(),
            Some("main")
        );
    }

    #[test]
    fn stays_none_when_no_ref_is_checked_out() {
        assert_eq!(
            resolve_project_thread_creation_branch(ThreadWorkspaceMode::Local, None, None),
            None
        );
    }

    #[test]
    fn never_borrows_the_current_checkout_for_a_worktree_draft() {
        assert_eq!(
            resolve_project_thread_creation_branch(
                ThreadWorkspaceMode::Worktree,
                None,
                Some("feature/x")
            ),
            None
        );
    }

    #[test]
    fn keeps_the_explicit_base_branch_for_a_worktree_draft() {
        assert_eq!(
            resolve_project_thread_creation_branch(
                ThreadWorkspaceMode::Worktree,
                Some("main"),
                Some("feature/x")
            )
            .as_deref(),
            Some("main")
        );
    }

    #[test]
    fn a_task_needs_text_and_a_worktree_needs_a_base_branch() {
        assert_eq!(
            validate_project_thread_creation(ThreadWorkspaceMode::Local, None, "  "),
            Some("Enter a task before starting the thread.".into())
        );
        assert_eq!(
            validate_project_thread_creation(ThreadWorkspaceMode::Worktree, Some(""), "Fix it"),
            Some("Select a base branch before creating a worktree.".into())
        );
        assert_eq!(
            validate_project_thread_creation(ThreadWorkspaceMode::Worktree, Some("main"), "Fix it"),
            None
        );
        assert_eq!(
            validate_project_thread_creation(ThreadWorkspaceMode::Local, None, "Fix it"),
            None
        );
    }
}
