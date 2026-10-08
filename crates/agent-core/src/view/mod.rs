//! View models: pure functions of the published snapshot and the clock that
//! native clients render without re-deriving them.
pub mod agents;
pub mod api;
pub mod appearance;
pub mod archived;
pub mod attachments;
pub mod checkpoints;
pub mod collation;
pub mod composer;
pub mod device;
pub mod header;
pub mod inbox;
pub mod keybindings;
pub mod models;
pub mod new_thread;
pub mod plan;
pub mod projects;
pub mod preview;
pub mod queue;
pub mod rejection;
pub mod relationships;
pub mod requests;
pub mod review_files;
pub mod search;
pub mod search_ranking;
pub mod settings;
pub mod setup_card;
pub mod sidebar;
pub mod snooze;
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
