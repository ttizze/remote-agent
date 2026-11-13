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
    type Input = super::super::conversations::ReadScope;
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        super::super::conversations::read_scope(snapshot, &self.session)
    }
    type Output = super::super::conversations::ReadRows;
    async fn run(
        &self,
        scope: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let result = context
            .call(&agent_protocol::conversation::records::Extend {
                session: self.session.clone(),
                subscription: scope.subscription,
                cached: scope.cursor,
            })
            .await;
        Ok(super::super::conversations::ReadRows { scope, result })
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        output: Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        use super::super::conversations::ReadResult;
        match super::super::conversations::finish_read(snapshot, &self.session, output, current)? {
            ReadResult::Applied(effects) => Ok(effects),
            ReadResult::Retry
                if snapshot
                    .sessions
                    .replicas
                    .get(&self.session)
                    .and_then(|replica| replica.window())
                    .is_some_and(|window| window.turn != 0 || window.item != 0) =>
            {
                Ok(vec![Effect::continuation(self)])
            }
            _ => Ok(Vec::new()),
        }
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
    type Input = super::super::conversations::ReadScope;
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        if item_source(self, snapshot).is_none() {
            return Err(PeerError::InvalidMessage(
                "item does not belong to the requested turn".into(),
            ));
        }
        super::super::conversations::read_scope(snapshot, &self.thread_id)
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Item(self.clone())
    }
    type Output = super::super::conversations::ReadRows;
    async fn run(
        &self,
        scope: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let result = context
            .call(&agent_protocol::conversation::records::Hydrate {
                session: self.thread_id.clone(),
                subscription: scope.subscription,
                cached: scope.cursor,
                item: self.item_id.clone(),
            })
            .await;
        let result = match result {
            Ok(response) => {
                crate::client::resolve_hydration(response, &self.item_id, context.session).await
            }
            Err(error) => Err(error),
        };
        Ok(super::super::conversations::ReadRows { scope, result })
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        output: Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        use super::super::conversations::ReadResult;
        match super::super::conversations::finish_read(snapshot, &self.thread_id, output, current)?
        {
            ReadResult::Applied(effects) => Ok(effects),
            ReadResult::Retry
                if item_source(&self, snapshot).is_some_and(|item| item.is_deferred()) =>
            {
                Ok(vec![Effect::continuation(self)])
            }
            _ => Ok(Vec::new()),
        }
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
/// Turns an opened conversation shows first, and each older page adds.
pub const HISTORY_WINDOW_TURNS: u32 = 10;
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
    type Input = Arc<agent_protocol::conversation::records::Replica>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .sessions
            .replicas
            .get(&self.thread_id)
            .cloned()
            .ok_or_else(|| {
                PeerError::InvalidMessage("conversation subscription was not scheduled".into())
            })
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
                cached: replica.cursor(),
                window: replica.window(),
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
        effects.extend(super::super::conversations::receipt_check(
            snapshot,
            &self.thread_id,
        ));
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if super::super::conversations::install(snapshot, output).is_some() {
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
) -> Result<agent_protocol::sync::Cursor, PeerError> {
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
    type Input = crate::session::SessionRef;
    fn capture(&self, _: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        Ok(crate::session::SessionRef {
            provider: None,
            id: uuid::Uuid::new_v4().to_string(),
        })
    }
    type Output = crate::session::SessionRef;
    async fn run(
        &self,
        target: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let returned = context
            .call(&agent_protocol::conversation::records::Fork {
                source: self.thread_id.clone(),
                target: target.clone(),
                through: self.last_turn_id.clone(),
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

/// What the user sent in `turn`: its text and the files that can be attached again.
fn sent_input(
    turns: &[Arc<crate::models::Turn>],
    turn: &agent_protocol::ids::TurnId,
) -> Option<(String, Vec<Attachment>)> {
    use agent_protocol::items::{ItemBody, MessagePart};
    let turn = turns.iter().find(|candidate| candidate.id == *turn)?;
    let (text, content) = turn
        .items
        .iter()
        .flatten()
        .find_map(|item| match item.body() {
            ItemBody::UserMessage { text, content } => Some((text, content)),
            _ => None,
        })?;
    let mut texts = Vec::new();
    let mut attachments = Vec::new();
    for part in content {
        match part {
            MessagePart::Text { text } => texts.push(text.as_str()),
            MessagePart::Image { source }
                if agent_protocol::paths::is_absolute_host_path(source) =>
            {
                attachments.push(Attachment {
                    name: file_name(source),
                    path: source.clone(),
                    is_image: true,
                });
            }
            MessagePart::Attachment { name, path } => attachments.push(Attachment {
                name: name.clone(),
                path: path.clone(),
                is_image: false,
            }),
            MessagePart::Image { .. } | MessagePart::Invocation { .. } => {}
        }
    }
    Some((
        text.clone().unwrap_or_else(|| texts.join("\n")),
        attachments,
    ))
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

impl Operation for RewindSession {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Rewind {
            session: self.thread_id.clone(),
        })
    }
    type Input = (String, Option<(String, Vec<Attachment>)>);
    type Output = (Self::Input, agent_protocol::sync::Cursor);
    fn capture(&self, snapshot: &Snapshot, _: &str) -> Result<Self::Input, PeerError> {
        let turns = snapshot
            .sessions
            .conversations
            .get(&self.thread_id)
            .and_then(|thread| thread.turns.as_deref())
            .unwrap_or_default();
        Ok((
            snapshot.connection.storage_scope.clone(),
            sent_input(turns, &self.turn_id),
        ))
    }
    async fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        Ok((
            input,
            command(
                context,
                &self.thread_id,
                agent_protocol::conversation::Command::Rewind(self.turn_id.clone()),
            )
            .await?,
        ))
    }
    fn invalidates(&self) -> bool {
        true
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane_id: &str,
        ((scope, original), _cursor): Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        if scope != snapshot.connection.storage_scope {
            return Ok(Vec::new());
        }
        let id = self.thread_id;
        if let Some((text, attachments)) = original {
            let drafts = Arc::make_mut(&mut snapshot.input.drafts);
            let draft = Arc::make_mut(drafts.entry(id.clone().into()).or_default());
            draft.text = text;
            for attachment in attachments {
                if !draft.attachments.contains(&attachment) {
                    draft.attachments.push(attachment);
                }
            }
        }
        let effects = if !snapshot.sessions.subscriptions.contains_key(&id) {
            vec![Effect::execute(if current {
                ReadThread::open(id)
            } else {
                ReadThread::new(id)
            })]
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
    type Output = agent_protocol::sync::Cursor;
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
    type Input = Arc<agent_protocol::conversation::records::Replica>;
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .sessions
            .replicas
            .get(&self.target)
            .cloned()
            .ok_or_else(|| {
                PeerError::InvalidMessage("conversation creation was not scheduled".into())
            })
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
        open_thread(
            snapshot,
            pane_id,
            self.target.clone(),
            response.thread.cwd.unwrap_or_default(),
            response.model,
            response.effort,
            response.service_tier,
        )
    }
    fn stale(self, snapshot: &mut Snapshot, _pane: &str, output: Self::Output) -> Vec<Effect> {
        super::super::conversations::install(snapshot, output);
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: match output.replica.scope() {
                agent_protocol::sync::Scope::Conversation(id) => id.clone(),
                _ => unreachable!("conversation operation"),
            },
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
    type Output = agent_protocol::sync::Cursor;
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
    type Output = agent_protocol::sync::Cursor;
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
        vec![Effect::execute(ReadThread::open(id))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};

    fn open_thread(
        snapshot: &mut Snapshot,
        pane: &str,
        thread: Thread,
        model: Option<crate::models::ModelRef>,
        effort: Option<String>,
        service_tier: Option<String>,
    ) -> Vec<Effect> {
        let id = thread.id.clone().unwrap();
        let output = crate::test_conversations::opened(
            crate::models::ThreadResponse {
                thread,
                model,
                effort,
                service_tier,
            },
            snapshot,
        );
        ReadThread::open(id).apply(snapshot, pane, output)
    }

    fn refresh_thread(snapshot: &mut Snapshot, pane: &str, thread: Thread) {
        let id = thread.id.clone().unwrap();
        let output = crate::test_conversations::opened(
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
            snapshot,
        );
        ReadThread::new(id).apply(snapshot, pane, output);
    }

    #[test]
    fn item_reads_require_the_items_actual_turn() {
        let id = SessionRef::new(ProviderKind::Codex, "details".into()).unwrap();
        let mut snapshot = Snapshot::default();
        crate::test_conversations::open(
            &mut snapshot,
            crate::models::ThreadResponse {
                thread: Thread {
                    id: Some(id.clone()),
                    turns: Some(vec![Arc::new(crate::models::Turn {
                        id: "correct".into(),
                        items: Some(vec![Arc::new(Item::new(
                            "item".into(),
                            crate::models::ItemStatus::Completed,
                            crate::models::ItemBody::ImageView {
                                path: "image.png".into(),
                            },
                        ))]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let request = ReadItem {
            thread_id: id,
            turn_id: "wrong".into(),
            item_id: "item".into(),
        };
        assert!(
            matches!(request.capture(&snapshot,""),Err(PeerError::InvalidMessage(message)) if message=="item does not belong to the requested turn")
        );
        assert!(
            ReadItem {
                turn_id: "correct".into(),
                ..request
            }
            .capture(&snapshot, "")
            .is_ok()
        );
    }

    #[rstest::rstest]
    fn missing_native_options_keep_saved_options_only_for_the_same_model(
        #[values(None, Some("saved"), Some("changed"))] native_model: Option<&str>,
    ) {
        let id = SessionRef::new(ProviderKind::Codex, "missing-options".into()).unwrap();
        let key = DraftKey::from(id.clone());
        let model = |name: &str| crate::models::ModelRef {
            provider: id.provider.unwrap(),
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
            "",
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
            provider: id.provider.unwrap(),
            id: "model".into(),
        };
        let mut snapshot = Snapshot::default();
        open_thread(
            &mut snapshot,
            "",
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
            "",
            Intent::SelectEffort {
                thread_id: key.clone(),
                effort: "high".into(),
            },
        );
        let (snapshot, _) = reduce_intent(
            &snapshot,
            "",
            Intent::SetDraftText {
                thread_id: key.clone(),
                text: "Continue".into(),
            },
        );
        let (mut snapshot, _) = reduce_intent(
            &snapshot,
            "",
            Intent::Submit {
                thread_id: Some(id.clone()),
                client_user_message_id: "input".into(),
                alternate: false,
            },
        );
        if edited_during_send {
            snapshot = reduce_intent(
                &snapshot,
                "",
                Intent::SelectEffort {
                    thread_id: key.clone(),
                    effort: "max".into(),
                },
            )
            .0;
        }
        if accepted {
            let pending = snapshot.sessions.pending_submissions["input"].clone();
            SendSubmission {
                thread_id: id.clone(),
                client_user_message_id: "input".into(),
                draft: snapshot.sessions.pending_submissions["input"].draft.clone(),
                follow_up: Default::default(),
            }
            .apply(
                &mut snapshot,
                "",
                agent_protocol::sync::Cursor {
                    store: uuid::Uuid::from_u128(1),
                    sequence: 7,
                },
            );
            let mut response = crate::models::ThreadResponse {
                thread: Thread {
                    id: Some(id.clone()),
                    ..Default::default()
                },
                ..Default::default()
            };
            response
                .thread
                .queue
                .inputs
                .push(agent_protocol::operations::Submission {
                    thread_id: id.clone(),
                    client_user_message_id: "input".into(),
                    input: vec![],
                    model: pending.draft.model.clone(),
                    effort: pending.draft.effort.clone(),
                    service_tier: pending.draft.service_tier.clone(),
                    follow_up: Default::default(),
                });
            let opened = crate::test_conversations::opened(response, &mut snapshot);
            super::super::conversations::install(&mut snapshot, opened);
        } else {
            snapshot = reduce(&snapshot, "", Event::SubmissionFailed("input".into())).0;
            assert_eq!(snapshot.input.drafts[&key].text, "Continue");
        }
        // Persistence must retain an unsent choice, including one made while
        // another submission was awaiting its acknowledgement.
        let mut snapshot =
            crate::persistence::decode(&crate::persistence::encode(&snapshot).unwrap()).unwrap();
        open_thread(
            &mut snapshot,
            "",
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
                "",
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
            LoadModels { provider }.apply(&mut snapshot, "", models());
        }
        let mut open = ReadThread::open(id.clone());
        open.prepare(&mut snapshot, "").unwrap();
        if catalog_at == "during" {
            LoadModels { provider }.apply(&mut snapshot, "", models());
        }
        let model = crate::models::ModelRef {
            provider,
            id: "saved".into(),
        };
        crate::test_conversations::open(
            &mut snapshot,
            crate::models::ThreadResponse {
                thread: Thread {
                    id: Some(id.clone()),
                    ..Default::default()
                },
                model: Some(model.clone()),
                effort: Some("high".into()),
                service_tier: Some("priority".into()),
            },
        );
        if catalog_at == "after" {
            LoadModels { provider }.apply(&mut snapshot, "", models());
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
            "",
            Intent::SetDraftText {
                thread_id: key,
                text: "Continue".into(),
            },
        );
        let (submitted, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::Submit {
                thread_id: Some(id),
                client_user_message_id: "next".into(),
                alternate: false,
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
            provider: Some(provider),
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
            "",
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
        assert_eq!(
            snapshot.panes.primary().navigation.thread_id.as_ref(),
            Some(&id)
        );
    }

    #[test]
    fn navigating_back_keeps_an_unsent_provider_change() {
        let id = SessionRef {
            provider: Some(ProviderKind::Codex),
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
        select_thread(&mut snapshot, "", id.clone(), "/workspace".into());
        assert_eq!(snapshot.input.drafts[&key].model.as_ref(), Some(&model));
        assert_eq!(snapshot.input.drafts[&key].text, "continue with Claude");
        assert_eq!(
            snapshot.panes.primary().navigation.thread_id.as_ref(),
            Some(&id)
        );
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
        ReadThread::open(id.clone())
            .prepare(&mut snapshot, "")
            .unwrap();
        assert_eq!(
            snapshot.panes.primary().navigation.thread_id.as_ref(),
            Some(&id)
        );
        assert_eq!(snapshot.panes.primary().navigation.cwd, "/old-project");
        assert_eq!(
            snapshot.selected_directory(snapshot.panes.primary()),
            "/old-project"
        );
        assert_eq!(
            snapshot
                .thread_metadata(&id)
                .unwrap()
                .can_accept_direct_input,
            Some(false)
        );
        let (disconnected, _) = reduce(&snapshot, "", Event::Disconnected);
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
            .prepare(&mut snapshot, "")
            .unwrap();
        open_thread(
            &mut snapshot,
            "",
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
            "",
            Intent::Submit {
                thread_id: Some(session),
                client_user_message_id: "gateway-input".into(),
                alternate: false,
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
            LoadModels { provider }.apply(&mut snapshot, "", catalog());
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
            .prepare(&mut snapshot, "")
            .unwrap();
        if catalog_at == "during" {
            LoadModels { provider }.apply(&mut snapshot, "", catalog());
        }
        open_thread(
            &mut snapshot,
            "",
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
            LoadModels { provider }.apply(&mut snapshot, "", catalog());
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
            "",
            Intent::SetDraftText {
                thread_id: key,
                text: "Next input".into(),
            },
        );
        let (submitted, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::Submit {
                thread_id: None,
                client_user_message_id: "next-input".into(),
                alternate: false,
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
