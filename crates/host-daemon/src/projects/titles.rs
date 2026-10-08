use std::collections::{BTreeMap, HashMap, HashSet};

use agent_protocol::models::{ListQuery, Project, Thread, ThreadList};

/// Select from newest-first DB metadata. Retain only visible titles and one
/// lookahead per section; never retain rollout bodies or unused thread fields.
pub(crate) struct TitleList<'a> {
    projects: &'a [Project],
    known_projects: HashSet<&'a str>,
    recent_projects: Vec<&'a str>,
    threads: HashMap<&'a str, Vec<Thread>>,
    chats: Vec<Thread>,
    children: Vec<Thread>,
    project_limit: usize,
    chat_limit: usize,
    thread_limits: &'a BTreeMap<String, u32>,
    searching: bool,
}

impl<'a> TitleList<'a> {
    pub(crate) fn new(projects: &'a [Project], params: &'a ListQuery) -> Self {
        let known_projects = projects.iter().map(|project| project.id.as_str()).collect();
        Self {
            projects,
            known_projects,
            recent_projects: Vec::new(),
            threads: HashMap::new(),
            chats: Vec::new(),
            children: Vec::new(),
            project_limit: params.project_limit.max(1) as usize,
            chat_limit: params.chat_limit.max(1) as usize,
            thread_limits: &params.project_thread_limits,
            searching: !params.search_term.trim().is_empty(),
        }
    }

    fn thread_limit(&self, project_id: &str) -> usize {
        self.thread_limits
            .get(project_id)
            .copied()
            .unwrap_or(5)
            .max(1) as usize
    }

    pub(crate) fn push(&mut self, thread: Thread) {
        let project_id = thread
            .project_id
            .as_deref()
            .and_then(|id| self.known_projects.get(id).copied());
        let target = if thread.parent_id.is_some() && !self.searching {
            // Children accompany visible roots without consuming their limits.
            &mut self.children
        } else if let Some(project_id) = project_id {
            let position = match self.recent_projects.iter().position(|id| *id == project_id) {
                Some(position) => position,
                None => {
                    self.recent_projects.push(project_id);
                    self.recent_projects.len() - 1
                }
            };
            // A new project encountered later cannot outrank visible projects.
            if position >= self.project_limit {
                return;
            }
            let maximum = self.thread_limit(project_id);
            if self
                .threads
                .get(project_id)
                .is_some_and(|entries| entries.len() > maximum)
            {
                return;
            }
            self.threads.entry(project_id).or_default()
        } else {
            if self.chats.len() > self.chat_limit {
                return;
            }
            &mut self.chats
        };
        target.push(summary(thread));
    }

    pub(crate) fn complete(&self) -> bool {
        self.recent_projects.len()
            >= self
                .project_limit
                .saturating_add(1)
                .min(self.projects.len())
            && self.chats.len() > self.chat_limit
            && self
                .recent_projects
                .iter()
                .take(self.project_limit)
                .all(|id| self.threads[id].len() > self.thread_limit(id))
    }

    pub(crate) fn finish(mut self) -> ThreadList {
        let positions: HashMap<_, _> = self
            .recent_projects
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let mut projects = self
            .projects
            .iter()
            .filter(|project| !self.searching || positions.contains_key(project.id.as_str()))
            .collect::<Vec<_>>();
        projects.sort_by_key(|project| {
            positions
                .get(project.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        let mut more_project_ids = Vec::new();
        let mut data = Vec::new();
        for id in self.recent_projects.iter().take(self.project_limit) {
            let maximum = self.thread_limit(id);
            let mut threads = self.threads.remove(id).unwrap_or_default();
            if threads.len() > maximum {
                more_project_ids.push((*id).to_owned());
                threads.truncate(maximum);
            }
            data.extend(threads);
        }
        let more_projects = projects.len() > self.project_limit;
        projects.truncate(self.project_limit);
        let more_chats = self.chats.len() > self.chat_limit;
        self.chats.truncate(self.chat_limit);
        data.extend(self.chats);
        ThreadList {
            provider_errors: None,
            data: append_descendants(data, self.children),
            projects: projects.into_iter().cloned().collect(),
            more_project_ids,
            has_more_projects: more_projects,
            has_more_chats: more_chats,
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

pub(crate) fn append_descendants(mut roots: Vec<Thread>, children: Vec<Thread>) -> Vec<Thread> {
    let mut visible: HashSet<_> = roots
        .iter()
        .filter_map(|thread| thread.id.clone())
        .collect();
    let mut included = visible.clone();
    loop {
        let before = visible.len();
        for child in &children {
            if child
                .parent_id
                .as_ref()
                .is_some_and(|id| visible.contains(id))
                && let Some(id) = &child.id
            {
                visible.insert(id.clone());
            }
        }
        if visible.len() == before {
            break;
        }
    }
    roots.extend(children.into_iter().filter(|thread| {
        thread
            .id
            .as_ref()
            .is_some_and(|id| visible.contains(id) && included.insert(id.clone()))
    }));
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn older_descendants_survive_root_pagination_without_duplicate_rows() {
        let query = ListQuery {
            chat_limit: 1,
            ..Default::default()
        };
        let mut list = TitleList::new(&[], &query);
        for (id, updated_at) in [("root", 100), ("hidden", 90)] {
            list.push(thread(
                json!({"id":{"provider":"codex","id":id},"updatedAt":updated_at}),
            ));
        }
        let mut page = list.finish();
        let children = [
            ("grandchild", "child"),
            ("child", "root"),
            ("child", "root"),
            ("hidden-child", "hidden"),
            ("orphan", "missing"),
        ]
        .into_iter()
        .map(|(id, parent)| {
            summary(thread(json!({
                "id":{"provider":"codex","id":id},
                "parentId":{"provider":"codex","id":parent},
                "updatedAt":10,
                "preview":format!("Title {id}\nBody"),
                "turns":[{"id":"turn"}]
            })))
        })
        .collect();
        page.data = append_descendants(page.data, children);
        assert!(page.has_more_chats);
        let ids: Vec<_> = page
            .data
            .iter()
            .map(|thread| thread.id.as_ref().unwrap().id.as_str())
            .collect();
        assert_eq!(ids, ["root", "grandchild", "child"]);
        assert_eq!(page.data[1].name.as_deref(), Some("Title grandchild"));
        assert!(
            page.data
                .iter()
                .all(|thread| thread.preview.is_none() && thread.turns.is_none())
        );
    }

    #[test]
    fn children_follow_visible_roots_without_consuming_title_limits() {
        let query = ListQuery {
            chat_limit: 1,
            ..Default::default()
        };
        let mut list = TitleList::new(&[], &query);
        for (id, parent) in [
            ("grandchild", Some("child")),
            ("hidden-child", Some("hidden")),
            ("child", Some("root")),
            ("root", None),
            ("hidden", None),
        ] {
            list.push(thread(json!({"id":{"provider":"codex","id":id}, "parentId":parent.map(|id| json!({"provider":"codex","id":id})), "preview":format!("Title {id}")})));
        }
        let result = list.finish();
        assert!(result.has_more_chats);
        let ids: Vec<_> = result
            .data
            .iter()
            .map(|thread| thread.id.as_ref().unwrap().id.as_str())
            .collect();
        assert_eq!(ids, ["root", "grandchild", "child"]);
        assert!(result.data.iter().all(|thread| thread.name.is_some()
            && thread.preview.is_none()
            && thread.turns.is_none()));
        let search = ListQuery {
            search_term: "child".into(),
            ..Default::default()
        };
        let mut list = TitleList::new(&[], &search);
        list.push(thread(json!({"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Matches"})));
        assert_eq!(
            list.finish().data.len(),
            1,
            "search matches remain visible without their parent"
        );
    }
    fn thread(value: Value) -> Thread {
        serde_json::from_value(value).unwrap()
    }
    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            roots: vec![],
        }
    }

    #[test]
    fn returns_latest_five_titles_per_recent_project_and_independent_chat_page() {
        let projects = (1..=7)
            .map(|id| project(&format!("p{id}")))
            .collect::<Vec<_>>();
        let query = ListQuery::default();
        let mut list = TitleList::new(&projects, &query);
        for index in 0..9 {
            for project in (1..=7).rev() {
                list.push(thread(json!({"id":{"provider":"codex","id":format!("p{project}-{index}")},"projectId":format!("p{project}"),"name":"title","preview":"long body".repeat(10000),"turns":[{"id":"turn"}]})));
            }
            list.push(thread(json!({"id":{"provider":"codex","id":format!("chat-{index}")},"preview":"\nFirst line\nprivate body","cwd":"/other"})));
        }
        assert!(list.complete());
        let page = list.finish();
        assert!(agent_protocol::protocol::encode(&page).unwrap().len() < 4096);
        let result = serde_json::to_value(page).unwrap();
        let data = result["data"].as_array().unwrap();
        assert_eq!(data.len(), 30);
        assert_eq!(data[0]["id"]["id"], "p7-0");
        assert_eq!(data[4]["id"]["id"], "p7-4");
        assert_eq!(data[25]["id"]["id"], "chat-0");
        assert_eq!(data[29]["id"]["id"], "chat-4");
        assert_eq!(data[25]["name"], "First line");
        assert!(
            data.iter()
                .all(|thread| thread["preview"].is_null() && thread["turns"].is_null())
        );
        assert_eq!(result["projects"][0]["id"], "p7");
        assert_eq!(result["projects"].as_array().unwrap().len(), 5);
        assert_eq!(result["hasMoreProjects"], true);
        assert_eq!(result["moreProjectIds"].as_array().unwrap().len(), 5);
        assert_eq!(result["hasMoreChats"], true);
    }

    #[test]
    fn repeated_expansion_has_no_hidden_total_title_limit() {
        let projects = [project("p")];
        let query = ListQuery {
            project_thread_limits: [("p".into(), 1005)].into(),
            ..Default::default()
        };
        let mut list = TitleList::new(&projects, &query);
        for index in 0..1001 {
            list.push(thread(
                json!({"id":{"provider":"codex","id":format!("t{index}")},"projectId":"p","name":"Title"}),
            ));
        }
        let result = serde_json::to_value(list.finish()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 1001);
        assert_eq!(result["data"][1000]["id"]["id"], "t1000");
        assert_eq!(result["moreProjectIds"], json!([]));
    }

    #[test]
    fn expanding_one_project_does_not_expand_other_sections_and_exact_end_has_no_more() {
        let projects = [project("p"), project("q")];
        let query = ListQuery {
            project_thread_limits: [("p".into(), 15)].into(),
            ..Default::default()
        };
        let mut list = TitleList::new(&projects, &query);
        for index in 0..15 {
            list.push(thread(
                json!({"id":{"provider":"codex","id":format!("p{index}")},"projectId":"p","name":"P"}),
            ));
            list.push(thread(
                json!({"id":{"provider":"codex","id":format!("q{index}")},"projectId":"q","name":"Q"}),
            ));
        }
        let result = serde_json::to_value(list.finish()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 20);
        assert_eq!(result["moreProjectIds"], json!(["q"]));
        assert_eq!(result["hasMoreChats"], false);
    }
}
