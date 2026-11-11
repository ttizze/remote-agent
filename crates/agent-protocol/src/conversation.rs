//! Deterministic decisions for one Host-owned conversation.
//!
//! The owner supplies a fresh seed for each new input and commits `Step` before
//! executing its actions. Replaying an input uses the same seed and timestamp.
//! Runtime records (attempts, bindings and allocation counters) stay on the Host;
//! only the five conversation entities are projected into synchronization rows.
use crate::{
    execution::{BackgroundTask, ExecutionError, ItemStatus},
    ids::{ClientInputId, ItemId, RequestId, TurnId},
    items::ItemBody,
    models::ModelRef,
    operations::{FollowUp, Input as InputPart},
    requests::{Answer, RequestBody},
    session::{PreparationStage, ProviderKind},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

mod commands;
mod execution;
mod recovery;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub name: String,
    pub workspace: Workspace,
    pub provider: ProviderKind,
    pub queue_held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Shared preparation progress outlives the input that initiated it. Retrying a
/// failed input must reuse this resource, rather than reset its directory.
pub enum Workspace {
    Missing,
    Folder(String),
    Ready(String),
}

impl Workspace {
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Self::Missing => None,
            Self::Folder(cwd) | Self::Ready(cwd) => Some(cwd),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryMode {
    Direct,
    Queued,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputContent {
    pub parts: Vec<InputPart>,
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub follow_up: FollowUp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingInput {
    pub id: ClientInputId,
    pub position: u64,
    pub content: InputContent,
    pub state: InputState,
}

impl PendingInput {
    fn live_delivery(&self) -> Option<&PendingDelivery> {
        match &self.state {
            InputState::Delivering(delivery)
            | InputState::Unknown {
                delivery,
                status: UnknownState::Live | UnknownState::Resolving { .. },
            } => Some(delivery),
            _ => None,
        }
    }

    fn resolved(&self, retry: bool) -> Change {
        if retry {
            Change::Input(Self {
                state: InputState::Queued { steer: None },
                ..self.clone()
            })
        } else {
            Change::RemoveInput(self.id.clone())
        }
    }

    fn fenced(&self) -> Option<Change> {
        let delivery = self.live_delivery()?;
        if let InputState::Unknown {
            status: UnknownState::Resolving { retry },
            ..
        } = self.state
        {
            return Some(self.resolved(retry));
        }
        Some(Change::Input(Self {
            state: InputState::Unknown {
                delivery: delivery.clone(),
                status: UnknownState::Fenced,
            },
            ..self.clone()
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingDelivery {
    pub action: Uuid,
    pub attempt: Attempt,
    pub ordinal: u64,
    pub interrupt: Option<Uuid>,
    pub steer: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputState {
    Preparing {
        stage: PreparationStage,
        action: Uuid,
        mode: DeliveryMode,
    },
    Queued {
        steer: Option<Uuid>,
    },
    Delivering(PendingDelivery),
    Unknown {
        delivery: PendingDelivery,
        status: UnknownState,
    },
    Failed(ExecutionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnknownState {
    Live,
    /// User's disposition is saved before fencing. A fence result, the turn
    /// ending, or restart settles it: retry restores a held input, discard
    /// removes it.
    Resolving {
        retry: bool,
    },
    Fenced,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub ordinal: u64,
    pub started_at: i64,
    pub state: TurnState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TurnState {
    Running { interrupt: Option<Uuid> },
    Waiting(Wait),
    Completed,
    Interrupted,
    Failed(ExecutionError),
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Wait {
    Retry { at: i64 },
    Background { tasks: Vec<BackgroundTask> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub turn: TurnId,
    pub position: u64,
    pub status: ItemStatus,
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub turn: TurnId,
    pub body: RequestBody,
    pub state: RequestState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RequestState {
    Awaiting,
    Sending { action: Uuid, answer: Answer },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    Ready { source: String },
    Injecting { source: String, action: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: Uuid,
    pub turn: TurnId,
    pub provider: ProviderKind,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub input: u64,
    pub turn: u64,
    pub item: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The owner's decision working set, not a full transcript or a client DTO.
/// Load all admitted inputs and unfinished turns/requests, the history rows
/// referenced by the event or a pending delivery (including completed turns and
/// their current attempts), the latest turn, and the persisted counters before
/// calling `step`.
pub struct State {
    pub thread: Thread,
    pub inputs: BTreeMap<ClientInputId, PendingInput>,
    pub turns: BTreeMap<TurnId, Turn>,
    pub items: BTreeMap<ItemId, Item>,
    pub requests: BTreeMap<RequestId, Request>,
    pub bindings: BTreeMap<ProviderKind, Binding>,
    pub attempts: BTreeMap<TurnId, Attempt>,
    pub counters: Counters,
}

impl State {
    fn has_queued_inputs(&self) -> bool {
        self.inputs.values().any(|input| {
            matches!(
                input.state,
                InputState::Queued { .. }
                    | InputState::Unknown {
                        status: UnknownState::Resolving { retry: true },
                        ..
                    }
            )
        })
    }
    pub fn new(thread: Thread) -> Self {
        Self {
            thread,
            inputs: BTreeMap::new(),
            turns: BTreeMap::new(),
            items: BTreeMap::new(),
            requests: BTreeMap::new(),
            bindings: BTreeMap::new(),
            attempts: BTreeMap::new(),
            counters: Counters::default(),
        }
    }

    pub fn active_turn(&self) -> Option<&Turn> {
        self.turns
            .values()
            .filter(|turn| {
                matches!(
                    turn.state,
                    TurnState::Running { .. } | TurnState::Waiting(_)
                )
            })
            .max_by_key(|turn| turn.ordinal)
    }

    fn steerable_attempt(&self) -> Option<&Attempt> {
        self.active_turn()
            .filter(|turn| matches!(turn.state, TurnState::Running { interrupt: None }))
            .and_then(|turn| self.attempts.get(&turn.id))
    }

    /// Recheck just before dispatching an action. The resource owner must also
    /// fence its own live connection generation while performing the I/O.
    pub fn permits(&self, action: &Action) -> bool {
        match action {
            Action::Prepare { id, action, stage, .. } => self.inputs.get(id).is_some_and(|input|
                matches!(input.state, InputState::Preparing { action: current, stage: expected, .. } if current == *action && expected == *stage)),
            Action::Deliver { id, action, attempt, .. } => self.inputs.get(id).is_some_and(|input|
                matches!(&input.state, InputState::Delivering(delivery) if delivery.action == *action && &delivery.attempt == attempt)),
            Action::FenceDelivery { id, action, attempt } => self.inputs.get(id).is_some_and(|input|
                matches!(&input.state, InputState::Unknown { delivery, status: UnknownState::Resolving { .. } }
                    if delivery.action == *action && &delivery.attempt == attempt)),
            Action::Interrupt { attempt, action } => self.attempts.get(&attempt.turn) == Some(attempt)
                && self.turns.get(&attempt.turn).is_some_and(|turn|
                    matches!(turn.state, TurnState::Running { interrupt: Some(current) } if current == *action)),
            Action::Resume { attempt, .. } => self.attempts.get(&attempt.turn) == Some(attempt)
                && self.turns.get(&attempt.turn).is_some_and(|turn| turn.state == TurnState::Running { interrupt: None }),
            Action::Answer { attempt, id, action, .. } => self.attempts.get(&attempt.turn) == Some(attempt)
                && self.requests.get(id).is_some_and(|request|
                    matches!(request.state, RequestState::Sending { action: current, .. } if current == *action)),
            Action::CancelPreparation { .. } => true,
            Action::WaitUntil(at) => self.turns.values().any(|turn|
                matches!(turn.state, TurnState::Waiting(Wait::Retry { at: due }) if due <= *at)),
        }
    }

    /// Apply only after the owner's transaction commits successfully.
    pub fn apply(&self, changes: &[Change]) -> Self {
        let mut next = self.clone();
        for change in changes {
            match change {
                Change::Thread(row) => next.thread = row.clone(),
                Change::Input(row) => {
                    next.inputs.insert(row.id.clone(), row.clone());
                }
                Change::RemoveInput(id) => {
                    next.inputs.remove(id);
                }
                Change::Turn(row) => {
                    next.turns.insert(row.id.clone(), row.clone());
                }
                Change::Item(row) => {
                    next.items.insert(row.id.clone(), row.clone());
                }
                Change::Request(row) => {
                    next.requests.insert(row.id.clone(), row.clone());
                }
                Change::RemoveRequest(id) => {
                    next.requests.remove(id);
                }
                Change::Binding { provider, value } => {
                    if let Some(value) = value {
                        next.bindings.insert(*provider, value.clone());
                    } else {
                        next.bindings.remove(provider);
                    }
                }
                Change::Attempt { turn, value } => {
                    if let Some(value) = value {
                        next.attempts.insert(turn.clone(), value.clone());
                    } else {
                        next.attempts.remove(turn);
                    }
                }
                Change::Counters(counters) => next.counters = counters.clone(),
            }
        }
        next
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Change {
    Thread(Thread),
    Input(PendingInput),
    RemoveInput(ClientInputId),
    Turn(Turn),
    Item(Item),
    Request(Request),
    RemoveRequest(RequestId),
    Binding {
        provider: ProviderKind,
        value: Option<Binding>,
    },
    Attempt {
        turn: TurnId,
        value: Option<Attempt>,
    },
    Counters(Counters),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ChangeKey {
    Thread,
    Input(ClientInputId),
    Turn(TurnId),
    Item(ItemId),
    Request(RequestId),
    Binding(ProviderKind),
    Attempt(TurnId),
    Counters,
}

impl Change {
    fn key(&self) -> ChangeKey {
        match self {
            Self::Thread(_) => ChangeKey::Thread,
            Self::Input(input) => ChangeKey::Input(input.id.clone()),
            Self::RemoveInput(id) => ChangeKey::Input(id.clone()),
            Self::Turn(turn) => ChangeKey::Turn(turn.id.clone()),
            Self::Item(item) => ChangeKey::Item(item.id.clone()),
            Self::Request(request) => ChangeKey::Request(request.id.clone()),
            Self::RemoveRequest(id) => ChangeKey::Request(id.clone()),
            Self::Binding { provider, .. } => ChangeKey::Binding(*provider),
            Self::Attempt { turn, .. } => ChangeKey::Attempt(turn.clone()),
            Self::Counters(_) => ChangeKey::Counters,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationId {
    Client(Uuid),
    Job(Uuid),
}

impl OperationId {
    /// The typed RPC accepts `Client` only. Internal delivery cannot collide
    /// with a client UUID, even when both UUID values happen to be identical.
    pub fn receipt_key(self) -> String {
        match self {
            Self::Client(id) => id.to_string(),
            Self::Job(id) => format!("job:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Submit {
        id: ClientInputId,
        content: InputContent,
    },
    ResumeQueue,
    HoldQueue,
    RemoveInput(ClientInputId),
    ResolveUnknown {
        id: ClientInputId,
        retry: bool,
    },
    RetryInput(ClientInputId),
    ContinueAfterRestart {
        source: TurnId,
        enabled: bool,
        content: InputContent,
    },
    Interrupt,
    Answer {
        id: RequestId,
        answer: Answer,
    },
    Rename(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// The owner looks up the receipt by operation ID and checks store and
    /// target identity before calling step. Existing receipts skip all work.
    Command {
        id: OperationId,
        command: Command,
        receipt: Option<Outcome>,
    },
    Prepared {
        id: ClientInputId,
        action: Uuid,
        cwd: Option<String>,
        source: Option<String>,
    },
    PreparationFailed {
        id: ClientInputId,
        action: Uuid,
        error: ExecutionError,
    },
    Delivered {
        id: ClientInputId,
        action: Uuid,
    },
    DeliveryFailed {
        id: ClientInputId,
        action: Uuid,
        error: ExecutionError,
        unknown: bool,
    },
    /// The owner has fenced this delivery's callbacks and native write. Only
    /// then is a saved user disposition applied. A turn ending and restart
    /// also settle it. A live ambiguous start can still be proved by its
    /// original attempt before fencing completes.
    DeliveryFenced {
        id: ClientInputId,
        action: Uuid,
    },
    Provider {
        attempt: Uuid,
        observation: Observation,
    },
    Answered {
        id: RequestId,
        action: Uuid,
        result: Result<(), crate::error::Delivery>,
    },
    Wake,
    Restart {
        updating: bool,
        continue_after_restart: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Observation {
    Item {
        /// An application ID, resolved by the Host's native-ID boundary. The
        /// owner commits that mapping together with this step's item changes.
        id: ItemId,
        status: ItemStatus,
        body: ItemBody,
    },
    Request {
        /// Application ID, resolved within the provider attempt just like items.
        id: RequestId,
        body: RequestBody,
    },
    Completed,
    Failed(ExecutionError),
    Interrupted,
    Retry {
        at: i64,
    },
    Background {
        tasks: Vec<BackgroundTask>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    FenceDelivery {
        id: ClientInputId,
        action: Uuid,
        attempt: Attempt,
    },
    CancelPreparation {
        action: Uuid,
    },
    Prepare {
        id: ClientInputId,
        action: Uuid,
        stage: PreparationStage,
        cwd: Option<String>,
        provider: ProviderKind,
    },
    Deliver {
        id: ClientInputId,
        action: Uuid,
        attempt: Attempt,
        cwd: String,
        content: InputContent,
        steer: bool,
    },
    Interrupt {
        attempt: Attempt,
        action: Uuid,
    },
    Resume {
        attempt: Attempt,
        cwd: String,
    },
    Answer {
        attempt: Attempt,
        id: RequestId,
        action: Uuid,
        answer: Answer,
    },
    WaitUntil(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub id: Uuid,
    pub source: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Accepted,
    Rejected(String),
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub changes: Vec<Change>,
    pub actions: Vec<Action>,
    /// The owner persists this self-delivery with the interrupted turn. Its ID
    /// is stable under replay; delivery uses OperationId::Job and a receipt.
    pub continuations: Vec<Continuation>,
    pub receipt: Option<(OperationId, Outcome)>,
    /// Row-changing inputs are acknowledged as Accepted; stale inputs are Ignored.
    /// Rejected commands have no row changes but still receive a receipt.
    pub outcome: Outcome,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            changes: Vec::new(),
            actions: Vec::new(),
            continuations: Vec::new(),
            receipt: None,
            outcome: Outcome::Ignored,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub now: i64,
    pub seed: Uuid,
    pub steers: bool,
}

fn allocate(seed: Uuid, label: &str) -> Uuid {
    Uuid::new_v5(&seed, label.as_bytes())
}

/// Allocate a new item after looking up the persisted native mapping first.
/// Existing mapped IDs take precedence, including IDs retained by migration.
pub fn provider_item_id(attempt: Uuid, native: &str) -> ItemId {
    allocate(attempt, &format!("item:{native}"))
        .to_string()
        .into()
}

/// Fresh request IDs use the same attempt boundary as fresh item IDs.
pub fn provider_request_id(attempt: Uuid, native: &str) -> RequestId {
    allocate(attempt, &format!("request:{native}"))
        .to_string()
        .into()
}

pub fn step(state: &State, event: Event, context: Context) -> Step {
    let mut result = match event {
        Event::Command {
            id,
            command,
            receipt,
        } => {
            if let Some(outcome) = receipt {
                return Step {
                    outcome,
                    ..Step::default()
                };
            }
            let mut result = commands::command(state, id, command, context);
            result.receipt = Some((id, result.outcome.clone()));
            result
        }
        Event::Restart {
            updating,
            continue_after_restart,
        } => recovery::restart(state, updating && continue_after_restart, context),
        event => execution::event(state, event, context),
    };
    // A composed decision can touch an entity more than once. Persist only its
    // final value, with parent turns before items and execution records.
    result.changes = result
        .changes
        .into_iter()
        .map(|change| (change.key(), change))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect();
    if !result.actions.is_empty() {
        let next = state.apply(&result.changes);
        result.actions.retain(|action| next.permits(action));
    }
    result
}
