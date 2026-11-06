use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

#[cfg(target_os = "macos")]
pub(crate) fn initialize() {
    let executable = std::env::current_exe().expect("Host executable path");
    let base = directories::BaseDirs::new();
    let user = std::env::var_os("USER");
    let inherited = std::env::var_os("PATH");
    let path = provider_path(
        executable.parent().expect("Host executable directory"),
        base.as_ref().map(|base| base.home_dir()),
        user.as_deref(),
        inherited.as_deref(),
    )
    .expect("provider search path");
    // main calls this before starting Tokio or any other threads. The Host
    // owns its environment, including when launched directly by an updater.
    unsafe { std::env::set_var("PATH", path) };
}

fn provider_path(
    directory: &Path,
    home: Option<&Path>,
    user: Option<&OsStr>,
    inherited: Option<&OsStr>,
) -> Result<OsString, std::env::JoinPathsError> {
    let mut paths = Vec::new();
    // Provider scripts with an env-node shebang also use the bundled runtime.
    if let Some(node) = agent_local_host::bundled_node_path(directory) {
        paths.push(node.parent().expect("Node has a parent").to_path_buf());
    }
    if let Some(home) = home {
        paths.push(home.join(".local/bin"));
        paths.push(home.join(".nix-profile/bin"));
    }
    if let Some(user) = user {
        paths.push(Path::new("/etc/profiles/per-user").join(user).join("bin"));
    }
    paths.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/run/current-system/sw/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(Into::into),
    );
    if let Some(inherited) = inherited {
        paths.extend(std::env::split_paths(inherited));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    std::env::join_paths(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_host_launch_finds_user_tools_and_bundled_node_with_a_minimal_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("Bex Dev.app/Contents/MacOS");
        let node = agent_local_host::bundled_node_path(&directory).unwrap();
        let home = root.path().join("user");
        let claude = home.join(".nix-profile/bin/claude");
        for binary in [&node, &claude] {
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::write(binary, b"fixture executable").unwrap();
        }
        let inherited = std::env::join_paths(["/usr/bin", "/bin", "/custom/bin"]).unwrap();
        let path = provider_path(
            &directory,
            Some(&home),
            Some(OsStr::new("fixture-user")),
            Some(&inherited),
        )
        .unwrap();
        let find = |name: &str| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(name))
                .find(|candidate| candidate.is_file())
        };
        assert_eq!(find("claude"), Some(claude));
        assert_eq!(find("node"), Some(node));
        assert!(
            std::env::split_paths(&path)
                .any(|directory| directory == Path::new("/etc/profiles/per-user/fixture-user/bin"))
        );
        assert_eq!(
            std::env::split_paths(&path).last().unwrap(),
            Path::new("/custom/bin")
        );
        assert_eq!(
            provider_path(
                &directory,
                Some(&home),
                Some(OsStr::new("fixture-user")),
                Some(&path)
            )
            .unwrap(),
            path
        );
    }
}
