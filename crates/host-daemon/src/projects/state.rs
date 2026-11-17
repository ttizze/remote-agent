use agent_protocol::models::{Project, ProjectMembership};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) projects: Vec<Project>,
    pub(super) resolved_roots: HashMap<String, PathBuf>,
    pub(super) worktree_roots: HashMap<String, String>,
    pub(crate) scratch_roots: BTreeSet<PathBuf>,
}

enum WorkspaceMatch<'a> {
    Project(&'a str),
    Missing,
    Ambiguous,
}

/// Include missing children, configured aliases and their physical targets.
/// Membership, discovery and SDK provenance recovery share this boundary.
pub(crate) fn scratch_root<'a>(roots: &'a BTreeSet<PathBuf>, path: &Path) -> Option<&'a Path> {
    let physical = dunce::canonicalize(path).ok();
    let path = if path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        physical.as_deref()?
    } else {
        path
    };
    roots
        .iter()
        .find(|root| {
            path.starts_with(root) || physical.as_ref().is_some_and(|path| path.starts_with(root))
        })
        .map(PathBuf::as_path)
}

impl Snapshot {
    pub(crate) fn project_membership(&self, cwd: Option<&str>) -> ProjectMembership {
        let Some(cwd) = cwd else {
            return ProjectMembership::Unknown {};
        };
        self.project_for_workspace(cwd)
            .map(|id| ProjectMembership::Assigned(id.to_owned()))
            .unwrap_or(ProjectMembership::Unassigned {})
    }
    pub(crate) fn project_for_workspace(&self, workspace: &str) -> Option<&str> {
        let workspace = Path::new(workspace);
        if !workspace.is_absolute() {
            return None;
        }
        if scratch_root(&self.scratch_roots, workspace).is_some() {
            return self
                .projects
                .iter()
                .any(|project| project.id == agent_protocol::models::SCRATCH_PROJECT_ID)
                .then_some(agent_protocol::models::SCRATCH_PROJECT_ID);
        }
        match self.match_workspace(workspace) {
            WorkspaceMatch::Project(id) => Some(id),
            WorkspaceMatch::Ambiguous => None,
            WorkspaceMatch::Missing => {
                // Registration stores a physical root, but providers can report
                // its alias (for example /tmp instead of /private/tmp on macOS).
                // Keep known and unavailable roots readable without filesystem I/O.
                let physical = dunce::canonicalize(workspace).ok()?;
                match self.match_workspace(&physical) {
                    WorkspaceMatch::Project(id) => Some(id),
                    WorkspaceMatch::Missing | WorkspaceMatch::Ambiguous => None,
                }
            }
        }
    }

    fn match_workspace(&self, workspace: &Path) -> WorkspaceMatch<'_> {
        let mapped = worktree_mapping(&self.worktree_roots, workspace)
            .map(|(root, source)| Path::new(source).join(workspace.strip_prefix(root).unwrap()));
        let workspace = mapped.as_deref().unwrap_or(workspace);
        let mut matched: Option<(&str, usize)> = None;
        let mut ambiguous = false;
        for project in &self.projects {
            for root in &project.roots {
                let configured = Path::new(&root.path);
                let root = self
                    .resolved_roots
                    .get(&root.path)
                    .filter(|resolved| workspace.starts_with(resolved))
                    .map(PathBuf::as_path)
                    .unwrap_or(configured);
                if !configured.is_absolute() || !workspace.starts_with(root) {
                    continue;
                }
                let depth = root.components().count();
                match matched {
                    Some((_, previous_depth)) if depth < previous_depth => {}
                    Some((previous_id, previous_depth)) if depth == previous_depth => {
                        ambiguous |= previous_id != project.id;
                    }
                    _ => {
                        matched = Some((&project.id, depth));
                        ambiguous = false;
                    }
                }
            }
        }
        if ambiguous {
            WorkspaceMatch::Ambiguous
        } else {
            matched.map_or(WorkspaceMatch::Missing, |(id, _)| {
                WorkspaceMatch::Project(id)
            })
        }
    }
}

pub(crate) fn worktree_mapping<'a>(
    roots: &'a HashMap<String, String>,
    workspace: &Path,
) -> Option<(&'a str, &'a str)> {
    roots
        .iter()
        .filter(|(root, _)| workspace.starts_with(root))
        .max_by_key(|(root, _)| root.len())
        .map(|(root, source)| (root.as_str(), source.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_path(path: &str) -> String {
        if cfg!(windows) && path.starts_with('/') {
            format!("C:{path}")
        } else {
            path.to_owned()
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            projects: serde_json::from_value(json!([
                {"id":"a","name":"A","roots":[{"path":fixture_path("/work/a")},{"path":fixture_path("/work/a-two")}]},
                {"id":"b","name":"B","roots":[{"path":fixture_path("/work/b")}]},
                {"id":"nested","name":"Nested","roots":[{"path":fixture_path("/work/a/packages/app")}]}
            ]))
            .unwrap(),
            ..Default::default()
        }
    }
    #[test]
    fn workspace_membership_respects_roots_worktrees_and_ambiguity() {
        let mut snapshot = snapshot();
        snapshot
            .worktree_roots
            .insert(fixture_path("/checkout"), fixture_path("/work/a"));
        for (cwd, expected) in [
            ("/work/a/src", Some("a")),
            ("/work/a-two", Some("a")),
            ("/work/another", None),
            ("work/a", None),
            ("/checkout", Some("a")),
            ("/checkout/packages/app", Some("nested")),
        ] {
            assert_eq!(snapshot.project_for_workspace(&fixture_path(cwd)), expected);
        }
        let mut duplicate = snapshot.projects[0].clone();
        duplicate.id = "duplicate".into();
        snapshot.projects.push(duplicate);
        assert_eq!(
            snapshot.project_for_workspace(&fixture_path("/work/a")),
            None
        );
        assert_eq!(
            snapshot.project_for_workspace(&fixture_path("/work/a/packages/app")),
            Some("nested")
        );
    }
    #[test]
    fn membership_uses_host_roots_and_keeps_standalone_chats_unassigned() {
        let mut snapshot = snapshot();
        snapshot
            .worktree_roots
            .insert(fixture_path("/checkout"), fixture_path("/work/a"));
        snapshot
            .scratch_roots
            .insert(fixture_path("/work/a/chats").into());
        for (cwd, expected) in [
            ("/work/a/src", ProjectMembership::Assigned("a".into())),
            (
                "/checkout/packages/app",
                ProjectMembership::Assigned("nested".into()),
            ),
            ("/work/a/chats", ProjectMembership::Unassigned {}),
            ("/outside", ProjectMembership::Unassigned {}),
        ] {
            assert_eq!(
                snapshot.project_membership(Some(&fixture_path(cwd))),
                expected
            );
        }
        assert_eq!(
            snapshot.project_membership(None),
            ProjectMembership::Unknown {}
        );
    }

    #[test]
    fn relative_roots_do_not_gain_project_membership() {
        let mut snapshot = snapshot();
        snapshot.projects = serde_json::from_value(
            json!([{"id":"relative","name":"Relative","roots":[{"path":"."}]}]),
        )
        .unwrap();
        snapshot
            .resolved_roots
            .insert(".".into(), fixture_path("/host/cwd").into());
        assert_eq!(
            snapshot.project_for_workspace(&fixture_path("/host/cwd")),
            None
        );
    }
}
