//! The visible list combines wire summaries with locally observed activity.
use crate::{models::Project, state::Snapshot};
use std::collections::HashSet;
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
    pub importing: bool,
    pub notice: Option<String>,
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<Project>,
    pub more_project_ids: Vec<String>,
    pub has_more_chats: bool,
    pub has_more_projects: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// The selected folder is separate from the provider's execution directory.
    pub fn selected_directory(&self) -> String {
        let thread = self.navigation.thread_id.as_ref().and_then(|id| {
            self.conversations.get(id).map(AsRef::as_ref).or_else(|| {
                self.threads
                    .as_ref()?
                    .data
                    .iter()
                    .find(|thread| thread.id.as_ref() == Some(id))
            })
        });
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
        if list.importing {
            notices.push("既存の会話を探しています。見つかった会話から表示します。".into());
        }
        if let Some(errors) = list.provider_errors.as_ref() {
            notices.push(format!(
                "利用できない提供元があります（{}）。保存済みの会話は引き続き表示できます。",
                errors.keys().cloned().collect::<Vec<_>>().join("、")
            ));
        }
        if !self.archived_scopes.is_empty() {
            notices.push("保存領域が変更されています。以前の下書き・未保存編集は保持しています。Hostの保存先設定を元に戻すと再び表示できます。".into());
        }
        Some(ThreadList {
            importing: list.importing,
            notice: (!notices.is_empty()).then(|| notices.join("\n")),
            threads: list
                .data
                .iter()
                .filter_map(|thread| {
                    let id = thread.id.clone()?;
                    let active =
                        self.activity.active.get(&id).copied().unwrap_or_else(|| {
                            thread.status == crate::models::SessionStatus::Running
                        });
                    let unread = self.activity.unread.contains(&id);
                    Some(ThreadSummary {
                        id,
                        title: thread
                            .name
                            .as_deref()
                            .filter(|name| !name.is_empty())
                            .or_else(|| {
                                thread
                                    .preview
                                    .as_deref()
                                    .filter(|preview| !preview.is_empty())
                            })
                            .unwrap_or("無題のタスク")
                            .to_owned(),
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
                .collect(),
            projects: list.projects.clone(),
            more_project_ids: list.more_project_ids.clone(),
            has_more_chats: list.has_more_chats,
            has_more_projects: list.has_more_projects,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::operations::ListSessions;
    use crate::state::operations::Operation;
    use agent_protocol::models;
    use serde_json::json;

    #[test]
    fn titles_use_a_nonempty_name_then_preview_then_the_untitled_label() {
        let mut snapshot = Snapshot::default();
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data": [
                    {"provider":"codex","id":{"id":"named"},"name":"Name","preview":"Preview"},
                    {"provider":"codex","id":{"id":"preview"},"name":"","preview":"First prompt"},
                    {"provider":"claude","id":{"id":"empty"},"name":"","preview":""},
                    {"provider":"claude","id":{"id":"missing"}}
                ],
                "projects":[], "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false
            }))
            .unwrap(),
        );
        let list = snapshot.thread_list().unwrap();
        assert_eq!(
            list.threads
                .iter()
                .map(|thread| thread.title.as_str())
                .collect::<Vec<_>>(),
            ["Name", "First prompt", "無題のタスク", "無題のタスク"]
        );
    }

    #[test]
    fn importing_lists_keep_progress_separate_from_pagination_and_provider_errors() {
        let mut snapshot = Snapshot::default();
        let mut page: models::ThreadList = serde_json::from_value(json!({
            "data":[], "projects":[], "moreProjectIds":[],
            "hasMoreChats":false, "hasMoreProjects":false, "importing":true,
            "providerErrors":{"codex":{"message":"unavailable"}}
        }))
        .unwrap();
        ListSessions::new(Default::default()).apply(&mut snapshot, page.clone());
        let list = snapshot.thread_list().unwrap();
        assert!(list.importing);
        assert!(!list.has_more_chats && !list.has_more_projects);
        let notice = list.notice.unwrap();
        assert!(notice.contains("探しています") && notice.contains("利用できない提供元"));
        page.importing = false;
        page.provider_errors = None;
        ListSessions::new(Default::default()).apply(&mut snapshot, page);
        let list = snapshot.thread_list().unwrap();
        assert!(!list.importing);
        assert!(list.notice.is_none());
    }

    #[test]
    fn list_preserves_order_and_only_exposes_known_project_membership() {
        let mut snapshot = Snapshot::default();
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data": [
                    {"provider":"codex","id":{"id":"assigned"}, "projectId":"known"},
                    {"provider":"codex","id":{"id":"missing"}, "projectId":"absent"},
                    {"provider":"codex","id":{"id":"chat"}, "projectId":null},
                    {"provider":"codex","id":{"id":"unknown"}}
                ],
                "projects":[{"id":"known", "name":"Project", "roots":[]}],
                "moreProjectIds":["known"], "hasMoreChats":true, "hasMoreProjects":true
            }))
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
            vec![
                ("assigned", Some("known")),
                ("missing", None),
                ("chat", None),
                ("unknown", None)
            ]
        );
        assert_eq!(list.projects[0].id, "known");
        assert_eq!(list.more_project_ids, vec!["known"]);
        assert!(list.has_more_chats && list.has_more_projects);
    }

    #[test]
    fn host_results_replace_cached_lists_even_when_a_provider_is_unavailable() {
        let mut snapshot = Snapshot::default();
        let page = |data, errors| {
            serde_json::from_value(serde_json::json!({"data":data,"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false,"providerErrors":errors})).unwrap()
        };
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"provider":"codex","id":{"id":"native"},"name":"Cached"}]),
                serde_json::json!({}),
            ),
        );
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"provider":"claude","id":{"id":"uuid"},"name":"Available"}]),
                serde_json::json!({"codex":{"message":"offline"}}),
            ),
        );
        let list = snapshot.thread_list().unwrap();
        assert_eq!(list.threads.len(), 1);
        assert_eq!(list.threads[0].title, "Available");
        assert_eq!(list.threads[0].id.id, "uuid");
        assert!(list.notice.unwrap().contains("利用できない提供元"));
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
                agent_protocol::session::SessionRef { id: "task".into() },
                active,
            );
        if unread {
            std::sync::Arc::make_mut(&mut snapshot.activity)
                .unread
                .insert(agent_protocol::session::SessionRef { id: "task".into() });
        }
        let mut thread = json!({"provider":"codex","id":{"id":"task"},"name":"Worktree task",
            "status":if active {"running"} else {"idle"}});
        if let Some(status) = status {
            thread["worktreeStatus"] = json!(status);
        }
        let page: models::ThreadList = serde_json::from_value(json!({
            "data":[thread], "projects":[], "moreProjectIds":[],
            "hasMoreChats":false, "hasMoreProjects":false
        }))
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
