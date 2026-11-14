//! Authenticated APNs registration. Task facts come from the list replica.
use super::*;

impl SessionRouter {
    pub(in crate::host_rpc) fn set_apns(&self, apns: Option<Arc<crate::apns::Apns>>) {
        *lock_state(&self.activity) = apns;
    }
    pub(in crate::host_rpc) fn apns(&self) -> Option<Arc<crate::apns::Apns>> {
        lock_state(&self.activity).clone()
    }
    pub(in crate::host_rpc) fn register_live_activity(
        &self,
        connection: SessionId,
        params: &agent_protocol::live_activity::RegisterLiveActivity,
    ) -> Result<(), String> {
        let activity = lock_state(&self.activity);
        let principal = self.principal(connection)?;
        activity
            .as_ref()
            .ok_or("APNs is unavailable")?
            .register(&principal, params)
    }
}
