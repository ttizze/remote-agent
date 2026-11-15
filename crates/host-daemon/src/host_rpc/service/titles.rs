//! Search queries native catalogs; ordinary list reads come from storage.
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
        let owned = self
            .inner
            .conversation
            .storage
            .active_sources()
            .map_err(super::super::runtime::storage_failure)?;
        let scope = SessionListScope::WithSubagents(&owned);
        let providers = self.inner.providers.all();
        let snapshot = self.project_snapshot().await?;
        let listings = futures_util::future::join_all(providers.into_iter().map(
            |(provider, agent)| async move {
                let mut pages = Box::pin(session_pages(
                    agent.as_ref(),
                    &request.term,
                    scope,
                    100,
                    100,
                    Some(tokio::time::Instant::now() + SESSION_LIST_TIMEOUT),
                    None,
                ));
                let mut rows = std::collections::BTreeMap::new();
                let mut error = None;
                'pages: loop {
                    let page = match pages.try_next().await {
                        Ok(Some(page)) => page,
                        Ok(None) => break,
                        Err(failure) => {
                            error = Some(failure);
                            break;
                        }
                    };
                    let storage = self.inner.conversation.storage.clone();
                    let page = tokio::task::spawn_blocking(move || {
                        let mut rows = Vec::new();
                        for summary in page {
                            if let Some(mut thread) = storage.native_summary(summary.thread)? {
                                thread.git_branch = summary.branch.or(thread.git_branch);
                                rows.push(thread);
                            }
                        }
                        Ok::<_, String>(rows)
                    })
                    .await
                    .map_err(|error| super::super::runtime::storage_failure(error.to_string()))?
                    .map_err(super::super::runtime::storage_failure)?;
                    for mut thread in page {
                        let Some(id) = thread.id.clone() else {
                            continue;
                        };
                        thread.capabilities =
                            Some(agent_protocol::conversation::records::capabilities(
                                thread.capabilities.unwrap_or_else(|| agent.capabilities()),
                            ));
                        rows.insert(id, thread);
                        if rows.len() > limit {
                            break 'pages;
                        }
                    }
                }
                Ok::<_, Failure>((provider, rows, error))
            },
        ))
        .await;
        let saved = self
            .inner
            .conversation
            .storage
            .summaries(&request.term, false)
            .map_err(super::super::runtime::storage_failure)?;
        let mut rows = std::collections::BTreeMap::new();
        let mut errors = self
            .provider_errors()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut successful = !saved.is_empty();
        for listing in listings {
            let (provider, native, error) = listing?;
            successful |= !native.is_empty();
            rows.extend(native);
            if let Some(error) = error {
                errors.insert(provider.key().into(), serde_json::to_value(error)?);
            } else {
                successful = true;
            }
        }
        for thread in saved {
            if let Some(id) = thread.id.clone() {
                rows.insert(id, thread);
            }
        }
        if !successful && !errors.is_empty() {
            return Err(Failure::new(
                "sessions_unavailable",
                serde_json::to_value(errors)?,
            ));
        }
        let mut data = rows.into_values().collect::<Vec<_>>();
        for thread in &mut data {
            if let Some((_, error)) = self.inner.providers.startup_errors().find(|(provider, _)| {
                Some(*provider)
                    == thread
                        .active_provider
                        .or_else(|| thread.id.as_ref().and_then(|id| id.provider))
            }) {
                *thread = agent_protocol::conversation::records::unavailable(
                    thread.clone(),
                    &error.message,
                );
            }
            let capabilities = thread.capabilities.unwrap_or_default();
            describe_thread(thread, capabilities, &snapshot);
        }
        data.sort_by(|a, b| {
            b.updated_at
                .unwrap_or_default()
                .total_cmp(&a.updated_at.unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        let has_more = data.len() > limit;
        data.truncate(limit);
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
                    let icon = roots.iter().find_map(|root| {
                        crate::projects::icons::resolve(std::path::Path::new(root))
                    });
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
