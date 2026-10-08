use crate::host_rpc::service::Failure;
use agent_protocol::permissions::{PermissionMode, PermissionSettings};
use serde_json::{Value, json};

fn failure(message: &str) -> Failure {
    Failure::new("permission_settings_failed", message)
}

use std::{io::Write, path::Path};

fn read_claude_file(path: &Path) -> Result<(Value, String), Failure> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err(failure("Claude の設定ファイルを読み取れませんでした。")),
    };
    let config = if bytes.is_empty() && !path.exists() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| failure("Claude の設定が不正な JSON です。ファイルを修正してください。"))?
    };
    if !config.is_object() || config.get("permissions").is_some_and(|v| !v.is_object()) {
        return Err(failure("Claude の設定形式が不正です。"));
    }
    let version = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((config, version))
}

fn claude_settings(config: &Value, version: String) -> PermissionSettings {
    PermissionSettings {
        mode: match config["permissions"]["defaultMode"].as_str() {
            Some("default" | "manual") => Some(PermissionMode::Ask),
            Some("auto") => Some(PermissionMode::Auto),
            Some("bypassPermissions") => Some(PermissionMode::FullAccess),
            _ => None,
        },
        version,
    }
}

pub(crate) fn read_claude_permissions(home: &Path) -> Result<PermissionSettings, Failure> {
    let (config, version) = read_claude_file(&home.join("settings.json"))?;
    Ok(claude_settings(&config, version))
}

pub(crate) fn update_claude_permissions(
    home: &Path,
    mode: PermissionMode,
    version: &str,
) -> Result<PermissionSettings, Failure> {
    let settings_path = home.join("settings.json");
    // Preserve a native config symlink instead of replacing it with a BEX file.
    let path = if settings_path.symlink_metadata().is_ok() {
        dunce::canonicalize(&settings_path)
            .map_err(|_| failure("Claude の設定の参照先を開けませんでした。"))?
    } else {
        settings_path
    };
    let (mut config, current_version) = read_claude_file(&path)?;
    if current_version != version {
        return Err(failure(
            "設定が外部で変更されました。再読み込みしてください。",
        ));
    }
    let permissions = std::fs::metadata(&path)
        .ok()
        .map(|metadata| metadata.permissions());
    if permissions
        .as_ref()
        .is_some_and(|permissions| permissions.readonly())
    {
        return Err(failure("Claude の設定は読み取り専用です。"));
    }
    config
        .as_object_mut()
        .expect("validated object")
        .entry("permissions")
        .or_insert_with(|| json!({}))["defaultMode"] = json!(match mode {
        PermissionMode::Ask => "default",
        PermissionMode::Auto => "auto",
        PermissionMode::FullAccess => "bypassPermissions",
    });
    let write = || -> std::io::Result<()> {
        let parent = path.parent().expect("settings parent");
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        if let Some(permissions) = permissions {
            temp.as_file().set_permissions(permissions)?;
        }
        serde_json::to_writer_pretty(&mut temp, &config)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(&path).map_err(|error| error.error)?;
        Ok(())
    };
    write().map_err(|_| failure("Claude の設定を保存できませんでした。"))?;
    read_claude_permissions(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_permissions_preserve_unrelated_keys_and_external_changes() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        let unset = read_claude_permissions(home.path()).unwrap();
        assert_eq!(unset.mode, None);
        let created =
            update_claude_permissions(home.path(), PermissionMode::Ask, &unset.version).unwrap();
        assert_eq!(created.mode, Some(PermissionMode::Ask));
        let native = json!({"model":"custom","permissions":{"allow":["Read"],"deny":["Bash(rm *)"],"defaultMode":"acceptEdits"}});
        std::fs::write(&path, serde_json::to_vec(&native).unwrap()).unwrap();
        let initial = read_claude_permissions(home.path()).unwrap();
        assert_eq!(initial.mode, None);
        let updated =
            update_claude_permissions(home.path(), PermissionMode::FullAccess, &initial.version)
                .unwrap();
        assert_eq!(updated.mode, Some(PermissionMode::FullAccess));
        let mut expected = native;
        expected["permissions"]["defaultMode"] = json!("bypassPermissions");
        let saved = std::fs::read(&path).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&saved).unwrap(), expected);
        assert!(
            update_claude_permissions(home.path(), PermissionMode::Ask, &initial.version).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        for invalid in ["", "{", "[]", r#"{"permissions":null}"#] {
            std::fs::write(&path, invalid).unwrap();
            assert!(read_claude_permissions(home.path()).is_err());
            assert!(
                update_claude_permissions(home.path(), PermissionMode::Ask, &updated.version)
                    .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_permissions_preserve_symlinks_and_refuse_readonly_settings() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("native.json");
        std::fs::write(&target, "{}").unwrap();
        let link = home.path().join("settings.json");
        symlink(&target, &link).unwrap();
        let initial = read_claude_permissions(home.path()).unwrap();
        let updated =
            update_claude_permissions(home.path(), PermissionMode::Auto, &initial.version).unwrap();
        assert!(link.is_symlink());
        assert_eq!(updated.mode, Some(PermissionMode::Auto));
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(
            update_claude_permissions(home.path(), PermissionMode::Ask, &updated.version).is_err()
        );
        assert_eq!(read_claude_permissions(home.path()).unwrap(), updated);
        std::fs::remove_file(&link).unwrap();
        symlink(&link, &link).unwrap();
        assert!(read_claude_permissions(home.path()).is_err());
    }
}
