use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeList {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
    pub subscription: uuid::Uuid,
    pub after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedList {
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
    pub initial: Vec<crate::orchestration::shell::ShellStreamItem>,
    pub provider_errors: serde_json::Map<String, serde_json::Value>,
}

pub fn validate(
    items: &[crate::orchestration::shell::ShellStreamItem],
) -> Result<(), &'static str> {
    use crate::orchestration::shell::{ShellStreamItem, validate_item};
    if items.last() != Some(&ShellStreamItem::Synchronized) {
        return Err("list bootstrap is not synchronized");
    }
    for (index, item) in items[..items.len() - 1].iter().enumerate() {
        validate_item(item)?;
        match item {
            ShellStreamItem::Snapshot { .. } if index == 0 => {}
            ShellStreamItem::Snapshot { .. }
            | ShellStreamItem::SnapshotRequired
            | ShellStreamItem::Synchronized => return Err("invalid list bootstrap item"),
            _ => {}
        }
    }
    Ok(())
}

impl crate::operations::RpcMethod for SubscribeList {
    crate::operations::rpc_contract!(SubscribeList);
    fn params(&self) -> Result<Self, crate::error::PeerError> {
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        validate(&output.initial)?;
        if self.after_sequence.is_none()
            && !matches!(
                output.initial.first(),
                Some(crate::orchestration::shell::ShellStreamItem::Snapshot { .. })
            )
        {
            return Err("list bootstrap has no snapshot");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchList {
    pub term: String,
    pub limit: Option<u32>,
}
/// T3 ThreadSearch defaults to fifty results when the caller supplies no bound.
pub const DEFAULT_SEARCH_LIMIT: u32 = 50;
