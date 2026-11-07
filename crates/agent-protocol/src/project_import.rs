//! Discovery registers native workspaces; conversations retain their provider identities.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProjects {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportProjects {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectCandidate {
    pub path: String,
    pub name: String,
    pub providers: Vec<crate::session::ProviderKind>,
    pub conversation_count: u32,
    pub last_active_at: Option<f64>,
    pub is_repository: bool,
    pub registered: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectScan {
    pub projects: Vec<ProjectCandidate>,
    pub warnings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectImportResult {
    pub warnings: BTreeMap<String, String>,
    pub imported: Vec<String>,
    pub errors: BTreeMap<String, String>,
}
