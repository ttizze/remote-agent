//! Import of existing Codex and Claude transcripts into registered projects.
mod fs;
mod git;
mod importer;
mod json;
mod paths;
mod record;
mod scanner;
mod sources;

pub use fs::*;
pub use git::{ProjectGit, normalize_remote_url};
pub use importer::*;
pub use paths::MANAGED_WORKTREE_SEGMENT;
pub use record::{
    MAX_IMPORT_RECORDS, MAX_IMPORTED_MESSAGES, SessionMessage, SessionThread, TranscriptMeta,
    parse_session_transcript,
};
pub use scanner::*;
pub use sources::FIRST_RUN_IMPORT_KEY;
