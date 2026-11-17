//! Search and list metadata read canonical storage; providers own only native discovery.
use super::*;
use agent_protocol::conversation::records;

impl HostRpcService {
    pub(in crate::host_rpc) async fn project_snapshot(
        &self,
    ) -> Result<crate::projects::state::Snapshot, Failure> {
        self.inner
            .projects
            .load()
            .await
            .map_err(|error| Failure::new("project_state_unavailable", error))
    }
    pub(super) async fn search_list(
        &self,
        request: &agent_protocol::conversation::records::SearchList,
    ) -> Result<agent_protocol::models::ThreadList, Failure> {
        let limit = request
            .limit
            .unwrap_or(records::DEFAULT_SEARCH_LIMIT)
            .max(1) as usize;
        let snapshot = self.project_snapshot().await?;
        let (mut data, has_more, message_matches) = self
            .inner
            .conversation
            .storage
            .search_threads(&request.term, limit)
            .map_err(super::super::runtime::storage_failure)?;
        for thread in &mut data {
            let capabilities = thread.capabilities.unwrap_or_default();
            describe_thread(thread, capabilities, &snapshot);
        }
        let errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let projects = snapshot
            .projects
            .into_iter()
            .filter(|project| {
                request.term.trim().is_empty()
                    || data
                        .iter()
                        .any(|thread| thread.project_id.as_deref() == Some(project.id.as_str()))
            })
            .collect();
        Ok(agent_protocol::models::ThreadList {
            limit: limit as u32,
            data,
            message_matches,
            projects,
            has_more,
            provider_errors: (!errors.is_empty()).then_some(errors),
            ..Default::default()
        })
    }
    pub(super) async fn list_decorations(
        &self,
        params: &op::ReadListDecorations,
    ) -> Result<op::ListDecorations, Failure> {
        let projects = params.projects.clone();
        let icons = tokio::task::spawn_blocking(move || {
            projects
                .into_iter()
                .map(|(id, roots)| {
                    let icon = if id == agent_protocol::models::SCRATCH_PROJECT_ID {
                        crate::projects::icons::scratch()
                    } else {
                        roots.iter().find_map(|root| {
                            crate::projects::icons::resolve(std::path::Path::new(root))
                        })
                    };
                    (id, icon)
                })
                .collect()
        });
        let (icons, changes) = tokio::join!(
            icons,
            crate::workspace_review::directory_changes(
                params
                    .threads
                    .iter()
                    .map(|(_, cwd, _)| cwd.clone())
                    .collect()
            )
        );
        let icons = icons.map_err(|error| Failure::new("project_icons_unavailable", error))?;
        let changes = changes.map_err(|error| Failure::new("workspace_review_failed", error))?;
        Ok(op::ListDecorations {
            icons,
            statuses: params
                .threads
                .iter()
                .map(|(id, cwd, _)| (id.clone(), changes.get(cwd).copied()))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests;
