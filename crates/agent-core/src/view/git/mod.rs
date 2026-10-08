//! Pure Git status and action presentations shared by the native clients.
//!
//! The Host owns Git and provider I/O. These modules only turn the folded
//! status and progress records into labels, enabled states and intents.
pub mod actions;
pub mod status;
pub mod terminology;
pub mod toolbar;

pub use actions::{
    build_menu_items, default_branch_action_copy, format_elapsed, progress_view,
    requires_default_branch_confirmation, resolve_quick_action, thread_branch_update,
    DefaultBranchActionCopy, GitAction, GitActionIcon, GitActionMenuItem,
    GitActionProgressView, GitActionResultTiming, GitQuickAction, GitQuickActionKind,
};
pub use status::{status_view, GitStatusView};
pub use terminology::{change_request_terminology, ChangeRequestTerminology};
pub use toolbar::{branch_label, pull_label};
