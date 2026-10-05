//! Native forms share the Host configuration schema and preserve opaque fields.
use crate::state::Snapshot;
use agent_protocol::providers::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProviderFieldKind {
    Text,
    Json,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderConfigField {
    pub key: String,
    pub label: String,
    pub kind: ProviderFieldKind,
    pub value: String,
}
impl std::fmt::Debug for ProviderConfigField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfigField")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderEditor {
    pub storage_scope: String,
    pub revision: u64,
    pub existing_id: Option<String>,
    pub instance_id: String,
    pub driver: String,
    pub display_name: String,
    pub accent_color: String,
    pub enabled: bool,
    pub environment: Vec<EnvironmentVariable>,
    pub config_json: String,
    pub fields: Vec<ProviderConfigField>,
}
impl std::fmt::Debug for ProviderEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEditor")
            .field("instance_id", &self.instance_id)
            .field("driver", &self.driver)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

fn fields(driver: &str, config: &Value) -> Vec<ProviderConfigField> {
    use ProviderFieldKind::*;
    let mut definitions = match driver {
        "codex" | "claudeAgent" => vec![
            ("binaryPath", "実行ファイル", Text),
            ("homePath", "履歴と設定の保存先", Text),
            ("launchArgs", "起動引数", Text),
            ("customModels", "カスタムモデル（JSON）", Json),
        ],
        _ => Vec::new(),
    };
    if driver == "codex" {
        definitions.insert(2, ("shadowHomePath", "分離したCodex設定の保存先", Text));
    }
    if driver == "claudeAgent" {
        definitions.push(("autoCompactWindow", "自動compactの閾値", Text));
    } else if driver == "codex" {
        definitions.push(("setupMode", "セットアップ方式", Text));
    }
    definitions
        .into_iter()
        .map(|(key, label, kind)| ProviderConfigField {
            key: key.into(),
            label: label.into(),
            kind,
            value: match kind {
                Text => config
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                Json => config.get(key).map(Value::to_string).unwrap_or_default(),
            },
        })
        .collect()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// Inputs remain strings until validation, so invalid form text never enters
    /// the native ABI's validated provider identifier converter.
    pub fn provider_editor(
        &self,
        instance_id: Option<String>,
        driver: String,
    ) -> Option<ProviderEditor> {
        let settings = self.provider_settings.as_ref()?;
        let (existing_id, instance_id, config) = if let Some(id) = instance_id {
            let saved = settings
                .instances
                .iter()
                .find(|saved| saved.instance_id.as_str() == id)?;
            (Some(id.clone()), id, saved.config.clone())
        } else {
            let driver: ProviderDriver = driver.parse().ok()?;
            (
                None,
                String::new(),
                ProviderConfig {
                    driver,
                    display_name: None,
                    accent_color: None,
                    enabled: true,
                    environment: Vec::new(),
                    config: serde_json::json!({}),
                },
            )
        };
        Some(ProviderEditor {
            storage_scope: self.storage_scope.clone(),
            revision: settings.revision,
            existing_id,
            instance_id,
            driver: config.driver.to_string(),
            display_name: config.display_name.unwrap_or_default(),
            accent_color: config.accent_color.unwrap_or_default(),
            enabled: config.enabled,
            environment: config.environment,
            fields: fields(config.driver.as_str(), &config.config),
            config_json: config.config.to_string(),
        })
    }
}

impl ProviderEditor {
    pub(crate) fn mutation(
        &self,
        connected: bool,
        storage_scope: &str,
        settings: Option<&ProviderSettings>,
        remove: bool,
    ) -> Result<ProviderMutation, String> {
        if !connected || storage_scope != self.storage_scope {
            return Err("編集を開始した環境へ接続してください。".into());
        }
        let settings = settings.ok_or("provider設定を再読み込みしてください。")?;
        if settings.revision != self.revision {
            return Err(
                "provider設定が更新されました。再読み込みしてから編集してください。".into(),
            );
        }
        let instance_id: ProviderInstanceId = self.instance_id.parse().map_err(
            |_| "IDは英字で始まる64文字以内の英数字・ハイフン・アンダースコアで指定してください。",
        )?;
        let existing = self
            .existing_id
            .as_ref()
            .map(|id| {
                if id != &self.instance_id {
                    return Err("保存済みのprovider IDは変更できません。");
                }
                settings
                    .instances
                    .iter()
                    .find(|saved| saved.instance_id.as_str() == id)
                    .ok_or("providerが削除されました。設定を再読み込みしてください。")
            })
            .transpose()?;
        if remove {
            return existing
                .map(|_| ProviderMutation::Remove { instance_id })
                .ok_or_else(|| "未保存のproviderは削除できません。".into());
        }
        let driver: ProviderDriver = self.driver.parse()
            .map_err(|_| "driverは英字で始まる64文字以内の英数字・ハイフン・アンダースコアで指定してください。")?;
        if existing.is_some_and(|saved| saved.config.driver != driver) {
            return Err(
                "保存済みのdriverは変更できません。新しいproviderを追加してください。".into(),
            );
        }
        let mut config: Value = serde_json::from_str(&self.config_json)
            .map_err(|_| "provider設定のJSONを確認してください。")?;
        let originals = fields(driver.as_str(), &config);
        if self.fields.len() != originals.len() {
            return Err("provider設定の入力項目を再読み込みしてください。".into());
        }
        for (field, original) in self.fields.iter().zip(originals) {
            if field.key != original.key || field.kind != original.kind {
                return Err("provider設定の入力項目を再読み込みしてください。".into());
            }
            if field.value == original.value {
                continue;
            }
            let value = match field.kind {
                ProviderFieldKind::Text if field.value.trim().is_empty() => None,
                ProviderFieldKind::Text => {
                    if field.key == "autoCompactWindow"
                        && !(field.value.bytes().all(|byte| byte.is_ascii_digit())
                            && !field.value.starts_with('0')
                            && field
                                .value
                                .parse::<u32>()
                                .is_ok_and(|value| (100_000..=1_000_000).contains(&value)))
                    {
                        return Err("自動compactの閾値は100000〜1000000で指定してください。".into());
                    }
                    if field.key == "setupMode"
                        && !matches!(field.value.as_str(), "managed" | "existing")
                    {
                        return Err(
                            "セットアップ方式はmanagedまたはexistingを指定してください。".into(),
                        );
                    }
                    Some(Value::String(field.value.clone()))
                }
                ProviderFieldKind::Json if field.value.trim().is_empty() => None,
                ProviderFieldKind::Json => Some(
                    serde_json::from_str(&field.value)
                        .map_err(|_| "カスタムモデルのJSONを確認してください。")?,
                ),
            };
            let object = config
                .as_object_mut()
                .ok_or("provider設定はJSONオブジェクトで指定してください。")?;
            if let Some(value) = value {
                object.insert(field.key.clone(), value);
            } else {
                object.remove(&field.key);
            }
        }
        validate_environment(&self.environment)
            .map_err(|_| "環境変数の名前・値・重複を確認してください。")?;
        for variable in &self.environment {
            if variable.value_redacted
                && !existing.is_some_and(|saved| {
                    saved
                        .config
                        .environment
                        .iter()
                        .any(|prior| prior.name == variable.name && prior.sensitive)
                })
            {
                return Err(
                    "保存済みの秘密値の名前を変更する場合は、新しい値を入力してください。".into(),
                );
            }
        }
        let optional = |value: &str| (!value.trim().is_empty()).then(|| value.trim().to_owned());
        let config = ProviderConfig {
            driver,
            display_name: optional(&self.display_name),
            accent_color: optional(&self.accent_color),
            environment: self.environment.clone(),
            enabled: self.enabled,
            config,
        };
        Ok(if existing.is_some() {
            ProviderMutation::Upsert {
                instance_id,
                config,
            }
        } else {
            ProviderMutation::Create {
                instance_id,
                config,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;

    fn snapshot(driver: &str, config: Value) -> Snapshot {
        Snapshot {
            connected: true,
            storage_scope: "host-A:device".into(),
            provider_settings: Some(Arc::new(ProviderSettings {
                revision: 7,
                instances: vec![ConfiguredProvider {
                    instance_id: "work".parse().unwrap(),
                    config: ProviderConfig {
                        driver: driver.parse().unwrap(),
                        display_name: None,
                        accent_color: None,
                        enabled: true,
                        environment: vec![EnvironmentVariable {
                            name: "EXAMPLE_TOKEN".into(),
                            value: String::new(),
                            sensitive: true,
                            value_redacted: true,
                        }],
                        config,
                    },
                }],
            })),
            ..Default::default()
        }
    }
    fn saved_config(mutation: ProviderMutation) -> ProviderConfig {
        match mutation {
            ProviderMutation::Create { config, .. } | ProviderMutation::Upsert { config, .. } => {
                config
            }
            _ => panic!("expected a saved configuration"),
        }
    }

    #[test]
    fn form_capture_keeps_shadow_paths_and_validates_options_before_sending() {
        use crate::state::operations::{ApplyProviderEdit, Operation};
        let snapshot = snapshot("codex", serde_json::json!({"customModels":["old"]}));
        let mut save = ApplyProviderEdit {
            editor: snapshot
                .provider_editor(Some("work".into()), "".into())
                .unwrap(),
            remove: false,
        };
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "shadowHomePath")
            .unwrap()
            .value = "~/separate".into();
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "customModels")
            .unwrap()
            .value
            .clear();
        assert_eq!(
            saved_config(save.capture(&snapshot).unwrap().mutation).config,
            serde_json::json!({"shadowHomePath":"~/separate"})
        );
        for valid in ["managed", "existing", ""] {
            save.editor
                .fields
                .iter_mut()
                .find(|field| field.key == "setupMode")
                .unwrap()
                .value = valid.into();
            assert!(save.capture(&snapshot).is_ok());
        }
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "setupMode")
            .unwrap()
            .value = "unsupported".into();
        assert!(save.capture(&snapshot).is_err());
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "setupMode")
            .unwrap()
            .value
            .clear();
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "customModels")
            .unwrap()
            .value = "{".into();
        assert!(save.capture(&snapshot).is_err());
        save.editor
            .fields
            .iter_mut()
            .find(|field| field.key == "customModels")
            .unwrap()
            .value
            .clear();
        save.editor.fields[0].kind = ProviderFieldKind::Json;
        assert!(save.capture(&snapshot).is_err());
        save.editor.fields[0].kind = ProviderFieldKind::Text;
        save.editor.fields[0].key = "unknown-field".into();
        assert!(save.capture(&snapshot).is_err());
        save.editor.fields.remove(0);
        assert!(save.capture(&snapshot).is_err());
    }

    #[test]
    fn editing_known_fields_preserves_opaque_fields_and_saved_secrets() {
        let snapshot = snapshot(
            "codex",
            serde_json::json!({"binaryPath":"old","unknown":{"nested":[1,2]},"setupMode":"existing"}),
        );
        let mut editor = snapshot
            .provider_editor(Some("work".into()), "ignored".into())
            .unwrap();
        editor
            .fields
            .iter_mut()
            .find(|field| field.key == "binaryPath")
            .unwrap()
            .value = "new".into();
        editor.display_name = "  Office  ".into();
        let saved = saved_config(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false,
                )
                .unwrap(),
        );
        assert_eq!(
            saved.config,
            serde_json::json!({"binaryPath":"new","unknown":{"nested":[1,2]},"setupMode":"existing"})
        );
        assert_eq!(saved.display_name.as_deref(), Some("Office"));
        assert_eq!(saved.environment, editor.environment);
        editor.environment[0].name = "RENAMED_TOKEN".into();
        assert!(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        editor.environment[0].value = "replacement-placeholder".into();
        editor.environment[0].value_redacted = false;
        assert_eq!(
            saved_config(
                editor
                    .mutation(
                        snapshot.connected,
                        &snapshot.storage_scope,
                        snapshot.provider_settings.as_deref(),
                        false
                    )
                    .unwrap()
            )
            .environment[0]
                .value,
            "replacement-placeholder"
        );
        let debug = format!("{editor:?}");
        assert!(!debug.contains("replacement-placeholder"));
        assert!(!format!("{:?}", editor.fields).contains("new"));
    }

    #[test]
    fn editors_cannot_write_to_another_host_or_revision_or_rename_saved_identity() {
        let snapshot = snapshot("codex", serde_json::json!({}));
        let editor = snapshot
            .provider_editor(Some("work".into()), "".into())
            .unwrap();
        let mut other = snapshot.clone();
        other.storage_scope = "host-B:device".into();
        assert!(
            editor
                .mutation(
                    other.connected,
                    &other.storage_scope,
                    other.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        assert!(
            editor
                .mutation(
                    other.connected,
                    &other.storage_scope,
                    other.provider_settings.as_deref(),
                    true
                )
                .is_err()
        );
        other = snapshot.clone();
        other.connected = false;
        assert!(
            editor
                .mutation(
                    other.connected,
                    &other.storage_scope,
                    other.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        other = snapshot.clone();
        Arc::make_mut(other.provider_settings.as_mut().unwrap()).revision += 1;
        assert!(
            editor
                .mutation(
                    other.connected,
                    &other.storage_scope,
                    other.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        let mut changed = editor.clone();
        changed.instance_id = "renamed".into();
        assert!(
            changed
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        changed = editor.clone();
        changed.driver = "claudeAgent".into();
        assert!(
            changed
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        assert!(
            matches!(editor.mutation(snapshot.connected, &snapshot.storage_scope, snapshot.provider_settings.as_deref(), true).unwrap(), ProviderMutation::Remove { instance_id } if instance_id.as_str() == "work")
        );
    }

    #[test]
    fn new_instances_validate_form_text_before_typed_ffi_and_keep_unknown_driver_json() {
        let snapshot = snapshot("codex", serde_json::json!({}));
        assert!(
            snapshot
                .provider_editor(None, "invalid/driver".into())
                .is_none()
        );
        let mut editor = snapshot
            .provider_editor(None, "FutureDriver".into())
            .unwrap();
        editor.instance_id = "../invalid".into();
        assert!(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
        editor.instance_id = "experimental".into();
        editor.config_json = "[1,{\"future\":true}]".into();
        assert_eq!(
            saved_config(
                editor
                    .mutation(
                        snapshot.connected,
                        &snapshot.storage_scope,
                        snapshot.provider_settings.as_deref(),
                        false
                    )
                    .unwrap()
            )
            .config,
            serde_json::json!([1,{"future":true}])
        );
        assert!(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    true
                )
                .is_err()
        );
        editor.config_json = "{".into();
        assert!(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn clearing_options_and_json_fields_produce_typed_driver_configuration() {
        let snapshot = snapshot(
            "claudeAgent",
            serde_json::json!({"launchArgs":"--old","autoCompactWindow":"300000"}),
        );
        let mut editor = snapshot
            .provider_editor(Some("work".into()), "".into())
            .unwrap();
        for field in &mut editor.fields {
            match field.key.as_str() {
                "launchArgs" => field.value.clear(),
                "customModels" => field.value = "[\"custom-model\"]".into(),
                _ => {}
            }
        }
        let saved = saved_config(
            editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    false,
                )
                .unwrap(),
        );
        assert_eq!(
            saved.config,
            serde_json::json!({"autoCompactWindow":"300000","customModels":["custom-model"]})
        );
        let field = editor
            .fields
            .iter()
            .position(|field| field.key == "autoCompactWindow")
            .unwrap();
        for invalid in ["99999", "1000001", "+100000", "0100000", "word"] {
            editor.fields[field].value = invalid.into();
            assert!(
                editor
                    .mutation(
                        snapshot.connected,
                        &snapshot.storage_scope,
                        snapshot.provider_settings.as_deref(),
                        false
                    )
                    .is_err()
            );
        }
        for valid in ["100000", "1000000", ""] {
            editor.fields[field].value = valid.into();
            assert!(
                editor
                    .mutation(
                        snapshot.connected,
                        &snapshot.storage_scope,
                        snapshot.provider_settings.as_deref(),
                        false
                    )
                    .is_ok()
            );
        }
    }

    proptest! {
        #[test]
        fn label_edits_never_lose_unrecognized_payloads(payload in prop::collection::vec(any::<i64>(), 0..40), name in "[A-Za-z]{1,30}") {
            let snapshot = snapshot("codex", serde_json::json!({"future":payload}));
            let mut editor = snapshot.provider_editor(Some("work".into()), "".into()).unwrap();
            editor.display_name = name.clone();
            let saved = saved_config(editor.mutation(snapshot.connected, &snapshot.storage_scope, snapshot.provider_settings.as_deref(), false).unwrap());
            prop_assert_eq!(saved.config, snapshot.provider_settings.as_ref().unwrap().instances[0].config.config.clone());
            prop_assert_eq!(saved.display_name, Some(name));
        }
    }
}
