use std::{os::unix::process::CommandExt, process::Command};

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
