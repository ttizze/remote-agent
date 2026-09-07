use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_PAGE_LIMIT: usize = 100;
const MAX_PAGE_LIMIT: usize = 512;
const CURSOR_PREFIX: &str = "desktop-projects:";

#[derive(Debug)]
pub(crate) enum Error {
    Invalid(serde_json::Error),
    InvalidCursor,
}

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    projects: Vec<DesktopProject>,
    assignments: HashMap<String, ProjectAssignment>,
    projectless_thread_ids: HashSet<String>,
    workspace_root_hints: HashMap<String, String>,
}

impl Snapshot {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let state: DesktopGlobalState = serde_json::from_slice(bytes).map_err(Error::Invalid)?;
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
            projects,
            assignments: state.thread_project_assignments,
            projectless_thread_ids: state.projectless_thread_ids.into_iter().collect(),
            workspace_root_hints: state.thread_workspace_root_hints,
        })
    }

    pub(crate) fn project_list(&self, params: &Value) -> Result<Value, Error> {
        let offset = params
            .get("cursor")
            .and_then(Value::as_str)
            .map(parse_cursor)
            .transpose()?
            .unwrap_or(0);
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(DEFAULT_PAGE_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT);
        let end = offset.saturating_add(limit).min(self.projects.len());
        let page = self
            .projects
            .get(offset..end)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(page_position, project)| {
                project.to_rpc_value(offset.saturating_add(page_position))
            })
            .collect::<Vec<_>>();
        let next_cursor = (end < self.projects.len()).then(|| format!("{CURSOR_PREFIX}{end}"));
        Ok(json!({
            "data": page,
            "nextCursor": next_cursor,
        }))
    }

    pub(crate) fn enrich_threads(&self, mut result: Value) -> Value {
        if let Some(data) = result.get_mut("data").and_then(Value::as_array_mut) {
            for thread in data {
                self.enrich_thread(thread);
            }
        }
        if let Some(thread) = result.get_mut("thread") {
            self.enrich_thread(thread);
        } else if result.get("id").is_some() {
            self.enrich_thread(&mut result);
        }
        result
    }

    fn enrich_thread(&self, thread: &mut Value) {
        let Some(thread_object) = thread.as_object_mut() else {
            return;
        };
        let Some(thread_id) = thread_object.get("id").and_then(Value::as_str) else {
            return;
        };
        // Explicit Desktop decisions override workspace matching, including
        // projectless threads whose cwd happens to be inside a project.
        if let Some(assignment) = self.assignments.get(thread_id) {
            thread_object.insert(
                "projectId".to_owned(),
                Value::String(assignment.project_id.clone()),
            );
        } else if self.projectless_thread_ids.contains(thread_id) {
            thread_object.insert("projectId".to_owned(), Value::Null);
        } else if thread_object.get("projectId").and_then(Value::as_str).is_none() {
            let project_id = self.workspace_root_hints.get(thread_id)
                .and_then(|root| self.project_for_workspace(root))
                .or_else(|| thread_object.get("cwd").and_then(Value::as_str)
                    .and_then(|cwd| self.project_for_workspace(cwd)));
            if let Some(project_id) = project_id {
                thread_object.insert("projectId".to_owned(), Value::String(project_id.to_owned()));
            }
        }
    }

    fn project_for_workspace(&self, workspace: &str) -> Option<&str> {
        let workspace = Path::new(workspace);
        if !workspace.is_absolute() { return None; }
        let mut matched: Option<(&str, usize)> = None;
        let mut ambiguous = false;
        for project in &self.projects {
            for root in &project.root_paths {
                let root = Path::new(root);
                if !root.is_absolute() || !workspace.starts_with(root) { continue; }
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
        if ambiguous { None } else { matched.map(|(id, _)| id) }
    }
}

fn parse_cursor(cursor: &str) -> Result<usize, Error> {
    cursor
        .strip_prefix(CURSOR_PREFIX)
        .and_then(|offset| offset.parse().ok())
        .ok_or(Error::InvalidCursor)
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
    fn to_rpc_value(&self, position: usize) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "roots": self.root_paths.iter().map(|path| json!({"path": path})).collect::<Vec<_>>(),
            "position": position,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
            "source": "codexDesktop",
        })
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
        let page = snapshot().project_list(&json!({"limit": 1})).unwrap();
        assert_eq!(
            page,
            json!({
                "data": [{
                    "id": "project-a",
                    "name": "A",
                    "roots": [{"path": "/work/a"}, {"path": "/work/a-two"}],
                    "position": 0,
                    "createdAt": 10,
                    "updatedAt": 11,
                    "source": "codexDesktop"
                }],
                "nextCursor": "desktop-projects:1"
            })
        );
        let second = snapshot()
            .project_list(&json!({"cursor": "desktop-projects:1", "limit": 1}))
            .unwrap();
        assert_eq!(second["data"][0]["id"], "project-b");
        assert!(second["nextCursor"].is_null());
    }

    #[test]
    fn clamps_page_limits_without_changing_global_positions() {
        let first = snapshot().project_list(&json!({"limit": 0})).unwrap();
        assert_eq!(first["data"].as_array().unwrap().len(), 1);
        assert_eq!(first["data"][0]["position"], 0);
        assert_eq!(first["nextCursor"], "desktop-projects:1");

        let all = snapshot().project_list(&json!({"limit": 9999})).unwrap();
        assert_eq!(all["data"].as_array().unwrap().len(), 2);
        assert!(all["nextCursor"].is_null());
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
        let unassigned = snapshot().enrich_threads(json!({"thread": {"id": "new-thread", "cwd": "/work/a"}}));
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
        let state = Snapshot::parse(br#"{"local-projects":{
            "a":{"id":"a","name":"A","rootPaths":["/work","/work/shared"]},
            "b":{"id":"b","name":"B","rootPaths":["/work/shared","/work/shared/nested"]}
        }}"#).unwrap();
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
    fn rejects_foreign_cursors() {
        assert!(matches!(
            snapshot().project_list(&json!({"cursor": "other:1"})),
            Err(Error::InvalidCursor)
        ));
    }
}
