//! Conversation RPC contracts. Canonical projections carry saved state.
use super::InputContent;
use crate::{
    models::ModelRef,
    session::{Capabilities, SessionRef},
};
use serde::{Deserialize, Serialize};
mod view;
pub use view::{compact, preview, unavailable};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribe {
    pub session: SessionRef,
    pub subscription: uuid::Uuid,
    pub after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opened {
    pub session: SessionRef,
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
    pub initial: Vec<crate::orchestration::projection::ThreadStreamItem>,
    pub provider: crate::session::ProviderKind,
    pub capabilities: Capabilities,
    pub project_root: Option<String>,
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
        validate_opened(&self.session, output, self.after_sequence.is_none())
    }
}

fn validate_opened(
    expected: &SessionRef,
    output: &Opened,
    require_snapshot: bool,
) -> Result<(), &'static str> {
    if expected != &output.session {
        return Err("conversation identity changed");
    }
    let expected = expected.application_id();
    for item in &output.initial {
        match item {
            crate::orchestration::projection::ThreadStreamItem::Snapshot(snapshot)
                if snapshot.projection.thread.id != expected =>
            {
                return Err("conversation identity changed");
            }
            crate::orchestration::projection::ThreadStreamItem::Event { event, .. }
                if event.thread_id != expected =>
            {
                return Err("conversation event belongs to another thread");
            }
            _ => {}
        }
    }
    if require_snapshot
        && !output.initial.iter().any(|item| {
            matches!(
                item,
                crate::orchestration::projection::ThreadStreamItem::Snapshot(_)
            )
        })
    {
        return Err("conversation header is missing");
    }
    Ok(())
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
        validate_opened(&self.session, output, true)
    }
}

impl Create {
    pub fn validate_creation(&self) -> Result<(), &'static str> {
        self.session.validate()?;
        if let Some(workspace) = &self.workspace {
            workspace.validate()?;
        }
        if self.session.provider.is_some() {
            return Err("new conversations require a provider-independent ID");
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
    pub command: super::Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acknowledged {
    pub sequence: u64,
    pub accepted_at: String,
    pub error: Option<String>,
}

impl Acknowledged {
    pub fn accepted(self) -> Result<u64, crate::error::RpcFailure> {
        match self.error {
            Some(message) => Err(crate::error::RpcFailure {
                code: "command_rejected".into(),
                message,
                delivery: crate::error::Delivery::NotSent,
                execution: None,
            }),
            None => Ok(self.sequence),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extend {
    pub session: SessionRef,
    pub cursor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hydrate {
    pub session: SessionRef,
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
    pub through: ForkPoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ForkPoint {
    Run { run_id: String },
    ProviderTurn { provider_turn_id: String },
    TurnItem { turn_item_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hydrated {
    pub item: Option<crate::orchestration::TurnItem>,
    /// Oversized detail bodies use the existing one-use blob transfer.
    pub transfer: Option<crate::models::TransferGrant>,
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
    DEFAULT_SEARCH_LIMIT, OpenedList, SearchList, SubscribeList, validate as validate_list,
};
