use super::*;
use crate::presentation::provider_settings::ProviderEditor;
pub use agent_protocol::providers::ReadProviderSettings;
use agent_protocol::providers::UpdateProviderInstance;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApplyProviderEdit {
    pub editor: ProviderEditor,
    pub remove: bool,
}
impl Operation for ApplyProviderEdit {
    type Input = UpdateProviderInstance;
    type Output = agent_protocol::providers::ProviderSettings;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProviderSettings)
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Control
    }
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(UpdateProviderInstance {
            operation_id: uuid::Uuid::new_v4(),
            revision: self.editor.revision,
            mutation: self
                .editor
                .mutation(
                    snapshot.connected,
                    &snapshot.storage_scope,
                    snapshot.provider_settings.as_deref(),
                    self.remove,
                )
                .map_err(PeerError::InvalidMessage)?,
        })
    }
    async fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context.call(&input).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if !apply_settings(snapshot, output) {
            return Vec::new();
        }
        snapshot.permission_settings = None;
        snapshot.composer_catalog = None;
        let mut effects = vec![
            Effect::continuation(LoadModels {}),
            Effect::continuation(ListAccounts {}),
        ];
        if let Some(session) = &snapshot.navigation.thread_id {
            effects.push(Effect::continuation(ReadThread::new(session.clone())));
        }
        effects
    }
}

fn apply_settings(
    snapshot: &mut Snapshot,
    settings: agent_protocol::providers::ProviderSettings,
) -> bool {
    if snapshot
        .provider_settings
        .as_ref()
        .is_none_or(|current| settings.revision >= current.revision)
    {
        snapshot.provider_settings = Some(Arc::new(settings));
        true
    } else {
        false
    }
}

impl Operation for ReadProviderSettings {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProviderSettings)
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        apply_settings(snapshot, output);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::providers::ProviderSettings;

    #[test]
    fn late_settings_receipts_cannot_overwrite_newer_settings_or_clear_caches() {
        let mut snapshot = Snapshot::default();
        assert!(apply_settings(
            &mut snapshot,
            ProviderSettings {
                revision: 3,
                instances: Vec::new()
            }
        ));
        let retained = snapshot.provider_settings.clone().unwrap();
        let permissions = Arc::new(PermissionSettingsState {
            instance_id: "codex".parse().unwrap(),
            result: Some(Err("current permission response".into())),
        });
        let catalog = Arc::new(agent_protocol::composer::ComposerCatalog {
            cwd: "/current".into(),
            ..Default::default()
        });
        snapshot.permission_settings = Some(permissions.clone());
        snapshot.composer_catalog = Some(catalog.clone());
        let request = ApplyProviderEdit {
            editor: snapshot.provider_editor(None, "codex".into()).unwrap(),
            remove: false,
        };
        assert!(
            request
                .apply(
                    &mut snapshot,
                    ProviderSettings {
                        revision: 1,
                        instances: Vec::new()
                    }
                )
                .is_empty()
        );
        assert!(Arc::ptr_eq(
            snapshot.provider_settings.as_ref().unwrap(),
            &retained
        ));
        assert!(Arc::ptr_eq(
            snapshot.permission_settings.as_ref().unwrap(),
            &permissions
        ));
        assert!(Arc::ptr_eq(
            snapshot.composer_catalog.as_ref().unwrap(),
            &catalog
        ));
        assert!(apply_settings(
            &mut snapshot,
            ProviderSettings {
                revision: 4,
                instances: Vec::new()
            }
        ));
        assert_eq!(snapshot.provider_settings.as_ref().unwrap().revision, 4);
    }
}
