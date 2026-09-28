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

pub struct PermissionControl<'a> {
    pub provider: ProviderKind,
    pub label: &'static str,
    pub mode: Option<PermissionMode>,
    pub version: Option<&'a str>,
    pub loading: bool,
    pub error: Option<&'a str>,
}
impl Snapshot {
    pub fn permission_control(&self, draft_key: &str) -> PermissionControl<'_> {
        let provider = self.model_provider_for_draft(draft_key.into());
        let state = self
            .permission_settings
            .as_ref()
            .filter(|state| state.provider == provider);
        let result = state.and_then(|state| state.result.as_ref());
        let settings = result.and_then(|result| result.as_ref().ok());
        let mode = settings.and_then(|settings| settings.mode);
        PermissionControl {
            provider,
            label: match mode {
                Some(mode) => {
                    PERMISSION_CHOICES
                        .iter()
                        .find(|choice| choice.0 == mode)
                        .unwrap()
                        .1
                }
                None if settings.is_some() => "カスタム・未設定",
                None => "権限",
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
    fn permission_cache_is_provider_specific_and_never_persisted() {
        let mut snapshot = Snapshot {
            permission_settings: Some(Arc::new(PermissionSettingsState {
                provider: ProviderKind::Codex,
                result: Some(Ok(PermissionSettings {
                    mode: Some(PermissionMode::FullAccess),
                    version: "native-version".into(),
                })),
            })),
            ..Default::default()
        };
        assert_eq!(snapshot.permission_control("draft").label, "フルアクセス");
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some("claude:sonnet".into()),
                ..Default::default()
            }),
        );
        let control = snapshot.permission_control("draft");
        assert_eq!(control.provider, ProviderKind::Claude);
        assert_eq!(control.label, "権限");
        assert_eq!(control.version, None);
        let bytes = crate::persistence::encode(&snapshot).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("native-version"));
        let restored = crate::persistence::decode(&bytes).unwrap();
        assert!(restored.permission_settings.is_none());
    }
}
