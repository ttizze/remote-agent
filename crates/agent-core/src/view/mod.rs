//! View models: pure functions of the published snapshot and the clock that
//! native clients render without re-deriving them.
pub mod activity;
pub mod agents;
pub mod api;
pub mod appearance;
pub mod archived;
pub mod attachments;
pub mod checkpoints;
pub mod diagnostics;
pub mod collation;
pub mod command_palette;
pub mod composer;
pub mod git;
pub mod device;
pub mod header;
pub mod inbox;
pub mod keybindings;
pub mod load_balancing;
pub mod models;
pub mod new_thread;
pub mod notifications;
pub mod plan;
pub mod provider_instances;
pub mod projects;
pub mod pull_requests;
pub mod preview;
pub mod queue;
pub mod rejection;
pub mod relationships;
pub mod requests;
pub mod review_files;
pub mod search;
pub mod search_ranking;
pub mod scheduled_tasks;
pub mod settings;
pub mod setup_card;
pub mod share;
pub mod shortcuts;
pub mod sidebar;
pub mod snapshot_capture;
pub mod snooze;
pub mod streaming;
pub mod terminals;
pub mod thread;
pub mod thread_arrangement;
pub mod thread_list;
pub mod thread_menu;
pub mod thread_order;
pub mod thread_sort;
pub mod thread_summary;
pub mod time;
pub mod timeline;
pub mod usage;
pub mod work_log;
pub mod working_status;
pub mod workspace_search;

/// A count and its noun: "1 file", "3 files".
pub(crate) fn quantity(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// A length as the native views count it.
pub(crate) fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}
