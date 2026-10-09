use super::*;
use crate::client::ClientExt;

pub use agent_protocol::operations::AddProject;

impl Operation for AddProject {
    rpc_operation!();
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, cwd: Self::Output) -> Vec<Effect> {
        let (next, mut effects) = reduce_intent(snapshot, Intent::NewChat { cwd });
        *snapshot = next;
        effects.push(Effect::execute(ListSessions::new(
            (*snapshot.list_query).clone(),
        )));
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ListSessions::new(
            (*snapshot.list_query).clone(),
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
    let project_ids: Vec<_> = projects
        .iter()
        .filter(|project| !project.roots.is_empty())
        .map(|project| project.id.clone())
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
    (!project_ids.is_empty() || !threads.is_empty()).then(|| {
        Effect::execute(ReadListDecorations {
            scope,
            project_ids,
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
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if let Some(list) = &mut snapshot.threads {
            for project in &mut Arc::make_mut(list).projects {
                if let Some(icon) = output.icons.get(&project.id) {
                    project.favicon_png = icon.clone();
                }
            }
        }
        for (id, status) in output.statuses {
            let source = self.threads.iter().find(|(source, _, _)| source == &id);
            for list in Arc::make_mut(&mut snapshot.project_threads)
                .values_mut()
                .chain(snapshot.threads.iter_mut())
            {
                for thread in &mut Arc::make_mut(list).data {
                    if thread.id.as_ref() == Some(&id)
                        && source.is_some_and(|(_, cwd, branch)| {
                            thread.cwd.as_ref() == Some(cwd) && &thread.git_branch == branch
                        })
                    {
                        thread.worktree_status = status;
                    }
                }
            }
        }
        Vec::new()
    }
}

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
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Agents {
            session: self.thread_id.clone(),
        })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestAgents(self.thread_id.clone())
    }
    fn apply(self, snapshot: &mut Snapshot, agents: Self::Output) -> Vec<Effect> {
        if snapshot.observed_agents.as_ref() != Some(&self.thread_id) {
            return Vec::new();
        }
        for agent in agents {
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.conversations)
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

fn merge_received(
    mut threads: crate::models::ThreadList,
    previous: Option<&crate::models::ThreadList>,
) -> crate::models::ThreadList {
    if let Some(previous) = previous {
        for project in &mut threads.projects {
            if let Some(old) = previous
                .projects
                .iter()
                .find(|old| old.id == project.id && old.roots == project.roots)
            {
                project.favicon_png = old.favicon_png.clone();
            }
        }
        for thread in &mut threads.data {
            if let Some(old) = previous.data.iter().find(|old| {
                old.id == thread.id && old.cwd == thread.cwd && old.git_branch == thread.git_branch
            }) {
                thread.worktree_status = old.worktree_status;
            }
        }
    }
    if let Some(errors) = threads.provider_errors.as_ref()
        && let Some(previous) = previous
    {
        for cached in &previous.data {
            let Some(id) = cached.id.as_ref() else {
                continue;
            };
            let provider = id.provider.key();
            if errors.contains_key(provider)
                && !threads
                    .data
                    .iter()
                    .any(|thread| thread.id.as_ref() == Some(id))
            {
                let mut cached = cached.clone();
                cached.list_stale = Some(true);
                cached.status = crate::models::SessionStatus::Unknown;
                threads.data.push(cached);
            }
        }
        for project in &previous.projects {
            if !threads
                .projects
                .iter()
                .any(|current| current.id == project.id)
                && threads
                    .data
                    .iter()
                    .any(|thread| thread.project_id.as_ref() == Some(&project.id))
            {
                threads.projects.push(project.clone());
            }
        }
    }
    threads
}

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
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if snapshot.threads.as_ref().is_none_or(|list| {
            !list
                .projects
                .iter()
                .any(|project| project.id == self.project_id)
        }) {
            return Vec::new();
        }
        let output = merge_received(
            output,
            snapshot
                .project_threads
                .get(&self.project_id)
                .map(AsRef::as_ref),
        );
        update_titles(snapshot, &output.data);
        if let Some(limit) =
            Arc::make_mut(&mut snapshot.expanded_projects).get_mut(&self.project_id)
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
        Arc::make_mut(&mut snapshot.project_threads).insert(self.project_id, Arc::new(output));
        effect.into_iter().collect()
    }
}

fn update_titles(snapshot: &mut Snapshot, summaries: &[crate::models::Thread]) {
    for summary in summaries {
        if let Some(id) = &summary.id
            && snapshot
                .conversations
                .get(id)
                .is_some_and(|thread| thread.name != summary.name)
            && let Some(thread) = shared_mut(&mut snapshot.conversations, id)
        {
            thread.name = summary.name.clone();
        }
    }
}

impl Operation for ListSessions {
    type Input = crate::models::ListQuery;
    type Output = crate::models::ThreadList;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::list(self.query.part))
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestList(self.query.part)
    }
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        let mut query = self.query.clone();
        if query.project_limits.is_none() && !snapshot.project_threads.is_empty() {
            query.project_limits = Some(
                snapshot
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
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        if snapshot.list_query.search_term != self.query.search_term {
            snapshot.expanded_projects = Arc::default();
            snapshot.project_threads = Arc::default();
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
            let paging = self.query.limit > snapshot.list_query.limit
                || self.query.project_limit > snapshot.list_query.project_limit;
            self.query.project_limits = paging.then(Default::default);
            if !paging {
                self.query.part = None;
            }
        }
        let mut retained = self.query.clone();
        retained.project_limits = None;
        retained.part = None;
        snapshot.list_query = Arc::new(retained);
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
        let current = &snapshot.list_query;
        if self.query.search_term != current.search_term
            || (self.query.part != Some(crate::models::ListPart::Projects)
                && self.query.limit != current.limit)
            || (self.query.part != Some(crate::models::ListPart::Chats)
                && self.query.project_limit != current.project_limit)
        {
            return Vec::new();
        }

        let mut threads = merge_received(threads, snapshot.threads.as_deref());
        let projects: Vec<_> = threads
            .projects
            .iter()
            .filter(|project| {
                self.query.part != Some(crate::models::ListPart::Projects)
                    || snapshot.threads.as_ref().is_none_or(|previous| {
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
        if let Some(previous) = snapshot.threads.as_ref() {
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
        update_titles(snapshot, &threads.data);
        let visible: std::collections::HashSet<_> =
            threads.projects.iter().map(|p| p.id.as_str()).collect();
        Arc::make_mut(&mut snapshot.expanded_projects)
            .retain(|id, _| visible.contains(id.as_str()));
        if !snapshot.list_query.search_term.trim().is_empty() {
            for id in &visible {
                Arc::make_mut(&mut snapshot.expanded_projects)
                    .entry((*id).to_owned())
                    .or_insert(5);
            }
        }
        let pages = Arc::make_mut(&mut snapshot.project_threads);
        pages.retain(|id, _| visible.contains(id.as_str()));
        for (id, page) in &threads.project_pages {
            if pages
                .get(id)
                .is_some_and(|current| current.limit > page.limit)
            {
                continue;
            }
            pages.insert(
                id.clone(),
                Arc::new(merge_received(
                    crate::models::ThreadList {
                        data: threads
                            .data
                            .iter()
                            .filter(|thread| thread.project_id.as_ref() == Some(id))
                            .cloned()
                            .collect(),
                        projects: Vec::new(),
                        has_more: page.has_more,
                        has_more_projects: false,
                        limit: page.limit,
                        project_pages: Default::default(),
                        provider_errors: threads.provider_errors.clone(),
                    },
                    pages.get(id).map(AsRef::as_ref),
                )),
            );
        }
        threads.data.retain(|thread| {
            thread
                .project_id
                .as_ref()
                .is_none_or(|id| !visible.contains(id.as_str()))
        });
        threads.project_pages.clear();
        snapshot.threads = Some(Arc::new(threads));
        decoration
            .into_iter()
            .chain(
                snapshot
                    .expanded_projects
                    .iter()
                    .filter(|(id, _)| {
                        snapshot.connected && !snapshot.project_threads.contains_key(*id)
                    })
                    .map(|(id, limit)| {
                        Effect::execute(ListProjectSessions {
                            project_id: id.clone(),
                            limit: *limit,
                            search_term: snapshot.list_query.search_term.clone(),
                        })
                    }),
            )
            .collect()
    }
}

pub use agent_protocol::operations::ReadItem;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadTurnItems {
    pub thread_id: crate::session::SessionRef,
    pub turn_id: agent_protocol::ids::TurnId,
}

impl Operation for LoadTurnItems {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::TurnItems {
            session: self.thread_id.clone(),
            turn: self.turn_id.clone(),
        })
    }
    no_input!();
    type Output = crate::models::Empty;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .client
            .call(&agent_protocol::session::ReadTurnItems {
                session: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
            })
            .await
    }
}

pub use agent_protocol::session::ReadHistory;

impl Operation for ReadHistory {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.session.clone(),
        })
    }
    type Input = Option<uuid::Uuid>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(snapshot.subscriptions.get(&self.session).copied())
    }
    type Output = (agent_protocol::session::HistoryPage, Option<uuid::Uuid>);
    async fn run(
        &self,
        subscription: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        Ok((context.call(self).await?, subscription))
    }
    fn apply(self, snapshot: &mut Snapshot, (page, subscription): Self::Output) -> Vec<Effect> {
        if subscription != snapshot.subscriptions.get(&self.session).copied()
            || snapshot
                .conversations
                .get(&self.session)
                .is_none_or(|thread| thread.history_cursor.as_deref() != Some(&self.cursor))
        {
            return Vec::new();
        }
        let details = item_details(&self.session, &page.turns);
        page.prepend_to(shared_mut(&mut snapshot.conversations, &self.session).unwrap());
        snapshot.error = None;
        reconcile_pending(snapshot, &self.session);
        details
    }
}

pub(crate) fn item_details(
    session: &crate::session::SessionRef,
    turns: &[Arc<crate::models::Turn>],
) -> Vec<Effect> {
    turns
        .iter()
        .flat_map(|turn| {
            turn.items
                .iter()
                .flatten()
                .filter(|item| {
                    item.is_deferred()
                        && matches!(
                            item.body(),
                            crate::models::ItemBody::UserMessage { .. }
                                | crate::models::ItemBody::AssistantText { .. }
                                | crate::models::ItemBody::ImageGeneration { .. }
                        )
                })
                .map(|item| {
                    Effect::execute(ReadItem {
                        thread_id: session.clone(),
                        turn_id: turn.id.clone(),
                        item_id: item.id.clone(),
                    })
                })
        })
        .collect()
}

/// The item Arc is its local source generation. Session updates replace this
/// Arc even when the new value happens to equal the old one.
#[derive(Debug)]
pub struct ItemRead {
    source: Option<Arc<Item>>,
    subscription: Option<uuid::Uuid>,
    response: Result<rpc::ItemResponse, PeerError>,
}

fn item_source<'a>(request: &ReadItem, snapshot: &'a Snapshot) -> Option<&'a Arc<Item>> {
    snapshot
        .conversations
        .get(&request.thread_id)?
        .turns
        .as_ref()?
        .iter()
        .rfind(|turn| turn.id == request.turn_id)?
        .items
        .as_ref()?
        .iter()
        .find(|item| item.id == request.item_id)
}
impl Operation for ReadItem {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Item { item: self.clone() })
    }
    type Input = (Option<Arc<Item>>, Option<uuid::Uuid>);
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok((
            item_source(self, snapshot).cloned(),
            snapshot.subscriptions.get(&self.thread_id).copied(),
        ))
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Item(self.clone())
    }
    type Output = ItemRead;
    async fn run(
        &self,
        (source, subscription): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context.call(self).await?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            crate::transfers::resolve_item(response, context.session),
        )
        .await
        .unwrap_or_else(|_| {
            Err(PeerError::RequestTimeout {
                method: "item body transfer".into(),
            })
        });
        Ok(ItemRead {
            source,
            subscription,
            response,
        })
    }

    fn complete(
        self,
        snapshot: &mut Snapshot,
        output: Self::Output,
        current_epoch: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        let current = item_source(&self, snapshot);
        if !current_epoch
            || output.subscription != snapshot.subscriptions.get(&self.thread_id).copied()
            || !current
                .zip(output.source.as_ref())
                .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        {
            // Full Item updates already supply the new body. A delta preserves
            // the deferred marker, so retry once this transfer has finished.
            return match current {
                Some(item) if item.is_deferred() => Ok(vec![Effect::continuation(self)]),
                Some(_) => Ok(Vec::new()),
                None => Err(PeerError::InvalidMessage(
                    "item is no longer available".into(),
                )),
            };
        }
        let response = output.response?;
        *snapshot = upsert_item(snapshot, &self.thread_id, &self.turn_id, response.item);
        reconcile_pending(snapshot, &self.thread_id);
        Ok(Vec::new())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadThread {
    pub thread_id: crate::session::SessionRef,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub open: bool,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = 5))]
    pub limit: u32,
}
impl ReadThread {
    pub fn new(thread_id: crate::session::SessionRef) -> Self {
        Self {
            thread_id,
            open: false,
            limit: 5,
        }
    }
    pub fn open(thread_id: crate::session::SessionRef) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
impl rpc::RpcMethod for ReadThread {
    agent_protocol::operations::rpc_contract!(OpenSession);
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(crate::session::OpenSession {
            session: self.thread_id.clone(),
            limit: self.limit as usize,
            include_activity: false,
        })
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.session != self.thread_id {
            return Err("session identity does not match");
        }
        rpc::validate_thread(&output.response, Some(&self.thread_id))
    }
}

impl Operation for ReadThread {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.thread_id.clone(),
        })
    }
    rpc_operation!();
    fn invalidates(&self) -> bool {
        self.open
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if self.open {
            let cwd = snapshot
                .thread_metadata(&self.thread_id)
                .and_then(|thread| thread.cwd.as_ref())
                .cloned()
                .unwrap_or_default();
            select_thread(snapshot, self.thread_id.clone(), cwd);
        }
        snapshot.error = None;
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, mut output: Self::Output) -> Vec<Effect> {
        if output
            .response
            .thread
            .history_read_state
            .as_ref()
            .is_some_and(|state| state.kind == crate::session::HistoryReadKind::Unavailable)
            && let Some(cached) = snapshot.conversations.get(&self.thread_id)
        {
            let live = output.response.thread.turns.get_or_insert_default();
            let mut turns: Vec<_> = cached
                .turns
                .iter()
                .flatten()
                .filter(|turn| turn.status != agent_protocol::execution::TurnStatus::Running)
                .cloned()
                .collect();
            for current in live.drain(..) {
                if let Some(index) = turns.iter().rposition(|turn| turn.id == current.id) {
                    turns[index] = current;
                } else {
                    turns.push(current);
                }
            }
            *live = turns;
        }
        let mut effects = if self.open {
            open_thread(snapshot, output.response.thread, output.response.model)
        } else {
            refresh_thread(snapshot, output.response.thread);
            Vec::new()
        };
        effects.extend(item_details(
            &self.thread_id,
            snapshot
                .conversations
                .get(&self.thread_id)
                .and_then(|thread| thread.turns.as_deref())
                .unwrap_or_default(),
        ));
        Arc::make_mut(&mut snapshot.subscriptions).insert(self.thread_id, output.subscription_id);
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        // A late navigation result can warm the cache, but must never replace
        // an already subscribed, more recent view or change the selected draft.
        if !snapshot.subscriptions.contains_key(&self.thread_id) {
            refresh_thread(snapshot, output.response.thread);
        }
        Vec::new()
    }
}

fn select_thread(snapshot: &mut Snapshot, id: crate::session::SessionRef, cwd: String) {
    let key: DraftKey = id.clone().into();
    let previous = snapshot.drafts.get(&key).cloned().unwrap_or_default();
    let mut draft = previous.clone();
    if !snapshot.models.is_empty() {
        let settings = supported_settings(
            previous.model.as_ref(),
            previous.effort.as_deref(),
            previous.service_tier.as_deref(),
            Some(id.provider),
            &snapshot.models,
            !snapshot.model_errors.is_empty(),
        );
        if settings
            != (
                previous.model.as_ref(),
                previous.effort.as_deref(),
                previous.service_tier.as_deref(),
            )
        {
            let draft = Arc::make_mut(&mut draft);
            draft.model = settings.0.cloned();
            draft.effort = settings.1.map(str::to_owned);
            draft.service_tier = settings.2.map(str::to_owned);
        }
    }
    if !snapshot.drafts.contains_key(&key) || !Arc::ptr_eq(&draft, &previous) {
        Arc::make_mut(&mut snapshot.drafts).insert(key, draft);
    }
    if snapshot.navigation.cwd != cwd {
        clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
    }
    let navigation = Arc::make_mut(&mut snapshot.navigation);
    navigation.thread_id = Some(id.clone());
    navigation.draft_key = id.into();
    navigation.cwd = cwd;
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    thread: Thread,
    model: Option<crate::models::ModelRef>,
) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    refresh_thread(snapshot, thread);
    if let Some(id) = id {
        navigate(
            snapshot,
            Navigation {
                thread_id: Some(id.clone()),
                draft_key: id.clone().into(),
                cwd,
            },
        );
        if let Some(model) = model {
            let (updated, _) = reduce(
                snapshot,
                Event::Intent(Intent::SelectModel {
                    thread_id: id.into(),
                    model,
                }),
            );
            *snapshot = updated;
        }
    }
    review_workspace(snapshot).into_iter().collect()
}

pub(super) fn refresh_thread(snapshot: &mut Snapshot, incoming: Thread) {
    let Some(id) = incoming.id.clone().filter(|id| id.validate().is_ok()) else {
        snapshot.error = Some("thread ID is missing".into());
        return;
    };
    let mut thread = incoming;
    if let Some(cached) = snapshot.conversations.get(&id)
        && let Some(turns) = crate::session::retained_history(
            cached.turns.as_deref().unwrap_or_default(),
            thread.turns.as_deref().unwrap_or_default(),
        )
    {
        let count = thread.turns.as_ref().map_or(0, Vec::len);
        if thread.history_cursor.is_some() && turns.len() > count {
            thread.turns = Some(turns);
            thread.history_cursor = cached.history_cursor.clone();
            thread.history_has_more = cached.history_has_more;
            thread.history_read_state = cached.history_read_state.clone();
        } else {
            thread.turns = Some(turns[turns.len() - count..].to_vec());
        }
    }
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
    reconcile_pending(snapshot, &id);
}

pub use agent_protocol::operations::ForkSession;

impl Operation for ForkSession {
    rpc_operation!();
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = output.thread.id.clone().expect("validated thread ID");
        let mut effects = open_thread(snapshot, output.thread, output.model);
        effects.push(Effect::execute(ReadThread::new(id)));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListSessions::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread);
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
}
pub use agent_protocol::operations::CreateSession;

impl Operation for CreateSession {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.subscriptions).insert(output.session, output.subscription_id);
        open_thread(snapshot, output.response.thread, output.response.model)
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.subscriptions).insert(output.session, output.subscription_id);
        refresh_thread(snapshot, output.response.thread);
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.session.clone(),
        }
    }
}
pub use agent_protocol::operations::Interrupt;

impl Operation for Interrupt {
    fn scheduling(&self) -> Scheduling {
        Scheduling::Control
    }

    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Interrupt {
            session: self.thread_id.clone(),
            turn: self.turn_id.clone(),
        })
    }
    rpc_operation!();
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {}
impl Operation for LoadModels {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Models)
    }
    no_input!();
    type Output = agent_protocol::operations::ModelPage;

    const STALE_POLICY: StalePolicy = StalePolicy::Retry;

    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context.client.models().await
    }
    fn apply(self, snapshot: &mut Snapshot, catalog: Self::Output) -> Vec<Effect> {
        let models = catalog.data;
        let errors = catalog
            .provider_errors
            .as_ref()
            .cloned()
            .unwrap_or_default();
        snapshot.drafts = normalized_model_drafts(
            &snapshot.drafts,
            &models,
            &errors,
            snapshot
                .account
                .accounts
                .as_ref()
                .map(|accounts| &accounts.selected),
            &snapshot.navigation.draft_key,
            &snapshot
                .model_defaults_for_cwd(&snapshot.navigation.cwd)
                .providers,
        );
        snapshot.models = Arc::new(models);
        snapshot.model_errors = Arc::new(errors);
        Vec::new()
    }
}

pub use agent_protocol::operations::OpenRequest;

impl Operation for OpenRequest {
    no_input!();
    type Output = crate::session::OpenedSession;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let id = context.client.call(self).await?;
        let open = ReadThread::new(id);
        context.call(&open).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.clone()).apply(snapshot, output)
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.clone()).stale(snapshot, output)
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
            connected: true,
            ..Default::default()
        };
        assert!(
            ListSessions::new(Default::default())
                .apply(&mut state, root)
                .is_empty()
        );
        assert_eq!(state.project_threads.len(), 5);
        assert!(state.threads.as_ref().unwrap().data.is_empty());
        let (mut state, effects) = reduce_intent(
            &state,
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: true,
            },
        );
        assert!(effects.is_empty(), "initial pages are already received");
        let received = state.project_threads["p1"].clone();
        ListProjectSessions {
            project_id: "p0".into(),
            limit: 25,
            search_term: String::new(),
        }
        .apply(
            &mut state,
            ThreadList {
                data: vec![row("p0")],
                projects: vec![],
                limit: 25,
                has_more: false,
                has_more_projects: false,
                project_pages: Default::default(),
                provider_errors: None,
            },
        );
        let mut query = (*state.list_query).clone();
        query.project_limit = 15;
        query.part = Some(ListPart::Projects);
        let mut more = ListSessions::new(query);
        more.prepare(&mut state).unwrap();
        assert_eq!(
            more.capture(&state).unwrap().project_limits,
            Some(Default::default())
        );
        more.apply(
            &mut state,
            ThreadList {
                data: vec![],
                projects: (0..15).map(|i| project(&format!("p{i}"))).collect(),
                limit: 5,
                has_more: false,
                has_more_projects: true,
                project_pages: Default::default(),
                provider_errors: None,
            },
        );
        assert!(Arc::ptr_eq(&received, &state.project_threads["p1"]));
        assert!(!state.project_threads.contains_key("p14"));
        let mut refresh = ListSessions::new((*state.list_query).clone());
        refresh.prepare(&mut state).unwrap();
        assert_eq!(
            refresh.capture(&state).unwrap().project_limits,
            Some([("p0".into(), 25)].into())
        );
        let (state, _) = reduce_intent(
            &state,
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: false,
            },
        );
        let (state, effects) = reduce_intent(
            &state,
            Intent::SetProjectExpanded {
                project_id: "p0".into(),
                expanded: true,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(
            state.expanded_projects["p0"], 25,
            "short pages preserve their requested count"
        );
        let (_, effects) = reduce_intent(
            &state,
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
                list_query: Arc::new(crate::models::ListQuery {
                    search_term: "Archive".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
            let store = crate::store::Store::new(peer, initial);
            let page =
                json!({"data":[],"projects":[{"id":"old","name":"Old","roots":[]}],"hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5});
            for _ in 0..4 {
                let request = read(&mut reader).await;
                let result = match request["method"].as_str().unwrap() {
                    "host/session/list" => page.clone(),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    "host/taskActivity/read" => json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
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
                snapshot.threads.is_none()
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
                !store
                    .snapshot()
                    .operations
                    .contains_key(&OperationKey::SessionList)
            );
            let recent = store.snapshot().threads.clone().unwrap();
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            writer
                .reply(&request, json!({"result":page}))
                .await
                .unwrap();
            while {
                let snapshot = updates.borrow_and_update();
                !snapshot.project_threads.contains_key("old")
            }
            {
                updates.changed().await.unwrap();
            }
            assert!(Arc::ptr_eq(
                &recent,
                store.snapshot().threads.as_ref().unwrap()
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
            assert!(store.snapshot().error.is_none());
            assert!(
                store.snapshot().thread_list().unwrap().projects[0]
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("fixture unavailable")
            );
            store
                .dispatch(Intent::SetProjectExpanded {
                    project_id: "old".into(),
                    expanded: false,
                })
                .await
                .unwrap();
            let received = store.snapshot().project_threads["old"].clone();
            assert!(
                store.snapshot().thread_list().unwrap().projects[0]
                    .error
                    .is_none()
            );
            store.dispatch(Intent::SetProjectExpanded {
                project_id: "old".into(), expanded: true,
            }).await.unwrap();
            assert!(Arc::ptr_eq(&received, &store.snapshot().project_threads["old"]));
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
            navigation: Arc::new(Navigation {
                thread_id: Some(parent.clone()),
                ..Default::default()
            }),
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
        };
        let (peer, mut reader, writer) = host_fixture::connect(&snapshot).await;
        let store = crate::store::Store::new(peer, snapshot);
        let empty_list = json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5});
        for _ in 0..5 {
            let request = read(&mut reader).await;
            let result = match request["method"].as_str().unwrap() {
                "host/session/list" => empty_list.clone(),
                "host/account/list" => json!({"accounts":[],"selected":{}}),
                "host/taskActivity/read" => {
                    json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
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
                snapshot.threads.is_some()
                    && snapshot.account.accounts.is_some()
                    && snapshot.subscriptions.contains_key(&parent)
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
            store.snapshot().agent_panel().is_empty(),
            "a superseded fleet response must not publish stale rows"
        );
        let mut latest = read(&mut reader).await;
        if latest["method"] == "host/taskActivity/read" {
            writer.reply(&latest, json!({"result":{"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
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
            store.snapshot().agent_panel().is_empty(),
            "closing the panel discards an in-flight fleet response"
        );
        let watching = store.dispatch(Intent::WatchAgents {
            thread_id: Some(parent.clone()),
        });
        let latest = read(&mut reader).await;
        writer.reply(&latest, json!({"result":[{"id":{"provider":"codex","id":"child"},"parentId":parent,"name":"Review","status":"running"}]})).await.unwrap();
        watching.await.unwrap();
        let after = store.snapshot();
        assert_eq!(after.agent_panel()[0].title, "Review");
        assert!(after.agent_panel()[0].active);
        assert_eq!(after.navigation, before.navigation);
        assert_eq!(after.drafts, before.drafts);
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
            connected: true,
            ..Default::default()
        };
        ListSessions::new(Default::default()).apply(&mut snapshot, root("chat"));
        let selected = SessionRef::new(ProviderKind::Codex, "selected".into()).unwrap();
        Arc::make_mut(&mut snapshot.navigation).thread_id = Some(selected.clone());
        Arc::make_mut(&mut snapshot.drafts).insert(
            selected.clone().into(),
            Arc::new(Draft {
                text: "Keep this draft".into(),
                ..Default::default()
            }),
        );
        let (opened, effects) = reduce_intent(
            &snapshot,
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            },
        );
        assert_eq!(effects.len(), 1);
        let mut published = opened;
        let root_before = published.threads.clone().unwrap();
        let refresh_effects =
            ListSessions::new(Default::default()).apply(&mut published, root("new-chat"));
        assert_eq!(refresh_effects.len(), 1);
        assert!(!Arc::ptr_eq(
            &root_before,
            published.threads.as_ref().unwrap()
        ));
        if count > 5 {
            (published, _) = reduce_intent(
                &published,
                Intent::ExpandThreadList {
                    project_id: Some("old".into()),
                },
            );
        }
        let limit = published.expanded_projects["old"];
        ListProjectSessions {
            project_id: "old".into(),
            limit,
            search_term: String::new(),
        }
        .apply(&mut published, project("old-task"));
        assert_eq!(published.project_threads["old"].data.len(), count);
        let received = published.project_threads["old"].clone();
        let navigation = published.navigation.clone();
        let drafts = published.drafts.clone();
        let (mut closed, effects) = reduce_intent(
            &published,
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
        .apply(&mut closed, project("late-task"));
        assert!(!Arc::ptr_eq(&received, &closed.project_threads["old"]));
        assert!(
            closed.project_threads["old"]
                .data
                .iter()
                .all(|row| row.name.as_deref() == Some("late-task"))
        );
        assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
        let (reopened, effects) = reduce_intent(
            &closed,
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
        assert_eq!(reopened.expanded_projects["old"], count.max(5) as u32);
        assert!(!reopened.thread_list().unwrap().projects[0].loading);
        assert_eq!(closed.navigation, navigation);
        assert_eq!(closed.drafts, drafts);
        ListSessions::new(crate::models::ListQuery {
            search_term: "other".into(),
            ..Default::default()
        })
        .prepare(&mut closed)
        .unwrap();
        assert!(closed.project_threads.is_empty());
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
            connected: true,
            ..Default::default()
        };
        let mut search = ListSessions::new(crate::models::ListQuery {
            search_term: "needle".into(),
            ..Default::default()
        });
        search.prepare(&mut snapshot).unwrap();
        let effects = search.apply(&mut snapshot, page());
        assert_eq!(snapshot.expanded_projects["matching"], 5);
        assert_eq!(effects.len(), 1);
        assert!(snapshot.thread_list().unwrap().projects[0].expanded);

        let mut clear = ListSessions::new(Default::default());
        clear.prepare(&mut snapshot).unwrap();
        clear.apply(&mut snapshot, page());
        assert!(snapshot.expanded_projects.is_empty());
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
            connected: true,
            ..Default::default()
        };
        ListSessions::new(Default::default()).apply(&mut snapshot, root_page("initial"));
        let (opened, effects) = reduce_intent(
            &snapshot,
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
        .apply(&mut opened, project_page());
        let loaded = opened.project_threads["project"].clone();
        let (mut paginating, effects) =
            reduce_intent(&opened, Intent::ExpandThreadList { project_id: None });
        assert_eq!(effects.len(), 1);
        let root_query = (*paginating.list_query).clone();
        let root_effects = ListSessions::new(root_query).apply(&mut paginating, root_page("next"));
        assert!(root_effects.is_empty());
        assert!(Arc::ptr_eq(&loaded, &paginating.project_threads["project"]));
        assert_eq!(
            paginating.project_threads["project"].data[0]
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
        Arc::make_mut(&mut snapshot.project_threads).insert("project".into(), loaded.clone());
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
        ListSessions::new(Default::default()).apply(&mut snapshot, page.clone());
        assert!(
            Arc::ptr_eq(&loaded, &snapshot.project_threads["project"]),
            "A delayed five-row root response cannot replace the fifteen-row project page"
        );
        assert!(
            snapshot.operations.contains_key(&key),
            "Only the project request can finish its own pending pagination"
        );
        let mut refresh = page;
        refresh.project_pages.get_mut("project").unwrap().limit = 15;
        ListSessions::new(Default::default()).apply(&mut snapshot, refresh);
        assert_eq!(
            snapshot.project_threads["project"].data[0]
                .id
                .as_ref()
                .unwrap()
                .id,
            "old",
            "Refreshing the same requested count must replace stale rows"
        );
        assert!(snapshot.operations.contains_key(&key));
    }

    #[test]
    fn opening_an_old_project_task_uses_metadata_before_history_and_clears_cached_status_on_disconnect()
     {
        let id = SessionRef::new(ProviderKind::Codex, "old".into()).unwrap();
        let page: crate::models::ThreadList = serde_json::from_value(serde_json::json!({
            "data":[{"id":id,"projectId":"p","cwd":"/old-project","status":"running","canAcceptDirectInput":false}],
            "projects":[{"id":"p","name":"P","roots":[]}],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,
        })).unwrap();
        let mut snapshot = Snapshot {
            connected: true,
            expanded_projects: Arc::new([("p".into(), 5)].into()),
            project_threads: Arc::new([("p".into(), Arc::new(page))].into()),
            ..Default::default()
        };
        ReadThread::open(id.clone()).prepare(&mut snapshot).unwrap();
        assert_eq!(snapshot.navigation.thread_id.as_ref(), Some(&id));
        assert_eq!(snapshot.navigation.cwd, "/old-project");
        assert_eq!(snapshot.selected_directory(), "/old-project");
        assert_eq!(
            snapshot
                .thread_metadata(&id)
                .unwrap()
                .can_accept_direct_input,
            Some(false)
        );
        let (disconnected, _) = reduce(&snapshot, Event::Disconnected("offline".into()));
        assert_eq!(
            disconnected.thread_metadata(&id).unwrap().status,
            crate::models::SessionStatus::Unknown
        );
        assert_eq!(
            snapshot.thread_metadata(&id).unwrap().status,
            crate::models::SessionStatus::Running
        );
    }

    #[test]
    fn agents_are_loaded_only_while_the_selected_fleet_is_observed() {
        let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let mut snapshot = Snapshot {
            connected: true,
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.navigation).thread_id = Some(parent.clone());
        Arc::make_mut(&mut snapshot.drafts).insert(
            parent.clone().into(),
            Arc::new(Draft {
                text: "Keep my draft".into(),
                ..Default::default()
            }),
        );
        let (_, effects) = reduce(&snapshot, Event::Connected);
        assert_eq!(
            effects.len(),
            5,
            "reconnecting loads the task aggregate without fetching an unobserved fleet"
        );
        let (ignored, effects) = reduce_intent(
            &snapshot,
            Intent::WatchAgents {
                thread_id: Some(other),
            },
        );
        assert!(effects.is_empty());
        assert!(ignored.observed_agents.is_none());
        let (watching, effects) = reduce_intent(
            &snapshot,
            Intent::WatchAgents {
                thread_id: Some(parent.clone()),
            },
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(watching.observed_agents, Some(parent.clone()));
        assert_eq!(watching.navigation, snapshot.navigation);
        assert_eq!(watching.drafts, snapshot.drafts);
        let (_, effects) = reduce_intent(
            &watching,
            Intent::WatchAgents {
                thread_id: Some(parent.clone()),
            },
        );
        assert!(
            effects.is_empty(),
            "rendering the same panel must not refetch"
        );
        let (_, effects) = reduce(&watching, Event::Connected);
        assert_eq!(
            effects.len(),
            6,
            "reconnect refreshes only the observed fleet"
        );
        let (closed, effects) = reduce_intent(&watching, Intent::WatchAgents { thread_id: None });
        assert!(effects.is_empty());
        assert!(closed.observed_agents.is_none());
        let mut offline = closed.clone();
        offline.connected = false;
        let (offline, effects) = reduce_intent(
            &offline,
            Intent::WatchAgents {
                thread_id: Some(parent),
            },
        );
        assert!(effects.is_empty());
        assert!(
            offline.observed_agents.is_some(),
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
            observed_agents: Some(parent.clone()),
            conversations: Arc::new([(child.clone(), history.clone())].into()),
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
        .apply(&mut snapshot, observations.clone());
        let thread = &snapshot.conversations[&child];
        assert_eq!(thread.parent_id, Some(parent.clone()));
        assert_eq!(thread.name.as_deref(), Some("Review"));
        assert_eq!(thread.status, crate::models::SessionStatus::Running);
        assert_eq!(thread.turns, history.turns);
        snapshot.observed_agents = None;
        let conversations = snapshot.conversations.clone();
        ListAgents { thread_id: parent }.apply(&mut snapshot, observations);
        assert!(Arc::ptr_eq(&snapshot.conversations, &conversations));
    }

    #[rstest::rstest]
    #[case::new_draft(false, false)]
    #[case::existing_empty_draft(true, false)]
    #[case::saved_settings(true, true)]
    fn opening_restores_model_metadata_or_uses_the_provider_catalog(
        #[values(true, false)] catalog_first: bool,
        #[values(true, false)] model_metadata: bool,
        #[values(ProviderKind::Codex, ProviderKind::Claude)] provider: ProviderKind,
        #[case] existing_draft: bool,
        #[case] saved_choice: bool,
    ) {
        let models: Vec<Model> = serde_json::from_value(serde_json::json!([
            {"id":"codex-default","model":{"provider":"codex","id":"default"},"displayName":"Codex default",
             "isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"}]},
            {"id":"claude-default","model":{"provider":"claude","id":"default"},"displayName":"Claude default",
             "isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]},
            {"id":"codex-saved","model":{"provider":"codex","id":"saved"},"displayName":"Codex saved",
             "defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}]},
            {"id":"claude-saved","model":{"provider":"claude","id":"saved"},"displayName":"Claude saved",
             "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}
        ])).unwrap();
        let session = SessionRef::new(provider, "external".into()).unwrap();
        let key: DraftKey = session.clone().into();
        let mut snapshot = Snapshot::default();
        let catalog = || agent_protocol::operations::ModelPage {
            data: models.clone(),
            next_cursor: None,
            provider_errors: None,
        };
        if catalog_first {
            LoadModels {}.apply(&mut snapshot, catalog());
        }
        if existing_draft {
            Arc::make_mut(&mut snapshot.drafts).insert(
                key.clone(),
                Arc::new(Draft {
                    text: "Unsent input".into(),
                    model: saved_choice.then(|| crate::models::ModelRef {
                        provider,
                        id: "saved".into(),
                    }),
                    effort: saved_choice.then(|| "low".into()),
                    ..Default::default()
                }),
            );
        }
        ReadThread::open(session.clone())
            .prepare(&mut snapshot)
            .unwrap();
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(session),
                ..Default::default()
            },
            model_metadata.then(|| crate::models::ModelRef {
                provider,
                id: "saved".into(),
            }),
        );
        if !catalog_first {
            LoadModels {}.apply(&mut snapshot, catalog());
        }
        let draft = &snapshot.drafts[&key];
        assert_eq!(draft.text, if existing_draft { "Unsent input" } else { "" });
        assert_eq!(
            draft.model,
            Some(crate::models::ModelRef {
                provider,
                id: if saved_choice || model_metadata {
                    "saved"
                } else {
                    "default"
                }
                .into(),
            })
        );
        let effort = if saved_choice {
            "low"
        } else if provider == ProviderKind::Codex {
            "medium"
        } else {
            "high"
        };
        // The composer and the next submission must use the same settings.
        assert_eq!(draft.effort.as_deref(), Some(effort));
        assert_eq!(draft.service_tier.as_deref(), Some("default"));
        assert_eq!(snapshot.model_quick_controls(key.clone()).effort, effort);
        let expected_model = draft.model.clone();
        let (snapshot, _) = reduce_intent(
            &snapshot,
            Intent::SetDraftText {
                thread_id: key,
                text: "Next input".into(),
            },
        );
        let (submitted, effects) = reduce_intent(
            &snapshot,
            Intent::Submit {
                thread_id: None,
                client_user_message_id: "next-input".into(),
            },
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(
            submitted.pending_submissions["next-input"].draft.model,
            expected_model
        );
    }
}

#[cfg(test)]
mod item_read_tests {
    fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
        match item.body() {
            agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
            agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
            agent_protocol::items::ItemBody::Reasoning { content, .. } => {
                content.first().map(String::as_str)
            }
            _ => None,
        }
    }

    use super::*;
    use crate::session::{SessionChange, TextField};

    fn fixture() -> (Snapshot, ReadItem) {
        let thread = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"chat"},"turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":{"deferred":{"summary":{"assistantText":{"text":"summary","phase":"unknown"}}}}}]}]})).unwrap();
        let snapshot = Snapshot {
            conversations: Arc::new(BTreeMap::from([(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chat".into(),
                },
                Arc::new(thread),
            )])),
            subscriptions: Arc::new(BTreeMap::from([(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chat".into(),
                },
                uuid::Uuid::new_v4(),
            )])),
            ..Default::default()
        };
        (
            snapshot,
            ReadItem {
                thread_id: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chat".into(),
                },
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
        )
    }
    fn read(snapshot: &Snapshot, request: &ReadItem) -> ItemRead {
        ItemRead { source: item_source(request, snapshot).cloned(), subscription: snapshot.subscriptions.get(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "chat".into() }).copied(), response: Ok(rpc::ItemResponse {item: serde_json::from_value(serde_json::json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old full body","phase":"unknown"}}}}})).unwrap(), transfer:None,}) }
    }
    fn update(snapshot: &mut Snapshot, change: SessionChange) {
        let session = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "chat".into(),
        };
        let subscription_id = snapshot.subscriptions[&session];
        let (next, effects) = crate::state::reduce(
            snapshot,
            crate::state::Event::SessionUpdate(Box::new(crate::session::SessionUpdate {
                subscription_id,
                change,
            })),
        );
        if next.error.is_some() {
            assert_eq!(effects.len(), 1);
            assert!(!next.subscriptions.contains_key(&session));
        }
        *snapshot = next;
    }
    #[test]
    fn full_item_update_wins_over_late_body_and_its_status() {
        let (mut snapshot, request) = fixture();
        let old = read(&snapshot, &request);
        update(&mut snapshot, SessionChange::Item {turn_id:"turn".into(),item: serde_json::from_value(serde_json::json!({"id":"item","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new full body","phase":"unknown"}}}}})).unwrap()});
        let current = snapshot.conversations.clone();
        assert!(
            request
                .complete(&mut snapshot, old, true)
                .unwrap()
                .is_empty()
        );
        assert!(Arc::ptr_eq(&current, &snapshot.conversations));
    }
    #[test]
    fn delta_and_reopen_invalidate_body_and_coalesce_into_one_retry() {
        for reopen in [false, true] {
            let (mut snapshot, request) = fixture();
            let old = read(&snapshot, &request);
            if reopen {
                // Even a read failure retaining cached Item Arcs changes the subscription.
                Arc::make_mut(&mut snapshot.subscriptions).insert(
                    agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "chat".into(),
                    },
                    uuid::Uuid::new_v4(),
                );
            } else {
                update(
                    &mut snapshot,
                    SessionChange::Text {
                        turn_id: "turn".into(),
                        item_id: "item".into(),
                        field: TextField::AssistantText,
                        delta: "delta".into(),
                    },
                );
            }
            assert!(snapshot.error.is_none());
            assert!(snapshot.subscriptions.contains_key(&request.thread_id));
            let effects = request.clone().complete(&mut snapshot, old, true).unwrap();
            assert_eq!(effects.len(), 1);
            assert!(item_source(&request, &snapshot).unwrap().is_deferred());
            assert_ne!(
                item_text(item_source(&request, &snapshot).unwrap()),
                Some("old full body")
            );
        }
    }
    #[test]
    fn deletion_rejects_completion_and_epoch_change_retries() {
        for deleted in [false, true] {
            let (mut snapshot, request) = fixture();
            let old = read(&snapshot, &request);
            if deleted {
                update(
                    &mut snapshot,
                    SessionChange::RemoveItem {
                        turn_id: "turn".into(),
                        item_id: "item".into(),
                    },
                );
            }
            let result = request.complete(&mut snapshot, old, deleted);
            if deleted {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().len(), 1);
            }
        }
    }
    #[test]
    fn transfer_failure_keeps_deferred_and_new_delta_still_gets_a_retry() {
        for (changed, error) in [
            (false, PeerError::InvalidMessage("digest mismatch".into())),
            (true, PeerError::InvalidMessage("digest mismatch".into())),
            (
                false,
                PeerError::RequestTimeout {
                    method: "item body transfer".into(),
                },
            ),
            (
                true,
                PeerError::RequestTimeout {
                    method: "item body transfer".into(),
                },
            ),
        ] {
            let (mut snapshot, request) = fixture();
            let mut old = read(&snapshot, &request);
            old.response = Err(error);
            if changed {
                update(
                    &mut snapshot,
                    SessionChange::Text {
                        turn_id: "turn".into(),
                        item_id: "item".into(),
                        field: TextField::AssistantText,
                        delta: "delta".into(),
                    },
                );
            }
            let result = request.clone().complete(&mut snapshot, old, true);
            if changed {
                assert_eq!(result.unwrap().len(), 1);
            } else {
                assert!(result.is_err());
            }
            assert!(item_source(&request, &snapshot).unwrap().is_deferred());
        }
    }
}
