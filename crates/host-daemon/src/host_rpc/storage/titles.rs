//! Lists read committed headers, never a live transcript overlay.
use super::*;
use agent_protocol::{
    models::Thread,
    search::{Query, TitleSubject},
};
use std::collections::{BTreeMap, BTreeSet};

impl Storage {
    pub(in crate::host_rpc) fn search_threads(
        &self,
        search: &str,
        limit: usize,
    ) -> Result<(Vec<Thread>, bool, Vec<agent_protocol::session::SessionRef>), String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let snapshot = shell::snapshot(&tx)?;
            let projects: BTreeMap<_, _> = snapshot
                .projects
                .iter()
                .map(|p| (p.id.as_str(), p.title.as_str()))
                .collect();
            let candidates: Vec<_> = snapshot
                .threads
                .iter()
                .filter(|shell| {
                    let t = &shell.thread;
                    agent_protocol::search::visible_thread(
                        t.lineage.relationship_to_parent.as_deref(),
                        t.lineage.side_chat,
                        t.deleted_at.is_some(),
                        t.archived_at.is_some(),
                        t.project_id.is_some(),
                        t.project_id
                            .as_deref()
                            .is_some_and(|id| projects.contains_key(id)),
                    )
                })
                .collect();
            // The Bex list RPC also serves an empty-query catalog. This still
            // reads only canonical metadata; an empty normalized search is not
            // a request to discover provider sessions.
            let (selected, more, message_ids) = if search.trim().is_empty() {
                let mut selected: Vec<_> = (0..candidates.len()).collect();
                selected.sort_by(|&a, &b| {
                    let at = |i: usize| {
                        chrono::DateTime::parse_from_rfc3339(&candidates[i].thread.updated_at).ok()
                    };
                    at(b)
                        .cmp(&at(a))
                        .then_with(|| candidates[a].thread.id.cmp(&candidates[b].thread.id))
                });
                let more = selected.len() > limit;
                selected.truncate(limit);
                (selected, more, Vec::new())
            } else {
                let query = Query::new(search);
                let now = chrono::DateTime::parse_from_rfc3339(&projects::timestamp())
                    .map_err(failure)?
                    .timestamp_millis();
                let subjects: Vec<_> = candidates
                    .iter()
                    .map(|shell| TitleSubject {
                        id: &shell.thread.id,
                        title: &shell.thread.title,
                        branch: shell.thread.branch.as_deref(),
                        project: shell
                            .thread
                            .project_id
                            .as_deref()
                            .and_then(|id| projects.get(id).copied()),
                        activity_at_ms: chrono::DateTime::parse_from_rfc3339(
                            shell
                                .latest_user_authored_message_at
                                .as_deref()
                                .unwrap_or(&shell.thread.created_at),
                        )
                        .ok()
                        .map(|at| at.timestamp_millis()),
                    })
                    .collect();
                let titles = query.rank_titles(&subjects, now);
                let shown: BTreeSet<_> =
                    titles.iter().take(limit).map(|&i| subjects[i].id).collect();
                let eligible: BTreeSet<_> = subjects.iter().map(|s| s.id).collect();
                let indexed: BTreeMap<_, _> = subjects
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        (
                            s.id,
                            (
                                i,
                                agent_protocol::search::usage_boost(s.activity_at_ms, now),
                            ),
                        )
                    })
                    .collect();
                let mut messages = message_search::matches(&tx, &query, &eligible, &shown)?;
                messages.sort_by(|(left, a), (right, b)| {
                    let score = |id: &str, hits: &message_search::MessageHit| {
                        (1.0 + (1.0 + hits.hits as f64).log2()) * indexed[id].1
                    };
                    (b.quality >= 0.8)
                        .cmp(&(a.quality >= 0.8))
                        .then_with(|| b.quality.total_cmp(&a.quality))
                        .then_with(|| score(right, b).total_cmp(&score(left, a)))
                        .then_with(|| b.created_at.cmp(&a.created_at))
                });
                let more = titles.len() + messages.len() > limit;
                messages.truncate(50);
                let slots = messages.len().min(limit / 5);
                let selected: Vec<_> = titles
                    .into_iter()
                    .take(limit.saturating_sub(slots))
                    .chain(
                        messages
                            .iter()
                            .filter_map(|(id, _)| indexed.get(id.as_str()).map(|(i, _)| *i)),
                    )
                    .take(limit)
                    .collect();
                let message_ids = selected
                    .iter()
                    .filter(|&&i| !shown.contains(subjects[i].id))
                    .map(|&i| {
                        agent_protocol::orchestration::shell::public_session_ref(subjects[i].id)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (selected, more, message_ids)
            };
            let result = selected
                .into_iter()
                .map(|index| summary(&tx, candidates[index]))
                .collect::<Result<Vec<_>, _>>()?;
            tx.commit().map_err(failure)?;
            Ok((result, more, message_ids))
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
    let root=shell.thread.project_id.as_deref().map(|id|connection.query_row("SELECT workspace_root FROM projection_projects WHERE project_id=?1 AND deleted_at IS NULL",[id],|row|row.get::<_,String>(0)).optional().map_err(failure)).transpose()?.flatten();
    agent_protocol::orchestration::shell::summary(shell, root.as_deref())
}
