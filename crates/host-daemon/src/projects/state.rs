use agent_core::models::{Project, ProjectMembership};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) projects: Vec<Project>,
    pub(super) resolved_roots: HashMap<String, PathBuf>,
    pub(super) worktree_roots: HashMap<String, String>,
    pub(super) chat_directory: Option<PathBuf>,
}
impl Snapshot {
    pub(crate) fn has_root(&self, path: &Path) -> bool {
        self.projects.iter().any(|project| {
            project.roots.iter().any(|root| {
                self.resolved_roots
                    .get(&root.path)
                    .map_or_else(|| Path::new(&root.path), PathBuf::as_path)
                    == path
            })
        })
    }
    pub(crate) fn project_for_directory(&self, cwd: Option<&str>) -> ProjectMembership {
        let Some(cwd) = cwd else {
            return ProjectMembership::Unknown {};
        };
        if self
            .chat_directory
            .as_deref()
            .is_some_and(|directory| Path::new(cwd) == directory)
        {
            ProjectMembership::Unassigned {}
        } else {
            self.project_for_workspace(cwd)
                .map(|id| ProjectMembership::Assigned(id.to_owned()))
                .unwrap_or_default()
        }
    }
    pub(crate) fn project_for_workspace(&self, workspace: &str) -> Option<&str> {
        let workspace = Path::new(workspace);
        let mapped = self
            .worktree_roots
            .iter()
            .filter(|(root, _)| workspace.starts_with(root))
            .max_by_key(|(root, _)| root.len())
            .map(|(root, source)| Path::new(source).join(workspace.strip_prefix(root).unwrap()));
        let workspace = mapped.as_deref().unwrap_or(workspace);
        if !workspace.is_absolute() {
            return None;
        }
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
            None
        } else {
            matched.map(|(id, _)| id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn snapshot() -> Snapshot {
        Snapshot {
            projects: serde_json::from_value(json!([
                {"id":"a","name":"A","roots":[{"path":"/work/a"},{"path":"/work/a-two"}]},
                {"id":"b","name":"B","roots":[{"path":"/work/b"}]},
                {"id":"nested","name":"Nested","roots":[{"path":"/work/a/packages/app"}]}
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
            .insert("/checkout".into(), "/work/a".into());
        for (cwd, expected) in [
            ("/work/a/src", Some("a")),
            ("/work/a-two", Some("a")),
            ("/work/another", None),
            ("work/a", None),
            ("/checkout", Some("a")),
            ("/checkout/packages/app", Some("nested")),
        ] {
            assert_eq!(snapshot.project_for_workspace(cwd), expected);
        }
        let mut duplicate = snapshot.projects[0].clone();
        duplicate.id = "duplicate".into();
        snapshot.projects.push(duplicate);
        assert_eq!(snapshot.project_for_workspace("/work/a"), None);
        assert_eq!(
            snapshot.project_for_workspace("/work/a/packages/app"),
            Some("nested")
        );
    }
    #[test]
    fn chat_scope_and_relative_roots_do_not_gain_project_membership() {
        let mut snapshot = snapshot();
        snapshot.chat_directory = Some("/work/a/chats".into());
        assert_eq!(
            snapshot.project_for_directory(Some("/work/a/chats")),
            ProjectMembership::Unassigned {}
        );
        snapshot.projects = serde_json::from_value(
            json!([{"id":"relative","name":"Relative","roots":[{"path":"."}]}]),
        )
        .unwrap();
        snapshot
            .resolved_roots
            .insert(".".into(), "/host/cwd".into());
        assert_eq!(snapshot.project_for_workspace("/host/cwd"), None);
    }
}
