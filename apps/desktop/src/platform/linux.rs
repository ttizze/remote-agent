use std::{os::unix::process::CommandExt, process::Command};

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
}

/// Linux desktop environments do not expose one pinned, portable app-badge
/// API. GPUI owns the notification surface here; retaining this adapter keeps
/// the pending-notice lifecycle explicit without drawing a fake sidebar badge.
pub(super) fn set_notification_badge(_count: u32) {}
