//! One conversation: opening, history and item reads, creation, forks and interrupts.
use super::*;
use crate::models::Item;

pub use agent_protocol::operations::ReadItem;

#[derive(Debug, Clone)]
pub struct ReadHistory {
    pub session: crate::session::SessionRef,
}

impl Operation for ReadHistory {
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.session.clone(),
        })
    }
    type Input = (uuid::Uuid, String);
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        let scope = super::super::conversations::read_scope(snapshot, &self.session)?;
        let cursor = snapshot
            .sessions
            .projections
            .get(&self.session)
            .and_then(|snapshot| snapshot.history_cursor.clone())
            .ok_or_else(|| PeerError::InvalidMessage("no earlier history is available".into()))?;
        Ok((scope, cursor))
    }
    type Output = (
        Self::Input,
        agent_protocol::orchestration::projection::ThreadHistoryPage,
    );
    async fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let page = context
            .call(&agent_protocol::conversation::records::Extend {
                session: self.session.clone(),
                cursor: input.1.clone(),
            })
            .await?;
        Ok((input, page))
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        ((subscription, cursor), page): Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        if !current || snapshot.sessions.subscriptions.get(&self.session) != Some(&subscription) {
            return Ok(Vec::new());
        }
        let next = snapshot
            .sessions
            .projections
            .get(&self.session)
            .and_then(|snapshot| crate::orchestration::receive_history(snapshot, &cursor, page));
        if let Some(next) = next {
            Arc::make_mut(&mut snapshot.sessions.expanded_history).insert(self.session.clone());
            Arc::make_mut(&mut snapshot.sessions.projections)
                .insert(self.session.clone(), Arc::new(next));
            return super::super::conversations::refresh(snapshot, &self.session);
        }
        Ok(Vec::new())
    }
}

pub(crate) fn item_detail(
    session: &crate::session::SessionRef,
    turn: &agent_protocol::ids::TurnId,
    item: &Item,
) -> Option<Effect> {
    (item.is_deferred()
        && matches!(
            item.body(),
            crate::models::ItemBody::UserMessage { .. }
                | crate::models::ItemBody::AssistantText { .. }
                | crate::models::ItemBody::ImageGeneration { .. }
                | crate::models::ItemBody::ImageView { .. }
        ))
    .then(|| {
        Effect::execute(ReadItem {
            thread_id: session.clone(),
            turn_id: turn.clone(),
            item_id: item.id.clone(),
        })
    })
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
                .filter_map(|item| item_detail(session, &turn.id, item))
        })
        .collect()
}

pub(crate) fn item_source<'a>(request: &ReadItem, snapshot: &'a Snapshot) -> Option<&'a Arc<Item>> {
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
    type Input = (uuid::Uuid, String);
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        if item_source(self, snapshot).is_none() {
            return Err(PeerError::InvalidMessage(
                "item does not belong to the requested turn".into(),
            ));
        }
        let source = snapshot
            .sessions
            .projections
            .get(&self.thread_id)
            .and_then(|snapshot| {
                snapshot
                    .projection
                    .visible_turn_items
                    .iter()
                    .find(|row| row.source_item_id == self.item_id.as_str())
            })
            .map(|row| row.source_thread_id.clone())
            .ok_or_else(|| PeerError::InvalidMessage("item source is missing".into()))?;
        Ok((
            super::super::conversations::read_scope(snapshot, &self.thread_id)?,
            source,
        ))
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Item(self.clone())
    }
    type Output = (uuid::Uuid, Option<agent_protocol::orchestration::TurnItem>);
    async fn run(
        &self,
        (subscription, source): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context
            .call(&agent_protocol::conversation::records::Hydrate {
                session: crate::session::SessionRef::new(None, source.clone())
                    .map_err(|error| PeerError::InvalidMessage(error.into()))?,
                item: self.item_id.clone(),
            })
            .await?;
        Ok((
            subscription,
            crate::client::resolve_hydration(response, &source, &self.item_id, context.session)
                .await?,
        ))
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        (subscription, body): Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        if !current || snapshot.sessions.subscriptions.get(&self.thread_id) != Some(&subscription) {
            return Ok(Vec::new());
        }
        if let Some(item) = body {
            let key = (item.thread_id.clone(), item.id.clone());
            let source =
                snapshot
                    .sessions
                    .projections
                    .get(&self.thread_id)
                    .and_then(|snapshot| {
                        snapshot.projection.visible_turn_items.iter().find(|row| {
                            row.source_thread_id == key.0 && row.source_item_id == key.1
                        })
                    });
            if source
                .is_some_and(|row| crate::orchestration::same_detail_revision(&row.item, &item))
            {
                Arc::make_mut(&mut snapshot.sessions.item_details).insert(key, Arc::new(item));
                return super::super::conversations::refresh(snapshot, &self.thread_id);
            }
            if source.is_some()
                && item_source(&self, snapshot).is_some_and(|item| item.is_deferred())
            {
                return Ok(vec![Effect::continuation(self)]);
            }
        }
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
}
impl ReadThread {
    pub fn new(thread_id: crate::session::SessionRef) -> Self {
        Self {
            thread_id,
            open: false,
        }
    }
    pub fn open(thread_id: crate::session::SessionRef) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
impl Operation for ReadThread {
    fn subscription_scope(
        &self,
    ) -> Option<agent_protocol::sync::Scope<crate::session::SessionRef>> {
        Some(agent_protocol::sync::Scope::Conversation(
            self.thread_id.clone(),
        ))
    }
    fn navigation_bound(&self) -> bool {
        self.open
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.thread_id.clone(),
        })
    }
    type Input = super::super::conversations::Capture;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        super::super::conversations::capture(snapshot, &self.thread_id)
    }

    type Output = super::super::conversations::Opened;
    async fn run(
        &self,
        replica: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context
            .call(&agent_protocol::conversation::records::Subscribe {
                session: self.thread_id.clone(),
                subscription: uuid::Uuid::nil(),
                after_sequence: replica
                    .projection
                    .as_ref()
                    .map(|snapshot| snapshot.snapshot_sequence),
            })
            .await?;
        super::super::conversations::opened(&replica, response)
    }
    fn invalidates(&self) -> bool {
        self.open
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, pane_id: &str) -> Result<(), String> {
        if self.open {
            let cwd = snapshot
                .thread_metadata(&self.thread_id)
                .and_then(|thread| thread.cwd.clone())
                .unwrap_or_default();
            select_thread(snapshot, pane_id, self.thread_id.clone(), cwd);
        }
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let Some(response) = super::super::conversations::install(snapshot, output) else {
            return Vec::new();
        };
        let mut effects = item_details(
            &self.thread_id,
            response.thread.turns.as_deref().unwrap_or_default(),
        );
        effects.extend(if self.open {
            open_thread(
                snapshot,
                pane_id,
                self.thread_id.clone(),
                response.thread.cwd.unwrap_or_default(),
                response.model,
                response.effort,
                response.service_tier,
            )
        } else {
            Vec::new()
        });
        snapshot.prune_subscriptions();
        effects.extend(super::super::conversations::receipt_check(
            snapshot,
            &self.thread_id,
        ));
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if super::super::conversations::install(snapshot, output).is_some() {
            snapshot.prune_subscriptions();
            super::super::conversations::receipt_check(snapshot, &self.thread_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    }
}

fn select_thread(
    snapshot: &mut Snapshot,
    pane_id: &str,
    id: crate::session::SessionRef,
    cwd: String,
) {
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
        pane_id,
        Navigation {
            thread_id: Some(id.clone()),
            draft_key: id.into(),
            cwd,
        },
    );
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    pane_id: &str,
    id: crate::session::SessionRef,
    cwd: String,
    model: Option<crate::models::ModelRef>,
    effort: Option<String>,
    service_tier: Option<String>,
) -> Vec<Effect> {
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
        snapshot
            .thread_metadata(&id)
            .and_then(|thread| thread.active_provider)
            .or(id.provider),
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
        pane_id,
        Navigation {
            thread_id: Some(id.clone()),
            draft_key: id.clone().into(),
            cwd,
        },
    );
    review_workspace(snapshot, pane_id).into_iter().collect()
}

async fn command(
    context: &mut Execution<'_>,
    session: &crate::session::SessionRef,
    command: agent_protocol::conversation::Command,
) -> Result<u64, PeerError> {
    crate::client::command_result(
        context
            .call(&agent_protocol::conversation::records::Change {
                session: session.clone(),
                operation: uuid::Uuid::new_v4(),
                command,
            })
            .await?,
    )
}

pub use agent_protocol::operations::ForkSession;

impl Operation for ForkSession {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Fork {
            session: self.thread_id.clone(),
            turn: self.last_turn_id.clone(),
        })
    }
    type Input = (
        crate::session::SessionRef,
        (
            crate::session::SessionRef,
            agent_protocol::conversation::records::ForkPoint,
        ),
    );
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        let through = snapshot
            .sessions
            .projections
            .get(&self.thread_id)
            .and_then(|snapshot| {
                crate::orchestration::fork_point(&snapshot.projection, &self.last_turn_id)
            })
            .ok_or_else(|| PeerError::InvalidMessage("fork history is unavailable".into()))?;
        Ok((
            crate::session::SessionRef {
                provider: None,
                id: uuid::Uuid::new_v4().to_string(),
            },
            through,
        ))
    }
    type Output = crate::session::SessionRef;
    async fn run(
        &self,
        (target, (source, through)): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let returned = context
            .call(&agent_protocol::conversation::records::Fork {
                source,
                target: target.clone(),
                through,
            })
            .await?;
        if returned != target {
            return Err(PeerError::InvalidMessage(
                "fork conversation identity changed".into(),
            ));
        }
        Ok(target)
    }
    fn invalidates(&self) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, _: &str, id: Self::Output) -> Vec<Effect> {
        if let Some(draft) = snapshot
            .input
            .drafts
            .get(&DraftKey::from(self.thread_id))
            .cloned()
        {
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(id.clone().into())
                .or_insert(draft);
        }
        vec![Effect::execute(ReadThread::open(id))]
    }
    fn stale(self, _snapshot: &mut Snapshot, _pane: &str, _id: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn outcome(id: &mut Self::Output) -> Outcome {
        Outcome::StartedThread { id: id.clone() }
    }
}

pub use agent_protocol::operations::RewindSession;

/// The canonical input retains attachment names that image display parts omit.
fn sent_input(
    projection: &agent_protocol::orchestration::projection::ThreadProjection,
    turn: &agent_protocol::ids::TurnId,
) -> Result<Option<(String, Vec<Attachment>)>, String> {
    use agent_protocol::{items::MessagePart, orchestration::TurnItemBody};
    let Some((text, images, context)) = projection
        .turn_items
        .iter()
        .map(AsRef::as_ref)
        .chain(
            projection
                .visible_turn_items
                .iter()
                .map(|row| row.item.as_ref()),
        )
        .filter(|item| {
            item.thread_id == projection.thread.id && item.run_id.as_deref() == Some(turn.as_str())
        })
        .find_map(|item| match &item.body {
            TurnItemBody::UserMessage {
                text,
                attachments,
                context,
                ..
            } => Some((text, attachments, context)),
            _ => None,
        })
    else {
        return Ok(None);
    };
    let mut attachments = crate::orchestration::attachments(images)?;
    attachments.extend(
        crate::orchestration::context_parts(context.as_ref())?
            .into_iter()
            .filter_map(|part| match part {
                MessagePart::Attachment { name, path } => Some(Attachment {
                    name,
                    path,
                    is_image: false,
                }),
                _ => None,
            }),
    );
    Ok(Some((text.clone(), attachments)))
}

#[derive(Debug)]
pub struct RewindCapture {
    scope: String,
    epoch: u64,
    draft_text: String,
    draft_attachments: Vec<Attachment>,
    original: Option<(String, Vec<Attachment>)>,
}

impl Operation for RewindSession {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Rewind {
            session: self.thread_id.clone(),
        })
    }
    type Input = RewindCapture;
    type Output = (Self::Input, u64);
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        let original = snapshot
            .sessions
            .projections
            .get(&self.thread_id)
            .map(|snapshot| sent_input(&snapshot.projection, &self.turn_id))
            .transpose()
            .map_err(PeerError::InvalidMessage)?
            .flatten();
        let draft = snapshot.input.drafts.get(&DraftKey::from(&self.thread_id));
        Ok(RewindCapture {
            scope: snapshot.connection.storage_scope.clone(),
            epoch: snapshot.connection.epoch,
            draft_text: draft.map_or_else(String::new, |draft| draft.text.clone()),
            draft_attachments: draft.map_or_else(Vec::new, |draft| draft.attachments.clone()),
            original,
        })
    }
    async fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let cursor = crate::client::rewind(context.client, &self.thread_id, &self.turn_id).await?;
        Ok((input, cursor))
    }
    fn invalidates(&self) -> bool {
        true
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane_id: &str,
        (captured, _cursor): Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        if captured.scope != snapshot.connection.storage_scope
            || captured.epoch != snapshot.connection.epoch
        {
            return Ok(Vec::new());
        }
        let id = self.thread_id;
        let draft = snapshot.input.drafts.get(&DraftKey::from(&id));
        let unchanged = draft.map_or(
            captured.draft_text.is_empty() && captured.draft_attachments.is_empty(),
            |draft| {
                draft.text == captured.draft_text && draft.attachments == captured.draft_attachments
            },
        );
        if unchanged && let Some((text, attachments)) = captured.original {
            let drafts = Arc::make_mut(&mut snapshot.input.drafts);
            let draft = Arc::make_mut(drafts.entry(id.clone().into()).or_default());
            draft.text = text;
            for attachment in attachments {
                if !draft.attachments.contains(&attachment) {
                    draft.attachments.push(attachment);
                }
            }
        }
        let effects = if current && !snapshot.sessions.subscriptions.contains_key(&id) {
            vec![Effect::execute(ReadThread::open(id))]
        } else {
            Vec::new()
        };
        Ok(effects)
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSession {
    pub provider: crate::session::ProviderKind,
    pub cwd: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StartConversation {
    pub target: crate::session::SessionRef,
    pub request: CreateSession,
}
pub use agent_protocol::operations::RetryPreparation;

impl Operation for RetryPreparation {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.thread_id.clone().into(),
        })
    }
    no_input!();
    type Output = u64;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        command(
            context,
            &self.thread_id,
            agent_protocol::conversation::Command::RetryInput {
                id: self.input_id.clone(),
                work_locally: self.work_locally,
            },
        )
        .await
    }
}

impl Operation for StartConversation {
    const REPORT_ERROR: bool = false;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::History {
            session: self.target.clone(),
        })
    }
    fn subscription_scope(
        &self,
    ) -> Option<agent_protocol::sync::Scope<crate::session::SessionRef>> {
        Some(agent_protocol::sync::Scope::Conversation(
            self.target.clone(),
        ))
    }
    type Input = super::super::conversations::Capture;
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        super::super::conversations::capture(snapshot, &self.target)
    }
    type Output = super::super::conversations::Opened;
    async fn run(
        &self,
        replica: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context
            .call(&agent_protocol::conversation::records::Create {
                workspace: None,
                provider: self.request.provider,
                session: self.target.clone(),
                subscription: uuid::Uuid::nil(),
                cwd: self.request.cwd.clone(),
                selection: Default::default(),
                initial: None,
            })
            .await?;
        super::super::conversations::opened(&replica, response)
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let Some(response) = super::super::conversations::install(snapshot, output) else {
            return Vec::new();
        };
        let effects = open_thread(
            snapshot,
            pane_id,
            self.target.clone(),
            response.thread.cwd.unwrap_or_default(),
            response.model,
            response.effort,
            response.service_tier,
        );
        snapshot.prune_subscriptions();
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _pane: &str, output: Self::Output) -> Vec<Effect> {
        super::super::conversations::install(snapshot, output);
        snapshot.prune_subscriptions();
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
    no_input!();
    type Output = u64;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        command(
            context,
            &self.thread_id,
            agent_protocol::conversation::Command::Interrupt {
                turn: Some(self.turn_id.clone()),
            },
        )
        .await
    }
}

pub use agent_protocol::operations::ChangeQueue;

impl Operation for ChangeQueue {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Queue {
            session: self.thread_id.clone(),
        })
    }
    no_input!();
    type Output = u64;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        use agent_protocol::{conversation::Command, operations::QueueChange};
        let change = match &self.change {
            QueueChange::Edit {
                client_user_message_id,
                input,
            } => Command::EditInput {
                id: client_user_message_id.clone(),
                parts: input.clone(),
            },
            QueueChange::Remove {
                client_user_message_id,
            } => Command::RemoveInput(client_user_message_id.clone()),
            QueueChange::Move {
                client_user_message_id,
                before,
            } => Command::MoveInput {
                id: client_user_message_id.clone(),
                before: before.clone(),
            },
            QueueChange::Steer {
                client_user_message_id,
            } => Command::SteerInput(client_user_message_id.clone()),
            QueueChange::Resume => Command::ResumeQueue,
        };
        command(context, &self.thread_id, change).await
    }
}

pub use agent_protocol::operations::OpenRequest;

impl Operation for OpenRequest {
    no_input!();
    type Output = crate::session::SessionRef;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context.client.call(self).await
    }
    fn apply(self, _snapshot: &mut Snapshot, _pane_id: &str, id: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ReadThread::new(id))]
    }
}

#[cfg(test)]
mod tests;
