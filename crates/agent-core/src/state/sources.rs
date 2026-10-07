//! What the Host reported about workspaces for the composer menu, the diff
//! panel and the new-task branch picker: provider commands, path search, Git
//! status, refs and diff previews.
use agent_protocol::workspace as w;
use std::{collections::BTreeMap, sync::Arc};

/// A failed or still pending provider command scan is asked again after this.
pub const PROVIDER_COMMANDS_RETRY_MS: u64 = 10_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderCommandsEntry {
    pub commands: Option<w::ProviderCommands>,
    pub in_flight: bool,
    /// When a pending or failed scan may be asked again.
    pub retry_at_ms: Option<u64>,
}
impl ProviderCommandsEntry {
    /// Holds commands the provider has finished reporting.
    pub fn complete(&self) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|commands| !commands.slash_commands_pending)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryQuery {
    pub cwd: String,
    pub query: String,
    pub limit: u32,
}

/// The composer's `@` path search: the query it waits to ask and the last
/// answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntrySearchState {
    pub wanted: Option<EntryQuery>,
    /// The wanted query is asked once its debounce passes.
    pub due_at_ms: Option<u64>,
    pub result: Option<(EntryQuery, Vec<w::WorkspaceEntry>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefScope {
    Local,
    Remote,
    /// Every branch, as the new-task picker lists them.
    All,
}
impl RefScope {
    pub fn kind(self) -> w::RefKind {
        match self {
            Self::Local => w::RefKind::Local,
            Self::Remote => w::RefKind::Remote,
            Self::All => w::RefKind::All,
        }
    }
}

/// The last ref listing of one checkout and scope.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RefsEntry {
    pub query: String,
    pub list: Option<w::RefList>,
    pub error: Option<String>,
    pub in_flight: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffPreviewEntry {
    pub request: w::DiffPreview,
    pub result: Option<Arc<w::DiffPreviewResult>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceSources {
    /// By provider instance and directory.
    pub provider_commands: BTreeMap<(String, String), ProviderCommandsEntry>,
    pub entries: EntrySearchState,
    /// By checkout directory.
    pub vcs_status: BTreeMap<String, w::VcsStatus>,
    pub refs: BTreeMap<(String, RefScope), RefsEntry>,
    pub diff_preview: Option<DiffPreviewEntry>,
}
impl WorkspaceSources {
    pub fn provider_commands(&self, instance: &str, cwd: &str) -> Option<&w::ProviderCommands> {
        self.provider_commands
            .get(&(instance.to_owned(), cwd.to_owned()))
            .and_then(|entry| entry.commands.as_ref())
    }
    pub fn refs(&self, cwd: &str, scope: RefScope) -> Option<&RefsEntry> {
        self.refs.get(&(cwd.to_owned(), scope))
    }
}
