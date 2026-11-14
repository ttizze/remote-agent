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

    /// Updating must inspect provider-owned processes as well as saved Bex
    /// executions. This admission read never publishes client list state.
    pub(super) async fn update_task_snapshot(
        &self,
    ) -> Result<agent_protocol::live_activity::TaskActivityState, Failure> {
        let agents = self
            .inner
            .providers
            .all()
            .into_iter()
            .filter(|(_, agent)| agent.requires_restart_snapshot())
            .collect::<Vec<_>>();
        let listings = futures_util::future::join_all(agents.into_iter().map(
            |(provider, agent)| async move {
                (
                    provider,
                    tokio::time::timeout(
                        SESSION_LIST_TIMEOUT,
                        session_pages(
                            agent.as_ref(),
                            "",
                            SessionListScope::Loaded,
                            100,
                            100,
                            None,
                            None,
                        )
                        .try_collect::<Vec<_>>(),
                    )
                    .await,
                )
            },
        ))
        .await;
        let mut headers: std::collections::BTreeMap<_, _> = self
            .inner
            .conversation
            .storage
            .headers(true)
            .map_err(super::super::runtime::storage_failure)?
            .into_iter()
            .collect();
        let mut observed = std::collections::HashSet::new();
        let mut inspected = std::collections::HashSet::new();
        for (provider, result) in listings {
            let pages = result.ok().and_then(Result::ok).ok_or_else(incomplete)?;
            inspected.insert(provider);
            for summary in pages.into_iter().flatten() {
                let native_status = summary.thread.status;
                if summary.thread.list_stale == Some(true) {
                    return Err(incomplete());
                }
                let Some(thread) = self
                    .inner
                    .conversation
                    .storage
                    .native_summary(summary.thread)
                    .map_err(super::super::runtime::storage_failure)?
                else {
                    continue;
                };
                let Some(id) = thread.id.clone() else {
                    continue;
                };
                let header = headers.entry(id.clone()).or_insert_with(|| {
                    super::super::storage::history::header(
                        &agent_protocol::models::ThreadResponse {
                            thread,
                            ..Default::default()
                        },
                        super::super::runtime::now(),
                    )
                });
                // A live source cannot be hidden by another binding's idle list entry.
                let first = observed.insert(id);
                if header.details.activity.latest
                    != Some(agent_protocol::models::TurnStatus::Running)
                    && (first
                        || !agent_protocol::models::task_active(
                            None,
                            header.details.activity.status,
                        ))
                {
                    header.details.activity.status = native_status;
                }
            }
        }
        for (id, header) in &mut headers {
            // A complete loaded-list is evidence that absent sources have no
            // provider-owned work. This is local to restart admission.
            if !observed.contains(id)
                && inspected.contains(&header.state.provider)
                && header.details.activity.latest
                    != Some(agent_protocol::models::TurnStatus::Running)
            {
                header.details.activity.status = agent_protocol::models::SessionStatus::Idle;
            }
        }
        Ok(agent_protocol::conversation::records::list_activity(
            headers.iter(),
        ))
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
