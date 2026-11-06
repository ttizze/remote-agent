//! One conversation: opening, history and item reads, creation, forks and interrupts.
use super::*;
use crate::models::Item;

pub use agent_protocol::operations::ReadItem;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.session.clone(),
        })
    }
    type Input = Option<uuid::Uuid>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(snapshot.sessions.subscriptions.get(&self.session).copied())
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
        if subscription != snapshot.sessions.subscriptions.get(&self.session).copied()
            || snapshot
                .sessions
                .conversations
                .get(&self.session)
                .is_none_or(|thread| thread.history_cursor.as_deref() != Some(&self.cursor))
        {
            return Vec::new();
        }
        let details = item_details(&self.session, &page.turns);
        page.prepend_to(shared_mut(&mut snapshot.sessions.conversations, &self.session).unwrap());
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
    let turns = snapshot
        .sessions
        .conversations
        .get(&request.thread_id)?
        .turns
        .as_deref()?;
    let (turn, item) = crate::session::item_position(turns, &request.turn_id, &request.item_id)?;
    Some(&turns[turn].items.as_ref()?[item])
}
impl Operation for ReadItem {
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Item { item: self.clone() })
    }
    type Input = (Option<Arc<Item>>, Option<uuid::Uuid>);
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok((
            item_source(self, snapshot).cloned(),
            snapshot
                .sessions
                .subscriptions
                .get(&self.thread_id)
                .copied(),
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
            || output.subscription
                != snapshot
                    .sessions
                    .subscriptions
                    .get(&self.thread_id)
                    .copied()
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
        let thread = crate::session::SessionChange::Item {
            turn_id: self.turn_id,
            item: Arc::new(response.item),
        }
        .apply(&snapshot.sessions.conversations[&self.thread_id])
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(self.thread_id.clone(), Arc::new(thread));
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
    // UniFFI defaults must be literals; keep this equal to HISTORY_WINDOW_TURNS.
    #[cfg_attr(feature = "bindings", uniffi(default = 10))]
    pub limit: u32,
}
/// Turns an opened conversation shows first, and each older page adds.
pub const HISTORY_WINDOW_TURNS: u32 = 10;
impl ReadThread {
    pub fn new(thread_id: crate::session::SessionRef) -> Self {
        Self {
            thread_id,
            open: false,
            limit: HISTORY_WINDOW_TURNS,
        }
    }
    pub fn open(thread_id: crate::session::SessionRef) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
/// The request for a ReadThread: the Host omits a window the client holds.
#[derive(Debug, Serialize)]
pub struct OpenThread<'a> {
    pub read: &'a ReadThread,
    pub known_version: Option<u64>,
}
impl rpc::RpcMethod for OpenThread<'_> {
    agent_protocol::operations::rpc_contract!(OpenSession);
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(crate::session::OpenSession {
            session: self.read.thread_id.clone(),
            limit: self.read.limit as usize,
            include_activity: false,
            known_version: self.known_version,
        })
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.session != self.read.thread_id {
            return Err("session identity does not match");
        }
        if output.unchanged {
            return Ok(());
        }
        rpc::validate_thread(&output.response, Some(&self.read.thread_id))
    }
}

/// An unchanged reply keeps the held window's turns and paging.
fn held_window(
    snapshot: &Snapshot,
    id: &crate::session::SessionRef,
    output: &mut crate::session::OpenedSession,
) {
    if !output.unchanged {
        return;
    }
    if let Some(held) = snapshot.sessions.conversations.get(id) {
        let thread = &mut output.response.thread;
        thread.turns = held.turns.clone();
        thread.history_cursor = held.history_cursor.clone();
        thread.history_has_more = held.history_has_more;
        thread.history_read_state = held.history_read_state.clone();
    }
}

impl Operation for ReadThread {
    fn navigation_bound(&self) -> bool {
        self.open
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.thread_id.clone(),
        })
    }
    /// A held window's version, when it has the requested size.
    type Input = Option<u64>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(snapshot
            .sessions
            .conversations
            .get(&self.thread_id)
            .filter(|thread| {
                thread.turns.is_some() && thread.history_limit == Some(u64::from(self.limit))
            })
            .and_then(|thread| thread.transcript_version))
    }
    type Output = crate::session::OpenedSession;
    async fn run(
        &self,
        known_version: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&OpenThread {
                read: self,
                known_version,
            })
            .await
    }
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
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, mut output: Self::Output) -> Vec<Effect> {
        held_window(snapshot, &self.thread_id, &mut output);
        if output
            .response
            .thread
            .history_read_state
            .as_ref()
            .is_some_and(|state| state.kind == crate::session::HistoryReadKind::Unavailable)
            && let Some(cached) = snapshot.sessions.conversations.get(&self.thread_id)
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
                if let Some(index) = turns
                    .iter()
                    .rposition(|turn| turn.matches_occurrence(&current))
                {
                    turns[index] = current;
                } else {
                    turns.push(current);
                }
            }
            *live = turns;
        }
        let mut effects = if self.open {
            open_thread(
                snapshot,
                output.response.thread,
                output.response.model,
                output.response.effort,
                output.response.service_tier,
            )
        } else {
            refresh_thread(snapshot, output.response.thread);
            Vec::new()
        };
        effects.extend(item_details(
            &self.thread_id,
            snapshot
                .sessions
                .conversations
                .get(&self.thread_id)
                .and_then(|thread| thread.turns.as_deref())
                .unwrap_or_default(),
        ));
        Arc::make_mut(&mut snapshot.sessions.subscriptions)
            .insert(self.thread_id, output.subscription_id);
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, mut output: Self::Output) -> Vec<Effect> {
        held_window(snapshot, &self.thread_id, &mut output);
        // A late navigation result can warm the cache, but must never replace
        // an already subscribed, more recent view or change the selected draft.
        if !snapshot
            .sessions
            .subscriptions
            .contains_key(&self.thread_id)
        {
            refresh_thread(snapshot, output.response.thread);
        }
        Vec::new()
    }
}

fn select_thread(snapshot: &mut Snapshot, id: crate::session::SessionRef, cwd: String) {
    let key: DraftKey = id.clone().into();
    let previous = snapshot.input.drafts.get(&key).cloned().unwrap_or_default();
    let mut draft = previous.clone();
    if previous.model.is_some() {
        let settings = supported_settings(
            previous.model.as_ref(),
            previous.effort.as_deref(),
            previous.service_tier.as_deref(),
            snapshot.draft_provider(&key, previous.model.as_ref()),
            &snapshot.catalog.models,
            &snapshot.catalog.model_errors,
            snapshot.allows_custom_codex_models(),
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
    if !snapshot.input.drafts.contains_key(&key) || !Arc::ptr_eq(&draft, &previous) {
        Arc::make_mut(&mut snapshot.input.drafts).insert(key, draft);
    }
    navigate(
        snapshot,
        Navigation {
            thread_id: Some(id.clone()),
            draft_key: id.into(),
            cwd,
        },
    );
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    thread: Thread,
    model: Option<crate::models::ModelRef>,
    effort: Option<String>,
    service_tier: Option<String>,
) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    refresh_thread(snapshot, thread);
    if let Some(id) = id {
        let key = DraftKey::from(id.clone());
        let previous = snapshot.input.drafts.get(&key).cloned().unwrap_or_default();
        let same_model = model.is_none() || model == previous.model;
        let (model, effort, tier) = if previous.model_settings_changed {
            (
                previous.model.as_ref().or(model.as_ref()),
                previous.effort.as_deref(),
                previous.service_tier.as_deref(),
            )
        } else {
            (
                model.as_ref().or(previous.model.as_ref()),
                effort
                    .as_deref()
                    .or_else(|| previous.effort.as_deref().filter(|_| same_model)),
                service_tier
                    .as_deref()
                    .or_else(|| previous.service_tier.as_deref().filter(|_| same_model)),
            )
        };
        let (model, effort, tier) = supported_settings(
            model,
            effort,
            tier,
            Some(
                snapshot
                    .thread_metadata(&id)
                    .and_then(|thread| thread.active_provider)
                    .unwrap_or(id.provider),
            ),
            &snapshot.catalog.models,
            &snapshot.catalog.model_errors,
            snapshot.allows_custom_codex_models(),
        );
        let (model, effort, tier) = (
            model.cloned(),
            effort.map(str::to_owned),
            tier.map(str::to_owned),
        );
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(key)
                .or_default(),
        );
        draft.model = model;
        draft.effort = effort;
        draft.service_tier = tier;

        navigate(
            snapshot,
            Navigation {
                thread_id: Some(id.clone()),
                draft_key: id.clone().into(),
                cwd,
            },
        );
    }
    review_workspace(snapshot).into_iter().collect()
}

pub(super) fn refresh_thread(snapshot: &mut Snapshot, incoming: Thread) {
    let Some(id) = incoming.id.clone().filter(|id| id.validate().is_ok()) else {
        Arc::make_mut(&mut snapshot.pane).error = Some("thread ID is missing".into());
        return;
    };
    let mut thread = incoming;
    if let Some(cached) = snapshot.sessions.conversations.get(&id)
        && cached.list_revision > thread.list_revision
    {
        thread.name = cached.name.clone();
        thread.list_revision = cached.list_revision;
    }
    if let Some(cached) = snapshot.sessions.conversations.get(&id)
        && let Some(turns) = crate::session::retained_history(
            cached.turns.as_deref().unwrap_or_default(),
            thread.turns.as_deref().unwrap_or_default(),
        )
    {
        let count = thread.turns.as_ref().map_or(0, Vec::len);
        if thread.history_cursor.is_some() && turns.len() > count {
            let managed_start = thread
                .active_provider
                .and_then(|_| thread.history_cursor.as_ref()?.parse::<usize>().ok())
                .and_then(|start| start.checked_add(count)?.checked_sub(turns.len()));
            thread.turns = Some(turns);
            let pagination_state = if let Some(start) = managed_start {
                thread.history_cursor = (start > 0).then(|| start.to_string());
                thread.history_has_more = Some(start > 0);
                Some(crate::session::HistoryReadState::new(
                    if start > 0 {
                        crate::session::HistoryReadKind::Partial
                    } else {
                        crate::session::HistoryReadKind::Complete
                    },
                    vec![],
                ))
            } else {
                thread.history_cursor = cached.history_cursor.clone();
                thread.history_has_more = cached.history_has_more;
                cached.history_read_state.clone()
            };
            if thread.history_read_state.as_ref().is_none_or(|state| {
                matches!(
                    state.kind,
                    crate::session::HistoryReadKind::Complete
                        | crate::session::HistoryReadKind::Partial
                ) && state.issues.is_empty()
            }) {
                thread.history_read_state = pagination_state;
            }
        } else {
            thread.turns = Some(turns[turns.len() - count..].to_vec());
        }
    }
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), Arc::new(thread));
    reconcile_pending(snapshot, &id);
}

pub use agent_protocol::operations::ForkSession;

impl Operation for ForkSession {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Fork {
            session: self.thread_id.clone(),
            turn: self.last_turn_id.clone(),
        })
    }
    rpc_operation!();
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = output.thread.id.clone().expect("validated thread ID");
        let draft = snapshot
            .input
            .drafts
            .get(&DraftKey::from(self.thread_id.clone()))
            .cloned();
        if let Some(draft) = draft {
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(id.clone().into())
                .or_insert(draft);
        }
        let mut effects = open_thread(
            snapshot,
            output.thread,
            output.model,
            output.effort,
            output.service_tier,
        );
        effects.push(Effect::execute(ReadThread::new(id)));
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
pub use agent_protocol::operations::RetryPreparation;

impl Operation for RetryPreparation {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.thread_id.clone().into(),
        })
    }
    rpc_operation!();
}

impl Operation for CreateSession {
    // Creation flows present their returned error. Do not also leave an
    // unrelated application notice behind after the user successfully retries.
    const REPORT_ERROR: bool = false;
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.sessions.subscriptions)
            .insert(output.session, output.subscription_id);
        open_thread(
            snapshot,
            output.response.thread,
            output.response.model,
            output.response.effort,
            output.response.service_tier,
        )
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.sessions.subscriptions)
            .insert(output.session, output.subscription_id);
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
        let read = ReadThread::new(id);
        context
            .call(&OpenThread {
                read: &read,
                known_version: None,
            })
            .await
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
    use crate::session::{HistoryReadKind, ProviderKind, SessionRef};

    #[test]
    fn a_held_current_window_is_reused_and_only_its_metadata_refreshes() {
        let id = SessionRef::new(ProviderKind::Codex, "held".into()).unwrap();
        let held = Thread {
            id: Some(id.clone()),
            name: Some("old".into()),
            history_limit: Some(u64::from(HISTORY_WINDOW_TURNS)),
            history_cursor: Some("t3".into()),
            history_has_more: Some(true),
            transcript_version: Some(5),
            turns: Some(vec![Arc::new(crate::models::Turn {
                id: "turn".into(),
                ..Default::default()
            })]),
            ..Default::default()
        };
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), Arc::new(held));
        let open = ReadThread::open(id.clone());
        assert_eq!(open.capture(&snapshot).unwrap(), Some(5));
        let other_size = ReadThread {
            limit: 5,
            ..ReadThread::open(id.clone())
        };
        assert_eq!(other_size.capture(&snapshot).unwrap(), None);
        open.apply(
            &mut snapshot,
            crate::session::OpenedSession {
                session: id.clone(),
                subscription_id: uuid::Uuid::new_v4(),
                unchanged: true,
                response: crate::models::ThreadResponse {
                    thread: Thread {
                        id: Some(id.clone()),
                        name: Some("renamed".into()),
                        history_limit: Some(u64::from(HISTORY_WINDOW_TURNS)),
                        transcript_version: Some(5),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
        );
        let thread = &snapshot.sessions.conversations[&id];
        assert_eq!(thread.name.as_deref(), Some("renamed"));
        assert_eq!(thread.turns.as_ref().unwrap()[0].id, "turn".into());
        assert_eq!(thread.history_cursor.as_deref(), Some("t3"));
        assert_eq!(thread.history_has_more, Some(true));
    }

    #[rstest::rstest]
    #[case(HistoryReadKind::Incomplete, vec!["claude: Claude failed to start".into()])]
    #[case(HistoryReadKind::Incomplete, vec![])]
    #[case(HistoryReadKind::Unavailable, vec![])]
    #[case(HistoryReadKind::Partial, vec!["claude: Claude failed to start".into()])]
    fn retaining_older_pages_preserves_source_errors_until_a_successful_refresh(
        #[case] kind: HistoryReadKind,
        #[case] issues: Vec<String>,
    ) {
        use crate::{presentation::conversation::history_notice, session::HistoryReadState};
        use agent_protocol::{execution::TurnStatus, models::Turn};
        let id = SessionRef {
            provider: ProviderKind::Codex,
            id: "conversation".into(),
        };
        let turns: Vec<_> = ["one", "two", "three"]
            .into_iter()
            .map(|id| {
                Arc::new(Turn {
                    id: id.into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                })
            })
            .collect();
        let mut snapshot = Snapshot::default();
        let cached = Thread {
            id: Some(id.clone()),
            active_provider: Some(ProviderKind::Claude),
            turns: Some(turns.clone()),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), Arc::new(cached));
        let source_state = HistoryReadState::new(kind, issues);
        let mut incoming = Thread {
            id: Some(id.clone()),
            active_provider: Some(ProviderKind::Claude),
            turns: Some(turns[1..].to_vec()),
            history_cursor: Some("1".into()),
            history_has_more: Some(true),
            history_read_state: Some(source_state.clone()),
            ..Default::default()
        };
        refresh_thread(&mut snapshot, incoming.clone());
        let retained = &snapshot.sessions.conversations[&id];
        assert_eq!(retained.turns.as_ref().unwrap().len(), 3);
        assert!(retained.history_cursor.is_none());
        assert_eq!(retained.history_read_state.as_ref(), Some(&source_state));
        assert!(history_notice(retained).is_some());
        incoming.history_read_state = Some(HistoryReadState::new(HistoryReadKind::Partial, vec![]));
        refresh_thread(&mut snapshot, incoming);
        assert_eq!(history_notice(&snapshot.sessions.conversations[&id]), None);
    }

    #[test]
    fn unavailable_history_keeps_distinct_occurrences_of_a_reused_turn_id() {
        use crate::models::{ItemBody, Turn, TurnStatus};
        for same_occurrence in [false, true] {
            let id = SessionRef::new(ProviderKind::Codex, "offline".into()).unwrap();
            let turn = |item: &str, status| {
                Arc::new(Turn {
                    id: "reused".into(),
                    status,
                    items: Some(vec![Arc::new(Item::new(
                        item.into(),
                        Default::default(),
                        ItemBody::Sleep {},
                    ))]),
                    ..Default::default()
                })
            };
            let cached = turn("old", TurnStatus::Completed);
            let live = turn(
                if same_occurrence { "old" } else { "new" },
                TurnStatus::Running,
            );
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                id.clone(),
                Arc::new(Thread {
                    id: Some(id.clone()),
                    turns: Some(vec![cached.clone()]),
                    ..Default::default()
                }),
            );
            ReadThread::new(id.clone()).apply(
                &mut snapshot,
                crate::session::OpenedSession {
                    session: id.clone(),
                    subscription_id: uuid::Uuid::new_v4(),
                    unchanged: false,
                    response: crate::models::ThreadResponse {
                        thread: Thread {
                            id: Some(id.clone()),
                            status: crate::models::SessionStatus::Running,
                            history_read_state: Some(crate::session::HistoryReadState::new(
                                HistoryReadKind::Unavailable,
                                vec![],
                            )),
                            turns: Some(vec![live.clone()]),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                },
            );
            let turns = snapshot.sessions.conversations[&id].turns.as_ref().unwrap();
            assert_eq!(turns.len(), if same_occurrence { 1 } else { 2 });
            if !same_occurrence {
                assert!(Arc::ptr_eq(&turns[0], &cached));
            }
            assert_eq!(turns.last().unwrap().as_ref(), live.as_ref());
        }
    }

    #[rstest::rstest]
    fn missing_native_options_keep_saved_options_only_for_the_same_model(
        #[values(None, Some("saved"), Some("changed"))] native_model: Option<&str>,
    ) {
        let id = SessionRef::new(ProviderKind::Codex, "missing-options".into()).unwrap();
        let key = DraftKey::from(id.clone());
        let model = |name: &str| crate::models::ModelRef {
            provider: id.provider,
            id: name.into(),
        };
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                model: Some(model("saved")),
                effort: Some("high".into()),
                service_tier: Some("priority".into()),
                ..Default::default()
            }),
        );
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            native_model.map(model),
            None,
            None,
        );
        assert_eq!(
            snapshot.input.drafts[&key].model.as_ref().unwrap().id,
            native_model.unwrap_or("saved")
        );
        let same_model = native_model != Some("changed");
        assert_eq!(
            snapshot.input.drafts[&key].effort.as_deref(),
            same_model.then_some("high")
        );
        assert_eq!(
            snapshot.input.drafts[&key].service_tier.as_deref(),
            same_model.then_some("priority")
        );
    }

    #[rstest::rstest]
    fn reopening_preserves_only_unsubmitted_local_settings(
        #[values(false, true)] accepted: bool,
        #[values(false, true)] edited_during_send: bool,
    ) {
        let id = SessionRef::new(ProviderKind::Codex, "local-settings".into()).unwrap();
        let key = DraftKey::from(id.clone());
        let model = crate::models::ModelRef {
            provider: id.provider,
            id: "model".into(),
        };
        let mut snapshot = Snapshot::default();
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            Some(model.clone()),
            Some("low".into()),
            None,
        );
        let (snapshot, _) = reduce_intent(
            &snapshot,
            Intent::SelectEffort {
                thread_id: key.clone(),
                effort: "high".into(),
            },
        );
        let (snapshot, _) = reduce_intent(
            &snapshot,
            Intent::SetDraftText {
                thread_id: key.clone(),
                text: "Continue".into(),
            },
        );
        let (mut snapshot, _) = reduce_intent(
            &snapshot,
            Intent::Submit {
                thread_id: Some(id.clone()),
                client_user_message_id: "input".into(),
            },
        );
        if edited_during_send {
            snapshot = reduce_intent(
                &snapshot,
                Intent::SelectEffort {
                    thread_id: key.clone(),
                    effort: "max".into(),
                },
            )
            .0;
        }
        if accepted {
            SendSubmission {
                thread_id: id.clone(),
                client_user_message_id: "input".into(),
                draft: snapshot.sessions.pending_submissions["input"].draft.clone(),
            }
            .apply(&mut snapshot, SubmissionProgress::Sent(Some("turn".into())));
        } else {
            snapshot = reduce(&snapshot, Event::SubmissionFailed("input".into())).0;
            assert_eq!(snapshot.input.drafts[&key].text, "Continue");
        }
        // Persistence must retain an unsent choice, including one made while
        // another submission was awaiting its acknowledgement.
        let mut snapshot =
            crate::persistence::decode(&crate::persistence::encode(&snapshot).unwrap()).unwrap();
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(id),
                ..Default::default()
            },
            Some(model),
            Some("low".into()),
            None,
        );
        let expected = if edited_during_send {
            "max"
        } else if accepted {
            "low"
        } else {
            "high"
        };
        assert_eq!(
            snapshot.input.drafts[&key].effort.as_deref(),
            Some(expected)
        );
        assert_eq!(
            snapshot.input.drafts[&key].model_settings_changed,
            edited_during_send || !accepted
        );
    }

    #[rstest::rstest]
    fn reopening_uses_latest_settings_despite_cached_defaults(
        #[values("before", "during", "after")] catalog_at: &str,
        #[values(false, true)] cached: bool,
        #[values(ProviderKind::Codex, ProviderKind::Claude, ProviderKind::DeepSeek)]
        provider: ProviderKind,
    ) {
        let id = SessionRef::new(provider, "reopened".into()).unwrap();
        let key = DraftKey::from(id.clone());
        let models = || {
            serde_json::from_value(serde_json::json!({"data":[
            {"id":"default","model":{"provider":provider,"id":"default"},"isDefault":true,"displayName":"Default"},
            {"id":"saved","model":{"provider":provider,"id":"saved"},"displayName":"Saved","settings":{
                "defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
                "serviceTiers":[{"id":"priority","fast":true}]
            }}
        ]})).unwrap()
        };
        let mut snapshot = Snapshot::default();
        if cached {
            refresh_thread(
                &mut snapshot,
                Thread {
                    id: Some(id.clone()),
                    ..Default::default()
                },
            );
            Arc::make_mut(&mut snapshot.input.drafts).insert(
                key.clone(),
                Arc::new(Draft {
                    text: "Unsent text".into(),
                    model: Some(crate::models::ModelRef {
                        provider,
                        id: "default".into(),
                    }),
                    effort: Some("medium".into()),
                    service_tier: Some("default".into()),
                    ..Default::default()
                }),
            );
        }
        if catalog_at == "before" {
            LoadModels { provider }.apply(&mut snapshot, models());
        }
        let mut open = ReadThread::open(id.clone());
        open.prepare(&mut snapshot).unwrap();
        if catalog_at == "during" {
            LoadModels { provider }.apply(&mut snapshot, models());
        }
        let model = crate::models::ModelRef {
            provider,
            id: "saved".into(),
        };
        open.apply(
            &mut snapshot,
            crate::session::OpenedSession {
                session: id.clone(),
                subscription_id: uuid::Uuid::new_v4(),
                unchanged: false,
                response: crate::models::ThreadResponse {
                    thread: Thread {
                        id: Some(id.clone()),
                        ..Default::default()
                    },
                    model: Some(model.clone()),
                    effort: Some("high".into()),
                    service_tier: Some("priority".into()),
                },
            },
        );
        if catalog_at == "after" {
            LoadModels { provider }.apply(&mut snapshot, models());
        }
        let draft = &snapshot.input.drafts[&key];
        assert_eq!(draft.model.as_ref(), Some(&model));
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("priority"));
        assert_eq!(draft.text, if cached { "Unsent text" } else { "" });
        let controls = snapshot.model_quick_controls(key.clone());
        assert_eq!(controls.effort, "high");
        assert!(controls.fast);
        let (snapshot, _) = reduce_intent(
            &snapshot,
            Intent::SetDraftText {
                thread_id: key,
                text: "Continue".into(),
            },
        );
        let (submitted, effects) = reduce_intent(
            &snapshot,
            Intent::Submit {
                thread_id: Some(id),
                client_user_message_id: "next".into(),
            },
        );
        assert_eq!(effects.len(), 1);
        let next = &submitted.sessions.pending_submissions["next"].draft;
        assert_eq!(next.model.as_ref(), Some(&model));
        assert_eq!(next.effort.as_deref(), Some("high"));
        assert_eq!(next.service_tier.as_deref(), Some("priority"));
    }

    #[rstest::rstest]
    fn reopening_preserves_the_saved_model_on_every_provider(
        #[values(ProviderKind::Codex, ProviderKind::Claude, ProviderKind::DeepSeek)]
        provider: ProviderKind,
    ) {
        let id = SessionRef {
            provider,
            id: "existing".into(),
        };
        let key = DraftKey::from(id.clone());
        let draft = Arc::new(Draft {
            text: "keep input".into(),
            model: Some(crate::models::ModelRef {
                provider,
                id: "local-choice".into(),
            }),
            effort: Some("high".into()),
            model_settings_changed: true,
            ..Default::default()
        });
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), draft.clone());
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            Some(crate::models::ModelRef {
                provider,
                id: "native-choice".into(),
            }),
            None,
            None,
        );
        assert_eq!(snapshot.input.drafts[&key], draft);
        assert_eq!(snapshot.pane.navigation.thread_id.as_ref(), Some(&id));
    }

    #[test]
    fn managed_refresh_retains_loaded_turns_and_recomputes_the_older_cursor() {
        let id = SessionRef {
            provider: ProviderKind::Codex,
            id: "managed".into(),
        };
        let turn = |number: usize| {
            Arc::new(crate::models::Turn {
                id: number.to_string().into(),
                ..Default::default()
            })
        };
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            id.clone(),
            Arc::new(Thread {
                id: Some(id.clone()),
                turns: Some((10..20).map(turn).collect()),
                history_cursor: Some("native-cursor".into()),
                history_has_more: Some(true),
                ..Default::default()
            }),
        );
        refresh_thread(
            &mut snapshot,
            Thread {
                id: Some(id.clone()),
                active_provider: Some(ProviderKind::Claude),
                turns: Some((16..21).map(turn).collect()),
                history_cursor: Some("16".into()),
                history_has_more: Some(true),
                ..Default::default()
            },
        );
        let updated = &snapshot.sessions.conversations[&id];
        assert_eq!(
            updated
                .turns
                .as_ref()
                .unwrap()
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            (10..21).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(updated.history_cursor.as_deref(), Some("10"));
    }

    #[test]
    fn navigating_back_keeps_an_unsent_provider_change() {
        let id = SessionRef {
            provider: ProviderKind::Codex,
            id: "existing".into(),
        };
        let key = DraftKey::from(id.clone());
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT","isDefault":true},
                    {"id":"sonnet","model":{"provider":"claude","id":"sonnet"},"displayName":"Claude","isDefault":true}
                ])).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        let model = snapshot.catalog.models[1].model.clone();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                model: Some(model.clone()),
                text: "continue with Claude".into(),
                ..Default::default()
            }),
        );
        select_thread(&mut snapshot, id.clone(), "/workspace".into());
        assert_eq!(snapshot.input.drafts[&key].model.as_ref(), Some(&model));
        assert_eq!(snapshot.input.drafts[&key].text, "continue with Claude");
        assert_eq!(snapshot.pane.navigation.thread_id.as_ref(), Some(&id));
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
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            lists: crate::state::Lists {
                expanded_projects: Arc::new([("p".into(), 5)].into()),
                project_threads: Arc::new([("p".into(), Arc::new(page))].into()),
                ..Default::default()
            },
            ..Default::default()
        };
        ReadThread::open(id.clone()).prepare(&mut snapshot).unwrap();
        assert_eq!(snapshot.pane.navigation.thread_id.as_ref(), Some(&id));
        assert_eq!(snapshot.pane.navigation.cwd, "/old-project");
        assert_eq!(snapshot.selected_directory(), "/old-project");
        assert_eq!(
            snapshot
                .thread_metadata(&id)
                .unwrap()
                .can_accept_direct_input,
            Some(false)
        );
        let (disconnected, _) = reduce(&snapshot, Event::Disconnected);
        assert_eq!(
            disconnected.thread_metadata(&id).unwrap().status,
            crate::models::SessionStatus::Unknown
        );
        assert_eq!(
            snapshot.thread_metadata(&id).unwrap().status,
            crate::models::SessionStatus::Running
        );
    }

    #[rstest::rstest]
    #[case::openrouter("openrouter")]
    #[case::groq("groq")]
    fn reopening_old_official_session_preserves_selected_gateway_for_next_submission(
        #[case] route: &str,
    ) {
        let official = crate::models::ModelRef {
            provider: ProviderKind::DeepSeek,
            id: r#"["deepseek-official","deepseek-flash"]"#.into(),
        };
        let selected = crate::models::ModelRef {
            provider: ProviderKind::DeepSeek,
            id: serde_json::to_string(&[route, "test/model"]).unwrap(),
        };
        let session = SessionRef::new(ProviderKind::DeepSeek, "selected-gateway".into()).unwrap();
        let key: DraftKey = session.clone().into();
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            catalog: crate::state::Catalog {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                {"id":"official","model":official,"displayName":"Official","isDefault":true, "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
                {"id":"gateway","model":selected,"displayName":"Gateway", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
            ])).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "Keep my unfinished request".into(),
                model: Some(selected.clone()),
                model_settings_changed: true,
                ..Default::default()
            }),
        );
        ReadThread::open(session.clone())
            .prepare(&mut snapshot)
            .unwrap();
        open_thread(
            &mut snapshot,
            Thread {
                id: Some(session.clone()),
                ..Default::default()
            },
            Some(official),
            None,
            None,
        );
        assert_eq!(snapshot.input.drafts[&key].model, Some(selected.clone()));
        assert_eq!(
            snapshot.input.drafts[&key].text,
            "Keep my unfinished request"
        );
        let (submitted, effects) = reduce_intent(
            &snapshot,
            Intent::Submit {
                thread_id: Some(session),
                client_user_message_id: "gateway-input".into(),
            },
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(
            submitted.sessions.pending_submissions["gateway-input"]
                .draft
                .model,
            Some(selected)
        );
    }

    #[rstest::rstest]
    #[case::new_draft(false, false)]
    #[case::existing_empty_draft(true, false)]
    #[case::saved_settings(true, true)]
    fn opening_restores_model_metadata_or_uses_the_provider_catalog(
        #[values("before", "during", "after")] catalog_at: &str,
        #[values(true, false)] model_metadata: bool,
        #[values(ProviderKind::Codex, ProviderKind::Claude)] provider: ProviderKind,
        #[case] existing_draft: bool,
        #[case] saved_choice: bool,
    ) {
        let models: Vec<Model> = serde_json::from_value(serde_json::json!([
            {"id":"codex-default","model":{"provider":"codex","id":"default"},"displayName":"Codex default",
             "isDefault":true, "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"}]}},
            {"id":"claude-default","model":{"provider":"claude","id":"default"},"displayName":"Claude default",
             "isDefault":true, "settings": {"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}},
            {"id":"codex-saved","model":{"provider":"codex","id":"saved"},"displayName":"Codex saved", "settings": {
             "defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}]}},
            {"id":"claude-saved","model":{"provider":"claude","id":"saved"},"displayName":"Claude saved", "settings": {
             "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}}
        ])).unwrap();
        let session = SessionRef::new(provider, "external".into()).unwrap();
        let key: DraftKey = session.clone().into();
        let mut snapshot = Snapshot::default();
        let catalog = || agent_protocol::operations::ModelPage {
            data: models.clone(),
            next_cursor: None,
            provider_errors: None,
        };
        if catalog_at == "before" {
            LoadModels { provider }.apply(&mut snapshot, catalog());
        }
        if existing_draft {
            Arc::make_mut(&mut snapshot.input.drafts).insert(
                key.clone(),
                Arc::new(Draft {
                    text: "Unsent input".into(),
                    model: saved_choice.then(|| crate::models::ModelRef {
                        provider,
                        id: "saved".into(),
                    }),
                    effort: saved_choice.then(|| "low".into()),
                    model_settings_changed: saved_choice,
                    ..Default::default()
                }),
            );
        }
        ReadThread::open(session.clone())
            .prepare(&mut snapshot)
            .unwrap();
        if catalog_at == "during" {
            LoadModels { provider }.apply(&mut snapshot, catalog());
        }
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
            None,
            None,
        );
        if catalog_at == "after" {
            LoadModels { provider }.apply(&mut snapshot, catalog());
        }
        let draft = &snapshot.input.drafts[&key];
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
            submitted.sessions.pending_submissions["next-input"]
                .draft
                .model,
            expected_model
        );
    }
}

#[cfg(test)]
mod item_read_tests {
    use crate::models::{ItemBody, Turn};

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

    #[test]
    fn older_items_hydrate_without_changing_a_later_turn_with_the_same_id() {
        let (mut snapshot, request) = fixture();
        let latest = Arc::new(Turn {
            id: request.turn_id.clone(),
            status: crate::models::TurnStatus::Completed,
            items: Some(vec![Arc::new(Item::new(
                "latest".into(),
                Default::default(),
                ItemBody::Sleep {},
            ))]),
            ..Default::default()
        });
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&request.thread_id)
                .unwrap(),
        )
        .turns
        .as_mut()
        .unwrap()
        .push(latest.clone());
        let captured = request.capture(&snapshot).unwrap();
        assert!(
            captured.0.is_some(),
            "the requested older item must be captured"
        );
        let response = read(&snapshot, &request);
        assert!(
            request
                .clone()
                .complete(&mut snapshot, response, true)
                .unwrap()
                .is_empty()
        );
        let turns = snapshot.sessions.conversations[&request.thread_id]
            .turns
            .as_ref()
            .unwrap();
        assert!(!turns[0].items.as_ref().unwrap()[0].is_deferred());
        assert_eq!(
            item_text(&turns[0].items.as_ref().unwrap()[0]),
            Some("old full body")
        );
        assert!(Arc::ptr_eq(&turns[1], &latest));
    }

    fn fixture() -> (Snapshot, ReadItem) {
        let thread = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"chat"},"turns":[{"id":"turn","status":"running","items":[{"id":"item","status":"unknown","clientInputId":null,"body":{"deferred":{"summary":{"assistantText":{"text":"summary","phase":"unknown"}}}}}]}]})).unwrap();
        let snapshot = Snapshot {
            sessions: crate::state::Sessions {
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
            },
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
        ItemRead { source: item_source(request, snapshot).cloned(), subscription: snapshot.sessions.subscriptions.get(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "chat".into() }).copied(), response: Ok(rpc::ItemResponse {item: serde_json::from_value(serde_json::json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old full body","phase":"unknown"}}}}})).unwrap(), transfer:None,}) }
    }
    fn update(snapshot: &mut Snapshot, change: SessionChange) {
        let session = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "chat".into(),
        };
        let subscription_id = snapshot.sessions.subscriptions[&session];
        let (next, effects) = crate::state::reduce(
            snapshot,
            crate::state::Event::SessionUpdate(Box::new(crate::session::SessionUpdate {
                subscription_id,
                change,
            })),
        );
        if next.pane.error.is_some() {
            assert_eq!(effects.len(), 1);
            assert!(!next.sessions.subscriptions.contains_key(&session));
        }
        *snapshot = next;
    }
    #[test]
    fn full_item_update_wins_over_late_body_and_its_status() {
        let (mut snapshot, request) = fixture();
        let old = read(&snapshot, &request);
        update(&mut snapshot, SessionChange::Item {turn_id:"turn".into(),item: serde_json::from_value(serde_json::json!({"id":"item","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new full body","phase":"unknown"}}}}})).unwrap()});
        let current = snapshot.sessions.conversations.clone();
        assert!(
            request
                .complete(&mut snapshot, old, true)
                .unwrap()
                .is_empty()
        );
        assert!(Arc::ptr_eq(&current, &snapshot.sessions.conversations));
    }
    #[test]
    fn delta_and_reopen_invalidate_body_and_coalesce_into_one_retry() {
        for reopen in [false, true] {
            let (mut snapshot, request) = fixture();
            let old = read(&snapshot, &request);
            if reopen {
                // Even a read failure retaining cached Item Arcs changes the subscription.
                Arc::make_mut(&mut snapshot.sessions.subscriptions).insert(
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
            assert!(snapshot.pane.error.is_none());
            assert!(
                snapshot
                    .sessions
                    .subscriptions
                    .contains_key(&request.thread_id)
            );
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
