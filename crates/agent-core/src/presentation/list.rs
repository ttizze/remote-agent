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
                        preview: thread.preview.clone().unwrap_or_default(),
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
