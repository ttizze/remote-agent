use super::*;
use agent_protocol::{
    items::{ItemBody, MessagePart},
    orchestration as model,
};

use super::native_metadata::timestamp;
use futures_util::TryStreamExt;

impl Runtime {
    /// T3 AgentSessionImporter imports an unowned source once. Project scans
    /// never backfill or replace an existing conversation's settled transcript.
    pub(in crate::host_rpc) async fn import_project_threads(
        service: &HostRpcService,
        roots: &[String],
    ) -> Result<(), Failure> {
        let projects = service.project_snapshot().await?;
        let selected: std::collections::BTreeSet<_> = roots
            .iter()
            .filter_map(|root| projects.project_for_workspace(root).map(str::to_owned))
            .collect();
        if selected.is_empty() {
            return Ok(());
        }
        for (provider, _) in service.inner.providers.all() {
            if service.require_provider_enabled(provider).is_err() {
                continue;
            }
            let result = async {
                let runtime = &service.inner.conversation;
                // A pending private creation must publish ownership before a
                // native scanner can decide that its source is importable.
                let _native = runtime.native_access[&provider].write().await;
                let agent = service.inner.providers.discovery(provider)?;
                let owned = runtime.storage.active_sources().map_err(storage_failure)?;
                let pages = crate::providers::native_pages(
                    agent.as_ref(),
                    crate::providers::SessionListScope::Roots(&owned),
                    100,
                    100,
                    Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5)),
                    None,
                );
                futures_util::pin_mut!(pages);
                while let Some(page) = pages.try_next().await? {
                    for summary in page {
                        if summary.parent_id.is_some() {
                            continue;
                        }
                        let Some(cwd) = summary.cwd.as_deref() else {
                            continue;
                        };
                        if !projects
                            .project_for_workspace(cwd)
                            .is_some_and(|id| selected.contains(id))
                        {
                            continue;
                        }
                        let Some(native) = summary.id.filter(|id| id.provider == Some(provider))
                        else {
                            continue;
                        };
                        if runtime
                            .storage
                            .native_owner(&native)
                            .map_err(storage_failure)?
                            .is_some()
                        {
                            continue;
                        }
                        if let Err(error) = Self::import_native(service, &native, &native).await {
                            tracing::warn!(target:"agent",operation="project.import-native",%error);
                        }
                    }
                }
                Ok::<_, Failure>(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(target:"agent",operation="project.scan-native",%error);
            }
        }
        Ok(())
    }

    pub(super) async fn import_native(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
    ) -> Result<(), Failure> {
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            Self::native_history(service, native),
        )
        .await
        .map_err(|_| Failure::new("history_unavailable", "native history read timed out"))??;
        Self::import_native_response(service, target, native, &response).await
    }

    pub(in crate::host_rpc) async fn native_header_records(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
        response: &agent_protocol::models::ThreadResponse,
        now: &str,
    ) -> Result<(model::Thread, model::ProviderThread), Failure> {
        let provider = native
            .native_provider()
            .map_err(|error| Failure::new("invalid_session", error))?;
        op::validate_thread(response, Some(native))
            .map_err(|error| Failure::new("invalid_thread", error))?;
        let id = target.application_id();
        if response
            .model
            .as_ref()
            .is_some_and(|model| model.provider != provider || model.id.trim().is_empty())
        {
            return Err(Failure::new(
                "invalid_thread",
                "native model belongs to another provider",
            ));
        }
        let selected = super::commands::selection(
            &super::native_metadata::selection(provider, response.model.as_ref()),
            &records::Selection {
                model: None,
                effort: response.effort.clone(),
                service_tier: response.service_tier.clone(),
            },
        );
        let projects = service.project_snapshot().await?;
        let (mut thread, mut provider_thread) = super::native_metadata::records(
            &response.thread,
            selected,
            projects
                .project_membership(response.thread.cwd.as_deref())
                .as_ref()
                .cloned(),
            now,
        )?;
        thread.id = id.clone();
        if thread.lineage.parent_thread_id.is_none() {
            thread.lineage.root_thread_id = id.clone();
        }
        provider_thread.app_thread_id = Some(id.clone());
        Ok((thread, provider_thread))
    }

    /// The caller supplies the application address that authorized this read.
    /// A private native alias must not adopt or mutate its public owner.
    pub(in crate::host_rpc) async fn import_native_response(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
        response: &agent_protocol::models::ThreadResponse,
    ) -> Result<(), Failure> {
        if let Some(state) = &response.thread.history_read_state
            && state.kind != agent_protocol::session::HistoryReadKind::Complete
        {
            return Err(Failure::new(
                "history_unavailable",
                if state.issues.is_empty() {
                    "native history is incomplete".into()
                } else {
                    state.issues.join("\n")
                },
            ));
        }
        let now = commands::at();
        let id = target.application_id();
        let (thread, mut provider_thread) =
            Self::native_header_records(service, target, native, response, &now).await?;
        // An imported transcript is static history, as in AgentSessionImporter.
        // Native file activity does not create an app-owned running resource.
        provider_thread.status = model::ProviderThreadStatus::Idle;
        let mut items = Vec::new();
        for turn in response.thread.turns.iter().flatten() {
            let at = timestamp(
                turn.started_at_ms
                    .or_else(|| turn.started_at.map(|at| (at * 1000.0) as u64)),
                &now,
            );
            for item in turn.items.iter().flatten() {
                if item.is_deferred() {
                    return Err(Failure::new(
                        "history_unavailable",
                        "native history contains an unread message body",
                    ));
                }
                let suffix = format!("{:06}", items.len());
                let message_id = format!("{id}:{suffix}");
                let body = match item.body() {
                    // T3 imports the native transcript's text, without copying
                    // native attachment files or persisting their old paths.
                    ItemBody::UserMessage { text, content } => model::TurnItemBody::UserMessage {
                        message_id,
                        created_by: model::Actor::User,
                        creation_source: model::CreationSource::Server,
                        input_intent: "turn_start".into(),
                        text: text.clone().unwrap_or_else(|| {
                            content
                                .iter()
                                .filter_map(|part| match part {
                                    MessagePart::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }),
                        attachments: vec![],
                        context: None,
                    },
                    ItemBody::AssistantText { text, .. } => model::TurnItemBody::AssistantMessage {
                        message_id,
                        text: text.clone(),
                        phase: None,
                        attachments: vec![],
                        streaming: false,
                    },
                    ItemBody::Reasoning { content, summary } => model::TurnItemBody::Reasoning {
                        text: summary
                            .iter()
                            .chain(content)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        streaming: false,
                    },
                    _ => continue,
                };
                if matches!(&body,model::TurnItemBody::UserMessage {text,..}|model::TurnItemBody::AssistantMessage {text,..}|model::TurnItemBody::Reasoning {text,..} if text.trim().is_empty())
                {
                    continue;
                }
                items.push(model::TurnItem {
                    detail_omitted: false,
                    id: format!("agent-session-import:v2:turn-item:{id}:{suffix}"),
                    thread_id: id.clone(),
                    run_id: None,
                    node_id: None,
                    provider_thread_id: None,
                    provider_turn_id: None,
                    native_item_ref: None,
                    parent_item_id: None,
                    ordinal: items.len() as u64 + 1,
                    status: model::TurnItemStatus::Completed,
                    title: None,
                    started_at: Some(at.clone()),
                    completed_at: Some(at.clone()),
                    updated_at: at.clone(),
                    body,
                });
            }
        }
        service
            .inner
            .conversation
            .storage
            .import_native_history(&thread, &provider_thread, items, &now)
            .map_err(storage_failure)
    }
}

#[cfg(test)]
mod tests {
    use super::super::native_fixture::{NativeAgent, Transcript, call};
    use super::*;
    use agent_protocol::conversation as decision;
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{Item, ItemBody},
        models::{Thread, ThreadResponse, Turn},
        protocol::Call,
    };

    #[rstest::rstest]
    #[case(agent_protocol::session::HistoryReadKind::Incomplete)]
    #[case(agent_protocol::session::HistoryReadKind::Unavailable)]
    #[case(agent_protocol::session::HistoryReadKind::Loading)]
    #[case(agent_protocol::session::HistoryReadKind::Partial)]
    #[tokio::test]
    async fn incomplete_native_reads_can_retry_and_text_import_does_not_require_old_image_files(
        #[case] kind: agent_protocol::session::HistoryReadKind,
    ) {
        use agent_protocol::session::{HistoryReadKind, HistoryReadState};
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Codex);
        Arc::get_mut(&mut agent).unwrap().deferred_history = true;
        agent
            .unreadable
            .lock()
            .unwrap()
            .insert("non-message-image".into());
        let native = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    history_read_state: Some(HistoryReadState::new(
                        kind,
                        vec!["transcript unavailable".into()],
                    )),
                    turns: Some(vec![Arc::new(Turn {
                        id: "history".into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![
                            Arc::new(Item::new(
                                "non-message-image".into(),
                                ItemStatus::Completed,
                                ItemBody::ImageView {
                                    path: "/missing-view.png".into(),
                                },
                            )),
                            Arc::new(Item::new(
                                "input".into(),
                                ItemStatus::Completed,
                                ItemBody::UserMessage {
                                    text: Some("keep this question".into()),
                                    content: vec![
                                        MessagePart::Image {
                                            source: "/removed.png".into(),
                                        },
                                        MessagePart::Image {
                                            source: "data:image/png;base64,old".into(),
                                        },
                                    ],
                                },
                            )),
                            Arc::new(Item::new(
                                "reasoning".into(),
                                ItemStatus::Completed,
                                ItemBody::Reasoning {
                                    summary: vec!["summary".into()],
                                    content: vec!["detail".into()],
                                },
                            )),
                            Arc::new(Item::new(
                                "answer".into(),
                                ItemStatus::Completed,
                                ItemBody::AssistantText {
                                    text: "keep this answer".into(),
                                    phase: Default::default(),
                                },
                            )),
                        ]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        assert!(Runtime::ensure_imported(&service, &native).await.is_err());
        assert!(
            service
                .inner
                .conversation
                .storage
                .thread_header(&native)
                .unwrap()
                .is_none()
        );
        agent
            .records
            .lock()
            .unwrap()
            .get_mut(&native.id)
            .unwrap()
            .thread
            .history_read_state = Some(HistoryReadState::new(HistoryReadKind::Complete, vec![]));
        Runtime::ensure_imported(&service, &native).await.unwrap();
        let stream = service
            .inner
            .conversation
            .storage
            .thread_stream(&native, None)
            .unwrap();
        let model::projection::ThreadStreamItem::Snapshot(snapshot) = &stream[0] else {
            panic!("snapshot")
        };
        assert_eq!(snapshot.projection.turn_items.len(), 3);
        assert!(snapshot.projection.turn_items.iter().any(|item|matches!(&item.body,model::TurnItemBody::Reasoning {text,..} if text=="summary\n\ndetail")));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("removed.png"));
        assert!(!encoded.contains("base64,old"));
        assert!(agent.inputs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn opening_native_history_keeps_runless_messages_once_and_continues_the_same_native_thread()
     {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex);
        let native = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        let mut turns = Vec::new();
        for index in 0..2 {
            turns.push(Arc::new(Turn {
                id: format!("native-{index}").into(),
                status: TurnStatus::Completed,
                items: Some(vec![
                    Arc::new(Item::new(
                        format!("user-{index}").into(),
                        ItemStatus::Completed,
                        ItemBody::UserMessage {
                            text: Some(format!("question {index}")),
                            content: vec![],
                        },
                    )),
                    Arc::new(Item::new(
                        format!("answer-{index}").into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: format!("answer {index}"),
                            phase: Default::default(),
                        },
                    )),
                ]),
                ..Default::default()
            }));
        }
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    name: Some("Native history".into()),
                    turns: Some(turns),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        for _ in 0..2 {
            let opened: records::Opened = call(
                &service,
                connection.id(),
                Call::SubscribeConversation(records::Subscribe {
                    session: native.clone(),
                    subscription: Uuid::new_v4(),
                    after_sequence: None,
                }),
            )
            .await
            .unwrap();
            let transcript = Transcript::opened(opened);
            assert!(transcript.snapshot.projection.runs.is_empty());
            assert_eq!(transcript.snapshot.projection.turn_items.len(), 4);
            assert_eq!(transcript.project().unwrap().thread.turns.unwrap().len(), 2);
        }
        assert!(agent.inputs.lock().unwrap().is_empty());
        let operation = Uuid::new_v4();
        let ack = Runtime::change(
            &service,
            &records::Change {
                session: native.clone(),
                operation,
                command: Command::Submit {
                    id: operation.to_string().into(),
                    content: decision::InputContent {
                        parts: vec![op::Input::Text {
                            text: "continue".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(ack.error, None);
        let storage = &service.inner.conversation.storage;
        let mut changed = storage.changes();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = storage.thread_state(&native.application_id(), &[]).unwrap();
                if state
                    .runs
                    .values()
                    .any(|run| run.status == model::RunStatus::Completed)
                {
                    break;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let inputs = agent.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].thread_id, native);
        let items = storage.thread_stream(&native, None).unwrap();
        let model::projection::ThreadStreamItem::Snapshot(snapshot) = &items[0] else {
            panic!("snapshot")
        };
        assert_eq!(snapshot.projection.runs.len(), 1);
        assert_eq!(
            snapshot
                .projection
                .turn_items
                .iter()
                .filter(|item| matches!(
                    item.body,
                    model::TurnItemBody::UserMessage { .. }
                        | model::TurnItemBody::AssistantMessage { .. }
                ))
                .count(),
            6
        );
        assert_eq!(
            snapshot
                .projection
                .turn_items
                .iter()
                .filter(|item| item.run_id.is_none())
                .count(),
            4
        );
    }
}
