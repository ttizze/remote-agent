//! Permission menu copy and provider selection shared by clients.
use crate::state::Snapshot;
use agent_protocol::{
    permissions::{PermissionMode, ReadPermissionSettings},
    session::ProviderInstanceId,
};

pub const PERMISSION_CHOICES: [(PermissionMode, &str, &str); 3] = [
    (
        PermissionMode::Ask,
        "承認を求める",
        "権限が必要な操作では確認を求めます",
    ),
    (
        PermissionMode::Auto,
        "代わりに承認",
        "操作のリスクに応じて自動で承認を判断します",
    ),
    (
        PermissionMode::FullAccess,
        "フルアクセス",
        "承認の確認なしで操作を実行します",
    ),
];

pub struct PermissionControl<'a> {
    pub instance_id: Option<ProviderInstanceId>,
    pub load_request: Option<ReadPermissionSettings>,
    pub label: &'static str,
    pub mode: Option<PermissionMode>,
    pub version: Option<&'a str>,
    pub loading: bool,
    pub error: Option<&'a str>,
}
impl Snapshot {
    pub fn permission_control(&self, draft_key: &crate::state::DraftKey) -> PermissionControl<'_> {
        let provider = self.provider_selection(draft_key.clone(), None);
        let state = self
            .permission_settings
            .as_ref()
            .filter(|state| Some(&state.instance_id) == provider.as_ref());
        let result = state.and_then(|state| state.result.as_ref());
        let settings = result.and_then(|result| result.as_ref().ok());
        let mode = settings.and_then(|settings| settings.mode);
        PermissionControl {
            instance_id: provider.clone(),
            load_request: provider
                .filter(|_| self.connected && state.is_none())
                .map(|instance_id| ReadPermissionSettings { instance_id }),
            label: match mode {
                Some(mode) => {
                    PERMISSION_CHOICES
                        .iter()
                        .find(|choice| choice.0 == mode)
                        .unwrap()
                        .1
                }
                None if settings.is_some() => "カスタム・未設定",
                None if result.is_some() => "取得できません",
                None => "読み込み中…",
            },
            mode,
            version: settings.map(|settings| settings.version.as_str()),
            loading: state.is_some() && result.is_none(),
            error: result.and_then(|result| result.as_ref().err().map(String::as_str)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, operations::PermissionSettingsState};
    use agent_protocol::permissions::PermissionSettings;
    use std::sync::Arc;

    #[test]
    fn permissions_load_on_connection_and_provider_change_without_retry_loops() {
        let mut snapshot = Snapshot {
            provider_instances: crate::test_support::instances(),
            ..Default::default()
        };
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_request
                .is_none()
        );
        snapshot.connected = true;
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_request
                .unwrap()
                .instance_id,
            "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap()
        );
        snapshot.permission_settings = Some(Arc::new(PermissionSettingsState {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            result: None,
        }));
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_request
                .is_none()
        );
        Arc::make_mut(snapshot.permission_settings.as_mut().unwrap()).result =
            Some(Err("offline".into()));
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_request
                .is_none()
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .label,
            "取得できません"
        );
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "sonnet".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_request
                .unwrap()
                .instance_id,
            "claude"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap()
        );
    }

    #[test]
    fn permission_cache_is_provider_specific_and_never_persisted() {
        let mut snapshot = Snapshot {
            provider_instances: crate::test_support::instances(),
            permission_settings: Some(Arc::new(PermissionSettingsState {
                instance_id: "codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                result: Some(Ok(PermissionSettings {
                    mode: Some(PermissionMode::FullAccess),
                    version: "native-version".into(),
                })),
            })),
            ..Default::default()
        };
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .label,
            "フルアクセス"
        );
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "sonnet".into(),
                }),
                ..Default::default()
            }),
        );
        let control = snapshot.permission_control(&crate::state::DraftKey::from("draft"));
        assert_eq!(
            control.instance_id,
            Some(
                "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap()
            )
        );
        assert_eq!(control.label, "読み込み中…");
        assert_eq!(control.version, None);
        let session = crate::session::SessionRef {
            id: "native".into(),
        };
        Arc::make_mut(&mut snapshot.conversations).insert(
            session.clone(),
            Arc::new(crate::models::Thread {
                id: Some(session.clone()),
                provider: Some(agent_protocol::providers::ProviderRef {
                    instance_id: "claude".parse().unwrap(),
                    driver: "claudeAgent".parse().unwrap(),
                }),
                ..Default::default()
            }),
        );
        let key = crate::state::DraftKey::from(session);
        assert_eq!(
            snapshot.permission_control(&key).instance_id,
            Some(
                "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap()
            )
        );
        Arc::make_mut(&mut snapshot.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "codex-model".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot.permission_control(&key).instance_id,
            Some(
                "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap()
            )
        );
        let bytes = crate::persistence::encode(&snapshot).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("native-version"));
        let restored = crate::persistence::decode(&bytes).unwrap();
        assert!(restored.permission_settings.is_none());
    }
}
