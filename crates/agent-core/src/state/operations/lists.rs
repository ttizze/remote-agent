//! Conversation lists: recent chats, project pages, decorations and sub-agents.
use super::*;
use std::collections::HashMap;

pub use agent_protocol::operations::AddProject;

impl Operation for AddProject {
    rpc_operation!();
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, cwd: Self::Output) -> Vec<Effect> {
        let (next, mut effects) = reduce_intent(snapshot, pane_id, Intent::NewChat { cwd });
        let query = (*next.lists.list_query).clone();
        let (next, refresh) = super::super::list_sync::query(next, query, false);
        *snapshot = next;
        effects.extend(refresh);
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _pane_id: &str, _: Self::Output) -> Vec<Effect> {
        let query = (*snapshot.lists.list_query).clone();
        let (next, effects) = super::super::list_sync::query(snapshot.clone(), query, false);
        *snapshot = next;
        effects
    }
}

pub use agent_protocol::operations::ListAgents;

pub use agent_protocol::operations::ReadListDecorations;

pub(crate) fn list_decorations(
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
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
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

#[cfg(test)]
mod agent_tests {
    use super::*;
    #[test]
    fn fleet_observation_survives_offline_and_metadata_keeps_history_until_the_panel_closes() {
        let root = crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let child =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "child".into())
                .unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(root.clone());
        let (mut offline, effects) = super::super::super::reduce(
            &snapshot,
            "",
            Event::Intent(Intent::WatchAgents {
                thread_id: Some(root.clone()),
            }),
        );
        assert!(effects.is_empty());
        assert_eq!(offline.panes.primary().observed_agents, Some(root.clone()));
        let (_, effects) = super::super::super::reduce(&offline, "", Event::Connected);
        let (_, without_fleet) = super::super::super::reduce(&snapshot, "", Event::Connected);
        assert_eq!(
            effects.len(),
            without_fleet.len() + 1,
            "reconnect resumes the observed fleet"
        );
        let turn = Arc::new(crate::models::Turn {
            id: "kept turn".into(),
            ..Default::default()
        });
        Arc::make_mut(&mut offline.sessions.conversations).insert(
            child.clone(),
            Arc::new(crate::models::Thread {
                id: Some(child.clone()),
                turns: Some(vec![turn.clone()]),
                ..Default::default()
            }),
        );
        let observation = || agent_protocol::models::AgentObservation {
            id: child.clone(),
            parent_id: root.clone(),
            name: Some("updated child".into()),
            status: crate::models::SessionStatus::Running,
        };
        ListAgents {
            thread_id: root.clone(),
        }
        .apply(&mut offline, "", vec![observation()]);
        assert!(Arc::ptr_eq(
            &offline.sessions.conversations[&child]
                .turns
                .as_ref()
                .unwrap()[0],
            &turn
        ));
        assert_eq!(
            offline.sessions.conversations[&child].name.as_deref(),
            Some("updated child")
        );
        let (mut closed, _) = super::super::super::reduce(
            &offline,
            "",
            Event::Intent(Intent::WatchAgents { thread_id: None }),
        );
        let before = closed.clone();
        assert!(
            ListAgents {
                thread_id: root.clone()
            }
            .apply(&mut closed, "", vec![observation()])
            .is_empty()
        );
        assert_eq!(
            closed, before,
            "a late fleet reply cannot publish after its last observer closes"
        );
    }
}
