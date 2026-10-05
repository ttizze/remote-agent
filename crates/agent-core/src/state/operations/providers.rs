use super::*;
pub use agent_protocol::providers::{ReadProviderSettings, UpdateProviderInstance};

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

impl Operation for UpdateProviderInstance {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProviderSettings)
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Control
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if !apply_settings(snapshot, output) {
            return Vec::new();
        }
        snapshot.permission_settings = None;
        snapshot.composer_catalog = None;
        vec![
            Effect::continuation(LoadModels {}),
            Effect::continuation(ListAccounts {}),
        ]
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
        let request = UpdateProviderInstance {
            operation_id: uuid::Uuid::new_v4(),
            revision: 0,
            mutation: agent_protocol::providers::ProviderMutation::Remove {
                instance_id: "work".parse().unwrap(),
            },
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
