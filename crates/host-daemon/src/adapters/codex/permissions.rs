//! The native user config owns permissions. Nothing is added to turn requests.
use super::Codex;
use crate::host_rpc::service::Failure;
use agent_protocol::permissions::{PermissionMode, PermissionSettings};
use serde::Deserialize;
use serde_json::{Value, json};

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

#[cfg(test)]
mod tests {
    use super::*;

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
