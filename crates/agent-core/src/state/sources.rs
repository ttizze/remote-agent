//! What the Host reported about workspaces for the composer menu, the diff
//! panel and the new-task branch picker: provider commands, path search, refs
//! and diff previews.
use agent_protocol::workspace as w;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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

/// One file's patch of a diff source too large to send whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiffFilePatch {
    /// The source of the requested kind, or why it could not be read.
    pub result: Option<Result<Arc<w::DiffSource>, String>>,
    pub in_flight: bool,
}

/// The per-file patches of the shown diff while its source is truncated but
/// lists every file: each file's patch is asked for on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFilesEntry {
    /// The preview request whose source lists the files.
    pub preview: w::DiffPreview,
    pub kind: w::DiffSourceKind,
    pub base_ref: Option<String>,
    pub diff_hash: String,
    /// The preview the loaded patches follow; a newer one reloads them.
    pub generated_at: agent_domain::Timestamp,
    /// Desktop orders the files by path, mobile keeps the Host's order.
    pub layout: crate::view::timeline::rows::TimelineLayout,
    /// The source's files in the Host's order, with complete counts.
    pub files: Vec<w::DiffFile>,
    /// By path, the requested files.
    pub patches: BTreeMap<String, DiffFilePatch>,
    /// Requested files waiting for a free read, first asked first.
    pub queue: Vec<String>,
    /// Files whose read in flight belongs to an older diff: its reply is
    /// dropped before the file is read again.
    pub superseded: BTreeSet<String>,
    /// Changes whenever a file's request or patch changes.
    pub revision: u64,
}
impl DiffFilesEntry {
    /// The single-file preview request of `file`.
    pub fn file_request(&self, file: &w::DiffFile) -> w::DiffPreview {
        w::DiffPreview {
            cwd: self.preview.cwd.clone(),
            base_ref: self
                .base_ref
                .clone()
                .or_else(|| self.preview.base_ref.clone()),
            ignore_whitespace: self.preview.ignore_whitespace,
            file: Some(w::DiffPreviewFile {
                path: file.path.clone(),
                previous_path: file.previous_path.clone(),
                source: self.kind,
            }),
        }
    }
    /// Reads in flight, current or outdated.
    pub fn reading(&self) -> usize {
        self.patches
            .values()
            .filter(|patch| patch.in_flight)
            .count()
            + self.superseded.len()
    }
    /// A patch is being read or waits for a read.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty() || self.reading() > 0
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceSources {
    /// By provider instance and directory.
    pub provider_commands: BTreeMap<(String, String), ProviderCommandsEntry>,
    pub entries: EntrySearchState,
    pub refs: BTreeMap<(String, RefScope), RefsEntry>,
    pub diff_preview: Option<DiffPreviewEntry>,
    pub diff_files: Option<DiffFilesEntry>,
    /// The layout the composer last reported, whose wording its menu uses.
    pub composer_layout: crate::view::timeline::rows::TimelineLayout,
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

/// A project's icon as the Host served it, keyed by its content hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectIcon {
    pub hash: String,
    pub mime_type: String,
    pub data: Arc<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectIconEntry {
    /// The project's saved icon path and update time the icon was read for.
    pub version: String,
    pub in_flight: bool,
    /// `None` shows the project's initials.
    pub icon: Option<ProjectIcon>,
}
