use super::*;
use crate::host_rpc::storage::jobs::{Claim, Work};

pub(in crate::host_rpc) const CONTINUE: &str = "Bexの更新に伴う再起動で作業が中断されました。この同じ会話の履歴から進捗と未完了の作業を確認し、中断したところから続けてください。元の依頼を最初からやり直したり、代わりの会話を作成したりしないでください。";

impl Runtime {
    pub(super) async fn stopped(
        service: &HostRpcService,
        provider: ProviderKind,
        error: &agent_protocol::execution::ExecutionError,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let _requests = scopeguard::guard(provider, |provider| {
            runtime
                .requests
                .lock()
                .unwrap()
                .retain(|_, request| request.attempt.provider != provider);
        });
        let mut sources: Vec<_> = runtime
            .resources
            .lock()
            .unwrap()
            .iter()
            .filter(|(native, _)| native.provider == Some(provider))
            .map(|(native, source)| (native.clone(), source.clone()))
            .collect();
        let running = runtime
            .storage
            .running_sources(provider)
            .unwrap_or_else(|error| {
                tracing::error!(target:"agent",operation="conversation.stopped.sources",%error);
                Vec::new()
            });
        for (target, native) in running {
            if !sources.iter().any(|(known, _)| known == &native) {
                sources.push((native.clone(), runtime.register_native(&target, &native)));
            }
        }
        for (native, source) in sources {
            let result = async {
            let _updates = source.updates.lock().await;
            Self::flush_text(service, &source).await?;
            for execution in runtime.storage.source_executions(&source.target, &native)? {
                let result = async {
                let attempt = execution.attempt;
                Self::settle_command_outputs(service, &source.target, attempt.id,
                    execution.native_turn,
                    agent_protocol::execution::TurnStatus::Failed).await?;
                runtime
                    .storage
                    .fence_execution(&source.target, attempt.id)?;
                let state = runtime.storage.state(&source.target)?;
                let pending = state.inputs.values().find_map(|input| match &input.state {
                    decision::InputState::Delivering(delivery)
                    | decision::InputState::Unknown { delivery, .. }
                        if delivery.attempt.id == attempt.id =>
                    {
                        Some((input.id.clone(), delivery.action))
                    }
                    _ => None,
                });
                let error = error.clone();
                if let Some((id, action)) = pending {
                    Self::event(
                        service,
                        &source.target,
                        Event::DeliveryFailed {
                            id: id.clone(),
                            action,
                            unknown: true,
                            error,
                        },
                    )
                    .await?;
                    Self::event(
                        service,
                        &source.target,
                        Event::DeliveryFenced { id, action },
                    )
                    .await?;
                } else {
                    Self::event(
                        service,
                        &source.target,
                        Event::ExecutionLost {
                            attempt: attempt.id,
                            error,
                        },
                    )
                    .await?;
                }
                Ok::<_, String>(())
                }.await;
                if let Err(error) = result {
                    tracing::error!(target:"agent",operation="conversation.stopped.execution",%error);
                }
            }
            Ok::<_, String>(())
            }.await;
            if let Err(error) = result {
                tracing::error!(target:"agent",operation="conversation.stopped.source",%error);
            }
        }
        let fenced = runtime.storage.fence_provider(provider);
        runtime
            .resources
            .lock()
            .unwrap()
            .retain(|native, _| native.provider != Some(provider));
        fenced
    }

    pub(in crate::host_rpc) async fn recover(
        service: &HostRpcService,
        target: &SessionRef,
        provider: ProviderKind,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        runtime
            .recovery(target)
            .get_or_try_init(|| async {
                runtime
                    .owners
                    .input(
                        target,
                        Event::Restart {
                            updating: runtime.updating_recovery,
                            continue_after_restart: service
                                .inner
                                .conversation
                                .storage
                                .header(target)?
                                .details
                                .parent
                                .is_none(),
                        },
                        Self::context(service, Some(provider)),
                        None,
                    )
                    .await?;
                if let Err(error)=service.update_recovery_committed(target).await {
                    tracing::error!(target:"agent",operation="conversation.recovery.report",%target,%error);
                }
                Ok::<_, String>(())
            })
            .await?;
        Ok(())
    }

    pub(super) fn start_recovery(service: &HostRpcService) {
        for (provider, _) in service.inner.providers.all() {
            if let Err(error) = service
                .inner
                .conversation
                .storage
                .queue_catalog(provider, now())
            {
                tracing::error!(target:"agent",operation="catalog.start",%error);
            }
        }
        let owner = Arc::downgrade(&service.inner);
        tokio::spawn(async move {
            let Some(inner) = owner.upgrade() else {
                return;
            };
            let service = HostRpcService { inner };
            match service.inner.conversation.storage.identities() {
                Ok(targets) => {
                    for target in targets {
                        let result = async {
                            let header = service.inner.conversation.storage.header(&target)?;
                            Self::recover(&service, &target, header.state.provider).await
                        }
                        .await;
                        if let Err(error) = result {
                            tracing::error!(target:"agent",operation="conversation.recovery",%target,%error);
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(target:"agent",operation="conversation.recovery.list",%error)
                }
            }
            if let Err(error) = service.inner.conversation.storage.set_updating(false) {
                tracing::error!(target:"agent",operation="conversation.recovery.update",%error);
            }
            drop(service);
            let mut jobs = tokio::task::JoinSet::new();
            loop {
                while let Some(result) = jobs.try_join_next() {
                    if let Err(error) = result {
                        tracing::error!(target:"agent",operation="conversation.jobs",%error);
                    }
                }
                let Some(inner) = owner.upgrade() else {
                    break;
                };
                let service = HostRpcService { inner };
                let claims = service
                    .inner
                    .conversation
                    .storage
                    .claim(now(), (16 - jobs.len()) as u32);
                match claims {
                    Ok(claims) => {
                        for claim in claims {
                            let service = service.clone();
                            jobs.spawn(async move {
                                Self::run_job(&service, claim).await;
                            });
                        }
                    }
                    Err(error) => {
                        tracing::error!(target: "agent", operation="conversation.jobs.claim", %error)
                    }
                }
                drop(service);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });
    }

    pub(super) async fn run_job(service: &HostRpcService, claim: Claim) {
        let storage = &service.inner.conversation.storage;
        let result = match &claim.work {
            Work::ProjectsChanged => {
                async {
                    let projects = service
                        .project_snapshot()
                        .await
                        .map_err(|error| error.to_string())?;
                    for (id, header, _) in storage.headers(false)? {
                        if let Some(cwd) = header.state.workspace.cwd() {
                            service
                                .inner
                                .conversation
                                .owners
                                .workspace(
                                    &id,
                                    cwd.to_owned(),
                                    projects.project_membership(Some(cwd)),
                                )
                                .await?;
                        }
                    }
                    storage.finish_job(claim.id, claim.attempt)?;
                    Ok::<_, String>(())
                }
                .await
            }
            Work::Catalog {
                provider, cursor, ..
            } => {
                async {
                    let agent = service
                        .inner
                        .providers
                        .discovery(*provider)
                        .map_err(|error| error.to_string())?;
                    let _catalog = service.inner.conversation.catalog_access[provider]
                        .write()
                        .await;
                    let owned = storage.active_sources()?;
                    let page = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        crate::providers::Discovery::list(
                            agent.as_ref(),
                            "",
                            cursor.clone(),
                            crate::providers::SessionListScope::WithSubagents(&owned),
                            100,
                        ),
                    )
                    .await
                    .map_err(|_| "provider catalog timed out".to_owned())?
                    .map_err(|error| error.to_string())?;
                    let projects = service
                        .project_snapshot()
                        .await
                        .map_err(|error| error.to_string())?;
                    let headers = page
                        .data
                        .into_iter()
                        .filter_map(|summary| {
                            let mut thread = summary.thread;
                            let id = thread.id.clone()?;
                            thread.project_id = projects.project_membership(thread.cwd.as_deref());
                            let response = agent_protocol::models::ThreadResponse {
                                thread,
                                ..Default::default()
                            };
                            let mut header =
                                super::super::storage::history::header(&response, now());
                            header.details.preview = header
                                .state
                                .name
                                .trim()
                                .is_empty()
                                .then(|| {
                                    header
                                        .details
                                        .preview
                                        .as_deref()
                                        .map(|preview| preview.chars().take(120).collect())
                                })
                                .flatten();
                            header.details.capabilities = records::capabilities(
                                service.inner.providers.get(*provider).ok()?.capabilities(),
                            );
                            Some((id, header))
                        })
                        .collect::<Vec<_>>();
                    storage.catalog_page(&claim, &headers, page.next_cursor, now())
                }
                .await
            }
            Work::Import { target, source } => {
                async {
                    let _retire = scopeguard::guard(source.clone(), |source| {
                        service.inner.conversation.retire_source(&source);
                    });
                    let resource = service.inner.conversation.register_native(target, source);
                    let _io = resource.io.lock().await;
                    let mut response = match Self::native_history(service, source).await {
                        Ok(response) => response,
                        Err(error) => {
                            service
                                .inner
                                .conversation
                                .owners
                                .import_failed(target, claim.id, claim.attempt, error.to_string())
                                .await?;
                            return Ok(());
                        }
                    };
                    response.thread.project_id = service
                        .project_snapshot()
                        .await
                        .map_err(|error| error.to_string())?
                        .project_membership(response.thread.cwd.as_deref());
                    service
                        .inner
                        .conversation
                        .owners
                        .import(
                            target,
                            source.clone(),
                            claim.id,
                            claim.attempt,
                            response,
                            now(),
                        )
                        .await?;
                    Self::event(service, target, Event::Wake).await?;
                    Ok::<_, String>(())
                }
                .await
            }
            Work::Continue { target, source } => {
                async {
                    let selection = storage.header(target)?.details.selection;
                    let (outcome, _) = Self::event(
                        service,
                        target,
                        Event::Command {
                            id: decision::OperationId::Job(claim.id),
                            receipt: None,
                            command: Command::ContinueAfterRestart {
                                source: source.clone(),
                                enabled: true,
                                content: decision::InputContent {
                                    parts: vec![op::Input::Text {
                                        text: CONTINUE.into(),
                                    }],
                                    model: selection.model,
                                    effort: selection.effort,
                                    service_tier: selection.service_tier,
                                    follow_up: Default::default(),
                                },
                            },
                        },
                    )
                    .await?;
                    service
                        .continuation_scheduled(target, source, claim.id, outcome)
                        .await?;
                    storage.delivered(claim.id, claim.attempt)?;
                    Ok::<_, String>(())
                }
                .await
            }
        };
        if let Err(error) = result {
            if let Work::Catalog { provider, .. } = claim.work
                && let Err(error) = storage.catalog_failed(&claim, provider)
            {
                tracing::error!(target:"agent",operation="catalog.failure",%error);
            }
            let delay = 1000i64.saturating_mul(1i64 << claim.attempt.min(6));
            if let Err(error) =
                storage.retry(claim.id, claim.attempt, now().saturating_add(delay), &error)
            {
                tracing::error!(target: "agent", operation="conversation.jobs.retry", %error);
            }
        }
    }
}
