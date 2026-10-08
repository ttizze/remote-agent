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

fn merge_unavailable(
    mut threads: crate::models::ThreadList,
    previous: Option<&crate::models::ThreadList>,
) -> crate::models::ThreadList {
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

/// A recent window can already prove a project's requested page. Reuse those
/// fresh rows instead of fetching the same titles and Git status again.
pub(crate) fn project_page(
    recent: &crate::models::ThreadList,
    id: &str,
    limit: u32,
) -> Option<crate::models::ThreadList> {
    if recent
        .provider_errors
        .as_ref()
        .is_some_and(|errors| !errors.is_empty())
    {
        return None;
    }
    let mut data: Vec<_> = recent
        .data
        .iter()
        .filter(|t| t.parent_id.is_none() && t.project_id.as_deref() == Some(id))
        .cloned()
        .collect();
    if data.len() <= limit as usize && recent.has_more {
        return None;
    }
    let has_more = data.len() > limit as usize;
    data.truncate(limit as usize);
    Some(crate::models::ThreadList {
        data,
        projects: recent
            .projects
            .iter()
            .filter(|p| p.id == id)
            .cloned()
            .collect(),
        has_more,
        provider_errors: None,
    })
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
        if snapshot.expanded_projects.get(&self.project_id) != Some(&self.limit) {
            return Vec::new();
        }
        let output = merge_unavailable(
            output,
            snapshot
                .project_threads
                .get(&self.project_id)
                .map(AsRef::as_ref),
        );
        update_titles(snapshot, &output.data);
        Arc::make_mut(&mut snapshot.project_threads).insert(self.project_id, Arc::new(output));
        Vec::new()
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
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::SessionList)
    }
    rpc_operation!();
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestList(self.query.clone())
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        if snapshot.list_query.search_term != self.query.search_term {
            snapshot.expanded_projects = Arc::default();
            snapshot.project_threads = Arc::default();
            Arc::make_mut(&mut snapshot.operations)
                .retain(|key, _| !matches!(key, OperationKey::ProjectList { .. }));
        }
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
        let threads = merge_unavailable(threads, snapshot.threads.as_deref());
        update_titles(snapshot, &threads.data);
        let visible: std::collections::HashSet<_> =
            threads.projects.iter().map(|p| p.id.as_str()).collect();
        Arc::make_mut(&mut snapshot.expanded_projects)
            .retain(|id, _| visible.contains(id.as_str()));
        Arc::make_mut(&mut snapshot.project_threads).retain(|id, _| visible.contains(id.as_str()));
        Arc::make_mut(&mut snapshot.operations).retain(|key, _| match key {
            OperationKey::ProjectList { project_id } => {
                snapshot.expanded_projects.contains_key(project_id)
            }
            _ => true,
        });
        snapshot.threads = Some(Arc::new(threads));
        let mut effects = Vec::new();
        for (id, limit) in snapshot.expanded_projects.iter() {
            if let Some(page) = project_page(snapshot.threads.as_ref().unwrap(), id, *limit) {
                Arc::make_mut(&mut snapshot.project_threads).insert(id.clone(), Arc::new(page));
                Arc::make_mut(&mut snapshot.operations).remove(&OperationKey::ProjectList {
                    project_id: id.clone(),
                });
            } else if snapshot.connected {
                effects.push(Effect::execute(ListProjectSessions {
                    project_id: id.clone(),
                    limit: *limit,
                    search_term: snapshot.list_query.search_term.clone(),
                }));
            }
        }
        effects
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
                json!({"data":[],"projects":[{"id":"old","name":"Old","roots":[]}],"hasMore":true});
            for _ in 0..3 {
                let request = read(&mut reader).await;
                let result = match request["method"].as_str().unwrap() {
                    "host/session/list" => page.clone(),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
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
                snapshot.threads.is_none() || !snapshot.operations.is_empty()
            } {
                updates.changed().await.unwrap();
            }
            store
                .dispatch(Intent::SetProjectExpanded {
                    project_id: "old".into(),
                    expanded: true,
                })
                .await
                .unwrap();
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
            assert!(store.snapshot().project_threads.is_empty());
            assert!(
                store.snapshot().thread_list().unwrap().projects[0]
                    .error
                    .is_none()
            );
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
        let empty_list = json!({"data":[],"projects":[],"hasMore":false});
        for _ in 0..5 {
            let request = read(&mut reader).await;
            let result = match request["method"].as_str().unwrap() {
                "host/session/list" => empty_list.clone(),
                "host/account/list" => json!({"accounts":[],"selected":{}}),
                "host/taskActivity/read" => {
                    json!({"revision":0,"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
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
        let latest = read(&mut reader).await;
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

    #[test]
    fn recent_list_publishes_before_open_project_reads_and_closing_discards_results() {
        let page = |id: &str| {
            serde_json::from_value::<crate::models::ThreadList>(serde_json::json!({"data":[],"projects":[{"id":id,"name":id,"roots":[]}],"hasMore":true})).unwrap()
        };
        let mut snapshot = Snapshot {
            connected: true,
            ..Default::default()
        };
        assert!(
            ListSessions::new(Default::default())
                .apply(&mut snapshot, page("old"))
                .is_empty()
        );
        let (opened, effects) = reduce_intent(
            &snapshot,
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            },
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(opened.expanded_projects["old"], 5);
        let mut opened = opened;
        let recent = opened.threads.clone();
        ListProjectSessions {
            project_id: "old".into(),
            limit: 5,
            search_term: String::new(),
        }
        .apply(&mut opened, page("old"));
        assert!(Arc::ptr_eq(
            recent.as_ref().unwrap(),
            opened.threads.as_ref().unwrap()
        ));
        assert_eq!(opened.project_threads.len(), 1);
        assert_eq!(
            ListSessions::new(Default::default())
                .apply(&mut opened, page("old"))
                .len(),
            1,
            "only an opened project is refreshed after recent publication"
        );
        let (mut closed, effects) = reduce_intent(
            &opened,
            Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: false,
            },
        );
        assert!(effects.is_empty());
        ListProjectSessions {
            project_id: "old".into(),
            limit: 5,
            search_term: String::new(),
        }
        .apply(&mut closed, page("old"));
        assert!(closed.project_threads.is_empty());
        assert!(
            ListSessions::new(Default::default())
                .apply(&mut closed, page("old"))
                .is_empty()
        );
        assert_eq!(closed.navigation, opened.navigation);
        assert_eq!(closed.drafts, opened.drafts);
    }
    proptest::proptest! {
        #[test]
        fn recent_project_pages_require_complete_evidence(
            roots in 0usize..16, others in 0usize..8, children in 0usize..8,
            limit in 1u32..16, has_more in proptest::bool::ANY, failed in proptest::bool::ANY,
        ) {
            let mut data=Vec::new();
            for (count,project,parent) in [(roots,"p",None),(others,"q",None),(children,"p",Some(SessionRef::new(ProviderKind::Codex,"parent".into()).unwrap()))] {
                for index in 0..count {
                    data.push(Thread { id:Some(SessionRef::new(ProviderKind::Codex,format!("{project}-{parent:?}-{index}")).unwrap()),project_id:crate::models::ProjectMembership::Assigned(project.into()),parent_id:parent.clone(),..Default::default() });
                }
            }
            let recent=crate::models::ThreadList { data,projects:Vec::new(),has_more,provider_errors:failed.then(||[("codex".into(),serde_json::json!({"message":"unavailable"}))].into_iter().collect()) };
            let original=recent.clone();
            let page=project_page(&recent,"p",limit);
            proptest::prop_assert_eq!(&recent,&original);
            proptest::prop_assert_eq!(page.is_some(),!failed && (roots>limit as usize || !has_more));
            if let Some(page)=page {
                proptest::prop_assert_eq!(page.data.len(),roots.min(limit as usize));
                proptest::prop_assert_eq!(page.has_more,roots>limit as usize);
                proptest::prop_assert!(page.data.iter().all(|thread|thread.parent_id.is_none() && thread.project_id.as_deref()==Some("p")));
            }
        }
    }

    #[test]
    fn opening_an_old_project_task_uses_metadata_before_history_and_clears_cached_status_on_disconnect()
     {
        let id = SessionRef::new(ProviderKind::Codex, "old".into()).unwrap();
        let page: crate::models::ThreadList = serde_json::from_value(serde_json::json!({
            "data":[{"id":id,"projectId":"p","cwd":"/old-project","status":"running","canAcceptDirectInput":false}],
            "projects":[{"id":"p","name":"P","roots":[]}],"hasMore":false,
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
    fn recent_rows_prove_project_page_only_with_lookahead_or_global_end() {
        let mut recent: crate::models::ThreadList=serde_json::from_value(serde_json::json!({"data":[{"id":{"provider":"codex","id":"1"},"projectId":"p"},{"id":{"provider":"codex","id":"2"},"projectId":"p"}],"projects":[{"id":"p","name":"P","roots":[]}],"hasMore":true})).unwrap();
        assert!(project_page(&recent, "p", 2).is_none());
        let page = project_page(&recent, "p", 1).unwrap();
        assert_eq!(page.data.len(), 1);
        assert!(page.has_more);
        recent.has_more = false;
        let page = project_page(&recent, "p", 2).unwrap();
        assert_eq!(page.data.len(), 2);
        assert!(!page.has_more);
        assert_eq!(page.projects[0].id, "p");
        assert_eq!(project_page(&recent, "empty", 5).unwrap().data.len(), 0);
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
