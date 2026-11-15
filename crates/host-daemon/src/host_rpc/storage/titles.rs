//! Lists read committed headers, never a live transcript overlay.
use super::*;
use agent_protocol::models::Thread;

impl Storage {
    pub(in crate::host_rpc) fn summaries(
        &self,
        search: &str,
        loaded_only: bool,
    ) -> Result<Vec<Thread>, String> {
        let search = search.trim().to_lowercase();
        self.with(|connection| {
            let snapshot = shell::snapshot(connection)?;
            snapshot
                .threads
                .into_iter()
                .filter(|thread| {
                    !loaded_only
                        || thread.thread.history_origin.is_some()
                        || thread.thread.created_by == agent_protocol::orchestration::Actor::User
                        || thread.item_count > 0
                        || thread.latest_run_id.is_some()
                })
                .filter(|thread| thread.thread.title.to_lowercase().contains(&search))
                .map(|thread| summary(connection, thread.as_ref()))
                .collect()
        })
    }

    /// A superseded or deleted native source cannot become a second result.
    pub(in crate::host_rpc) fn native_summary(
        &self,
        native: Thread,
    ) -> Result<Option<Thread>, String> {
        let Some(id) = native.id.clone() else {
            return Ok(Some(records::compact(native)));
        };
        self.with(|connection| {
            let Some(owner) = native_bindings::owner(connection, &id)? else {
                return Ok(Some(records::compact(native)));
            };
            let Some(thread) = projections::read::<agent_protocol::orchestration::Thread>(
                connection,
                &owner.application_id(),
            )?
            .filter(|thread| thread.deleted_at.is_none()) else {
                return Ok(None);
            };
            let active = thread
                .active_provider_thread_id
                .as_deref()
                .map(|id| {
                    projections::read::<agent_protocol::orchestration::ProviderThread>(
                        connection, id,
                    )
                })
                .transpose()?
                .flatten();
            let selected = active.is_some_and(|source| {
                source.app_thread_id.as_deref() == Some(&thread.id)
                    && source.provider_instance_id == thread.provider_instance_id
                    && id.provider.is_some_and(|kind| {
                        source.provider_instance_id == kind.key() && source.driver == kind.driver()
                    })
                    && source.native_thread_ref.as_ref().is_some_and(|reference| {
                        reference.driver == source.driver
                            && reference.native_id.as_ref() == Some(&id.id)
                    })
            });
            if !selected {
                return Ok(None);
            }
            summary(connection, &shell::thread(connection, thread)?).map(Some)
        })
    }
}

fn summary(
    connection: &Connection,
    shell: &agent_protocol::orchestration::shell::ThreadShell,
) -> Result<Thread, String> {
    let root=shell.thread.project_id.as_deref().map(|id|connection.query_row(
        "SELECT workspace_root FROM projection_projects WHERE project_id=?1 AND deleted_at IS NULL",[id],|row|row.get::<_,String>(0)).optional().map_err(failure)).transpose()?.flatten();
    agent_protocol::orchestration::shell::summary(shell, root.as_deref())
}
