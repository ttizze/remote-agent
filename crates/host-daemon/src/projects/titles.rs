use agent_protocol::models::{Project, Thread, ThreadList};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) enum TitleQuery<'a> {
    Recent { limit: u32, searching: bool },
    Project { id: &'a str, limit: u32 },
}

/// Retain only the requested root titles and one lookahead. Project completeness
/// is checked only when that project is opened, never during the recent list.
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
            TitleQuery::Recent { limit, .. } | TitleQuery::Project { limit, .. } => {
                limit.max(1) as usize
            }
        }
    }
    pub(crate) fn remaining_recent(&self) -> Option<usize> {
        matches!(self.query, TitleQuery::Recent { .. }).then(|| {
            self.limit()
                .saturating_add(1)
                .saturating_sub(self.data.len())
        })
    }
    pub(crate) fn push(&mut self, thread: Thread) {
        if self.complete() || thread.parent_id.is_some() {
            return;
        }
        if let TitleQuery::Project { id, .. } = self.query
            && thread.project_id.as_deref() != Some(id)
        {
            return;
        }
        if self.data.len() < self.limit()
            && let Some(id) = thread.project_id.as_ref()
            && !self.recent_projects.contains(id)
        {
            self.recent_projects.push(id.clone());
        }
        self.data.push(summary(thread));
    }
    pub(crate) fn complete(&self) -> bool {
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
                TitleQuery::Recent { searching, .. } => {
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
        self.data.truncate(self.limit());
        ThreadList {
            data: self.data,
            projects,
            has_more,
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
    fn recent_list_stops_without_filling_or_reading_old_projects() {
        let projects = vec![project("old"), project("recent"), project("empty")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Recent {
                limit: 3,
                searching: false,
            },
        );
        titles.push(thread("latest", Some("recent")));
        titles.push(thread("chat", None));
        titles.push(thread("third", Some("recent")));
        assert!(!titles.complete());
        titles.push(thread("lookahead", Some("recent")));
        assert!(titles.complete());
        assert_eq!(titles.remaining_recent(), Some(0));
        titles.push(thread("old-task", Some("old")));
        let page = titles.finish();
        assert_eq!(page.data.len(), 3);
        assert!(page.has_more);
        assert_eq!(
            page.projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["recent", "old", "empty"]
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
    fn children_never_consume_limits_and_search_keeps_only_matching_headers() {
        let projects = vec![project("p"), project("q")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Recent {
                limit: 1,
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
        titles.push(thread("root", Some("p")));
        titles.push(thread("next", Some("q")));
        assert!(titles.complete());
        let page = titles.finish();
        assert_eq!(page.data[0].id.as_ref().unwrap().id, "root");
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
            let mut titles=TitleList::new(&[],TitleQuery::Recent { limit, searching:false });
            for index in 0..count { titles.push(thread(&index.to_string(),None)); }
            let page=titles.finish();
            proptest::prop_assert_eq!(page.data.len(), count.min(limit as usize));
            proptest::prop_assert_eq!(page.has_more, count>limit as usize);
        }
    }
}
