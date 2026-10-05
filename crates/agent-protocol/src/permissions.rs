//! Editing provider-owned user defaults, never per-turn overrides.
use crate::session::ProviderInstanceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Ask,
    Auto,
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSettings {
    pub mode: Option<PermissionMode>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadPermissionSettings {
    #[serde(rename = "instanceId")]
    pub instance_id: ProviderInstanceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePermissionSettings {
    #[serde(rename = "instanceId")]
    pub instance_id: ProviderInstanceId,
    pub mode: PermissionMode,
    pub version: String,
}
