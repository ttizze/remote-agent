use super::*;
pub use agent_protocol::permissions::{ReadPermissionSettings, UpdatePermissionSettings};
use agent_protocol::{permissions::PermissionSettings, session::ProviderKind};

/// A connection-local read cache. The provider's file is the only saved setting.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionSettingsState {
    pub provider: ProviderKind,
    /// None while fetching; an error never masquerades as saved settings.
    pub result: Option<Result<PermissionSettings, String>>,
}

macro_rules! permission_operation {
    ($ty:ty) => {
        impl Operation for $ty {
            type Output = Result<PermissionSettings, String>;
            fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
                snapshot.permission_settings = Some(Arc::new(PermissionSettingsState {
                    provider: self.provider,
                    result: None,
                }));
                Ok(())
            }
            async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
                Ok(context.call(self).await.map_err(|error| error.to_string()))
            }
            fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
                if snapshot
                    .permission_settings
                    .as_ref()
                    .is_some_and(|state| state.provider == self.provider)
                {
                    snapshot.permission_settings = Some(Arc::new(PermissionSettingsState {
                        provider: self.provider,
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
