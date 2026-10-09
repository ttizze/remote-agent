//! The visible list combines wire summaries with locally observed activity.
use crate::{
    models::{Project, ProjectRoot, task_active, task_title},
    state::Snapshot,
};
use base64::Engine;
use std::collections::{BTreeMap, BTreeSet, HashSet};
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    pub icon_png: Option<Vec<u8>>,
    pub monogram: String,
    pub icon_color: u32,
    pub expanded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub threads: Vec<ThreadSummary>,
    pub has_more: bool,
}

fn project_summary(project: &Project) -> ProjectSummary {
    let name = project.name.trim();
    let mut words = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let monogram = words
        .next()
        .map(|word| {
            let first = word.chars().next().unwrap();
            let second = word
                .chars()
                .skip(1)
                .find(|c| c.is_numeric())
                .or_else(|| words.next_back().and_then(|last| last.chars().next()))
                .unwrap_or_else(|| word.chars().next_back().unwrap());
            format!("{first}{second}")
                .to_uppercase()
                .chars()
                .take(2)
                .collect()
        })
        .unwrap_or_else(|| "PR".into());
    const COLORS: [u32; 8] = [
        0x60a5fa, 0xa78bfa, 0xf472b6, 0xfb923c, 0xfbbf24, 0x4ade80, 0x2dd4bf, 0x38bdf8,
    ];
    let index = name
        .to_lowercase()
        .chars()
        .fold(0, |index, c| (index * 31 + c as usize) % COLORS.len());
    ProjectSummary {
        id: project.id.clone(),
        name: project.name.clone(),
        roots: project.roots.clone(),
        icon_png: project
            .favicon_png
            .as_ref()
            .and_then(|png| base64::engine::general_purpose::STANDARD.decode(png).ok()),
        monogram,
        icon_color: COLORS[index],
        expanded: false,
        loading: false,
        error: None,
        threads: Vec::new(),
        has_more: false,
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadSummary {
    pub id: crate::session::SessionRef,
    pub title: String,
    pub project_id: Option<String>,
    pub active: bool,
    pub unread: bool,
    pub worktree_status: Option<crate::models::WorktreeStatus>,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadList {
    pub loading_projects: bool,
    pub loading_chats: bool,
    pub notice: Option<String>,
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<ProjectSummary>,
    pub has_more: bool,
    pub has_more_projects: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// The selected folder is separate from the provider's execution directory.
    pub fn selected_directory(&self) -> String {
        let thread = self
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.thread_metadata(id));
        if thread.is_some_and(|thread| {
            thread.project_id == (crate::models::ProjectMembership::Unassigned {})
        }) {
            String::new()
        } else {
            self.navigation.cwd.clone()
        }
    }

    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.threads.as_ref()?;
        let project_ids: HashSet<_> = list
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect();
        let mut notices = Vec::new();
        if let Some(errors) = list.provider_errors.as_ref() {
            notices.push(format!("会話一覧は部分結果です（{}）。取得できない提供元の保存済み表示は最新とは限りません。", errors.keys().cloned().collect::<Vec<_>>().join("、")));
        }
        if !self.archived_scopes.is_empty() {
            notices.push("保存領域が変更されています。以前の下書き・未保存編集は保持しています。Hostの保存先設定を元に戻すと再び表示できます。".into());
        }
        let summaries = thread_summaries(
            &list.data,
            &self.activity.active,
            &self.activity.unread,
            &project_ids,
        )
        .into_iter()
        .filter(|thread| thread.project_id.is_none())
        .collect();
        Some(ThreadList {
            notice: (!notices.is_empty()).then(|| notices.join("\n")),
            threads: summaries,
            loading_projects: self.operation_running(
                crate::state::operations::OperationKey::SessionPage {
                    part: crate::models::ListPart::Projects,
                },
            ),
            loading_chats: self.operation_running(
                crate::state::operations::OperationKey::SessionPage {
                    part: crate::models::ListPart::Chats,
                },
            ),
            projects: list
                .projects
                .iter()
                .map(|project| {
                    let page = self.project_threads.get(&project.id);
                    let mut summary = project_summary(project);
                    summary.expanded = self.expanded_projects.contains_key(&project.id);
                    if summary.expanded && !self.connected && page.is_none() {
                        summary.error = Some("接続するとタスクを読み込めます".into());
                    }
                    if summary.expanded
                        && let Some(operation) = self.operations.get(
                            &crate::state::operations::OperationKey::ProjectList {
                                project_id: project.id.clone(),
                            },
                        )
                    {
                        match &operation.phase {
                            crate::state::operations::OperationPhase::Running => {
                                summary.loading = true
                            }
                            crate::state::operations::OperationPhase::Failed { message } => {
                                summary.error = Some(message.clone())
                            }
                        }
                    }
                    if summary.expanded
                        && let Some(page) = page
                    {
                        summary.threads = thread_summaries(
                            &page.data,
                            &self.activity.active,
                            &self.activity.unread,
                            &project_ids,
                        );
                        summary.has_more = page.has_more;
                        if let Some(errors) = &page.provider_errors {
                            summary.error = Some(format!(
                                "一部を取得できません（{}）",
                                errors.keys().cloned().collect::<Vec<_>>().join("、")
                            ));
                        }
                    }
                    summary
                })
                .collect(),
            has_more: list.has_more,
            has_more_projects: list.has_more_projects,
        })
    }
}

fn thread_summaries(
    data: &[crate::models::Thread],
    active: &BTreeMap<crate::session::SessionRef, bool>,
    unread: &BTreeSet<crate::session::SessionRef>,
    project_ids: &HashSet<&str>,
) -> Vec<ThreadSummary> {
    data.iter()
        .filter(|thread| thread.parent_id.is_none())
        .filter_map(|thread| {
            let id = thread.id.clone()?;
            let active = task_active(active.get(&id).copied(), thread.status);
            let unread = unread.contains(&id);
            Some(ThreadSummary {
                id,
                title: task_title(thread.name.as_deref(), thread.preview.as_deref()).to_owned()
                    + if thread.list_stale == Some(true) {
                        "（保存済み・未確認）"
                    } else {
                        ""
                    },
                project_id: thread
                    .project_id
                    .as_ref()
                    .filter(|id| project_ids.contains(id.as_str()))
                    .cloned(),
                active,
                unread,
                worktree_status: thread.worktree_status,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::operations::ListSessions;
    use crate::state::operations::Operation;
    use agent_protocol::models;
    use serde_json::json;
    use std::sync::Arc;

    #[rstest::rstest]
    #[case("remote-agent", "RA")]
    #[case("t3code", "T3")]
    #[case("Bex", "BX")]
    #[case("日本語プロジェクト", "日ト")]
    #[case("", "PR")]
    #[case(" !!! ", "PR")]
    fn project_names_receive_shared_monograms(#[case] name: &str, #[case] monogram: &str) {
        let project = Project {
            name: name.into(),
            ..Default::default()
        };
        let summary = project_summary(&project);
        assert_eq!(summary.monogram, monogram);
        assert!(summary.icon_png.is_none());
    }

    proptest::proptest! {
        #[test]
        fn project_identity_is_bounded_and_ignores_surrounding_whitespace(name in ".{0,80}") {
            let project = |name| Project { name, ..Default::default() };
            let plain = project_summary(&project(name.clone()));
            let padded = project_summary(&project(format!(" \t{name}\n ")));
            proptest::prop_assert_eq!(plain.monogram.chars().count(), 2);
            proptest::prop_assert_eq!(plain.monogram, padded.monogram);
            proptest::prop_assert_eq!(plain.icon_color, padded.icon_color);
        }
    }

    fn snapshot_list(data: serde_json::Value) -> Snapshot {
        Snapshot {
            threads: Some(std::sync::Arc::new(
                serde_json::from_value(json!({
                    "data":data,"projects":[{"id":"project","name":"Project","roots":[]}],
                    "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                .unwrap(),
            )),
            ..Default::default()
        }
    }

    #[test]
    fn conversation_list_excludes_subagents_and_project_tasks_at_root() {
        let snapshot = snapshot_list(json!([
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"}},
            {"id":{"provider":"claude","id":"child"}},
            {"id":{"provider":"codex","id":"root"},"projectId":"project"},
            {"id":{"provider":"codex","id":"fork"}}
        ]));
        let list = snapshot.thread_list().unwrap();
        let rows: Vec<_> = list
            .threads
            .iter()
            .map(|thread| (thread.id.provider, thread.id.id.as_str()))
            .collect();
        use crate::session::ProviderKind::{Claude, Codex};
        assert_eq!(rows, [(Claude, "child"), (Codex, "fork")]);
    }

    #[test]
    fn list_keeps_only_standalone_chats_at_root_and_exposes_known_membership() {
        let mut snapshot = Snapshot::default();
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data": [
                    {"id":{"provider":"codex","id":"assigned"}, "projectId":"known"},
                    {"id":{"provider":"codex","id":"missing"}, "projectId":"absent"},
                    {"id":{"provider":"codex","id":"chat"}, "projectId":null},
                    {"id":{"provider":"codex","id":"unknown"}}
                ],
                "projects":[{"id":"known", "name":"Project", "roots":[]}],
                 "hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
            .unwrap(),
        );
        let list = snapshot.thread_list().unwrap();
        let memberships: Vec<_> = list
            .threads
            .iter()
            .map(|thread| (thread.id.id.as_str(), thread.project_id.as_deref()))
            .collect();
        assert_eq!(
            memberships,
            vec![("missing", None), ("chat", None), ("unknown", None)]
        );
        assert_eq!(list.projects[0].id, "known");
        assert!(list.has_more);
        assert!(!list.has_more_projects);
    }

    #[test]
    fn returned_project_header_replaces_branding_while_preserving_loaded_tasks() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.expanded_projects).insert("project".into(), 5);
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"task"},"projectId":"project"}],
                "projects":[{"id":"project","name":"Old project","roots":[],"faviconPng":"BAUG"}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{"project":{"limit":5,"hasMore":false}},"limit":5
            }))
            .unwrap(),
        );
        for icon in [Some("AQID"), None] {
            ListSessions::new(Default::default()).apply(&mut snapshot, serde_json::from_value(json!({
                "data":[],
                "projects":[{"id":"project","name":"Renamed project","roots":[],"faviconPng":icon}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            })).unwrap());
            crate::state::operations::ReadListDecorations {
                scope: agent_protocol::operations::ListDecorationScope::Root { part: None },
                project_ids: vec!["project".into()],
                threads: vec![],
            }
            .apply(
                &mut snapshot,
                agent_protocol::operations::ListDecorations {
                    icons: [("project".into(), icon.map(str::to_owned))].into(),
                    statuses: vec![],
                },
            );
            let list = snapshot.thread_list().unwrap();
            let project = &list.projects[0];
            assert_eq!(project.name, "Renamed project");
            assert_eq!(project.icon_png, icon.map(|_| vec![1, 2, 3]));
            assert_eq!(project.threads[0].id.id, "task");
        }
    }

    #[test]
    fn unavailable_provider_keeps_explicitly_stale_cached_summaries() {
        let mut snapshot = Snapshot::default();
        let page = |data, errors| {
            serde_json::from_value(serde_json::json!({"data":data,"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,"providerErrors":errors})).unwrap()
        };
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"id":{"provider":"codex","id":"native"},"name":"Cached"}]),
                serde_json::json!({}),
            ),
        );
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"id":{"provider":"claude","id":"uuid"},"name":"Available"}]),
                serde_json::json!({"codex":{"message":"offline"}}),
            ),
        );
        let list = snapshot.thread_list().unwrap();
        assert_eq!(list.threads.len(), 2);
        assert!(list.notice.unwrap().contains("部分結果"));
        assert!(
            list.threads
                .iter()
                .find(|thread| thread.id
                    == agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "native".into()
                    })
                .unwrap()
                .title
                .contains("未確認")
        );
        assert!(snapshot.error.is_none());
    }
    #[rstest::rstest]
    fn list_preserves_worktree_status_alongside_activity_after_serialization_and_refresh(
        #[values(false, true)] active: bool,
        #[values(false, true)] unread: bool,
        #[values(
            None,
            Some(models::WorktreeStatus::Unmerged),
            Some(models::WorktreeStatus::Merged)
        )]
        status: Option<models::WorktreeStatus>,
    ) {
        let mut snapshot = Snapshot::default();
        std::sync::Arc::make_mut(&mut snapshot.activity)
            .active
            .insert(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "task".into(),
                },
                active,
            );
        if unread {
            std::sync::Arc::make_mut(&mut snapshot.activity)
                .unread
                .insert(agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "task".into(),
                });
        }
        let mut thread = json!({"id":{"provider":"codex","id":"task"},"name":"Worktree task",
            "status":if active {"running"} else {"idle"}});
        if let Some(status) = status {
            thread["worktreeStatus"] = json!(status);
        }
        let page: models::ThreadList = serde_json::from_value(json!({
            "data":[thread], "projects":[],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
        .unwrap();
        ListSessions::new(Default::default()).apply(&mut snapshot, page);
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let rows = restored.thread_list().unwrap();
        assert_eq!(rows.threads[0].worktree_status, status);
        assert_eq!(rows.threads[0].active, active);
        assert_eq!(rows.threads[0].unread, unread);
    }
}
