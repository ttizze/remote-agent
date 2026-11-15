//! Task activity shared with every client, Live Activities and APNs.
use super::*;

impl HostRpcService {
    pub(super) fn register_live_activity(
        &self,
        session: SessionId,
        params: &agent_protocol::live_activity::RegisterLiveActivity,
    ) -> Result<agent_protocol::live_activity::LiveActivityRegistration, Failure> {
        params
            .validate()
            .map_err(|error| Failure::new("invalid_params", error))?;
        let enabled = self.inner.router.apns().is_some();
        if enabled {
            self.inner
                .router
                .register_live_activity(session, params)
                .map_err(|error| Failure::new("live_activity_failed", error))?;
        }
        Ok(agent_protocol::live_activity::LiveActivityRegistration { enabled })
    }

    pub(super) fn unregister_live_activity(
        &self,
        session: SessionId,
        params: &agent_protocol::live_activity::UnregisterLiveActivity,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let principal = self
            .inner
            .router
            .principal(session)
            .map_err(|error| Failure::new("connection_closed", error))?;
        if let Some(apns) = self.inner.router.apns() {
            apns.unregister(&principal, &params.activity_id);
        }
        Ok(agent_protocol::models::Empty {})
    }

    pub async fn enable_apns(
        &self,
        path: &std::path::Path,
        host_name: &str,
        host_id: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .router
            .set_apns(crate::apns::Apns::load(path, host_name, host_id).await?);
        Ok(())
    }
}

pub(super) fn incomplete() -> Failure {
    Failure::new(
        "task_activity_incomplete",
        "実行中のタスクをすべて確認できません。時間をおいて再試行してください。",
    )
}

#[cfg(test)]
mod tests;
