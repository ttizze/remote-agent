use super::*;
use crate::host_rpc::routing::{HostReply, SessionId};
use agent_protocol::protocol::{self, Call, Response};

impl Runtime {
    pub(in crate::host_rpc) async fn dispatch(
        service: &HostRpcService,
        session: SessionId,
        call: &Call,
    ) -> Result<Option<HostReply>, String> {
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
                return Ok(Some(match result {
                    Ok(reply) => reply,
                    Err(error) => {
                        Response::from_result::<(), _>(Err(storage_failure(error))).into()
                    }
                }));
            }
            Call::SubscribeCommandOutput(request) => {
                let result = (|| {
                    let item = service
                        .inner
                        .conversation
                        .storage
                        .item(&request.key.session, &request.key.item)
                        .map_err(storage_failure)?
                        .ok_or_else(|| {
                            Failure::new("item_not_found", "command item does not exist")
                        })?;
                    let agent_protocol::items::ItemBody::CommandExecution { output, .. } =
                        item.body
                    else {
                        return Err(Failure::new("invalid_item", "item is not a command"));
                    };
                    let seed = if matches!(
                        item.status,
                        agent_protocol::execution::ItemStatus::Running
                            | agent_protocol::execution::ItemStatus::Unknown
                    ) {
                        service
                            .inner
                            .conversation
                            .storage
                            .state(&request.key.session)
                            .map_err(storage_failure)?
                            .attempts
                            .get(&item.turn)
                            .map(|attempt| attempt.id)
                    } else {
                        None
                    };
                    let tail = agent_protocol::sync::Output::default()
                        .apply(agent_protocol::sync::OutputUpdate::Tail(
                            agent_protocol::sync::Output {
                                offset: 0,
                                text: output,
                            },
                        ))
                        .map_err(|error| Failure::new("invalid_output", error.to_string()))?;
                    let (initial, updates) = match service.inner.conversation.outputs.subscribe(
                        &request.key,
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
                return Ok(Some(match result {
                    Ok(reply) => reply,
                    Err(error) => Response::from_result::<(), _>(Err(error)).into(),
                }));
            }
            Call::SubscribeConversation(request) => {
                let result = async {
                    Self::ensure_imported(service, &request.session).await?;
                    service
                        .inner
                        .conversation
                        .owners
                        .subscribe(
                            &request.session,
                            session,
                            request.subscription,
                            request.cached,
                            request.window,
                            service
                                .inner
                                .router
                                .queue_budget(session)
                                .map_err(storage_failure)?,
                        )
                        .await
                        .map_err(storage_failure)
                }
                .await;
                return Ok(Some(Self::opened(&request.session, result)?));
            }
            Call::CreateConversation(request) => {
                let result = async {
                    Self::create(service, request).await?;
                    service
                        .inner
                        .conversation
                        .owners
                        .subscribe(
                            &request.session,
                            session,
                            request.subscription,
                            None,
                            None,
                            service
                                .inner
                                .router
                                .queue_budget(session)
                                .map_err(storage_failure)?,
                        )
                        .await
                        .map_err(storage_failure)
                }
                .await;
                return Ok(Some(Self::opened(&request.session, result)?));
            }
            Call::ForkConversation(request) => {
                let result = async {
                    Self::await_import(service, &request.source).await?;
                    let _workspace = service.inner.worktree_access.read().await;
                    service.require_not_updating()?;
                    service
                        .inner
                        .conversation
                        .storage
                        .fork(&request.source, &request.target, &request.through, now())
                        .map_err(storage_failure)?;
                    Ok::<_, Failure>(request.target.clone())
                }
                .await;
                Response::from_result(result).into()
            }
            Call::ChangeConversation(request) => {
                Response::from_result(Self::change(service, request).await).into()
            }
            Call::CheckConversationCommands(request) => Response::from_result(
                service
                    .inner
                    .conversation
                    .storage
                    .receipts(&request.session, &request.operations)
                    .map_err(storage_failure),
            )
            .into(),
            Call::ExtendConversation(request) => Response::from_result(
                service
                    .inner
                    .conversation
                    .owners
                    .read(
                        &request.session,
                        session,
                        request.subscription,
                        request.cached,
                        None,
                    )
                    .await
                    .map_err(storage_failure),
            )
            .into(),
            Call::HydrateConversation(request) => {
                let result = async {
                    let mut messages = service
                        .inner
                        .conversation
                        .owners
                        .read(
                            &request.session,
                            session,
                            request.subscription,
                            request.cached,
                            Some(request.item.clone()),
                        )
                        .await
                        .map_err(storage_failure)?;
                    if messages.last().is_some_and(|message| matches!(message,
                        records::Message::Hydrate { row, .. } if matches!(&row.content,
                            agent_protocol::sync::Content::Full(value) | agent_protocol::sync::Content::Summary(value)
                            if matches!(value.as_ref(), records::Record::Item(item) if item.content.is_deferred()))))
                    {
                        Self::hydrate_native(service, &request.session, &request.item).await?;
                        messages = service.inner.conversation.owners.read(
                            &request.session, session, request.subscription, request.cached,
                            Some(request.item.clone()),
                        ).await.map_err(storage_failure)?;
                    }
                    let mut transfer = None;
                    if let Some(records::Message::Hydrate { row, .. }) = messages.last_mut() {
                        let value = match &row.content {
                            agent_protocol::sync::Content::Full(value)
                            | agent_protocol::sync::Content::Summary(value) => value,
                        };
                        if let records::Record::Item(item) = value.as_ref() {
                            let bytes = protocol::encode(item.content.as_ref())
                                .map_err(|error| storage_failure(error.to_string()))?;
                            if bytes.len() > agent_protocol::models::MAX_INLINE_ITEM_BYTES {
                                transfer = Some(
                                    service
                                        .files()
                                        .download_bytes(session, bytes)
                                        .await
                                        .map_err(|error| {
                                            Failure::new("item_transfer_failed", error)
                                        })?,
                                );
                                records::defer(row);
                            }
                        }
                    }
                    Ok::<_, Failure>(records::Hydrated { messages, transfer })
                }
                .await;
                Response::from_result(result).into()
            }
            _ => return Ok(None),
        };
        Ok(Some(reply))
    }

    fn opened(
        target: &SessionRef,
        result: Result<super::super::owners::Opened, Failure>,
    ) -> Result<HostReply, String> {
        match result {
            Ok(opened) => Ok(HostReply {
                initial: protocol::response_frame(Response::Success {
                    result: records::Opened {
                        session: target.clone(),
                        subscription_id: opened.subscription.id,
                        initial: opened.initial,
                    }
                    .into(),
                })
                .map_err(|error| error.to_string())?,
                updates: Some(super::super::routing::HostSubscription::Conversation(
                    opened.subscription,
                )),
            }),
            Err(error) => Ok(Response::from_result::<(), _>(Err(error)).into()),
        }
    }

    async fn create(service: &HostRpcService, request: &records::Create) -> Result<(), Failure> {
        request
            .validate_creation()
            .map_err(|error| Failure::new("invalid_params", error))?;
        let event = match &request.initial {
            Some((id, content)) => Event::Command {
                id: decision::OperationId::Client(
                    Uuid::parse_str(id.as_str()).expect("validated initial input"),
                ),
                command: Command::Submit {
                    id: id.clone(),
                    content: content.clone(),
                },
                receipt: None,
            },
            None => Event::Wake,
        };
        if service
            .inner
            .conversation
            .storage
            .lookup(&request.session)
            .map_err(storage_failure)?
            .is_some()
        {
            if let Event::Command {
                id: decision::OperationId::Client(operation),
                command,
                ..
            } = event
            {
                let reply = Self::change(
                    service,
                    &records::Change {
                        session: request.session.clone(),
                        operation,
                        command,
                    },
                )
                .await?;
                if let Outcome::Rejected(reason) = reply.outcome {
                    return Err(Failure::new("input_rejected", reason));
                }
            }
            return Ok(());
        }
        let agent = service.creation_agent(request.provider, request.selection.model.as_ref())?;
        let header = records::Header {
            state: decision::Thread {
                name: String::new(),
                workspace: request
                    .cwd
                    .clone()
                    .map(decision::Workspace::Folder)
                    .unwrap_or(decision::Workspace::Missing),
                provider: request.provider,
                queue_held: false,
            },
            details: records::Details {
                selection: request.selection.clone(),
                capabilities: records::capabilities(agent.capabilities()),
                updated_at: now(),
                project: service
                    .project_snapshot()
                    .await?
                    .project_membership(request.cwd.as_deref()),
                ..Default::default()
            },
        };
        let runtime = &service.inner.conversation;
        runtime
            .recovery(&request.session)
            .get_or_init(|| async {})
            .await;
        let (outcome, _) = runtime
            .owners
            .input(
                &request.session,
                event,
                Self::context(service, Some(request.provider)),
                Some(header),
            )
            .await
            .map_err(storage_failure)?;
        if let Outcome::Rejected(reason) = outcome {
            return Err(Failure::new("input_rejected", reason));
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
            .loaded(target)
            .map_err(storage_failure)?
        {
            return Ok(());
        }
        service.inner.providers.get(
            target
                .native_provider()
                .map_err(|error| Failure::new("invalid_session", error))?,
        )?;
        let runtime = &service.inner.conversation;
        runtime.recovery(target).get_or_init(|| async {}).await;
        runtime
            .owners
            .import_status(target, None, now())
            .await
            .map_err(storage_failure)?;
        Ok(())
    }
    pub(in crate::host_rpc) async fn await_import(
        service: &HostRpcService,
        target: &SessionRef,
    ) -> Result<(), Failure> {
        Self::ensure_imported(service, target).await?;
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        loop {
            if storage.loaded(target).map_err(storage_failure)? {
                return Ok(());
            }
            if let Some(state) = storage
                .header(target)
                .map_err(storage_failure)?
                .details
                .history_read_state
                && state.kind == agent_protocol::session::HistoryReadKind::Unavailable
            {
                return Err(Failure::new("history_unavailable", state.issues.join("\n")));
            }
            changed
                .changed()
                .await
                .map_err(|error| storage_failure(error.to_string()))?;
        }
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
        let mut issues = Vec::new();
        for turn in response.thread.turns.iter_mut().flatten() {
            let turn = Arc::make_mut(turn);
            let mut kept = Vec::new();
            for item in turn.items.take().unwrap_or_default() {
                if !item.is_history_message() {
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
                if item.is_history_message() {
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
