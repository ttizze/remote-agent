//! Conversation lists: recent chats, project pages, decorations and sub-agents.
use super::*;
use std::collections::{HashMap, HashSet};

pub use agent_protocol::operations::AddProject;

impl Operation for AddProject {
    rpc_operation!();
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, cwd: Self::Output) -> Vec<Effect> {
        let (next, mut effects) = reduce_intent(snapshot, pane_id, Intent::NewChat { cwd });
        *snapshot = next;
        effects.push(Effect::execute(ListSessions::new(
            (*snapshot.lists.list_query).clone(),
        )));
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _pane_id: &str, _: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ListSessions::new(
            (*snapshot.lists.list_query).clone(),
        ))]
    }
}

pub use agent_protocol::operations::ListAgents;
pub use agent_protocol::operations::ListProjectSessions;
pub use agent_protocol::operations::ListSessions;

pub use agent_protocol::operations::ReadListDecorations;

fn decorations(
    scope: agent_protocol::operations::ListDecorationScope,
    projects: &[crate::models::Project],
    threads: &[crate::models::Thread],
) -> Option<Effect> {
    let projects: Vec<_> = projects
        .iter()
        .filter(|project| !project.roots.is_empty())
        .map(|project| {
            (
                project.id.clone(),
                project.roots.iter().map(|root| root.path.clone()).collect(),
            )
        })
        .collect();
    let threads: Vec<_> = threads
        .iter()
        .filter_map(|thread| {
            Some((
                thread.id.clone()?,
                thread.cwd.clone()?,
                thread.git_branch.clone(),
            ))
        })
        .collect();
    (!projects.is_empty() || !threads.is_empty()).then(|| {
        Effect::execute(ReadListDecorations {
            scope,
            projects,
            threads,
        })
    })
}

impl Operation for ReadListDecorations {
    rpc_operation!();
    const BACKGROUND: bool = true;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ListDecorations {
            scope: self.scope.clone(),
        })
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if let Some(list) = &mut snapshot.lists.threads {
            let icons: Vec<_> = list
                .projects
                .iter()
                .enumerate()
                .filter_map(|(index, project)| {
                    output
                        .icons
                        .get(&project.id)
                        .filter(|icon| **icon != project.icon)
                        .map(|icon| (index, icon))
                })
                .collect();
            if !icons.is_empty() {
                let list = Arc::make_mut(list);
                for (index, icon) in icons {
                    list.projects[index].icon = icon.clone();
                }
            }
        }

        if output.statuses.is_empty() {
            return Vec::new();
        }
        let mut statuses: HashMap<_, _> = output
            .statuses
            .iter()
            .map(|(id, status)| (id, (*status, None)))
            .collect();
        for (id, cwd, branch) in &self.threads {
            if let Some((_, source)) = statuses.get_mut(id) {
                source.get_or_insert((cwd.as_str(), branch.as_deref()));
            }
        }
        // Match the exact source captured by the read, including provider,
        // directory and branch. A later duplicate reply keeps its last value.
        let updates: Vec<_> = snapshot
            .lists
            .project_threads
            .iter()
            .map(|(id, list)| (Some(id), list))
            .chain(snapshot.lists.threads.iter().map(|list| (None, list)))
            .filter_map(|(project, list)| {
                let changed: Vec<_> = list
                    .data
                    .iter()
                    .enumerate()
                    .filter_map(|(index, thread)| {
                        let (status, source) = statuses.get(thread.id.as_ref()?)?;
                        let (cwd, branch) = source.as_ref()?;
                        (thread.cwd.as_deref() == Some(*cwd)
                            && thread.git_branch.as_deref() == *branch
                            && thread.has_changes != *status)
                            .then_some((index, *status))
                    })
                    .collect();
                (!changed.is_empty()).then(|| (project.cloned(), changed))
            })
            .collect();
        for (project, changed) in updates {
            let list = match project {
                Some(id) => Arc::make_mut(&mut snapshot.lists.project_threads)
                    .get_mut(&id)
                    .expect("captured project page is present"),
                None => snapshot
                    .lists
                    .threads
                    .as_mut()
                    .expect("captured root is present"),
            };
            let list = Arc::make_mut(list);
            for (index, status) in changed {
                list.data[index].has_changes = status;
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod decorations_tests;

pub(crate) fn refresh_agents(
    connected: bool,
    thread_id: Option<&crate::session::SessionRef>,
) -> Vec<Effect> {
    thread_id
        .filter(|_| connected)
        .map(|thread_id| {
            Effect::execute(ListAgents {
                thread_id: thread_id.clone(),
            })
        })
        .into_iter()
        .collect()
}

impl Operation for ListAgents {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Agents {
            session: self.thread_id.clone(),
        })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestAgents(self.thread_id.clone())
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, agents: Self::Output) -> Vec<Effect> {
        if !snapshot
            .panes
            .0
            .values()
            .any(|pane| pane.observed_agents.as_ref() == Some(&self.thread_id))
        {
            return Vec::new();
        }
        for agent in agents {
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .entry(agent.id.clone())
                    .or_default(),
            );
            thread.id = Some(agent.id);
            thread.parent_id = Some(agent.parent_id);
            thread.name = agent.name;
            thread.status = agent.status;
        }
        Vec::new()
    }
}

fn merge_newer_rows(
    mut received: Vec<crate::models::Thread>,
    cached: &[crate::models::Thread],
    revision: u64,
) -> Vec<crate::models::Thread> {
    // The last qualifying notice wins, while a new ID is appended where its
    // first notice appeared. Only the first received occurrence is replaced.
    let mut positions = HashMap::new();
    for (index, row) in received.iter().enumerate() {
        positions.entry(row.id.clone()).or_insert(index);
    }
    for cached in cached.iter().filter(|row| row.list_revision > revision) {
        let index = *positions.entry(cached.id.clone()).or_insert(received.len());
        if index == received.len() {
            received.push(cached.clone());
        } else {
            received[index] = cached.clone();
        }
    }
    received
}

fn missing_failed_provider_rows<'a>(
    received: &[crate::models::Thread],
    cached: &'a [crate::models::Thread],
    errors: &serde_json::Map<String, serde_json::Value>,
) -> Vec<&'a crate::models::Thread> {
    let mut present: HashSet<_> = received.iter().filter_map(|row| row.id.as_ref()).collect();
    cached
        .iter()
        .filter(|row| {
            row.id.as_ref().is_some_and(|id| {
                errors.contains_key(row.active_provider.unwrap_or(id.provider).key())
                    && present.insert(id)
            })
        })
        .collect()
}

fn missing_referenced_projects<'a>(
    received: &[crate::models::Project],
    cached: &'a [crate::models::Project],
    rows: &[crate::models::Thread],
) -> Vec<&'a crate::models::Project> {
    let mut missing: Vec<&crate::models::Project> = Vec::new();
    let used: HashSet<_> = rows
        .iter()
        .filter_map(|row| row.project_id.as_deref())
        .collect();
    if used.is_empty() {
        return missing;
    }
    let mut present = HashSet::with_capacity(received.len().max(cached.len()));
    for project in received {
        present.insert(project.id.as_str());
    }
    for project in cached {
        if used.contains(project.id.as_str()) && present.insert(project.id.as_str()) {
            missing.push(project);
        }
    }
    missing
}

fn merge_newer_projects(
    mut received: Vec<crate::models::Project>,
    cached: &[crate::models::Project],
    revision: u64,
) -> Vec<crate::models::Project> {
    let newer: Vec<_> = cached
        .iter()
        .filter(|project| project.list_revision > revision)
        .collect();
    let ids: HashSet<_> = newer.iter().map(|project| project.id.as_str()).collect();
    received.retain(|project| !ids.contains(project.id.as_str()));
    // Keep every newer project, including duplicates, in cached order before
    // the surviving received projects.
    received.splice(0..0, newer.into_iter().cloned());
    received
}

fn retained_project_icons(
    mut received: Vec<crate::models::Project>,
    cached: &[crate::models::Project],
) -> Vec<crate::models::Project> {
    let icons: HashMap<_, _> = cached
        .iter()
        .rev()
        .map(|project| {
            (
                (project.id.as_str(), project.roots.as_slice()),
                &project.icon,
            )
        })
        .collect();
    for project in &mut received {
        let icon = icons
            .get(&(project.id.as_str(), project.roots.as_slice()))
            .copied();
        if let Some(icon) = icon
            && &project.icon != icon
        {
            project.icon = icon.clone();
        }
    }
    received
}

fn merge_received(
    mut threads: crate::models::ThreadList,
    previous: Option<&crate::models::ThreadList>,
) -> crate::models::ThreadList {
    if let Some(previous) = previous {
        // Notifications carry complete list metadata. Preserve only changes
        // newer than this read; a later explicit refresh can remove old rows.
        threads.data = merge_newer_rows(threads.data, &previous.data, threads.revision);
        threads.projects =
            merge_newer_projects(threads.projects, &previous.projects, threads.revision);
        threads.projects = retained_project_icons(threads.projects, &previous.projects);
        // Repeated sources keep the first cached decoration.
        let statuses: HashMap<_, _> = previous
            .data
            .iter()
            .rev()
            .map(|old| {
                (
                    (
                        old.id.as_ref(),
                        old.cwd.as_deref(),
                        old.git_branch.as_deref(),
                    ),
                    old.has_changes,
                )
            })
            .collect();
        for thread in &mut threads.data {
            let status = statuses
                .get(&(
                    thread.id.as_ref(),
                    thread.cwd.as_deref(),
                    thread.git_branch.as_deref(),
                ))
                .copied();
            if let Some(status) = status {
                thread.has_changes = status;
            }
        }
    }
    if let Some(errors) = threads.provider_errors.as_ref()
        && let Some(previous) = previous
    {
        for cached in missing_failed_provider_rows(&threads.data, &previous.data, errors) {
            let mut cached = cached.clone();
            cached.list_stale = Some(true);
            cached.status = crate::models::SessionStatus::Unknown;
            threads.data.push(cached);
        }
        for project in
            missing_referenced_projects(&threads.projects, &previous.projects, &threads.data)
        {
            threads.projects.push(project.clone());
        }
    }
    threads
}

#[cfg(test)]
mod merge_tests;

impl Operation for ListProjectSessions {
    const BACKGROUND: bool = true;
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectList {
            project_id: self.project_id.clone(),
        })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestProject(self.project_id.clone())
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if snapshot.lists.threads.as_ref().is_none_or(|list| {
            !list
                .projects
                .iter()
                .any(|project| project.id == self.project_id)
        }) {
            return Vec::new();
        }
        let mut output = merge_received(
            output,
            snapshot
                .lists
                .project_threads
                .get(&self.project_id)
                .map(AsRef::as_ref),
        );
        let (data, more) = page_rows(output.data, output.limit);
        output.data = data;
        output.has_more |= more;
        Arc::make_mut(&mut snapshot.lists.unconfirmed_lists).remove(&OperationKey::ProjectList {
            project_id: self.project_id.clone(),
        });
        update_titles(snapshot, &output.data);
        if let Some(limit) =
            Arc::make_mut(&mut snapshot.lists.expanded_projects).get_mut(&self.project_id)
        {
            *limit = output.limit;
        }
        let effect = decorations(
            agent_protocol::operations::ListDecorationScope::Project {
                project_id: self.project_id.clone(),
            },
            &[],
            &output.data,
        );
        Arc::make_mut(&mut snapshot.lists.project_threads)
            .insert(self.project_id, Arc::new(output));
        effect.into_iter().collect()
    }
}

fn update_titles(snapshot: &mut Snapshot, summaries: &[crate::models::Thread]) {
    for summary in summaries {
        if let Some(id) = &summary.id
            && snapshot
                .sessions
                .conversations
                .get(id)
                .is_some_and(|thread| {
                    thread.name != summary.name && thread.list_revision <= summary.list_revision
                })
            && let Some(thread) = shared_mut(&mut snapshot.sessions.conversations, id)
        {
            thread.name = summary.name.clone();
            thread.list_revision = summary.list_revision;
        }
    }
}

fn group_project_rows<'a>(
    rows: Vec<crate::models::Thread>,
    projects: impl Iterator<Item = &'a str>,
    visible: &HashSet<&str>,
) -> (
    Vec<crate::models::Thread>,
    HashMap<&'a str, Vec<crate::models::Thread>>,
) {
    if rows.iter().all(|row| row.project_id.is_none()) {
        return (rows, HashMap::new());
    }
    let mut grouped: HashMap<_, Vec<_>> = projects.map(|id| (id, Vec::new())).collect();
    if grouped.is_empty() {
        return (rows, grouped);
    }
    let mut remaining = Vec::new();
    for row in rows {
        if let Some(id) = row.project_id.as_deref()
            && let Some(group) = grouped.get_mut(id)
        {
            if visible.contains(id) {
                group.push(row);
                continue;
            }
            // A project outside the visible catalogue also keeps its rows in
            // the root list, so only those rows need a second owned copy.
            group.push(row.clone());
        }
        remaining.push(row);
    }
    (remaining, grouped)
}

#[cfg(test)]
mod pages_tests;

impl Operation for ListSessions {
    type Input = crate::models::ListQuery;
    type Output = crate::models::ThreadList;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::list(self.query.part))
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestList(self.query.part)
    }
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        let mut query = self.query.clone();
        if query.project_limits.is_none() && !snapshot.lists.project_threads.is_empty() {
            query.project_limits = Some(
                snapshot
                    .lists
                    .expanded_projects
                    .iter()
                    .map(|(id, limit)| (id.clone(), *limit))
                    .collect(),
            );
        }
        Ok(query)
    }
    async fn run(
        &self,
        query: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context.call(&Self::new(query)).await
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if snapshot.lists.list_query.search_term != self.query.search_term {
            snapshot.lists.expanded_projects = Arc::default();
            snapshot.lists.project_threads = Arc::default();
            Arc::make_mut(&mut snapshot.lists.unconfirmed_lists)
                .retain(|key| *key == OperationKey::SessionList);
            self.query.project_limits = None;
            Arc::make_mut(&mut snapshot.operations).retain(|key, _| {
                !matches!(
                    key,
                    OperationKey::ProjectList { .. }
                        | OperationKey::SessionPage { .. }
                        | OperationKey::ListDecorations { .. }
                )
            });
        } else {
            let paging = self.query.limit > snapshot.lists.list_query.limit
                || self.query.project_limit > snapshot.lists.list_query.project_limit;
            self.query.project_limits = paging.then(Default::default);
            if !paging {
                self.query.part = None;
            }
        }
        let mut retained = self.query.clone();
        retained.project_limits = None;
        retained.part = None;
        snapshot.lists.list_query = Arc::new(retained);
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, threads: Self::Output) -> Vec<Effect> {
        let current = &snapshot.lists.list_query;
        if self.query.search_term != current.search_term
            || (self.query.part != Some(crate::models::ListPart::Projects)
                && self.query.limit != current.limit)
            || (self.query.part != Some(crate::models::ListPart::Chats)
                && self.query.project_limit != current.project_limit)
        {
            return Vec::new();
        }

        let mut threads = merge_received(threads, snapshot.lists.threads.as_deref());
        Arc::make_mut(&mut snapshot.lists.unconfirmed_lists).remove(&OperationKey::SessionList);
        let projects: Vec<_> = threads
            .projects
            .iter()
            .filter(|project| {
                self.query.part != Some(crate::models::ListPart::Projects)
                    || snapshot.lists.threads.as_ref().is_none_or(|previous| {
                        !previous.projects.iter().any(|old| old.id == project.id)
                    })
            })
            .cloned()
            .collect();
        let decoration = decorations(
            agent_protocol::operations::ListDecorationScope::Root {
                part: self.query.part,
            },
            &projects,
            &threads.data,
        );
        if let Some(previous) = snapshot.lists.threads.as_ref() {
            match self.query.part {
                Some(crate::models::ListPart::Projects) => {
                    threads.data = previous.data.clone();
                    threads.has_more = previous.has_more;
                }
                Some(crate::models::ListPart::Chats) => {
                    threads.projects = previous.projects.clone();
                    threads.has_more_projects = previous.has_more_projects;
                }
                None => {}
            }
        }
        threads
            .projects
            .truncate(snapshot.lists.list_query.project_limit as usize);
        update_titles(snapshot, &threads.data);
        let visible: std::collections::HashSet<_> =
            threads.projects.iter().map(|p| p.id.as_str()).collect();
        Arc::make_mut(&mut snapshot.lists.expanded_projects)
            .retain(|id, _| visible.contains(id.as_str()));
        if !snapshot.lists.list_query.search_term.trim().is_empty() {
            for id in &visible {
                Arc::make_mut(&mut snapshot.lists.expanded_projects)
                    .entry((*id).to_owned())
                    .or_insert(5);
            }
        }
        let pages = Arc::make_mut(&mut snapshot.lists.project_threads);
        pages.retain(|id, _| visible.contains(id.as_str()));
        Arc::make_mut(&mut snapshot.lists.unconfirmed_lists).retain(|key| match key {
            OperationKey::ProjectList { project_id } => visible.contains(project_id.as_str()),
            _ => true,
        });
        let project_pages: Vec<_> = threads
            .project_pages
            .iter()
            .filter(|(id, page)| {
                pages
                    .get(*id)
                    .is_none_or(|current| current.limit <= page.limit)
            })
            .collect();
        let (remaining, mut project_rows) = group_project_rows(
            threads.data,
            project_pages.iter().map(|(id, _)| id.as_str()),
            &visible,
        );
        threads.data = remaining;
        for (id, page) in project_pages {
            Arc::make_mut(&mut snapshot.lists.unconfirmed_lists).remove(
                &OperationKey::ProjectList {
                    project_id: id.clone(),
                },
            );
            let mut received = merge_received(
                crate::models::ThreadList {
                    revision: threads.revision,
                    data: project_rows.remove(id.as_str()).unwrap_or_default(),
                    projects: Vec::new(),
                    has_more: page.has_more,
                    has_more_projects: false,
                    limit: page.limit,
                    project_pages: Default::default(),
                    provider_errors: threads.provider_errors.clone(),
                },
                pages.get(id).map(AsRef::as_ref),
            );
            let (data, more) = page_rows(received.data, received.limit);
            received.data = data;
            received.has_more |= more;
            pages.insert(id.clone(), Arc::new(received));
        }
        // A refresh reads only expanded projects. Collapsed pages it omitted
        // stay visible but are read again when expanded.
        if self.query.part.is_none() {
            let omitted: Vec<_> = pages
                .keys()
                .filter(|id| !threads.project_pages.contains_key(*id))
                .map(|id| OperationKey::ProjectList {
                    project_id: id.clone(),
                })
                .collect();
            if !omitted.is_empty() {
                Arc::make_mut(&mut snapshot.lists.unconfirmed_lists).extend(omitted);
            }
        }
        threads.data.retain(|thread| {
            thread
                .project_id
                .as_ref()
                .is_none_or(|id| !visible.contains(id.as_str()))
        });
        threads.project_pages.clear();
        let (data, more) = page_rows(threads.data, threads.limit);
        threads.data = data;
        threads.has_more |= more;
        snapshot.lists.threads = Some(Arc::new(threads));
        decoration
            .into_iter()
            .chain(
                snapshot
                    .lists
                    .expanded_projects
                    .iter()
                    .filter(|(id, _)| {
                        snapshot.connection.connected
                            && !snapshot.lists.project_threads.contains_key(*id)
                    })
                    .map(|(id, limit)| {
                        Effect::execute(ListProjectSessions {
                            project_id: id.clone(),
                            limit: *limit,
                            search_term: snapshot.lists.list_query.search_term.clone(),
                        })
                    }),
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};

    #[allow(dead_code)]
    mod host_fixture {
        include!("../../../tests/support/host.rs");
    }

    #[test]
    fn recent_paging_does_not_read_unopened_project_pages() {
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            lists: crate::state::Lists {
                recent_chats: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let effects = ListSessions::new(Default::default()).apply(
            &mut snapshot,
            "",
            serde_json::from_value(serde_json::json!({
                "data":[],"projects":[{"id":"p","name":"P","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            }))
            .unwrap(),
        );
        assert!(effects.is_empty());
        let mut operation = ListSessions::new(crate::models::ListQuery {
            limit: 15,
            part: Some(crate::models::ListPart::Chats),
            ..Default::default()
        });
        operation.prepare(&mut snapshot, "").unwrap();
        assert!(
            operation
                .capture(&snapshot, "")
                .unwrap()
                .project_limits
                .unwrap()
                .is_empty()
        );
        let effects = operation.apply(&mut snapshot, "", serde_json::from_value(serde_json::json!({
            "data":[],"projects":[{"id":"p","name":"P","roots":[]},{"id":"q","name":"Q","roots":[]}],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":15
        })).unwrap());
        assert!(effects.is_empty());
        assert!(snapshot.lists.expanded_projects.is_empty());
        assert!(
            snapshot
                .lists
                .project_threads
                .values()
                .all(|page| page.limit == 0 && page.data.is_empty())
        );
    }

    #[test]
    fn list_and_conversation_reads_cannot_overwrite_a_newer_name_notice() {
        let id =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "task".into())
                .unwrap();
        let old: crate::models::ThreadList = serde_json::from_value(serde_json::json!({"data":[{"id":id,"name":"Old","updatedAt":1}],"projects":[],"limit":5,"hasMore":false,"hasMoreProjects":false,"projectPages":{}})).unwrap();
        let mut state = Snapshot {
            lists: crate::state::Lists {
                threads: Some(Arc::new(old.clone())),
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                conversations: Arc::new([(id.clone(), Arc::new(old.data[0].clone()))].into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let (next, effects) = super::super::super::notifications::notification(
            &state,
            crate::protocol::Notification::SessionRenamed {
                session: id.clone(),
                name: Some("New".into()),
                revision: 2,
            },
        );
        assert!(effects.is_empty());
        state = next;
        let (next, effects) = super::super::super::notifications::notification(
            &state,
            crate::protocol::Notification::SessionUpdated {
                thread: Thread {
                    id: Some(SessionRef::new(ProviderKind::Codex, "created".into()).unwrap()),
                    project_id: crate::models::ProjectMembership::Assigned("new-project".into()),
                    list_revision: 2,
                    ..Default::default()
                }
                .into(),
                project: Some(crate::models::Project {
                    id: "new-project".into(),
                    name: "Created project".into(),
                    list_revision: 2,
                    ..Default::default()
                }),
            },
        );
        assert!(effects.is_empty());
        state = next;
        ListSessions::new(Default::default()).apply(&mut state, "", old.clone());
        assert_eq!(
            state.lists.threads.as_ref().unwrap().projects[0].id,
            "new-project"
        );
        assert_eq!(
            state.lists.threads.as_ref().unwrap().data[0]
                .name
                .as_deref(),
            Some("New")
        );
        refresh_thread(&mut state, "", old.data[0].clone());
        assert_eq!(
            state.sessions.conversations[&id].name.as_deref(),
            Some("New")
        );
        let mut authoritative = old;
        authoritative.revision = 2;
        authoritative.data.clear();
        ListSessions::new(Default::default()).apply(&mut state, "", authoritative);
        assert!(
            state.lists.threads.as_ref().unwrap().data.is_empty(),
            "later refresh can remove a deleted task"
        );
        assert!(
            state.lists.threads.as_ref().unwrap().projects.is_empty(),
            "later refresh can remove a project"
        );
    }

    #[test]
    fn reconnect_retains_received_pages_and_accepts_a_restarted_host_revision() {
        let id = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        let page = serde_json::from_value(serde_json::json!({"revision":99,"data":[{"id":id,"name":"Before restart","listRevision":99}],"projects":[],"limit":15,"hasMore":false,"hasMoreProjects":false,"projectPages":{}})).unwrap();
        let snapshot = Snapshot {
            lists: crate::state::Lists {
                threads: Some(Arc::new(page)),
                ..Default::default()
            },
            ..Default::default()
        };
        let (connected, effects) = reduce(&snapshot, "", Event::Connected);
        assert_eq!(connected.lists.threads.as_ref().unwrap().limit, 15);
        assert_eq!(
            effects.len(),
            6,
            "status, clone jobs, three independent model reads, and accounts only; no list read"
        );
        let (renamed, effects) = super::super::super::notifications::notification(
            &connected,
            crate::protocol::Notification::SessionRenamed {
                session: id,
                name: Some("After restart".into()),
                revision: 1,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(
            renamed.lists.threads.as_ref().unwrap().data[0]
                .name
                .as_deref(),
            Some("After restart")
        );
    }

    #[test]
    fn recent_pages_more_and_refresh_have_independent_read_targets_and_counts() {
        use crate::models::{ListPage, ListPart, Project, Thread, ThreadList};
        let project = |id: &str| Project {
            id: id.into(),
            name: id.into(),
            ..Default::default()
        };
        let row = |id: &str| Thread {
            id: Some(SessionRef::new(ProviderKind::Codex, id.into()).unwrap()),
            project_id: crate::models::ProjectMembership::Assigned(id.into()),
            ..Default::default()
        };
        let root = ThreadList {
            revision: 0,
            data: (0..5).map(|i| row(&format!("p{i}"))).collect(),
            projects: (0..5).map(|i| project(&format!("p{i}"))).collect(),
            limit: 5,
            has_more: false,
            has_more_projects: true,
            project_pages: (0..5)
                .map(|i| {
                    (
                        format!("p{i}"),
                        ListPage {
                            limit: 5,
                            has_more: false,
                        },
                    )
                })
                .collect(),
            provider_errors: None,
        };
        let mut state = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            ListSessions::new(Default::default())
                .apply(&mut state, "", root)
                .is_empty()
        );
        assert_eq!(state.lists.project_threads.len(), 5);
        assert!(state.lists.threads.as_ref().unwrap().data.is_empty());
        let (mut state, effects) = reduce_intent(
            &state,
            "",
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: true,
            },
        );
        assert!(effects.is_empty(), "initial pages are already received");
        let received = state.lists.project_threads["p1"].clone();
        ListProjectSessions {
            project_id: "p0".into(),
            limit: 25,
            search_term: String::new(),
        }
        .apply(
            &mut state,
            "",
            ThreadList {
                revision: 0,
                data: vec![row("p0")],
                projects: vec![],
                limit: 25,
                has_more: false,
                has_more_projects: false,
                project_pages: Default::default(),
                provider_errors: None,
            },
        );
        let mut query = (*state.lists.list_query).clone();
        query.project_limit = 15;
        query.part = Some(ListPart::Projects);
        let mut more = ListSessions::new(query);
        more.prepare(&mut state, "").unwrap();
        assert_eq!(
            more.capture(&state, "").unwrap().project_limits,
            Some(Default::default())
        );
        more.apply(
            &mut state,
            "",
            ThreadList {
                revision: 0,
                data: vec![],
                projects: (0..15).map(|i| project(&format!("p{i}"))).collect(),
                limit: 5,
                has_more: false,
                has_more_projects: true,
                project_pages: Default::default(),
                provider_errors: None,
            },
        );
        assert!(Arc::ptr_eq(&received, &state.lists.project_threads["p1"]));
        assert!(!state.lists.project_threads.contains_key("p14"));
        let mut refresh = ListSessions::new((*state.lists.list_query).clone());
        refresh.prepare(&mut state, "").unwrap();
        assert_eq!(
            refresh.capture(&state, "").unwrap().project_limits,
            Some([("p0".into(), 25)].into())
        );
        let (state, _) = reduce_intent(
            &state,
            "",
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: false,
            },
        );
        let (state, effects) = reduce_intent(
            &state,
            "",
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: true,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(
            state.lists.expanded_projects["p0"], 25,
            "short pages preserve their requested count"
        );
        let (_, effects) = reduce_intent(
            &state,
            "",
            Intent::SetProjectExpanded {
                project_id: "p14".into(),
                expanded: true,
            },
        );
        assert_eq!(
            effects.len(),
            1,
            "only an unread older project needs its first page"
        );
    }

    #[test]
    fn refresh_rereads_collapsed_project_pages_when_they_expand() {
        use crate::models::{ListPage, Project, Thread, ThreadList};
        let list = |pages: &[&str], rows: &[&str]| ThreadList {
            revision: 0,
            data: rows
                .iter()
                .map(|id| Thread {
                    id: Some(SessionRef::new(ProviderKind::Codex, (*id).into()).unwrap()),
                    project_id: crate::models::ProjectMembership::Assigned("p".into()),
                    ..Default::default()
                })
                .collect(),
            projects: vec![Project {
                id: "p".into(),
                name: "p".into(),
                ..Default::default()
            }],
            limit: 5,
            project_pages: pages
                .iter()
                .map(|id| {
                    (
                        (*id).into(),
                        ListPage {
                            limit: 5,
                            has_more: false,
                        },
                    )
                })
                .collect(),
            has_more: false,
            has_more_projects: false,
            provider_errors: None,
        };
        let mut state = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        ListSessions::new(Default::default()).apply(&mut state, "", list(&["p"], &["old"]));
        let mut refresh = ListSessions::new((*state.lists.list_query).clone());
        refresh.prepare(&mut state, "").unwrap();
        assert_eq!(
            refresh.capture(&state, "").unwrap().project_limits,
            Some(Default::default()),
            "a refresh reads no collapsed project rows"
        );
        refresh.apply(&mut state, "", list(&[], &[]));
        assert_eq!(
            state.lists.project_threads["p"].data.len(),
            1,
            "the omitted page stays visible"
        );
        let (_, effects) = reduce_intent(
            &state,
            "",
            Intent::SetProjectExpanded {
                project_id: "p".into(),
                expanded: true,
            },
        );
        assert_eq!(effects.len(), 1, "expanding reads the omitted page again");
    }

    #[tokio::test]
    async fn project_reads_survive_task_navigation_and_report_failure_only_in_their_section() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            use serde_json::json;
            async fn read(reader: &mut host_fixture::Reader) -> host_fixture::Request {
                tokio::time::timeout(std::time::Duration::from_secs(30), reader.read_request())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
            }
            let initial = Snapshot {
                lists: crate::state::Lists {
                    list_query: Arc::new(crate::models::ListQuery {
                        search_term: "Archive".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
            let store = crate::store::Store::new(peer, initial, None);
            let page =
                json!({"data":[],"projects":[{"id":"old","name":"Old","roots":[]}],"hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5});
            for _ in 0..4 {
                let request = read(&mut reader).await;
                let result = match request["method"].as_str().unwrap() {
                    "host/session/list" => page.clone(),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    "host/taskActivity/read" => json!({"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                    method => panic!("unexpected {method}"),
                };
                writer
                    .reply(&request, json!({"result":result}))
                    .await
                    .unwrap();
            }
            let mut updates = store.subscribe();
            while {
                let snapshot = updates.borrow_and_update();
                snapshot.lists.threads.is_none()
                    || snapshot.operations.keys().any(|key| !matches!(key, OperationKey::ProjectList { .. } | OperationKey::SessionPage { .. } | OperationKey::ListDecorations { .. }))
            } {
                updates.changed().await.unwrap();
            }
            assert!(store.snapshot().thread_list().unwrap().projects[0].expanded);
            let request = read(&mut reader).await;
            assert_eq!(request["method"], "host/project/sessions");
            assert_eq!(request["params"], json!({"projectId":"old","limit":5,"searchTerm":"Archive"}));
            assert!(store.snapshot().thread_list().unwrap().projects[0].loading);
            assert!(
                store
                    .snapshot()
                    .operations.get(&OperationKey::SessionList)
                    .is_none()
            );
            let recent = store.snapshot().lists.threads.clone().unwrap();
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            writer
                .reply(&request, json!({"result":page}))
                .await
                .unwrap();
            while {
                let snapshot = updates.borrow_and_update();
                !snapshot.lists.project_threads.contains_key("old")
            }
            {
                updates.changed().await.unwrap();
            }
            assert!(Arc::ptr_eq(
                &recent,
                store.snapshot().lists.threads.as_ref().unwrap()
            ));
            store
                .dispatch(Intent::RefreshProject {
                    project_id: "old".into(),
                })
                .await
                .unwrap();
            let request = read(&mut reader).await;
            assert_eq!(request["params"]["searchTerm"], "Archive");
            writer.reply(&request,json!({"error":{"code":"provider_failed","message":"fixture unavailable","delivery":"notSent"}})).await.unwrap();
            while {
                let snapshot = updates.borrow_and_update();
                snapshot.thread_list().unwrap().projects[0].loading
            } {
                updates.changed().await.unwrap();
            }
            assert!(store.snapshot().panes.primary().error.is_none());
            assert!(
                store.snapshot().thread_list().unwrap().projects[0]
                    .error
                    .as_ref()
                    .unwrap()
                    .to_string()
                    .contains("fixture unavailable")
            );
            store
                .dispatch(Intent::SetProjectExpanded {
                    project_id: "old".into(),
                    expanded: false,
                })
                .await
                .unwrap();
            let received = store.snapshot().lists.project_threads["old"].clone();
            assert!(
                store.snapshot().thread_list().unwrap().projects[0]
                    .error
                    .is_none()
            );
            store.dispatch(Intent::SetProjectExpanded {
                project_id: "old".into(), expanded: true,
            }).await.unwrap();
            assert!(Arc::ptr_eq(&received, &store.snapshot().lists.project_threads["old"]));
            assert!(!store.snapshot().thread_list().unwrap().projects[0].loading);
            assert!(tokio::time::timeout(std::time::Duration::from_millis(50), reader.read_request()).await.is_err(),
                "a received page must not be read again on reopening");
            store.close().await.unwrap();
        })
        .await
        .expect("project lifecycle deadline");
    }

    #[tokio::test]
    async fn fleet_reads_coalesce_and_do_not_block_the_root_list_or_publish_after_closing() {
        async fn read(reader: &mut host_fixture::Reader) -> host_fixture::Request {
            tokio::time::timeout(std::time::Duration::from_secs(30), reader.read_request())
                .await
                .expect("fleet request was not sent")
                .unwrap()
                .unwrap()
        }
        use serde_json::json;
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let snapshot = Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        parent.clone(),
                        Arc::new(Thread {
                            id: Some(parent.clone()),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
                crate::state::PaneState {
                    navigation: Arc::new(Navigation {
                        thread_id: Some(parent.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let (peer, mut reader, writer) = host_fixture::connect(&snapshot).await;
        let store = crate::store::Store::new(peer, snapshot, None);
        let empty_list = json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5});
        for _ in 0..5 {
            let request = read(&mut reader).await;
            let result = match request["method"].as_str().unwrap() {
                "host/session/list" => empty_list.clone(),
                "host/account/list" => json!({"accounts":[],"selected":{}}),
                "host/taskActivity/read" => {
                    json!({"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
                }
                "host/model/list" => json!({"data":[],"nextCursor":null}),
                "host/session/open" => json!({"thread":{"id":parent}}),
                method => panic!("unexpected initial request {method}"),
            };
            writer
                .reply(&request, json!({"result":result}))
                .await
                .unwrap();
        }
        let mut updates = store.subscribe();
        loop {
            let ready = {
                let snapshot = updates.borrow_and_update();
                snapshot.lists.threads.is_some()
                    && snapshot.catalog.account.accounts.is_some()
                    && snapshot.sessions.subscriptions.contains_key(&parent)
                    && snapshot.operations.is_empty()
            };
            if ready {
                break;
            }
            updates.changed().await.unwrap();
        }
        let before = store.snapshot();
        let first = store.dispatch(Intent::WatchAgents {
            thread_id: Some(parent.clone()),
        });
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/session/agents");
        assert_eq!(request["params"], json!({"threadId":parent}));
        let mut receipts = Vec::new();
        for _ in 0..20 {
            store
                .dispatch(Intent::WatchAgents { thread_id: None })
                .await
                .unwrap();
            receipts.push(store.dispatch(Intent::WatchAgents {
                thread_id: Some(parent.clone()),
            }));
        }
        let listing = store.dispatch(Intent::ListSessions(ListSessions::new(Default::default())));
        let list_request = read(&mut reader).await;
        assert_eq!(
            list_request["method"], "host/session/list",
            "fleet requests must coalesce while the first response is pending"
        );
        writer
            .reply(&list_request, json!({"result":empty_list}))
            .await
            .unwrap();
        listing.await.unwrap();
        writer.reply(&request, json!({"result":[{"id":{"provider":"codex","id":"obsolete"},"parentId":parent,"name":"Old","status":"running"}]})).await.unwrap();
        first.await.unwrap();
        assert!(
            store
                .snapshot()
                .agent_panel(store.snapshot().panes.primary())
                .is_empty(),
            "a superseded fleet response must not publish stale rows"
        );
        let mut latest = read(&mut reader).await;
        if latest["method"] == "host/taskActivity/read" {
            writer.reply(&latest, json!({"result":{"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
            latest = read(&mut reader).await;
        }
        assert_eq!(latest["method"], "host/session/agents");
        store
            .dispatch(Intent::WatchAgents { thread_id: None })
            .await
            .unwrap();
        writer.reply(&latest, json!({"result":[{"id":{"provider":"codex","id":"child"},"parentId":parent,"name":"Review","status":"running"}]})).await.unwrap();
        for receipt in receipts {
            receipt.await.unwrap();
        }
        assert!(
            store
                .snapshot()
                .agent_panel(store.snapshot().panes.primary())
                .is_empty(),
            "closing the panel discards an in-flight fleet response"
        );
        let watching = store.dispatch(Intent::WatchAgents {
            thread_id: Some(parent.clone()),
        });
        let latest = read(&mut reader).await;
        writer.reply(&latest, json!({"result":[{"id":{"provider":"codex","id":"child"},"parentId":parent,"name":"Review","status":"running"}]})).await.unwrap();
        watching.await.unwrap();
        let after = store.snapshot();
        assert_eq!(after.agent_panel(after.panes.primary())[0].title, "Review");
        assert!(after.agent_panel(after.panes.primary())[0].active);
        assert_eq!(
            after.panes.primary().navigation,
            before.panes.primary().navigation
        );
        assert_eq!(after.input.drafts, before.input.drafts);
        store
            .dispatch(Intent::WatchAgents { thread_id: None })
            .await
            .unwrap();
        let first = store.dispatch(Intent::WatchAgents {
            thread_id: Some(parent.clone()),
        });
        let request = read(&mut reader).await;
        store
            .dispatch(Intent::WatchAgents { thread_id: None })
            .await
            .unwrap();
        let queued = store.dispatch(Intent::WatchAgents {
            thread_id: Some(parent),
        });
        store
            .dispatch(Intent::WatchAgents { thread_id: None })
            .await
            .unwrap();
        writer.reply(&request, json!({"result":[]})).await.unwrap();
        first.await.unwrap();
        queued.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.read_request())
                .await
                .is_err(),
            "closing the panel must also discard the queued read before sending it"
        );
        store.close().await.unwrap();
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(5)]
    #[case(15)]
    fn project_disclosure_keeps_received_rows_and_finishes_reads_while_closed(
        #[case] count: usize,
    ) {
        let root = |title: &str| {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":title},"name":title}],
                "projects":[{"id":"old","name":"Old","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":count.max(5) as u32
            }))
            .unwrap()
        };
        let project = |title: &str| {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({
                "data":(0..count).map(|index| serde_json::json!({"id":{"provider":"codex","id":format!("{title}-{index}")},"projectId":"old","name":title})).collect::<Vec<_>>(),
                "projects":[{"id":"old","name":"Old","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":count.max(5) as u32
            }))
            .unwrap()
        };
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        ListSessions::new(Default::default()).apply(&mut snapshot, "", root("chat"));
        let selected = SessionRef::new(ProviderKind::Codex, "selected".into()).unwrap();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(selected.clone());
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            selected.clone().into(),
            Arc::new(Draft {
                text: "Keep this draft".into(),
                ..Default::default()
            }),
        );
        let (opened, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            },
        );
        assert_eq!(effects.len(), 1);
        let mut published = opened;
        let root_before = published.lists.threads.clone().unwrap();
        let refresh_effects =
            ListSessions::new(Default::default()).apply(&mut published, "", root("new-chat"));
        assert_eq!(refresh_effects.len(), 1);
        assert!(!Arc::ptr_eq(
            &root_before,
            published.lists.threads.as_ref().unwrap()
        ));
        if count > 5 {
            (published, _) = reduce_intent(
                &published,
                "",
                Intent::ExpandThreadList {
                    project_id: Some("old".into()),
                },
            );
        }
        let limit = published.lists.expanded_projects["old"];
        ListProjectSessions {
            project_id: "old".into(),
            limit,
            search_term: String::new(),
        }
        .apply(&mut published, "", project("old-task"));
        assert_eq!(published.lists.project_threads["old"].data.len(), count);
        let received = published.lists.project_threads["old"].clone();
        let navigation = published.panes.primary().navigation.clone();
        let drafts = published.input.drafts.clone();
        let (mut closed, effects) = reduce_intent(
            &published,
            "",
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: false,
            },
        );
        assert!(effects.is_empty());
        ListProjectSessions {
            project_id: "old".into(),
            limit,
            search_term: String::new(),
        }
        .apply(&mut closed, "", project("late-task"));
        assert!(!Arc::ptr_eq(
            &received,
            &closed.lists.project_threads["old"]
        ));
        assert!(
            closed.lists.project_threads["old"]
                .data
                .iter()
                .all(|row| row.name.as_deref() == Some("late-task"))
        );
        assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
        let (reopened, effects) = reduce_intent(
            &closed,
            "",
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(
            reopened.thread_list().unwrap().projects[0].threads.len(),
            count
        );
        assert_eq!(reopened.lists.expanded_projects["old"], count.max(5) as u32);
        assert!(!reopened.thread_list().unwrap().projects[0].loading);
        assert_eq!(closed.panes.primary().navigation, navigation);
        assert_eq!(closed.input.drafts, drafts);
        ListSessions::new(crate::models::ListQuery {
            search_term: "other".into(),
            ..Default::default()
        })
        .prepare(&mut closed, "")
        .unwrap();
        assert!(closed.lists.project_threads.is_empty());
    }

    #[test]
    fn searching_root_auto_expands_visible_projects_and_queues_scoped_reads() {
        let page = || {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({
                "data":[],
                "projects":[{"id":"matching","name":"Matching","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            }))
            .unwrap()
        };
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut search = ListSessions::new(crate::models::ListQuery {
            search_term: "needle".into(),
            ..Default::default()
        });
        search.prepare(&mut snapshot, "").unwrap();
        let effects = search.apply(&mut snapshot, "", page());
        assert_eq!(snapshot.lists.expanded_projects["matching"], 5);
        assert_eq!(effects.len(), 1);
        assert!(snapshot.thread_list().unwrap().projects[0].expanded);

        let mut clear = ListSessions::new(Default::default());
        clear.prepare(&mut snapshot, "").unwrap();
        clear.apply(&mut snapshot, "", page());
        assert!(snapshot.lists.expanded_projects.is_empty());
        assert!(!snapshot.thread_list().unwrap().projects[0].expanded);
    }

    #[test]
    fn root_pagination_keeps_loaded_project_pages_without_refetching_them() {
        let root_page = |title: &str| {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":title},"name":title}],
                "projects":[{"id":"project","name":"Project","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            }))
            .unwrap()
        };
        let project_page = || {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":"project-task"},"projectId":"project","name":"Project task"}],
                "projects":[{"id":"project","name":"Project","roots":[]}],
                "hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5
            }))
            .unwrap()
        };
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        ListSessions::new(Default::default()).apply(&mut snapshot, "", root_page("initial"));
        let (opened, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::SetProjectExpanded {
                project_id: "project".into(),
                expanded: true,
            },
        );
        assert_eq!(effects.len(), 1);
        let mut opened = opened;
        ListProjectSessions {
            project_id: "project".into(),
            limit: 5,
            search_term: String::new(),
        }
        .apply(&mut opened, "", project_page());
        let loaded = opened.lists.project_threads["project"].clone();
        let (mut paginating, effects) =
            reduce_intent(&opened, "", Intent::ExpandThreadList { project_id: None });
        assert_eq!(effects.len(), 1);
        let root_query = (*paginating.lists.list_query).clone();
        let root_effects =
            ListSessions::new(root_query).apply(&mut paginating, "", root_page("next"));
        assert!(root_effects.is_empty());
        assert!(Arc::ptr_eq(
            &loaded,
            &paginating.lists.project_threads["project"]
        ));
        assert_eq!(
            paginating.lists.project_threads["project"].data[0]
                .name
                .as_deref(),
            Some("Project task")
        );
    }

    #[test]
    fn late_root_page_keeps_project_pagination_and_its_pending_request() {
        let mut snapshot = Snapshot::default();
        let page: crate::models::ThreadList = serde_json::from_value(serde_json::json!({
            "data":[{"id":{"provider":"codex","id":"old"},"projectId":"project"}],
            "projects":[{"id":"project","name":"Project","roots":[]}],
            "hasMore":false,"hasMoreProjects":false,"limit":5,
            "projectPages":{"project":{"limit":5,"hasMore":true}}
        }))
        .unwrap();
        let loaded = Arc::new(crate::models::ThreadList {
            revision: 0,
            limit: 15,
            data: vec![crate::models::Thread {
                id: Some(SessionRef::new(ProviderKind::Codex, "new".into()).unwrap()),
                project_id: crate::models::ProjectMembership::Assigned("project".into()),
                ..Default::default()
            }],
            projects: Vec::new(),
            has_more: false,
            has_more_projects: false,
            project_pages: Default::default(),
            provider_errors: None,
        });
        Arc::make_mut(&mut snapshot.lists.project_threads).insert("project".into(), loaded.clone());
        let key = OperationKey::ProjectList {
            project_id: "project".into(),
        };
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        ListSessions::new(Default::default()).apply(&mut snapshot, "", page.clone());
        assert!(
            Arc::ptr_eq(&loaded, &snapshot.lists.project_threads["project"]),
            "A delayed five-row root response cannot replace the fifteen-row project page"
        );
        assert!(
            snapshot.operations.get(&key).is_some(),
            "Only the project request can finish its own pending pagination"
        );
        let mut refresh = page;
        refresh.project_pages.get_mut("project").unwrap().limit = 15;
        ListSessions::new(Default::default()).apply(&mut snapshot, "", refresh);
        assert_eq!(
            snapshot.lists.project_threads["project"].data[0]
                .id
                .as_ref()
                .unwrap()
                .id,
            "old",
            "Refreshing the same requested count must replace stale rows"
        );
        assert!(snapshot.operations.get(&key).is_some());
    }

    #[test]
    fn agents_are_loaded_only_while_the_selected_fleet_is_observed() {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(parent.clone());
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            parent.clone().into(),
            Arc::new(Draft {
                text: "Keep my draft".into(),
                ..Default::default()
            }),
        );
        let (_, effects) = reduce(&snapshot, "", Event::Connected);
        assert_eq!(
            effects.len(),
            8,
            "reconnecting loads task aggregate, clone jobs and each model provider without an unobserved fleet"
        );
        let (ignored, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::WatchAgents {
                thread_id: Some(other),
            },
        );
        assert!(effects.is_empty());
        assert!(ignored.panes.primary().observed_agents.is_none());
        let (watching, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::WatchAgents {
                thread_id: Some(parent.clone()),
            },
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(
            watching.panes.primary().observed_agents,
            Some(parent.clone())
        );
        assert_eq!(
            watching.panes.primary().navigation,
            snapshot.panes.primary().navigation
        );
        assert_eq!(watching.input.drafts, snapshot.input.drafts);
        let (_, effects) = reduce_intent(
            &watching,
            "",
            Intent::WatchAgents {
                thread_id: Some(parent.clone()),
            },
        );
        assert!(
            effects.is_empty(),
            "rendering the same panel must not refetch"
        );
        let (_, effects) = reduce(&watching, "", Event::Connected);
        assert_eq!(
            effects.len(),
            9,
            "reconnect refreshes clone jobs, each model provider and only the observed fleet"
        );
        let (closed, effects) =
            reduce_intent(&watching, "", Intent::WatchAgents { thread_id: None });
        assert!(effects.is_empty());
        assert!(closed.panes.primary().observed_agents.is_none());
        let mut offline = closed.clone();
        offline.connection.connected = false;
        let (offline, effects) = reduce_intent(
            &offline,
            "",
            Intent::WatchAgents {
                thread_id: Some(parent),
            },
        );
        assert!(effects.is_empty());
        assert!(
            offline.panes.primary().observed_agents.is_some(),
            "offline observations survive until reconnect"
        );
    }

    #[test]
    fn fleet_metadata_preserves_history_and_ignores_results_after_closing() {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
        let history = Arc::new(Thread {
            id: Some(child.clone()),
            turns: Some(vec![Arc::new(crate::models::Turn {
                id: "history".into(),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let mut snapshot = Snapshot {
            panes: crate::state::PaneStates::with_primary(Arc::new(crate::state::PaneState {
                observed_agents: Some(parent.clone()),
                ..Default::default()
            })),
            sessions: crate::state::Sessions {
                conversations: Arc::new([(child.clone(), history.clone())].into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let observations = vec![agent_protocol::models::AgentObservation {
            id: child.clone(),
            parent_id: parent.clone(),
            name: Some("Review".into()),
            status: crate::models::SessionStatus::Running,
        }];
        ListAgents {
            thread_id: parent.clone(),
        }
        .apply(&mut snapshot, "", observations.clone());
        let thread = &snapshot.sessions.conversations[&child];
        assert_eq!(thread.parent_id, Some(parent.clone()));
        assert_eq!(thread.name.as_deref(), Some("Review"));
        assert_eq!(thread.status, crate::models::SessionStatus::Running);
        assert_eq!(thread.turns, history.turns);
        Arc::make_mut(snapshot.panes.primary_mut()).observed_agents = None;
        let conversations = snapshot.sessions.conversations.clone();
        ListAgents { thread_id: parent }.apply(&mut snapshot, "", observations);
        assert!(Arc::ptr_eq(
            &snapshot.sessions.conversations,
            &conversations
        ));
    }
}
