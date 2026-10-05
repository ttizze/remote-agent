//! An account overlay shares history and configuration while keeping auth local.
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, path::Path};

const SHARED_DIRECTORIES: &[&str] = &[
    "sessions",
    "archived_sessions",
    "sqlite",
    "shell_snapshots",
    "worktrees",
    "skills",
    "plugins",
    "cache",
    "logs",
    "mcp-oauth-locks",
];
const PRIVATE: &[&str] = &["auth.json", "models_cache.json"];
const LOCAL: &[&str] = &["log", "memories", "tmp"];

pub(super) fn materialize(shared: &Path, shadow: &Path) -> Result<()> {
    std::fs::create_dir_all(shared)?;
    crate::platform::create_state_directory(shadow)?;
    let shared = dunce::canonicalize(shared)?;
    let shadow = dunce::canonicalize(shadow)?;
    ensure!(
        shared != shadow,
        "Codex shadow home must differ from shared home"
    );
    let mut entries: BTreeSet<_> = SHARED_DIRECTORIES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    for name in SHARED_DIRECTORIES {
        std::fs::create_dir_all(shared.join(name))?;
    }
    for entry in std::fs::read_dir(&shared)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Codex shared home contains a non-UTF-8 entry"))?;
        if !PRIVATE.contains(&name.as_str()) && !LOCAL.contains(&name.as_str()) {
            entries.insert(name);
        }
    }
    ensure!(
        !std::fs::symlink_metadata(shadow.join("auth.json"))
            .is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Codex shadow auth must not be a symlink"
    );
    if std::fs::symlink_metadata(shadow.join("models_cache.json"))
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        std::fs::remove_file(shadow.join("models_cache.json"))?;
    }
    for name in entries {
        let target = shared.join(&name);
        let link = shadow.join(&name);
        match std::fs::symlink_metadata(&link) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let current = std::fs::read_link(&link)?;
                if current == target {
                    continue;
                }
                std::fs::remove_file(&link)?;
            }
            Ok(metadata) => {
                ensure!(
                    name == "mcp-oauth-locks" && metadata.is_dir(),
                    "Codex shadow entry conflicts with a shared entry"
                );
                std::fs::remove_dir_all(&link)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).context("cannot link Codex shared entry")?;
        #[cfg(windows)]
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(&target, &link)?;
        } else {
            std::os::windows::fs::symlink_file(&target, &link)?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn overlay_shares_native_state_but_preserves_local_auth_and_cache() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        let shadow = root.path().join("shadow");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&shadow).unwrap();
        for home in [&shared, &shadow] {
            std::fs::write(
                home.join("auth.json"),
                home.file_name().unwrap().as_encoded_bytes(),
            )
            .unwrap();
        }
        std::fs::write(shared.join("config.toml"), "shared configuration").unwrap();
        std::fs::write(shared.join("models_cache.json"), "shared cache").unwrap();
        std::fs::create_dir_all(shadow.join("mcp-oauth-locks")).unwrap();
        std::fs::write(shadow.join("mcp-oauth-locks/stale"), "runtime lock").unwrap();
        std::fs::create_dir_all(shadow.join("sessions")).unwrap();
        std::fs::write(shadow.join("sessions/keep"), "existing user data").unwrap();
        assert!(materialize(&shared, &shadow).is_err());
        assert_eq!(
            std::fs::read_to_string(shadow.join("sessions/keep")).unwrap(),
            "existing user data"
        );
        std::fs::remove_file(shadow.join("sessions/keep")).unwrap();
        std::fs::remove_dir(shadow.join("sessions")).unwrap();
        symlink(
            shared.join("models_cache.json"),
            shadow.join("models_cache.json"),
        )
        .unwrap();
        materialize(&shared, &shadow).unwrap();
        materialize(&shared, &shadow).unwrap();
        assert_eq!(
            std::fs::read_to_string(shadow.join("auth.json")).unwrap(),
            "shadow"
        );
        assert_eq!(
            std::fs::read_to_string(shared.join("auth.json")).unwrap(),
            "shared"
        );
        assert!(!shadow.join("models_cache.json").exists());
        assert!(shadow.join("sessions").is_symlink());
        assert!(shadow.join("mcp-oauth-locks").is_symlink());
        assert_eq!(
            std::fs::read_to_string(shadow.join("config.toml")).unwrap(),
            "shared configuration"
        );
        assert!(!shadow.join("tmp").is_symlink());
        assert!(materialize(&shared, &shared).is_err());
    }

    #[test]
    fn unsafe_auth_links_are_rejected_and_old_shared_links_can_be_retargeted() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        let newer = root.path().join("newer");
        let shadow = root.path().join("shadow");
        materialize(&shared, &shadow).unwrap();
        std::fs::write(shared.join("auth.json"), "placeholder").unwrap();
        symlink(shared.join("auth.json"), shadow.join("auth.json")).unwrap();
        assert!(materialize(&shared, &shadow).is_err());
        assert_eq!(
            std::fs::read_to_string(shared.join("auth.json")).unwrap(),
            "placeholder"
        );
        std::fs::remove_file(shadow.join("auth.json")).unwrap();
        materialize(&newer, &shadow).unwrap();
        assert_eq!(
            std::fs::read_link(shadow.join("sessions")).unwrap(),
            dunce::canonicalize(&newer).unwrap().join("sessions")
        );
    }
}
