//! Physical preparation, provider discovery and history excerpts.
use super::*;
use agent_protocol::session::SessionRef;

impl HostRpcService {
    pub(in crate::host_rpc) fn creation_agent(
        &self,
        provider: ProviderKind,
        model: Option<&agent_protocol::models::ModelRef>,
    ) -> Result<Arc<dyn Agent>, Failure> {
        self.require_provider_enabled(provider)?;
        if model.is_some_and(|model| model.provider != provider || model.id.trim().is_empty()) {
            return Err(Failure::new(
                "provider_mismatch",
                "model must name a model of the requested provider",
            ));
        }
        let agent = self.inner.providers.get(provider)?;
        agent.validate_create()?;
        Ok(agent)
    }

    pub(super) async fn list_agents(
        &self,
        params: &op::ListAgents,
    ) -> Result<Vec<agent_protocol::models::AgentObservation>, Failure> {
        let native = self
            .inner
            .conversation
            .storage
            .active_source(&params.thread_id)
            .map_err(super::super::runtime::storage_failure)?
            .unwrap_or_else(|| params.thread_id.clone());
        if native.provider.is_none() {
            return Ok(Vec::new());
        }
        let agent = self.inner.providers.discovery(
            native
                .native_provider()
                .map_err(|error| Failure::new("invalid_session", error))?,
        )?;
        let deadline = tokio::time::Instant::now() + SESSION_LIST_TIMEOUT;
        let mut threads = session_pages(
            agent.as_ref(),
            "",
            SessionListScope::Descendants(&native.id),
            100,
            100,
            Some(deadline),
            None,
        )
        .map_ok(|page| futures_util::stream::iter(page.into_iter().map(Ok::<_, Failure>)))
        .try_flatten()
        .boxed();
        let mut agents = Vec::new();
        while let Some(summary) = threads.try_next().await? {
            let Some(thread) = self
                .inner
                .conversation
                .storage
                .native_summary(summary.thread)
                .map_err(super::super::runtime::storage_failure)?
            else {
                continue;
            };
            if let (Some(id), Some(parent_id)) = (thread.id, thread.parent_id) {
                agents.push(agent_protocol::models::AgentObservation {
                    id,
                    parent_id,
                    name: thread.name,
                    status: thread.status,
                });
            }
        }
        Ok(agents)
    }

    pub fn set_history_scope(&self, directory: std::path::PathBuf, isolated: bool) {
        let _ = self.inner.history_scope.set((directory, isolated));
    }
    pub(in crate::host_rpc) fn history_command(&self, root: &SessionRef) -> String {
        let program = std::env::current_exe().unwrap_or_else(|_| "host-daemon".into());
        let quote = |text: &str| {
            format!(
                "'{}'",
                text.replace('\'', if cfg!(windows) { "''" } else { "'\\''" })
            )
        };
        let scope = self
            .inner
            .history_scope
            .get()
            .map(|(path, isolated)| {
                format!(
                    "{} --state-dir {}",
                    if *isolated { " --isolated" } else { "" },
                    quote(&path.to_string_lossy())
                )
            })
            .unwrap_or_default();
        format!(
            "{}{}{} history {} --offset 0 --limit 20\nContinue with --offset next_offset, or --offset position --limit 1 --text-offset next_text_offset to read a long item.",
            if cfg!(windows) { "& " } else { "" },
            quote(&program.to_string_lossy()),
            scope,
            quote(&root.to_string())
        )
    }
    pub(super) async fn read_handoff_history(
        &self,
        params: &agent_protocol::handoff::ReadHistory,
    ) -> Result<agent_protocol::handoff::Page, Failure> {
        super::super::runtime::Runtime::await_import(self, &params.session).await?;
        let (history, _) =
            super::super::runtime::Runtime::handoff_history(self, &params.session, 0).await?;
        let messages = history
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| agent_protocol::handoff::Message::from_turn(turn));
        let mut page = agent_protocol::handoff::page(
            messages,
            params.offset,
            params.limit,
            params.text_offset,
        );
        page.history_read_state = history.thread.history_read_state;
        Ok(page)
    }
}

pub(super) fn describe_thread(
    thread: &mut Thread,
    capabilities: agent_protocol::session::Capabilities,
    projects: &crate::projects::state::Snapshot,
) {
    thread.project_id = projects.project_membership(thread.cwd.as_deref());
    thread.capabilities = Some(capabilities);
}
