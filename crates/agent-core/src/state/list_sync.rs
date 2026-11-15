//! One shell supplies local pages and activity. Page limits never alter
//! the replicated scope or send another native catalog read.
use super::*;
use crate::{peer::PeerError, store::Execution};
use agent_protocol::{
    conversation::records,
    orchestration::shell::{self, ShellSnapshot, ShellStreamItem},
    sync::Scope,
};
use op::Operation;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) struct SubscribeCatalog {
    pub refresh: bool,
}

#[derive(Debug)]
struct SearchCatalog {
    term: String,
}

impl Operation for SearchCatalog {
    fn key(&self) -> Option<op::OperationKey> {
        Some(op::OperationKey::ListSearch)
    }
    fn scheduling(&self) -> op::Scheduling {
        op::Scheduling::LatestHostState(self.key().expect("search key"))
    }
    type Input = ();
    fn capture(&self, _: &Snapshot, _: &str) -> Result<(), PeerError> {
        Ok(())
    }
    type Output = ThreadList;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context
            .call(&records::SearchList {
                term: self.term.clone(),
                limit: Some(records::DEFAULT_SEARCH_LIMIT),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, _: &str, results: ThreadList) -> Vec<Effect> {
        if snapshot.lists.list_query.search_term != self.term {
            return Vec::new();
        }
        snapshot.lists.search_results = Some((self.term, Arc::new(results)));
        project(snapshot, false)
    }
}

pub(crate) fn reconnect_search(snapshot: &Snapshot) -> Option<Effect> {
    let term = &snapshot.lists.list_query.search_term;
    (!term.trim().is_empty()).then(|| Effect::execute(SearchCatalog { term: term.clone() }))
}

fn search_rows(
    term: &str,
    results: Option<&ThreadList>,
    current: Vec<crate::models::Thread>,
    projects: Vec<crate::models::Project>,
) -> (Vec<crate::models::Thread>, Vec<crate::models::Project>) {
    let term = term.trim().to_lowercase();
    let mut rows = results
        .into_iter()
        .flat_map(|results| &results.data)
        .filter_map(|thread| thread.id.clone().map(|id| (id, thread.clone())))
        .collect::<BTreeMap<_, _>>();
    for thread in current {
        let Some(id) = thread.id.clone() else {
            continue;
        };
        if rows.contains_key(&id)
            || format!(
                "{} {}",
                thread.name.as_deref().unwrap_or_default(),
                thread.preview.as_deref().unwrap_or_default()
            )
            .to_lowercase()
            .contains(&term)
        {
            rows.insert(id, thread);
        }
    }
    let matching = rows
        .values()
        .filter_map(|thread| thread.project_id.as_deref())
        .collect::<std::collections::HashSet<_>>();
    let mut groups = results
        .into_iter()
        .flat_map(|results| &results.projects)
        .map(|project| (project.id.clone(), project.clone()))
        .collect::<BTreeMap<_, _>>();
    for project in projects {
        if groups.contains_key(&project.id)
            || matching.contains(project.id.as_str())
            || project.name.to_lowercase().contains(&term)
        {
            groups.insert(project.id.clone(), project);
        }
    }
    (rows.into_values().collect(), groups.into_values().collect())
}

pub(crate) fn query(
    mut snapshot: Snapshot,
    query: ListQuery,
    refresh: bool,
) -> (Snapshot, Vec<Effect>) {
    let changed = query.search_term != snapshot.lists.list_query.search_term;
    if changed {
        snapshot.lists.search_results = None;
        snapshot.lists.search_expanded_projects = Arc::default();
    }
    let searching = !query.search_term.trim().is_empty();
    let term = query.search_term.clone();
    snapshot.lists.list_query = Arc::new(query);
    let mut effects = project(&mut snapshot, refresh);
    if snapshot.connection.connected {
        if snapshot.lists.subscription.is_none()
            && !snapshot.operation_running(op::OperationKey::SessionList)
            || refresh && !searching
        {
            effects.push(Effect::execute(SubscribeCatalog { refresh }));
        }
        if searching && (changed || refresh || snapshot.lists.search_results.is_none()) {
            effects.push(Effect::execute(SearchCatalog { term }));
        }
    }
    (snapshot, effects)
}

pub(crate) fn refresh_project(snapshot: &Snapshot, project_id: &str) -> Vec<Effect> {
    if !snapshot.connection.connected {
        return Vec::new();
    }
    let projects = snapshot
        .lists
        .threads
        .as_ref()
        .map(|page| {
            page.projects
                .iter()
                .filter(|project| project.id == project_id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let threads = snapshot
        .lists
        .project_threads
        .get(project_id)
        .map(|page| page.data.as_slice())
        .unwrap_or_default();
    op::list_decorations(
        agent_protocol::operations::ListDecorationScope::Project {
            project_id: project_id.into(),
        },
        &projects,
        threads,
    )
    .into_iter()
    .collect()
}

impl Operation for SubscribeCatalog {
    fn key(&self) -> Option<op::OperationKey> {
        Some(op::OperationKey::SessionList)
    }
    fn scheduling(&self) -> op::Scheduling {
        op::Scheduling::LatestHostState(op::OperationKey::SessionList)
    }
    fn subscription_scope(&self) -> Option<Scope<crate::session::SessionRef>> {
        Some(Scope::List)
    }
    type Input = (u64, Option<Arc<ShellSnapshot>>);
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        Ok((snapshot.lists.generation, snapshot.lists.shell.clone()))
    }
    type Output = (
        u64,
        Option<u64>,
        ShellSnapshot,
        uuid::Uuid,
        serde_json::Map<String, Value>,
    );
    async fn run(
        &self,
        (generation, base): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let after = base.as_ref().map(|base| base.snapshot_sequence);
        let response = context
            .call(&records::SubscribeList {
                refresh: self.refresh,
                subscription: uuid::Uuid::nil(),
                after_sequence: after,
            })
            .await?;
        let mut opened = base.as_deref().cloned();
        for item in response.initial {
            opened = crate::orchestration::shell::receive(opened.as_ref(), item, true)
                .map_err(PeerError::InvalidMessage)?;
        }
        Ok((
            generation,
            after,
            opened.ok_or_else(|| {
                PeerError::InvalidMessage("list bootstrap has no snapshot".into())
            })?,
            response.subscription_id,
            response.provider_errors,
        ))
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        _: &str,
        (generation, base_sequence, opened, subscription, provider_errors): Self::Output,
    ) -> Vec<Effect> {
        if snapshot.lists.generation != generation {
            return Vec::new();
        }
        let shell = match &snapshot.lists.shell {
            Some(current)
                if base_sequence.is_some_and(|base| current.snapshot_sequence > base)
                    && opened.snapshot_sequence < current.snapshot_sequence =>
            {
                current.clone()
            }
            _ => Arc::new(opened),
        };
        let mut effects = discovered_children(snapshot, &shell);
        snapshot.lists.shell = Some(shell);
        snapshot.lists.subscription = Some(subscription);
        snapshot.lists.provider_errors = Arc::new(provider_errors);
        effects.extend(project(snapshot, false));
        effects
    }
}

fn discovered_children(snapshot: &Snapshot, incoming: &ShellSnapshot) -> Vec<Effect> {
    let roots = incoming
        .threads
        .iter()
        .filter_map(|shell| {
            if snapshot.lists.shell.as_ref().is_some_and(|previous| {
                previous
                    .threads
                    .iter()
                    .any(|old| old.thread.id == shell.thread.id)
            }) {
                return None;
            }
            if shell.thread.lineage.relationship_to_parent.as_deref() != Some("subagent") {
                return None;
            }
            let parent = shell.thread.lineage.parent_thread_id.as_deref()?;
            snapshot
                .panes
                .0
                .values()
                .filter_map(|pane| pane.observed_agents.as_ref())
                .find(|observed| observed.application_id() == parent)
        })
        .collect::<BTreeSet<_>>();
    roots
        .into_iter()
        .flat_map(|root| op::refresh_agents(snapshot.connection.connected, Some(root)))
        .collect()
}

pub(crate) fn received(
    previous: &Snapshot,
    subscription: uuid::Uuid,
    message: ShellStreamItem,
) -> (Snapshot, Vec<Effect>) {
    if previous.lists.subscription != Some(subscription) {
        return (previous.clone(), Vec::new());
    }
    let apply =
        crate::orchestration::shell::receive(previous.lists.shell.as_deref(), message, false);
    let mut next = previous.clone();
    match apply {
        Ok(Some(shell)) => {
            if previous.lists.shell.as_deref() == Some(&shell) {
                return (next, Vec::new());
            }
            let mut effects = discovered_children(previous, &shell);
            next.lists.shell = Some(Arc::new(shell));
            effects.extend(project(&mut next, false));
            (next, effects)
        }
        Ok(None) | Err(_) => {
            next.lists.shell = None;
            next.lists.subscription = None;
            (
                next,
                vec![Effect::execute(SubscribeCatalog { refresh: false })],
            )
        }
    }
}

pub(crate) fn project(snapshot: &mut Snapshot, refresh: bool) -> Vec<Effect> {
    let Some(shell) = &snapshot.lists.shell else {
        return Vec::new();
    };
    let mut projects = shell
        .projects
        .iter()
        .map(|project| crate::orchestration::shell::project(project))
        .collect();
    let mut summaries = Vec::new();
    let mut errors = snapshot.lists.provider_errors.as_ref().clone();
    for thread in &shell.threads {
        let root = thread
            .thread
            .project_id
            .as_ref()
            .and_then(|id| shell.projects.iter().find(|project| &project.id == id))
            .map(|project| project.workspace_root.as_str());
        match agent_protocol::orchestration::shell::summary(thread, root) {
            Ok(mut summary) => {
                if let Some(error) = snapshot
                    .lists
                    .provider_errors
                    .get(&thread.thread.provider_instance_id)
                {
                    summary.list_stale = Some(true);
                    summary = records::unavailable(
                        summary,
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("AIを起動できませんでした。"),
                    );
                }
                summaries.push(summary);
            }
            Err(error) => {
                errors.insert(
                    thread.thread.provider_instance_id.clone(),
                    serde_json::json!({"message":error}),
                );
            }
        }
    }
    let Ok(mut activity) = shell::activity(shell.threads.iter().map(AsRef::as_ref)) else {
        return Vec::new();
    };
    if snapshot.lists.subscription.is_none() {
        for (_, status) in &mut activity.statuses {
            *status = crate::models::SessionStatus::Unknown;
        }
        activity.waiting.clear();
        activity.background.clear();
        activity.attention.clear();
        for summary in &mut summaries {
            summary.status = crate::models::SessionStatus::Unknown;
        }
    }
    let mut effects = Vec::new();
    for thread in &summaries {
        if let Some(id) = &thread.id {
            super::conversations::promote_draft(snapshot, id, thread.cwd.as_deref());
        }
    }
    let unread = activity
        .unread
        .iter()
        .filter(|(id, _)| snapshot.panes.primary().navigation.thread_id.as_ref() != Some(id))
        .map(|(id, _)| id.clone())
        .collect();
    let active = summaries
        .iter()
        .filter_map(|thread| {
            let id = thread.id.clone()?;
            match thread.status {
                crate::models::SessionStatus::Preparing | crate::models::SessionStatus::Running => {
                    Some((id, true))
                }
                crate::models::SessionStatus::Idle => Some((id, false)),
                _ => None,
            }
        })
        .collect();
    let previous = snapshot.lists.task_activity.as_ref();
    for (id, turn) in &activity.unread {
        if snapshot.connection.connected
            && snapshot.panes.primary().navigation.thread_id.as_ref() == Some(id)
            && previous
                .is_none_or(|previous| !previous.unread.contains(&(id.clone(), turn.clone())))
        {
            effects.push(Effect::execute(op::MarkSessionRead {
                session: id.clone(),
                turn_id: turn.clone(),
            }));
        }
    }
    if snapshot.lists.activity.active != active || snapshot.lists.activity.unread != unread {
        snapshot.lists.activity = Arc::new(Activity { active, unread });
    }
    snapshot.prune_subscriptions();
    if snapshot.lists.task_activity.as_deref() != Some(&activity) {
        snapshot.lists.task_activity = Some(Arc::new(activity));
    }
    if !snapshot.lists.list_query.search_term.trim().is_empty() {
        let results = snapshot
            .lists
            .search_results
            .as_ref()
            .filter(|(term, _)| term == &snapshot.lists.list_query.search_term);
        (summaries, projects) = search_rows(
            &snapshot.lists.list_query.search_term,
            results.map(|(_, page)| page.as_ref()),
            summaries,
            projects,
        );
        if let Some((_, page)) = results
            && let Some(provider_errors) = &page.provider_errors
        {
            errors.extend(provider_errors.clone());
        }
    }
    effects.extend(pages(
        snapshot,
        projects,
        summaries,
        (!errors.is_empty()).then_some(errors),
        refresh,
    ));
    effects
}

fn pages(
    snapshot: &mut Snapshot,
    mut projects: Vec<crate::models::Project>,
    summaries: Vec<Thread>,
    errors: Option<serde_json::Map<String, serde_json::Value>>,
    refresh: bool,
) -> Vec<Effect> {
    let known_projects = projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut groups = BTreeMap::<_, Vec<Thread>>::new();
    let mut standalone = Vec::new();
    for thread in summaries {
        if thread.parent_id.is_some() && snapshot.lists.list_query.search_term.trim().is_empty() {
            continue;
        }
        if let Some(project) = thread
            .project_id
            .as_deref()
            .filter(|id| known_projects.contains(id))
        {
            groups.entry(project.to_owned()).or_default().push(thread)
        } else {
            standalone.push(thread)
        }
    }
    let newest = groups
        .iter()
        .map(|(id, rows)| {
            (
                id.as_str(),
                rows.iter()
                    .filter_map(|row| row.updated_at)
                    .max_by(f64::total_cmp)
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    projects.sort_by(|a, b| {
        newest
            .get(b.id.as_str())
            .copied()
            .unwrap_or_default()
            .total_cmp(&newest.get(a.id.as_str()).copied().unwrap_or_default())
    });
    let has_more_projects = projects.len() > snapshot.lists.list_query.project_limit as usize;
    projects.truncate(snapshot.lists.list_query.project_limit as usize);
    let searching = !snapshot.lists.list_query.search_term.trim().is_empty();
    if searching {
        let choices = Arc::make_mut(&mut snapshot.lists.search_expanded_projects);
        let preferences = snapshot
            .lists
            .project_preferences
            .get(&snapshot.connection.storage_scope);
        for project in &projects {
            if !preferences
                .and_then(|prefs| prefs.get(&project.id))
                .is_some_and(|prefs| prefs.hidden)
            {
                choices.entry(project.id.clone()).or_insert(5);
            }
        }
    }
    let expanded = if searching {
        &snapshot.lists.search_expanded_projects
    } else {
        &snapshot.lists.expanded_projects
    };
    let mut pages = BTreeMap::new();
    for (id, rows) in groups {
        let limit = expanded
            .get(&id)
            .copied()
            .unwrap_or(5)
            .max(snapshot.lists.list_query.limit * u32::from(snapshot.lists.recent_chats));
        let (data, has_more) = op::page_rows(rows, limit);
        pages.insert(
            id,
            Arc::new(ThreadList {
                data,
                limit,
                has_more,
                provider_errors: errors.clone(),
                ..Default::default()
            }),
        );
    }
    // Decorations are read separately. Retain a result only for the exact
    // directory/branch or project-root source captured by that read.
    let old = snapshot.lists.threads.as_ref();
    for project in &mut projects {
        project.icon = old
            .and_then(|page| {
                page.projects
                    .iter()
                    .find(|previous| previous.id == project.id && previous.roots == project.roots)
            })
            .and_then(|project| project.icon.clone());
    }
    let (mut data, has_more) = op::page_rows(standalone, snapshot.lists.list_query.limit);
    let previous: HashMap<_, _> = snapshot
        .listed_threads()
        .filter_map(|row| Some((row.id.as_ref()?, row)))
        .collect();
    let mut decorate = Vec::new();
    for row in data.iter_mut().chain(
        pages
            .values_mut()
            .flat_map(|page| &mut Arc::make_mut(page).data),
    ) {
        let old = row.id.as_ref().and_then(|id| previous.get(id)).copied();
        let matching = old.filter(|old| old.cwd == row.cwd && old.git_branch == row.git_branch);
        if let Some(old) = matching {
            row.has_changes = old.has_changes;
        }
        if refresh
            || matching.is_none()
            || matching.is_some_and(|old| {
                crate::models::task_active(None, old.status)
                    && !crate::models::task_active(None, row.status)
            })
        {
            decorate.push(row.clone());
        }
    }
    let new_projects = projects
        .iter()
        .filter(|project| {
            refresh
                || old.is_none_or(|old| {
                    !old.projects.iter().any(|previous| {
                        previous.id == project.id && previous.roots == project.roots
                    })
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    let decorations = op::list_decorations(
        agent_protocol::operations::ListDecorationScope::Root,
        &new_projects,
        &decorate,
    );
    let list = ThreadList {
        data,
        projects,
        has_more,
        has_more_projects,
        provider_errors: errors,
        limit: snapshot.lists.list_query.limit,
    };
    if snapshot.lists.threads.as_deref() != Some(&list) {
        snapshot.lists.threads = Some(Arc::new(list));
    }
    for (id, page) in &mut pages {
        if let Some(previous) = snapshot.lists.project_threads.get(id)
            && previous.as_ref() == page.as_ref()
        {
            *page = previous.clone();
        }
    }
    if snapshot.lists.project_threads.as_ref() != &pages {
        snapshot.lists.project_threads = Arc::new(pages);
    }

    decorations
        .filter(|_| snapshot.connection.connected)
        .into_iter()
        .collect()
}

#[cfg(test)]
pub(crate) fn seed(snapshot: &mut Snapshot, list: ThreadList) -> Vec<Effect> {
    let mut errors = list
        .provider_errors
        .clone()
        .unwrap_or_else(|| snapshot.lists.provider_errors.as_ref().clone());
    for thread in &list.data {
        if thread.list_stale == Some(true)
            && let Some(provider) = thread
                .active_provider
                .or_else(|| thread.id.as_ref().and_then(|id| id.provider))
        {
            errors.insert(
                provider.key().into(),
                serde_json::json!({"message":"会話一覧を確認できません。"}),
            );
        }
    }
    snapshot.lists.provider_errors = Arc::new(errors);
    let sequence = snapshot
        .lists
        .shell
        .as_ref()
        .map_or(1, |shell| shell.snapshot_sequence + 1);
    snapshot.lists.shell = Some(Arc::new(crate::test_conversations::shell(list, sequence)));
    snapshot.lists.generation = snapshot.lists.generation.max(1);
    snapshot
        .lists
        .subscription
        .get_or_insert_with(uuid::Uuid::new_v4);
    project(snapshot, false)
}

#[cfg(test)]
pub(crate) fn activity_fixture(shell: &mut shell::ThreadShell, facts: ActivityFacts) {
    use agent_protocol::{
        execution::{SessionStatus, TurnStatus},
        orchestration::RunStatus,
    };
    let active = match facts.status {
        SessionStatus::Preparing => Some(RunStatus::Preparing),
        SessionStatus::Running => Some(RunStatus::Running),
        _ => None,
    };
    shell.activity_run_status = active;
    shell.active_run_id = active.map(|_| "fixture-active".into());
    shell.status = active
        .map(Into::into)
        .unwrap_or_else(|| match facts.latest {
            Some(TurnStatus::Completed) => shell::ShellStatus::Completed,
            Some(TurnStatus::Failed) => shell::ShellStatus::Failed,
            Some(TurnStatus::Interrupted) => shell::ShellStatus::Interrupted,
            _ => shell::ShellStatus::Idle,
        });
    shell.latest_run_id = facts.completion.as_ref().map(ToString::to_string);
    shell.latest_run_completed_at = facts
        .completion
        .as_ref()
        .map(|_| "2026-11-14T00:00:00Z".into());
    shell.thread.last_visited_at = None;
    shell.pending_runtime_request =
        (facts.waiting || facts.approval).then(|| shell::PendingRequestSummary {
            id: "fixture-request".into(),
            kind: if facts.approval {
                "approval"
            } else {
                "user_input"
            }
            .into(),
            created_at: "2026-11-14T00:00:00Z".into(),
            blocking: Some(facts.waiting),
            user_only: None,
        });
    shell.pending_background_tasks = facts.background.into_iter().map(|task| serde_json::from_value(serde_json::json!({
        "taskId":task.id, "kind":match task.kind {
            agent_protocol::execution::BackgroundTaskKind::Command=>"command", agent_protocol::execution::BackgroundTaskKind::Subagent=>"subagent",
            agent_protocol::execution::BackgroundTaskKind::Monitor=>"monitor", agent_protocol::execution::BackgroundTaskKind::BackgroundTask=>"background_task"},
        "description":task.description
    })).unwrap()).collect();
}

#[cfg(test)]
pub(crate) fn set_activity(
    snapshot: &mut Snapshot,
    facts: Vec<(crate::session::SessionRef, ActivityFacts)>,
) -> ShellStreamItem {
    if snapshot.lists.shell.is_none() {
        let mut list = snapshot
            .lists
            .threads
            .as_deref()
            .cloned()
            .unwrap_or_default();
        list.data.extend(
            snapshot
                .lists
                .project_threads
                .values()
                .flat_map(|page| page.data.clone()),
        );
        seed(snapshot, list);
    }
    let mut shell = snapshot.lists.shell.as_ref().unwrap().as_ref().clone();
    shell.snapshot_sequence += 1;
    for (id, facts) in facts {
        let saved = shell
            .threads
            .iter_mut()
            .find(|thread| thread.thread.id == id.application_id())
            .unwrap();
        activity_fixture(Arc::make_mut(saved), facts);
    }
    let message = ShellStreamItem::Snapshot {
        snapshot: shell,
        resolved_repository_identity_roots: None,
    };
    let (next, _) = received(
        snapshot,
        snapshot.lists.subscription.unwrap(),
        message.clone(),
    );
    *snapshot = next;
    message
}

#[cfg(test)]
/// Covers work outside the loaded history window as well.
#[derive(Debug, Clone)]
pub(crate) struct ActivityFacts {
    pub status: agent_protocol::execution::SessionStatus,
    pub waiting: bool,
    pub approval: bool,
    pub latest: Option<agent_protocol::execution::TurnStatus>,
    pub completion: Option<agent_protocol::ids::TurnId>,
    pub background: Vec<agent_protocol::execution::BackgroundTask>,
}

#[cfg(test)]
impl Default for ActivityFacts {
    fn default() -> Self {
        Self {
            status: agent_protocol::execution::SessionStatus::Idle,
            waiting: false,
            approval: false,
            latest: None,
            completion: None,
            background: Vec::new(),
        }
    }
}
