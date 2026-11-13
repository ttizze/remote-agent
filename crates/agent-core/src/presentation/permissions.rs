//! Permission menu copy and provider selection shared by clients.
use crate::state::Snapshot;
use agent_protocol::{permissions::PermissionMode, session::ProviderKind};

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

#[derive(Clone, PartialEq)]
pub struct PermissionControl {
    pub provider: Option<ProviderKind>,
    /// The menu's heading: whose approval mode it changes.
    pub title: &'static str,
    pub load_provider: Option<ProviderKind>,
    pub label: &'static str,
    pub mode: Option<PermissionMode>,
    pub version: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}
impl Snapshot {
    pub fn permission_control(&self, draft_key: &crate::state::DraftKey) -> PermissionControl {
        let provider = self.model_provider_for_draft(draft_key.clone());
        let state = provider.and_then(|provider| self.catalog.permission_settings.get(&provider));
        let result = state.and_then(|state| state.result.as_ref());
        let settings = result.and_then(|result| result.as_ref().ok());
        let mode = settings.and_then(|settings| settings.mode);
        PermissionControl {
            provider,
            title: self.catalog.ui_language.text(match provider {
                Some(ProviderKind::Codex) => "Codex の承認方法",
                Some(ProviderKind::Claude) => "Claude の承認方法",
                Some(ProviderKind::DeepSeek) => "DeepSeek Harness の承認方法",
                None => "エージェント未選択",
            }),
            load_provider: provider.filter(|_| self.connection.connected && state.is_none()),
            label: self.catalog.ui_language.text(match mode {
                Some(mode) => {
                    PERMISSION_CHOICES
                        .iter()
                        .find(|choice| choice.0 == mode)
                        .unwrap()
                        .1
                }
                None if provider.is_none() => "エージェント未選択",
                None if settings.is_some() => "カスタム・未設定",
                None if result.is_some() => "取得できません",
                None => "読み込み中…",
            }),
            mode,
            version: settings.map(|settings| settings.version.clone()),
            loading: state.is_some() && result.is_none(),
            error: result.and_then(|result| result.as_ref().err().cloned()),
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
        let mut snapshot = Snapshot::default();
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_provider
                .is_none()
        );
        snapshot.connection.connected = true;
        assert!(
            snapshot
                .permission_control(&"draft".into())
                .load_provider
                .is_none()
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: ProviderKind::Codex,
                    id: "gpt".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_provider
                .unwrap(),
            ProviderKind::Codex
        );
        snapshot.catalog.permission_settings = Arc::new(
            [(
                ProviderKind::Codex,
                Arc::new(PermissionSettingsState { result: None }),
            )]
            .into(),
        );
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_provider
                .is_none()
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.catalog.permission_settings)
                .get_mut(&ProviderKind::Codex)
                .unwrap(),
        )
        .result = Some(Err("offline".into()));
        assert!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_provider
                .is_none()
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .label,
            "取得できません"
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "sonnet".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .load_provider
                .unwrap(),
            ProviderKind::Claude
        );
    }

    #[test]
    fn permission_cache_is_provider_specific_and_never_persisted() {
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                permission_settings: Arc::new(
                    [(
                        ProviderKind::Codex,
                        Arc::new(PermissionSettingsState {
                            result: Some(Ok(PermissionSettings {
                                mode: Some(PermissionMode::FullAccess),
                                version: "native-version".into(),
                            })),
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: ProviderKind::Codex,
                    id: "gpt".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot
                .permission_control(&crate::state::DraftKey::from("draft"))
                .label,
            "フルアクセス"
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "sonnet".into(),
                }),
                ..Default::default()
            }),
        );
        let control = snapshot.permission_control(&crate::state::DraftKey::from("draft"));
        assert_eq!(control.provider, Some(ProviderKind::Claude));
        assert_eq!(control.title, "Claude の承認方法");
        assert_eq!(control.label, "読み込み中…");
        assert_eq!(control.version, None);
        let session = crate::session::SessionRef {
            provider: Some(ProviderKind::Claude),
            id: "native".into(),
        };
        let key = crate::state::DraftKey::from(session);
        assert_eq!(
            snapshot.permission_control(&key).provider,
            Some(ProviderKind::Claude)
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "codex-model".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            snapshot.permission_control(&key).provider,
            Some(ProviderKind::Codex)
        );
        assert_eq!(snapshot.permission_control(&key).label, "フルアクセス");
        let bytes = crate::persistence::encode(&snapshot).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("native-version"));
        let restored = crate::persistence::decode(&bytes).unwrap();
        assert!(restored.catalog.permission_settings.is_empty());
    }
}
