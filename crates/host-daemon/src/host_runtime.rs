use crate::{HostCredentials, HostRpcService, SessionId};
use agent_protocol::{
    models::{
        EnvironmentCapabilities, EnvironmentDescriptor, EnvironmentFileAttachments,
        EnvironmentPlatform, HostStatus, Invitation, RemoteHost,
    },
    protocol::{Body, Call, Response},
};
use agent_transport::transport::{
    Endpoint, HostPeer, IncomingRequest, IncomingSession, NodeId, Session, Ticket, authorize,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct HostRuntime {
    pub(crate) service: HostRpcService,
    endpoint: Endpoint,
    credentials: Arc<HostCredentials>,
    local_node: NodeId,
    name: String,
    environment: EnvironmentDescriptor,
    invitation_lifetime: Duration,
    active: Mutex<BTreeMap<SessionId, Session>>,
}
impl HostRuntime {
    pub async fn new(
        service: HostRpcService,
        endpoint: Endpoint,
        credentials: Arc<HostCredentials>,
        name: String,
        invitation_lifetime: Duration,
    ) -> Self {
        let local_node = credentials.local_identity().await.node_id();
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let environment = environment_descriptor(endpoint.node_id().to_string(), name.clone(), cwd);
        Self {
            service,
            endpoint,
            credentials,
            local_node,
            name,
            environment,
            invitation_lifetime,
            active: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn ticket(&self) -> Ticket {
        self.endpoint.ticket()
    }
    pub fn environment(&self) -> &EnvironmentDescriptor {
        &self.environment
    }
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> Result<()> {
        self.service.start().await?;
        let service = self.service.clone();
        let maintenance_stop = shutdown.clone();
        let maintenance = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut handoff = tokio::time::interval(Duration::from_millis(250));
            handoff.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut next_cleanup = Instant::now();
            let mut next_fetch = Instant::now();
            let mut next_health = Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = maintenance_stop.cancelled() => break,
                    _ = handoff.tick() => {
                        match service.accept_handoff_if_idle().await {
                            Ok(true) => {
                                tracing::info!(
                                    target: "bex",
                                    operation = "host.update.handoff",
                                    message = "accepted update handoff after active work settled"
                                );
                                maintenance_stop.cancel();
                            }
                            Ok(false) => {}
                            Err(error) => tracing::warn!(
                                target: "bex",
                                operation = "host.update.handoff",
                                message = %format_args!("{error:#}")
                            ),
                        }
                    }
                    _ = interval.tick() => {
                        let now = Instant::now();
                        let activity = service.background_activity();
                        let fetch = activity.automatic_git_fetch_interval_ms > 0
                            && now >= next_fetch;
                        let health = activity.provider_health_refresh_interval_ms > 0
                            && now >= next_health;
                        if fetch {
                            next_fetch = now + Duration::from_millis(activity.automatic_git_fetch_interval_ms);
                        }
                        if health {
                            next_health = now + Duration::from_millis(activity.provider_health_refresh_interval_ms);
                        }
                        if fetch || health {
                            if let Err(error) = service.background_activity_tick(fetch, health).await {
                                tracing::warn!(target: "bex", operation = "host.background_activity", message = %error);
                            }
                        }
                        if now >= next_cleanup {
                            next_cleanup = now + Duration::from_secs(60);
                            if let Err(error) = service.cleanup_merged_worktrees().await {
                                tracing::warn!(target: "bex", operation = "host.worktree.cleanup", message = %error);
                            }
                        }
                    }
                }
            }
        });
        let mut sessions = JoinSet::new();
        let authorized_slots = Arc::new(tokio::sync::Semaphore::new(64));
        let pairing_slots = Arc::new(tokio::sync::Semaphore::new(16));
        let handshake_slots = Arc::new(tokio::sync::Semaphore::new(64));
        let result = {
            let accept = self.endpoint.accept();
            tokio::pin!(accept);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break Ok(()),
                    incoming = &mut accept => {
                        accept.set(self.endpoint.accept());
                        match incoming {
                            Some(incoming) => {
                                let Ok(handshake) = handshake_slots.clone().try_acquire_owned() else { continue; };
                                let runtime = self.clone();
                                let stop = shutdown.child_token();
                                let authorized_slots = authorized_slots.clone();
                                let pairing_slots = pairing_slots.clone();
                                sessions.spawn(async move {
                                    let incoming = tokio::select! {
                                        _ = stop.cancelled() => return Ok(()),
                                        result = tokio::time::timeout(Duration::from_secs(15), incoming.establish()) => result.context("QUIC handshake timed out")??,
                                    };
                                    drop(handshake);
                                    let known = runtime.credentials.record.lock().await.trust.allowed.contains(&incoming.node_id());
                                    let slots = if known { &authorized_slots } else { &pairing_slots };
                                    let Ok(permit) = slots.clone().try_acquire_owned() else { return Ok(()); };
                                    runtime.serve(incoming, stop, permit, authorized_slots).await
                                });
                            }
                            None => break Ok(()),
                        }
                    },
                    Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                        match result {
                            Ok(Err(error)) => tracing::error!(target: "bex", operation = "host.session", message = %format_args!("{error:#}")),
                            Err(error) => tracing::error!(target: "bex", operation = "host.session", message = %error),
                            Ok(Ok(())) => {}
                        }
                    }
                }
            }
        };
        shutdown.cancel();
        let maintenance_result = maintenance.await;
        while sessions.join_next().await.is_some() {}
        self.service.shutdown_owned_processes().await;
        self.endpoint.close().await;
        maintenance_result?;
        result
    }
    async fn serve(
        self: Arc<Self>,
        incoming: IncomingSession,
        stop: CancellationToken,
        permit: tokio::sync::OwnedSemaphorePermit,
        authorized_slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<()> {
        let node = incoming.node_id();
        let establish = async {
            let record = self.credentials.record.lock().await;
            if record.trust.allowed.contains(&node) {
                let connection = scopeguard::guard(incoming.authorize(&record.trust)?, |session| {
                    session.close()
                });
                drop(record);
                let peer = connection.accept_peer().await?;
                Ok::<_, anyhow::Error>((scopeguard::ScopeGuard::into_inner(connection), peer))
            } else {
                drop(record);
                let pairing = incoming.pairing().await?;
                self.pair_node(node, pairing.invitation).await?;
                let trust = self.credentials.record.lock().await.trust.clone();
                Ok(pairing.authorize(&trust).await?)
            }
        };
        let (connection, peer) = tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(15), establish) =>
                result.context("session establishment timed out")??,
        };
        let connection = scopeguard::guard(connection, |session| session.close());
        let _permit = if Arc::ptr_eq(permit.semaphore(), &authorized_slots) {
            permit
        } else {
            // Paired sessions move out of the pre-authorization pool.
            let authorized = authorized_slots
                .try_acquire_owned()
                .context("maximum authorized sessions reached")?;
            drop(permit);
            authorized
        };
        self.serve_connection(&connection, peer, stop).await
    }
    async fn serve_connection(
        self: Arc<Self>,
        connection: &Session,
        mut peer: HostPeer,
        stop: CancellationToken,
    ) -> Result<()> {
        let node = connection.node_id();
        let mut session = {
            // Registration and revocation use the same lock order. A revoked node
            // cannot become active in the gap after initial authentication.
            let record = self.credentials.record.lock().await;
            if !record.trust.allowed.contains(&node) {
                return Err(anyhow::anyhow!("peer is not authorized"));
            }
            let session = self.service.open_authenticated_session(node.to_string());
            self.active
                .lock()
                .unwrap()
                .insert(session.id(), connection.clone());
            session
        };
        let id = session.id();
        let session_started = std::time::Instant::now();
        let outgoing = async {
            loop {
                let line = session
                    .recv()
                    .await
                    .context("session outbound queue closed")?;
                tokio::time::timeout(
                    Duration::from_secs(30),
                    agent_transport::framing::write_frame(&mut peer, &line),
                )
                .await
                .context("notification write timed out")??;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        tokio::pin!(outgoing);
        let mut requests = JoinSet::new();
        let mut transfers = JoinSet::new();
        let mut decoders = JoinSet::new();
        let accepting = connection.accept_stream();
        tokio::pin!(accepting);
        let result = loop {
            tokio::select! {
                _ = stop.cancelled() => break Ok(()),
                incoming = &mut accepting => {
                    accepting.set(connection.accept_stream());
                    match incoming {
                        Ok(stream) => {
                            if decoders.len() >= 32 { continue; }
                            decoders.spawn(async move {
                                tokio::time::timeout(Duration::from_secs(30), stream.decode()).await
                                    .context("request decode timed out")?.map_err(anyhow::Error::from)
                            });
                        }
                        Err(error) => break Err(error.into()),
                    }
                },
                Some(decoded) = decoders.join_next(), if !decoders.is_empty() => {
                    let incoming = match decoded {
                        Ok(Ok(incoming)) => incoming,
                        Ok(Err(error)) => { tracing::warn!(target:"bex", operation="host.request.decode", message=%error); continue; }
                        Err(error) => { tracing::warn!(target:"bex", operation="host.request.decode", message=%error); continue; }
                    };
                    match incoming {
                        IncomingRequest::Call(request) => {
                            if requests.len() >= 128 { break Err(anyhow::anyhow!("maximum in-flight request count reached")); }
                            let runtime = self.clone();
                            requests.spawn(async move {
                                let started = std::time::Instant::now();
                                let stream = u64::from(request.send.id());
                                let decode_us = request.decoded_at.duration_since(request.accepted_at).as_micros();
                                let queue_us = started.duration_since(request.decoded_at).as_micros();
                                let accepted_us = request.accepted_at.duration_since(session_started).as_micros();
                                if let Call::ConnectionPerformance(performance) = &request.call && !performance.recovered {
                                    let endpoint = runtime.endpoint.clone();
                                    tokio::task::spawn_blocking(move || endpoint.log_connection_diagnostics()).await?;
                                    tracing::info!(target: "bex", operation = "host.connection.link", message = %format_args!("session={} trace={} connection={} attempt={}", id, performance.timeline.id, performance.connection_id, performance.attempt_id));
                                }
                                let measured = runtime.endpoint.connection_diagnostics_active() && !matches!(request.call, Call::ConnectionPerformance(_));
                                let stopped = request.send.stopped();
                                tokio::pin!(stopped);
                                let mut send = request.send;
                                let parsed = request.call;
                                let response = tokio::select! {
                                    _ = &mut stopped => return Ok(()),
                                    response = runtime.dispatch(node, id, &parsed) => response?,
                                };
                                let handled_us = started.elapsed().as_micros();
                                let writing = std::time::Instant::now();
                                agent_transport::framing::write_frame(&mut send,&response.initial).await?;
                                if measured {
                                    let finished = std::time::Instant::now();
                                    let (trace, accepted_at_us) = runtime.endpoint.connection_time(request.accepted_at);
                                    let (_, write_started_at_us) = runtime.endpoint.connection_time(writing);
                                    let (_, write_finished_at_us) = runtime.endpoint.connection_time(finished);
                                    let message = format!("session={} stream={} method={} accepted_us={} decode_us={} queue_us={} handle_encode_us={} write_us={} bytes={} trace={} accepted_at_us={} write_started_at_us={} write_finished_at_us={}", id, stream, parsed.method(), accepted_us, decode_us, queue_us, handled_us, finished.duration_since(writing).as_micros(), response.initial.len(), trace, accepted_at_us, write_started_at_us, write_finished_at_us);
                                    tokio::task::spawn_blocking(move || {
                                        tracing::info!(target: "bex", operation = "host.rpc.performance", message);
                                    });
                                }
                                if let Some(mut updates) = response.updates {
                                    loop {
                                        tokio::select! {
                                            _ = &mut stopped => break,
                                            line = updates.recv() => match line {
                                                Some(line) => agent_transport::framing::write_frame(&mut send, &line).await?,
                                                None => break,
                                            }
                                        }
                                    }
                                }
                                send.finish()?;
                                Ok::<(), anyhow::Error>(())
                            });
                        }
                        IncomingRequest::Blob(stream) => {
                            if transfers.len() >= 16 { continue; }
                            let service = self.service.clone();
                            transfers.spawn(async move { service.files().transfer(id, stream).await });
                        }
                        IncomingRequest::Close => break Ok(()),
                    }
                },
                result = &mut outgoing => break result,
                Some(_) = requests.join_next(), if !requests.is_empty() => {},
                Some(_) = transfers.join_next(), if !transfers.is_empty() => {},
            }
        };
        requests.abort_all();
        transfers.abort_all();
        decoders.abort_all();
        while requests.join_next().await.is_some() {}
        while transfers.join_next().await.is_some() {}
        while decoders.join_next().await.is_some() {}
        self.active.lock().unwrap().remove(&id);
        self.service.close_session(id);
        result
    }
    async fn pair_node(&self, node: NodeId, invitation: uuid::Uuid) -> Result<()> {
        let mut record = self.credentials.record.lock().await;
        if let Some(trust) = authorize(&record.trust, node, Some(invitation), now())? {
            let mut next = record.clone();
            next.trust = trust;
            *record = self.credentials.persist(next).await?;
        }
        Ok(())
    }
    pub(crate) async fn dispatch(
        &self,
        node: NodeId,
        session: SessionId,
        message: &Call,
    ) -> Result<crate::host_rpc::connections::HostReply> {
        if matches!(message, Call::HostName(_)) {
            return Ok(Response::Success {
                result: Body::from(self.name.clone()),
            }
            .into());
        }
        if self.service.handoff_is_draining()
            && !matches!(
                message,
                Call::HostStatus(_) | Call::ReadUpdateStatus(_) | Call::ListRemotes(_)
            )
        {
            return Err(anyhow::anyhow!(
                "Host is waiting for its installed update to start"
            ));
        }
        if matches!(message, Call::Environment(_)) {
            return Ok(Response::Success {
                result: Body::from(self.environment.clone()),
            }
            .into());
        }
        if let Call::RegisterAwareness(registration) = message {
            let _gate = self
                .service
                .acquire_handoff_gate(false)
                .await
                .map_err(anyhow::Error::msg)?;
            let result = self
                .service
                .register_awareness(session, registration.clone())
                .map_err(|error| anyhow::anyhow!(error.to_string()));
            return Ok(Response::from_result(result.map_err(|error| {
                agent_protocol::error::RpcFailure {
                    code: "awareness_registration_failed".into(),
                    message: format!("{error:#}"),
                    delivery: agent_protocol::error::Delivery::NotSent,
                }
            }))
            .into());
        }
        if matches!(message, Call::Awareness(_)) {
            let cancel = self
                .service
                .cancellation(session)
                .map_err(anyhow::Error::msg)?;
            return Ok(self.service.awareness(self.environment.clone(), cancel));
        }
        let management = matches!(
            message,
            Call::Pair(_)
                | Call::HostStatus(_)
                | Call::ReadUpdateStatus(_)
                | Call::CheckUpdate(_)
                | Call::DownloadUpdate(_)
                | Call::InstallUpdate(_)
                | Call::SetUpdateChannel(_)
                | Call::ReadNativeUpdate(_)
                | Call::Invite(_)
                | Call::Revoke(_)
                | Call::ListRemotes(_)
                | Call::RegisterRemote(_)
                | Call::RemoveRemote(_)
        );
        if management {
            let _management_gate = match message {
                Call::Pair(_)
                | Call::HostStatus(_)
                | Call::Invite(_)
                | Call::Revoke(_)
                | Call::ListRemotes(_)
                | Call::RegisterRemote(_)
                | Call::RemoveRemote(_) => Some(
                    self.service
                        .acquire_handoff_gate(matches!(
                            message,
                            Call::HostStatus(_) | Call::ListRemotes(_)
                        ))
                        .await
                        .map_err(anyhow::Error::msg)?,
                ),
                _ => None,
            };
            let result = if matches!(message, Call::Pair(_)) {
                // Only authorized sessions reach dispatch; the invitation was
                // already consumed at the transport gate.
                Ok(agent_protocol::models::Empty {}.into())
            } else if node != self.local_node {
                Err(anyhow::anyhow!("management requires the local node"))
            } else {
                self.manage(message).await
            };
            return Ok(Response::from_result(result.map_err(|message| {
                agent_protocol::error::RpcFailure {
                    code: "management_failed".into(),
                    message: format!("{message:#}"),
                    delivery: agent_protocol::error::Delivery::NotSent,
                }
            }))
            .into());
        }
        self.service
            .dispatch_from_peer(session, message, node == self.local_node)
            .await
            .map_err(anyhow::Error::msg)
    }
    async fn manage(&self, call: &Call) -> Result<Body> {
        match call {
            Call::ReadUpdateStatus(_)
            | Call::CheckUpdate(_)
            | Call::DownloadUpdate(_)
            | Call::InstallUpdate(_)
            | Call::SetUpdateChannel(_)
            | Call::ReadNativeUpdate(_) => {
                self.service.update(call).await.map_err(anyhow::Error::msg)
            }
            Call::HostStatus(_) => {
                let record = self.credentials.record.lock().await;
                Ok(HostStatus {
                    node_id: self.endpoint.node_id().to_string(),
                    name: self.name.clone(),
                    devices: record
                        .trust
                        .allowed
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    provider_errors: self.service.provider_errors().as_object().cloned(),
                }
                .into())
            }
            Call::Invite(_) => {
                let (ai_recipients, transcription_recipient) = self.service.data_recipients();
                let ticket = Invitation {
                    endpoint: self.endpoint.ticket().to_string(),
                    invitation: uuid::Uuid::new_v4(),
                    expires_at: now() + self.invitation_lifetime.as_secs(),
                    host_name: self.name.clone(),
                    ai_recipients,
                    transcription_recipient,
                };
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.trust.invitations.retain(|_, expiry| now() < *expiry);
                next.trust
                    .invitations
                    .insert(ticket.invitation, ticket.expires_at);
                *record = self.credentials.persist(next).await?;
                Ok(ticket.into())
            }
            Call::Revoke(params) => {
                let node_id: NodeId = params.id.parse().context("invalid node ID")?;
                if node_id == self.local_node {
                    return Err(anyhow::anyhow!("cannot revoke the local node"));
                }
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.trust.allowed.remove(&node_id);
                *record = self.credentials.persist(next).await?;
                self.service.revoke_device(&node_id.to_string());
                for connection in self.active.lock().unwrap().values() {
                    if connection.node_id() == node_id {
                        connection.close();
                    }
                }
                Ok(agent_protocol::models::Empty {}.into())
            }
            Call::ListRemotes(_) => Ok(self
                .credentials
                .record
                .lock()
                .await
                .remotes
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .into()),
            Call::RegisterRemote(params) => {
                // Pairing runs on the client's existing endpoint. The Host only
                // persists its local client's destination; it never binds that key.
                let ticket: Ticket = params.ticket.parse().context("invalid remote ticket")?;
                let node_id = ticket.node_id();
                let profile = RemoteHost {
                    id: node_id.to_string(),
                    name: params.name.clone(),
                    ticket: ticket.to_string(),
                };
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.remotes.insert(node_id, profile.clone());
                *record = self.credentials.persist(next).await?;
                Ok(profile.into())
            }
            Call::RemoveRemote(params) => {
                let id: NodeId = params.id.parse().context("invalid remote ID")?;
                let mut record = self.credentials.record.lock().await;
                let mut next = record.clone();
                next.remotes.remove(&id);
                *record = self.credentials.persist(next).await?;
                Ok(agent_protocol::models::Empty {}.into())
            }
            _ => Err(anyhow::anyhow!("unknown management method")),
        }
    }
}

fn environment_descriptor(
    environment_id: String,
    label: String,
    cwd: String,
) -> EnvironmentDescriptor {
    let machine = detect_machine_kind();
    EnvironmentDescriptor {
        environment_id,
        label,
        cwd,
        platform: EnvironmentPlatform {
            os: match std::env::consts::OS {
                "macos" => "darwin",
                "linux" => "linux",
                "windows" => "windows",
                _ => "unknown",
            }
            .into(),
            arch: match std::env::consts::ARCH {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                _ => "other",
            }
            .into(),
            machine: machine.clone(),
        },
        server_version: env!("CARGO_PKG_VERSION").into(),
        orchestration_protocol_version: Some(2),
        capabilities: EnvironmentCapabilities {
            repository_identity: true,
            connection_probe: true,
            attachment_uploads: true,
            question_attachments: true,
            file_attachments: Some(EnvironmentFileAttachments {
                max_upload_bytes: 50 * 1024 * 1024,
            }),
            pull_requests: false,
            pull_request_checks: false,
            inline_message_context: true,
            required_worktree_bootstrap: true,
            thread_settlement: true,
            thread_auto_settlement: true,
            thread_snooze: true,
            storage_cleanup: true,
            project_worktree_cleanup: true,
            thread_restart_continuation: true,
            project_settings_overrides: true,
            environment_themes: false,
            usage_limit_sources: false,
            usage_price_overrides: false,
            usage_model_aliases: false,
            thread_pinning: true,
            thread_pin_reorder: true,
            thread_active_reorder: true,
            thread_auto_settle_opt_out: true,
            thread_title_regeneration: true,
            thread_visited_tracking: true,
            thread_pull_request_linking: true,
            server_resolved_command_context: false,
            thread_pull_requests: false,
            thread_pull_request_watch: false,
            pull_request_stack_actions: false,
            server_self_update: None,
            server_installation: None,
            server_self_update_progress: false,
            server_update_thread_continuation: false,
            project_clone_tracking: false,
            environment_icon: machine.is_some(),
            desktop_app_update: false,
            agent_activity_publishing: true,
        },
    }
}

fn detect_machine_kind() -> Option<String> {
    if let Some(value) = std::env::var_os("AGENT_ENVIRONMENT_MACHINE") {
        let value = value.to_string_lossy().trim().to_ascii_lowercase();
        if matches!(
            value.as_str(),
            "server" | "cloud" | "linux" | "desktop" | "laptop" | "mac-mini" | "mac-studio"
        ) {
            return Some(value);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let model = std::process::Command::new("sysctl")
            .args(["-n", "hw.model"])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        return Some(
            if model.starts_with("macmini") {
                "mac-mini"
            } else if model.starts_with("macstudio") {
                "mac-studio"
            } else if model.starts_with("macbook") {
                "laptop"
            } else {
                "desktop"
            }
            .into(),
        );
    }
    #[cfg(target_os = "linux")]
    {
        let product = std::fs::read_to_string("/sys/class/dmi/id/product_name")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let chassis = std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
            .unwrap_or_default()
            .trim()
            .parse::<u16>()
            .ok();
        if product.contains("virtual") || product.contains("vmware") || product.contains("kvm") {
            return Some("cloud".into());
        }
        return Some(
            match chassis {
                Some(8 | 9 | 10 | 14) => "laptop",
                Some(3 | 4 | 5 | 6 | 7 | 15 | 16 | 17) => "desktop",
                _ => "linux",
            }
            .into(),
        );
    }
    #[cfg(target_os = "windows")]
    {
        return Some("desktop".into());
    }
    #[allow(unreachable_code)]
    None
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
