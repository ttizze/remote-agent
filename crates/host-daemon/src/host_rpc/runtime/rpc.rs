use super::*;
use crate::host_rpc::routing::{HostReply, SessionId};
use agent_protocol::protocol::{self, Call, Response};

impl Runtime {
    pub(in crate::host_rpc) async fn dispatch(
        service: &HostRpcService,
        session: SessionId,
        call: &Call,
    ) -> Result<Option<HostReply>, Failure> {
        let reply = match call {
            Call::SubscribeList(request) => {
                let storage = service.inner.conversation.storage.clone();
                let providers = service
                    .inner
                    .providers
                    .all()
                    .into_iter()
                    .map(|(provider, _)| provider)
                    .collect::<Vec<_>>();
                let refresh = request.refresh;
                let result = async {
                    tokio::task::spawn_blocking(move || {
                        if refresh {
                            for provider in providers {
                                storage.queue_catalog(provider, now())?;
                            }
                        }
                        Ok::<_, String>(())
                    })
                    .await
                    .map_err(|error| error.to_string())??;
                    super::list::subscribe(
                        service.inner.conversation.storage.clone(),
                        request,
                        service
                            .provider_errors()
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .await
                }
                .await;
                return result.map(Some).map_err(storage_failure);
            }
            Call::SubscribeCommandOutput(request) => {
                let result = (|| {
                    let storage = &service.inner.conversation.storage;
                    let item = storage
                        .thread_item(&request.key.session, request.key.item.as_str())
                        .map_err(storage_failure)?
                        .ok_or_else(|| {
                            Failure::new("item_not_found", "command item does not exist")
                        })?;
                    let agent_protocol::orchestration::TurnItemBody::CommandExecution {
                        output,
                        ..
                    } = &item.body
                    else {
                        return Err(Failure::new("invalid_item", "item is not a command"));
                    };
                    let seed = if matches!(
                        item.status,
                        agent_protocol::orchestration::TurnItemStatus::Running
                            | agent_protocol::orchestration::TurnItemStatus::Waiting
                    ) {
                        storage
                            .thread_item_attempt(&item)
                            .map_err(storage_failure)?
                    } else {
                        None
                    };
                    let output = output.as_deref().unwrap_or_default();
                    let tail = agent_protocol::sync::Output::new(0, &output);
                    let (initial, updates) = match service.inner.conversation.outputs.subscribe(
                        &records::OutputKey {
                            session: SessionRef::new(None, request.key.session.application_id())
                                .map_err(|error| Failure::new("invalid_session", error))?,
                            item: request.key.item.clone(),
                        },
                        seed.map(|execution| (execution, tail.clone())),
                    ) {
                        Some((initial, updates)) => (
                            initial,
                            Some(super::super::routing::HostSubscription::Output(updates)),
                        ),
                        None => (tail, None),
                    };
                    Ok::<_, Failure>(HostReply {
                        initial: protocol::response_frame(Response::Success {
                            result: records::OpenedOutput {
                                key: request.key.clone(),
                                initial,
                                subscription_id: request.subscription,
                            }
                            .into(),
                        })
                        .map_err(|error| Failure::new("invalid_output", error.to_string()))?,
                        updates,
                    })
                })();
                return result.map(Some);
            }
            Call::SubscribeConversation(request) => {
                Self::ensure_imported(service, &request.session).await?;
                return Self::open(service, session, request).await.map(Some);
            }
            Call::CreateConversation(request) => {
                Self::create(service, request).await?;
                return Self::open(
                    service,
                    session,
                    &records::Subscribe {
                        session: request.session.clone(),
                        subscription: request.subscription,
                        after_sequence: None,
                    },
                )
                .await
                .map(Some);
            }
            Call::ForkConversation(request) => {
                let result = async {
                    Self::ensure_imported(service, &request.source).await?;
                    let _workspace = service.inner.worktree_access.read().await;
                    service.require_not_updating()?;
                    service
                        .inner
                        .conversation
                        .storage
                        .fork_thread(
                            &request.source,
                            &request.target,
                            &request.through,
                            &commands::at(),
                        )
                        .map_err(storage_failure)?;
                    Ok::<_, Failure>(request.target.clone())
                }
                .await;
                Response::Success {
                    result: result?.into(),
                }
                .into()
            }
            Call::ChangeConversation(request) => Response::Success {
                result: Self::change(service, request).await?.into(),
            }
            .into(),
            Call::CheckConversationCommands(request) => Response::Success {
                result: (service
                    .inner
                    .conversation
                    .storage
                    .receipts(&request.session, &request.operations)
                    .map_err(storage_failure)?)
                .into(),
            }
            .into(),
            Call::ExtendConversation(request) => Response::Success {
                result: service
                    .inner
                    .conversation
                    .storage
                    .thread_history(&request.session, &request.cursor)
                    .map_err(storage_failure)?
                    .into(),
            }
            .into(),
            Call::HydrateConversation(request) => {
                let item = service
                    .inner
                    .conversation
                    .storage
                    .thread_item(&request.session, request.item.as_str())
                    .map_err(storage_failure)?;
                let mut reply = records::Hydrated {
                    item,
                    transfer: None,
                };
                if let Some(item) = &reply.item {
                    let bytes = protocol::encode(item)
                        .map_err(|error| storage_failure(error.to_string()))?;
                    if bytes.len() > agent_protocol::models::MAX_INLINE_ITEM_BYTES {
                        reply.transfer = Some(
                            service
                                .files()
                                .download_bytes(session, bytes)
                                .await
                                .map_err(|error| Failure::new("item_transfer_failed", error))?,
                        );
                        reply.item = None;
                    }
                }
                Response::Success {
                    result: reply.into(),
                }
                .into()
            }
            _ => return Ok(None),
        };
        Ok(Some(reply))
    }

    async fn open(
        service: &HostRpcService,
        session: SessionId,
        request: &records::Subscribe,
    ) -> Result<HostReply, Failure> {
        let storage = &service.inner.conversation.storage;
        let thread = storage
            .thread_header(&request.session)
            .map_err(storage_failure)?
            .ok_or_else(|| Failure::new("conversation_not_found", "conversation does not exist"))?;
        let provider =
            serde_json::from_value(serde_json::Value::String(thread.provider_instance_id))
                .map_err(|error| Failure::new("invalid_provider", error))?;
        let capabilities =
            records::capabilities(service.inner.providers.get(provider)?.capabilities());
        let root = storage
            .thread_project_root(&request.session)
            .map_err(storage_failure)?;
        super::stream::subscribe(
            storage.clone(),
            request,
            provider,
            capabilities,
            root,
            service
                .inner
                .router
                .queue_budget(session)
                .map_err(storage_failure)?,
        )
        .await
        .map_err(storage_failure)
    }

    async fn create(service: &HostRpcService, request: &records::Create) -> Result<(), Failure> {
        use agent_protocol::orchestration as model;
        request
            .validate_creation()
            .map_err(|error| Failure::new("invalid_params", error))?;
        let storage = &service.inner.conversation.storage;
        if storage
            .thread_header(&request.session)
            .map_err(storage_failure)?
            .is_some()
        {
            if let Some((id, content)) = &request.initial {
                Self::change(
                    service,
                    &records::Change {
                        session: request.session.clone(),
                        operation: Uuid::parse_str(id.as_str()).expect("validated message ID"),
                        command: Command::Submit {
                            id: id.clone(),
                            content: content.clone(),
                        },
                    },
                )
                .await?
                .accepted()
                .map_err(|error| Failure::new("command_rejected", error.message))?;
            }
            return Ok(());
        }
        let _workspace = service.inner.worktree_access.read().await;
        service.require_not_updating()?;
        if let Some((_, content)) = &request.initial {
            service.creation_agent(request.provider, content.model.as_ref())?;
        }
        let selected =
            Self::default_selection(service, request.provider, &request.selection).await?;
        let cwd = match request.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
            Some(cwd) => cwd.to_owned(),
            None => service
                .inner
                .projects
                .create_chat_directory()
                .await
                .map_err(|error| Failure::new("workspace_unavailable", error))?
                .to_str()
                .ok_or_else(|| {
                    Failure::new("workspace_unavailable", "workspace path is not UTF-8")
                })?
                .to_owned(),
        };
        let projects = service.project_snapshot().await?;
        let project_id = match projects.project_membership(Some(&cwd)) {
            agent_protocol::models::ProjectMembership::Assigned(id) => Some(id),
            _ => None,
        };
        let strategy = match &request.workspace {
            Some(strategy) => Some(strategy.clone()),
            None if request.cwd.is_none() => {
                Some(agent_protocol::workspace::WorkspaceStrategy::Root)
            }
            None => {
                let settings = service
                    .inner
                    .worktrees
                    .settings(None)
                    .await
                    .map_err(|error| Failure::new("worktree_settings_failed", error))?;
                settings.create_on_new_session.then_some(
                    agent_protocol::workspace::WorkspaceStrategy::Worktree {
                        base_ref: String::new(),
                        start_from_origin: true,
                    },
                )
            }
        };
        let preparation = match strategy {
            Some(agent_protocol::workspace::WorkspaceStrategy::Worktree {
                base_ref,
                start_from_origin,
            }) => Some(model::WorkspacePreparation::Worktree {
                base_ref,
                start_from_origin,
                branch: None,
            }),
            Some(agent_protocol::workspace::WorkspaceStrategy::ExistingWorktree { path }) => {
                Some(model::WorkspacePreparation::ExistingWorktree {
                    worktree_path: path,
                    branch: None,
                })
            }
            _ => None,
        };
        let thread_id = request.session.application_id();
        let now = super::commands::at();
        let thread = model::Thread {
            id: thread_id.clone(),
            history_origin: None,
            created_by: model::Actor::User,
            creation_source: model::CreationSource::Mobile,
            project_id,
            title: String::new(),
            provider_instance_id: request.provider.key().into(),
            model_selection: selected,
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            branch: None,
            worktree_path: match &preparation {
                // T3 claims an existing worktree on thread creation, including
                // launches without an initial message (ThreadLaunchService).
                Some(model::WorkspacePreparation::ExistingWorktree { worktree_path, .. }) => {
                    Some(worktree_path.clone())
                }
                _ => Some(cwd),
            },
            active_provider_thread_id: None,
            lineage: model::Lineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: thread_id.clone(),
                side_chat: None,
            },
            forked_from: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            last_visited_at: None,
            deleted_at: None,
        };
        let message = if let Some((id, content)) = &request.initial {
            Some(Self::message_content(service, &thread_id, id.as_str(), &content.parts).await?)
        } else {
            None
        };
        let operation = request
            .initial
            .as_ref()
            .map(|(id, _)| id.to_string())
            .unwrap_or_else(|| {
                Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("thread.create:{thread_id}").as_bytes(),
                )
                .to_string()
            });
        let receipt = storage
            .thread_command(
                super::super::storage::commands::Command {
                    id: &operation,
                    thread_id: &thread_id,
                    kind: if message.is_some() {
                        "message.dispatch"
                    } else {
                        "thread.create"
                    },
                    now: &now,
                    item_ids: &[],
                },
                Some(&thread),
                |state| {
                    let Some((id, options)) = &request.initial else {
                        return Ok(model::Decision::default());
                    };
                    let content = message.as_ref().ok_or("message was not prepared")?;
                    let selected = super::commands::selection(
                        &thread.model_selection,
                        &records::Selection {
                            model: options.model.clone(),
                            effort: options.effort.clone(),
                            service_tier: options.service_tier.clone(),
                        },
                    );
                    model::admit(
                        state,
                        model::Input {
                            message_id: id.to_string(),
                            created_by: model::Actor::User,
                            creation_source: model::CreationSource::Mobile,
                            text: content.text.clone(),
                            attachments: content.attachments.clone(),
                            context: content.context.clone(),
                            model_selection: selected,
                            driver: request.provider.driver().into(),
                            workspace_preparation: preparation.clone(),
                        },
                        model::Admission::Queue,
                        model::Context {
                            command_id: &operation,
                            now: &now,
                            next_run_ordinal: 1,
                            can_steer: false,
                        },
                    )
                },
            )
            .map_err(storage_failure)?;
        if let Some(error) = receipt.error {
            return Err(Failure::new("input_rejected", error));
        }
        Ok(())
    }

    pub(in crate::host_rpc) async fn ensure_imported(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<(), Failure> {
        if service
            .inner
            .conversation
            .storage
            .thread_header(target)
            .map_err(storage_failure)?
            .is_some()
        {
            return Ok(());
        }
        if target.provider.is_none() {
            return Err(Failure::new(
                "session_not_found",
                "conversation does not exist",
            ));
        }
        Self::import_native(service, target).await
    }

    pub(super) async fn native_history(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<agent_protocol::models::ThreadResponse, Failure> {
        let agent = service.inner.providers.discovery(
            target
                .native_provider()
                .map_err(|error| Failure::new("invalid_session", error))?,
        )?;
        let mut response = agent.open(&target.id, 10, true).await?;
        op::validate_thread(&response, Some(target))
            .map_err(|error| Failure::new("invalid_thread", error))?;
        let mut seen = std::collections::HashSet::new();
        while let Some(cursor) = response.thread.history_cursor.take() {
            if !seen.insert(cursor.clone()) {
                return Err(Failure::new(
                    "invalid_history",
                    "provider repeated its history cursor",
                ));
            }
            let page = agent.read_history(&target.id, &cursor, true).await?;
            response.thread.turns = Some(agent_protocol::session::prepend_history(
                &page.turns,
                response.thread.turns.as_deref().unwrap_or_default(),
            ));
            if let Some(state) = page.history_read_state {
                let current = response
                    .thread
                    .history_read_state
                    .get_or_insert_with(|| state.clone());
                if matches!(
                    state.kind,
                    agent_protocol::session::HistoryReadKind::Incomplete
                        | agent_protocol::session::HistoryReadKind::Unavailable
                        | agent_protocol::session::HistoryReadKind::Loading
                ) {
                    current.kind = state.kind;
                }
                current.issues.extend(state.issues);
            }
            response.thread.history_cursor = page.next_cursor;
        }
        if let Some(state) = &mut response.thread.history_read_state {
            if state.kind == agent_protocol::session::HistoryReadKind::Partial
                && state.issues.is_empty()
            {
                state.kind = agent_protocol::session::HistoryReadKind::Complete;
            }
            state.issues.sort();
            state.issues.dedup();
        }
        let is_message = |item: &agent_protocol::items::Item| {
            matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
                    | agent_protocol::items::ItemBody::AssistantText { .. }
                    | agent_protocol::items::ItemBody::Reasoning { .. }
            )
        };
        let mut issues = Vec::new();
        for turn in response.thread.turns.iter_mut().flatten() {
            let turn = Arc::make_mut(turn);
            let mut kept = Vec::new();
            for item in turn.items.take().unwrap_or_default() {
                if !is_message(&item) {
                    continue;
                }
                let item = if item.is_deferred() {
                    match Self::read_native_body(
                        service,
                        &op::ReadItem {
                            thread_id: target.clone(),
                            turn_id: turn.id.clone(),
                            item_id: item.id.clone(),
                        },
                    )
                    .await
                    {
                        Ok(detail) => Arc::new(detail),
                        Err(error) => {
                            issues.push(format!("{}: {}", item.id, error.message));
                            item
                        }
                    }
                } else {
                    item
                };
                if is_message(&item) {
                    kept.push(item);
                }
            }
            turn.items = Some(kept);
        }
        if !issues.is_empty() {
            let state = response.thread.history_read_state.get_or_insert_with(|| {
                agent_protocol::session::HistoryReadState::new(
                    agent_protocol::session::HistoryReadKind::Partial,
                    vec![],
                )
            });
            if state.kind == agent_protocol::session::HistoryReadKind::Complete {
                state.kind = agent_protocol::session::HistoryReadKind::Partial;
            }
            state.issues.extend(issues);
        }
        response.thread.capabilities = Some(records::capabilities(
            service
                .inner
                .providers
                .get(
                    target
                        .native_provider()
                        .map_err(|error| Failure::new("invalid_session", error))?,
                )?
                .capabilities(),
        ));
        Ok(response)
    }
}
