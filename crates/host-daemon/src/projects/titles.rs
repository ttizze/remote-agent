use agent_protocol::models::{Project, Thread, ThreadList};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy)]
pub(crate) enum TitleQuery<'a> {
    Root {
        limit: u32,
        project_limit: u32,
        searching: bool,
        project_limits: Option<&'a HashMap<String, u32>>,
        part: Option<agent_protocol::models::ListPart>,
    },
    Project {
        id: &'a str,
        limit: u32,
        searching: bool,
    },
}

/// One native title walk supplies chats and explicitly requested project pages.
/// Unrequested project contents never enter the reply or Git-status work.
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
    fn page_limit(&self, id: &str) -> Option<usize> {
        match self.query {
            TitleQuery::Root {
                project_limits: Some(limits),
                ..
            } => limits.get(id).map(|limit| (*limit).max(1) as usize),
            TitleQuery::Root {
                project_limits: None,
                ..
            } => self
                .recent_projects
                .iter()
                .take(self.project_limit().min(5))
                .any(|recent| recent == id)
                .then_some(5),
            TitleQuery::Project { .. } => None,
        }
    }
    fn count(&self, project: Option<&str>) -> usize {
        self.data
            .iter()
            .filter(|thread| thread.project_id.as_deref() == project)
            .count()
    }
    pub(crate) fn remaining_chats(&self) -> Option<usize> {
        matches!(self.query, TitleQuery::Root { .. }).then(|| {
            self.limit()
                .saturating_add(1)
                .saturating_sub(self.count(None))
        })
    }
    fn projects_complete(&self) -> bool {
        self.recent_projects.len()
            >= self
                .project_limit()
                .saturating_add(1)
                .min(self.projects.len())
    }
    fn complete(&self) -> bool {
        match self.query {
            TitleQuery::Root {
                part: Some(agent_protocol::models::ListPart::Projects),
                ..
            } => true,
            TitleQuery::Root { .. } => self.count(None) > self.limit(),
            TitleQuery::Project { .. } => self.data.len() > self.limit(),
        }
    }
    pub(crate) fn done(&self) -> bool {
        if !self.complete() {
            return false;
        }
        match self.query {
            TitleQuery::Project { .. } => true,
            TitleQuery::Root {
                part: Some(agent_protocol::models::ListPart::Chats),
                ..
            } => true,
            TitleQuery::Root {
                part: Some(agent_protocol::models::ListPart::Projects),
                ..
            } => self.projects_complete(),
            TitleQuery::Root {
                project_limits: Some(limits),
                ..
            } => {
                self.projects_complete()
                    && limits
                        .iter()
                        .filter(|(id, _)| self.projects.iter().any(|project| &project.id == *id))
                        .all(|(id, limit)| self.count(Some(id)) > (*limit).max(1) as usize)
            }
            TitleQuery::Root {
                project_limits: None,
                ..
            } => {
                self.projects_complete()
                    && self
                        .recent_projects
                        .iter()
                        .take(self.project_limit().min(5))
                        .all(|id| self.count(Some(id)) > 5)
            }
        }
    }
    pub(crate) fn push(&mut self, thread: Thread) {
        let searching = match self.query {
            TitleQuery::Root { searching, .. } | TitleQuery::Project { searching, .. } => searching,
        };
        let child = thread.parent_id.is_some();
        if self.done() || (!searching && child) {
            return;
        }
        if let TitleQuery::Project { id, .. } = self.query {
            if thread.project_id.as_deref() != Some(id) {
                return;
            }
        } else if let Some(id) = thread.project_id.as_deref() {
            if matches!(
                self.query,
                TitleQuery::Root {
                    part: Some(agent_protocol::models::ListPart::Chats),
                    ..
                }
            ) {
                return;
            }
            if !self.projects_complete() && !self.recent_projects.iter().any(|recent| recent == id)
            {
                self.recent_projects.push(id.to_owned());
            }
            if self
                .page_limit(id)
                .is_none_or(|limit| self.count(Some(id)) > limit)
            {
                return;
            }
        } else if self.complete() {
            return;
        }
        self.data.push(summary(thread));
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
                TitleQuery::Root {
                    part: Some(agent_protocol::models::ListPart::Chats),
                    ..
                } => false,
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
        projects.truncate(self.project_limit());
        let limits: BTreeMap<_, _> = match self.query {
            TitleQuery::Root {
                project_limits: None,
                ..
            } => projects
                .iter()
                .take(5)
                .map(|p| (p.id.clone(), 5usize))
                .collect(),
            TitleQuery::Root {
                project_limits: Some(limits),
                ..
            } => projects
                .iter()
                .filter_map(|p| {
                    limits
                        .get(&p.id)
                        .map(|limit| (p.id.clone(), (*limit).max(1) as usize))
                })
                .collect(),
            TitleQuery::Project { .. } => Default::default(),
        };
        let project_pages = limits
            .iter()
            .map(|(id, limit)| {
                (
                    id.clone(),
                    agent_protocol::models::ListPage {
                        limit: *limit as u32,
                        has_more: self.count(Some(id)) > *limit,
                    },
                )
            })
            .collect();
        let mut counts = HashMap::<Option<String>, usize>::new();
        let limit = self.limit();
        self.data.retain(|thread| {
            let id = thread.project_id.as_ref().cloned();
            let count = counts.entry(id.clone()).or_default();
            *count += 1;
            let maximum = match self.query {
                TitleQuery::Project { .. } => limit,
                TitleQuery::Root { .. } => id
                    .as_ref()
                    .map_or(limit, |id| limits.get(id).copied().unwrap_or(0)),
            };
            *count <= maximum
        });
        ThreadList {
            limit: limit as u32,
            data: self.data,
            projects,
            has_more,
            has_more_projects,
            project_pages,
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
                project_limits: None,
                part: None,
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
        assert_eq!(
            page.data
                .iter()
                .filter(|thread| thread.project_id.is_none())
                .count(),
            1
        );
        assert_eq!(
            page.data
                .iter()
                .filter(|thread| thread.project_id.as_deref() == Some("recent"))
                .count(),
            3
        );
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
            ["latest", "chat", "third", "lookahead"]
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
                project_limits: None,
                part: None,
                searching: false,
            },
        );
        titles.push(thread("p4-task", Some("p4")));
        titles.push(thread("chat", None));
        titles.push(thread("p3-task", Some("p3")));
        titles.push(thread("p2-task", Some("p2")));
        let page = titles.finish();
        assert_eq!(page.data.len(), 3);
        assert_eq!(
            page.data
                .iter()
                .find(|thread| thread.project_id.is_none())
                .unwrap()
                .id
                .as_ref()
                .unwrap()
                .id,
            "chat"
        );
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
                project_limits: None,
                part: None,
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
        assert!(!titles.done());
        for i in 0..5 {
            titles.push(thread(&format!("p1-{i}"), Some("p1")));
        }
        assert!(titles.done());
    }

    #[test]
    fn initial_pages_include_empty_projects_without_inventing_more_rows() {
        let projects = vec![project("p1"), project("p2")];
        let mut exact_page = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 1,
                project_limit: 2,
                project_limits: None,
                part: None,
                searching: false,
            },
        );
        exact_page.push(thread("chat-1", None));
        exact_page.push(thread("chat-2", None));
        assert!(!exact_page.done());
        let page = exact_page.finish();
        assert_eq!(page.projects.len(), 2);
        assert_eq!(page.data.len(), 1);
        assert!(page.has_more);
        assert!(!page.has_more_projects);
    }

    #[test]
    fn refresh_waits_for_open_project_lookahead_and_project_headers_in_either_order() {
        let projects = vec![project("p1"), project("p2"), project("p3")];
        let limits = [("p1".into(), 15)].into();
        for headers_first in [false, true] {
            let mut titles = TitleList::new(
                &projects,
                TitleQuery::Root {
                    limit: 1,
                    project_limit: 2,
                    project_limits: Some(&limits),
                    part: None,
                    searching: false,
                },
            );
            titles.push(thread("chat-1", None));
            titles.push(thread("chat-2", None));
            titles.push(thread("p1-0", Some("p1")));
            if headers_first {
                titles.push(thread("p2-task", Some("p2")));
                titles.push(thread("p3-task", Some("p3")));
            }
            for index in 1..15 {
                titles.push(thread(&format!("p1-{index}"), Some("p1")));
            }
            assert!(!titles.done(), "fifteen rows do not establish a lookahead");
            titles.push(thread("p1-lookahead", Some("p1")));
            if !headers_first {
                assert!(!titles.done(), "project header lookahead is still missing");
                titles.push(thread("p2-task", Some("p2")));
                titles.push(thread("p3-task", Some("p3")));
            }
            assert!(titles.done());
            let page = titles.finish();
            assert_eq!(page.project_pages["p1"].limit, 15);
            assert!(page.project_pages["p1"].has_more && page.has_more_projects);
            assert_eq!(page.data.len(), 16);
            assert!(
                page.data
                    .iter()
                    .all(|row| row.project_id.as_deref() != Some("p2")
                        && row.project_id.as_deref() != Some("p3"))
            );
        }
    }

    #[test]
    fn assigned_rows_do_not_consume_the_root_chat_lookahead() {
        let projects = vec![project("p1")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 5,
                project_limit: 1,
                project_limits: None,
                part: None,
                searching: false,
            },
        );
        for index in 0..5 {
            titles.push(thread(&format!("assigned-{index}"), Some("p1")));
        }
        assert!(!titles.complete());
        assert!(titles.projects_complete());
        for index in 0..6 {
            titles.push(thread(&format!("chat-{index}"), None));
        }
        assert!(
            !titles.done(),
            "five project rows do not establish a lookahead"
        );
        for index in 5..64 {
            titles.push(thread(&format!("assigned-{index}"), Some("p1")));
        }
        assert!(titles.done());
        let page = titles.finish();
        assert_eq!(
            page.data
                .iter()
                .filter(|thread| thread.project_id.is_none())
                .count(),
            5
        );
        assert_eq!(
            page.data
                .iter()
                .filter(|thread| thread.project_id.as_deref() == Some("p1"))
                .count(),
            5
        );
        assert!(page.has_more && page.project_pages["p1"].has_more);
    }

    #[test]
    fn search_preserves_child_project_matches_without_showing_child_rows() {
        let projects = vec![project("p"), project("q")];
        let mut titles = TitleList::new(
            &projects,
            TitleQuery::Root {
                limit: 5,
                project_limit: 5,
                project_limits: None,
                part: None,
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
        assert_eq!(page.data.len(), 1);
        assert!(page.data[0].parent_id.is_some());
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
            let mut titles=TitleList::new(&[],TitleQuery::Root { limit, project_limit:5, project_limits: None,
                part: None, searching:false });
            for index in 0..count { titles.push(thread(&index.to_string(),None)); }
            proptest::prop_assert_eq!(titles.remaining_chats(), Some((limit as usize + 1).saturating_sub(count)));
            let page=titles.finish();
            proptest::prop_assert_eq!(page.data.len(), count.min(limit as usize));
            proptest::prop_assert_eq!(page.has_more, count>limit as usize);
        }
    }
}
