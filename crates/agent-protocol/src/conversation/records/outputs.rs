use crate::{ids::ItemId, session::SessionRef, sync::Output};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OutputKey {
    pub session: SessionRef,
    pub item: ItemId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeOutput {
    pub key: OutputKey,
    pub subscription: uuid::Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedOutput {
    pub key: OutputKey,
    pub initial: Output,
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
}

impl crate::operations::RpcMethod for SubscribeOutput {
    crate::operations::rpc_contract!(SubscribeCommandOutput);
    fn params(&self) -> Result<Self, crate::error::PeerError> {
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if self.key != output.key {
            return Err("command output target changed");
        }
        Ok(())
    }
}
