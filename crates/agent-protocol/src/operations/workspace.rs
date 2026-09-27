//! Workspace, file, and project request records.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub revision: String,
    pub text: String,
}
// Shared Host/Client request records; Store behavior lives in state::operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProject {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListThreads {
    pub query: crate::models::ListQuery,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub directory: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFiles {
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadComposerCatalog {
    pub cwd: String,
}
impl ListThreads {
    pub fn new(query: crate::models::ListQuery) -> Self {
        Self { query }
    }
}
