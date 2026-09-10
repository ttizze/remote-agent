//! The visible list combines wire summaries with locally observed activity.
use crate::{models::Project, state::Snapshot};
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadSummary {
    pub id: String,
    pub name: String,
    pub preview: String,
    pub cwd: String,
    pub project_id: Option<String>,
    pub active: bool,
    pub unread: bool,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadList {
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<Project>,
    pub more_project_ids: Vec<String>,
    pub has_more_chats: bool,
    pub has_more_projects: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.threads.as_ref()?;
        Some(ThreadList {
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
                        name: thread.name.clone().unwrap_or_default(),
                        preview: thread
                            .extra
                            .get("preview")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        cwd: thread.cwd.clone().unwrap_or_default(),
                        project_id: thread.project_id.clone().flatten(),
                        active,
                        unread,
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
