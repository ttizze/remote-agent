use super::*;
pub use agent_protocol::permissions::{ReadPermissionSettings, UpdatePermissionSettings};
use agent_protocol::{permissions::PermissionSettings, session::ProviderInstanceId};

/// A connection-local read cache. The provider's file is the only saved setting.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionSettingsState {
    pub instance_id: ProviderInstanceId,
    /// None while fetching; an error never masquerades as saved settings.
    pub result: Option<Result<PermissionSettings, String>>,
}

macro_rules! permission_operation {
    ($ty:ty) => {
        impl Operation for $ty {
            fn key(&self) -> Option<OperationKey> {
                Some(OperationKey::Permissions {
                    provider: self.instance_id.clone(),
                })
            }
            no_input!();
            type Output = Result<PermissionSettings, String>;
            fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
                snapshot.permission_settings = Some(Arc::new(PermissionSettingsState {
                    instance_id: self.instance_id.clone(),
                    result: None,
                }));
                Ok(())
            }
            async fn run(
                &self,
                _: Self::Input,
                context: &mut Execution<'_>,
            ) -> Result<Self::Output, PeerError> {
                Ok(context.call(self).await.map_err(|error| error.to_string()))
            }
            fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
                if snapshot
                    .permission_settings
                    .as_ref()
                    .is_some_and(|state| state.instance_id == self.instance_id)
                {
                    snapshot.permission_settings = Some(Arc::new(PermissionSettingsState {
                        instance_id: self.instance_id,
                        result: Some(output),
                    }));
                }
                Vec::new()
            }
        }
    };
}
permission_operation!(ReadPermissionSettings);
permission_operation!(UpdatePermissionSettings);
