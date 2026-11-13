//! Saved conversation rows shared by the Host writer and client replica.
use super::{self as decision, InputContent, Thread, Wait};
use crate::{
    ids::TurnId,
    models::ModelRef,
    session::{Capabilities, SessionRef},
};
use serde::{Deserialize, Serialize};

mod view;
pub use view::{compact, preview, project, summary, unavailable};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}

impl Selection {
    pub fn resolve(
        &self,
        model: Option<&ModelRef>,
        effort: Option<&str>,
        service_tier: Option<&str>,
    ) -> Self {
        let saved = model
            .is_none_or(|model| Some(model) == self.model.as_ref())
            .then_some(self);
        Self {
            model: model
                .or_else(|| saved.and_then(|saved| saved.model.as_ref()))
                .cloned(),
            effort: effort
                .or_else(|| saved.and_then(|saved| saved.effort.as_deref()))
                .map(str::to_owned),
            service_tier: service_tier
                .or_else(|| saved.and_then(|saved| saved.service_tier.as_deref()))
                .map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePreparation {
    pub project_cwd: String,
    pub strategy: Option<crate::workspace::WorkspaceStrategy>,
    pub generation: uuid::Uuid,
    /// The current preparation created this checkout; cancellation owns its cleanup.
    pub created_worktree: bool,
    pub setup: Option<crate::workspace::WorkspaceSetup>,
    pub visible: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Details {
    pub steerable: bool,
    pub skips_compaction: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Box<WorkspacePreparation>>,
    pub parent: Option<SessionRef>,
    pub selection: Selection,
    pub capabilities: Capabilities,
    pub preview: Option<String>,
    pub updated_at: i64,
    pub read_through: Option<TurnId>,
    pub project: crate::models::ProjectMembership,
    pub branch: Option<String>,
    pub has_changes: Option<bool>,
    pub agent: Option<String>,
    pub can_accept_direct_input: Option<bool>,
    pub history_read_state: Option<crate::session::HistoryReadState>,
    pub activity: Activity,
}

/// Covers work outside the loaded history window as well.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    pub status: crate::execution::SessionStatus,
    pub waiting: bool,
    pub approval: bool,
    pub latest: Option<crate::execution::TurnStatus>,
    pub completion: Option<TurnId>,
    pub background: Vec<crate::execution::BackgroundTask>,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            stale: false,
            status: crate::execution::SessionStatus::Idle,
            waiting: false,
            approval: false,
            latest: None,
            completion: None,
            background: Vec::new(),
        }
    }
}

impl Activity {
    pub fn unobserved_running(&self) -> bool {
        self.status == crate::execution::SessionStatus::Running
            && self.latest != Some(crate::execution::TurnStatus::Running)
            && !self.stale
    }
    pub fn status(&self) -> crate::execution::SessionStatus {
        if self.stale {
            crate::execution::SessionStatus::Unknown
        } else {
            self.status
        }
    }

    pub fn facts(&self) -> crate::live_activity::TaskState {
        crate::live_activity::TaskState {
            status: self.status(),
            waiting: self.waiting && !self.stale,
            approval: self.approval && !self.stale,
            latest: self.latest,
            background: (!self.stale && !self.background.is_empty())
                .then(|| crate::execution::background_holds_completion(&self.background)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub state: Thread,
    pub details: Details,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub id: crate::ids::ClientInputId,
    pub position: u64,
    pub content: InputContent,
    pub state: Delivery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Delivery {
    Preparing(crate::session::PreparationStage),
    Queued,
    Delivering,
    Unknown {
        resolving: bool,
    },
    Failed {
        stage: crate::session::PreparationStage,
        error: Option<crate::execution::ExecutionError>,
    },
}

impl From<decision::PendingInput> for Input {
    fn from(input: decision::PendingInput) -> Self {
        let state = match input.state {
            decision::InputState::Preparing { stage, .. } => Delivery::Preparing(stage),
            decision::InputState::Queued { .. } => Delivery::Queued,
            decision::InputState::Delivering(_) => Delivery::Delivering,
            decision::InputState::Unknown { status, .. } => Delivery::Unknown {
                resolving: matches!(status, decision::UnknownState::Resolving { .. }),
            },
            decision::InputState::Failed { stage, error } => Delivery::Failed { stage, error },
        };
        Self {
            id: input.id,
            position: input.position,
            content: input.content,
            state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub ordinal: u64,
    pub provider: crate::session::ProviderKind,
    pub queued: bool,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<u64>,
    pub error: Option<crate::execution::ExecutionError>,
    pub state: Execution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Execution {
    Running { stopping: bool },
    Waiting(Wait),
    Completed,
    Interrupted,
    Failed(crate::execution::ExecutionError),
    RolledBack,
}

impl From<decision::Turn> for Turn {
    fn from(turn: decision::Turn) -> Self {
        let state = match turn.state {
            decision::TurnState::Running { interrupt } => Execution::Running {
                stopping: interrupt.is_some(),
            },
            decision::TurnState::Waiting(wait) => Execution::Waiting(wait),
            decision::TurnState::Completed => Execution::Completed,
            decision::TurnState::Interrupted => Execution::Interrupted,
            decision::TurnState::Failed(error) => Execution::Failed(error),
            decision::TurnState::RolledBack => Execution::RolledBack,
        };
        Self {
            id: turn.id,
            ordinal: turn.ordinal,
            provider: turn.provider,
            queued: turn.queued,
            started_at: turn.started_at,
            completed_at: turn.completed_at,
            duration_ms: turn.duration_ms,
            error: turn.error,
            state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: crate::ids::RequestId,
    pub turn: TurnId,
    pub body: crate::requests::RequestBody,
    pub state: AnswerState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub turn: TurnId,
    pub position: u64,
    pub content: std::sync::Arc<crate::models::Item>,
}

impl From<decision::Item> for Item {
    fn from(item: decision::Item) -> Self {
        let mut content = crate::models::Item::new(item.id.clone(), item.status, item.body);
        content.client_input_id = item.input;
        if item.deferred {
            content.defer();
        }
        if matches!(content.body(), crate::items::ItemBody::UserMessage { .. })
            && content.client_input_id.is_none()
            && uuid::Uuid::parse_str(item.id.as_str()).is_ok()
        {
            content.client_input_id = Some(item.id.to_string().into());
        }
        Self {
            turn: item.turn,
            position: item.position,
            content: std::sync::Arc::new(content),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnswerState {
    Awaiting,
    Sending,
    Unknown,
}

impl From<decision::Request> for Request {
    fn from(request: decision::Request) -> Self {
        let state = match request.state {
            decision::RequestState::Awaiting => AnswerState::Awaiting,
            decision::RequestState::Sending { .. } => AnswerState::Sending,
            decision::RequestState::Unknown => AnswerState::Unknown,
        };
        Self {
            id: request.id,
            turn: request.turn,
            body: request.body,
            state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Record {
    Thread(Header),
    Input(Input),
    Turn(Turn),
    Item(Item),
    Request(Request),
    Project(crate::models::Project),
    Catalog(Catalog),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Catalog {
    Loading,
    Ready,
    Failed(String),
}

// Replica updates can copy row maps without copying old message bodies.
pub type Message = crate::sync::Message<SessionRef, std::sync::Arc<Record>>;
pub type Row = crate::sync::Row<SessionRef, std::sync::Arc<Record>>;
pub type Replica = crate::sync::Replica<SessionRef, std::sync::Arc<Record>>;

/// Every row has one identity and one position, including when its body is
/// deferred. Check the domain content before applying any part of a frame.
pub fn validate_message(message: &Message) -> Result<(), crate::sync::ApplyError> {
    use crate::sync::{Content, Key, Position};
    let rows = match message {
        Message::Snapshot { rows, .. }
        | Message::Changes { rows, .. }
        | Message::Extend { rows, .. } => rows.as_slice(),
        Message::Hydrate { row, .. } => std::slice::from_ref(row),
        Message::Missing => return Ok(()),
    };
    for row in rows {
        let record = match &row.content {
            Content::Full(value) | Content::Summary(value) => value.as_ref(),
        };
        let valid = match (&row.key, &row.position, record) {
            (Key::Thread(_), Position::Unordered, Record::Thread(_))
            | (Key::Catalog(_), Position::Unordered, Record::Catalog(_)) => true,
            (Key::Project(id), Position::Project { .. }, Record::Project(project)) => {
                id == &project.id
            }
            (Key::Input { id, .. }, Position::Unordered, Record::Input(input)) => id == &input.id,
            (Key::Request { id, .. }, Position::Unordered, Record::Request(request)) => {
                id == &request.id
            }
            (Key::Turn { id, .. }, Position::Turn { ordinal }, Record::Turn(turn)) => {
                id == &turn.id && ordinal == &turn.ordinal
            }
            (Key::Item { id, .. }, Position::Item { turn, position, .. }, Record::Item(item)) => {
                id == &item.content.id && turn == &item.turn && position == &item.position
            }
            _ => false,
        };
        if !valid {
            return Err(crate::sync::ApplyError::Identity);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribe {
    pub session: SessionRef,
    pub subscription: uuid::Uuid,
    pub cached: Option<crate::sync::Cursor>,
    pub window: Option<crate::sync::Window>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opened {
    pub session: SessionRef,
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
    pub initial: Message,
}

impl crate::operations::RpcMethod for Subscribe {
    crate::operations::rpc_contract!(SubscribeConversation);
    fn params(&self) -> Result<Self, crate::error::PeerError> {
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        validate_opened(&self.session, output)
    }
}

fn validate_opened(expected: &SessionRef, output: &Opened) -> Result<(), &'static str> {
    if expected != &output.session {
        return Err("conversation identity changed");
    }
    match &output.initial {
        Message::Snapshot { rows, .. }
            if rows
                .iter()
                .any(|row| row.key == crate::sync::Key::Thread(expected.clone())) => {}
        Message::Changes { .. } => {}
        _ => return Err("conversation header is missing"),
    }
    validate_message(&output.initial).map_err(|_| "conversation row identity changed")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Create {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::workspace::WorkspaceStrategy>,
    pub session: SessionRef,
    pub provider: crate::session::ProviderKind,
    pub subscription: uuid::Uuid,
    pub cwd: Option<String>,
    pub selection: Selection,
    pub initial: Option<(crate::ids::ClientInputId, InputContent)>,
}

impl crate::operations::RpcMethod for Create {
    crate::operations::rpc_contract!(CreateConversation);
    fn params(&self) -> Result<Self, crate::error::PeerError> {
        self.validate_creation()
            .map_err(|error| crate::error::PeerError::InvalidMessage(error.into()))?;
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        validate_opened(&self.session, output)
    }
}

impl Create {
    pub fn validate_creation(&self) -> Result<(), &'static str> {
        self.session.validate()?;
        if let Some(workspace) = &self.workspace {
            workspace.validate()?;
        }
        if self.session.provider.is_some() {
            return Err("new conversations require a provider-independent UUID");
        }
        if let Some((id, _)) = &self.initial {
            uuid::Uuid::parse_str(id.as_str()).map_err(|_| "input ID must be a UUID")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub session: SessionRef,
    pub operation: uuid::Uuid,
    pub command: decision::Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acknowledged {
    pub outcome: decision::Outcome,
    pub cursor: crate::sync::Cursor,
}

impl Acknowledged {
    pub fn accepted(self) -> Result<crate::sync::Cursor, crate::error::RpcFailure> {
        match self.outcome {
            decision::Outcome::DeliveryUnknown => Err(crate::error::RpcFailure {
                code: "submission_delivery_unknown".into(),
                message:
                    "以前の送信結果を確認できません。履歴を確認してから再送または破棄してください。"
                        .into(),
                delivery: crate::error::Delivery::Unknown,
                execution: None,
            }),
            decision::Outcome::Rejected(message) => Err(crate::error::RpcFailure {
                code: "command_rejected".into(),
                message,
                delivery: crate::error::Delivery::NotSent,
                execution: None,
            }),
            _ => Ok(self.cursor),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extend {
    pub session: SessionRef,
    pub subscription: uuid::Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hydrate {
    pub session: SessionRef,
    pub subscription: uuid::Uuid,
    pub item: crate::ids::ItemId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckCommands {
    pub session: SessionRef,
    pub operations: Vec<uuid::Uuid>,
}
impl CheckCommands {
    pub const MAX_OPERATIONS: usize = 256;
}

pub type Receipts = Vec<(uuid::Uuid, Acknowledged)>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fork {
    pub source: SessionRef,
    pub target: SessionRef,
    pub through: TurnId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hydrated {
    pub cursor: crate::sync::Cursor,
    pub version: Option<u64>,
    /// Only large body bytes travel separately; rows are ordered on the subscription.
    pub transfer: Option<crate::models::TransferGrant>,
}

/// Attach separately transferred bytes only to the exact row that granted them.
/// This never introduces an item, position or row version through an RPC reply.
pub fn hydrate_body(replica: &Replica, version: u64, item: crate::models::Item) -> Option<Row> {
    let crate::sync::Scope::Conversation(id) = replica.scope() else {
        return None;
    };
    let mut row = replica
        .rows()
        .get(&crate::sync::Key::Item {
            thread: id.clone(),
            id: item.id.clone(),
        })?
        .clone();
    if row.version != version {
        return None;
    }
    let (crate::sync::Content::Full(value) | crate::sync::Content::Summary(value)) = &row.content;
    let Record::Item(saved) = value.as_ref() else {
        return None;
    };
    if item.is_deferred() {
        return None;
    }
    row.content = crate::sync::Content::Full(std::sync::Arc::new(Record::Item(Item {
        content: std::sync::Arc::new(item),
        ..saved.clone()
    })));
    Some(row)
}

/// Deferred bodies keep their identity, position and saved row version.
pub fn defer(row: &mut Row) {
    let value = match &row.content {
        crate::sync::Content::Full(value) | crate::sync::Content::Summary(value) => value,
    };
    let Record::Item(item) = value.as_ref() else {
        return;
    };
    let mut item = item.clone();
    std::sync::Arc::make_mut(&mut item.content).defer();
    row.content = crate::sync::Content::Summary(std::sync::Arc::new(Record::Item(item)));
}

pub const WORK_DETAIL_BYTES: usize = 128 * 1024;

pub fn summarize(rows: &mut [Row], mut remaining: usize) -> Result<(), String> {
    // Keep normal messages inline. Work details spend their
    // separate budget newest-first, independent of a database's return order.
    let mut order: Vec<_> = (0..rows.len()).collect();
    order.sort_by_key(|index| match &rows[*index].position {
        crate::sync::Position::Item {
            ordinal, position, ..
        } => std::cmp::Reverse((*ordinal, *position)),
        _ => std::cmp::Reverse((0, 0)),
    });
    for index in order {
        let row = &mut rows[index];
        let value = match &row.content {
            crate::sync::Content::Full(value) | crate::sync::Content::Summary(value) => value,
        };
        let Record::Item(item) = value.as_ref() else {
            continue;
        };
        let bytes = crate::protocol::encode(item.content.as_ref())
            .map_err(|error| error.to_string())?
            .len();
        if matches!(
            item.content.body(),
            crate::items::ItemBody::UserMessage { .. }
                | crate::items::ItemBody::AssistantText { .. }
        ) {
            // The independent large-body transfer still bounds one exceptional
            // message; the work-detail budget never truncates normal text.
            if bytes > crate::models::MAX_INLINE_ITEM_BYTES {
                defer(row);
            }
        } else if bytes > remaining {
            defer(row);
        } else {
            remaining -= bytes;
        }
    }
    Ok(())
}

/// Host-owned history and names support these operations independently of the
/// provider's native optimization; steering still reflects the actual resource.
pub fn capabilities(provider: Capabilities) -> Capabilities {
    Capabilities {
        rename: true,
        fork: true,
        ..provider
    }
}
mod outputs;
pub use outputs::{OpenedOutput, OutputKey, SubscribeOutput};

mod list;
pub use list::{
    DEFAULT_SEARCH_LIMIT, OpenedList, SearchList, SubscribeList, activity as list_activity,
    catalog_ready, rows_activity as list_rows_activity, validate as validate_list,
};
