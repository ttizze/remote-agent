use super::*;
use agent_protocol::{
    items::{ItemBody, MessagePart},
    orchestration as model,
};

use super::native_metadata::timestamp;
use futures_util::TryStreamExt;

// T3 AgentSessionScanner keeps the first human message and recent text history.
const PROJECT_MESSAGES: usize = 200;
const PROJECT_TRANSCRIPTS: usize = 100;
const PROJECT_HISTORY_BYTES: usize = 32 * 1024 * 1024;

fn message_text(body: &ItemBody) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match body {
        ItemBody::UserMessage {
            text: Some(text), ..
        }
        | ItemBody::AssistantText { text, .. } => Some(Cow::Borrowed(text)),
        ItemBody::UserMessage {
            text: None,
            content,
        } => Some(Cow::Owned(
            content
                .iter()
                .filter_map(|part| {
                    if let MessagePart::Text { text } = part {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        _ => None,
    }
}

/// Retain only importable text before hydrating deferred bodies or writing SQLite.
pub(super) fn project_messages(
    turns: Vec<Arc<agent_protocol::models::Turn>>,
    limit: usize,
) -> (Vec<Arc<agent_protocol::models::Turn>>, usize) {
    let eligible = |item: &agent_protocol::items::Item| {
        message_text(item.body()).is_some_and(|text| item.is_deferred() || !text.trim().is_empty())
    };
    let mut recent = std::collections::VecDeque::new();
    let mut first_user = None;
    let mut count = 0;
    let mut bytes = 0;
    for item in turns
        .iter()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter(|item| eligible(item))
    {
        if first_user.is_none() && matches!(item.body(), ItemBody::UserMessage { .. }) {
            first_user = Some(count);
        }
        let length = message_text(item.body()).map_or(0, |text| text.len());
        recent.push_back((count, length));
        count += 1;
        bytes += length;
        while recent.len() > limit || bytes > PROJECT_HISTORY_BYTES {
            let (_, length) = recent.pop_front().unwrap();
            bytes -= length;
        }
    }
    if first_user.is_none() {
        return (vec![], count);
    }
    if limit > 0
        && let Some(first) = first_user
        && !recent.iter().any(|(index, _)| *index == first)
    {
        if recent.len() == limit {
            recent.pop_front();
        }
        recent.push_front((first, 0));
    }
    let kept: std::collections::HashSet<_> = recent.into_iter().map(|(index, _)| index).collect();
    let omitted = count - kept.len();
    let mut position = 0;
    let turns = turns
        .into_iter()
        .filter_map(|mut turn| {
            let items = Arc::make_mut(&mut turn)
                .items
                .take()
                .unwrap_or_default()
                .into_iter()
                .filter(|item| {
                    if !eligible(item) {
                        return false;
                    }
                    let keep = kept.contains(&position);
                    position += 1;
                    keep
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                return None;
            }
            Arc::make_mut(&mut turn).items = Some(items);
            Some(turn)
        })
        .collect();
    (turns, omitted)
}

fn imported_item(
    thread: &str,
    id: String,
    ordinal: u64,
    at: &str,
    body: model::TurnItemBody,
) -> model::TurnItem {
    model::TurnItem {
        detail_omitted: false,
        id,
        thread_id: thread.into(),
        run_id: None,
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: model::TurnItemStatus::Completed,
        title: None,
        started_at: Some(at.into()),
        completed_at: Some(at.into()),
        updated_at: at.into(),
        body,
    }
}

impl Runtime {
    /// T3 AgentSessionImporter imports an unowned source once. Project scans
    /// never backfill or replace an existing conversation's settled transcript.
    pub(in crate::host_rpc) async fn import_project_threads(
        service: &HostRpcService,
        roots: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, Failure> {
        let projects = service.project_snapshot().await?;
        let selected: std::collections::BTreeMap<_, _> = roots
            .iter()
            .filter_map(|root| {
                projects
                    .project_for_workspace(root)
                    .map(|id| (id.to_owned(), root.clone()))
            })
            .collect();
        if selected.is_empty() {
            return Ok(Default::default());
        }
        let runtime = &service.inner.conversation;
        let owned = runtime.storage.active_sources().map_err(storage_failure)?;
        let owned_index: std::collections::HashSet<_> = owned.iter().collect();
        let mut candidates = Vec::new();
        let mut errors = std::collections::BTreeMap::new();
        for (provider, agent) in service.inner.providers.all() {
            if service.require_provider_enabled(provider).is_err() {
                continue;
            }
            // Finish discovery before loading transcripts. The scan deadline
            // must not expire while an unrelated transcript is being imported.
            let pages = crate::providers::native_pages(
                agent.as_ref(),
                crate::providers::SessionListScope::Roots {
                    owned: &owned,
                    directories: &projects.scratch_roots,
                },
                100,
                50,
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5)),
                None,
            );
            futures_util::pin_mut!(pages);
            loop {
                let page = match pages.try_next().await {
                    Ok(Some(page)) => page,
                    Ok(None) => break,
                    Err(error) => {
                        for root in selected.values() {
                            errors.insert(root.clone(), error.to_string());
                        }
                        break;
                    }
                };
                for summary in page {
                    if summary.parent_id.is_some() {
                        continue;
                    }
                    let Some(cwd) = summary.cwd.as_deref() else {
                        continue;
                    };
                    let Some(root) = projects
                        .project_for_workspace(cwd)
                        .and_then(|id| selected.get(id))
                    else {
                        continue;
                    };
                    let Some(native) = summary.id.filter(|id| id.provider == Some(provider)) else {
                        continue;
                    };
                    if !owned_index.contains(&native) {
                        candidates.push((
                            summary.updated_at.unwrap_or_default(),
                            native,
                            root.clone(),
                        ));
                    }
                }
            }
        }
        candidates.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        let mut seen = std::collections::HashSet::new();
        for (_, native, root) in candidates
            .into_iter()
            .filter(|(_, native, _)| seen.insert(native.clone()))
            .take(PROJECT_TRANSCRIPTS)
        {
            let provider = native
                .native_provider()
                .map_err(|error| Failure::new("invalid_session", error))?;
            // Creation and this one import serialize ownership. Other user
            // operations can use the provider between imported conversations.
            let _native = runtime.native_access[&provider].write().await;
            if runtime
                .storage
                .native_owner(&native)
                .map_err(storage_failure)?
                .is_some()
            {
                continue;
            }
            if let Err(error) =
                Self::import_native(service, &native, &native, Some(PROJECT_MESSAGES)).await
            {
                errors.insert(root, error.to_string());
            }
        }
        Ok(errors)
    }

    pub(super) async fn import_native(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
        message_limit: Option<usize>,
    ) -> Result<(), Failure> {
        let (response, omitted) = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            Self::native_history(service, native, message_limit),
        )
        .await
        .map_err(|_| Failure::new("history_unavailable", "native history read timed out"))??;
        if message_limit.is_some()
            && !response
                .thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .any(|item| matches!(item.body(), ItemBody::UserMessage { .. }))
        {
            return Ok(());
        }
        Self::import_native_response(service, target, native, &response, omitted).await
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
        let mut projects = service.project_snapshot().await?;
        if !projects
            .projects
            .iter()
            .any(|project| project.id == agent_protocol::models::SCRATCH_PROJECT_ID)
            && response.thread.cwd.as_ref().is_some_and(|cwd| {
                crate::projects::state::scratch_root(
                    &projects.scratch_roots,
                    std::path::Path::new(cwd),
                )
                .is_some()
            })
        {
            service
                .inner
                .projects
                .ensure_scratch()
                .await
                .map_err(|error| Failure::new("workspace_unavailable", error))?;
            projects = service.project_snapshot().await?;
        }
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
        omitted: usize,
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
                    ItemBody::UserMessage { .. } => model::TurnItemBody::UserMessage {
                        message_id,
                        created_by: thread.native_prompt_actor(),
                        creation_source: model::CreationSource::Server,
                        scheduled_task_id: None,
                        input_intent: "turn_start".into(),
                        text: message_text(item.body()).unwrap().into_owned(),
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
                items.push(imported_item(
                    &id,
                    format!("agent-session-import:v2:turn-item:{id}:{suffix}"),
                    items.len() as u64 + 1,
                    &at,
                    body,
                ));
            }
        }
        if omitted > 0 {
            let (noun, verb) = if omitted == 1 {
                ("message", "was")
            } else {
                ("messages", "were")
            };
            let body = model::TurnItemBody::SystemNotice {
                message: format!(
                    "{omitted} earlier {noun} {verb} not imported. This thread keeps the first user message and the most recent history. The original session transcript is unchanged."
                ),
            };
            items.insert(
                0,
                imported_item(
                    &id,
                    format!("agent-session-import:v2:history-truncated:{id}"),
                    0,
                    &thread.created_at,
                    body,
                ),
            );
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
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn project_import_reads_only_retained_messages_and_keeps_the_provider_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Claude);
        Arc::get_mut(&mut agent).unwrap().deferred_history = true;
        let native = SessionRef::new(ProviderKind::Claude, "project-source".into()).unwrap();
        let turns = (0..300)
            .map(|index| {
                Arc::new(Turn {
                    id: format!("turn-{index}").into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![
                        Arc::new(Item::new(
                            format!("user-{index}").into(),
                            ItemStatus::Completed,
                            ItemBody::UserMessage {
                                text: Some(format!("question-{index}")),
                                content: vec![],
                            },
                        )),
                        Arc::new(Item::new(
                            format!("reasoning-{index}").into(),
                            ItemStatus::Completed,
                            ItemBody::Reasoning {
                                summary: vec![],
                                content: vec![format!("private trace-{index}")],
                            },
                        )),
                        Arc::new(Item::new(
                            format!("commentary-{index}").into(),
                            ItemStatus::Completed,
                            ItemBody::AssistantText {
                                text: format!("commentary-{index}"),
                                phase: agent_protocol::items::AssistantPhase::Commentary,
                            },
                        )),
                        Arc::new(Item::new(
                            format!("answer-{index}").into(),
                            ItemStatus::Completed,
                            ItemBody::AssistantText {
                                text: format!("answer-{index}"),
                                phase: Default::default(),
                            },
                        )),
                    ]),
                    ..Default::default()
                })
            })
            .collect();
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    name: Some("Imported".into()),
                    cwd: Some(directory.path().to_str().unwrap().into()),
                    turns: Some(turns),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // These source bodies must never be requested during project import.
        for index in 0..300 {
            let mut unreadable = agent.unreadable.lock().unwrap();
            unreadable.insert(format!("reasoning-{index}"));
            if (1..=233).contains(&index) {
                unreadable.insert(format!("user-{index}"));
            }
            if index < 233 {
                unreadable.insert(format!("answer-{index}"));
            }
            if index <= 233 {
                unreadable.insert(format!("commentary-{index}"));
            }
        }
        let service =
            HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
        let connection = service.open_session();
        let import = Call::ImportProjects(agent_protocol::project_import::ImportProjects {
            paths: vec![directory.path().to_str().unwrap().into()],
        });
        let _: agent_protocol::project_import::ProjectImportResult =
            call(&service, connection.id(), import.clone())
                .await
                .unwrap();
        let state = super::super::native_fixture::canonical_snapshot(&service, &native).unwrap();
        let items = &state.projection.turn_items;
        assert_eq!(items.len(), 201);
        let mut messages = items
            .iter()
            .filter_map(|item| match &item.body {
                model::TurnItemBody::UserMessage { text, .. }
                | model::TurnItemBody::AssistantMessage { text, .. } => {
                    Some((item.ordinal, text.as_str()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        messages.sort_by_key(|(ordinal, _)| *ordinal);
        assert_eq!(messages.len(), 200);
        assert_eq!(messages[0].1, "question-0");
        assert_eq!(messages[1].1, "answer-233");
        assert_eq!(messages.last().unwrap().1, "answer-299");
        assert!(messages.iter().any(|(_, text)| *text == "commentary-234"));
        // Codex's compact view drops intermediate assistant messages.
        assert!(
            agent
                .history_activity
                .lock()
                .unwrap()
                .iter()
                .all(|include| *include)
        );
        let notice = items.iter().find(|item| matches!(&item.body, model::TurnItemBody::SystemNotice { message } if message.starts_with("700 earlier messages"))).expect("omission notice");
        assert_eq!(
            state.projection.visible_turn_items[0].source_item_id, notice.id,
            "the omission notice precedes retained history"
        );
        let reads = agent.body_reads.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(reads, 200, "only retained deferred bodies are hydrated");
        assert_eq!(
            agent.history_activity.lock().unwrap().len(),
            1,
            "Claude reads the transcript once, without re-reading it for history pages"
        );
        let _: agent_protocol::project_import::ProjectImportResult =
            call(&service, connection.id(), import).await.unwrap();
        assert_eq!(
            agent.body_reads.load(std::sync::atomic::Ordering::SeqCst),
            reads
        );
        assert_eq!(
            agent.records.lock().unwrap()[&native.id]
                .thread
                .turns
                .as_ref()
                .unwrap()
                .len(),
            300
        );

        // Explicit native history still keeps all messages and reasoning.
        let complete = SessionRef::new(ProviderKind::Claude, "complete-source".into()).unwrap();
        let mut transcript = agent.records.lock().unwrap()[&native.id].clone();
        transcript.thread.id = Some(complete.clone());
        agent
            .records
            .lock()
            .unwrap()
            .insert(complete.id.clone(), transcript);
        agent.unreadable.lock().unwrap().clear();
        Runtime::import_native(&service, &complete, &complete, None)
            .await
            .unwrap();
        let complete =
            super::super::native_fixture::canonical_snapshot(&service, &complete).unwrap();
        assert_eq!(complete.projection.turn_items.len(), 1200);
    }

    #[test]
    fn project_history_bounds_recent_bytes_without_losing_the_first_question() {
        let turns = vec![Arc::new(Turn {
            id: "large".into(),
            status: TurnStatus::Completed,
            items: Some(vec![
                Arc::new(Item::new(
                    "first".into(),
                    ItemStatus::Completed,
                    ItemBody::UserMessage {
                        text: Some("first question".into()),
                        content: vec![],
                    },
                )),
                Arc::new(Item::new(
                    "middle".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: "m".repeat(17 * 1024 * 1024),
                        phase: Default::default(),
                    },
                )),
                Arc::new(Item::new(
                    "latest".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: "l".repeat(17 * 1024 * 1024),
                        phase: Default::default(),
                    },
                )),
            ]),
            ..Default::default()
        })];
        let (kept, omitted) = project_messages(turns.clone(), PROJECT_MESSAGES);
        assert_eq!(omitted, 1);
        let ids = kept[0]
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["first", "latest"]);
        assert_eq!(
            turns[0].items.as_ref().unwrap().len(),
            3,
            "source projection stays immutable"
        );
    }

    #[test]
    fn project_history_keeps_all_two_hundred_messages_including_an_assistant_preamble() {
        let items = (0..200)
            .map(|index| {
                let text = format!("message-{index}");
                let body = if index == 1 {
                    ItemBody::UserMessage {
                        text: Some(text),
                        content: vec![],
                    }
                } else {
                    ItemBody::AssistantText {
                        text,
                        phase: Default::default(),
                    }
                };
                Arc::new(Item::new(
                    index.to_string().into(),
                    ItemStatus::Completed,
                    body,
                ))
            })
            .collect();
        let (turns, omitted) = project_messages(
            vec![Arc::new(Turn {
                id: "full-window".into(),
                items: Some(items),
                ..Default::default()
            })],
            PROJECT_MESSAGES,
        );
        assert_eq!(omitted, 0, "the 200-message window fits without omissions");
        let ids: Vec<_> = turns[0]
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.to_string())
            .collect();
        assert_eq!(
            ids,
            (0..200).map(|index| index.to_string()).collect::<Vec<_>>()
        );
    }

    #[rstest::rstest]
    #[case(0, 0)]
    #[case(1, 1)]
    #[case(101, 100)]
    #[tokio::test]
    async fn project_import_requires_a_question_and_caps_recent_transcripts(
        #[case] count: usize,
        #[case] expected: usize,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = NativeAgent::new(ProviderKind::Codex);
        Arc::get_mut(&mut agent).unwrap().deferred_history = true;
        for index in 0..count.max(1) {
            let native = SessionRef::new(ProviderKind::Codex, format!("source-{index}")).unwrap();
            let turns = if count == 0 {
                agent.unreadable.lock().unwrap().insert("orphan".into());
                vec![Arc::new(Turn {
                    id: "source-turn".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(Item::new(
                        "orphan".into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: "no human question".into(),
                            phase: Default::default(),
                        },
                    ))]),
                    ..Default::default()
                })]
            } else {
                vec![Arc::new(Turn {
                    id: "source-turn".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(Item::new(
                        "source-user".into(),
                        ItemStatus::Completed,
                        ItemBody::UserMessage {
                            text: Some(format!("question-{index}")),
                            content: vec![],
                        },
                    ))]),
                    ..Default::default()
                })]
            };
            agent.records.lock().unwrap().insert(
                native.id.clone(),
                ThreadResponse {
                    thread: Thread {
                        id: Some(native),
                        cwd: Some(directory.path().to_str().unwrap().into()),
                        updated_at: Some(index as f64),
                        turns: Some(turns),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        let service = HostRpcService::new(
            [
                agent.backend(),
                crate::providers::Backend::unavailable(ProviderKind::Claude, "not installed"),
            ],
            directory.path().join("projects.json"),
        );
        service
            .inner
            .projects
            .register(directory.path())
            .await
            .unwrap();
        Runtime::import_project_threads(&service, &[directory.path().to_str().unwrap().into()])
            .await
            .unwrap();
        let shell = service.inner.conversation.storage.shell_snapshot().unwrap();
        assert_eq!(shell.threads.len(), expected);
        assert_eq!(agent.body_reads.load(Ordering::SeqCst), expected);
        if count > 100 {
            let native = SessionRef::new(ProviderKind::Codex, "source-0".into()).unwrap();
            assert!(
                service
                    .inner
                    .conversation
                    .storage
                    .thread_header(&native)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            agent.inputs.lock().unwrap().is_empty(),
            "import never sends a native prompt"
        );
    }

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
                        fresh_context: false,
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
