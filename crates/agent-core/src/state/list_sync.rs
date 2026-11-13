//! One list replica supplies local pages and activity. Page limits never alter
//! the replicated scope or send another native catalog read.
use super::*;
use crate::{peer::PeerError, store::Execution};
use agent_protocol::{
    conversation::records,
    sync::{Content, Key, Scope},
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
    type Input = Arc<records::Replica>;
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .lists
            .replica
            .clone()
            .ok_or_else(|| PeerError::InvalidMessage("list scope is missing".into()))
    }
    type Output = (
        records::Replica,
        uuid::Uuid,
        serde_json::Map<String, serde_json::Value>,
    );
    async fn run(
        &self,
        base: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context
            .call(&records::SubscribeList {
                refresh: self.refresh,
                subscription: uuid::Uuid::nil(),
                cached: base.cursor(),
            })
            .await?;
        records::validate_list(&response.initial)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        if let records::Message::Snapshot { cursor, .. } = &response.initial
            && base.cursor().is_some_and(|current| {
                current.store == cursor.store && current.sequence > cursor.sequence
            })
        {
            return Err(PeerError::InvalidMessage(
                "list snapshot precedes its cached cursor".into(),
            ));
        }
        Ok((
            base.apply(base.subscription(), response.initial)
                .map_err(|error| PeerError::InvalidMessage(error.to_string()))?,
            response.subscription_id,
            response.provider_errors,
        ))
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        _: &str,
        (opened, subscription, provider_errors): Self::Output,
    ) -> Vec<Effect> {
        let Some(current) = &snapshot.lists.replica else {
            return Vec::new();
        };
        if current.subscription() != opened.subscription() {
            return Vec::new();
        }
        let replica = match (current.cursor(), opened.cursor()) {
            (Some(current_cursor), Some(opened_cursor))
                if current_cursor.store == opened_cursor.store
                    && current_cursor.sequence > opened_cursor.sequence =>
            {
                current.as_ref().clone()
            }
            _ => opened,
        };
        let mut effects = discovered_children(snapshot, &replica);
        snapshot.lists.replica = Some(Arc::new(replica));
        snapshot.lists.subscription = Some(subscription);
        snapshot.lists.provider_errors = Arc::new(provider_errors);
        effects.extend(project(snapshot, false));
        effects
    }
}

fn discovered_children(snapshot: &Snapshot, incoming: &records::Replica) -> Vec<Effect> {
    let roots = incoming
        .rows()
        .iter()
        .filter_map(|(key, row)| {
            if snapshot
                .lists
                .replica
                .as_ref()
                .is_some_and(|previous| previous.rows().contains_key(key))
            {
                return None;
            }
            let (Content::Full(record) | Content::Summary(record)) = &row.content;
            let records::Record::Thread(header) = record.as_ref() else {
                return None;
            };
            let parent = header.details.parent.as_ref()?;
            snapshot
                .panes
                .0
                .values()
                .any(|pane| pane.observed_agents.as_ref() == Some(parent))
                .then_some(parent)
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
    message: records::Message,
) -> (Snapshot, Vec<Effect>) {
    if previous.lists.subscription != Some(subscription) {
        return (previous.clone(), Vec::new());
    }
    let Some(replica) = &previous.lists.replica else {
        return (previous.clone(), Vec::new());
    };
    let apply = records::validate_list(&message).and_then(|()| {
        if let records::Message::Snapshot { cursor, .. } = &message
            && replica.cursor().is_some_and(|current| {
                current.store == cursor.store && current.sequence > cursor.sequence
            })
        {
            return Err(agent_protocol::sync::ApplyError::Version);
        }
        replica.apply(replica.subscription(), message)
    });
    let mut next = previous.clone();
    match apply {
        Ok(replica) => {
            if &replica == previous.lists.replica.as_ref().unwrap().as_ref() {
                return (next, Vec::new());
            }
            let mut effects = discovered_children(previous, &replica);
            next.lists.replica = Some(Arc::new(replica));
            effects.extend(project(&mut next, false));
            (next, effects)
        }
        Err(_) => {
            next.lists.subscription = None;
            (
                next,
                vec![Effect::execute(SubscribeCatalog { refresh: false })],
            )
        }
    }
}

pub(crate) fn project(snapshot: &mut Snapshot, refresh: bool) -> Vec<Effect> {
    let Some(replica) = &snapshot.lists.replica else {
        return Vec::new();
    };
    let mut projects = Vec::new();
    let mut summaries = Vec::new();
    let mut headers = Vec::new();
    let mut errors = snapshot.lists.provider_errors.as_ref().clone();
    for row in replica.rows().values() {
        let record = match &row.content {
            Content::Full(record) | Content::Summary(record) => record,
        };
        match (&row.key, record.as_ref()) {
            (Key::Project(_), records::Record::Project(project)) => {
                if let agent_protocol::sync::Position::Project { position } = row.position {
                    projects.push((position, project.clone()));
                }
            }
            (Key::Catalog(provider), records::Record::Catalog(records::Catalog::Failed(error))) => {
                errors.insert(provider.key().into(), serde_json::json!({"message":error}));
            }
            (Key::Thread(id), records::Record::Thread(header)) => {
                if header.details.activity.stale {
                    errors.insert(
                        header.state.provider.key().into(),
                        serde_json::json!({"message":"会話一覧を確認できません。"}),
                    );
                }
                let mut summary = records::summary(id, header);
                if let Some(error) = snapshot
                    .lists
                    .provider_errors
                    .get(header.state.provider.key())
                {
                    summary = records::unavailable(
                        summary,
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("AIを起動できませんでした。"),
                    );
                }
                summaries.push(summary);
                headers.push((id, header));
            }
            _ => {}
        }
    }
    projects.sort_by_key(|(position, _)| *position);
    let mut projects = projects.into_iter().map(|(_, project)| project).collect();
    let mut activity = records::list_activity(headers);
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
    use agent_protocol::sync::Cursor;
    let sequence = snapshot
        .lists
        .replica
        .as_ref()
        .and_then(|replica| replica.cursor())
        .map_or(1, |cursor| cursor.sequence + 1);
    let generation = snapshot
        .lists
        .replica
        .as_ref()
        .map_or(1, |replica| replica.subscription());
    let message = crate::test_conversations::catalog(
        list,
        Cursor {
            store: uuid::Uuid::from_u128(1),
            sequence,
        },
    );
    let replica = records::Replica::new(Scope::List, generation)
        .apply(generation, message)
        .unwrap();
    snapshot.lists.replica = Some(Arc::new(replica));
    snapshot
        .lists
        .subscription
        .get_or_insert_with(uuid::Uuid::new_v4);
    project(snapshot, false)
}

#[cfg(test)]
pub(crate) fn set_activity(
    snapshot: &mut Snapshot,
    facts: Vec<(crate::session::SessionRef, records::Activity)>,
) -> records::Message {
    if snapshot.lists.replica.is_none() {
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
    let replica = snapshot.lists.replica.as_ref().unwrap();
    let from = replica.cursor().unwrap();
    let sequence = from.sequence + 1;
    let mut rows = Vec::new();
    for (id, facts) in facts {
        let mut row = replica.rows()[&Key::Thread(id)].clone();
        let Content::Full(record) = &mut row.content else {
            unreachable!()
        };
        let records::Record::Thread(header) = Arc::make_mut(record) else {
            unreachable!()
        };
        header.details.activity = facts;
        row.version = sequence;
        rows.push(row);
    }
    let message = records::Message::Changes {
        from,
        through: agent_protocol::sync::Cursor { sequence, ..from },
        rows,
        removed: Vec::new(),
    };
    let (next, _) = received(
        snapshot,
        snapshot.lists.subscription.unwrap(),
        message.clone(),
    );
    *snapshot = next;
    message
}
