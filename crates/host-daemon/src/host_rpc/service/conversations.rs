use super::*;
use crate::host_rpc::conversations::{Binding, Conversation, Delivery, Handoff, inline_context};
use agent_protocol::{
    handoff,
    session::{HistoryPage, SessionRef},
};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

#[cfg(test)]
pub(super) mod tests;

// Browsing and excerpts may report missing providers. An import must establish
// complete source history before the Host can hand that transcript to an AI.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryUse {
    Read,
    Excerpt,
    /// A transcript copies messages and generated images in full.
    Import,
}

fn readable_history(state: Option<&agent_protocol::session::HistoryReadState>) -> bool {
    state.is_none_or(|state| {
        matches!(
            state.kind,
            agent_protocol::session::HistoryReadKind::Complete
                | agent_protocol::session::HistoryReadKind::Partial
        )
    })
}

impl HostRpcService {
    pub(super) async fn read_handoff_history(
        &self,
        params: &agent_protocol::handoff::ReadHistory,
    ) -> Result<agent_protocol::handoff::Page, Failure> {
        let history = if let Some(row) = self.inner.conversations.get(&params.session)? {
            self.conversation_history(&row, HistoryUse::Excerpt).await?
        } else if let Some(response) = self
            .inner
            .transcripts
            .history(&params.session)
            .map_err(transcripts::storage_failure)?
        {
            response
        } else {
            self.native_history(&params.session, HistoryUse::Excerpt)
                .await?
        };
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

    pub(super) async fn fork_conversation(
        &self,
        row: &Conversation,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<ThreadResponse, Failure> {
        let (fork, inherited) = self.branch(row, turn_id).await?;
        self.inner
            .transcripts
            .import(&fork.id, inherited)
            .map_err(transcripts::storage_failure)?;
        self.inner.conversations.save(fork.clone())?;
        self.publish_conversation(&fork).await
    }

    /// Returns a conversation to just before `params.turn_id`, as T3 Code's
    /// "Edit from here": its public identity stays while a native session
    /// holding only the earlier turns takes over.
    pub(super) async fn rewind_session(
        &self,
        target: &SessionRef,
        params: &op::RewindSession,
    ) -> Result<ThreadResponse, Failure> {
        let busy = || {
            Failure::new(
                "conversation_busy",
                "Stop the current work and clear the queue before rewinding.",
            )
        };
        if !self.inner.router.queue(target).inputs.is_empty() {
            return Err(busy());
        }
        let row = match self.inner.conversations.get(target)? {
            Some(row) => row,
            None => {
                Conversation::new(&self.native_history(target, HistoryUse::Read).await?.thread)?
            }
        };
        if row.startup.is_some() {
            return Err(busy());
        }
        let history = self.conversation_history(&row, HistoryUse::Import).await?;
        let turns = history.thread.turns.as_deref().unwrap_or_default();
        if turns
            .iter()
            .any(|turn| turn.status == agent_protocol::models::TurnStatus::Running)
        {
            return Err(busy());
        }
        let index = turns
            .iter()
            .rposition(|turn| turn.id == params.turn_id)
            .ok_or_else(|| Failure::new("invalid_turn", "Rewind target turn is unavailable."))?;
        let (mut rewound, kept) = match index.checked_sub(1) {
            Some(previous) => self.branch(&row, &turns[previous].id).await?,
            None => self.restart(&row, &history).await?,
        };
        rewound.id = row.id;
        rewound.name = row.name;
        rewound.preview = row.preview;
        self.inner
            .transcripts
            .replace(&rewound.id, kept)
            .map_err(transcripts::storage_failure)?;
        self.inner.conversations.save(rewound.clone())?;
        self.publish_conversation(&rewound).await
    }

    /// A new native session for a conversation rewound past its first turn.
    async fn restart(
        &self,
        row: &Conversation,
        history: &ThreadResponse,
    ) -> Result<(Conversation, ThreadResponse), Failure> {
        let agent = self.creation_agent(row.active.provider, None)?;
        let model = history.model.as_ref().map(|model| model.id.as_str());
        let response = self
            .native_thread(agent.as_ref(), |browser| {
                agent.create(&row.cwd, model, browser)
            })
            .await?;
        let mut restarted = Conversation::new(&response.thread)?;
        restarted.cover(0);
        let mut empty = response;
        empty.thread.turns = Some(Vec::new());
        Ok((restarted, empty))
    }

    async fn publish_conversation(&self, row: &Conversation) -> Result<ThreadResponse, Failure> {
        let page = Self::conversation_page(
            self.conversation_history(row, HistoryUse::Read).await?,
            None,
            5,
        )?;
        self.notify_thread(page.thread.clone()).await?;
        Ok(page)
    }

    /// An unsaved conversation continuing `row` after `turn_id`, and the
    /// history it inherits.
    async fn branch(
        &self,
        row: &Conversation,
        turn_id: &agent_protocol::ids::TurnId,
    ) -> Result<(Conversation, ThreadResponse), Failure> {
        use crate::host_rpc::conversations::Segment;
        let (native, turn) = row.source(turn_id)?;
        if !row.sources().any(|source| *source == native) {
            return Err(Failure::new(
                "invalid_turn",
                "The fork source is outside this conversation.",
            ));
        }
        self.require_provider_enabled(native.provider)?;
        let agent = self.inner.providers.get(native.provider)?;
        if !agent.capabilities().fork {
            return Err(Failure::new(
                "unsupported_operation",
                "The source provider cannot fork this response.",
            ));
        }
        let history = if let Some(history) = self
            .inner
            .transcripts
            .history(&row.id)
            .map_err(transcripts::storage_failure)?
        {
            history
        } else {
            // A native import can fork an earlier readable range without
            // depending on providers used only after that response.
            let history = self.native_history(&native, HistoryUse::Read).await?;
            let end = history
                .thread
                .turns
                .as_deref()
                .unwrap_or_default()
                .iter()
                .rposition(|candidate| candidate.id.as_str() == turn)
                .ok_or_else(|| Failure::new("invalid_turn", "Fork source turn is unavailable."))?
                + 1;
            let index = row
                .segments
                .iter()
                .rposition(|segment| {
                    segment.session == native
                        && segment.from < end
                        && segment.end.is_none_or(|boundary| end <= boundary)
                })
                .ok_or_else(|| {
                    Failure::new(
                        "invalid_turn",
                        "Fork source is outside the saved history range.",
                    )
                })?;
            let mut prefix = row.clone();
            prefix.active = native.clone();
            prefix.segments.truncate(index + 1);
            prefix.segments[index].end = Some(end);
            self.conversation_history(&prefix, HistoryUse::Import)
                .await?
        };
        let turns = history.thread.turns.as_deref().unwrap_or_default();
        let boundary = turns
            .iter()
            .rposition(|candidate| candidate.id == *turn_id)
            .ok_or_else(|| Failure::new("invalid_turn", "Fork source turn is unavailable."))?
            + 1;
        let response = self
            .native_thread(agent.as_ref(), |browser| {
                agent.fork(&native.id, &turn, browser)
            })
            .await?;
        let mut fork = Conversation::new(&response.thread)?;
        fork.segments.clear();
        let mut inherited = history;
        inherited.thread.turns.as_mut().unwrap().truncate(boundary);
        inherited.model = response.model;
        inherited.effort = response.effort;
        inherited.service_tier = response.service_tier;
        // Historical references keep their source runtime even when the fork's
        // first owned binding has a different native identity.
        for turn in inherited.thread.turns.iter_mut().flatten() {
            let turn = Arc::make_mut(turn);
            let (source, id) = row.source(&turn.id)?;
            if !fork
                .segments
                .iter()
                .any(|segment| segment.session == source)
            {
                fork.segments.push(Segment {
                    session: source.clone(),
                    from: 0,
                    end: Some(0),
                });
            }
            turn.id = id.into();
            for item in turn.items.iter_mut().flatten() {
                let item = Arc::make_mut(item);
                let (_, id) = row.source(&item.id)?;
                item.id = id.into();
            }
            *turn = fork.turn(&source, turn.clone());
        }
        fork.segments.push(Segment {
            session: fork.active.clone(),
            from: 0,
            end: None,
        });
        fork.cover(boundary);
        Ok((fork, inherited))
    }
    pub fn set_history_scope(&self, directory: std::path::PathBuf, isolated: bool) {
        let _ = self.inner.history_scope.set((directory, isolated));
    }
    /// Native paging is consumed on the Host. Never export just the client's
    /// five visible turns as if they were the entire conversation.
    pub(super) async fn native_history(
        &self,
        session: &SessionRef,
        usage: HistoryUse,
    ) -> Result<ThreadResponse, Failure> {
        let agent = self.inner.providers.get(session.provider)?;
        let mut response = agent.open(&session.id, 100, true).await?;
        if !readable_history(response.thread.history_read_state.as_ref()) {
            return Err(Failure::new(
                "incomplete_history",
                "The provider could not read the source history completely.",
            ));
        }
        if response.thread.id.as_ref() != Some(session) {
            return Err(Failure::new(
                "invalid_session",
                "native session identity changed",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        while let Some(cursor) = response.thread.history_cursor.clone() {
            if !seen.insert(cursor.clone()) {
                return Err(Failure::new("invalid_history", "history cursor repeated"));
            }
            agent
                .read_history(&session.id, &cursor, true)
                .await?
                .prepend_to(&mut response.thread);
        }
        if response.thread.history_has_more == Some(true) {
            return Err(Failure::new(
                "incomplete_history",
                "The provider did not supply complete history.",
            ));
        }
        if usage != HistoryUse::Read {
            for turn in response.thread.turns.iter_mut().flatten() {
                let turn = Arc::make_mut(turn);
                if turn.items_summary {
                    turn.items = Some(agent.read_turn_items(&session.id, &turn.id).await?);
                    turn.items_summary = false;
                }
                for item in turn.items.iter_mut().flatten() {
                    // Handoffs deliberately omit reasoning and native tool
                    // state. Their deferred bodies need not be recovered.
                    if item.is_deferred()
                        && (transcripts::content(item.body())
                            || (usage != HistoryUse::Import
                                && matches!(
                                    item.body(),
                                    agent_protocol::items::ItemBody::CommandExecution { .. }
                                        | agent_protocol::items::ItemBody::FileChange { .. }
                                        | agent_protocol::items::ItemBody::Plan { .. }
                                )))
                    {
                        let output = match agent
                            .read_item(&op::ReadItem {
                                thread_id: session.clone(),
                                turn_id: turn.id.clone(),
                                item_id: item.id.clone(),
                            })
                            .await
                        {
                            Ok(output) if !output.item.is_deferred() => output,
                            // A transcript keeps an unreadable body deferred;
                            // reading the item later reports the provider's failure.
                            _ if usage == HistoryUse::Import => continue,
                            Ok(_) => {
                                return Err(Failure::new(
                                    "incomplete_history",
                                    "Historical item could not be read.",
                                ));
                            }
                            Err(error) => return Err(error),
                        };
                        *item = Arc::new(output.item);
                    }
                }
            }
        }
        Ok(response)
    }

    pub(super) fn conversation_capabilities(
        &self,
        active: ProviderKind,
        sources: impl Iterator<Item = ProviderKind>,
    ) -> agent_protocol::session::Capabilities {
        let capabilities = self
            .inner
            .providers
            .get(active)
            .and_then(|agent| {
                agent.availability()?;
                Ok(agent.capabilities())
            })
            .unwrap_or(agent_protocol::session::Capabilities {
                steer: false,
                model_change: false,
                fork: false,
                rename: false,
            });
        agent_protocol::session::Capabilities {
            rename: true,
            fork: sources.into_iter().any(|provider| {
                self.inner
                    .providers
                    .get(provider)
                    .is_ok_and(|agent| agent.availability().is_ok() && agent.capabilities().fork)
            }),
            ..capabilities
        }
    }

    pub(super) async fn conversation_history(
        &self,
        row: &Conversation,
        usage: HistoryUse,
    ) -> Result<ThreadResponse, Failure> {
        if row.startup.is_some() {
            return self.preparation_history(row).await;
        }
        if (usage != HistoryUse::Import
            || row.bindings.iter().any(|binding| binding.covered.is_some()))
            && let Some(response) = self
                .inner
                .transcripts
                .history(&row.id)
                .map_err(transcripts::storage_failure)?
        {
            let options = self
                .inner
                .model_selections
                .resolve(&row.active, None, None, None)?;
            return self.describe_conversation_history(row, response, options);
        }
        // Coverage is committed only after a complete import. Never rebuild a
        // Host-owned conversation from an older native import range if storage
        // disappears; that would silently discard turns accepted by the Host.
        if row.bindings.iter().any(|binding| binding.covered.is_some()) {
            return Err(Failure::new(
                "incomplete_history",
                "The Host conversation transcript is unavailable.",
            ));
        }
        let mut sources = BTreeMap::new();
        for segment in &row.segments {
            let Entry::Vacant(slot) = sources.entry(segment.session.clone()) else {
                continue;
            };
            let agent = match self.inner.providers.get(segment.session.provider) {
                Ok(agent) => agent,
                Err(error) if usage != HistoryUse::Import => {
                    slot.insert(Err(error));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let history = match self.native_history(&segment.session, usage).await {
                Ok(response) => Ok((
                    response,
                    agent.availability().is_ok() && agent.capabilities().fork,
                )),
                Err(error)
                    if usage != HistoryUse::Import
                        && error.code == "provider_unavailable"
                        && agent.availability().is_err() =>
                {
                    Err(error)
                }
                Err(error) => return Err(error),
            };
            slot.insert(history);
        }
        // Imports can read a prefix ending on an earlier provider.
        let mut response = sources
            .get(&row.active)
            .and_then(|source| source.as_ref().ok())
            .map(|(source, _)| source.clone())
            .unwrap_or_default();
        let mut turns = Vec::new();
        for segment in &row.segments {
            let Some(Ok((source, supports_fork))) = sources.get(&segment.session) else {
                continue;
            };
            let source = source.thread.turns.as_deref().unwrap_or_default();
            let end = segment.end.unwrap_or(source.len());
            let slice = source.get(segment.from..end).ok_or_else(|| {
                Failure::new(
                    "history_changed",
                    "Native history was shortened. Saved conversation boundaries are unavailable.",
                )
            })?;
            turns.extend(slice.iter().map(|turn| {
                let mut turn = row.turn(&segment.session, (**turn).clone());
                turn.supports_fork = Some(*supports_fork);
                Arc::new(turn)
            }));
        }
        response.thread.turns = Some(turns);
        let issues: BTreeSet<_> = sources
            .iter()
            .filter_map(|(session, source)| {
                source
                    .as_ref()
                    .err()
                    .map(|error| (session.provider, &error.message))
            })
            .collect();
        if !issues.is_empty() {
            // Missing providers never erase readable native ranges or invent turns.
            // Saved selections remain known settings, not evidence of availability.
            if sources.get(&row.active).is_none_or(Result::is_err) {
                response.thread.status = agent_protocol::models::SessionStatus::Unavailable;
            }
            response.thread.history_read_state =
                Some(agent_protocol::session::HistoryReadState::new(
                    if sources.values().all(Result::is_err) {
                        agent_protocol::session::HistoryReadKind::Unavailable
                    } else {
                        agent_protocol::session::HistoryReadKind::Incomplete
                    },
                    issues
                        .into_iter()
                        .map(|(provider, message)| format!("{}: {message}", provider.key()))
                        .collect(),
                ));
        }
        let options = self.inner.model_selections.resolve(
            &row.active,
            response.model.as_ref(),
            response.effort.as_deref(),
            response.service_tier.as_deref(),
        )?;
        self.describe_conversation_history(row, response, options)
    }

    pub(super) fn describe_conversation_history(
        &self,
        row: &Conversation,
        mut response: ThreadResponse,
        options: super::model_selections::SelectedOptions,
    ) -> Result<ThreadResponse, Failure> {
        (response.model, response.effort, response.service_tier) = (
            options.model.or(response.model),
            options.effort.or(response.effort),
            options.service_tier.or(response.service_tier),
        );
        row.describe(&mut response.thread);
        if self
            .inner
            .providers
            .get(row.active.provider)
            .and_then(|agent| agent.availability())
            .is_err()
        {
            response.thread.status = agent_protocol::models::SessionStatus::Unavailable;
        }
        for turn in response.thread.turns.iter_mut().flatten() {
            if turn.provider.is_none() {
                let turn = Arc::make_mut(turn);
                let source = row.source(&turn.id)?.0;
                turn.provider = Some(source.provider);
                turn.supports_fork = Some(
                    self.inner
                        .providers
                        .get(source.provider)
                        .is_ok_and(|agent| {
                            agent.availability().is_ok() && agent.capabilities().fork
                        }),
                );
            }
        }
        response.thread.capabilities = Some(self.conversation_capabilities(
            row.active.provider,
            row.sources().map(|source| source.provider),
        ));
        response.thread.history_cursor = None;
        response.thread.history_has_more = Some(false);
        Ok(response)
    }

    /// Like T3 Code's ContextHandoffService, portable context comes from the
    /// application's committed conversation, independently of provider storage.
    async fn handoff_history(&self, root: &SessionRef) -> Result<ThreadResponse, Failure> {
        let response = if let Some(response) = self
            .inner
            .transcripts
            .history(root)
            .map_err(transcripts::storage_failure)?
        {
            response
        } else {
            if let Some(importing) = self.import_transcript(root.clone(), false) {
                importing.await?;
            }
            self.inner
                .transcripts
                .history(root)
                .map_err(transcripts::storage_failure)?
                .ok_or_else(|| {
                    Failure::new(
                        "incomplete_history",
                        "The complete Host conversation transcript is unavailable.",
                    )
                })?
        };
        if response
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .any(|item| item.is_deferred() && transcripts::content(item.body()))
        {
            return Err(Failure::new(
                "incomplete_history",
                "Historical message content could not be read completely.",
            ));
        }
        Ok(response)
    }

    pub(super) fn conversation_page(
        mut response: ThreadResponse,
        end: Option<usize>,
        limit: usize,
    ) -> Result<ThreadResponse, Failure> {
        let turns = response.thread.turns.take().unwrap_or_default();
        let end = end.unwrap_or(turns.len());
        let start = end.saturating_sub(limit);
        response.thread.turns = Some(
            turns
                .get(start..end)
                .ok_or_else(|| {
                    Failure::new(
                        "invalid_history_cursor",
                        "conversation history cursor is no longer valid",
                    )
                })?
                .to_vec(),
        );
        response.thread.history_cursor = (start > 0).then(|| start.to_string());
        response.thread.history_has_more = Some(start > 0);
        if response
            .thread
            .history_read_state
            .as_ref()
            .is_none_or(|state| state.issues.is_empty())
        {
            response.thread.history_read_state =
                Some(agent_protocol::session::HistoryReadState::new(
                    if start > 0 {
                        agent_protocol::session::HistoryReadKind::Partial
                    } else {
                        agent_protocol::session::HistoryReadKind::Complete
                    },
                    vec![],
                ));
        }
        response.thread.history_limit = Some(limit as u64);
        agent_protocol::models::defer_item_details(
            response.thread.turns.as_mut().unwrap(),
            agent_protocol::models::MAX_INLINE_ITEM_BYTES,
        );
        Ok(response)
    }

    pub(super) async fn switch_provider(
        &self,
        root: &SessionRef,
        input: &op::Submission,
    ) -> Result<op::SubmissionReceipt, Failure> {
        let previous = self.inner.conversations.get(root)?;
        let active = previous
            .as_ref()
            .map_or_else(|| root.clone(), |row| row.active.clone());
        let source_agent = self.inner.providers.get(active.provider)?;
        let source = source_agent.state(&active.id).await?.response;
        if source.thread.id.as_ref() != Some(&active) {
            return Err(Failure::new(
                "invalid_session",
                "native session identity changed",
            ));
        }
        let selected = input
            .model
            .as_ref()
            .or(source.model.as_ref())
            .ok_or_else(|| {
                Failure::new("model_required", "Select a model for the target provider.")
            })?;
        // Receipts and live events use public IDs, including after a handoff.
        // Compare the native history in that same namespace.
        let mut observed = source.thread.clone();
        if let Some(row) = &previous {
            for turn in observed.turns.iter_mut().flatten() {
                Arc::make_mut(turn).id = row.identity(&active, &turn.id).into();
            }
        }
        let mut observed = self.inner.router.overlay_execution(root, observed);
        let unfinished = observed
            .submissions
            .iter()
            .filter(|(id, _)| *id != &input.client_user_message_id)
            .map(|(_, delivery)| delivery)
            .filter(|delivery| match delivery {
                // Earlier switches already checked and released their source.
                // Replayed receipts from those bindings cannot own current work.
                agent_protocol::session::SubmissionDelivery::Accepted {
                    turn_id: Some(turn),
                } => !previous
                    .as_ref()
                    .is_some_and(|row| row.source(turn).is_ok_and(|(source, _)| source != active)),
                _ => true,
            })
            .collect::<Vec<_>>();
        // Runtime metadata need not replay finished turns. Host-recorded
        // completions still prove that retained receipts no longer own work.
        for delivery in &unfinished {
            let agent_protocol::session::SubmissionDelivery::Accepted { turn_id: Some(id) } =
                delivery
            else {
                continue;
            };
            if let Some(turn) = self
                .inner
                .transcripts
                .turn(root, id)
                .map_err(transcripts::storage_failure)?
            {
                observed.turns.get_or_insert_default().push(Arc::new(turn));
            }
        }
        if source.thread.status == agent_protocol::models::SessionStatus::Running
            || worktree_active(
                observed.status,
                observed.turns.as_deref().unwrap_or_default(),
                !observed.requests.is_empty(),
                unfinished.into_iter(),
            )
        {
            return Err(Failure::new(
                "provider_switch_busy",
                "現在の応答・確認・送信が確定してからAIを切り替えてください。",
            ));
        }
        if source.thread.can_accept_direct_input == Some(false)
            || (source.thread.parent_id.is_some()
                && source.thread.can_accept_direct_input != Some(true))
        {
            return Err(Failure::new(
                "provider_switch_unavailable",
                "サブエージェントのAIは切り替えられません。親の会話から指示してください。",
            ));
        }
        let deadline = tokio::time::Instant::now() + SESSION_LIST_TIMEOUT;
        let children = session_pages(
            source_agent.as_ref(),
            "",
            SessionListScope::Descendants(&active.id),
            100,
            100,
            Some(deadline),
            None,
        )
        .try_collect::<Vec<_>>()
        .await?;
        for child in children.into_iter().flatten() {
            let thread = if let Some(id) = &child.thread.id {
                self.inner
                    .router
                    .overlay_execution(id, child.thread.clone())
            } else {
                child.thread
            };
            if worktree_active(
                thread.status,
                thread.turns.as_deref().unwrap_or_default(),
                !thread.requests.is_empty(),
                thread.submissions.values(),
            ) {
                return Err(Failure::new(
                    "provider_switch_busy",
                    "サブエージェントの応答・確認が終わってからAIを切り替えてください。",
                ));
            }
        }
        self.require_provider_enabled(selected.provider)?;
        let agent = self.inner.providers.get(selected.provider)?;
        agent.validate_create()?;
        let mut row = match previous {
            Some(row) => row,
            None => {
                let row = Conversation::new(&source.thread)?;
                self.inner.conversations.save(row.clone())?;
                row
            }
        };
        let history = self.handoff_history(root).await?;
        let turns = history.thread.turns.as_deref().unwrap_or_default();
        let count = turns.len();
        row.cover(count);
        let mut target = row
            .binding(selected.provider)
            // Reuse requires known coverage in this transcript. A fresh
            // runtime can receive all history without duplicating native context.
            .filter(|binding| binding.covered.is_some())
            .map(|binding| binding.session.clone());
        // An ambiguous history injection must never be replayed into the same
        // native session. Recover the logical history into a fresh binding.
        if target.as_ref().is_some_and(|target| {
            row.handoffs
                .iter()
                .any(|handoff| &handoff.session == target && handoff.delivery == Delivery::Pending)
        }) {
            target = None;
        }
        let target_history = self
            .handoff_target(&row.cwd, root, selected, target.as_ref())
            .await?;
        let target = target_history
            .thread
            .id
            .clone()
            .ok_or_else(|| Failure::new("invalid_session", "target session ID is missing"))?;
        if !row.bindings.iter().any(|binding| binding.session == target) {
            row.bindings.push(Binding {
                session: target.clone(),
                covered: Some(0),
            });
        }
        // Persist the native binding before attempting delivery; events and
        // reconnection must resolve it to the same public conversation.
        self.inner.conversations.save(row.clone())?;
        let last_seen = row
            .bindings
            .iter()
            .find(|binding| binding.session == target)
            .expect("binding saved")
            .covered
            .unwrap_or(0);
        let messages = turns
            .get(last_seen..)
            .ok_or_else(|| {
                Failure::new(
                    "history_changed",
                    "Host transcript coverage is outside the saved conversation.",
                )
            })?
            .iter()
            .flat_map(|turn| handoff::Message::from_turn(turn))
            .collect::<Vec<_>>();
        let coverage = format!(
            "Provider context handoff. Conversation: {root}. Covered history turns: {last_seen}..{count}.\nRead omitted history on this Host with the BEX history command shown below.\n{}",
            self.history_command(root)
        );
        let context = handoff::select(&messages, &coverage, handoff::DEFAULT_BUDGET)
            .map_err(|error| Failure::new("handoff_context_limit", error))?;
        source_agent
            .unload(&active.id)
            .await
            .map_err(Failure::before_submission)?;
        let index = row.handoffs.len();
        row.handoffs.push(Handoff {
            session: target.clone(),
            input_id: input.client_user_message_id.clone(),
            context,
            delivery: Delivery::Pending,
            receipt: None,
            submission: agent_protocol::session::SubmissionDelivery::Rejected,
        });
        self.inner.conversations.save(row.clone())?;
        let injected = agent
            .inject_history(&target.id, &row.handoffs[index].context)
            .await
            .map_err(Failure::before_submission)?;
        if injected {
            row.handoffs[index].delivery = Delivery::Injected;
            self.inner.conversations.save(row.clone())?;
        }
        row.activate(target.clone(), count);
        self.inner.conversations.save(row.clone())?;
        let mut native_input = input.clone();
        native_input.thread_id = target.clone();
        if !injected {
            native_input.input.insert(
                0,
                op::Input::Text {
                    text: inline_context(&row.handoffs[index].context),
                },
            );
        }
        row.handoffs[index].submission = agent_protocol::session::SubmissionDelivery::Sending;
        self.inner.conversations.save(row.clone())?;
        let result = self.submit_native(root, &target, &native_input).await;
        match result {
            Ok(mut receipt) => {
                if !injected {
                    row.handoffs[index].delivery = Delivery::Inline;
                }
                receipt.turn_id = receipt.turn_id.map(|id| row.identity(&target, &id).into());
                row.handoffs[index].receipt = Some(receipt.clone());
                row.handoffs[index].submission =
                    agent_protocol::session::SubmissionDelivery::Accepted {
                        turn_id: receipt.turn_id.clone(),
                    };
                self.inner.conversations.save(row)?;
                Ok(receipt)
            }
            Err(error) => {
                // A refused prompt has no tool effects. Keep delivered native
                // history and restore the source binding.
                if error.delivery == agent_protocol::error::Delivery::NotSent {
                    row.active = active.clone();
                    if !injected {
                        row.handoffs[index].delivery = Delivery::Ready;
                        row.bindings
                            .iter_mut()
                            .find(|binding| binding.session == target)
                            .expect("target binding")
                            .covered = Some(last_seen);
                    }
                    row.handoffs[index].submission =
                        agent_protocol::session::SubmissionDelivery::Rejected;
                    self.inner.conversations.save(row)?;
                } else {
                    row.handoffs[index].submission =
                        agent_protocol::session::SubmissionDelivery::Unknown;
                    self.inner.conversations.save(row)?;
                }
                Err(error)
            }
        }
    }

    async fn handoff_target(
        &self,
        cwd: &str,
        root: &SessionRef,
        model: &agent_protocol::models::ModelRef,
        previous: Option<&SessionRef>,
    ) -> Result<ThreadResponse, Failure> {
        let agent = self.inner.providers.get(model.provider)?;
        if let Some(previous) = previous
            && let Ok(state) = agent.state(&previous.id).await
        {
            let response = state.response;
            if response.thread.id.as_ref() != Some(previous) {
                return Err(Failure::new(
                    "invalid_session",
                    "native session identity changed",
                ));
            }
            if response.thread.status == agent_protocol::models::SessionStatus::Running
                || !response.thread.requests.is_empty()
            {
                return Err(Failure::new(
                    "provider_switch_busy",
                    "切り替え先のAIがまだ実行中です。完了を待ってください。",
                ));
            }
            if readable_history(response.thread.history_read_state.as_ref()) {
                return Ok(response);
            }
        }
        // An unreadable continuation receives all Host history in a fresh
        // runtime. Keep the exact workspace and the same public conversation.
        agent
            .create(
                cwd,
                Some(&model.id),
                self.browser_config(&root.to_string())?,
            )
            .await
    }

    fn history_command(&self, root: &SessionRef) -> String {
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

    pub(super) async fn read_conversation_page(
        &self,
        row: &Conversation,
        cursor: &str,
    ) -> Result<HistoryPage, Failure> {
        let end = cursor.parse().map_err(|_| {
            Failure::new(
                "invalid_history_cursor",
                "invalid conversation history cursor",
            )
        })?;
        let page = Self::conversation_page(
            self.conversation_history(row, HistoryUse::Read).await?,
            Some(end),
            5,
        )?;
        Ok(HistoryPage {
            history_read_state: page.thread.history_read_state,
            turns: page.thread.turns.unwrap_or_default(),
            next_cursor: page.thread.history_cursor,
        })
    }
}
