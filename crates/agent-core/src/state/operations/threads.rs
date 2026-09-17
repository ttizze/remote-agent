use super::*;

pub use crate::client::AddProject;
rpc::rpc_method!(
    AddProject,
    String,
    "host/project/add",
    AddProject,
    |self| self.clone()
);

impl Operation for AddProject {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, cwd: Self::Output) -> Vec<Effect> {
        let (next, mut effects) = reduce_intent(snapshot, Intent::NewChat { cwd });
        *snapshot = next;
        effects.push(Effect::execute(ListThreads::new(
            (*snapshot.list_query).clone(),
        )));
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ListThreads::new(
            (*snapshot.list_query).clone(),
        ))]
    }
}

pub use crate::client::ListThreads;
impl ListThreads {
    pub fn new(query: ListQuery) -> Self {
        Self { query }
    }
}
rpc::rpc_method!(
    ListThreads,
    ThreadList,
    "host/thread/list",
    ListThreads,
    |self| self.clone()
);

impl Operation for ListThreads {
    rpc_operation!();
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        self.query != *snapshot.list_query
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, mut threads: Self::Output) -> Vec<Effect> {
        if let Some(errors) = threads.provider_errors.as_ref()
            && let Some(previous) = &snapshot.threads
        {
            for cached in &previous.data {
                let Some(id) = cached.id.as_ref() else {
                    continue;
                };
                let session = cached
                    .session
                    .clone()
                    .or_else(|| crate::session::SessionRef::from_thread_id(id).ok());
                let Some(session) = session else { continue };
                let provider = match session.provider {
                    crate::session::ProviderKind::Codex => "codex",
                    crate::session::ProviderKind::Claude => "claude",
                };
                if errors.contains_key(provider)
                    && !threads
                        .data
                        .iter()
                        .any(|thread| thread.id.as_ref() == Some(id))
                {
                    let mut cached = cached.clone();
                    cached.list_stale = Some(true);
                    cached.status = None;
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
        for summary in &threads.data {
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
        snapshot.threads = Some(Arc::new(threads));
        Vec::new()
    }
}

pub use crate::client::ReadItem;
impl rpc::RpcMethod for ReadItem {
    type Output = rpc::ItemResponse;
    const METHOD: &'static str = "host/thread/item/read";
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::ReadItem(self.clone()))
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.item.id == self.item_id.as_str() {
            Ok(())
        } else {
            Err("item ID does not match")
        }
    }
}

/// The item Arc is its local source generation. Session updates replace this
/// Arc even when the new value happens to equal the old one.
#[derive(Debug)]
pub struct ItemRead {
    source: Option<Arc<Item>>,
    subscription: Option<uuid::Uuid>,
    response: Result<rpc::ItemResponse, PeerError>,
}

impl ReadItem {
    fn source<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a Arc<Item>> {
        snapshot
            .conversations
            .get(&self.thread_id)?
            .turns
            .as_ref()?
            .iter()
            .rfind(|turn| turn.id == self.turn_id)?
            .items
            .as_ref()?
            .iter()
            .find(|item| item.id == self.item_id)
    }
    fn deferred(&self, snapshot: &Snapshot) -> bool {
        snapshot
            .conversations
            .get(&self.thread_id)
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.iter().rfind(|turn| turn.id == self.turn_id))
            .and_then(|turn| turn.deferred_item_ids.as_ref())
            .is_some_and(|ids| ids.contains(&self.item_id))
    }
    fn apply_read(
        self,
        snapshot: &mut Snapshot,
        output: ItemRead,
        current_epoch: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        let current = self.source(snapshot);
        if !current_epoch
            || output.subscription != snapshot.subscriptions.get(&self.thread_id).copied()
            || !current
                .zip(output.source.as_ref())
                .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        {
            // Full Item updates already supply the new body. A delta preserves
            // the deferred marker, so retry once this transfer has finished.
            return if current.is_some() && self.deferred(snapshot) {
                Ok(vec![Effect::continuation(self)])
            } else if current.is_some() {
                Ok(Vec::new())
            } else {
                Err(PeerError::InvalidMessage(
                    "item is no longer available".into(),
                ))
            };
        }
        let response = output.response?;
        *snapshot = upsert_item(snapshot, &self.thread_id, &self.turn_id, response.item);
        reconcile_pending(snapshot, &self.thread_id);
        Ok(Vec::new())
    }
}
impl Operation for ReadItem {
    fn item_read(&self) -> Option<&ReadItem> {
        Some(self)
    }
    type Output = ItemRead;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let source = self.source(context.snapshot).cloned();
        let subscription = context.snapshot.subscriptions.get(&self.thread_id).copied();
        let response = context.call(self).await?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            response.resolve(context.session),
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
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        self.apply_read(snapshot, output, current)
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadThread {
    pub thread_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub open: bool,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = 5))]
    pub limit: u32,
}
impl ReadThread {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            open: false,
            limit: 5,
        }
    }
    pub(super) fn with_history(mut self, thread: Option<&Thread>) -> Self {
        if let Some(thread) = thread {
            let requested = thread.history_limit.unwrap_or(5);
            self.limit = self
                .limit
                .max(u32::try_from(requested).unwrap_or(u32::MAX))
                .max(
                    thread
                        .turns
                        .as_ref()
                        .map_or(0, |turns| u32::try_from(turns.len()).unwrap_or(u32::MAX)),
                );
        }
        self.limit = self.limit.max(5);
        self
    }
    pub fn open(thread_id: String) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
impl rpc::RpcMethod for ReadThread {
    type Output = crate::session::OpenedSession;
    const METHOD: &'static str = "host/session/open";
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::OpenSession(
            crate::session::OpenSession {
                session: crate::session::SessionRef::from_thread_id(&self.thread_id)
                    .map_err(|error| PeerError::InvalidMessage(error.into()))?,
                limit: self.limit as usize,
            },
        ))
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.session.thread_id() != self.thread_id {
            return Err("session identity does not match");
        }
        rpc::validate_thread(&output.response, Some(&self.thread_id))
    }
}

impl Operation for ReadThread {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        self.open
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        *self = self
            .clone()
            .with_history(snapshot.conversations.get(&self.thread_id).map(Arc::as_ref));
        if self.open {
            let cwd = snapshot
                .conversations
                .get(&self.thread_id)
                .and_then(|thread| thread.cwd.as_ref())
                .or_else(|| {
                    snapshot
                        .threads
                        .as_ref()?
                        .data
                        .iter()
                        .find(|thread| thread.id.as_ref() == Some(&self.thread_id))?
                        .cwd
                        .as_ref()
                })
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
                .filter(|turn| turn.status.as_deref() != Some("inProgress"))
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
        let mut details: Vec<_> = output
            .response
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| {
                turn.items
                    .iter()
                    .flatten()
                    .filter(|item| {
                        matches!(
                            item.kind.as_deref(),
                            Some("userMessage" | "agentMessage" | "imageGeneration")
                        ) && turn
                            .deferred_item_ids
                            .as_ref()
                            .is_some_and(|ids| ids.contains(&item.id))
                    })
                    .map(|item| {
                        Effect::execute(ReadItem {
                            thread_id: self.thread_id.clone(),
                            turn_id: turn.id.clone(),
                            item_id: item.id.clone(),
                        })
                    })
            })
            .collect();
        let mut effects = if self.open {
            open_thread(snapshot, output.response.thread, output.response.model)
        } else {
            refresh_thread(snapshot, output.response.thread)
        };
        effects.append(&mut details);
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

fn select_thread(snapshot: &mut Snapshot, id: String, cwd: String) {
    if snapshot.navigation.cwd != cwd {
        clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
    }
    let navigation = Arc::make_mut(&mut snapshot.navigation);
    navigation.thread_id = Some(id.clone());
    navigation.draft_key = id;
    navigation.cwd = cwd;
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    thread: Thread,
    model: Option<String>,
) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let mut effects = refresh_thread(snapshot, thread);
    if let Some(id) = id {
        navigate(
            snapshot,
            Navigation {
                thread_id: Some(id.clone()),
                draft_key: id.clone(),
                cwd,
            },
        );
        if let Some(model) = model {
            let (updated, _) = reduce(
                snapshot,
                Event::Intent(Intent::SelectModel {
                    thread_id: id,
                    model,
                }),
            );
            *snapshot = updated;
        }
    }
    effects.extend(review_workspace(snapshot));
    effects
}

pub(super) fn refresh_thread(snapshot: &mut Snapshot, incoming: Thread) -> Vec<Effect> {
    let Some(id) = incoming.id.clone().filter(|id| !id.trim().is_empty()) else {
        snapshot.error = Some("thread ID is missing".into());
        return Vec::new();
    };
    let thread = incoming;
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
    project_requests(snapshot);
    reconcile_pending(snapshot, &id);

    Vec::new()
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    #[serde(default)]
    pub exclude_turns: bool,
}
impl ForkThread {
    pub fn new(thread_id: String, last_turn_id: String) -> Self {
        Self {
            thread_id,
            last_turn_id,
            exclude_turns: false,
        }
    }
}
impl rpc::RpcMethod for ForkThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "thread/fork";
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::ForkThread(self.clone()))
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for ForkThread {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = output.thread.id.clone().expect("validated thread ID");
        let mut effects = open_thread(snapshot, output.thread, output.model);
        effects.push(Effect::execute(ReadThread::new(id)));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListThreads::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartThread {
    pub cwd: Option<String>,
    pub model: Option<String>,
}

impl rpc::RpcMethod for StartThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/start";
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::StartThread(self.clone()))
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for StartThread {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    const APPLY_WHEN_STALE: bool = true;
    fn outcome(output: &mut Self::Output) -> Outcome {
        ForkThread::outcome(output)
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupt {
    pub thread_id: String,
    pub turn_id: String,
}
rpc::rpc_method!(
    Interrupt,
    crate::models::Empty,
    "turn/interrupt",
    Interrupt,
    |self| self.clone()
);

impl Operation for Interrupt {
    rpc_operation!();
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {}
impl Operation for LoadModels {
    type Output = crate::client::ModelPage;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.models().await
    }
    fn apply(self, snapshot: &mut Snapshot, catalog: Self::Output) -> Vec<Effect> {
        let models = catalog.data;
        let errors = catalog
            .provider_errors
            .as_ref()
            .cloned()
            .unwrap_or_default();
        let drafts = snapshot.drafts.clone();
        for (id, previous_draft) in drafts.iter() {
            let settings = supported_settings(previous_draft, &models, &errors);
            if settings
                != (
                    previous_draft.model.as_deref(),
                    previous_draft.effort.as_deref(),
                    previous_draft.service_tier.as_deref(),
                )
                && let Some(draft) = shared_mut(&mut snapshot.drafts, id)
            {
                draft.model = settings.0.map(str::to_owned);
                draft.effort = settings.1.map(str::to_owned);
                draft.service_tier = settings.2.map(str::to_owned);
            }
        }
        snapshot.models = Arc::new(models);
        snapshot.model_errors = Arc::new(errors);
        Vec::new()
    }
}

pub use crate::client::OpenRequest;
impl rpc::RpcMethod for OpenRequest {
    type Output = crate::session::SessionRef;
    const METHOD: &'static str = "host/session/request";
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        Ok(crate::protocol::Call::RequestSession(self.clone()))
    }
}
impl Operation for OpenRequest {
    type Output = crate::session::OpenedSession;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let session = context.client.call(self).await?;
        let id = session.thread_id();
        let open = ReadThread::new(id.clone())
            .with_history(context.snapshot.conversations.get(&id).map(Arc::as_ref));
        context.call(&open).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.thread_id()).apply(snapshot, output)
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.thread_id()).stale(snapshot, output)
    }
}

#[cfg(test)]
mod item_read_tests {
    use super::*;
    use crate::session::{SessionChange, TextField};

    fn fixture() -> (Snapshot, ReadItem) {
        let thread = serde_json::from_value(serde_json::json!({"id":"chat","turns":[{"id":"turn","status":"inProgress","deferredItemIds":["item"],"items":[{"id":"item","type":"agentMessage","text":"summary"}]}]})).unwrap();
        let snapshot = Snapshot {
            conversations: Arc::new(BTreeMap::from([("chat".into(), Arc::new(thread))])),
            ..Default::default()
        };
        (
            snapshot,
            ReadItem {
                thread_id: "chat".into(),
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
        )
    }
    fn read(snapshot: &Snapshot, request: &ReadItem) -> ItemRead {
        ItemRead { source: request.source(snapshot).cloned(), subscription: snapshot.subscriptions.get("chat").copied(), response: Ok(rpc::ItemResponse {item: serde_json::from_value(serde_json::json!({"id":"item","type":"agentMessage","text":"old full body","status":"inProgress"})).unwrap(), transfer:None,}) }
    }
    fn update(snapshot: &mut Snapshot, change: SessionChange) {
        let thread = change.apply(&snapshot.conversations["chat"]).unwrap();
        Arc::make_mut(&mut snapshot.conversations).insert("chat".into(), Arc::new(thread));
    }
    #[test]
    fn full_item_update_wins_over_late_body_and_its_status() {
        let (mut snapshot, request) = fixture();
        let old = read(&snapshot, &request);
        update(&mut snapshot, SessionChange::Item {turn_id:"turn".into(),item: serde_json::from_value(serde_json::json!({"id":"item","type":"agentMessage","text":"new full body","status":"completed"})).unwrap()});
        let current = snapshot.conversations.clone();
        assert!(
            request
                .apply_read(&mut snapshot, old, true)
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
                Arc::make_mut(&mut snapshot.subscriptions)
                    .insert("chat".into(), uuid::Uuid::new_v4());
            } else {
                update(
                    &mut snapshot,
                    SessionChange::Text {
                        turn_id: "turn".into(),
                        item_id: "item".into(),
                        field: TextField::Message,
                        delta: "delta".into(),
                    },
                );
            }
            let effects = request
                .clone()
                .apply_read(&mut snapshot, old, true)
                .unwrap();
            assert_eq!(effects.len(), 1);
            assert!(request.deferred(&snapshot));
            assert_ne!(
                request.source(&snapshot).unwrap().text.as_deref(),
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
            let result = request.apply_read(&mut snapshot, old, deleted);
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
                        field: TextField::Message,
                        delta: "delta".into(),
                    },
                );
            }
            let result = request.clone().apply_read(&mut snapshot, old, true);
            if changed {
                assert_eq!(result.unwrap().len(), 1);
            } else {
                assert!(result.is_err());
            }
            assert!(request.deferred(&snapshot));
        }
    }
}
