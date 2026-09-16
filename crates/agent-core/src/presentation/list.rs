//! The visible list combines wire summaries with locally observed activity.
use crate::{models::Project, state::Snapshot};
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub active: bool,
    pub unread: bool,
    pub worktree_merged: bool,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadList {
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
        if thread.is_some_and(|thread| thread.project_id == Some(None)) {
            String::new()
        } else {
            self.navigation.cwd.clone()
        }
    }

    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.threads.as_ref()?;
        let mut notices = Vec::new();
        if let Some(errors) = list
            .extra
            .get("providerErrors")
            .and_then(serde_json::Value::as_object)
        {
            notices.push(format!("会話一覧は部分結果です（{}）。取得できない提供元の保存済み表示は最新とは限りません。", errors.keys().cloned().collect::<Vec<_>>().join("、")));
        }
        if !self.archived_scopes.is_empty() {
            notices.push("保存領域が変更されています。以前の下書き・未保存編集は保持しています。Hostの保存先設定を元に戻すと再び表示できます。".into());
        }
        Some(ThreadList {
            notice: (!notices.is_empty()).then(|| notices.join("\n")),
            threads: list
                .data
                .iter()
                .map(|thread| {
                    let id = thread.id.clone().unwrap_or_default();
                    let active = self.activity.active.get(&id).copied().unwrap_or_else(|| {
                        thread
                            .status
                            .as_ref()
                            .is_some_and(|status| status.kind == "active")
                    });
                    let unread = self.activity.unread.contains(&id);
                    ThreadSummary {
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
                            .to_owned()
                            + if thread.extra.get("listStale")
                                == Some(&serde_json::Value::Bool(true))
                            {
                                "（保存済み・未確認）"
                            } else {
                                ""
                            },
                        project_id: thread
                            .project_id
                            .clone()
                            .flatten()
                            .filter(|id| list.projects.iter().any(|project| &project.id == id)),
                        active,
                        unread,
                        worktree_merged: thread.worktree_merged.unwrap_or(false),
                    }
                })
                .collect(),
            projects: list.projects.clone(),
            more_project_ids: list.more_project_ids.clone(),
            has_more_chats: list.has_more_chats,
            has_more_projects: list.has_more_projects,
        })
    }
}

/// A newly created branch shares main's history without having merged any work.
/// Missing creation history cannot establish that work has been integrated.
pub fn worktree_branch_merged(head: &str, initial: Option<&str>, contained_in_main: bool) -> bool {
    contained_in_main && initial.is_some_and(|initial| initial != head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models,
        state::operations::{ListThreads, Operation},
    };
    use serde_json::json;

    #[test]
    fn unavailable_provider_keeps_explicitly_stale_cached_summaries() {
        let mut snapshot = Snapshot::default();
        let page = |data, errors| {
            serde_json::from_value(serde_json::json!({"data":data,"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false,"providerErrors":errors})).unwrap()
        };
        ListThreads::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"id":"native","name":"Cached"}]),
                serde_json::json!({}),
            ),
        );
        ListThreads::new(Default::default()).apply(
            &mut snapshot,
            page(
                serde_json::json!([{"id":"claude:uuid","name":"Available"}]),
                serde_json::json!({"codex":{"message":"offline"}}),
            ),
        );
        let list = snapshot.thread_list().unwrap();
        assert_eq!(list.threads.len(), 2);
        assert!(list.notice.unwrap().contains("部分結果"));
        assert!(
            list.threads
                .iter()
                .find(|thread| thread.id == "native")
                .unwrap()
                .title
                .contains("未確認")
        );
        assert!(snapshot.error.is_none());
    }
    #[test]
    fn merged_work_requires_a_changed_tip_contained_in_main() {
        assert!(!worktree_branch_merged("base", Some("base"), true));
        assert!(!worktree_branch_merged("work", Some("base"), false));
        assert!(worktree_branch_merged("work", Some("base"), true));
        assert!(!worktree_branch_merged("work", None, true));
    }

    #[test]
    fn list_preserves_merge_status_alongside_activity_after_serialization_and_refresh() {
        for active in [false, true] {
            for unread in [false, true] {
                for merged in [None, Some(false), Some(true)] {
                    let mut snapshot = Snapshot::default();
                    std::sync::Arc::make_mut(&mut snapshot.activity)
                        .active
                        .insert("task".into(), active);
                    if unread {
                        std::sync::Arc::make_mut(&mut snapshot.activity)
                            .unread
                            .insert("task".into());
                    }
                    let mut thread = json!({"id":"task","name":"Worktree task",
                        "status":{"type":if active { "active" } else { "idle" }}});
                    if let Some(merged) = merged {
                        thread["worktreeMerged"] = json!(merged);
                    }
                    let page: models::ThreadList = serde_json::from_value(json!({
                        "data":[thread], "projects":[], "moreProjectIds":[],
                        "hasMoreChats":false, "hasMoreProjects":false
                    }))
                    .unwrap();
                    ListThreads::new(Default::default()).apply(&mut snapshot, page);
                    let restored: Snapshot =
                        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
                    let rows = restored.thread_list().unwrap();
                    assert_eq!(rows.threads[0].worktree_merged, merged.unwrap_or(false));
                    assert_eq!(rows.threads[0].active, active);
                    assert_eq!(rows.threads[0].unread, unread);
                }
            }
        }
    }
}
