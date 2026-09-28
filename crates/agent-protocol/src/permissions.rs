//! Editing provider-owned user defaults, never per-turn overrides.
use crate::session::ProviderKind;
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
    pub provider: ProviderKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePermissionSettings {
    pub provider: ProviderKind,
    pub mode: PermissionMode,
    pub version: String,
}
