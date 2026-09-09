use std::collections::{HashMap, HashSet};

use agent_core::models::{Project, Thread, ThreadList};
use serde_json::{Value, json};

/// Select from newest-first DB metadata. Retain only visible titles and one
/// lookahead per section; never retain rollout bodies or unused thread fields.
pub(crate) struct TitleList {
    projects: Vec<Project>,
    known_projects: HashSet<String>,
    recent_projects: Vec<String>,
    threads: HashMap<String, Vec<Thread>>,
    chats: Vec<Thread>,
    project_limit: usize,
    chat_limit: usize,
    thread_limits: HashMap<String, usize>,
    searching: bool,
}

fn limit(value: Option<&Value>, default: usize) -> usize {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
        .max(1)
}

impl TitleList {
    pub(crate) fn new(projects: Vec<Project>, params: &Value) -> Self {
        let known_projects = projects.iter().map(|project| project.id.clone()).collect();
        Self {
            projects,
            known_projects,
            recent_projects: Vec::new(),
            threads: HashMap::new(),
            chats: Vec::new(),
            project_limit: limit(params.get("projectLimit"), 5),
            chat_limit: limit(params.get("chatLimit"), 5),
            thread_limits: params
                .get("projectThreadLimits")
                .and_then(Value::as_object)
                .map(|limits| {
                    limits
                        .iter()
                        .map(|(id, count)| (id.clone(), limit(Some(count), 5)))
                        .collect()
                })
                .unwrap_or_default(),
            searching: params
                .get("searchTerm")
                .and_then(Value::as_str)
                .is_some_and(|term| !term.trim().is_empty()),
        }
    }

    fn thread_limit(&self, project_id: &str) -> usize {
        self.thread_limits.get(project_id).copied().unwrap_or(5)
    }

    pub(crate) fn push(&mut self, mut thread: Thread) {
        let project_id = thread
            .project_id
            .as_ref()
            .and_then(Option::as_deref)
            .filter(|id| self.known_projects.contains(*id));
        let target = if let Some(project_id) = project_id {
            let position = match self.recent_projects.iter().position(|id| id == project_id) {
                Some(position) => position,
                None => {
                    self.recent_projects.push(project_id.to_owned());
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
            self.threads.entry(project_id.to_owned()).or_default()
        } else {
            if self.chats.len() > self.chat_limit {
                return;
            }
            &mut self.chats
        };
        // Untitled threads use a short first-message title, not its payload.
        if !thread
            .name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
        {
            let title: String = thread
                .extra
                .get("preview")
                .and_then(Value::as_str)
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
        thread.history_cursor = None;
        thread.path = None;
        thread
            .extra
            .retain(|key, _| matches!(key.as_str(), "createdAt" | "updatedAt"));
        target.push(thread);
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
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        if self.searching {
            self.projects
                .retain(|project| positions.contains_key(project.id.as_str()));
        }
        self.projects.sort_by_key(|project| {
            positions
                .get(project.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        for (position, project) in self.projects.iter_mut().enumerate() {
            project.extra.insert("position".into(), json!(position));
        }
        let mut more_project_ids = Vec::new();
        let mut data = Vec::new();
        for id in self.recent_projects.iter().take(self.project_limit) {
            let maximum = self.thread_limit(id);
            let mut threads = self.threads.remove(id).unwrap_or_default();
            if threads.len() > maximum {
                more_project_ids.push(id.clone());
                threads.truncate(maximum);
            }
            data.extend(threads);
        }
        let more_projects = self.projects.len() > self.project_limit;
        self.projects.truncate(self.project_limit);
        let more_chats = self.chats.len() > self.chat_limit;
        self.chats.truncate(self.chat_limit);
        data.extend(self.chats);
        ThreadList {
            data,
            projects: self.projects,
            more_project_ids,
            has_more_projects: more_projects,
            has_more_chats: more_chats,
            extra: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn thread(value: Value) -> Thread {
        serde_json::from_value(value).unwrap()
    }
    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            roots: vec![],
            extra: Default::default(),
        }
    }

    #[test]
    fn returns_latest_five_titles_per_recent_project_and_independent_chat_page() {
        let projects = (1..=7).map(|id| project(&format!("p{id}"))).collect();
        let mut list = TitleList::new(projects, &json!({}));
        for index in 0..9 {
            for project in (1..=7).rev() {
                list.push(thread(json!({"id":format!("p{project}-{index}"),"projectId":format!("p{project}"),"name":"title","preview":"long body".repeat(10000),"turns":[{"id":"turn"}]})));
            }
            list.push(thread(json!({"id":format!("chat-{index}"),"preview":"\nFirst line\nprivate body","cwd":"/other"})));
        }
        assert!(list.complete());
        let result = serde_json::to_value(list.finish()).unwrap();
        let data = result["data"].as_array().unwrap();
        assert_eq!(data.len(), 30);
        assert_eq!(data[0]["id"], "p7-0");
        assert_eq!(data[4]["id"], "p7-4");
        assert_eq!(data[25]["id"], "chat-0");
        assert_eq!(data[29]["id"], "chat-4");
        assert_eq!(data[25]["name"], "First line");
        assert!(
            data.iter()
                .all(|thread| thread.get("preview").is_none() && thread.get("turns").is_none())
        );
        assert_eq!(result["projects"][0]["id"], "p7");
        assert_eq!(result["projects"].as_array().unwrap().len(), 5);
        assert_eq!(result["hasMoreProjects"], true);
        assert_eq!(result["moreProjectIds"].as_array().unwrap().len(), 5);
        assert_eq!(result["hasMoreChats"], true);
        assert!(serde_json::to_vec(&result).unwrap().len() < 4096);
    }

    #[test]
    fn repeated_expansion_has_no_hidden_total_title_limit() {
        let mut list = TitleList::new(
            vec![project("p")],
            &json!({"projectThreadLimits":{"p":1005}}),
        );
        for index in 0..1001 {
            list.push(thread(
                json!({"id":format!("t{index}"),"projectId":"p","name":"Title"}),
            ));
        }
        let result = serde_json::to_value(list.finish()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 1001);
        assert_eq!(result["data"][1000]["id"], "t1000");
        assert_eq!(result["moreProjectIds"], json!([]));
    }

    #[test]
    fn expanding_one_project_does_not_expand_other_sections_and_exact_end_has_no_more() {
        let mut list = TitleList::new(
            vec![project("p"), project("q")],
            &json!({"projectThreadLimits":{"p":15}}),
        );
        for index in 0..15 {
            list.push(thread(
                json!({"id":format!("p{index}"),"projectId":"p","name":"P"}),
            ));
            list.push(thread(
                json!({"id":format!("q{index}"),"projectId":"q","name":"Q"}),
            ));
        }
        let result = serde_json::to_value(list.finish()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 20);
        assert_eq!(result["moreProjectIds"], json!(["q"]));
        assert_eq!(result["hasMoreChats"], false);
    }
}
