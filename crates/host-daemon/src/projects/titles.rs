use agent_protocol::models::{Project, Thread, ThreadList};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) enum TitleQuery<'a> {
    Root {
        limit: u32,
        project_limit: u32,
        searching: bool,
    },
    Project {
        id: &'a str,
        limit: u32,
        searching: bool,
    },
}

/// Retain only the requested standalone chat titles and one lookahead. Project
/// contents are read only when a project is opened; the root list keeps only
/// enough project headers to determine its own page.
pub(crate) struct TitleList<'a> {
    projects: &'a [Project],
    query: TitleQuery<'a>,
    data: Vec<Thread>,
    recent_projects: Vec<String>,
}
impl<'a> TitleList<'a> {
    pub(crate) fn new(projects: &'a [Project], query: TitleQuery<'a>) -> Self {
        Self {
            projects,
            query,
            data: Vec::new(),
            recent_projects: Vec::new(),
        }
    }
    fn limit(&self) -> usize {
        match self.query {
            TitleQuery::Root { limit, .. } | TitleQuery::Project { limit, .. } => {
                limit.max(1) as usize
            }
        }
    }
    fn project_limit(&self) -> usize {
        match self.query {
            TitleQuery::Root { project_limit, .. } => project_limit.max(1) as usize,
            TitleQuery::Project { .. } => usize::MAX,
        }
    }
    pub(crate) fn remaining_chats(&self) -> Option<usize> {
        matches!(self.query, TitleQuery::Root { .. }).then(|| {
            self.limit()
                .saturating_add(1)
                .saturating_sub(self.data.len())
        })
    }
    fn projects_complete(&self) -> bool {
        match self.query {
            TitleQuery::Root { .. } => {
                self.recent_projects.len()
                    >= self
                        .project_limit()
                        .saturating_add(1)
                        .min(self.projects.len())
            }
            TitleQuery::Project { .. } => true,
        }
    }
    pub(crate) fn done(&self) -> bool {
        self.complete()
            && (!matches!(
                self.query,
                TitleQuery::Root {
                    searching: true,
                    ..
                }
            ) || self.projects_complete())
    }
    pub(crate) fn push(&mut self, thread: Thread) -> bool {
        let searching = match self.query {
            TitleQuery::Root { searching, .. } | TitleQuery::Project { searching, .. } => searching,
        };
        let child = thread.parent_id.is_some();
        if self.done() {
            return false;
        }
        if !searching && child {
            return false;
        }
        if let TitleQuery::Project { id, .. } = self.query {
            if thread.project_id.as_deref() != Some(id) {
                return false;
            }
            self.data.push(summary(thread));
            return !child;
        }
        if let Some(id) = thread.project_id.as_ref() {
            if !self.projects_complete() && !self.recent_projects.contains(id) {
                self.recent_projects.push(id.clone());
            }
            return false;
        }
        if self.complete() {
            return false;
        }
        self.data.push(summary(thread));
        !child
    }
    fn complete(&self) -> bool {
        self.data.len() > self.limit()
    }
    pub(crate) fn finish(mut self) -> ThreadList {
        let positions: HashMap<_, _> = self
            .recent_projects
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        let mut projects: Vec<_> = self
            .projects
            .iter()
            .filter(|project| match self.query {
                TitleQuery::Root { searching, .. } => {
                    !searching || positions.contains_key(project.id.as_str())
                }
                TitleQuery::Project { id, .. } => project.id == id,
            })
            .cloned()
            .collect();
        projects.sort_by_key(|project| {
            positions
                .get(project.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        let has_more = self.complete();
        let has_more_projects = projects.len() > self.project_limit();
        self.data.truncate(self.limit());
        projects.truncate(self.project_limit());
        ThreadList {
            data: self.data,
            projects,
            has_more,
            has_more_projects,
            provider_errors: None,
        }
    }
}

pub(crate) fn summary(mut thread: Thread) -> Thread {
    // Untitled threads use a short first-message title, not its payload.
    if !thread
        .name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        let title: String = thread
            .preview
            .as_deref()
            .unwrap_or("")
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect();
        thread.name = (!title.is_empty()).then_some(title);
    }
    thread.turns = None;
    thread.preview = None;
    thread.history_has_more = None;
    thread.history_limit = None;
    thread.agent_id = None;
    thread
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            ..Default::default()
        }
    }
    fn thread(id: &str, project: Option<&str>) -> Thread {
        serde_json::from_value(
            json!({"id":{"provider":"codex","id":id},"projectId":project,
            "preview":"\nFirst line\nprivate body", "turns":[{"id":"turn"}]}),
        )
        .unwrap()
    }
    #[test]
    fn root_list_pages_standalone_chats_without_consuming_slots_for_projects() {
        let projects = vec![project("old"), project("recent"), project("empty")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 1,
                project_limit: 1,
                searching: false,
            },
        );
        titles.push(thread("latest", Some("recent")));
        titles.push(thread("chat", None));
        titles.push(thread("third", Some("recent")));
        assert!(!titles.complete());
        titles.push(thread("lookahead", Some("recent")));
        titles.push(thread("chat-2", None));
        assert!(titles.complete());
        assert_eq!(titles.remaining_chats(), Some(0));
        titles.push(thread("old-task", Some("old")));
        let page = titles.finish();
        assert_eq!(page.data.len(), 1);
        assert!(page.has_more);
        assert_eq!(
            page.projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["recent"]
        );
        assert!(page.has_more_projects);
        assert_eq!(
            page.data
                .iter()
                .map(|thread| thread.id.as_ref().unwrap().id.as_str())
                .collect::<Vec<_>>(),
            ["chat"]
        );
        assert!(page.data.iter().all(|t| t.turns.is_none()
            && t.preview.is_none()
            && t.name.as_deref() == Some("First line")));
    }
    #[test]
    fn project_page_selects_only_requested_roots_and_has_no_total_limit() {
        let projects = vec![project("p"), project("q")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Project {
                id: "p",
                limit: 1005,
                searching: false,
            },
        );
        for index in 0..1001 {
            titles.push(thread(&format!("q-{index}"), Some("q")));
            titles.push(thread(&format!("p-{index}"), Some("p")));
        }
        assert!(!titles.complete());
        let page = titles.finish();
        assert_eq!(page.data.len(), 1001);
        assert!(!page.has_more);
        assert_eq!(page.projects.len(), 1);
        assert!(
            page.data
                .iter()
                .all(|t| t.project_id.as_deref() == Some("p"))
        );
    }

    #[test]
    fn root_project_headers_page_independently_from_standalone_chats() {
        let projects = (1..=4)
            .map(|id| project(&format!("p{id}")))
            .collect::<Vec<_>>();
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 1,
                project_limit: 2,
                searching: false,
            },
        );
        titles.push(thread("p4-task", Some("p4")));
        titles.push(thread("chat", None));
        titles.push(thread("p3-task", Some("p3")));
        titles.push(thread("p2-task", Some("p2")));
        let page = titles.finish();
        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].id.as_ref().unwrap().id, "chat");
        assert_eq!(
            page.projects
                .iter()
                .map(|project| project.id.as_str())
                .collect::<Vec<_>>(),
            ["p4", "p3"]
        );
        assert!(page.has_more_projects);
    }

    #[test]
    fn search_completion_waits_for_chat_and_project_lookaheads() {
        let projects = vec![project("p1"), project("p2")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 1,
                project_limit: 1,
                searching: true,
            },
        );
        titles.push(thread("chat-1", None));
        titles.push(thread("chat-2", None));
        assert!(titles.complete());
        assert!(!titles.projects_complete());
        assert!(!titles.done());
        titles.push(thread("p1-task", Some("p1")));
        titles.push(thread("p2-task", Some("p2")));
        assert!(titles.projects_complete());
        assert!(titles.done());
    }

    #[test]
    fn root_stops_without_scanning_old_tasks_for_empty_project_headers() {
        let projects = vec![project("p1"), project("p2")];
        let mut exact_page = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 1,
                project_limit: 5,
                searching: false,
            },
        );
        exact_page.push(thread("chat-1", None));
        exact_page.push(thread("chat-2", None));
        assert!(exact_page.done());
        let page = exact_page.finish();
        assert_eq!(page.projects.len(), 2);
        assert_eq!(page.data.len(), 1);
        assert!(page.has_more);
        assert!(!page.has_more_projects);
    }

    #[test]
    fn assigned_rows_do_not_consume_the_root_chat_lookahead() {
        let projects = vec![project("p1")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 5,
                project_limit: 1,
                searching: false,
            },
        );
        for index in 0..64 {
            titles.push(thread(&format!("assigned-{index}"), Some("p1")));
        }
        assert!(!titles.complete());
        assert!(titles.projects_complete());
        for index in 0..6 {
            titles.push(thread(&format!("chat-{index}"), None));
        }
        assert!(titles.done());
        let page = titles.finish();
        assert_eq!(page.data.len(), 5);
        assert!(page.has_more);
    }

    #[test]
    fn search_preserves_child_project_matches_without_showing_child_rows() {
        let projects = vec![project("p"), project("q")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 5,
                project_limit: 5,
                searching: true,
            },
        );
        let mut child = thread("child", Some("p"));
        child.parent_id = Some(
            agent_protocol::session::SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "root".into(),
            )
            .unwrap(),
        );
        titles.push(child);
        assert!(!titles.complete());
        let page = titles.finish();
        assert!(page.data.is_empty());
        assert_eq!(
            page.projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["p"]
        );
    }
    proptest::proptest! {
        #[test]
        fn limits_use_exact_lookahead(count in 0usize..80, limit in 1u32..40) {
            let mut titles=TitleList::new(&[],TitleQuery::Root { limit, project_limit:5, searching:false });
            for index in 0..count { titles.push(thread(&index.to_string(),None)); }
            proptest::prop_assert_eq!(titles.remaining_chats(), Some((limit as usize + 1).saturating_sub(count)));
            let page=titles.finish();
            proptest::prop_assert_eq!(page.data.len(), count.min(limit as usize));
            proptest::prop_assert_eq!(page.has_more, count>limit as usize);
        }
    }
}
