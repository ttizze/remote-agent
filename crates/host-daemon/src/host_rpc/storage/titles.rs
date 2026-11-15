//! Lists read committed headers, never a live transcript overlay.
use super::*;
use agent_protocol::models::Thread;

impl Storage {
    pub(in crate::host_rpc) fn search_threads(&self, search: &str) -> Result<Vec<Thread>, String> {
        let terms: Vec<_> = search.split_whitespace().map(str::to_lowercase).collect();
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let snapshot = shell::snapshot(&tx)?;
            let messages = matching_message_threads(&tx, &terms)?;
            let projects = &snapshot.projects;
            let result = snapshot
                .threads
                .into_iter()
                .filter(|shell| {
                    let thread = &shell.thread;
                    if thread.lineage.relationship_to_parent.as_deref() == Some("subagent")
                        || thread.lineage.side_chat == Some(true)
                    {
                        return false;
                    }
                    let project = thread
                        .project_id
                        .as_deref()
                        .and_then(|id| projects.iter().find(|project| project.id == id));
                    if thread.project_id.is_some() && project.is_none() {
                        return false;
                    }
                    messages.contains(&thread.id)
                        || contains_terms(
                            &format!(
                                "{} {} {}",
                                thread.title,
                                thread.branch.as_deref().unwrap_or_default(),
                                project.map_or("", |project| project.title.as_str())
                            ),
                            &terms,
                        )
                })
                .map(|shell| summary(&tx, &shell))
                .collect::<Result<Vec<_>, _>>()?;
            tx.commit().map_err(failure)?;
            Ok(result)
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

fn contains_terms(text: &str, terms: &[String]) -> bool {
    let text = text.to_lowercase();
    terms.iter().all(|term| text.contains(term))
}

fn matching_message_threads(
    connection: &Connection,
    terms: &[String],
) -> Result<std::collections::BTreeSet<String>, String> {
    if terms.is_empty() {
        return Ok(Default::default());
    }
    // ThreadSearch.messageHitsQuery bounds a scan to the latest 20,000 messages.
    // Bex keeps message text in turn items; tools do not consume that budget.
    // The role/streaming rules follow messageSearchIndex.indexedMessageCondition.
    let mut query = connection.prepare("SELECT i.thread_id, json_extract(i.payload_json,'$.text')
        FROM (SELECT * FROM orchestration_v2_projection_turn_items
            WHERE type IN ('user_message','assistant_message') ORDER BY rowid DESC LIMIT 20000) i
        LEFT JOIN orchestration_v2_projection_runs r ON r.thread_id=i.thread_id AND r.run_id=i.run_id
        WHERE (i.type='assistant_message' OR i.created_by='user')
          AND coalesce(json_extract(i.payload_json,'$.streaming'),0)=0
          AND coalesce(r.status,'')!='rolled_back'
          AND NOT(i.input_intent IS 'queued_turn' AND r.status IN ('queued','cancelled'))
          AND coalesce(json_extract(i.payload_json,'$.messageId'),'') NOT LIKE 'async-answer:%'").map_err(failure)?;
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(failure)?;
    let mut found = std::collections::BTreeSet::new();
    for row in rows {
        let (thread, text) = row.map_err(failure)?;
        if text
            .as_deref()
            .is_some_and(|text| contains_terms(text, terms))
        {
            found.insert(thread);
        }
    }
    Ok(found)
}
