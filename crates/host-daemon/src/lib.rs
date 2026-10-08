mod account_usage;
pub mod adapters;
pub mod browser;
mod claude;
mod dictation;
mod git;
mod host_identity;
mod host_rpc;
mod host_runtime;
pub mod local_host;
pub mod platform;
mod projects;
mod terminals;
mod workspace_files;
mod workspace_review;
mod worktrees;
use workspace_review::inspect_workspace;

pub use host_runtime::HostRuntime;

pub use host_identity::{CredentialStore, FileKeyStore, HostCredentials, load_local_identity};

pub use host_rpc::{HostRpcService, HostSession, SessionId};
pub use projects::ProjectStore;

mod visualize;
