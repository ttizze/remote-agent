//! The native user config owns permissions. Nothing is added to turn requests.
use super::{codex::Codex, service::Failure};
use agent_protocol::permissions::{PermissionMode, PermissionSettings};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Write, path::Path};

fn failure(message: &str) -> Failure {
    Failure::new("permission_settings_failed", message)
}

#[derive(Deserialize)]
struct ConfigRead {
    layers: Vec<ConfigLayer>,
}
#[derive(Deserialize)]
struct ConfigLayer {
    name: Value,
    config: Value,
    version: String,
}

impl Codex {
    async fn user_permission_layer(&self) -> Result<ConfigLayer, Failure> {
        let config: ConfigRead = self
            .request("config/read", &json!({"includeLayers":true}))
            .await?;
        config
            .layers
            .into_iter()
            .find(|layer| layer.name["type"] == "user" && layer.name["profile"].is_null())
            .ok_or_else(|| failure("Codex のユーザー設定を取得できませんでした。"))
    }

    pub(super) async fn read_permissions(&self) -> Result<PermissionSettings, Failure> {
        let layer = self.user_permission_layer().await?;
        Ok(PermissionSettings {
            mode: codex_mode(&layer.config),
            version: layer.version,
        })
    }

    pub(super) async fn update_permissions(
        &self,
        mode: PermissionMode,
        version: &str,
    ) -> Result<PermissionSettings, Failure> {
        let layer = self.user_permission_layer().await?;
        if layer.version != version {
            return Err(failure(
                "設定が外部で変更されました。再読み込みしてください。",
            ));
        }
        let (approval, reviewer, sandbox) = match mode {
            PermissionMode::Ask => ("on-request", "user", "workspace-write"),
            PermissionMode::Auto => ("on-request", "auto_review", "workspace-write"),
            PermissionMode::FullAccess => ("never", "user", "danger-full-access"),
        };
        let edits: Vec<_> = [
            ("approval_policy", json!(approval)),
            ("approvals_reviewer", json!(reviewer)),
            ("sandbox_mode", json!(sandbox)),
        ]
        .into_iter()
        .map(|(key, value)| json!({"keyPath":key,"value":value,"mergeStrategy":"replace"}))
        .collect();
        let _: Value = self
            .request(
                "config/batchWrite",
                &json!({
                    "edits":edits, "filePath":layer.name["file"],
                    "expectedVersion":version,"reloadUserConfig":false,
                }),
            )
            .await?;
        self.read_permissions().await
    }
}

fn codex_mode(config: &Value) -> Option<PermissionMode> {
    match (
        config["approval_policy"].as_str(),
        config["approvals_reviewer"].as_str(),
        config["sandbox_mode"].as_str(),
    ) {
        (Some("never"), _, Some("danger-full-access")) => Some(PermissionMode::FullAccess),
        (Some("on-request"), None | Some("user"), Some("workspace-write")) => {
            Some(PermissionMode::Ask)
        }
        (Some("on-request"), Some("auto_review"), Some("workspace-write")) => {
            Some(PermissionMode::Auto)
        }
        _ => None,
    }
}

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

pub(super) fn read_claude_permissions(home: &Path) -> Result<PermissionSettings, Failure> {
    let (config, version) = read_claude_file(&home.join("settings.json"))?;
    Ok(claude_settings(&config, version))
}

pub(super) fn update_claude_permissions(
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

    #[test]
    fn codex_permissions_do_not_mislabel_custom_policies() {
        for (config, mode) in [
            (
                json!({"approval_policy":"on-request","sandbox_mode":"workspace-write"}),
                PermissionMode::Ask,
            ),
            (
                json!({"approval_policy":"on-request","sandbox_mode":"workspace-write","approvals_reviewer":"auto_review"}),
                PermissionMode::Auto,
            ),
            (
                json!({"approval_policy":"never","sandbox_mode":"danger-full-access"}),
                PermissionMode::FullAccess,
            ),
        ] {
            assert_eq!(codex_mode(&config), Some(mode));
        }
        for config in [
            json!({}),
            json!({"approval_policy":"untrusted","sandbox_mode":"workspace-write"}),
            json!({"approval_policy":"on-request","sandbox_mode":"read-only"}),
            json!({"approval_policy":"on-request","sandbox_mode":"workspace-write","approvals_reviewer":"custom"}),
        ] {
            assert_eq!(codex_mode(&config), None);
        }
    }
}
