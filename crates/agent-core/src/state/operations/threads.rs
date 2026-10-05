use super::*;
use crate::client::ClientExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportHistory {}
rpc::rpc_method!(ImportHistory, ImportHistory, |self| crate::models::Empty {});
impl Operation for ImportHistory {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ListSessions::new(
            (*snapshot.list_query).clone(),
        ))]
    }
}

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

pub use agent_protocol::operations::ListSessions;

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
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
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

fn normalize_draft_settings(snapshot: &mut Snapshot, key: &DraftKey, catalog_received: bool) {
    let provider = match key {
        DraftKey::Session { session } | DraftKey::Queued { session, .. } => {
            snapshot.session_provider(session)
        }
        DraftKey::Local { .. } => None,
    };
    let previous = snapshot.drafts.get(key).cloned().unwrap_or_default();
    let mut draft = previous.clone();
    if catalog_received || !snapshot.models.is_empty() {
        let settings = supported_settings(
            previous.model.as_ref(),
            previous.effort.as_deref(),
            previous.service_tier.as_deref(),
            provider,
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
    if !snapshot.drafts.contains_key(key) || !Arc::ptr_eq(&draft, &previous) {
        Arc::make_mut(&mut snapshot.drafts).insert(key.clone(), draft);
    }
}

fn select_thread(snapshot: &mut Snapshot, id: crate::session::SessionRef, cwd: String) {
    normalize_draft_settings(snapshot, &DraftKey::from(&id), false);
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
        normalize_draft_settings(snapshot, &DraftKey::from(&id), false);
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
    super::super::queued_edit::reconcile(snapshot);
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
        snapshot.models = Arc::new(catalog.data);
        snapshot.model_errors = Arc::new(catalog.provider_errors.unwrap_or_default());
        let drafts = snapshot.drafts.clone();
        for key in drafts.keys() {
            normalize_draft_settings(snapshot, key, true);
        }
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

    #[test]
    fn confirmed_empty_catalog_clears_unsupported_options_but_loading_or_failed_catalog_preserves_them()
     {
        let session = crate::session::SessionRef {
            id: "conversation".into(),
        };
        let key = DraftKey::from(&session);
        let original = Arc::new(Draft {
            text: "keep text".into(),
            model: Some(crate::models::ModelRef {
                provider: crate::session::ProviderKind::Codex,
                id: "selected".into(),
            }),
            effort: Some("max".into()),
            service_tier: Some("priority".into()),
            ..Default::default()
        });
        for failed in [false, true] {
            let mut snapshot = Snapshot {
                drafts: Arc::new([(key.clone(), original.clone())].into()),
                ..Default::default()
            };
            ReadThread::open(session.clone())
                .prepare(&mut snapshot)
                .unwrap();
            assert!(Arc::ptr_eq(&snapshot.drafts[&key], &original));
            LoadModels {}.apply(
                &mut snapshot,
                agent_protocol::operations::ModelPage {
                    data: Vec::new(),
                    next_cursor: None,
                    provider_errors: failed.then(|| {
                        [("codex".into(), serde_json::json!({"message":"unavailable"}))]
                            .into_iter()
                            .collect()
                    }),
                },
            );
            let draft = &snapshot.drafts[&key];
            assert_eq!(draft.text, original.text);
            assert_eq!(draft.model, original.model);
            assert_eq!(draft.effort.as_deref(), failed.then_some("max"));
            assert_eq!(draft.service_tier.as_deref(), failed.then_some("priority"));
        }
    }

    #[rstest::rstest]
    #[case::new_draft(false, false)]
    #[case::existing_empty_draft(true, false)]
    #[case::saved_settings(true, true)]
    fn opening_without_model_metadata_uses_the_provider_catalog(
        #[values(true, false)] catalog_first: bool,
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
        let session = SessionRef::new("external".into()).unwrap();
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
                provider: Some(provider),
                ..Default::default()
            },
            None,
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
                id: if saved_choice { "saved" } else { "default" }.into(),
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
        assert_eq!(snapshot.model_quick_controls(key).effort, effort);
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
        let thread = serde_json::from_value(serde_json::json!({"provider":"codex","id":{"id":"chat"},"turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":{"deferred":{"summary":{"assistantText":{"text":"summary","phase":"unknown"}}}}}]}]})).unwrap();
        let snapshot = Snapshot {
            conversations: Arc::new(BTreeMap::from([(
                agent_protocol::session::SessionRef { id: "chat".into() },
                Arc::new(thread),
            )])),
            subscriptions: Arc::new(BTreeMap::from([(
                agent_protocol::session::SessionRef { id: "chat".into() },
                uuid::Uuid::new_v4(),
            )])),
            ..Default::default()
        };
        (
            snapshot,
            ReadItem {
                thread_id: agent_protocol::session::SessionRef { id: "chat".into() },
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
        )
    }
    fn read(snapshot: &Snapshot, request: &ReadItem) -> ItemRead {
        ItemRead { source: item_source(request, snapshot).cloned(), subscription: snapshot.subscriptions.get(&agent_protocol::session::SessionRef { id: "chat".into() }).copied(), response: Ok(rpc::ItemResponse {item: serde_json::from_value(serde_json::json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old full body","phase":"unknown"}}}}})).unwrap(), transfer:None,}) }
    }
    fn update(snapshot: &mut Snapshot, change: SessionChange) {
        let session = agent_protocol::session::SessionRef { id: "chat".into() };
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
                    agent_protocol::session::SessionRef { id: "chat".into() },
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
