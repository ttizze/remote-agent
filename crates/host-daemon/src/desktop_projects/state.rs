use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use agent_core::models::{Project, ProjectRoot, Thread};
use serde::Deserialize;

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) projects: Vec<Project>,
    pub(super) resolved_roots: HashMap<String, PathBuf>,
    assignments: HashMap<String, ProjectAssignment>,
    projectless_thread_ids: HashSet<String>,
    workspace_root_hints: HashMap<String, String>,
    pub(super) worktree_roots: HashMap<String, String>,
    pub(super) chat_directory: Option<PathBuf>,
}

impl Snapshot {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let state: DesktopGlobalState = serde_json::from_slice(bytes)?;
        let mut projects = state.local_projects.into_values().collect::<Vec<_>>();
        let positions = state
            .project_order
            .into_iter()
            .enumerate()
            .map(|(position, id)| (id, position))
            .collect::<HashMap<_, _>>();
        projects.sort_by(|left, right| {
            positions
                .get(&left.id)
                .copied()
                .unwrap_or(usize::MAX)
                .cmp(&positions.get(&right.id).copied().unwrap_or(usize::MAX))
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(Self {
            projects: projects
                .into_iter()
                .enumerate()
                .map(|(position, project)| project.into_model(position))
                .collect(),
            assignments: state.thread_project_assignments,
            resolved_roots: HashMap::new(),
            projectless_thread_ids: state.projectless_thread_ids.into_iter().collect(),
            workspace_root_hints: state.thread_workspace_root_hints,
            worktree_roots: HashMap::new(),
            chat_directory: None,
        })
    }

    pub(crate) fn enrich_thread(&self, thread: &mut Thread) {
        let Some(thread_id) = thread.id.as_deref() else {
            return;
        };
        // Explicit Desktop decisions override workspace matching, including
        // projectless threads whose cwd happens to be inside a project.
        if let Some(assignment) = self.assignments.get(thread_id) {
            thread.project_id = Some(Some(assignment.project_id.clone()));
        } else if self.projectless_thread_ids.contains(thread_id)
            || self.chat_directory.as_deref().is_some_and(|directory| {
                thread
                    .cwd
                    .as_deref()
                    .is_some_and(|cwd| Path::new(cwd) == directory)
            })
        {
            thread.project_id = Some(None);
        } else if thread
            .project_id
            .as_ref()
            .and_then(Option::as_ref)
            .is_none()
        {
            let project_id = self
                .workspace_root_hints
                .get(thread_id)
                .and_then(|root| self.project_for_workspace(root))
                .or_else(|| {
                    thread
                        .cwd
                        .as_deref()
                        .and_then(|cwd| self.project_for_workspace(cwd))
                });
            if let Some(project_id) = project_id {
                thread.project_id = Some(Some(project_id.to_owned()));
            }
        }
    }

    fn project_for_workspace(&self, workspace: &str) -> Option<&str> {
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
                if !root.is_absolute() || !workspace.starts_with(root) {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct DesktopGlobalState {
    #[serde(default)]
    local_projects: HashMap<String, DesktopProject>,
    #[serde(default)]
    project_order: Vec<String>,
    #[serde(default)]
    thread_project_assignments: HashMap<String, ProjectAssignment>,
    #[serde(default)]
    projectless_thread_ids: Vec<String>,
    #[serde(default)]
    thread_workspace_root_hints: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProject {
    id: String,
    name: String,
    #[serde(default)]
    root_paths: Vec<String>,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    updated_at: u64,
}

impl DesktopProject {
    fn into_model(self, position: usize) -> Project {
        Project {
            id: self.id,
            name: self.name,
            roots: self
                .root_paths
                .into_iter()
                .map(|path| ProjectRoot {
                    path,
                    extra: Default::default(),
                })
                .collect(),
            position: Some(position as u64),
            created_at: Some(self.created_at),
            updated_at: Some(self.updated_at),
            source: Some("codexDesktop".into()),
            extra: Default::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectAssignment {
    project_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    impl Snapshot {
        fn enrich_threads(&self, mut result: Value) -> Value {
            let enrich = |value: &mut Value| {
                let mut thread: Thread = serde_json::from_value(value.take()).unwrap();
                self.enrich_thread(&mut thread);
                *value = serde_json::to_value(thread).unwrap();
            };
            if let Some(data) = result.get_mut("data").and_then(Value::as_array_mut) {
                for thread in data {
                    enrich(thread);
                }
            } else if let Some(thread) = result.get_mut("thread") {
                enrich(thread);
            } else {
                enrich(&mut result);
            }
            result
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot::parse(
            br#"{
                "local-projects": {
                    "project-b": {
                        "id": "project-b",
                        "name": "B",
                        "rootPaths": ["/work/b"],
                        "createdAt": 20,
                        "updatedAt": 21
                    },
                    "project-a": {
                        "id": "project-a",
                        "name": "A",
                        "rootPaths": ["/work/a", "/work/a-two"],
                        "createdAt": 10,
                        "updatedAt": 11
                    }
                },
                "project-order": ["project-a", "project-b"],
                "thread-project-assignments": {
                    "assigned": {"projectKind": "local", "projectId": "project-a"},
                    "overlap": {"projectKind": "local", "projectId": "project-b"}
                },
                "projectless-thread-ids": ["projectless", "overlap"],
                "thread-workspace-root-hints": {"worktree-thread": "/work/b", "projectless": "/work/b"}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn lists_desktop_projects_in_desktop_order_with_roots() {
        let snapshot = snapshot();
        assert_eq!(snapshot.projects.len(), 2);
        assert_eq!(
            serde_json::to_value(&snapshot.projects[0]).unwrap(),
            json!({
                "id":"project-a", "name":"A", "roots":[{"path":"/work/a"},{"path":"/work/a-two"}],
                "position":0, "createdAt":10, "updatedAt":11, "source":"codexDesktop"
            })
        );
        assert_eq!(snapshot.projects[1].id, "project-b");
        assert_eq!(snapshot.projects[1].position, Some(1));
    }

    #[test]
    fn explicit_membership_wins_and_unassigned_threads_use_workspace_roots() {
        let result = snapshot().enrich_threads(json!({
            "data": [
                {"id": "assigned", "cwd": "/elsewhere", "projectId": null},
                {"id": "projectless", "cwd": "/work/a", "projectId": "upstream"},
                {"id": "overlap", "cwd": "/work/a"},
                {"id": "upstream-only", "cwd": "/work/a", "projectId": "upstream"},
                {"id": "workspace-thread", "cwd": "/work/a"}
            ]
        }));
        assert_eq!(result["data"][0]["projectId"], "project-a");
        assert!(result["data"][1]["projectId"].is_null());
        assert_eq!(result["data"][2]["projectId"], "project-b");
        assert_eq!(result["data"][3]["projectId"], "upstream");
        assert_eq!(result["data"][4]["projectId"], "project-a");
    }

    #[test]
    fn overlays_thread_read_and_start_response_shapes() {
        let read = snapshot().enrich_threads(json!({"thread": {"id": "assigned"}}));
        assert_eq!(read["thread"]["projectId"], "project-a");
        let start = snapshot().enrich_threads(json!({"id": "assigned"}));
        assert_eq!(start["projectId"], "project-a");
        let unassigned =
            snapshot().enrich_threads(json!({"thread": {"id": "new-thread", "cwd": "/work/a"}}));
        assert_eq!(unassigned["thread"]["projectId"], "project-a");
        let started = snapshot().enrich_threads(json!({"id": "new-thread", "cwd": "/work/a"}));
        assert_eq!(started["projectId"], "project-a");
    }

    #[test]
    fn workspace_membership_respects_boundaries_secondary_roots_and_worktree_hints() {
        let result = snapshot().enrich_threads(json!({"data": [
            {"id": "child", "cwd": "/work/a/src"},
            {"id": "secondary", "cwd": "/work/a-two/"},
            {"id": "neighbor", "cwd": "/work/another"},
            {"id": "relative", "cwd": "work/a"},
            {"id": "worktree-thread", "cwd": "/work/a"}
        ]}));
        assert_eq!(result["data"][0]["projectId"], "project-a");
        assert_eq!(result["data"][1]["projectId"], "project-a");
        assert!(result["data"][2].get("projectId").is_none());
        assert!(result["data"][3].get("projectId").is_none());
        assert_eq!(result["data"][4]["projectId"], "project-b");
    }

    #[test]
    fn workspace_membership_prefers_specific_roots_and_leaves_equal_matches_unassigned() {
        let state = Snapshot::parse(
            br#"{"local-projects":{
            "a":{"id":"a","name":"A","rootPaths":["/work","/work/shared"]},
            "b":{"id":"b","name":"B","rootPaths":["/work/shared","/work/shared/nested"]}
        }}"#,
        )
        .unwrap();
        let result = state.enrich_threads(json!({"data": [
            {"id":"unique", "cwd":"/work/other"},
            {"id":"ambiguous", "cwd":"/work/shared"},
            {"id":"specific", "cwd":"/work/shared/nested/src"}
        ]}));
        assert_eq!(result["data"][0]["projectId"], "a");
        assert!(result["data"][1].get("projectId").is_none());
        assert_eq!(result["data"][2]["projectId"], "b");
    }

    #[test]
    fn worktree_membership_preserves_nested_project_roots() {
        let mut snapshot = Snapshot::parse(
            br#"{
            "local-projects": {
                "repo": {"id":"repo","name":"Repo","rootPaths":["/repo"]},
                "app": {"id":"app","name":"App","rootPaths":["/repo/packages/app"]}
            }
        }"#,
        )
        .unwrap();
        snapshot
            .worktree_roots
            .insert("/repo/.git/bex-worktrees/session-a".into(), "/repo".into());
        let result = snapshot.enrich_threads(json!({"data":[
            {"id":"nested","cwd":"/repo/.git/bex-worktrees/session-a/packages/app"},
            {"id":"root","cwd":"/repo/.git/bex-worktrees/session-a"}
        ]}));
        assert_eq!(result["data"][0]["projectId"], "app");
        assert_eq!(result["data"][1]["projectId"], "repo");
    }
}
