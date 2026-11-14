//! Session lifecycle: opening, creation, submission, answers and native reads.
use super::*;

impl HostRpcService {
    pub(super) async fn read_turn_items(
        &self,
        session: SessionId,
        params: &agent_protocol::session::ReadTurnItems,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let read = self
            .inner
            .router
            .retain_execution(params.session.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        let stored = self
            .inner
            .store
            .turn(&params.session, &params.turn_id)
            .map_err(transcripts::storage_failure)?
            .and_then(|turn| turn.items);
        let items = if let Some(items) = stored {
            items
        } else if let Some(row) = self.inner.conversations.get(&params.session)? {
            let (source, turn) = row.source(&params.turn_id)?;
            if !row.sources().any(|session| *session == source) {
                return Err(Failure::new(
                    "invalid_history_reference",
                    "The turn is outside this conversation.",
                ));
            }
            let mut items = self
                .inner
                .providers
                .get(source.provider)?
                .read_turn_items(&source.id, &turn.into())
                .await?;
            for item in &mut items {
                row.item(&source, Arc::make_mut(item));
            }
            items
        } else {
            self.inner
                .providers
                .get(params.session.provider)?
                .read_turn_items(&params.session.id, &params.turn_id)
                .await?
        };
        self.inner
            .router
            .finish_turn_read(read, session, params.turn_id.clone(), items)
            .map_err(|error| Failure::new("turn_details_failed", error))?;
        Ok(agent_protocol::models::Empty {})
    }

    pub(super) async fn read_history(
        &self,
        params: &agent_protocol::session::ReadHistory,
    ) -> Result<agent_protocol::session::HistoryPage, Failure> {
        let row = self.inner.conversations.get(&params.session)?;
        // Transcript cursors are positions. A window read from native history
        // before its import keeps paging native history.
        let stored = match transcripts::position(row.is_some(), &params.cursor) {
            Some(end) => self.stored_history(
                &params.session,
                row.as_ref(),
                Some(end),
                transcripts::WINDOW_TURNS,
                params.include_activity,
            )?,
            None => None,
        };
        let mut page = if let Some(response) = stored {
            agent_protocol::session::HistoryPage {
                history_read_state: response.thread.history_read_state,
                turns: response.thread.turns.unwrap_or_default(),
                next_cursor: response.thread.history_cursor,
            }
        } else if let Some(row) = row {
            self.read_conversation_page(&row, &params.cursor).await?
        } else {
            self.inner
                .providers
                .get(params.session.provider)?
                .read_history(&params.session.id, &params.cursor, params.include_activity)
                .await?
        };
        agent_protocol::models::defer_item_details(
            &mut page.turns,
            agent_protocol::models::MAX_INLINE_ITEM_BYTES,
        );
        if agent_protocol::protocol::encode(Response::Success { result: &page })
            .map_err(|error| Failure::new("invalid_thread_history", error))?
            .len()
            > agent_protocol::protocol::MAX_FRAME_BYTES
        {
            agent_protocol::models::defer_item_details(&mut page.turns, 0);
        }
        Ok(page)
    }

    pub(super) async fn list_agents(
        &self,
        params: &op::ListAgents,
    ) -> Result<Vec<agent_protocol::models::AgentObservation>, Failure> {
        let row = self.inner.conversations.get(&params.thread_id)?;
        if row.as_ref().is_some_and(|row| row.bindings.is_empty()) {
            return Ok(Vec::new());
        }
        let native = row.as_ref().map_or(&params.thread_id, |row| &row.active);
        let agent = self.inner.providers.get(native.provider)?;
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
                .conversations
                .summary(crate::projects::titles::summary(summary.thread))?
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

    pub(super) async fn fork_session(
        &self,
        target: &agent_protocol::session::SessionRef,
        params: &op::ForkSession,
    ) -> Result<ThreadResponse, Failure> {
        if let Some(row) = self.inner.conversations.get(target)? {
            return self.fork_conversation(&row, &params.last_turn_id).await;
        }
        self.require_provider_enabled(target.provider)?;
        let agent = self.inner.providers.get(target.provider)?;
        require_capability::<agent_protocol::protocol::contracts::ForkSession>(
            agent.capabilities().fork,
        )?;
        self.start_thread(agent.as_ref(), |browser| {
            agent.fork(&target.id, &params.last_turn_id, browser)
        })
        .await
    }

    pub(super) async fn interrupt(
        &self,
        target: &agent_protocol::session::SessionRef,
        params: &op::Interrupt,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        // A stop leaves what was queued behind the turn for the user to resume;
        // the hold comes first because the stopped turn can end before the reply.
        let held = self.hold_queue(target, true);
        let result = self.stop_turn(target, params).await;
        if result.is_err() && held {
            self.hold_queue(target, false);
        }
        result
    }

    async fn stop_turn(
        &self,
        target: &agent_protocol::session::SessionRef,
        params: &op::Interrupt,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let (handled, turn_id) = self.capacity_interrupt(target, &params.turn_id);
        if handled {
            return Ok(agent_protocol::models::Empty {});
        }
        let row = self.inner.conversations.get(target)?;
        if let Some(row) = row.as_ref().filter(|row| {
            row.startup.as_ref().is_some_and(|startup| {
                startup
                    .preparation
                    .submission
                    .client_user_message_id
                    .as_str()
                    == params.turn_id.as_str()
            })
        }) {
            return self.interrupt_preparation(&row.id, &params.turn_id).await;
        }
        let native = row.as_ref().map_or(target, |row| &row.active);
        let turn = if let Some(row) = row.as_ref() {
            let (source, id) = row.source(&turn_id)?;
            if &source != native {
                return Err(Failure::new(
                    "invalid_turn",
                    "Only the current provider's turn can be interrupted.",
                ));
            }
            id.into()
        } else {
            turn_id
        };
        self.inner
            .providers
            .get(native.provider)?
            .interrupt(&native.id, &turn)
            .await
    }

    pub(super) async fn rename_session(
        &self,
        target: &agent_protocol::session::SessionRef,
        params: &op::RenameSession,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        if let Some(mut row) = self.inner.conversations.get(target)? {
            row.name = Some(params.name.clone());
            self.inner.conversations.save(row)?;
            self.inner
                .router
                .broadcast(agent_protocol::protocol::Notification::SessionRenamed {
                    session: target.clone(),
                    name: Some(params.name.clone()),
                    revision: 0,
                });
            return Ok(agent_protocol::models::Empty {});
        }
        let agent = self.inner.providers.get(target.provider)?;
        require_capability::<agent_protocol::protocol::contracts::RenameSession>(
            agent.capabilities().rename,
        )?;
        let renamed = agent.rename(&target.id, &params.name).await?;
        self.inner
            .store
            .rename(target, Some(&params.name))
            .map_err(transcripts::storage_failure)?;
        Ok(renamed)
    }

    pub(super) async fn submit_input(
        &self,
        target: &agent_protocol::session::SessionRef,
        input: &op::Submission,
    ) -> Result<op::SubmissionReceipt, Failure> {
        op::Input::validate_contexts(&input.input)
            .map_err(|reason| Failure::new("invalid_params", reason))?;
        self.capacity_input(target);
        let mut row = self.inner.conversations.get(target)?;
        if let Some(current) = row.as_ref().filter(|row| {
            row.startup.as_ref().is_some_and(|startup| {
                startup.preparation.status == agent_protocol::session::PreparationStatus::Unknown
            })
        }) {
            self.preparation_history(current).await?;
            row = self.inner.conversations.get(target)?;
            if let Some(receipt) = self
                .inner
                .router
                .submission_receipt(target, &input.client_user_message_id)
            {
                return Ok(receipt);
            }
        }
        if let Some(startup) = row.as_ref().and_then(|row| row.startup.as_ref()) {
            if startup.preparation.status == agent_protocol::session::PreparationStatus::Unknown {
                return Err(Failure::unknown(
                    "submission_delivery_unknown",
                    "最初の送信結果を確認できません。会話を確認してから送信してください。",
                ));
            }
            return Err(Failure::new(
                "preparation_unfinished",
                "タスクの準備を再試行してください。",
            ));
        }
        if let Some(receipt) = row
            .as_ref()
            .and_then(|row| {
                row.handoffs.iter().rev().find(|handoff| {
                    handoff.input_id == input.client_user_message_id && handoff.receipt.is_some()
                })
            })
            .and_then(|handoff| handoff.receipt.clone())
        {
            return Ok(receipt);
        }
        let active = row.as_ref().map_or(target, |row| &row.active);
        if row.as_ref().is_some_and(|row| {
            row.handoffs.iter().any(|handoff| {
                handoff.input_id == input.client_user_message_id
                    && matches!(
                        handoff.submission,
                        agent_protocol::session::SubmissionDelivery::Sending
                            | agent_protocol::session::SubmissionDelivery::Unknown
                    )
            })
        }) {
            return Err(Failure::unknown(
                "submission_delivery_unknown",
                "The previous provider input may already have run. It will not be sent again.",
            ));
        }
        if input.fresh_context
            || input
                .model
                .as_ref()
                .is_some_and(|model| model.provider != active.provider)
            || row.as_ref().is_some_and(|row| {
                row.handoffs.iter().any(|handoff| {
                    handoff.session == row.active
                        && handoff.delivery == crate::host_rpc::conversations::Delivery::Pending
                })
            })
        {
            return self.change_context(target, input).await;
        }
        let mut native = input.clone();
        native.thread_id = active.clone();
        let mut receipt = self.submit_native(target, active, &native).await?;
        if let Some(row) = row.as_ref() {
            receipt.turn_id = receipt.turn_id.map(|id| row.identity(active, &id).into());
        }
        Ok(receipt)
    }
    pub(super) async fn submit_native(
        &self,
        root: &agent_protocol::session::SessionRef,
        target: &agent_protocol::session::SessionRef,
        input: &op::Submission,
    ) -> Result<op::SubmissionReceipt, Failure> {
        self.require_provider_enabled(target.provider)?;
        if input
            .model
            .as_ref()
            .is_some_and(|model| model.provider != target.provider || model.id.trim().is_empty())
        {
            return Err(Failure::new(
                "provider_mismatch",
                "The model must belong to the target provider.",
            ));
        }
        let agent = self.inner.providers.get(target.provider)?;
        agent.availability()?;
        let (state, projects) = tokio::try_join!(agent.state(&target.id), self.project_snapshot())?;
        let mut response = state.response;
        let options = self.inner.model_selections.resolve(
            target,
            response.model.as_ref(),
            response.effort.as_deref(),
            response.service_tier.as_deref(),
        )?;
        (response.model, response.effort, response.service_tier) =
            (options.model, options.effort, options.service_tier);
        if response.thread.id.as_ref() != Some(target) {
            return Err(Failure::new(
                "invalid_session",
                "native session identity changed",
            ));
        }
        response.thread = self.inner.router.overlay_execution(root, response.thread);
        describe_thread(&mut response.thread, agent.capabilities(), &projects);
        if response
            .thread
            .preview
            .as_deref()
            .is_none_or(|preview| preview.trim().is_empty())
        {
            response.thread.preview = input.input.iter().find_map(|part| match part {
                op::Input::Text { text } => Some(text.clone()),
                _ => None,
            });
        }
        let running_turn = response
            .thread
            .turns
            .iter()
            .flatten()
            .rev()
            .find(|t| {
                t.status == agent_protocol::models::TurnStatus::Running && !t.id.trim().is_empty()
            })
            .map(|t| {
                crate::host_rpc::conversations::source(&t.id)
                    .map(|(_, id)| id)
                    .unwrap_or_else(|_| t.id.to_string())
            });
        let queue = self.inner.router.queue(root);
        let execution = crate::host_rpc::submission::Execution {
            status: response.thread.status,
            running_turn: running_turn.as_deref(),
            steers: agent.capabilities().steer,
            // A held queue waits for the user; new input starts as T3 Code's does.
            ahead: !queue.held
                && queue.inputs.first().is_some_and(|first| {
                    first.client_user_message_id != input.client_user_message_id
                }),
        };
        let route = crate::host_rpc::submission::submission_route(
            execution,
            input.follow_up,
            response.thread.cwd.as_deref(),
        )
        .map_err(|e| Failure::new("submission_unavailable", e))?;
        let route = match route {
            crate::host_rpc::submission::Route::Provider(route) => route,
            crate::host_rpc::submission::Route::Queue => {
                return self.enqueue(root, input, queue);
            }
        };
        let (reload, starts_turn) = match route {
            crate::providers::SubmissionTarget::Start { cwd } => (
                self.restore_workspace(cwd).await? || state.needs_reload,
                true,
            ),
            _ => (state.needs_reload, false),
        };
        let receipt = agent
            .submit(
                input,
                route,
                reload,
                self.browser_config(&root.to_string())?,
            )
            .await;
        if matches!(input.input.as_slice(), [op::Input::Text { text }] if text.trim().eq_ignore_ascii_case("/compact"))
        {
            self.capacity_compaction(root);
        }
        let receipt = receipt?;
        if starts_turn {
            let same_model = input
                .model
                .as_ref()
                .is_none_or(|model| Some(model) == response.model.as_ref());
            self.inner.model_selections.record(
                target,
                input.model.as_ref().or(response.model.as_ref()),
                input
                    .effort
                    .as_deref()
                    .or_else(|| response.effort.as_deref().filter(|_| same_model)),
                input
                    .service_tier
                    .as_deref()
                    .or_else(|| response.service_tier.as_deref().filter(|_| same_model)),
            );
        }
        response.thread.updated_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        );
        response.thread = self.inner.router.overlay_execution(root, response.thread);
        if let Some(row) = self.inner.conversations.get(root)? {
            row.describe(&mut response.thread);
            response.thread.capabilities = Some(agent_protocol::session::Capabilities {
                rename: true,
                ..agent.capabilities()
            });
        }
        self.notify_session_updated(response.thread, &projects.projects);
        Ok(receipt)
    }
    pub(super) fn notify_session_updated(
        &self,
        thread: Thread,
        projects: &[agent_protocol::models::Project],
    ) {
        // Admit new Host conversations without replacing newer provider events.
        self.inner.router.task_activity(vec![thread.clone()], 0);
        let project = thread
            .project_id
            .as_ref()
            .and_then(|id| projects.iter().find(|project| &project.id == id))
            .cloned();
        self.inner
            .router
            .broadcast(agent_protocol::protocol::Notification::SessionUpdated {
                thread: crate::projects::titles::summary(thread).into(),
                project,
            });
    }
    pub(super) async fn answer_request(
        &self,
        session: SessionId,
        id: agent_protocol::ids::RequestId,
        answer: agent_protocol::requests::Answer,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        use agent_protocol::session::RequestDelivery;
        let (origin, body) = self
            .inner
            .router
            .claim_response(session, &id, &answer)
            .map_err(|error| Failure::new("invalid_answer", error))?;
        // Until a native command is admitted, cancellation is proven not sent.
        let mut delivery = scopeguard::guard((id, RequestDelivery::Awaiting), |(id, state)| {
            self.inner.router.response_delivery(&id, state)
        });
        let write = origin
            .source
            .prepare(&origin.native_id, &body, &answer)
            .await?;
        delivery.1 = RequestDelivery::Unknown;
        write.await?;
        delivery.1 = RequestDelivery::Sent;
        Ok(agent_protocol::models::Empty {})
    }
    pub(super) async fn session_open(
        &self,
        session: SessionId,
        params: &agent_protocol::session::OpenSession,
    ) -> Result<HostReply, String> {
        let result: anyhow::Result<HostReply> = async {
            if params.limit == 0 {
                anyhow::bail!("invalid session reference or zero history limit");
            }
            let target = params.session.clone();
            let row = self.inner.conversations.get(&target)?;
            let native = row.as_ref().map_or(&target, |row| &row.active);
            let limit = params.limit;
            let read = self
                .inner
                .router
                .retain_execution(target.clone())
                .map_err(anyhow::Error::msg)?;
            let revision = self.inner.router.list_revision();
            let started = std::time::Instant::now();
            let mut stored = self.stored_history(&target, row.as_ref(), None, limit, params.include_activity)?;
            // A conversation's first open imports it. Native history serves only
            // conversations it can read only partly, or that are still preparing.
            if stored.is_none()
                && row.as_ref().is_none_or(|row| row.startup.is_none())
                && let Some(importing) = self.import_transcript(target.clone(), false)
            {
                match importing.await {
                    Ok(()) => {
                        stored = self.stored_history(&target, row.as_ref(), None, limit, params.include_activity)?;
                    }
                    Err(error) if transcripts::readable_without_import(&error) => {}
                    Err(error) => return Err(anyhow::anyhow!(error.message)),
                }
            }
            let source = if stored.is_some() { "transcript" } else { "native" };
            let mut response = if let Some(response) = stored {
                response
            } else if let Some(row) = &row {
                Self::conversation_page(self.conversation_history(row, conversations::HistoryUse::Read).await?, None, limit)?
            } else {
                let agent = self.inner.providers.get(native.provider)?;
                let mut response = agent.open(&target.id, limit, params.include_activity).await?;
                response.thread.capabilities = Some(agent.capabilities());
                let options = self.inner.model_selections.resolve(native, response.model.as_ref(), response.effort.as_deref(), response.service_tier.as_deref())?;
                (response.model, response.effort, response.service_tier) = (options.model, options.effort, options.service_tier);
                response
            };
            response.thread.list_revision = revision;
            let native_ms = started.elapsed().as_millis();
            if response.thread.id.as_ref() != Some(&target) {
                return Err(anyhow::anyhow!("native session identity changed"));
            }
            let capabilities = response.thread.capabilities.unwrap_or_default();
            describe_thread(
                &mut response.thread,
                capabilities,
                &self.project_snapshot().await?,
            );
            let project_ms = started.elapsed().as_millis() - native_ms;
            let more = response.thread.history_has_more == Some(true);
            response.thread.history_has_more = Some(more);
            response.thread.history_limit = Some(limit as u64);
            response.thread.history_read_state.get_or_insert_with(|| {
                agent_protocol::session::HistoryReadState::new(
                    if more {
                        agent_protocol::session::HistoryReadKind::Partial
                    } else {
                        agent_protocol::session::HistoryReadKind::Complete
                    },
                    Vec::new(),
                )
            });
            let known = params.known_version.is_some() && params.known_version == response.thread.transcript_version;
            let reply = self.inner
                .router
                .finish_session_read(read, session, response, known)
                .map_err(anyhow::Error::msg)?;
            tracing::info!(target: "agent", operation = "history.open",
                message = %format_args!("source={source} known={known} native_ms={native_ms} project_ms={project_ms} total_ms={} bytes={} limit={limit}",
                    started.elapsed().as_millis(), reply.initial.len()));
            Ok(reply)
        }
        .await;
        match result {
            Ok(reply) => Ok(reply),
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(target: "agent", operation = "history.open", message);
                Ok(Response::error("session_open_failed", &message).into())
            }
        }
    }
    pub(super) async fn read_item(
        &self,
        session: SessionId,
        params: &op::ReadItem,
        target: &agent_protocol::session::SessionRef,
    ) -> Result<agent_protocol::operations::ItemResponse, Failure> {
        let live = self
            .inner
            .router
            .item(target, &params.turn_id, &params.item_id)
            .filter(|item| !item.is_deferred());
        let known = match live {
            Some(item) => Some(item),
            None => self
                .inner
                .store
                .item(target, &params.turn_id, &params.item_id, true)
                .map_err(transcripts::storage_failure)?
                .filter(|item| !item.is_deferred()),
        };
        let mut response = if let Some(item) = known {
            agent_protocol::operations::ItemResponse {
                item: Arc::unwrap_or_clone(item),
                transfer: None,
            }
        } else if let Some(row) = self.inner.conversations.get(target)? {
            let (source, turn) = row.source(&params.turn_id)?;
            let (item_source, item) = row.source(&params.item_id)?;
            if source != item_source || !row.sources().any(|session| *session == source) {
                return Err(Failure::new(
                    "invalid_history_reference",
                    "The item is outside this conversation.",
                ));
            }
            let mut output = self
                .inner
                .providers
                .get(source.provider)?
                .read_item(&op::ReadItem {
                    thread_id: source.clone(),
                    turn_id: turn.into(),
                    item_id: item.into(),
                })
                .await?;
            row.item(&source, &mut output.item);
            output
        } else {
            self.inner
                .providers
                .get(target.provider)?
                .read_item(params)
                .await?
        };
        let bytes = agent_protocol::protocol::encode(&response.item).expect("item serializes");
        if bytes.len() > agent_protocol::models::MAX_INLINE_ITEM_BYTES {
            response.transfer = Some(
                self.inner
                    .files
                    .download_bytes(session, bytes)
                    .await
                    .map_err(|error| Failure::new("item_transfer_failed", error))?,
            );
            response.item.defer();
        }
        Ok(response)
    }
    pub(super) async fn create_session(
        &self,
        params: agent_protocol::operations::CreateSession,
    ) -> Result<ThreadResponse, Failure> {
        let agent = self.creation_agent(params.provider, params.model.as_ref())?;
        let (cwd, created) = self
            .prepare_directory(params.cwd.as_deref(), || async { Ok(()) })
            .await?;
        let started = self
            .start_thread(agent.as_ref(), |browser| {
                agent.create(&cwd, params.model.as_ref().map(|m| m.id.as_str()), browser)
            })
            .await;
        if started.is_err() && created {
            self.discard_worktree(&cwd).await;
        }
        started
    }

    pub(super) fn creation_agent(
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

    /// Returns the session's working directory and whether this call created its checkout.
    pub(super) async fn prepare_directory<F: std::future::Future<Output = anyhow::Result<()>>>(
        &self,
        cwd: Option<&str>,
        on_worktree: impl FnOnce() -> F,
    ) -> Result<(String, bool), Failure> {
        // A missing selection must not inherit the App Server's checkout.
        // Keep the real cwd on the thread; project enrichment identifies
        // this persisted location as a chat even after a Host restart.
        if cwd.is_none_or(|cwd| cwd.trim().is_empty()) {
            let directory = self.inner.projects.chat_directory();
            tokio::fs::create_dir_all(&directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            let directory = tokio::fs::canonicalize(directory)
                .await
                .map_err(|error| Failure::new("chat_directory_unavailable", error))?;
            let directory = dunce::simplified(&directory).to_owned();
            return directory
                .into_os_string()
                .into_string()
                .map(|directory| (directory, false))
                .map_err(|_| Failure::new("chat_directory_unavailable", "chat path is not UTF-8"));
        }
        match self.inner.worktrees.prepare(cwd, on_worktree).await {
            Ok(Some(created)) => created
                .into_os_string()
                .into_string()
                .map(|created| (created, true))
                .map_err(|_| {
                    Failure::new("worktree_creation_failed", "worktree path is not UTF-8")
                }),
            Ok(None) => Ok((cwd.unwrap_or_default().into(), false)),
            Err(error) => Err(Failure::new("worktree_creation_failed", error)),
        }
    }
    /// Persist Host ownership before publishing a newly created conversation.
    pub(super) async fn start_thread<F>(
        &self,
        agent: &dyn Agent,
        start: impl FnOnce(Option<serde_json::Value>) -> F,
    ) -> Result<ThreadResponse, Failure>
    where
        F: std::future::Future<Output = Result<ThreadResponse, Failure>>,
    {
        let response = self.native_thread(agent, start).await?;
        if let Err(error) = crate::host_rpc::conversations::Conversation::new(&response.thread)
            .and_then(|row| self.inner.conversations.save(row))
        {
            if let Some(id) = &response.thread.id {
                let _ = agent.unload(&id.id).await;
            }
            return Err(error);
        }
        self.notify_thread(response.thread.clone()).await?;
        Ok(response)
    }

    /// A native session no client has heard of yet.
    pub(super) async fn native_thread<F>(
        &self,
        agent: &dyn Agent,
        start: impl FnOnce(Option<serde_json::Value>) -> F,
    ) -> Result<ThreadResponse, Failure>
    where
        F: std::future::Future<Output = Result<ThreadResponse, Failure>>,
    {
        let scope = uuid::Uuid::new_v4().to_string();
        let mut response = start(self.browser_config(&scope)?).await?;
        if let Some(browser) = self.inner.browser.get()
            && let Some(id) = &response.thread.id
        {
            browser.bind_scope(scope, id.to_string()).await;
        }
        let projects = self.project_snapshot().await?;
        describe_thread(&mut response.thread, agent.capabilities(), &projects);
        Ok(response)
    }

    pub(super) async fn notify_thread(&self, thread: Thread) -> Result<(), Failure> {
        let projects = self.project_snapshot().await?;
        self.notify_session_updated(thread, &projects.projects);
        Ok(())
    }
}

fn require_capability<C: agent_protocol::protocol::contracts::Contract>(
    supported: bool,
) -> Result<(), Failure> {
    if supported {
        Ok(())
    } else {
        Err(Failure::new(
            "unsupported_operation",
            format!("{} is unsupported by this provider", C::METHOD),
        ))
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

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn answers_keep_delivery_evidence_until_the_source_resolves_them() {
        use super::*;
        use agent_protocol::{
            requests::{Answer, ElicitationAnswer},
            session::{RequestDelivery, SessionChange},
        };
        use futures_util::FutureExt;
        let root = tempfile::tempdir().unwrap();
        let claude = crate::providers::Claude::load(
            root.path().join("unused-cli"),
            root.path().join("claude"),
            Some(root.path().join("native")),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("worktrees.json")),
        );
        let connection = service.open_session();
        let router = &service.inner.router;
        let target =
            agent_protocol::session::SessionRef::new(ProviderKind::Claude, "native".into())
                .unwrap();
        let (input, mut receiver) = tokio::sync::mpsc::channel(1);
        let instance = uuid::Uuid::new_v4();
        let answer = Answer::Elicitation {
            action: ElicitationAnswer::Accept {
                values: serde_json::json!({}),
            },
        };
        let delivery = |id: &agent_protocol::ids::RequestId| {
            let thread = router.overlay_execution(&target, Thread::default());
            thread.requests.get(id).map(|request| request.delivery)
        };
        for native in ["cancelled", "interrupted", "written"] {
            let adapted = crate::providers::requests::claude(
                uuid::Uuid::new_v4().to_string().into(),
                &"unrelated".into(),
                &crate::providers::claude::SdkRequest::Elicitation {
                    server_name: "server".into(),
                    message: String::new(),
                    mode: None,
                    url: None,
                    requested_schema: Some(serde_json::json!({"type":"object","properties":{}})),
                },
            )
            .unwrap();
            let id = adapted.request.id.clone();
            router
                .request(
                    target.clone(),
                    crate::providers::claude::request_origin(
                        instance,
                        serde_json::json!(native),
                        input.clone(),
                        adapted.answers,
                    ),
                    adapted.request,
                )
                .unwrap();
            // Session-scoped elicitation works without a live turn and survives another turn's completion.
            router.session_change(
                &target,
                SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: "unrelated".into(),
                        ..Default::default()
                    },
                    completed: true,
                },
            );
            assert_eq!(delivery(&id), Some(RequestDelivery::Awaiting));
            if native == "cancelled" {
                assert!(
                    input
                        .try_send(crate::providers::claude::Command {
                            value: crate::providers::claude::SdkInput::Interrupt {
                                request_id: "unused".into()
                            },
                            user: None,
                            delivered: None,
                        })
                        .is_ok()
                );
            }
            let mut operation =
                Box::pin(service.answer_request(connection.id(), id.clone(), answer.clone()));
            assert!(operation.as_mut().now_or_never().is_none());
            assert_eq!(delivery(&id), Some(RequestDelivery::Sending));
            assert!(
                router
                    .claim_response(connection.id(), &id, &answer)
                    .is_err()
            );
            let command = receiver.try_recv().unwrap();
            if native == "cancelled" {
                drop(operation);
                assert_eq!(delivery(&id), Some(RequestDelivery::Awaiting));
                assert!(command.delivered.is_none());
            } else if native == "interrupted" {
                drop(operation);
                assert_eq!(delivery(&id), Some(RequestDelivery::Unknown));
                assert!(command.delivered.is_some());
            } else {
                assert!(
                    matches!(command.value, crate::providers::claude::SdkInput::Answer { request_id, .. } if request_id == native)
                );
                command
                    .delivered
                    .unwrap()
                    .send(Ok(Some("turn".into())))
                    .unwrap();
                operation.await.unwrap();
                assert_eq!(delivery(&id), Some(RequestDelivery::Sent));
            }
            if native != "cancelled" {
                assert!(
                    router
                        .claim_response(connection.id(), &id, &answer)
                        .is_err()
                );
            }
            router.resolve_native_request(instance, &serde_json::json!(native));
            assert_eq!(delivery(&id), None);
        }
    }

    #[tokio::test]
    async fn answer_preflight_failures_keep_awaiting_and_prove_non_delivery() {
        use super::*;
        use agent_protocol::requests::{Answer, ElicitationAnswer};
        for case in [
            "invalidClaudeId",
            "closedClaude",
            "unavailableCodex",
            "stoppedCodex",
        ] {
            let root = tempfile::tempdir().unwrap();
            let claude = case.ends_with("Claude") || case == "invalidClaudeId";
            let provider = if claude {
                ProviderKind::Claude
            } else {
                ProviderKind::Codex
            };
            let backend = if claude {
                crate::providers::Claude::load(
                    root.path().join("unused-cli"),
                    root.path().join("claude"),
                    Some(root.path().join("native")),
                )
                .await
                .unwrap()
                .into()
            } else {
                Backend::unavailable(provider, "unavailable")
            };
            let service = HostRpcService::new(
                [backend],
                ProjectStore::new(root.path().join("worktrees.json")),
            );
            let connection = service.open_session();
            let target =
                agent_protocol::session::SessionRef::new(provider, "native".into()).unwrap();
            let (input, mut receiver) = tokio::sync::mpsc::channel(1);
            let stopped = tokio_util::sync::CancellationToken::new();
            let adapted = if claude {
                crate::providers::requests::claude("request".into(), &"turn".into(), &crate::providers::claude::SdkRequest::Elicitation { server_name: String::new(), message: String::new(), mode: None, url: None, requested_schema: Some(serde_json::json!({"type":"object","properties":{}})) })
            } else {
                crate::providers::requests::codex("request".into(),"mcpServer/elicitation/request",&serde_json::json!({"mode":"form","requestedSchema":{"type":"object","properties":{}}}))
            }.unwrap();
            let id = adapted.request.id.clone();
            let instance = uuid::Uuid::new_v4();
            let native_id = if case == "invalidClaudeId" {
                serde_json::json!(1)
            } else {
                serde_json::json!("native-request")
            };
            let origin = if claude {
                crate::providers::claude::request_origin(
                    instance,
                    native_id.clone(),
                    input,
                    adapted.answers,
                )
            } else {
                crate::providers::unavailable_origin(instance, native_id.clone(), stopped.clone())
            };
            service
                .inner
                .router
                .request(target.clone(), origin, adapted.request)
                .unwrap();
            if case == "closedClaude" {
                receiver.close();
            }
            if case == "stoppedCodex" {
                stopped.cancel();
            }
            for _ in 0..2 {
                let failure = service
                    .answer_request(
                        connection.id(),
                        id.clone(),
                        Answer::Elicitation {
                            action: ElicitationAnswer::Accept {
                                values: serde_json::json!({}),
                            },
                        },
                    )
                    .await
                    .unwrap_err();
                assert_eq!(
                    failure.delivery,
                    agent_transport::peer::Delivery::NotSent,
                    "{case}"
                );
                let thread = service
                    .inner
                    .router
                    .overlay_execution(&target, Thread::default());
                assert_eq!(
                    thread.requests[&id].delivery,
                    agent_protocol::session::RequestDelivery::Awaiting,
                    "{case}"
                );
                assert!(receiver.try_recv().is_err());
            }
            service
                .inner
                .router
                .resolve_native_request(instance, &native_id);
        }
    }

    #[tokio::test]
    async fn opening_claude_history_imports_native_files_once_without_starting_a_cli() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native");
        let project = native.join("projects/example");
        std::fs::create_dir_all(&project).unwrap();
        let id = "12345678-1234-4234-8234-123456789abc";
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            include_str!("../../../tests/fixtures/claude-2.1.266.jsonl"),
        )
        .unwrap();
        let claude = crate::providers::Claude::load(
            root.path().join("does-not-exist"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let service = HostRpcService::new(
            [claude.into()],
            ProjectStore::new(root.path().join("bex-worktrees.json")),
        );
        let session = service.open_session();
        let call =
            agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                include_activity: false,
                known_version: None,
                session: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: id.into(),
                },
                limit: 5,
            });
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert!(reply.get("error").is_none(), "{reply}");
        let thread = &reply["result"]["response"]["thread"];
        assert!(!thread["turns"].as_array().unwrap().is_empty());
        assert_ne!(thread["historyReadState"]["type"], "unavailable");
        let target = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: id.into(),
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while service
                .inner
                .store
                .window(&target, None, 1)
                .unwrap()
                .is_none()
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first open imports native history");
        let transcript = project.join(format!("{id}.jsonl"));
        let changed = std::fs::read_to_string(&transcript)
            .unwrap()
            .replace("Fixture user input", "Changed outside Bex");
        std::fs::write(transcript, changed).unwrap();
        // Later opens read the Host transcript; changes made outside the Host
        // wait for the project to be imported again.
        let response = service.dispatch(session.id(), &call).await.unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value()
        .to_string();
        assert!(reply.contains("Fixture user input"), "{reply}");
        assert!(!reply.contains("Changed outside Bex"));
    }
}
