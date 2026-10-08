mod account_usage;
mod acp_registry;
mod acp_runtime;
mod background;
pub use background::{DesktopProcessMonitor, sample_desktop_power, sample_local_power};
pub mod browser;
mod checkpoints;
mod claude;
mod codex_accounts;
pub mod conversation;
pub mod device;
mod device_stream;
mod dictation;
mod favicon;
pub mod ffmpeg;
mod git;
mod github;
mod host_identity;
mod host_rpc;
mod host_runtime;
mod keybindings;
pub mod local_host;
pub mod platform;
mod power_events;
pub(crate) mod preview;
mod projects;
mod provider_maintenance;
mod repository;
mod terminals;
mod text_generation;
mod update;
mod usage;
mod vcs;
mod workspace_files;
mod workspace_review;
mod workspace_search;
mod worktrees;
use workspace_review::inspect_workspace;

pub use host_runtime::HostRuntime;

pub use host_identity::{CredentialStore, FileKeyStore, HostCredentials, load_local_identity};

pub use host_rpc::service::ConversationSettings;
pub use host_rpc::{HostRpcService, HostSession, SessionId};
pub use projects::ProjectStore;
pub(crate) use update::UpdateManager;
pub use update::{
    acknowledge_update_target, clear_update_handoff, current_update_version, request_update_handoff,
};

mod visualize;
