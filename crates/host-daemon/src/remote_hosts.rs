use agent_core::transport::{NodeId, Ticket};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHostProfile {
    pub id: NodeId,
    pub name: String,
    pub ticket: Ticket,
}
