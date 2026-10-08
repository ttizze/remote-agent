use std::{os::unix::process::CommandExt, process::Command};

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
    let mut paths = Vec::new();
    if let Some(base) = directories::BaseDirs::new() {
        paths.push(base.home_dir().join(".local/bin"));
        paths.push(base.home_dir().join(".nix-profile/bin"));
    }
    if let Some(user) = std::env::var_os("USER") {
        paths.push(
            std::path::Path::new("/etc/profiles/per-user")
                .join(user)
                .join("bin"),
        );
    }
    paths.extend(
        [
            "/run/current-system/sw/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(Into::into),
    );
    if let Some(inherited) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&inherited));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
}

pub(super) fn set_notification_badge(count: u32) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(mtm);
    let dock_tile = application.dockTile();
    let count = if application.isActive() { 0 } else { count };
    if count == 0 {
        dock_tile.setBadgeLabel(None);
        dock_tile.setShowsApplicationBadge(false);
    } else {
        let label = NSString::from_str(&count.to_string());
        dock_tile.setBadgeLabel(Some(&label));
        dock_tile.setShowsApplicationBadge(true);
    }
    dock_tile.display();
}
